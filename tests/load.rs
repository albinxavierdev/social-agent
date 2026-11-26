//! Load / backpressure tests. Heavier than the unit suite, so opt-in:
//!   cargo test --release --test load -- --ignored --nocapture
mod common;
use common::*;
use content_agent::{
    testing::{full_ctx, harness, HarnessOptions},
    worker,
};
use serde_json::json;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn site() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blog/why-rust-wins-for-teams"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ARTICLE_HTML, "text/html"))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
#[ignore]
async fn concurrent_ingestion_and_parallel_workers() {
    let s = site().await;
    let h = harness(HarnessOptions::default());
    let api = Arc::new(Api::new(&h));
    let url = format!("{}/blog/why-rust-wins-for-teams", s.uri());
    let started = Instant::now();
    let mut tasks = vec![];
    for i in 0..400 {
        let (api, url) = (api.clone(), url.clone());
        tasks.push(tokio::spawn(async move {
            api.ok("ingest_url", &full_ctx(&format!("tenant_{}", i % 20), "b"), json!({ "urls": [url], "allow_duplicates": true })).await
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let ingest = started.elapsed();
    assert_eq!(h.state.queue.stats().await.unwrap().pending, 400, "backpressure: all work is durably queued, none dropped");

    // 8 workers drain concurrently.
    let drained = Instant::now();
    let mut ws = vec![];
    for _ in 0..8 {
        let st = h.state.clone();
        ws.push(tokio::spawn(async move {
            let mut n = 0;
            while let Some(t) = st.queue.lease(Duration::from_secs(60)).await.unwrap() {
                worker::process(&st, t).await;
                n += 1;
            }
            n
        }));
    }
    let mut total = 0;
    for w in ws {
        total += w.await.unwrap();
    }
    assert_eq!(total, 400);
    println!("ingest 400 in {ingest:?}; extract 400 with 8 workers in {:?}", drained.elapsed());
    assert_eq!(h.state.queue.stats().await.unwrap().pending, 0);
}

#[tokio::test]
#[ignore]
async fn rate_limits_hold_under_burst_and_tenants_do_not_starve_each_other() {
    let mut cfg = content_agent::config::Config::for_tests();
    cfg.rate_tenant_per_min = 100;
    let h = harness(HarnessOptions { cfg, ..Default::default() });
    let api = Arc::new(Api::new(&h));
    let noisy = full_ctx("noisy", "b");
    let mut tasks = vec![];
    for _ in 0..300 {
        let (api, c) = (api.clone(), noisy.clone());
        tasks.push(tokio::spawn(async move { api.call("get_job", &c, json!({ "job_id": "x" })).await.1["error"]["code"].as_str().map(str::to_owned) }));
    }
    let mut limited = 0;
    for t in tasks {
        if t.await.unwrap().as_deref() == Some("RATE_LIMIT_EXCEEDED") {
            limited += 1;
        }
    }
    assert!(limited >= 190, "burst beyond the tenant bucket is rejected ({limited}/300)");
    // A quiet tenant is unaffected by the noisy one.
    assert_ne!(api.call("get_job", &full_ctx("quiet", "b"), json!({ "job_id": "x" })).await.1["error"]["code"], "RATE_LIMIT_EXCEEDED");
}
