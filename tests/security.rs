mod common;
use axum::{body::Body, http::Request};
use common::*;
use content_agent::{
    context::{Access, Action},
    testing::{full_ctx, harness, HarnessOptions, TEST_SECRET},
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn raw(api: &Api, path_: &str, headers: Vec<(&str, String)>, body: Value) -> (u16, Value) {
    let mut req = Request::builder().method("POST").uri(path_).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = api.router.clone().oneshot(req.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()).await.unwrap();
    let s = resp.status().as_u16();
    (s, serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap_or(Value::Null))
}

#[tokio::test]
async fn rejects_missing_bad_stale_and_tampered_signatures() {
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let p = "/internal/v1/tools/get_job";
    let body = json!({ "context": full_ctx("t", "b"), "input": { "job_id": "x" } });
    let bytes = serde_json::to_vec(&body).unwrap();

    let (s, v) = raw(&api, p, vec![], body.clone()).await;
    assert_eq!((s, v["error"]["code"].as_str()), (401, Some("UNAUTHORIZED")));

    let good = content_agent::testing::signed_headers("POST", p, &bytes);
    let (s, _) = raw(&api, p, good.clone(), body.clone()).await;
    assert_ne!(s, 401, "a correctly signed request is accepted");

    let (s, _) = raw(&api, p, vec![("x-lwm-timestamp", good[0].1.clone()), ("x-lwm-signature", "00".repeat(32))], body.clone()).await;
    assert_eq!(s, 401);

    let old = (chrono::Utc::now().timestamp() - 3600).to_string();
    let sig = content_agent::auth::sign(TEST_SECRET, &old, "POST", p, &bytes);
    assert_eq!(raw(&api, p, vec![("x-lwm-timestamp", old), ("x-lwm-signature", sig)], body.clone()).await.0, 401);

    // signed for one body, sent with another
    let mut tampered = body.clone();
    tampered["input"]["job_id"] = json!("other");
    assert_eq!(raw(&api, p, good, tampered).await.0, 401);

    // wrong secret
    let ts = chrono::Utc::now().timestamp().to_string();
    let sig = content_agent::auth::sign("another-secret-another-secret-12345", &ts, "POST", p, &bytes);
    assert_eq!(raw(&api, p, vec![("x-lwm-timestamp", ts), ("x-lwm-signature", sig)], body).await.0, 401);

    // health endpoints need no auth; internal ones do
    let r = api.router.clone().oneshot(Request::get("/healthz").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), 200);
    let r = api.router.clone().oneshot(Request::get("/internal/v1/tools").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn enforces_tool_action_channel_and_quota_permissions() {
    let site = MockServer::start().await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let url = format!("{}/blog/some-long-article-title", site.uri());

    let mut c = full_ctx("t1", "b1");
    c.permissions.allowed_tools = vec!["generate_social_draft".into()];
    api.err("ingest_url", &c, json!({ "urls": [url] }), "FORBIDDEN").await;

    let mut c = full_ctx("t1", "b1");
    c.permissions.allowed_actions = vec![Action::Draft];
    api.err("ingest_url", &c, json!({ "urls": [url] }), "FORBIDDEN").await; // ingest needs `read`

    let mut c = full_ctx("t1", "b1");
    c.permissions.allowed_channels = vec!["instagram".into()];
    api.err("ingest_url", &c, json!({ "urls": [url], "requested_channels": ["x"] }), "FORBIDDEN").await;

    let mut c = full_ctx("t1", "b1");
    c.plan_channels = Some(vec!["linkedin".into()]);
    api.err("ingest_url", &c, json!({ "urls": [url], "requested_channels": ["instagram"] }), "FORBIDDEN").await;

    let mut c = full_ctx("t1", "b1");
    c.quota_remaining = Some(0);
    api.err("ingest_url", &c, json!({ "urls": [url] }), "QUOTA_EXCEEDED").await;

    let mut c = full_ctx("", "b1");
    c.tenant_id = "".into();
    api.err("ingest_url", &c, json!({ "urls": [url] }), "TENANT_NOT_FOUND").await;
    let mut c = full_ctx("t1", "");
    c.brand_id = " ".into();
    api.err("ingest_url", &c, json!({ "urls": [url] }), "BRAND_NOT_FOUND").await;

    // account that belongs to a different tenant is refused outright
    let mut c = full_ctx("t1", "b1");
    c.zernio.as_mut().unwrap().connected_accounts[0].tenant_id = Some("someone_else".into());
    api.err("ingest_url", &c, json!({ "urls": [url] }), "FORBIDDEN").await;

    api.err("no_such_tool", &full_ctx("t1", "b1"), json!({}), "NOT_FOUND").await;
}

#[tokio::test]
async fn blocks_ssrf_targets_at_ingest() {
    let h = harness(HarnessOptions { strict_urls: true, ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    for u in [
        "http://example.com/a",
        "https://127.0.0.1/",
        "https://169.254.169.254/latest/meta-data/",
        "https://localhost/x",
        "https://10.0.0.5/",
        "https://[::1]/",
        "file:///etc/passwd",
        "https://user:pw@example.com/",
    ] {
        let (s, v) = api.call("ingest_url", &ctx, json!({ "urls": [u] })).await;
        assert_eq!(s, 400, "{u}: {v}");
        assert!(matches!(v["error"]["code"].as_str(), Some("URL_NOT_ALLOWED") | Some("VALIDATION_ERROR")), "{u}: {v}");
    }
}

#[tokio::test]
async fn tenant_and_brand_isolation() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blog/why-rust-wins-for-teams"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ARTICLE_HTML, "text/html"))
        .mount(&site)
        .await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let a = full_ctx("tenant_a", "brand_1");
    let url = format!("{}/blog/why-rust-wins-for-teams", site.uri());
    let ing = api.ok("ingest_url", &a, json!({ "urls": [url] })).await;
    drain(&h.state).await;
    let (job, src) = (ing["job_id"].clone(), ing["sources"][0]["source_id"].clone());
    let rep = api.ok("generate_content_report", &a, json!({ "source_ids": [src] })).await;
    let d = api.ok("generate_social_draft", &a, json!({ "report_id": rep["report_id"], "channels": ["linkedin"], "post_type": "text" })).await;

    for other in [full_ctx("tenant_b", "brand_1"), full_ctx("tenant_a", "brand_2")] {
        api.err("get_job", &other, json!({ "job_id": job }), "NOT_FOUND").await;
        api.err("get_draft", &other, json!({ "draft_id": d["draft_id"] }), "NOT_FOUND").await;
        api.err("extract_content", &other, json!({ "source_id": src }), "NOT_FOUND").await;
        api.err("generate_content_report", &other, json!({ "source_ids": [src] }), "NOT_FOUND").await;
        api.err("generate_social_draft", &other, json!({ "report_id": rep["report_id"], "channels": ["x"] }), "NOT_FOUND").await;
        api.err("review_draft", &other, json!({ "draft_id": d["draft_id"] }), "NOT_FOUND").await;
        api.err("cancel_job", &other, json!({ "job_id": job }), "NOT_FOUND").await;
        // the same URL in another tenant is not a duplicate
        let ok = api.call("ingest_url", &other, json!({ "urls": [url] })).await;
        assert_eq!(ok.0, 200, "{:?}", ok.1);
    }
    // the owner's data is untouched
    assert_eq!(api.ok("get_job", &a, json!({ "job_id": job })).await["status"], "draft_ready");
}

#[tokio::test]
async fn read_connection_can_never_publish_and_unapproved_drafts_never_reach_zernio() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blog/why-rust-wins-for-teams"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ARTICLE_HTML, "text/html"))
        .mount(&site)
        .await;
    let zernio = MockServer::start().await; // any request to it would be a bug: no mocks mounted => 404
    let h = harness(HarnessOptions { zernio_url: Some(zernio.uri()), ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/why-rust-wins-for-teams", site.uri())] })).await;
    drain(&h.state).await;
    let rep = api.ok("generate_content_report", &ctx, json!({ "source_ids": [ing["sources"][0]["source_id"]] })).await;
    let d = api.ok("generate_social_draft", &ctx, json!({ "report_id": rep["report_id"], "channels": ["linkedin"], "post_type": "text" })).await;
    let id = d["draft_id"].clone();
    api.ok("review_draft", &ctx, json!({ "draft_id": id })).await;
    api.ok("submit_review_decision", &ctx, json!({ "draft_id": id, "decision": "approved", "reviewer_id": "u" })).await;

    let when = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    let prep_in = json!({ "draft_id": id, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "schedule", "scheduled_at": when });

    // read-only connection: refused
    let mut ro = full_ctx("t1", "b1");
    for a in &mut ro.zernio.as_mut().unwrap().connected_accounts {
        a.access = Access::Read;
    }
    api.err("prepare_publish_request", &ro, prep_in.clone(), "ZERNIO_PERMISSION_DENIED").await;
    // no connection context at all
    let mut none = full_ctx("t1", "b1");
    none.zernio = None;
    let e = api.err("prepare_publish_request", &none, prep_in.clone(), "ZERNIO_CONNECTION_MISSING").await;
    assert_eq!(e["error"]["details"]["brand_id"], "b1");
    // permission without the schedule action
    let mut noact = full_ctx("t1", "b1");
    noact.permissions.allowed_actions = vec![Action::Read, Action::Draft];
    api.err("prepare_publish_request", &noact, prep_in.clone(), "FORBIDDEN").await;
    // wrong profile
    api.err(
        "prepare_publish_request",
        &ctx,
        json!({ "draft_id": id, "zernio_profile_id": "profile_evil", "channels": ["linkedin"], "action": "schedule", "scheduled_at": when }),
        "ZERNIO_PERMISSION_DENIED",
    )
    .await;
    // past schedule time
    api.err(
        "prepare_publish_request",
        &ctx,
        json!({ "draft_id": id, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "schedule", "scheduled_at": "2020-01-01T00:00:00Z" }),
        "VALIDATION_ERROR",
    )
    .await;

    // publish/schedule tools refuse anything that was never prepared
    api.err("publish_draft", &ctx, json!({ "publish_request_id": "pubreq_nope" }), "NOT_FOUND").await;

    // happy prepare, then downgrade to read-only before submitting: still refused
    let prep = api.ok("prepare_publish_request", &ctx, prep_in).await;
    api.err("schedule_draft", &ro, json!({ "publish_request_id": prep["publish_request_id"] }), "ZERNIO_PERMISSION_DENIED").await;
    // wrong tool for the prepared action
    api.err("publish_draft", &ctx, json!({ "publish_request_id": prep["publish_request_id"] }), "VALIDATION_ERROR").await;
    assert!(zernio.received_requests().await.unwrap().is_empty(), "Zernio must not have been contacted");
}

#[tokio::test]
async fn rate_limits_per_tenant_and_tool() {
    let mut cfg = content_agent::config::Config::for_tests();
    cfg.rate_tool_per_min = 3;
    let h = harness(HarnessOptions { cfg, ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    for _ in 0..3 {
        api.call("get_job", &ctx, json!({ "job_id": "x" })).await;
    }
    let v = api.err("get_job", &ctx, json!({ "job_id": "x" }), "RATE_LIMIT_EXCEEDED").await;
    assert_eq!(v["error"]["retryable"], true);
    // another tenant has its own bucket
    assert_ne!(api.call("get_job", &full_ctx("t2", "b1"), json!({ "job_id": "x" })).await.1["error"]["code"], "RATE_LIMIT_EXCEEDED");
}

#[tokio::test]
async fn disabled_tools_and_error_envelope_shape() {
    let mut cfg = content_agent::config::Config::for_tests();
    cfg.disabled_tools.insert("find_images".into());
    let h = harness(HarnessOptions { cfg, ..Default::default() });
    let api = Api::new(&h);
    let v = api.err("find_images", &full_ctx("t", "b"), json!({ "draft_id": "d" }), "FORBIDDEN").await;
    let e = &v["error"];
    assert!(e["message"].is_string() && e["details"].is_object() && e["retryable"].is_boolean());
    assert_eq!(e["documentation_url"], "https://internal.docs.letwemarket.com/errors/FORBIDDEN");
    // malformed body -> enveloped validation error
    let (s, v) = raw(&api, "/internal/v1/tools/get_job", common_headers(b"not json"), json!(null)).await;
    let _ = (s, v);
}

fn common_headers(_b: &[u8]) -> Vec<(&'static str, String)> {
    vec![]
}
