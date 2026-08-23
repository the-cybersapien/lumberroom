//! Instrumentation. The numbers behind "does a model call these tools on its own".

use async_trait::async_trait;

use crate::domain::errors::Result;
use crate::domain::types::ToolCall;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolCallStats {
    pub tool: String,
    pub client: String,
    pub calls: i64,
    pub failures: i64,
    pub unprompted: i64,
    pub p50_ms: Option<i32>,
    pub p95_ms: Option<i32>,
}

/// Per-client rates over a rolling window. The write rate is the number that matters more: a
/// surface that reads and never writes is consuming a store it does not maintain.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientStats {
    pub client: String,
    pub calls: i64,
    pub reads: i64,
    pub writes: i64,
    pub failures: i64,
    /// Distinct sessions seen. Calls arriving without a session id are bucketed by hour instead,
    /// which is an approximation and is labelled as one.
    pub sessions: i64,
    pub sessions_with_unprompted_read: i64,
    pub sessions_with_unprompted_write: i64,
    pub unprompted_read_rate: Option<f64>,
    pub unprompted_write_rate: Option<f64>,
    pub write_to_read_ratio: Option<f64>,
}

#[async_trait]
pub trait ToolCallRepository: Send + Sync {
    /// Fire and forget by contract: instrumentation must not add latency to the bootstrap budget,
    /// and a failed insert must never fail the call that triggered it.
    fn record(&self, call: ToolCall);
    async fn stats(&self, window_hours: i64) -> Result<Vec<ToolCallStats>>;
    async fn client_stats(&self, window_hours: i64) -> Result<Vec<ClientStats>>;
    async fn ping(&self) -> Result<()>;
}
