//! The cleanup pass, against a real database. Skipped when none is reachable.
//!
//!   DATABASE_URL=postgres://lumberroom:pw@127.0.0.1:5432/lumberroom cargo test --test cleanup
//!
//! Six properties, and every one of them is a thing the unit tests cannot reach because it is a
//! claim about what SQL returns rather than about what Rust computes.
//!
//! The window admits a pair where only one side is new. Filtering both sides is the obvious reading
//! of "read what changed" and it makes the pass blind to a new row restating an old fact.
//!
//! A second run over the same window queues nothing. This is the whole argument for an hourly
//! cadence being safe to run hourly.
//!
//! A private row never reaches the list handed to a model. The filter is in the query, so this is
//! the only place it can be checked.
//!
//! Applying retires through supersession, so the retired text is still readable afterwards.
//!
//! A member edited since the pass read it makes apply refuse rather than adapt.
//!
//! A finding the owner resolved by hand closes itself instead of sitting in the queue forever.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use lumberroom_server::adapters::auth;
use lumberroom_server::adapters::embedding::HashEmbedder;
use lumberroom_server::adapters::postgres::{self, PgCleanupRepository};
use lumberroom_server::config::{self, Config};
use lumberroom_server::crypto::kek::{EnvKeyProvider, KeyProvider};
use lumberroom_server::domain::policy::NamespaceGrant;
use lumberroom_server::domain::types::{Invocation, Principal};
use lumberroom_server::mcp::AppState;
use lumberroom_server::ports::cleanup::{CandidateQuery, CleanupRepository};
use lumberroom_server::ports::OauthStore;
use lumberroom_server::services::{bootstrap, cleanup, review, search, Ctx, Repos};

mod common;

const TEST_DB: &str = "lumberroom_rust_test";
const TEST_KEK_HEX: &str = "5375747254657374204b454b20666f722074686520696e746567726174696f6e";
const TEST_KEK_VAR: &str = "LUMBERROOM_TEST_KEK";
const TEST_KEK_ID: &str = "kek-test";

/// Two tokens, and the difference between them is the whole of the capability test: the owner's
/// own client reaches everything and still may not ingest, and the ingesting client is the one the
/// owner named.
const OWNER_TOKEN: &str = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";
const INGEST_TOKEN: &str = "iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii";

/// A connection string with an inline password. The tripwire fires on it as
/// `connection_string_password`, and it is the shape a transcript actually carries.

/// Every test here truncates the shared test database, so they serialise themselves rather than
/// relying on `--test-threads=1` being remembered. Cargo runs one test binary at a time, so this
/// mutex and the ones in `integration.rs` and `console.rs` do not have to know about each other.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A setup step that is allowed to be missing, with the reason printed rather than swallowed.
///
/// A laptop with no Postgres and a store whose migrations no longer match this binary both skip,
/// and without the printed reason they skip with the same sentence. The suite skips rather than
/// fails, which makes that sentence the only thing standing between a broken run and a run
/// somebody reads as a pass.
macro_rules! step {
    ($what:expr, $result:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipping: {} failed: {e:?}", $what);
                return None;
            }
        }
    };
}

struct Harness {
    ctx: Ctx,
    repo: Arc<dyn CleanupRepository>,
    pool: PgPool,
    /// A live server on a loopback port, because the capability gate lives in the router and a
    /// service-level test cannot see it.
    base: String,
    _serial: tokio::sync::MutexGuard<'static, ()>,
    /// Held for the whole test. The mutex above serialises this binary's own threads; this is what
    /// keeps the other five binaries out of the same database.
    _db: common::DbGuard,
}

