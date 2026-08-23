//! Transcript ingestion, server side. Real Postgres with pgvector, the hash embedder, the same
//! `lumberroom_rust_test` database as the rest of the suite. Skipped when no database is reachable.
//!
//!   DATABASE_URL=postgres://lumberroom:pw@127.0.0.1:5432/lumberroom cargo test --test ingest
//!
//! Four properties are the reason this file exists, and each of them has already been got wrong
//! once in the design.
//!
//! Approval goes through `services::write::run`, so a credential-shaped proposal inherits the
//! tripwire refusal instead of finding a second write path with no checks on it.
//!
//! The watermark advances to the first byte of the earliest unextracted span, never past it. Going
//! further loses transcript bytes with no recovery.
//!
//! A file with no surviving span still advances, or most of the corpus stalls forever.
//!
//! The emission check matches on content hash inside a window, tenant-wide and never on a session
//! id. A session-keyed check fires never, which is what the first version of it did.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::{Duration, Utc};
use sqlx::PgPool;
use lumberroom_server::adapters::auth;
use lumberroom_server::adapters::embedding::HashEmbedder;
use lumberroom_server::adapters::postgres::{self, PgIngestRepository};
use lumberroom_server::config::{self, Config};
use lumberroom_server::crypto::kek::{EnvKeyProvider, KeyProvider};
use lumberroom_server::domain::policy::NamespaceGrant;
use lumberroom_server::domain::types::{Invocation, Principal};
use lumberroom_server::mcp::AppState;
use lumberroom_server::ports::ingest::{IngestRepository, NewProposal, ProposalFilter, ProposalSource};
use lumberroom_server::ports::OauthStore;
use lumberroom_server::services::{bootstrap, ingest, search, write, Ctx, Repos};

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
const CREDENTIAL: &str = "postgres://lumberroom:s3cr3tPassw0rd@db.internal:5432/lumberroom";

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
    repo: Arc<dyn IngestRepository>,
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

    /// Wait for the fire-and-forget emission writer to land.
    ///
    /// `record_emissions` is spawned by contract, so a read never waits on it and never fails
    /// because of it. A test that asserted straight after the read would be racing the task rather
    /// than testing the loop.
    async fn wait_for_emissions(&self, expected: i64) {
        for _ in 0..100 {
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM recall_emission")
                .fetch_one(&self.pool)
                .await
                .unwrap();
            if rows >= expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("no emission was recorded within two seconds");
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

    let repo: Arc<dyn IngestRepository> = Arc::new(PgIngestRepository::new(pool.clone()));

    let oauth: Arc<dyn OauthStore> = Arc::new(postgres::PgOauthStore::new(pool.clone()));
    let state = Arc::new(AppState {
        cleanup: Arc::new(postgres::PgCleanupRepository::new(pool.clone())),
        aliases: Arc::new(postgres::PgAliasRepository::new(pool.clone())),
        cfg: Arc::clone(&ctx.cfg),
        repos: ctx.repos.clone(),
        oauth: Arc::clone(&oauth),
        ingest: Arc::clone(&repo),
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

fn source(file: &str, entry: &str, speaker: &str, run_id: uuid::Uuid) -> ProposalSource {
    ProposalSource {
        source_key: format!("{file}#{entry}"),
        file_path: file.into(),
        session_id: Some("transcript-session".into()),
        is_sidechain: false,
        entry_uuid: Some(entry.into()),
        speaker: speaker.into(),
        observed_at: Some(Utc::now()),
        run_id,
    }
}

fn fact(content: &str, speaker: &str, src: ProposalSource) -> ingest::FactInput {
    ingest::FactInput {
        content: content.into(),
        namespace: "user:me".into(),
        tags: vec!["preference".into()],
        supersedes: None,
        speaker: speaker.into(),
        quote: None,
        span_text: None,
        source: src,
    }
}

async fn open_run(h: &Harness) -> uuid::Uuid {
    ingest::open_run(&h.ctx, h.repo.as_ref(), "agent:claude-code", serde_json::json!({"roots": []}))
        .await
        .unwrap()
}

// -- the queue ---------------------------------------------------------------------------------

/// Post, list, show, and post the same fact again. The second arrival is a source row on the row
/// that already exists, which is what turns "this appeared 808 times" into one question.
#[tokio::test]
async fn a_proposal_round_trips_and_a_repeat_adds_a_source() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let first = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(
            "Dana keeps the dev Postgres on port 5433",
            "main_model",
            source("/p/a.jsonl", "e1", "main_model", run),
        )],
    )
    .await
    .unwrap();
    assert_eq!(first.proposals_new, 1, "{:?}", first.outcomes);

    let id = match first.outcomes[0] {
        ingest::FactOutcome::Proposed { id, auto } => {
            assert!(!auto, "a main_model fact never auto-approves");
            id
        }
        ref other => panic!("expected a proposal, got {other:?}"),
    };

    let second = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(
            // The same fact in a different shape. The normaliser is what makes them one row.
            "  dana keeps the dev postgres on port 5433.  ",
            "owner_typed",
            source("/p/b.jsonl", "e2", "owner_typed", run),
        )],
    )
    .await
    .unwrap();
    assert_eq!(second.proposals_new, 0);
    assert_eq!(second.proposals_reinforced, 1, "{:?}", second.outcomes);

    let (proposal, sources) = ingest::show(&h.ctx, h.repo.as_ref(), id).await.unwrap();
    assert_eq!(proposal.state, "proposed");
    assert_eq!(proposal.speaker, "main_model", "the frozen speaker is never upgraded");
    assert!(!proposal.auto, "auto is frozen at first insert too");
    assert_eq!(sources.len(), 2);
    assert_eq!(
        ingest::strongest_speaker(&sources).map(|s| s.speaker.as_str()),
        Some("owner_typed"),
        "the stronger speaker shows on read without touching the parent row"
    );

    let queued = ingest::list(
        &h.ctx,
        h.repo.as_ref(),
        ProposalFilter { state: Some("proposed".into()), limit: 50, ..Default::default() },
    )
    .await
    .unwrap();
    assert_eq!(queued.len(), 1);
}

