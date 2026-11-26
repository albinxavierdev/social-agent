mod common;
use async_trait::async_trait;
use common::*;
use content_agent::{
    extract::{firecrawl::FirecrawlExtractor, ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest},
    testing::{full_ctx, harness, HarnessOptions},
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use wiremock::{
    matchers::{method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

const SPA: &str = r#"<html><head><title>App</title></head><body><div id="root"></div><script src="/a.js"></script><script src="/b.js"></script></body></html>"#;

struct FakeObscura(Arc<AtomicUsize>);
#[async_trait]
impl ContentExtractor for FakeObscura {
    fn name(&self) -> &'static str {
        "obscura"
    }
    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ExtractedContent {
            title: Some("Rendered".into()),
            author: None,
            published_at: None,
            content_type: "article".into(),
            language: "en".into(),
            text: "Rendered by a real browser. This landing page only exists after JavaScript has executed in the page.".into(),
            summary: "Rendered".into(),
            metadata: ExtractedMetadata { canonical_url: Some(req.url.to_string()), ..Default::default() },
            screenshot_url: Some("https://storage.test/shot.png".into()),
            extraction_method: "obscura".into(),
            extraction_status: "success".into(),
        })
    }
}

async fn page(s: &MockServer, p: &str, status: u16, body: &str) {
    Mock::given(method("GET")).and(path(p)).respond_with(ResponseTemplate::new(status).set_body_raw(body.to_owned(), "text/html")).mount(s).await;
}