impl Harness {
    async fn post(&self, path: &str, token: &str, body: serde_json::Value) -> (u16, String) {
        let res = reqwest::Client::new()
            .post(format!("{}{path}", self.base))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.text().await.unwrap())
    }

    async fn get(&self, path: &str, token: &str) -> (u16, String) {
        let res = reqwest::Client::new()
            .get(format!("{}{path}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.text().await.unwrap())
    }

}

/// Returns None when no database is reachable, so the suite skips rather than fails on a machine
/// without one.
async fn setup() -> Option<Harness> {
    let guard = SERIAL.lock().await;
    let admin_url = std::env::var("DATABASE_URL").ok()?;
    let base = admin_url.rsplit_once('/')?.0.to_string();
    let admin = step!("connecting to the admin database", PgPool::connect(&admin_url).await);

    let exists: Result<Option<i32>, _> =
        sqlx::query_scalar("SELECT 1 FROM pg_database WHERE datname = $1")
            .bind(TEST_DB)
            .fetch_optional(&admin)
            .await;
    let exists = step!("looking for the test database", exists);
    if exists.is_none() {
        // DDL cannot take a bind parameter, so this is the one statement here that has to be built
        // as a string. Audited: TEST_DB is a compile-time constant with no external input.
        let created = sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {TEST_DB}")))
            .execute(&admin)
            .await;
        step!("creating the test database", created);
    }
    admin.close().await;

    let url = format!("{base}/{TEST_DB}");
    std::env::set_var("DATABASE_URL", &url);
    std::env::set_var(
        "AUTH_TOKENS",
        format!(
            r#"[{{"client":"mac","token":"{OWNER_TOKEN}","read":[{{"namespace":"*","max":"sealed"}}],"write":[{{"namespace":"*","max":"sealed"}}],"sealedCapable":true,"registryWrite":true}},
                {{"client":"ingester","token":"{INGEST_TOKEN}","read":[{{"namespace":"*","max":"sealed"}}],"write":[{{"namespace":"*","max":"sealed"}}],"sealedCapable":true,"registryWrite":true,"mayIngest":true}}]"#
        ),
    );
    std::env::set_var("EMBED_PROVIDER", "hash");
    std::env::set_var(TEST_KEK_VAR, TEST_KEK_HEX);

    // Before the truncate below, and before anything reads. Every other binary
    // targeting this database waits here.
    let db_lock = common::lock_database(&url).await?;
    let pool = step!("connecting to the test database", postgres::connect(&url).await);
    step!("migrating the test database", postgres::migrate(&pool).await);
    let truncated = sqlx::query(
        "TRUNCATE memory, registry, registry_history, entity_alias, sealed_item, tool_calls,
                  registry_alias, kek_state,
                  oauth_client, oauth_code, oauth_token, oauth_refresh,
                  ingest_proposal, ingest_proposal_source, ingest_watermark, ingest_run,
                  cleanup_proposal, cleanup_proposal_member, cleanup_watermark,
                  recall_emission
         RESTART IDENTITY CASCADE",
    )
    .execute(&pool)
    .await;
    step!("truncating the test database", truncated);

    let cfg: Config = step!("loading the config", config::load());
    let keys: Arc<dyn KeyProvider> = Arc::new(EnvKeyProvider::new(TEST_KEK_VAR, TEST_KEK_ID));
    let kek = step!("reading the test key", keys.kek().await);
    let check = postgres::verify_kek(
        &pool,
        &cfg.tenant_id,
        TEST_KEK_ID,
        &lumberroom_server::crypto::kek::fingerprint(&kek),
        keys.provider(),
    )
    .await;
    let check = step!("verifying the test key", check);
    let kek_verified = !matches!(check, postgres::KekCheck::Mismatch { .. });

    let memories = Arc::new(postgres::PgMemoryRepository::new(pool.clone()));
    let ctx = Ctx {
        cfg: Arc::new(cfg),
        repos: Repos {
            aliases: Arc::new(postgres::PgAliasRepository::new(pool.clone())),
            memories: memories.clone(),
            registry: Arc::new(postgres::PgRegistryRepository::new(pool.clone())),
            tool_calls: Arc::new(postgres::PgToolCallRepository::new(pool.clone())),
            sealed: Some(Arc::new(postgres::PgSealedRepository::new(pool.clone()))),
            ciphertext: Some(memories),
        },
        embedder: Arc::new(HashEmbedder::new(768)),
        keys: Some(keys),
        kek_verified,
        principal: owner_like("mac"),
        invocation: Invocation::Cli,
        // Set, and deliberately never used by anything in this file. The emission check is
        // tenant-wide on content hash: a check keyed on this would fire never.
        session_id: Some("test-session".into()),
    };
    bootstrap::clear_cache();

    let repo: Arc<dyn CleanupRepository> = Arc::new(PgCleanupRepository::new(pool.clone()));

    let oauth: Arc<dyn OauthStore> = Arc::new(postgres::PgOauthStore::new(pool.clone()));
    let state = Arc::new(AppState {
        cleanup: Arc::clone(&repo),
        aliases: Arc::new(postgres::PgAliasRepository::new(pool.clone())),
        cfg: Arc::clone(&ctx.cfg),
        repos: ctx.repos.clone(),
        oauth: Arc::clone(&oauth),
        ingest: Arc::new(postgres::PgIngestRepository::new(pool.clone())),
        embedder: Arc::clone(&ctx.embedder),
        degraded_embedder: false,
        keys: ctx.keys.clone(),
        kek_verified: ctx.kek_verified,
    });
    let authenticator = auth::create(&ctx.cfg, Some(oauth)).ok()?;
    let app = lumberroom_server::http::router(state, authenticator);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
    let addr: SocketAddr = listener.local_addr().ok()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app.into_make_service()).await;
    });

    Some(Harness { ctx, repo, pool, base: format!("http://{addr}"), _serial: guard, _db: db_lock })
}

