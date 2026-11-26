//! PostgreSQL integration tests. Run with a real database:
//!   TEST_DATABASE_URL=postgres://postgres:test@127.0.0.1:55432/content_agent cargo test --test postgres
//! Without TEST_DATABASE_URL these tests skip (they never touch a database you did not name).
mod common;
use common::*;
use content_agent::{
    context::Scope,
    queue::{FailOutcome, NewTask, PgQueue, Queue, KIND_EXTRACT_SOURCE},
    store::{postgres::PgStore, Store},
    testing::{full_ctx, harness, HarnessOptions},
    workflow::WorkflowState,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn pg() -> Option<PgStore> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let s = PgStore::connect(&url, 8).await.expect("connect");
    s.migrate().await.expect("migrations apply");
    Some(s)
}

/// A pool whose search_path is a brand-new schema with all migrations applied: queue tests get their own
/// `queue_tasks` table, so parallel tests cannot steal each other's work.
async fn isolated_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let schema = format!("t_{}", uuid::Uuid::new_v4().simple());
    let admin = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query(&format!("CREATE SCHEMA {schema}")).execute(&admin).await.unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .after_connect(move |conn, _| {
            let q = format!("SET search_path TO {schema}");
            Box::pin(async move { sqlx::query(&q).execute(conn).await.map(|_| ()) })
        })
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    Some(pool)
}

fn uniq(p: &str) -> String {
    format!("{p}_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn full_workflow_on_postgres() {
    let Some(store) = pg().await else { return eprintln!("skipped: TEST_DATABASE_URL not set") };
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blog/why-rust-wins-for-teams"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ARTICLE_HTML, "text/html"))
        .mount(&site)
        .await;
    let z = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "post": { "_id": "zp", "platforms": [{ "platform": "linkedin", "status": "scheduled" }] } })),
        )
        .mount(&z)
        .await;

    let mut h = harness(HarnessOptions { zernio_url: Some(z.uri()), ..Default::default() });
    h.state.queue = Arc::new(PgQueue::new(store.pool.clone()));
    h.state.store = Arc::new(store.clone());
    let api = Api::new(&h);
    let (tenant, brand) = (uniq("tenant"), uniq("brand"));
    let ctx = full_ctx(&tenant, &brand);

    let (job, id) = approved_draft(&api, &h, &ctx, &format!("{}/blog/why-rust-wins-for-teams", site.uri()), &["linkedin"]).await;
    let prep = api
        .ok(
            "prepare_publish_request",
            &ctx,
            json!({ "draft_id": id, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "schedule", "scheduled_at": future() }),
        )
        .await;
    let sub = api.ok("schedule_draft", &ctx, json!({ "publish_request_id": prep["publish_request_id"] })).await;
    assert_eq!(sub["draft_status"], "scheduled_or_published");

    let g = api.ok("get_draft", &ctx, json!({ "draft_id": id })).await;
    let states: Vec<&str> = g["events"].as_array().unwrap().iter().map(|e| e["to"].as_str().unwrap()).collect();
    assert_eq!(states, ["draft_ready", "awaiting_customer_review", "approved", "ready_for_zernio", "submitted_to_zernio", "scheduled_or_published"]);
    assert_eq!(api.ok("get_job", &ctx, json!({ "job_id": job })).await["status"], "scheduled_or_published");
    // isolation holds in SQL, not just in memory
    api.err("get_draft", &full_ctx(&tenant, "other_brand"), json!({ "draft_id": id }), "NOT_FOUND").await;
    api.err("get_draft", &full_ctx("other_tenant", &brand), json!({ "draft_id": id }), "NOT_FOUND").await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tool_invocations WHERE tenant_id=$1").bind(&tenant).fetch_one(&store.pool).await.unwrap();
    assert!(n >= 8, "tool invocations persisted: {n}");
}

#[tokio::test]
async fn idempotent_job_creation_is_race_free() {
    let Some(store) = pg().await else { return };
    let store = Arc::new(store);
    let (tenant, brand, key) = (uniq("t"), uniq("b"), uniq("idem"));
    let mut handles = vec![];
    for _ in 0..12 {
        let (s, t, b, k) = (store.clone(), tenant.clone(), brand.clone(), key.clone());
        handles.push(tokio::spawn(async move {
            let now = chrono::Utc::now();
            s.create_job(content_agent::model::Job {
                id: uniq("job"),
                tenant_id: t,
                brand_id: b,
                actor_id: "u".into(),
                state: WorkflowState::Created,
                source_type: None,
                requested_channels: vec![],
                idempotency_key: Some(k),
                brand_context: Default::default(),
                trace_id: "tr".into(),
                created_at: now,
                updated_at: now,
                completed_at: None,
                error: None,
            })
            .await
            .unwrap()
        }));
    }
    let mut ids = std::collections::HashSet::new();
    let mut created = 0;
    for h in handles {
        let (job, c) = h.await.unwrap();
        ids.insert(job.id);
        created += c as usize;
    }
    assert_eq!((ids.len(), created), (1, 1), "exactly one job wins; everyone else gets it back");
}