#[tokio::test]
async fn static_pages_stay_on_http_and_js_pages_escalate_to_obscura() {
    let site = MockServer::start().await;
    page(&site, "/blog/static-article-about-rust", 200, ARTICLE_HTML).await;
    page(&site, "/app", 200, SPA).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let h = harness(HarnessOptions { extra_extractors: vec![Arc::new(FakeObscura(calls.clone()))], ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");

    let a = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/static-article-about-rust", site.uri())] })).await;
    let b = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/app", site.uri())], "content_type": "landing_page" })).await;
    assert_eq!(b["sources"][0]["source_type"], "web_page");
    drain(&h.state).await;
    let ea = api.ok("extract_content", &ctx, json!({ "source_id": a["sources"][0]["source_id"] })).await;
    let eb = api.ok("extract_content", &ctx, json!({ "source_id": b["sources"][0]["source_id"] })).await;
    assert_eq!(ea["extraction_method"], "http");
    assert_eq!(eb["extraction_method"], "obscura");
    assert_eq!(eb["screenshot_url"], "https://storage.test/shot.png");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "browser used only when static extraction was insufficient");
}

#[tokio::test]
async fn firecrawl_is_the_fallback_when_static_fetch_is_denied() {
    let site = MockServer::start().await;
    page(&site, "/blog/paywalled-article-title-here", 403, "denied").await;
    let fc = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/scrape")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "success": true, "data": { "markdown": "# Managed\n\nContent fetched by the managed provider.", "metadata": { "title": "Managed", "language": "en", "sourceURL": "https://x/y" } } }))).expect(1).mount(&fc).await;
    let fire = FirecrawlExtractor::new(&fc.uri(), "fc-key", Duration::from_secs(5)).unwrap();
    let h = harness(HarnessOptions { extra_extractors: vec![Arc::new(fire)], ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/paywalled-article-title-here", site.uri())] })).await;
    drain(&h.state).await;
    let e = api.ok("extract_content", &ctx, json!({ "source_id": ing["sources"][0]["source_id"] })).await;
    assert_eq!(e["extraction_method"], "firecrawl");
    assert!(e["text"].as_str().unwrap().contains("managed provider"));
}

#[tokio::test]
async fn github_x_and_youtube_use_their_dedicated_integrations() {
    let gh = MockServer::start().await;
    Mock::given(method("GET")).and(path("/repos/rust-lang/rust")).respond_with(ResponseTemplate::new(200).set_body_json(json!({ "full_name": "rust-lang/rust", "description": "Empowering everyone", "language": "Rust", "stargazers_count": 99, "topics": ["compiler"], "owner": { "login": "rust-lang" }, "pushed_at": "2026-11-01T00:00:00Z" }))).mount(&gh).await;
    Mock::given(method("GET"))
        .and(path("/repos/rust-lang/rust/readme"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# Rust\nA language empowering everyone to build reliable software."))
        .mount(&gh)
        .await;
    let x = MockServer::start().await;
    Mock::given(method("GET")).and(path("/2/tweets/12345")).respond_with(ResponseTemplate::new(200).set_body_json(json!({ "data": { "id": "12345", "text": "Shipping the new release today, thank you all!", "created_at": "2026-11-20T10:00:00Z", "lang": "en" }, "includes": { "users": [{ "name": "Jane", "username": "jane" }] } }))).mount(&x).await;
    let yt = MockServer::start().await;
    Mock::given(method("GET")).and(path("/youtube/v3/videos")).and(query_param("id", "abc123")).respond_with(ResponseTemplate::new(200).set_body_json(json!({ "items": [{ "snippet": { "title": "Launch day", "description": "We launched a thing and talked about it for a while.", "channelTitle": "Acme", "publishedAt": "2026-11-01T00:00:00Z", "tags": ["launch"], "thumbnails": { "high": { "url": "https://i.ytimg.com/x.jpg" } } }, "contentDetails": { "duration": "PT5M" }, "statistics": { "viewCount": "10" } }] }))).mount(&yt).await;

    let h = harness(HarnessOptions { github_url: Some(gh.uri()), x_url: Some(x.uri()), youtube_url: Some(yt.uri()), ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api
        .ok(
            "ingest_url",
            &ctx,
            json!({ "urls": ["https://github.com/rust-lang/rust", "https://x.com/jane/status/12345", "https://www.youtube.com/watch?v=abc123"] }),
        )
        .await;
    let types: Vec<&str> = ing["sources"].as_array().unwrap().iter().map(|s| s["source_type"].as_str().unwrap()).collect();
    assert_eq!(types, ["github_repository", "x_post", "youtube_video"]);
    assert_eq!(ing["sources"][0]["extraction_strategy"], "github_api");
    drain(&h.state).await;
    let methods: Vec<String> = {
        let mut v = vec![];
        for s in ing["sources"].as_array().unwrap() {
            v.push(api.ok("extract_content", &ctx, json!({ "source_id": s["source_id"] })).await["extraction_method"].as_str().unwrap().to_string());
        }
        v
    };
    assert_eq!(methods, ["github_api", "x_api", "youtube_api"]);
    // read_x_post / read_youtube_video work standalone and persist nothing
    let r = api.ok("read_x_post", &ctx, json!({ "url": "https://x.com/jane/status/12345" })).await;
    assert_eq!(r["author"], "Jane");
    let e = api.err("read_x_post", &ctx, json!({ "url": "https://www.instagram.com/p/abc123/" }), "VALIDATION_ERROR").await;
    assert!(e["error"]["message"].as_str().unwrap().contains("instagram.com"), "names the wrong host and the expected form: {e}");
    assert_eq!(api.ok("read_youtube_video", &ctx, json!({ "url": "https://youtu.be/abc123" })).await["title"], "Launch day");
}

#[tokio::test]
async fn duplicate_sources_and_idempotent_job_creation() {
    let site = MockServer::start().await;
    page(&site, "/blog/why-rust-wins-for-teams", 200, ARTICLE_HTML).await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let mut ctx = full_ctx("t1", "b1");
    let url = format!("{}/blog/why-rust-wins-for-teams", site.uri());

    ctx.idempotency_key = Some("idem_1".into());
    let first = api.ok("ingest_url", &ctx, json!({ "urls": [url] })).await;
    let replay = api.ok("ingest_url", &ctx, json!({ "urls": [url] })).await;
    assert_eq!(first["job_id"], replay["job_id"]);
    assert_eq!(first["sources"][0]["source_id"], replay["sources"][0]["source_id"]);
    assert_eq!(h.state.queue.stats().await.unwrap().pending, 1, "replay must not enqueue more work");
    drain(&h.state).await;

    // Same URL (with tracking params) again -> DUPLICATE_SOURCE; job is failed, not left dangling.
    ctx.idempotency_key = Some("idem_2".into());
    let e = api.err("ingest_url", &ctx, json!({ "urls": [format!("{url}?utm_campaign=x")] }), "DUPLICATE_SOURCE").await;
    assert_eq!(e["error"]["details"]["existing_source_id"], first["sources"][0]["source_id"]);
    // ...unless explicitly allowed
    ctx.idempotency_key = Some("idem_3".into());
    api.ok("ingest_url", &ctx, json!({ "urls": [url], "allow_duplicates": true })).await;
    // same URL twice in one request is collapsed
    ctx.idempotency_key = None;
    let c = api.ok("ingest_url", &ctx, json!({ "urls": [url, url.clone() + "#frag"], "allow_duplicates": true })).await;
    assert_eq!(c["sources"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn transient_failures_retry_with_backoff_then_dead_letter_and_fail_the_job() {
    let site = MockServer::start().await;
    page(&site, "/blog/flaky-upstream-article-title", 503, "boom").await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/flaky-upstream-article-title", site.uri())] })).await;

    for attempt in 1..=5 {
        h.queue.make_all_ready();
        assert_eq!(drain(&h.state).await, 1, "attempt {attempt}");
    }
    assert_eq!(h.queue.dead_letters().len(), 1, "dead-lettered after max attempts");
    h.queue.make_all_ready();
    assert_eq!(drain(&h.state).await, 0, "nothing left to run");
    let j = api.ok("get_job", &ctx, json!({ "job_id": ing["job_id"] })).await;
    assert_eq!(j["status"], "failed");
    assert_eq!(j["sources"][0]["extraction_status"], "failed");
    assert_eq!(h.state.queue.stats().await.unwrap().dead, 1);
}

#[tokio::test]
async fn permanent_failures_skip_retries() {
    let site = MockServer::start().await;
    page(&site, "/blog/gone-article-title-here-now", 404, "nope").await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/gone-article-title-here-now", site.uri())] })).await;
    assert_eq!(drain(&h.state).await, 1);
    assert_eq!(h.queue.dead_letters().len(), 1);
    // The only source failed permanently, so the job is failed and further extraction is refused.
    assert_eq!(api.ok("get_job", &ctx, json!({ "job_id": ing["job_id"] })).await["status"], "failed");
    let e = api.err("extract_content", &ctx, json!({ "source_id": ing["sources"][0]["source_id"], "force": true }), "INVALID_STATE").await;
    assert!(e["error"]["message"].as_str().unwrap().contains("404"), "the error explains why the job failed: {e}");
}

#[tokio::test]
async fn cancelling_a_job_stops_queued_work() {
    let site = MockServer::start().await;
    page(&site, "/blog/why-rust-wins-for-teams", 200, ARTICLE_HTML).await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/why-rust-wins-for-teams", site.uri())] })).await;
    let c = api.ok("cancel_job", &ctx, json!({ "job_id": ing["job_id"], "reason": "changed my mind" })).await;
    assert_eq!((c["status"].as_str(), c["cancelled_tasks"].as_u64()), (Some("cancelled"), Some(1)));
    assert_eq!(drain(&h.state).await, 0);
    assert_eq!(api.ok("cancel_job", &ctx, json!({ "job_id": ing["job_id"] })).await["cancelled_tasks"], 0, "idempotent");
    // extraction of a cancelled job is refused
    api.err("extract_content", &ctx, json!({ "source_id": ing["sources"][0]["source_id"] }), "INVALID_STATE").await;
    // audit trail records who/why
    let j = api.ok("get_job", &ctx, json!({ "job_id": ing["job_id"] })).await;
    let last = j["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((last["to"].as_str(), last["reason"].as_str(), last["actor_id"].as_str()), (Some("cancelled"), Some("changed my mind"), Some("user_1")));
}

#[tokio::test]
async fn zernio_failures_are_mapped_retried_and_traceable() {
    let site = MockServer::start().await;
    page(&site, "/blog/why-rust-wins-for-teams", 200, ARTICLE_HTML).await;
    let z = MockServer::start().await;
    // 1st attempt: rate limited. 2nd: success. Both must carry the same Idempotency-Key.
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({ "message": "slow down" })))
        .up_to_n_times(1)
        .mount(&z)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "post": { "_id": "zp", "platforms": [{ "platform": "linkedin", "status": "published", "platformPostUrl": "https://li/1" }] } }),
        ))
        .mount(&z)
        .await;

    let h = harness(HarnessOptions { zernio_url: Some(z.uri()), ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let (_, id) = approved_draft(&api, &h, &ctx, &format!("{}/blog/why-rust-wins-for-teams", site.uri()), &["linkedin"]).await;
    let prep = api
        .ok("prepare_publish_request", &ctx, json!({ "draft_id": id, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "publish" }))
        .await;
    let pid = prep["publish_request_id"].clone();

    let e = api.err("publish_draft", &ctx, json!({ "publish_request_id": pid }), "ZERNIO_RATE_LIMITED").await;
    assert_eq!(e["error"]["retryable"], true);
    // draft reopened for retry, not stuck or failed
    assert_eq!(api.ok("get_draft", &ctx, json!({ "draft_id": id })).await["status"], "ready_for_zernio");
    let ok = api.ok("publish_draft", &ctx, json!({ "publish_request_id": pid })).await;
    assert_eq!(ok["status"], "scheduled_or_published");
    assert_eq!(ok["platforms"][0]["url"], "https://li/1");

    let reqs = z.received_requests().await.unwrap();
    assert_eq!(reqs.len(), 2);
    let keys: Vec<_> = reqs.iter().map(|r| r.headers.get("idempotency-key").unwrap().to_str().unwrap().to_string()).collect();
    assert_eq!(keys[0], keys[1], "retry reuses the idempotency key so Zernio cannot double-post");
    let body = String::from_utf8_lossy(&reqs[1].body).to_lowercase();
    assert!(!body.contains("token") && !body.contains("oauth"), "no credentials ever leave this service");
    assert!(reqs[1].headers.get("authorization").unwrap().to_str().unwrap().starts_with("Bearer "));
}

#[tokio::test]
async fn zernio_partial_success_permanent_failure_and_async_submission() {
    let site = MockServer::start().await;
    page(&site, "/blog/why-rust-wins-for-teams", 200, ARTICLE_HTML).await;

    // Partial success across channels keeps the draft open and reports per-channel results.
    let z = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "post": { "_id": "zp", "platforms": [
        { "platform": "linkedin", "status": "published" }, { "platform": "twitter", "status": "failed", "errorMessage": "duplicate" } ] } })))
        .mount(&z)
        .await;
    let h = harness(HarnessOptions { zernio_url: Some(z.uri()), ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let (_, id) = approved_draft(&api, &h, &ctx, &format!("{}/blog/why-rust-wins-for-teams", site.uri()), &["linkedin", "x"]).await;
    let prep = api
        .ok("prepare_publish_request", &ctx, json!({ "draft_id": id, "zernio_profile_id": "profile_1", "channels": ["linkedin", "x"], "action": "publish" }))
        .await;
    let r = api.ok("publish_draft", &ctx, json!({ "publish_request_id": prep["publish_request_id"] })).await;
    assert_eq!(r["status"], "partial_success");
    assert_eq!(r["draft_status"], "submitted_to_zernio");
    assert_eq!(r["platforms"][1]["error"], "duplicate");

    // Permanent rejection fails the draft (media/validation error, not retryable).
    let z2 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "message": "bad media" })))
        .mount(&z2)
        .await;
    let h2 = harness(HarnessOptions { zernio_url: Some(z2.uri()), ..Default::default() });
    let api2 = Api::new(&h2);
    let (_, id2) = approved_draft(&api2, &h2, &ctx, &format!("{}/blog/why-rust-wins-for-teams", site.uri()), &["linkedin"]).await;
    let prep2 = api2
        .ok("prepare_publish_request", &ctx, json!({ "draft_id": id2, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "publish" }))
        .await;
    api2.err("publish_draft", &ctx, json!({ "publish_request_id": prep2["publish_request_id"] }), "ZERNIO_PUBLISH_FAILED").await;
    assert_eq!(api2.ok("get_draft", &ctx, json!({ "draft_id": id2 })).await["status"], "failed");

    // Async path: queued, then submitted by the worker.
    let z3 = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "post": { "_id": "zp", "platforms": [{ "platform": "linkedin", "status": "scheduled" }] } })),
        )
        .expect(1)
        .mount(&z3)
        .await;
    let h3 = harness(HarnessOptions { zernio_url: Some(z3.uri()), ..Default::default() });
    let api3 = Api::new(&h3);
    let (_, id3) = approved_draft(&api3, &h3, &ctx, &format!("{}/blog/why-rust-wins-for-teams", site.uri()), &["linkedin"]).await;
    let prep3 = api3
        .ok(
            "prepare_publish_request",
            &ctx,
            json!({ "draft_id": id3, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "schedule", "scheduled_at": future() }),
        )
        .await;
    let q = api3.ok("schedule_draft", &ctx, json!({ "publish_request_id": prep3["publish_request_id"], "async": true })).await;
    assert_eq!(q["status"], "queued");
    assert_eq!(drain(&h3.state).await, 1);
    assert_eq!(api3.ok("get_draft", &ctx, json!({ "draft_id": id3 })).await["status"], "scheduled_or_published");
}

