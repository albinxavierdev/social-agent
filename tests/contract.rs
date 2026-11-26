//! Contract tests: live responses must validate against the published OpenAPI schemas.
mod common;
use common::*;
use content_agent::{
    openapi,
    testing::{full_ctx, harness, HarnessOptions},
};
use serde_json::{json, Value};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

fn check(tool: &str, instance: &Value) {
    let doc = openapi::spec();
    let schema = json!({ "components": doc["components"], "allOf": [openapi::output_schema(tool)] });
    let v = jsonschema::validator_for(&schema).expect("schema compiles");
    let errs: Vec<String> = v.iter_errors(instance).map(|e| format!("{e} at {}", e.instance_path)).collect();
    assert!(errs.is_empty(), "{tool} response violates its contract: {errs:?}\n{instance}");
}

#[tokio::test]
async fn tool_responses_match_the_openapi_contract() {
    let site = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blog/why-rust-wins-for-teams"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(ARTICLE_HTML, "text/html"))
        .mount(&site)
        .await;
    let z = MockServer::start().await;
    let h = harness(HarnessOptions { zernio_url: Some(z.uri()), ..Default::default() });
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");

    let ing = api.ok("ingest_url", &ctx, json!({ "urls": [format!("{}/blog/why-rust-wins-for-teams", site.uri())], "requested_channels": ["linkedin"] })).await;
    check("ingest_url", &ing);
    drain(&h.state).await;
    let ex = api.ok("extract_content", &ctx, json!({ "source_id": ing["sources"][0]["source_id"] })).await;
    check("extract_content", &ex);
    let rep = api.ok("generate_content_report", &ctx, json!({ "source_ids": [ing["sources"][0]["source_id"]] })).await;
    check("generate_content_report", &rep);
    let d = api.ok("generate_social_draft", &ctx, json!({ "report_id": rep["report_id"], "channels": ["linkedin", "x"], "post_type": "text" })).await;
    check("generate_social_draft", &d);
    let id = d["draft_id"].clone();
    check("find_images", &api.ok("find_images", &ctx, json!({ "draft_id": id })).await);
    check("validate_content", &api.ok("validate_content", &ctx, json!({ "draft_id": id })).await);
    check("review_draft", &api.ok("review_draft", &ctx, json!({ "draft_id": id })).await);
    check("submit_review_decision", &api.ok("submit_review_decision", &ctx, json!({ "draft_id": id, "decision": "approved", "reviewer_id": "u" })).await);
    check(
        "prepare_publish_request",
        &api.ok(
            "prepare_publish_request",
            &ctx,
            json!({ "draft_id": id, "zernio_profile_id": "profile_1", "channels": ["linkedin"], "action": "schedule", "scheduled_at": future() }),
        )
        .await,
    );
    let rp = api.ok("repurpose_content", &ctx, json!({ "report_id": rep["report_id"], "targets": [{ "channel": "x", "format": "thread" }] })).await;
    check("repurpose_content", &rp);
}

#[tokio::test]
async fn errors_always_use_the_envelope_and_openapi_is_served() {
    let h = harness(HarnessOptions::default());
    let api = Api::new(&h);
    let ctx = full_ctx("t1", "b1");
    let doc = openapi::spec();
    let env = jsonschema::validator_for(&json!({ "components": doc["components"], "allOf": [{ "$ref": "#/components/schemas/ErrorEnvelope" }] })).unwrap();
    for (tool, input) in [
        ("get_job", json!({ "job_id": "nope" })),
        ("ingest_url", json!({ "urls": [] })),
        ("ingest_url", json!({})),
        ("find_images", json!({ "draft_id": "x", "count": 9 })),
    ] {
        let (s, v) = api.call(tool, &ctx, input).await;
        assert!(s >= 400, "{tool}");
        assert!(env.is_valid(&v), "{tool}: {v}");
    }
    assert!(env.is_valid(&api.call("nope", &ctx, json!({})).await.1));
}

/// The committed docs/openapi.json must equal what the code generates.
#[test]
fn committed_openapi_document_is_current() {
    let committed = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/openapi.json"))
        .expect("docs/openapi.json exists; regenerate with `cargo run --bin content-agent-api -- --print-openapi > docs/openapi.json`");
    let committed: Value = serde_json::from_str(&committed).unwrap();
    assert_eq!(committed, openapi::spec(), "docs/openapi.json is stale; regenerate it");
}