/// Approval is the only path into the store, and it is `write::run`.
#[tokio::test]
async fn approving_a_proposal_writes_it_through_the_write_path() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let posted = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(
            "Dana deploys lumberroom on an Oracle Ampere A1 instance",
            "main_model",
            source("/p/a.jsonl", "e1", "main_model", run),
        )],
    )
    .await
    .unwrap();
    let id = match posted.outcomes[0] {
        ingest::FactOutcome::Proposed { id, .. } => id,
        ref other => panic!("expected a proposal, got {other:?}"),
    };

    let outcome = ingest::approve(&h.ctx, h.repo.as_ref(), id).await.unwrap();
    assert!(outcome.refused.is_none(), "{:?}", outcome.refused);
    let memory_id = outcome.memory_id.expect("an approval returns the memory it wrote");

    let stored = h.ctx.repos.memories.find_by_id(h.ctx.tenant(), memory_id).await.unwrap();
    let stored = stored.expect("the memory exists");
    assert_eq!(stored.content, "Dana deploys lumberroom on an Oracle Ampere A1 instance");
    assert_eq!(stored.namespace, "user:me");

    let proposal = h.repo.proposal(h.ctx.tenant(), id).await.unwrap().unwrap();
    assert_eq!(proposal.state, "written");
    assert_eq!(proposal.memory_id, Some(memory_id));
    assert!(proposal.last_error.is_none());
}

/// The reason approval is one call: everything `memory_write` refuses, this refuses.
///
/// Seeded through the repository rather than through `post`, because the tripwire at post time
/// stops this content before a row exists. What is under test here is the approval path inheriting
/// the refusal, which is the check a proposal store with its own insert would lose.
#[tokio::test]
async fn a_credential_shaped_proposal_is_refused_on_approval_and_says_why() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let seeded = h
        .repo
        .insert_proposal(
            h.ctx.tenant(),
            NewProposal {
                fingerprint: ingest::fingerprint(CREDENTIAL),
                content: CREDENTIAL.into(),
                namespace: "user:me".into(),
                tags: vec![],
                supersedes: None,
                speaker: "owner_typed".into(),
                quote: Some(CREDENTIAL.into()),
                auto: true,
                extractor: "agent:claude-code".into(),
                source: source("/p/a.jsonl", "e1", "owner_typed", run),
            },
        )
        .await
        .unwrap();
    let id = seeded.proposal().id;

    let outcome = ingest::approve(&h.ctx, h.repo.as_ref(), id).await.unwrap();
    let refusal = outcome.refused.expect("write::run refuses credential-shaped content at open");
    assert!(refusal.contains("connection_string_password"), "{refusal}");
    assert!(!refusal.contains("s3cr3tPassw0rd"), "the matched secret never travels: {refusal}");
    assert!(outcome.memory_id.is_none());

    let proposal = h.repo.proposal(h.ctx.tenant(), id).await.unwrap().unwrap();
    assert_eq!(proposal.state, "proposed", "a refused proposal stays in the queue");
    assert!(proposal.last_error.unwrap().contains("connection_string_password"));
    assert!(proposal.last_error_at.is_some());

    let written: i64 =
        sqlx::query_scalar("SELECT count(*) FROM memory").fetch_one(&h.pool).await.unwrap();
    assert_eq!(written, 0, "nothing reached the store");
}

