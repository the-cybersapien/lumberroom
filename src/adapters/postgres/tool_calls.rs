//! Postgres implementation of ToolCallRepository.
//!
//! `record` spawns rather than awaiting, by contract: instrumentation must never add latency to
//! the bootstrap budget, and a failed insert must never fail the call that triggered it.
//!
//! `client_stats` is the number the project exists to watch. A surface that reads the store and
//! never writes to it is consuming something it does not maintain, and the system PRD calls a store
//! read often and written rarely a decaying one.

use async_trait::async_trait;
use sqlx::{PgPool, Row};

use crate::domain::errors::Result;
use crate::domain::types::ToolCall;
use crate::ports::{ClientStats, ToolCallRepository, ToolCallStats};

/// Which tools count as reads and which as writes.
///
/// A literal list rather than a pattern on the name. A tool in neither list still counts in `calls`,
/// so a new tool shows up as traffic that is neither a read nor a write, which is visible in the
/// output; a name pattern would quietly file it under whichever side it happened to resemble.
/// Adding a tool means adding it here.
const READ_TOOLS: &[&str] = &["memory_search", "context_bootstrap", "registry_get"];
const WRITE_TOOLS: &[&str] = &["memory_write", "memory_forget"];

/// Per-client rates over a window.
///
/// **The session approximation, stated in the code because it changes how the numbers read.** Only
/// clients that send a session id can have their calls correlated into a conversation. For everything
/// else the calls are bucketed by the hour they arrived in, which merges two conversations that
/// happened in the same hour into one and splits one conversation that crossed an hour boundary into
/// two. Both distortions push `sessions` and therefore the two rates in directions that are not
/// predictable, so a client without session ids gives an indication rather than a measurement. The
/// `hour:` prefix keeps a real session id from colliding with a bucket.
const CLIENT_STATS_SQL: &str = r#"
    WITH calls AS (
        SELECT client,
               tool,
               succeeded,
               unprompted,
               COALESCE(session_id, 'hour:' || date_trunc('hour', created_at)::text) AS bucket
          FROM tool_calls
         WHERE created_at > now() - ($1 || ' hours')::interval
    )
    SELECT client,
           count(*)                                                     AS calls,
           count(*) FILTER (WHERE tool = ANY($2))                        AS reads,
           count(*) FILTER (WHERE tool = ANY($3))                        AS writes,
           count(*) FILTER (WHERE NOT succeeded)                         AS failures,
           count(DISTINCT bucket)                                        AS sessions,
           count(DISTINCT bucket) FILTER (WHERE unprompted IS TRUE AND tool = ANY($2))
             AS sessions_with_unprompted_read,
           count(DISTINCT bucket) FILTER (WHERE unprompted IS TRUE AND tool = ANY($3))
             AS sessions_with_unprompted_write
      FROM calls
     GROUP BY client
     ORDER BY calls DESC
"#;

pub struct PgToolCallRepository {
    pool: PgPool,
}

impl PgToolCallRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn tool_list(tools: &[&str]) -> Vec<String> {
    tools.iter().map(|t| (*t).to_string()).collect()
}

/// A rate nobody asked for is worse than no rate. Zero sessions or zero reads gives `None`, which
/// prints as "no data" rather than as 0%, and 0% would read as a surface that never writes.
fn rate(part: i64, whole: i64) -> Option<f64> {
    if whole <= 0 {
        return None;
    }
    Some((part as f64 / whole as f64 * 10_000.0).round() / 10_000.0)
}

#[async_trait]
impl ToolCallRepository for PgToolCallRepository {
    fn record(&self, call: ToolCall) {
        let pool = self.pool.clone();
        tokio::spawn(async move {
            let result = sqlx::query(
                "INSERT INTO tool_calls (client, tool, succeeded, unprompted, latency_ms,
                                         session_id, namespace)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(&call.client)
            .bind(&call.tool)
            .bind(call.succeeded)
            .bind(call.unprompted)
            .bind(call.latency_ms)
            .bind(&call.session_id)
            .bind(&call.namespace)
            .execute(&pool)
            .await;
            if let Err(e) = result {
                tracing::warn!(tool = %call.tool, error = %e, "tool_calls insert failed");
            }
        });
    }

