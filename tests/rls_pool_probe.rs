//! Throwaway probe. Does sqlx's PgPool leak an RLS tenant across a connection release?
//!
//! Delete after reading the answer.

use sqlx::{postgres::PgPoolOptions, Executor, PgPool, Row};

const ALICE: &str = "11111111-1111-1111-1111-111111111111";
const BOB: &str = "22222222-2222-2222-2222-222222222222";

fn urls() -> Option<(String, String, String)> {
    let base = std::env::var("DATABASE_URL").ok()?;
    let cut = base.rfind('/')?;
    let (prefix, _) = base.split_at(cut);
    let admin = format!("{prefix}/postgres");
    let probe = format!("{prefix}/rls_pool_probe");
    // Same host and port, but the unprivileged role the app would really run as.
    let at = prefix.rfind('@')?;
    let hostpart = &prefix[at + 1..];
    let scheme = prefix.split("://").next()?;
    let app = format!("{scheme}://app:probe@{hostpart}/rls_pool_probe");
    Some((admin, probe, app))
}

async fn setup(admin: &str, probe: &str) -> PgPool {
    let a = PgPool::connect(admin).await.expect("admin connect");
    let _ = a.execute("DROP DATABASE IF EXISTS rls_pool_probe").await;
    let _ = a.execute("DROP ROLE IF EXISTS app").await;
    a.execute("CREATE DATABASE rls_pool_probe").await.expect("create db");
    a.execute("CREATE ROLE app LOGIN PASSWORD 'probe'").await.expect("create role");
    a.close().await;

    let p = PgPool::connect(probe).await.expect("probe connect");
    for stmt in [
        "CREATE TABLE memory (id serial primary key, tenant_id uuid not null, content text)",
        "ALTER TABLE memory ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE memory FORCE ROW LEVEL SECURITY",
        "CREATE POLICY tenant_isolation ON memory \
           USING (tenant_id = NULLIF(current_setting('app.tenant_id', true), '')::uuid) \
           WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant_id', true), '')::uuid)",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON memory TO app",
        "GRANT USAGE, SELECT ON SEQUENCE memory_id_seq TO app",
    ] {
        p.execute(stmt).await.expect(stmt);
    }
    sqlx::query("INSERT INTO memory (tenant_id, content) VALUES ($1::uuid,'alice secret'),($2::uuid,'bob secret')")
        .bind(ALICE).bind(BOB).execute(&p).await.expect("seed");
    p
}

async fn setup2(admin: &str, probe: &str) -> PgPool {
    let probe = probe.replace("rls_pool_probe", "rls_pool_probe2");
    let a = PgPool::connect(admin).await.expect("admin connect");
    let _ = a.execute("DROP DATABASE IF EXISTS rls_pool_probe2 WITH (FORCE)").await;
    let _ = a.execute("DROP ROLE IF EXISTS app2").await;
    a.execute("CREATE DATABASE rls_pool_probe2").await.expect("create db2");
    a.execute("CREATE ROLE app2 LOGIN PASSWORD 'probe'").await.expect("create role2");
    a.close().await;
    let p = PgPool::connect(&probe).await.expect("probe2 connect");
    for stmt in [
        "CREATE TABLE memory (id serial primary key, tenant_id uuid not null, content text)",
        "ALTER TABLE memory ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE memory FORCE ROW LEVEL SECURITY",
        "CREATE POLICY tenant_isolation ON memory \
           USING (tenant_id = NULLIF(current_setting('app.tenant_id', true), '')::uuid) \
           WITH CHECK (tenant_id = NULLIF(current_setting('app.tenant_id', true), '')::uuid)",
        "GRANT SELECT, INSERT, UPDATE, DELETE ON memory TO app2",
        "GRANT USAGE, SELECT ON SEQUENCE memory_id_seq TO app2",
    ] {
        p.execute(stmt).await.expect(stmt);
    }
    sqlx::query("INSERT INTO memory (tenant_id, content) VALUES ($1::uuid,'alice secret'),($2::uuid,'bob secret')")
        .bind(ALICE).bind(BOB).execute(&p).await.expect("seed2");
    p
}

async fn visible(pool: &PgPool) -> i64 {
    sqlx::query("SELECT count(*) AS c FROM memory")
        .fetch_one(pool)
        .await
        .expect("count")
        .get::<i64, _>("c")
}