/// The tripwire runs before a proposal exists, so credential-shaped content never lands in a table.
#[tokio::test]
async fn the_tripwire_refuses_a_credential_before_a_proposal_exists() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let report = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(CREDENTIAL, "owner_typed", source("/p/a.jsonl", "e1", "owner_typed", run))],
    )
    .await
    .unwrap();

    assert_eq!(report.refused, 1);
    assert_eq!(report.proposals_new, 0);
    assert_eq!(
        report.outcomes[0],
        ingest::FactOutcome::Refused { rule: "connection_string_password" }
    );

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_proposal")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn a_credential_hiding_in_the_quote_is_refused_with_the_content_clean() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    // The quote is stored, printed by `ingest show`, and kept through a rejection. An extractor
    // handed a clean sentence and a dirty span can put one in each field, so both are scanned.
    let mut f = fact(
        "the owner deploys the box by hand",
        "owner_typed",
        source("/p/a.jsonl", "e1", "owner_typed", run),
    );
    f.quote = Some(CREDENTIAL.to_string());

    let report = ingest::post(&h.ctx, h.repo.as_ref(), "agent:claude-code", vec![f]).await.unwrap();

    assert_eq!(report.refused, 1);
    assert_eq!(report.proposals_new, 0);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_proposal")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "a refused fact never becomes a row that holds the credential");
}

/// A rejection blocks its fingerprint until the owner takes it back by hand.
#[tokio::test]
async fn a_rejected_fingerprint_stays_blocked_until_it_is_unrejected() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;
    let content = "Dana reviews the ingest queue on Fridays";

    let posted = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(content, "main_model", source("/p/a.jsonl", "e1", "main_model", run))],
    )
    .await
    .unwrap();
    let id = match posted.outcomes[0] {
        ingest::FactOutcome::Proposed { id, .. } => id,
        ref other => panic!("expected a proposal, got {other:?}"),
    };

    assert!(ingest::reject(&h.ctx, h.repo.as_ref(), id, Some("not durable")).await.unwrap());
    assert!(
        !ingest::reject(&h.ctx, h.repo.as_ref(), id, None).await.unwrap(),
        "a second rejection is not a second decision"
    );

    let again = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(content, "main_model", source("/p/b.jsonl", "e2", "main_model", run))],
    )
    .await
    .unwrap();
    assert_eq!(again.blocked, 1, "{:?}", again.outcomes);
    assert_eq!(again.proposals_new, 0);
    assert!(ingest::approve(&h.ctx, h.repo.as_ref(), id).await.is_err());

    assert!(ingest::unreject(&h.ctx, h.repo.as_ref(), id).await.unwrap());
    let back = h.repo.proposal(h.ctx.tenant(), id).await.unwrap().unwrap();
    assert_eq!(back.state, "proposed");
    assert!(back.decided_at.is_some(), "the earlier rejection stays visible after the undo");
}

// -- the watermark -----------------------------------------------------------------------------