#[tokio::test]
async fn optimistic_concurrency_rejects_stale_writers() {
    let Some(store) = pg().await else { return };
    let (tenant, brand) = (uniq("t"), uniq("b"));
    let now = chrono::Utc::now();
    let job = content_agent::model::Job {
        id: uniq("job"),
        tenant_id: tenant.clone(),
        brand_id: brand.clone(),
        actor_id: "u".into(),
        state: WorkflowState::Created,
        source_type: None,
        requested_channels: vec![],
        idempotency_key: None,
        brand_context: Default::default(),
        trace_id: "tr".into(),
        created_at: now,
        updated_at: now,
        completed_at: None,
        error: None,
    };
    store.create_job(job.clone()).await.unwrap();
    let (mut a, mut b) = (job.clone(), job.clone());
    a.state = WorkflowState::Queued;
    b.state = WorkflowState::Cancelled;
    store.save_job(&a, WorkflowState::Created).await.unwrap();
    let e = store.save_job(&b, WorkflowState::Created).await.unwrap_err();
    assert_eq!(e.code, content_agent::error::ErrorCode::InvalidState);
    let scope = Scope { tenant_id: tenant, brand_id: brand };
    assert_eq!(store.get_job(&scope, &job.id).await.unwrap().unwrap().state, WorkflowState::Queued);
}

#[tokio::test]
async fn queue_leases_each_task_once_across_concurrent_workers() {
    let Some(pool) = isolated_pool().await else { return };
    let q = Arc::new(PgQueue::new(pool));
    let scope = Scope { tenant_id: uniq("t"), brand_id: uniq("b") };
    let job = uniq("job");
    for i in 0..24 {
        q.enqueue(NewTask::new(KIND_EXTRACT_SOURCE, &scope, Some(job.clone()), json!({ "i": i }), "corr")).await.unwrap();
    }
    let mut workers = vec![];
    for _ in 0..6 {
        let q = q.clone();
        let job = job.clone();
        workers.push(tokio::spawn(async move {
            let mut mine = vec![];
            let _ = &job;
            while let Some(t) = q.lease(Duration::from_secs(60)).await.unwrap() {
                mine.push(t.payload["i"].as_i64().unwrap());
                tokio::task::yield_now().await;
            }
            mine
        }));
    }
    let mut all = vec![];
    for w in workers {
        all.extend(w.await.unwrap());
    }
    all.sort();
    assert_eq!(all, (0..24).collect::<Vec<_>>(), "every task leased exactly once (SKIP LOCKED)");
}

#[tokio::test]
async fn queue_retry_dead_letter_cancel_and_lease_expiry() {
    let Some(pool) = isolated_pool().await else { return };
    let q = PgQueue::new(pool.clone());
    let scope = Scope { tenant_id: uniq("t"), brand_id: uniq("b") };
    let job = uniq("job");
    let mut n = NewTask::new(KIND_EXTRACT_SOURCE, &scope, Some(job.clone()), json!({}), "corr");
    n.max_attempts = 2;
    let id = q.enqueue(n).await.unwrap();

    let t = q.lease(Duration::from_millis(50)).await.unwrap().unwrap();
    assert_eq!((t.id.as_str(), t.attempts), (id.as_str(), 1));
    tokio::time::sleep(Duration::from_millis(120)).await;
    let t = q.lease(Duration::from_secs(60)).await.unwrap().unwrap();
    assert_eq!(t.attempts, 2, "expired lease is redelivered (at-least-once)");
    assert_eq!(q.fail(&id, "boom", true, Duration::ZERO).await.unwrap(), FailOutcome::DeadLettered, "attempts exhausted");
    assert!(q.stats().await.unwrap().dead >= 1);

    let id2 = q.enqueue(NewTask::new(KIND_EXTRACT_SOURCE, &scope, Some(job.clone()), json!({}), "corr")).await.unwrap();
    assert_eq!(q.cancel_job(&Scope { tenant_id: "other".into(), brand_id: scope.brand_id.clone() }, &job).await.unwrap(), 0);
    assert_eq!(q.cancel_job(&scope, &job).await.unwrap(), 1);
    let state: String = sqlx::query_scalar("SELECT state FROM queue_tasks WHERE id=$1").bind(&id2).fetch_one(&pool).await.unwrap();
    assert_eq!(state, "cancelled");
}