#[tokio::test]
async fn zernio_read_tools_use_read_access_and_never_write() {
    let z = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/inbox/comments"))
        .and(query_param("profileId", "profile_1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "comments": [{ "id": "c1", "text": "love it" }] })))
        .mount(&z)
        .await;
    let h = harness(HarnessOptions { zernio_url: Some(z.uri()), ..Default::default() });
    let api = Api::new(&h);
    let mut ctx = full_ctx("t1", "b1");
    for a in &mut ctx.zernio.as_mut().unwrap().connected_accounts {
        a.access = content_agent::context::Access::Read;
    }
    let r = api.ok("read_comments", &ctx, json!({ "zernio_profile_id": "profile_1", "platform": "instagram" })).await;
    assert_eq!(r["data"]["comments"][0]["text"], "love it");
    assert!(z.received_requests().await.unwrap().iter().all(|r| r.method.as_str() == "GET"));
    api.err("read_comments", &ctx, json!({ "zernio_profile_id": "other" }), "ZERNIO_PERMISSION_DENIED").await;
    let mut no_conn = ctx.clone();
    no_conn.zernio = None;
    api.err("read_inbox", &no_conn, json!({ "zernio_profile_id": "profile_1" }), "ZERNIO_CONNECTION_MISSING").await;
}

#[tokio::test]
async fn concurrent_multi_tenant_ingestion_stays_isolated() {
    let site = MockServer::start().await;
    page(&site, "/blog/why-rust-wins-for-teams", 200, ARTICLE_HTML).await;
    let h = harness(HarnessOptions::default());
    let api = Arc::new(Api::new(&h));
    let url = format!("{}/blog/why-rust-wins-for-teams", site.uri());

    let mut tasks = vec![];
    for t in 0..5 {
        for n in 0..10 {
            let (api, url) = (api.clone(), url.clone());
            tasks.push(tokio::spawn(async move {
                let ctx = full_ctx(&format!("tenant_{t}"), "brand_1");
                let r = api.ok("ingest_url", &ctx, json!({ "urls": [url], "allow_duplicates": true })).await;
                (t, n, r["job_id"].as_str().unwrap().to_string())
            }));
        }
    }
    let mut jobs = vec![];
    for t in tasks {
        jobs.push(t.await.unwrap());
    }
    assert_eq!(drain(&h.state).await, 50);
    for (t, _, job) in &jobs {
        let mine = api.ok("get_job", &full_ctx(&format!("tenant_{t}"), "brand_1"), json!({ "job_id": job })).await;
        assert_eq!(mine["sources"][0]["extraction_status"], "success");
        let other = full_ctx(&format!("tenant_{}", (t + 1) % 5), "brand_1");
        api.err("get_job", &other, json!({ "job_id": job }), "NOT_FOUND").await;
    }
}

#[tokio::test]
async fn retention_purges_extracted_content_but_keeps_the_audit_trail() {
    let site = MockServer::start().await;
    page(&site, "/blog/why-rust-wins-for-teams", 200, ARTICLE_HTML).await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/why-rust-wins-for-teams", site.uri())] })).await;
    drain(&h.state).await;
    let sid = ing["sources"][0]["source_id"].clone();
    assert_eq!(h.state.store.purge_source_content(chrono::Utc::now() - chrono::Duration::days(30)).await.unwrap(), 0, "recent content is kept");
    assert_eq!(h.state.store.purge_source_content(chrono::Utc::now() + chrono::Duration::days(1)).await.unwrap(), 1);
    let ex = api.ok("get_job", &ctx, json!({ "job_id": ing["job_id"] })).await;
    assert_eq!(ex["sources"][0]["source_id"], sid, "the source row and job history survive");
    // purged content can no longer be reported on
    api.err("generate_content_report", &ctx, json!({ "source_ids": [sid] }), "INVALID_STATE").await;
}

struct MissingObscura;
#[async_trait]
impl ContentExtractor for MissingObscura {
    fn name(&self) -> &'static str {
        "obscura"
    }
    async fn extract(&self, _: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        Err(ExtractionError::NotConfigured("obscura binary not found (set OBSCURA_BIN)"))
    }
}

async fn extract_error(h: &content_agent::testing::Harness, url: &str) -> serde_json::Value {
    let api = Api::new(h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [url] })).await;
    drain(&h.state).await;
    api.ok("get_job", &ctx, json!({ "job_id": ing["job_id"] })).await["sources"][0]["error"].clone()
}