/// The hold-back rule. A file with spans in a missing chunk advances to the first byte of the
/// earliest one, and not one byte further.
#[tokio::test]
async fn the_watermark_advances_to_the_earliest_unextracted_span() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let report = ingest::advance_watermarks(
        &h.ctx,
        h.repo.as_ref(),
        run,
        &[ingest::FileAdvance {
            file_path: "/p/big.jsonl".into(),
            session_id: Some("s1".into()),
            is_sidechain: false,
            plan_ceiling: 20_000,
            prefix_sha256: "abc".into(),
            entries_seen: 900,
            // Chunks 401 to 405 never came back. Their spans start here.
            unextracted_from: vec![9_100, 4_200, 12_600],
        }],
    )
    .await
    .unwrap();

    assert_eq!(report.advanced, vec![("/p/big.jsonl".to_string(), 4_200)]);
    assert_eq!(
        report.held_back,
        vec![ingest::HeldBack {
            file: "/p/big.jsonl".into(),
            held_at: 4_200,
            ceiling: 20_000
        }],
        "a held-back file is named, or the owner learns nothing from the report"
    );

    let mark = h.repo.watermark(h.ctx.tenant(), "/p/big.jsonl").await.unwrap().unwrap();
    assert_eq!(mark.byte_offset, 4_200, "advancing past unextracted bytes has no recovery");
    assert_eq!(mark.last_run_id, Some(run));
}

/// Most of the corpus is this case: read, classified, excluded, nothing to extract. A rule that
/// held these back would stall the watermark on almost every file forever.
#[tokio::test]
async fn a_file_with_no_surviving_spans_still_advances_to_the_ceiling() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let report = ingest::advance_watermarks(
        &h.ctx,
        h.repo.as_ref(),
        run,
        &[ingest::FileAdvance {
            file_path: "/p/quiet.jsonl".into(),
            session_id: None,
            is_sidechain: true,
            plan_ceiling: 51_200,
            prefix_sha256: "def".into(),
            entries_seen: 4_000,
            unextracted_from: vec![],
        }],
    )
    .await
    .unwrap();

    assert_eq!(report.advanced, vec![("/p/quiet.jsonl".to_string(), 51_200)]);
    assert!(report.held_back.is_empty());
}

/// Two runs overlap on an ordinary Tuesday. The older one finishing last must not drag the mark
/// backwards, or everything between the two ceilings is read and proposed again.
#[tokio::test]
async fn an_older_runs_advance_cannot_rewind_the_watermark() {
    let h = harness_or_skip!();
    let newer = open_run(&h).await;
    let older = open_run(&h).await;

    let file = |ceiling: i64, hash: &str| ingest::FileAdvance {
        file_path: "/p/live.jsonl".into(),
        session_id: Some("s1".into()),
        is_sidechain: false,
        plan_ceiling: ceiling,
        prefix_sha256: hash.into(),
        entries_seen: ceiling / 10,
        unextracted_from: vec![],
    };

    ingest::advance_watermarks(&h.ctx, h.repo.as_ref(), newer, &[file(80_000, "newer")])
        .await
        .unwrap();
    let report =
        ingest::advance_watermarks(&h.ctx, h.repo.as_ref(), older, &[file(30_000, "older")])
            .await
            .unwrap();

    assert_eq!(
        report.advanced,
        vec![("/p/live.jsonl".to_string(), 80_000)],
        "the stored offset is reported, not the one this run asked for"
    );
    let mark = h.repo.watermark(h.ctx.tenant(), "/p/live.jsonl").await.unwrap().unwrap();
    assert_eq!(mark.byte_offset, 80_000);
    assert_eq!(mark.prefix_sha256, "newer", "the hash belongs to whichever offset won");
    assert_eq!(mark.last_run_id, Some(newer), "a losing run leaves no trace on the row");
}

// -- the emission check ------------------------------------------------------------------------

/// The anti-loop layer, and the test proving it fires at all.
///
/// The store handed this content out, the transcript recorded it afterwards, and it comes back as
/// an extracted fact. That is an echo: the memory is confirmed and no proposal is created.
#[tokio::test]
async fn content_the_store_emitted_comes_back_as_a_confirmation() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;
    let content = "Dana runs the lumberroom server behind Cloudflare Tunnel";

    let written = write::run(&h.ctx, content, "user:me", None, None, None, None).await.unwrap();
    let memory_id = uuid::Uuid::parse_str(&written.id).unwrap();
    ingest::record_emission(&h.ctx, h.repo.as_ref(), content, memory_id, "context_bootstrap")
        .await
        .unwrap();

    // A different shape of the same sentence, which is what a transcript holds after a digest went
    // through a model. One normaliser on both sides is the only reason these hashes meet.
    let report = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(
            "dana runs the lumberroom server behind cloudflare tunnel.",
            "main_model",
            source("/p/a.jsonl", "e1", "main_model", run),
        )],
    )
    .await
    .unwrap();

    assert_eq!(report.confirmations, 1, "{:?}", report.outcomes);
    assert_eq!(report.proposals_new, 0);
    assert_eq!(report.outcomes[0], ingest::FactOutcome::Confirmed { memory_id });

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_proposal")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "an echo asks the owner nothing");

    let confirmed: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT last_confirmed_at FROM memory WHERE id = $1")
            .bind(memory_id)
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert!(confirmed.is_some(), "a hit confirms the memory it matched");
}

