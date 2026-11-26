//! Tool registry and dispatcher (spec §6, §7.1). Every tool is independently callable and goes through
//! the same enforcement pipeline: context validation → tool/action authorization → quota → rate limits →
//! timeout → handler → audit record + usage event + metrics.

mod draft;
mod extract;
mod images;
mod ingest;
mod jobs;
mod publish;
mod report;
mod repurpose;
mod review;
mod validate;
mod zernio_read;

use crate::app::{Actor, AppState};
use crate::context::{Action, RequestContext, Scope};
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::ids::{new_id, INVOCATION};
use crate::model::ToolInvocation;
use crate::usage::UsageEvent;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZernioRequirement {
    No,
    Optional,
    Yes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZernioAccess {
    None,
    Read,
    Write,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub zernio_connection: ZernioRequirement,
    pub zernio_access: ZernioAccess,
    /// Permission `action` the caller must hold. `None` = the handler decides (depends on input).
    pub required_action: Option<Action>,
}

const fn t(name: &'static str, description: &'static str, z: ZernioRequirement, a: ZernioAccess, action: Option<Action>) -> ToolSpec {
    ToolSpec { name, description, zernio_connection: z, zernio_access: a, required_action: action }
}

use Action::*;
use ZernioAccess as A;
use ZernioRequirement as Z;

/// The registry. Rows for the first seven tools mirror the table in spec §7.1.
pub const TOOLS: &[ToolSpec] = &[
    t("ingest_url", "Create an ingestion job from one or more URLs", Z::No, A::None, Some(Read)),
    t("extract_content", "Extract clean structured content from a source", Z::No, A::None, Some(Read)),
    t("generate_content_report", "Generate a structured content report from extracted sources", Z::No, A::None, Some(Draft)),
    t("generate_social_draft", "Generate channel-specific drafts from a report", Z::No, A::None, Some(Draft)),
    t("repurpose_content", "Repurpose a source, report or draft into multiple variants", Z::No, A::None, Some(Draft)),
    t("find_images", "Find, validate, rank and store image candidates for a draft", Z::No, A::None, Some(Draft)),
    t("validate_content", "Run pre-publication checks on a draft", Z::No, A::None, Some(Draft)),
    t("review_draft", "Create or update the human-review state of a draft", Z::No, A::None, Some(Draft)),
    t("submit_review_decision", "Record the customer's review decision", Z::No, A::None, Some(Draft)),
    t("prepare_publish_request", "Create a Zernio-ready publish/schedule request for an approved draft", Z::Yes, A::Write, None),
    t("read_x_post", "Read a public X post", Z::Optional, A::Read, Some(Read)),
    t("read_youtube_video", "Read a YouTube video's metadata", Z::Optional, A::Read, Some(Read)),
    t("read_comments", "Read comments via Zernio", Z::Yes, A::Read, Some(Read)),
    t("read_inbox", "Read inbox conversations via Zernio", Z::Yes, A::Read, Some(Read)),
    t("read_performance", "Read performance analytics via Zernio", Z::Yes, A::Read, Some(Read)),
    t("publish_draft", "Submit a prepared request to Zernio for immediate publishing", Z::Yes, A::Write, Some(Publish)),
    t("schedule_draft", "Submit a prepared request to Zernio for scheduled publishing", Z::Yes, A::Write, Some(Schedule)),
    t("get_job", "Get a job, its sources and its audit trail", Z::No, A::None, Some(Read)),
    t("get_draft", "Get a draft, its review events and its audit trail", Z::No, A::None, Some(Read)),
    t("cancel_job", "Cancel a job and its queued work", Z::No, A::None, Some(Draft)),
];

pub fn spec(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

/// What a handler hands back: the contract JSON plus any metered usage.
pub struct ToolOutput {
    pub value: Value,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl ToolOutput {
    pub fn new<T: Serialize>(v: &T) -> ApiResult<Self> {
        Ok(Self { value: serde_json::to_value(v).map_err(ApiError::internal)?, input_tokens: 0, output_tokens: 0, cost_usd: 0.0 })
    }
    pub fn with_usage(mut self, m: &crate::model::ModelInfo) -> Self {
        self.input_tokens += m.input_tokens;
        self.output_tokens += m.output_tokens;
        self.cost_usd += m.cost_usd;
        self
    }
}

/// Per-call data shared by every handler.
pub struct Call<'a> {
    pub ctx: &'a RequestContext,
    pub trace_id: String,
    pub scope: Scope,
    pub who: Actor,
}

pub(crate) fn parse_input<T: DeserializeOwned>(input: Value) -> ApiResult<T> {
    serde_json::from_value(input).map_err(|e| ApiError::validation(format!("invalid input: {e}")))
}

pub(crate) fn require_len<T>(field: &str, v: &[T], min: usize, max: usize) -> ApiResult<()> {
    if v.len() < min || v.len() > max {
        return Err(ApiError::validation(format!("{field} must contain between {min} and {max} items")));
    }
    Ok(())
}

pub(crate) fn input_hash(input: &Value) -> String {
    hex::encode(Sha256::digest(input.to_string().as_bytes()))
}

fn enforce(state: &AppState, spec: &ToolSpec, ctx: &RequestContext) -> ApiResult<()> {
    ctx.validate()?;
    if state.cfg.disabled_tools.contains(spec.name) {
        return Err(ApiError::forbidden(format!("tool '{}' is currently disabled", spec.name)));
    }
    ctx.require_tool(spec.name)?;
    if let Some(a) = spec.required_action {
        ctx.require_action(a)?;
    }
    ctx.require_quota()?;
    let (t, b) = (&ctx.tenant_id, &ctx.brand_id);
    state.limiter.check(&format!("tenant:{t}"), state.cfg.rate_tenant_per_min, "tenant")?;
    state.limiter.check(&format!("brand:{t}:{b}"), state.cfg.rate_brand_per_min, "brand")?;
    state.limiter.check(&format!("tool:{t}:{}", spec.name), state.cfg.rate_tool_per_min, "tool")?;
    Ok(())
}

async fn dispatch(state: &AppState, name: &str, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    match name {
        "ingest_url" => ingest::ingest_url(state, call, input).await,
        "extract_content" => extract::extract_content(state, call, input).await,
        "generate_content_report" => report::generate_content_report(state, call, input).await,
        "generate_social_draft" => draft::generate_social_draft(state, call, input).await,
        "repurpose_content" => repurpose::repurpose_content(state, call, input).await,
        "find_images" => images::find_images(state, call, input).await,
        "validate_content" => validate::validate_content(state, call, input).await,
        "review_draft" => review::review_draft(state, call, input).await,
        "submit_review_decision" => review::submit_review_decision(state, call, input).await,
        "prepare_publish_request" => publish::prepare_publish_request(state, call, input).await,
        "publish_draft" => publish::submit_tool(state, call, input, crate::model::PublishAction::Publish).await,
        "schedule_draft" => publish::submit_tool(state, call, input, crate::model::PublishAction::Schedule).await,
        "read_x_post" => zernio_read::read_source(state, call, input, "x").await,
        "read_youtube_video" => zernio_read::read_source(state, call, input, "youtube").await,
        "read_comments" => zernio_read::read_zernio(state, call, input, crate::zernio::ReadResource::Comments).await,
        "read_inbox" => zernio_read::read_zernio(state, call, input, crate::zernio::ReadResource::Inbox).await,
        "read_performance" => zernio_read::read_zernio(state, call, input, crate::zernio::ReadResource::Performance).await,
        "get_job" => jobs::get_job(state, call, input).await,
        "get_draft" => jobs::get_draft(state, call, input).await,
        "cancel_job" => jobs::cancel_job(state, call, input).await,
        _ => Err(ApiError::not_found("tool", name)),
    }
}

/// Entry point used by the HTTP layer. Returns the tool's contract JSON and the trace id.
pub async fn invoke(state: &AppState, tool: &str, ctx: RequestContext, input: Value) -> (ApiResult<Value>, String) {
    let trace_id = AppState::new_trace_id();
    let started = Instant::now();
    let Some(spec) = spec(tool) else {
        return (Err(ApiError::not_found("tool", tool)), trace_id);
    };
    let hash = input_hash(&input);

    let result: ApiResult<ToolOutput> = async {
        enforce(state, spec, &ctx)?;
        let who = Actor { scope: ctx.scope(), actor_id: ctx.actor_id.clone(), correlation_id: ctx.request_id.clone(), trace_id: trace_id.clone() };
        let call = Call { ctx: &ctx, trace_id: trace_id.clone(), scope: ctx.scope(), who };
        match tokio::time::timeout(state.cfg.tool_timeout, dispatch(state, tool, &call, input)).await {
            Ok(r) => r,
            Err(_) => Err(ApiError::new(ErrorCode::InternalError, "tool timed out").retryable(true)),
        }
    }
    .await;

    let latency_ms = started.elapsed().as_millis() as u64;
    let (status, usage) = match &result {
        Ok(o) => ("success", Some((o.input_tokens, o.output_tokens, o.cost_usd))),
        Err(_) => ("error", None),
    };
    metrics::counter!("tool_requests_total", "tool" => spec.name, "status" => status).increment(1);
    metrics::histogram!("tool_latency_seconds", "tool" => spec.name).record(latency_ms as f64 / 1000.0);

    // Audit + usage. Failures here are logged, never surfaced: they must not turn a success into an error.
    if ctx.validate().is_ok() {
        let (i, o, c) = usage.unwrap_or((0, 0, 0.0));
        let rec = ToolInvocation {
            id: new_id(INVOCATION),
            tenant_id: ctx.tenant_id.clone(),
            brand_id: ctx.brand_id.clone(),
            actor_id: ctx.actor_id.clone(),
            tool_name: spec.name.into(),
            input_hash: hash,
            status: status.into(),
            latency_ms,
            token_usage: usage.map(|_| json!({ "input": i, "output": o })),
            cost: usage.map(|_| c),
            error: result.as_ref().err().map(|e| json!({ "code": e.code.as_str(), "message": e.message })),
            trace_id: trace_id.clone(),
            request_id: ctx.request_id.clone(),
            created_at: chrono::Utc::now(),
        };
        if let Err(e) = state.store.record_tool_invocation(rec).await {
            tracing::error!(error = %e.message, "failed to persist tool invocation");
        }
        state
            .usage
            .record(UsageEvent {
                tenant_id: ctx.tenant_id.clone(),
                brand_id: ctx.brand_id.clone(),
                actor_id: ctx.actor_id.clone(),
                tool: spec.name.into(),
                request_id: ctx.request_id.clone(),
                trace_id: trace_id.clone(),
                status: status.into(),
                input_tokens: i,
                output_tokens: o,
                cost_usd: c,
                latency_ms,
            })
            .await;
    }
    tracing::info!(tool = spec.name, tenant_id = %ctx.tenant_id, brand_id = %ctx.brand_id, request_id = %ctx.request_id, trace_id = %trace_id, status, latency_ms, "tool invocation");

    (result.map(|o| o.value), trace_id)
}

/// Dedupe + canonicalise a channel list, keeping only channels we support and the caller may use.
pub(crate) fn resolve_channels(ctx: &RequestContext, raw: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = vec![];
    for c in raw {
        let c = crate::channels::canonical(c);
        if crate::channels::spec(&c).is_none() {
            return Err(ApiError::validation(format!("unsupported channel '{c}'")));
        }
        ctx.require_channel(&c)?;
        if !out.contains(&c) {
            out.push(c);
        }
    }
    Ok(out)
}

pub use publish::submit_publish_request;