#[tokio::test]
async fn meta_refresh_stub_pages_are_followed_to_the_real_article() {
    let site = MockServer::start().await;
    let stub = format!(
        r#"<html><head><title>Redirect</title><meta http-equiv="refresh" content="0; url={}/blog/the-real-article-page"></head><body><p>Redirecting…</p></body></html>"#,
        site.uri()
    );
    page(&site, "/2024/old-post.html", 200, &stub).await;
    page(&site, "/blog/the-real-article-page", 200, ARTICLE_HTML).await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/2024/old-post.html", site.uri())] })).await;
    drain(&h.state).await;
    let ex = api.ok("extract_content", &ctx, json!({ "source_id": ing["sources"][0]["source_id"] })).await;
    assert_eq!(ex["title"], "Why Rust Wins", "followed the stub to the real page, not 'Redirect'");
    assert!(ex["text"].as_str().unwrap().contains("data races"));
}

#[tokio::test]
async fn js_redirect_stubs_with_noscript_fallback_are_followed() {
    let site = MockServer::start().await;
    let stub = format!(
        r#"<!doctype html><title>Redirect</title><script>const target = "{0}/blog/the-real-article-page"; window.location.replace(target);</script><noscript><meta http-equiv="refresh" content="0; url={0}/blog/the-real-article-page"></noscript><p>Click here to be redirected.</p>"#,
        site.uri()
    );
    page(&site, "/2024/js-moved.html", 200, &stub).await;
    page(&site, "/blog/the-real-article-page", 200, ARTICLE_HTML).await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/2024/js-moved.html", site.uri())] })).await;
    drain(&h.state).await;
    let ex = api.ok("extract_content", &ctx, json!({ "source_id": ing["sources"][0]["source_id"] })).await;
    assert_eq!(ex["title"], "Why Rust Wins");
}

