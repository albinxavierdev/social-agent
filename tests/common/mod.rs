#![allow(dead_code)]
//! Shared helpers for integration tests: signed HTTP calls through the real router + a worker drain loop.

use axum::{body::Body, http::Request, Router};
use content_agent::{
    app::AppState,
    context::RequestContext,
    testing::{signed_headers, Harness},
    worker,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::time::Duration;
use tower::ServiceExt;

pub struct Api {
    pub router: Router,
    pub state: AppState,
}

impl Api {
    pub fn new(h: &Harness) -> Self {
        Self { router: content_agent::api::router(h.state.clone()), state: h.state.clone() }
    }

    /// POST a signed tool call; returns (status, json body, headers' trace id).
    pub async fn call(&self, tool: &str, ctx: &RequestContext, input: Value) -> (u16, Value) {
        let path = format!("/internal/v1/tools/{tool}");
        let body = serde_json::to_vec(&json!({ "context": ctx, "input": input })).unwrap();
        let mut req = Request::builder().method("POST").uri(&path).header("content-type", "application/json");
        for (k, v) in signed_headers("POST", &path, &body) {
            req = req.header(k, v);
        }
        let resp = self.router.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
        let status = resp.status().as_u16();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    /// Call and require success.
    pub async fn ok(&self, tool: &str, ctx: &RequestContext, input: Value) -> Value {
        let (s, v) = self.call(tool, ctx, input).await;
        assert_eq!(s, 200, "{tool} failed: {v}");
        v
    }

    /// Call and require a specific error code.
    pub async fn err(&self, tool: &str, ctx: &RequestContext, input: Value, code: &str) -> Value {
        let (s, v) = self.call(tool, ctx, input).await;
        assert_ne!(s, 200, "{tool} unexpectedly succeeded: {v}");
        assert_eq!(v["error"]["code"], code, "{tool}: {v}");
        v
    }
}

/// Run every ready queue task to completion (as the worker would).
pub async fn drain(state: &AppState) -> usize {
    let mut n = 0;
    while let Some(task) = state.queue.lease(Duration::from_secs(60)).await.unwrap() {
        worker::process(state, task).await;
        n += 1;
        assert!(n < 500, "worker loop did not settle");
    }
    n
}

pub const ARTICLE_HTML: &str = r#"<!doctype html><html lang="en"><head><title>Why Rust Wins</title>
<meta property="og:description" content="A long form look at why Rust is winning in backend systems everywhere today.">
<meta name="author" content="Jane Doe"><meta property="og:image" content="/hero.png"></head><body><nav>menu</nav><article>
<h1>Why Rust Wins</h1>
<p>Rust makes systems programming safer by preventing data races at compile time for teams of any size.</p>
<p>Teams that adopt Rust report fewer production incidents and faster onboarding for new engineers over time.</p>
<p>The ecosystem around async programming has matured quickly and now powers large production backends worldwide.</p>
</article><footer>footer</footer></body></html>"#;

/// Drive a draft all the way to `approved` and return `(job_id, draft_id)`.
pub async fn approved_draft(api: &Api, h: &Harness, ctx: &RequestContext, url: &str, channels: &[&str]) -> (Value, Value) {
    let ing = api.ok("ingest_url", ctx, json!({ "urls": [url] })).await;
    drain(&h.state).await;
    let rep = api.ok("generate_content_report", ctx, json!({ "source_ids": [ing["sources"][0]["source_id"]] })).await;
    let d = api.ok("generate_social_draft", ctx, json!({ "report_id": rep["report_id"], "channels": channels, "post_type": "text" })).await;
    let id = d["draft_id"].clone();
    api.ok("review_draft", ctx, json!({ "draft_id": id })).await;
    api.ok("submit_review_decision", ctx, json!({ "draft_id": id, "decision": "approved", "reviewer_id": "u1" })).await;
    (ing["job_id"].clone(), id)
}

pub fn future() -> String {
    (chrono::Utc::now() + chrono::Duration::days(2)).to_rfc3339()
}