macro_rules! harness_or_skip {
    () => {
        match setup().await {
            Some(h) => h,
            None => {
                eprintln!("skipping: no database reachable");
                return;
            }
        }
    };
}

fn owner_like(client: &str) -> Principal {
    Principal {
        client: client.into(),
        token_id: "test".into(),
        mode: "token",
        scopes: vec![],
        read: NamespaceGrant::everything(),
        write: NamespaceGrant::everything(),
        registry_write: true,
        sealed_capable: true,
        may_delete: true,
        may_ingest: true,
        may_read_history: true,
    }
}

/// The same, classified private, which is what the model-visibility test needs.
async fn put_private(h: &Harness, namespace: &str, content: &str) -> String {
    let id = put_raw(h, namespace, content).await;
    sqlx::query("UPDATE memory SET sensitivity = 'private' WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&id).unwrap())
        .execute(&h.pool)
        .await
        .unwrap();
    id
}

/// A row inserted straight into the table, bypassing `write::run`.
///
/// This is not a shortcut. `write::run` collapses a near-identical write into the row it matches,
/// so a duplicate cannot be created through it at all, and every duplicate in the owner's real
/// store got there some other way: written before that check existed, or restored from a dump, or
/// put there by a test harness. Those are the rows this pass is for, and this is how a test makes
/// one.
async fn put_raw(h: &Harness, namespace: &str, content: &str) -> String {
    let id = uuid::Uuid::new_v4();
    let vectors = h.ctx.embedder.embed_documents(vec![content.to_string()]).await.unwrap();
    let embedding = pgvector::Vector::from(vectors[0].clone());
    sqlx::query(
        "INSERT INTO memory (id, tenant_id, namespace, content, embedding, source_client,
                             embedding_model, sensitivity)
         VALUES ($1, $2, $3, $4, $5, 'test', 'hash', 'open')",
    )
    .bind(id)
    .bind(&h.ctx.cfg.tenant_id)
    .bind(namespace)
    .bind(content)
    .bind(embedding)
    .execute(&h.pool)
    .await
    .unwrap();
    id.to_string()
}

/// Set a row's valid time, which is the clock supersession validates on.
async fn set_occurred(h: &Harness, id: &str, at: &str) {
    sqlx::query("UPDATE memory SET occurred_at = $2::timestamptz WHERE id = $1")
        .bind(uuid::Uuid::parse_str(id).unwrap())
        .bind(at)
        .execute(&h.pool)
        .await
        .unwrap();
}