#[tokio::test]
async fn redirect_stubs_cannot_be_used_to_reach_forbidden_targets() {
    // The stub lives on a reachable test host but forwards to a denied host: the follower must re-apply the URL
    // policy to that hop and refuse it instead of fetching.
    use content_agent::{
        extract::http::HttpExtractor,
        ssrf::{FetchLimits, SafeFetcher},
        urls::{SourceType, UrlPolicy},
    };
    let site = MockServer::start().await;
    let evil = MockServer::start().await; // would record any request that wrongly gets through
    page(
        &site,
        "/s",
        200,
        &format!(r#"<title>Redirect</title><meta http-equiv="refresh" content="0; url=http://localhost:{}/secret">"#, evil.address().port()),
    )
    .await;
    let mut policy = UrlPolicy::default().dangerously_allow_private_networks();
    policy.denylist_hosts = vec!["localhost".into()]; // the test host is 127.0.0.1; the stub names "localhost"
    let ex = HttpExtractor::new(SafeFetcher::new(policy, FetchLimits::default()));
    let req = ExtractionRequest {
        source_id: "s".into(),
        tenant_id: "t".into(),
        brand_id: "b".into(),
        url: url::Url::parse(&format!("{}/s", site.uri())).unwrap(),
        source_type: SourceType::WebArticle,
        want_screenshot: false,
    };
    let err = ex.extract(&req).await.unwrap_err().into_api();
    assert_eq!(err.code, content_agent::error::ErrorCode::UrlNotAllowed, "{err:?}");
    assert!(evil.received_requests().await.unwrap().is_empty(), "the forbidden target must never be contacted");
}

#[tokio::test]
async fn unsupported_files_report_the_real_reason_not_a_missing_browser() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/files/report-final.pdf"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"%PDF-1.4 ...".to_vec(), "application/pdf"))
        .mount(&site)
        .await;
    let h = harness(HarnessOptions { extra_extractors: vec![Arc::new(MissingObscura)], ..Default::default() });
    let e = extract_error(&h, &format!("{}/files/report-final.pdf", site.uri())).await;
    let msg = e["message"].as_str().unwrap();
    assert!(msg.contains("application/pdf") && !msg.contains("obscura"), "{msg}");
}