#[tokio::test]
async fn pool_release_does_not_leak_tenant() {
    let Some((admin, probe, app_url)) = urls() else {
        eprintln!("SKIP: DATABASE_URL unset");
        return;
    };
    let owner = setup(&admin, &probe).await;

    // max_connections = 1 forces every request onto the SAME physical connection, which is the
    // only configuration where a leak is observable at all.
    let app = PgPoolOptions::new().max_connections(1).connect(&app_url).await.expect("app connect");

    // 1. transaction-local set_config, the shape the design proposes.
    let mut tx = app.begin().await.expect("begin");
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(ALICE)
        .execute(&mut *tx)
        .await
        .expect("set");
    let seen: String = sqlx::query("SELECT content FROM memory")
        .fetch_one(&mut *tx)
        .await
        .expect("read")
        .get("content");
    tx.commit().await.expect("commit");
    println!("[1] inside tx as alice: {seen}");
    assert_eq!(seen, "alice secret");

    // 2. same pooled connection, next request forgets the tenant.
    let after_tx = visible(&app).await;
    println!("[2] after commit, no tenant set, rows visible: {after_tx}");

    // 3. a session-level SET, then release. Does sqlx clear it?
    let mut conn = app.acquire().await.expect("acquire");
    // `SET` is a utility statement and takes no bind parameters, so a session-level set has to go
    // through set_config with local = false.
    sqlx::query("SELECT set_config('app.tenant_id', $1, false)")
        .bind(ALICE)
        .execute(&mut *conn)
        .await
        .expect("session set");
    drop(conn); // returns to the pool
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let after_release = visible(&app).await;
    println!("[3] after session SET then pool release, rows visible: {after_release}");

    // 4. a rolled-back transaction.
    let mut tx = app.begin().await.expect("begin2");
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(BOB)
        .execute(&mut *tx)
        .await
        .expect("set2");
    tx.rollback().await.expect("rollback");
    let after_rollback = visible(&app).await;
    println!("[4] after rollback, rows visible: {after_rollback}");

    app.close().await;
    let a = PgPool::connect(&admin).await.expect("admin reconnect");
    let _ = a.execute("DROP DATABASE IF EXISTS rls_pool_probe WITH (FORCE)").await;
    let _ = a.execute("DROP ROLE IF EXISTS app").await;
    a.close().await;
    owner.close().await;

    println!(
        "--- unhardened pool: tx={after_tx} release={after_release} rollback={after_rollback}"
    );
    assert_eq!(after_tx, 0, "LEAK: tenant survived commit");
}

/// The same probe against a pool that scrubs the connection on release.
#[tokio::test]
async fn hardened_pool_clears_tenant_on_release() {
    let Some((admin, probe, app_url)) = urls() else {
        return;
    };
    let app_url = app_url.replace("//app:", "//app2:").replace("rls_pool_probe", "rls_pool_probe2");
    let owner = setup2(&admin, &probe).await;

    let app = PgPoolOptions::new()
        .max_connections(1)
        // sqlx does not reset session state on its own. Without this, one stray session-level
        // set_config outlives the request that made it and every later transaction-local set
        // reverts to it rather than to unset.
        //
        // RESET ALL rather than DISCARD ALL. DISCARD ALL includes DEALLOCATE ALL, which drops the
        // prepared statements sqlx still believes it has cached on that connection; the next query
        // fails with 26000 prepared statement "sqlx_s_2" does not exist.
        .after_release(|conn, _meta| {
            Box::pin(async move {
                conn.execute("RESET ALL").await?;
                Ok(true)
            })
        })
        .connect(&app_url)
        .await
        .expect("app connect");

    let mut conn = app.acquire().await.expect("acquire");
    sqlx::query("SELECT set_config('app.tenant_id', $1, false)")
        .bind(ALICE)
        .execute(&mut *conn)
        .await
        .expect("session set");
    drop(conn);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let after_release = visible(&app).await;
    println!("[H1] hardened pool, after session set + release: {after_release}");

    let mut tx = app.begin().await.expect("begin");
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(BOB)
        .execute(&mut *tx)
        .await
        .expect("set");
    let seen: String = sqlx::query("SELECT content FROM memory")
        .fetch_one(&mut *tx)
        .await
        .expect("read")
        .get("content");
    tx.rollback().await.expect("rollback");
    let after_rollback = visible(&app).await;
    println!("[H2] hardened pool, tx saw: {seen}; after rollback: {after_rollback}");

    app.close().await;
    let a = PgPool::connect(&admin).await.expect("admin reconnect");
    let _ = a.execute("DROP DATABASE IF EXISTS rls_pool_probe2 WITH (FORCE)").await;
    let _ = a.execute("DROP ROLE IF EXISTS app2").await;
    a.close().await;
    owner.close().await;

    assert_eq!(seen, "bob secret");
    assert_eq!(after_release, 0, "LEAK: session set survived a DISCARD ALL release");
    assert_eq!(after_rollback, 0, "LEAK: tenant survived rollback");
}
