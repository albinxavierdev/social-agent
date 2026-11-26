mod common;
use common::*;
use content_agent::testing::{full_ctx, harness, HarnessOptions};
use serde_json::json;
use wiremock::{
    matchers::{header_exists, method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn article_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blog/why-rust-wins-for-teams"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ARTICLE_HTML, "text/html"))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn full_flow_ingest_to_scheduled_on_zernio() {
    let site = article_server().await;
    let zernio = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/posts"))
        .and(header_exists("Idempotency-Key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "post": { "_id": "zp_1", "platforms": [
            { "platform": "linkedin", "status": "scheduled" }, { "platform": "twitter", "status": "scheduled" } ] } })))
        .expect(1)
        .mount(&zernio)
        .await;

    let h = harness(HarnessOptions { zernio_url: Some(zernio.uri()), ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("tenant_1", "brand_1");

    // ingest -> queued job with typed sources
    let url = format!("{}/blog/why-rust-wins-for-teams?utm_source=x", site.uri());
    let ing = api
        .ok(
            "ingest_url",
            &ctx,
            json!({ "urls": [url], "requested_channels": ["linkedin", "x"],
        "brand_context": { "tone_of_voice": "friendly", "content_rules": ["no jargon"] } }),
        )
        .await;
    assert_eq!(ing["status"], "queued");
    assert_eq!(ing["sources"][0]["source_type"], "web_article");
    let job_id = ing["job_id"].as_str().unwrap().to_string();
    let source_id = ing["sources"][0]["source_id"].as_str().unwrap().to_string();

    // worker extracts
    assert_eq!(drain(&h.state).await, 1);
    let ex = api.ok("extract_content", &ctx, json!({ "source_id": source_id })).await;
    assert_eq!(ex["extraction_status"], "success");
    assert_eq!(ex["extraction_method"], "http");
    assert_eq!(ex["author"], "Jane Doe");
    assert!(ex["text"].as_str().unwrap().contains("data races"));
    assert!(!ex["text"].as_str().unwrap().contains("footer"));

    // report -> draft -> images -> validate
    let rep = api
        .ok("generate_content_report", &ctx, json!({ "source_ids": [source_id], "campaign_goal": "awareness", "requested_channels": ["linkedin", "x"] }))
        .await;
    let report_id = rep["report_id"].as_str().unwrap();
    assert!(!rep["key_points"].as_array().unwrap().is_empty());
    assert!(rep["source_links"][0].as_str().unwrap().contains("/blog/why-rust-wins-for-teams"));

    let d = api.ok("generate_social_draft", &ctx, json!({ "report_id": report_id, "channels": ["linkedin", "x"], "post_type": "text" })).await;
    assert_eq!(d["status"], "draft_ready");
    assert_eq!(d["variants"].as_array().unwrap().len(), 2);
    let draft_id = d["draft_id"].as_str().unwrap().to_string();

    let img = api.ok("find_images", &ctx, json!({ "draft_id": draft_id, "count": 1 })).await;
    assert_eq!(img["images"][0]["aspect_ratio"], "1:1");
    assert_eq!(img["images"][0]["validation_status"], "passed");

    let v = api.ok("validate_content", &ctx, json!({ "draft_id": draft_id })).await;
    assert_eq!(v["status"], "passed", "{v}");

    // cannot publish before approval
    let pr_in = json!({ "draft_id": draft_id, "zernio_profile_id": "profile_1", "channels": ["linkedin", "x"], "action": "schedule", "scheduled_at": (chrono::Utc::now() + chrono::Duration::days(2)).to_rfc3339() });
    api.err("prepare_publish_request", &ctx, pr_in.clone(), "INVALID_STATE").await;

    // review -> approve -> prepare
    let r = api.ok("review_draft", &ctx, json!({ "draft_id": draft_id })).await;
    assert_eq!(r["status"], "awaiting_customer_review");
    assert_eq!(r["next_action"], "letwemarket_review");
    assert!(r["review_url"].as_str().unwrap().contains(&draft_id));
    api.err("prepare_publish_request", &ctx, pr_in.clone(), "INVALID_STATE").await; // still not approved

    let dec = api
        .ok(
            "submit_review_decision",
            &ctx,
            json!({ "draft_id": draft_id, "decision": "approved", "edited_content": {}, "reviewer_id": "user_9", "notes": "lgtm" }),
        )
        .await;
    assert_eq!(dec["status"], "approved");
    // idempotent replay
    assert_eq!(
        api.ok("submit_review_decision", &ctx, json!({ "draft_id": draft_id, "decision": "approved", "reviewer_id": "user_9" })).await["replayed"],
        true
    );

    let prep = api.ok("prepare_publish_request", &ctx, pr_in.clone()).await;
    assert_eq!(prep["status"], "ready_for_zernio");
    let again = api.ok("prepare_publish_request", &ctx, pr_in).await;
    assert_eq!(again["publish_request_id"], prep["publish_request_id"], "identical request is idempotent");
    assert_eq!(prep["payload"]["zernio_request"]["platforms"][1]["platform"], "twitter");

    // submit to Zernio
    let sub = api.ok("schedule_draft", &ctx, json!({ "publish_request_id": prep["publish_request_id"] })).await;
    assert_eq!(sub["status"], "scheduled_or_published");
    assert_eq!(sub["draft_status"], "scheduled_or_published");
    // replay does not double-post (mock expects exactly one call)
    let replay = api.ok("schedule_draft", &ctx, json!({ "publish_request_id": prep["publish_request_id"] })).await;
    assert_eq!(replay["replayed"], true);

    // audit trail: every transition persisted with actor + trace
    let g = api.ok("get_draft", &ctx, json!({ "draft_id": draft_id })).await;
    let states: Vec<&str> = g["events"].as_array().unwrap().iter().map(|e| e["to"].as_str().unwrap()).collect();
    assert_eq!(states, ["draft_ready", "awaiting_customer_review", "approved", "ready_for_zernio", "submitted_to_zernio", "scheduled_or_published"]);
    assert!(g["events"][1]["trace_id"].as_str().unwrap().starts_with("trace_"));
    let j = api.ok("get_job", &ctx, json!({ "job_id": job_id })).await;
    assert_eq!(j["status"], "scheduled_or_published");

    // every invocation is recorded
    assert!(h.store.invocations().len() >= 15);
    assert!(h.store.invocations().iter().all(|i| !i.trace_id.is_empty() && !i.input_hash.is_empty()));
}

#[tokio::test]
async fn repurpose_creates_one_draft_per_target() {
    let site = article_server().await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/why-rust-wins-for-teams", site.uri())] })).await;
    drain(&h.state).await;
    let sid = ing["sources"][0]["source_id"].as_str().unwrap();
    let r = api
        .ok(
            "repurpose_content",
            &ctx,
            json!({ "source_id": sid, "targets": [{ "channel": "x", "format": "thread" }, { "channel": "instagram", "format": "carousel" }] }),
        )
        .await;
    let v = r["variants"].as_array().unwrap();
    assert_eq!(v.len(), 2);
    assert!(r["campaign_id"].as_str().unwrap().starts_with("camp_"));
    assert_ne!(v[0]["draft_id"], v[1]["draft_id"]);
    // exactly one input kind
    api.err("repurpose_content", &ctx, json!({ "source_id": sid, "report_id": "r", "targets": [{ "channel": "x", "format": "thread" }] }), "VALIDATION_ERROR")
        .await;
}

#[tokio::test]
async fn revision_loop_returns_to_review() {
    let site = article_server().await;
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/why-rust-wins-for-teams", site.uri())] })).await;
    drain(&h.state).await;
    let rep = api.ok("generate_content_report", &ctx, json!({ "source_ids": [ing["sources"][0]["source_id"]] })).await;
    let d = api.ok("generate_social_draft", &ctx, json!({ "report_id": rep["report_id"], "channels": ["linkedin"], "post_type": "text" })).await;
    let id = d["draft_id"].clone();
    api.ok("review_draft", &ctx, json!({ "draft_id": id })).await;
    let rv = api.ok("submit_review_decision", &ctx, json!({ "draft_id": id, "decision": "revision_requested", "reviewer_id": "u", "notes": "shorter" })).await;
    assert_eq!(rv["next_action"], "revise_draft");
    // approval is impossible from revision_requested; re-review is allowed
    api.err("submit_review_decision", &ctx, json!({ "draft_id": id, "decision": "approved", "reviewer_id": "u" }), "INVALID_STATE").await;
    assert_eq!(api.ok("review_draft", &ctx, json!({ "draft_id": id })).await["status"], "awaiting_customer_review");
    // rejection is terminal
    api.ok("submit_review_decision", &ctx, json!({ "draft_id": id, "decision": "rejected", "reviewer_id": "u" })).await;
    api.err("review_draft", &ctx, json!({ "draft_id": id }), "INVALID_STATE").await;
}