/// The window is the whole distinction between an echo and a coincidence, in both directions.
///
/// A span written before the store ever emitted the content cannot be quoting it, and an emission
/// from outside the window is a fact the owner has restated rather than a loop.
#[tokio::test]
async fn an_emission_outside_the_window_is_not_an_echo() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;

    let earlier_span = "The lumberroom console listens on port 8787";
    let ancient = "Dana keeps their transcripts under the projects directory";

    for content in [earlier_span, ancient] {
        let written = write::run(&h.ctx, content, "user:me", None, None, None, None).await.unwrap();
        let memory_id = uuid::Uuid::parse_str(&written.id).unwrap();
        ingest::record_emission(&h.ctx, h.repo.as_ref(), content, memory_id, "memory_search")
            .await
            .unwrap();
    }

    // The span was recorded two hours before the store emitted anything, which is more than the
    // five minutes of clock slack. The transcript cannot be quoting the store.
    let mut before =
        fact(earlier_span, "main_model", source("/p/a.jsonl", "e1", "main_model", run));
    before.source.observed_at = Some(Utc::now() - Duration::hours(2));

    // The span sits a hundred days after the emission, which is outside the ninety-day window.
    let mut long_after = fact(ancient, "main_model", source("/p/b.jsonl", "e2", "main_model", run));
    long_after.source.observed_at = Some(Utc::now() + Duration::days(100));

    let report =
        ingest::post(&h.ctx, h.repo.as_ref(), "agent:claude-code", vec![before, long_after])
            .await
            .unwrap();

    assert_eq!(report.confirmations, 0, "{:?}", report.outcomes);
    assert_eq!(report.proposals_new, 2, "{:?}", report.outcomes);
}

// -- the capability, and the routes -------------------------------------------------------------

/// The gate, and the reason it exists. The owner's own token reaches every namespace at every
/// level and still may not ingest, because filling a queue somebody has to read is a thing the
/// owner asks for by name.
#[tokio::test]
async fn an_ingest_route_refuses_a_client_the_owner_did_not_grant_it_to() {
    let h = harness_or_skip!();

    let (status, body) = h
        .post(
            "/admin/ingest/runs",
            OWNER_TOKEN,
            serde_json::json!({ "extractor": "agent:claude-code", "scope": {} }),
        )
        .await;

    assert_eq!(status, 403, "{body}");
    // The refusal names the flag. An error that said only "forbidden" would send the owner to read
    // a log for a grant he can edit.
    assert!(body.contains("may_ingest"), "{body}");
    assert!(body.contains("mayIngest"), "{body}");

    let runs: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ingest_run").fetch_one(&h.pool).await.unwrap();
    assert_eq!(runs, 0, "a refused route opened nothing");
}