/// Back-date a row, so a test can put one side of a pair outside the window without waiting an hour.
async fn backdate(h: &Harness, id: &str, days: i64) {
    sqlx::query("UPDATE memory SET created_at = now() - make_interval(days => $2::int) WHERE id = $1")
        .bind(uuid::Uuid::parse_str(id).unwrap())
        .bind(i32::try_from(days).unwrap())
        .execute(&h.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn two_identical_rows_become_one_proposal_and_the_oldest_survives() {
    let h = harness_or_skip!();
    let first = put_raw(&h, "user:me", "the deploy runbook lives in DEPLOY.md").await;
    // Case and spacing differ, which is what makes this the pass's job rather than find_exact's:
    // that one compares bytes.
    let second = put_raw(&h, "user:me", "The deploy runbook lives in DEPLOY.md  ").await;
    assert_ne!(first, second);

    let (report, _) = cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    assert_eq!(report.exact_groups, 1, "case and spacing should not make two rows distinct");
    // One finding, not two. Identical text also sits at a cosine of 1.0, so the same pair would
    // otherwise arrive again as a paraphrase under a different cluster key.
    assert_eq!(report.queued, 1, "the same pair was queued twice: {report:?}");

    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let p = rows.iter().find(|p| p.kind == lumberroom_server::domain::cleanup::CleanupKind::Exact).unwrap();
    assert_eq!(p.keep_id.as_deref(), Some(first.as_str()), "the oldest carries the reads");
}

#[tokio::test]
async fn a_second_run_over_the_same_window_queues_nothing() {
    // The whole argument for an hourly cadence. Without it the queue grows by one row per hour per
    // cluster and stops being readable by lunchtime.
    let h = harness_or_skip!();
    put_raw(&h, "user:me", "the builder image carries g++").await;
    put_raw(&h, "user:me", "the builder image carries g++").await;

    let (first, _) = cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    assert!(first.queued >= 1);
    let (second, _) = cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    assert_eq!(second.queued, 0, "the same cluster was queued twice");
    assert!(second.already_known >= 1, "and it was not counted as known either");
}

#[tokio::test]
async fn the_exact_query_still_groups_an_old_row_once_the_watermark_has_passed_it() {
    // The anchor admits a whole group when one member is new. Case and spacing normalise away, so
    // these two are one group even though nothing compares their bytes.
    let h = harness_or_skip!();
    let old = put_raw(&h, "user:me", "the acceptance gates run against a live server").await;
    backdate(&h, &old, 30).await;
    put_raw(&h, "user:me", "an unrelated fact that moves the watermark").await;

    let (first, _) = cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    let mark = first.through.expect("a run that found nothing still has to advance its watermark");
    let old_created: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT created_at FROM memory WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&old).unwrap())
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert!(old_created < mark, "the fixture did not put the old row outside the window");

    let restated = put_raw(&h, "user:me", "the  ACCEPTANCE gates run  against a live server").await;
    let (report, _) = cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();

    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let found = rows.iter().any(|p| {
        p.members.iter().any(|m| m.memory_id == old)
            && p.members.iter().any(|m| m.memory_id == restated)
    });
    assert!(found, "the old row fell out of its own group. report: {report:?}");
}