    async fn stats(&self, window_hours: i64) -> Result<Vec<ToolCallStats>> {
        let rows = sqlx::query(
            "SELECT tool,
                    client,
                    count(*)                                                      AS calls,
                    count(*) FILTER (WHERE NOT succeeded)                         AS failures,
                    count(*) FILTER (WHERE unprompted)                            AS unprompted,
                    percentile_disc(0.5)  WITHIN GROUP (ORDER BY latency_ms)::int AS p50_ms,
                    percentile_disc(0.95) WITHIN GROUP (ORDER BY latency_ms)::int AS p95_ms
               FROM tool_calls
              WHERE created_at > now() - ($1 || ' hours')::interval
              GROUP BY tool, client
              ORDER BY calls DESC",
        )
        .bind(window_hours.to_string())
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .iter()
            .map(|r| ToolCallStats {
                tool: r.get("tool"),
                client: r.get("client"),
                calls: r.get("calls"),
                failures: r.get("failures"),
                unprompted: r.get("unprompted"),
                p50_ms: r.get("p50_ms"),
                p95_ms: r.get("p95_ms"),
            })
            .collect())
    }

    async fn client_stats(&self, window_hours: i64) -> Result<Vec<ClientStats>> {
        let rows = sqlx::query(CLIENT_STATS_SQL)
            .bind(window_hours.to_string())
            .bind(tool_list(READ_TOOLS))
            .bind(tool_list(WRITE_TOOLS))
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .iter()
            .map(|r| {
                let sessions: i64 = r.get("sessions");
                let reads: i64 = r.get("reads");
                let writes: i64 = r.get("writes");
                let with_read: i64 = r.get("sessions_with_unprompted_read");
                let with_write: i64 = r.get("sessions_with_unprompted_write");
                ClientStats {
                    client: r.get("client"),
                    calls: r.get("calls"),
                    reads,
                    writes,
                    failures: r.get("failures"),
                    sessions,
                    sessions_with_unprompted_read: with_read,
                    sessions_with_unprompted_write: with_write,
                    unprompted_read_rate: rate(with_read, sessions),
                    unprompted_write_rate: rate(with_write, sessions),
                    write_to_read_ratio: rate(writes, reads),
                }
            })
            .collect())
    }

    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tool_counts_as_both_a_read_and_a_write() {
        for tool in READ_TOOLS {
            assert!(!WRITE_TOOLS.contains(tool), "{tool} is on both lists");
        }
    }

    #[test]
    fn the_four_tools_that_exist_today_are_all_classified() {
        for tool in ["memory_search", "context_bootstrap", "registry_get", "memory_write"] {
            assert!(
                READ_TOOLS.contains(&tool) || WRITE_TOOLS.contains(&tool),
                "{tool} would count as traffic and in neither rate"
            );
        }
    }

    #[test]
    fn a_rate_with_no_denominator_is_absent_rather_than_zero() {
        assert_eq!(rate(0, 0), None);
        assert_eq!(rate(3, 0), None);
        assert_eq!(rate(1, 4), Some(0.25));
        assert_eq!(rate(1, 3), Some(0.3333));
    }

    #[test]
    fn a_client_that_only_reads_has_a_write_ratio_of_zero_not_none() {
        assert_eq!(rate(0, 12), Some(0.0));
    }

    #[test]
    fn calls_without_a_session_id_are_bucketed_by_the_hour_they_arrived_in() {
        assert!(CLIENT_STATS_SQL.contains("COALESCE(session_id, 'hour:'"));
        assert!(CLIENT_STATS_SQL.contains("date_trunc('hour', created_at)"));
    }
}