/// The granted client walks the surface: open a run, post a proposal, read it back off the queue.
#[tokio::test]
async fn the_granted_client_opens_a_run_posts_and_lists() {
    let h = harness_or_skip!();

    let (status, body) = h
        .post(
            "/admin/ingest/runs",
            INGEST_TOKEN,
            serde_json::json!({ "extractor": "agent:claude-code", "scope": { "roots": [] } }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let run_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["run_id"]
        .as_str()
        .expect("a run id comes back")
        .to_string();

    let (status, body) = h
        .post(
            "/admin/ingest/proposals",
            INGEST_TOKEN,
            serde_json::json!({
                "extractor": "agent:claude-code",
                "facts": [{
                    "content": "Dana runs the builder image because there is no local cargo",
                    "namespace": "user:me",
                    "tags": ["preference"],
                    "speaker": "main_model",
                    "source": {
                        "file_path": "/p/a.jsonl",
                        "entry_uuid": "e1",
                        "run_id": run_id,
                    },
                }],
            }),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["proposals_new"], 1, "{body}");
    assert_eq!(report["outcomes"][0]["outcome"], "proposed");

    let (status, body) = h.get("/admin/ingest/proposals?state=proposed", INGEST_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows = listed["proposals"].as_array().expect("a list comes back");
    assert_eq!(rows.len(), 1, "{body}");
    assert_eq!(rows[0]["namespace"], "user:me");
    assert_eq!(rows[0]["auto"], false, "a model speaker never writes itself");

    // The source key the server built from file and entry, which is what makes a re-post idempotent
    // at the source grain.
    let id = rows[0]["id"].as_str().unwrap();
    let (status, body) = h.get(&format!("/admin/ingest/proposals/{id}"), INGEST_TOKEN).await;
    assert_eq!(status, 200, "{body}");
    let shown: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(shown["sources"][0]["source_key"], "/p/a.jsonl#e1");
    assert_eq!(shown["strongest_speaker"], "main_model");
}

/// The anti-loop layer end to end, with nothing seeded by hand.
///
/// A memory is written, `memory_search` hands it back to a client, and the same content arrives as
/// an extracted fact. That is the digest loop, and it has to come back as a confirmation rather
/// than as a question the owner is asked again. The emission and the proposal meet only because
/// both hashes come from `ingest::fingerprint`; a second normaliser anywhere breaks this test and
/// nothing else.
#[tokio::test]
async fn a_fact_read_back_through_search_returns_as_a_confirmation() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;
    let content = "Dana keeps the lumberroom builder image on the deploy box";

    let written = write::run(&h.ctx, content, "user:me", None, None, None, None).await.unwrap();
    let memory_id = uuid::Uuid::parse_str(&written.id).unwrap();

    // The read that hands the content out. Nothing else records an emission, so this call is the
    // only reason the check below can fire.
    let found = search::run(&h.ctx, content, None, Some(5), None, None, None).await.unwrap();
    assert!(found.hits.iter().any(|hit| hit.id == written.id), "the search returned the row");
    h.wait_for_emissions(1).await;

    let recorded: (String, i64) = sqlx::query_as(
        "SELECT tool, emit_count FROM recall_emission WHERE content_sha256 = $1 AND memory_id = $2",
    )
    .bind(ingest::fingerprint(content))
    .bind(memory_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(recorded.0, "memory_search");
    assert_eq!(recorded.1, 1);

    // The transcript's version, reshaped the way a model would restate it. Same fingerprint.
    let report = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(
            "Dana keeps the lumberroom builder image on the deploy box.",
            "main_model",
            source("/p/a.jsonl", "e1", "main_model", run),
        )],
    )
    .await
    .unwrap();

    assert_eq!(report.confirmations, 1, "{:?}", report.outcomes);
    assert_eq!(report.proposals_new, 0, "{:?}", report.outcomes);
    assert_eq!(report.outcomes[0], ingest::FactOutcome::Confirmed { memory_id });

    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM ingest_proposal")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(queued, 0, "an echo asks the owner nothing");
}

/// The same loop through the other emitting path. `context_bootstrap` is what a session opens with,
/// so it is the one that puts the digest in front of a model in the first place.
#[tokio::test]
async fn a_fact_read_back_through_the_digest_returns_as_a_confirmation() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;
    let content = "Dana prefers the object form when writing a namespace grant";

    let written = write::run(&h.ctx, content, "user:me", None, None, None, None).await.unwrap();
    let memory_id = uuid::Uuid::parse_str(&written.id).unwrap();

    let digest = bootstrap::run(&h.ctx, None).await.unwrap();
    assert!(!digest.cached, "the first call builds rather than replaying");
    h.wait_for_emissions(1).await;

    let tool: String = sqlx::query_scalar(
        "SELECT tool FROM recall_emission WHERE content_sha256 = $1 AND memory_id = $2",
    )
    .bind(ingest::fingerprint(content))
    .bind(memory_id)
    .fetch_one(&h.pool)
    .await
    .unwrap();
    assert_eq!(tool, "context_bootstrap");

    let report = ingest::post(
        &h.ctx,
        h.repo.as_ref(),
        "agent:claude-code",
        vec![fact(content, "main_model", source("/p/b.jsonl", "e2", "main_model", run))],
    )
    .await
    .unwrap();
    assert_eq!(report.outcomes[0], ingest::FactOutcome::Confirmed { memory_id });
}

// -- the valid-time fill -------------------------------------------------------------------------

/// One source row with the timestamp the span carried, or none.
fn observed(file: &str, entry: &str, run_id: uuid::Uuid, at: Option<chrono::DateTime<Utc>>) -> ProposalSource {
    ProposalSource { observed_at: at, ..source(file, entry, "owner_typed", run_id) }
}

