//! HTTP surface: `/internal/v1/...` (HMAC-authenticated) plus unauthenticated health/metrics endpoints.

use crate::app::AppState;
use crate::auth::require_internal_auth;
use crate::context::RequestContext;
use crate::error::ApiError;
use crate::tools;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::limit::RequestBodyLimitLayer;

#[derive(Deserialize)]
struct ToolRequest {
    context: RequestContext,
    #[serde(default)]
    input: Value,
}

pub fn router(state: AppState) -> Router {
    let internal = Router::new()
        .route("/tools", get(list_tools))
        .route("/tools/{tool}", post(call_tool))
        .route("/openapi.json", get(openapi))
        .layer(middleware::from_fn_with_state(state.clone(), require_internal_auth));

    let max = state.cfg.max_body_bytes;
    let mut app = Router::new().route("/healthz", get(healthz)).route("/readyz", get(readyz)).route("/metrics", get(metrics)).nest("/internal/v1", internal);
    if state.cfg.console_enabled {
        app = app.route("/console", get(console));
    }
    app.fallback(|| async { ApiError::not_found("route", "-") }).layer(RequestBodyLimitLayer::new(max)).with_state(state)
}

/// Developer console: one static page, no build step, no secrets inside. The browser signs requests itself with a
/// secret the developer types in. Only mounted when CONSOLE_ENABLED=true (refused in production).
async fn console() -> Response {
    const PAGE: &str = include_str!("../static/console.html");
    (
        StatusCode::OK,
        [
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("x-frame-options", "DENY"),
            (
                "content-security-policy",
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src https: data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            ),
        ],
        PAGE,
    )
        .into_response()
}

async fn healthz() -> &'static str {
    "ok"
}

/// Ready = the store answers and the queue can be inspected.
async fn readyz(State(s): State<AppState>) -> Response {
    let db = s.store.ping().await;
    let q = s.queue.stats().await;
    match (db, q) {
        (Ok(()), Ok(stats)) => {
            metrics::gauge!("queue_depth").set(stats.pending as f64);
            metrics::gauge!("queue_oldest_pending_age_seconds").set(stats.oldest_pending_age_secs as f64);
            metrics::gauge!("queue_dead_letters").set(stats.dead as f64);
            Json(json!({ "status": "ready", "queue": stats })).into_response()
        }
        (db, q) => {
            tracing::error!(db_ok = db.is_ok(), queue_ok = q.is_ok(), "readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "unavailable" }))).into_response()
        }
    }
}

async fn metrics(State(s): State<AppState>) -> Response {
    match &s.metrics {
        Some(h) => (StatusCode::OK, [("content-type", "text/plain; version=0.0.4")], h.render()).into_response(),
        None => (StatusCode::NOT_FOUND, "metrics disabled").into_response(),
    }
}

async fn list_tools() -> Json<Value> {
    Json(json!({ "tools": tools::TOOLS }))
}

async fn openapi() -> Json<Value> {
    Json(crate::openapi::spec())
}

async fn call_tool(State(state): State<AppState>, Path(tool): Path<String>, body: Bytes) -> Response {
    let req: ToolRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return ApiError::validation(format!("body must be {{\"context\": {{...}}, \"input\": {{...}}}}: {e}")).into_response(),
    };
    let request_id = req.context.request_id.clone();
    let (result, trace_id) = tools::invoke(&state, &tool, req.context, req.input).await;
    let mut resp = match result {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => e.into_response(),
    };
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&trace_id) {
        h.insert("x-trace-id", v);
    }
    if let Ok(v) = HeaderValue::from_str(&request_id) {
        h.insert("x-request-id", v);
    }
    resp
}