#[tokio::test]
async fn the_similarity_window_admits_a_pair_where_only_one_side_is_new() {
    // The property on its own, at the repository, with the threshold set to zero so neither the
    // band nor the embedder can decide the outcome. Only the window is under test.
    //
    // Checked by mutation: turning the OR in SIMILAR_PAIRS_SQL into an AND fails this and nothing
    // else. An earlier version of this test went through cleanup::run and passed against both,
    // because the exact query was quietly finding the pair.
    let h = harness_or_skip!();
    let old = put_raw(&h, "user:me", "the release image is built from Dockerfile").await;
    backdate(&h, &old, 30).await;
    let fresh = put_raw(&h, "user:me", "an unrelated newer fact").await;

    let q = CandidateQuery {
        namespace: Some("user:me".into()),
        max_sensitivity: lumberroom_server::domain::types::Sensitivity::Sealed,
        since: Some(Utc::now() - chrono::Duration::days(1)),
        limit: 100,
    };
    let pairs = h.repo.similar_pairs(&h.ctx.cfg.tenant_id, &q, 0.0).await.unwrap();
    let covered = pairs.iter().any(|p| {
        let ids = [p.older.id.as_str(), p.newer.id.as_str()];
        ids.contains(&old.as_str()) && ids.contains(&fresh.as_str())
    });
    assert!(
        covered,
        "the window dropped a pair whose older side predates it, which is the most common \
         duplicate there is. pairs: {:?}",
        pairs.iter().map(|p| (&p.older.id, &p.newer.id)).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn applying_retires_through_supersession_so_the_old_text_is_still_readable() {
    let h = harness_or_skip!();
    let first = put_raw(&h, "user:me", "the CLI is dependency-free JavaScript").await;
    let second = put_raw(&h, "user:me", "The CLI is dependency-free JavaScript").await;

    cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let p = rows.iter().find(|p| p.kind == lumberroom_server::domain::cleanup::CleanupKind::Exact).unwrap();

    let applied = cleanup::apply(&h.ctx, h.repo.as_ref(), &p.id).await.unwrap();
    assert_eq!(applied.retired, vec![second.clone()]);
    assert!(applied.deleted.is_empty(), "an exact duplicate must retire, never delete");

    // The retired row is still there, pointing at its replacement.
    let superseded_by: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT superseded_by FROM memory WHERE id = $1")
            .bind(uuid::Uuid::parse_str(&second).unwrap())
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert_eq!(superseded_by.map(|u| u.to_string()), Some(first.clone()));

    // And the survivor still answers.
    let hits = search::run(
        &h.ctx,
        "dependency-free JavaScript",
        Some(vec!["user:me".into()]),
        Some(10),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(hits.hits.iter().any(|hit| hit.id == first), "the surviving row stopped answering");
}

#[tokio::test]
async fn a_member_edited_since_the_pass_read_it_makes_apply_refuse() {
    let h = harness_or_skip!();
    put_raw(&h, "user:me", "migrations are forward-only").await;
    let second = put_raw(&h, "user:me", "Migrations are forward-only").await;

    cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let p = rows.iter().find(|p| p.kind == lumberroom_server::domain::cleanup::CleanupKind::Exact).unwrap();

    sqlx::query("UPDATE memory SET content = $2 WHERE id = $1")
        .bind(uuid::Uuid::parse_str(&second).unwrap())
        .bind("migrations are forward-only, and sqlx embeds them at compile time")
        .execute(&h.pool)
        .await
        .unwrap();

    let err = cleanup::apply(&h.ctx, h.repo.as_ref(), &p.id).await.unwrap_err();
    assert_eq!(err.kind, lumberroom_server::domain::errors::Kind::Conflict);
    assert!(
        err.client_message().contains("changed since"),
        "the refusal should say the store moved: {}",
        err.client_message()
    );
}

#[tokio::test]
async fn a_finding_the_owner_answered_by_hand_closes_itself() {
    let h = harness_or_skip!();
    let first = put_raw(&h, "user:me", "the eval fixture lives under ~/.config/lumberroom").await;
    let second = put_raw(&h, "user:me", "The eval fixture lives under ~/.config/lumberroom").await;

    cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let p = rows.iter().find(|p| p.kind == lumberroom_server::domain::cleanup::CleanupKind::Exact).unwrap();

    // The owner resolves it himself, which is what he does with a contradiction.
    review::supersede(&h.ctx, &second, &first).await.unwrap();

    let closed = h.repo.close_answered(&h.ctx.cfg.tenant_id).await.unwrap();
    assert!(closed.contains(&p.id), "the proposal stayed queued after the store answered it");

    let still_proposed = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    assert!(!still_proposed.iter().any(|q| q.id == p.id));
}

#[tokio::test]
async fn a_private_row_never_reaches_the_list_handed_to_a_model() {
    // The filter is a predicate inside the query, so this is the only place it can be checked.
    let h = harness_or_skip!();
    put_private(&h, "personal:finance", "the household budget review is the first Sunday").await;
    put_private(&h, "personal:finance", "The household budget review is the first Sunday").await;

    let (report, for_model) =
        cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    assert!(
        for_model.is_empty(),
        "a private row was handed to the model path: {for_model:?} (report {report:?})"
    );

    // And the deterministic pass still sees them, because nothing it reads leaves the machine.
    let q = CandidateQuery {
        namespace: Some("personal:finance".into()),
        max_sensitivity: lumberroom_server::domain::types::Sensitivity::Sealed,
        since: None,
        limit: 100,
    };
    let groups = h.repo.exact_duplicates(&h.ctx.cfg.tenant_id, &q).await.unwrap();
    assert_eq!(groups.len(), 1, "the deterministic pass should see private duplicates");
}

#[tokio::test]
async fn a_client_without_may_ingest_cannot_run_the_pass_or_read_the_queue() {
    // The capability gate lives in the router, so a service-level test cannot see it. The owner's
    // own credential reaches every namespace and still carries no mayIngest.
    let h = harness_or_skip!();
    let (status, body) = h.post("/admin/cleanup/run", OWNER_TOKEN, serde_json::json!({})).await;
    assert_eq!(status, 403, "the owner's read-everything token ran the pass: {body}");
    assert!(body.contains("may_ingest"), "the refusal should name the grant to edit: {body}");

    let (status, _) = h.get("/admin/cleanup/proposals", OWNER_TOKEN).await;
    assert_eq!(status, 403);

    let (status, body) = h.post("/admin/cleanup/run", INGEST_TOKEN, serde_json::json!({})).await;
    assert_eq!(status, 200, "the granted client was refused: {body}");
}

#[tokio::test]
async fn the_scheduled_pass_needs_no_principal_and_writes_proposals() {
    // The claim the in-server timer rests on: `run` takes a tenant, so a background task calls it
    // with no caller, no request and no invented identity. A synthetic principal holding `*` at
    // sealed would satisfy the old signature and then be reachable from anywhere.
    let h = harness_or_skip!();
    put_raw(&h, "user:me", "the scratch server refuses port 8787").await;
    put_raw(&h, "user:me", "The scratch server refuses port 8787  ").await;

    let (report, _) =
        cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None)
            .await
            .expect("a pass with no principal should run");
    assert_eq!(report.queued, 1, "the scheduled pass wrote nothing: {report:?}");
}

#[tokio::test]
async fn a_survivor_that_became_true_first_is_swapped_before_the_proposal_is_queued() {
    // The bug this exists for, found by applying a correct proposal against the owner's real store:
    // the pass picks its survivor by created_at or by which wording reads better, and supersession
    // validates on valid time. When those disagree the queue holds a finding nobody can act on.
    let h = harness_or_skip!();
    let older_fact = put_raw(&h, "user:me", "the console renders a supersession chain").await;
    let newer_fact = put_raw(&h, "user:me", "The console renders a supersession chain  ").await;
    // The row written second became true FIRST, which is what a backfill from transcript
    // timestamps produces and what migration 015 warns its values are.
    set_occurred(&h, &older_fact, "2026-08-14T12:55:45Z").await;
    set_occurred(&h, &newer_fact, "2026-08-14T12:54:08Z").await;

    cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let p = rows.iter().find(|p| p.kind == lumberroom_server::domain::cleanup::CleanupKind::Exact).unwrap();

    assert_eq!(
        p.keep_id.as_deref(),
        Some(older_fact.as_str()),
        "the survivor should be the row that became true later, whatever order they were written in"
    );

    // And the proposal actually applies, which is the property the swap exists to give.
    cleanup::apply(&h.ctx, h.repo.as_ref(), &p.id)
        .await
        .expect("a reconciled proposal must be appliable");
}

#[tokio::test]
async fn a_cluster_with_no_valid_time_is_left_alone() {
    // The guard fires on two known dates in the wrong order and nothing else. Most of the store
    // carries no occurred_at, and inventing an ordering for it would be worse than leaving it.
    let h = harness_or_skip!();
    let first = put_raw(&h, "user:me", "the recall monitor compares two scans").await;
    put_raw(&h, "user:me", "The recall monitor compares two scans  ").await;

    cleanup::run(&h.ctx.cfg.tenant_id, h.repo.as_ref(), None, "hourly", 500, None).await.unwrap();
    let rows = cleanup::list(&h.ctx, h.repo.as_ref(), Some("proposed"), 50).await.unwrap();
    let p = rows.iter().find(|p| p.kind == lumberroom_server::domain::cleanup::CleanupKind::Exact).unwrap();
    assert_eq!(p.keep_id.as_deref(), Some(first.as_str()), "the oldest should still survive");
}