fn dated_fact(content: &str, src: ProposalSource) -> ingest::FactInput {
    fact(content, "owner_typed", src)
}

async fn queue(h: &Harness, fact: ingest::FactInput) -> ingest::FactOutcome {
    let posted = ingest::post(&h.ctx, h.repo.as_ref(), "agent:claude-code", vec![fact]).await.unwrap();
    posted.outcomes.into_iter().next().expect("one fact in, one outcome out")
}

/// The fill takes the earliest moment a source was recorded stating the fact.
///
/// The later source arrives first, so a fill that took whichever source it saw first would pass a
/// min-of-one assertion and fail this one. What the value means is narrow: it is when the owner
/// said it, which is an upper bound on when it became true and the tightest bound this store holds.
#[tokio::test]
async fn an_approved_proposal_takes_the_earliest_observation_as_its_valid_time() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;
    let content = "Dana runs the dev Postgres on port 5433 zqxfillzqx";
    let early = Utc::now() - Duration::days(200);
    let late = Utc::now() - Duration::days(60);

    let first = queue(&h, dated_fact(content, observed("/p/late.jsonl", "e1", run, Some(late)))).await;
    let id = match first {
        ingest::FactOutcome::Proposed { id, .. } => id,
        other => panic!("expected a proposal, got {other:?}"),
    };
    let second = queue(&h, dated_fact(content, observed("/p/early.jsonl", "e2", run, Some(early)))).await;
    assert!(
        matches!(second, ingest::FactOutcome::Reinforced { .. }),
        "the same fact twice is one proposal with two sources, got {second:?}"
    );
    assert_eq!(ingest::show(&h.ctx, h.repo.as_ref(), id).await.unwrap().1.len(), 2);

    let outcome = ingest::approve(&h.ctx, h.repo.as_ref(), id).await.unwrap();
    assert!(outcome.refused.is_none(), "{:?}", outcome.refused);
    let stored = h
        .ctx
        .repos
        .memories
        .find_by_id(h.ctx.tenant(), outcome.memory_id.unwrap())
        .await
        .unwrap()
        .expect("the memory exists");

    let filled = stored.occurred_at.expect("a dated proposal fills occurred_at");
    assert!(
        (filled - early).num_seconds().abs() <= 1,
        "the fill has to be the earliest observation: got {filled}, wanted {early}"
    );
    assert!(
        (filled - late).num_seconds().abs() > 1,
        "and it must not be the source that arrived first"
    );
}

/// A proposal no source could date approves with no valid time at all.
///
/// `now()` is the wrong answer and it is the tempting one: it would stamp the approval clock into a
/// column that means when the fact held in the world, and every undated row in the store would then
/// claim to have become true on the day somebody pressed approve. An absent date stays absent.
#[tokio::test]
async fn a_proposal_no_source_could_date_approves_with_no_valid_time() {
    let h = harness_or_skip!();
    let run = open_run(&h).await;
    let before = Utc::now();

    let queued =
        queue(&h, dated_fact("Dana prefers tabs over spaces zqxundatedzqx", observed("/p/a.jsonl", "e1", run, None)))
            .await;
    let id = match queued {
        ingest::FactOutcome::Proposed { id, .. } => id,
        other => panic!("expected a proposal, got {other:?}"),
    };
    let sources = ingest::show(&h.ctx, h.repo.as_ref(), id).await.unwrap().1;
    assert_eq!(sources.len(), 1);
    assert!(sources[0].observed_at.is_none(), "the undated source must stay undated in the store");

    let outcome = ingest::approve(&h.ctx, h.repo.as_ref(), id).await.unwrap();
    assert!(outcome.refused.is_none(), "{:?}", outcome.refused);
    let memory_id = outcome.memory_id.unwrap();
    let stored = h
        .ctx
        .repos
        .memories
        .find_by_id(h.ctx.tenant(), memory_id)
        .await
        .unwrap()
        .expect("the memory exists");
    assert!(stored.occurred_at.is_none(), "an undated proposal wrote {:?}", stored.occurred_at);
    assert!(stored.occurred_until.is_none());

    // The row still knows when the store learned it, which is the clock `occurred_at` is not.
    assert!(stored.created_at >= before, "created_at is transaction time and is still set");
}