#[tokio::test]
async fn js_only_pages_explain_that_a_renderer_is_needed() {
    let site = MockServer::start().await;
    page(&site, "/app", 200, SPA).await;
    let h = harness(HarnessOptions { extra_extractors: vec![Arc::new(MissingObscura)], ..Default::default() });
    let e = extract_error(&h, &format!("{}/app", site.uri())).await;
    let msg = e["message"].as_str().unwrap();
    assert!(msg.contains("requires JavaScript rendering") && msg.contains("no renderer is available") && msg.contains("OBSCURA_BIN"), "{msg}");
}

#[tokio::test]
async fn an_unconfigured_extractor_explains_the_setup_needed() {
    // Harness has no X token configured (no x_url), so the "x" extractor is not registered at all and the
    // chain is empty; register one that reports NotConfigured exactly like the real extractor does without a token.
    struct NoXToken;
    #[async_trait]
    impl ContentExtractor for NoXToken {
        fn name(&self) -> &'static str {
            "x"
        }
        async fn extract(&self, _: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
            Err(ExtractionError::NotConfigured("X_BEARER_TOKEN is not set"))
        }
    }
    let h = harness(HarnessOptions { extra_extractors: vec![Arc::new(NoXToken)], ..Default::default() });
    let api = Api::new(&h);
    let e = api.err("read_x_post", &full_ctx("t1", "b1"), json!({ "url": "https://x.com/jack/status/20" }), "EXTRACTION_FAILED").await;
    let msg = e["error"]["message"].as_str().unwrap();
    assert!(msg.contains("X_BEARER_TOKEN") && msg.contains("restart"), "{msg}");
    assert_eq!(e["error"]["details"]["reason"], "extractor_not_configured");
    assert_eq!(e["error"]["retryable"], false);
}
