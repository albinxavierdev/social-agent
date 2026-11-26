//! `prepare_publish_request` (spec §6.10) plus the Zernio write path (`publish_draft` / `schedule_draft`).
//! Drafts can only reach Zernio from the `approved` state, through `ready_for_zernio`.

use super::validate::full_validation;
use super::*;
use crate::context::Access;
use crate::ids::PUBREQ;
use crate::model::{PublishAction, PublishRequest, PublishStatus};
use crate::queue::{NewTask, KIND_SUBMIT_ZERNIO};
use crate::validate::compose_text;
use crate::workflow::WorkflowState;
use crate::zernio::{action_required, build_post_payload, validate_connection};
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Deserialize)]
struct PrepareInput {
    draft_id: String,
    zernio_profile_id: String,
    channels: Vec<String>,
    action: PublishAction,
    #[serde(default)]
    scheduled_at: Option<DateTime<Utc>>,
}

fn prepared_json(pr: &PublishRequest, draft: &crate::model::Draft) -> Value {
    let content: serde_json::Map<String, Value> = draft
        .content
        .variants
        .iter()
        .filter(|v| pr.channels.contains(&crate::channels::canonical(&v.channel)))
        .map(|v| (v.channel.clone(), json!({ "caption": v.caption, "hashtags": v.hashtags, "cta": v.cta, "parts": v.parts, "text": compose_text(v) })))
        .collect();
    let media = pr.zernio_request_payload.get("mediaItems").cloned().unwrap_or_else(|| json!([]));
    json!({
        "publish_request_id": pr.id,
        "zernio_profile_id": pr.zernio_profile_id,
        "action": pr.action,
        "scheduled_at": pr.scheduled_at,
        "payload": { "channels": pr.channels, "content": content, "media": media, "zernio_request": pr.zernio_request_payload },
        "status": pr.status,
    })
}

pub async fn prepare_publish_request(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: PrepareInput = parse_input(input)?;
    call.ctx.require_action(action_required(input.action))?;
    require_len("channels", &input.channels, 1, 8)?;
    let channels = resolve_channels(call.ctx, &input.channels)?;
    let mut draft = state.store.get_draft(&call.scope, &input.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &input.draft_id))?;

    // Idempotent replay: an identical open request for this draft is returned as-is.
    for pr in state.store.list_publish_requests(&call.scope, &draft.id).await? {
        if pr.status == PublishStatus::ReadyForZernio
            && pr.action == input.action
            && pr.channels == channels
            && pr.scheduled_at == input.scheduled_at
            && pr.zernio_profile_id == input.zernio_profile_id
        {
            return ToolOutput::new(&prepared_json(&pr, &draft));
        }
    }
    // Hard gate: an explicit approval is the only way in (spec §19).
    if draft.state != WorkflowState::Approved {
        return Err(ApiError::invalid_state(format!("a draft must be approved before a publish request can be prepared (is {})", draft.state.as_str()))
            .with_details(json!({ "draft_id": draft.id, "state": draft.state })));
    }
    // Re-validate: the approved content must still be publishable right now.
    let rep = full_validation(state, &call.scope, &draft, false).await?;
    if !rep.issues.is_empty() {
        return Err(
            ApiError::new(ErrorCode::ContentPolicyViolation, "approved draft no longer passes validation").with_details(json!({ "issues": rep.issues }))
        );
    }
    // Zernio connection checks: connected, same tenant/brand, write access, channel in plan.
    let accounts = validate_connection(call.ctx, &input.zernio_profile_id, &channels, Access::Write)?;
    let payload = build_post_payload(&draft, &accounts, input.action, input.scheduled_at)?;

    let now = Utc::now();
    let pr = PublishRequest {
        id: new_id(PUBREQ),
        draft_id: draft.id.clone(),
        tenant_id: draft.tenant_id.clone(),
        brand_id: draft.brand_id.clone(),
        zernio_profile_id: input.zernio_profile_id,
        channels,
        action: input.action,
        scheduled_at: input.scheduled_at,
        zernio_request_payload: payload,
        zernio_response: None,
        status: PublishStatus::ReadyForZernio,
        zernio_idempotency_key: new_id("idem"),
        created_at: now,
        updated_at: now,
    };
    state.store.insert_publish_request(pr.clone()).await?;
    state.transition_draft(&call.who, &mut draft, WorkflowState::ReadyForZernio, &format!("publish request {} prepared", pr.id)).await?;
    state.sync_job(&call.who, &draft.job_id, WorkflowState::ReadyForZernio, "publish request prepared").await;
    tracing::info!(tenant_id = %pr.tenant_id, brand_id = %pr.brand_id, draft_id = %pr.draft_id, publish_request_id = %pr.id, zernio_profile_id = %pr.zernio_profile_id, "publish request prepared");
    ToolOutput::new(&prepared_json(&pr, &draft))
}

#[derive(Deserialize)]
struct SubmitInput {
    publish_request_id: String,
    /// Hand the submission to the durable queue instead of calling Zernio inline.
    #[serde(default, rename = "async")]
    asynchronous: bool,
}

pub async fn submit_tool(state: &AppState, call: &Call<'_>, input: Value, action: PublishAction) -> ApiResult<ToolOutput> {
    let input: SubmitInput = parse_input(input)?;
    let pr = state
        .store
        .get_publish_request(&call.scope, &input.publish_request_id)
        .await?
        .ok_or_else(|| ApiError::not_found("publish_request", &input.publish_request_id))?;
    if pr.action != action {
        return Err(ApiError::validation(format!("publish request was prepared for {:?}; use the matching tool", pr.action)));
    }
    if input.asynchronous {
        // Fail fast on connection problems so the caller learns now, not from a dead letter.
        validate_connection(call.ctx, &pr.zernio_profile_id, &pr.channels, Access::Write)?;
        let draft = state.store.get_draft(&call.scope, &pr.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &pr.draft_id))?;
        state
            .queue
            .enqueue(NewTask::new(
                KIND_SUBMIT_ZERNIO,
                &call.scope,
                Some(draft.job_id),
                json!({ "publish_request_id": pr.id, "context": call.ctx }),
                &call.ctx.request_id,
            ))
            .await?;
        return ToolOutput::new(&json!({ "publish_request_id": pr.id, "status": "queued" }));
    }
    let v = submit_publish_request(state, call.ctx, &call.who, &input.publish_request_id).await?;
    ToolOutput::new(&v)
}

/// Submit a prepared request to Zernio. Used by the tools (inline) and the worker (queued).
pub async fn submit_publish_request(state: &AppState, ctx: &RequestContext, who: &crate::app::Actor, publish_request_id: &str) -> ApiResult<Value> {
    let scope = &who.scope;
    let mut pr = state.store.get_publish_request(scope, publish_request_id).await?.ok_or_else(|| ApiError::not_found("publish_request", publish_request_id))?;
    ctx.require_action(action_required(pr.action))?;
    let mut draft = state.store.get_draft(scope, &pr.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &pr.draft_id))?;

    if pr.status == PublishStatus::ScheduledOrPublished {
        return Ok(
            json!({ "publish_request_id": pr.id, "status": pr.status, "draft_status": draft.state, "replayed": true, "zernio_response": pr.zernio_response }),
        );
    }
    if !matches!(pr.status, PublishStatus::ReadyForZernio | PublishStatus::SubmittedToZernio) {
        return Err(ApiError::invalid_state(format!("publish request is {:?} and cannot be submitted", pr.status)));
    }
    if !matches!(draft.state, WorkflowState::ReadyForZernio | WorkflowState::SubmittedToZernio) {
        return Err(ApiError::invalid_state(format!("draft is {} (expected ready_for_zernio)", draft.state.as_str())));
    }
    // Re-check write access against the fresh context: a read connection must never publish.
    validate_connection(ctx, &pr.zernio_profile_id, &pr.channels, Access::Write)?;
    if pr.action == PublishAction::Schedule && pr.scheduled_at.is_none_or(|t| t <= Utc::now()) {
        return Err(ApiError::validation("scheduled_at has passed; prepare a new publish request"));
    }
    let zernio = state.zernio.as_ref().ok_or_else(|| ApiError::internal("Zernio integration is not configured (ZERNIO_API_KEY)"))?;
    state.rate_limit_provider("zernio")?;

    if draft.state == WorkflowState::ReadyForZernio {
        state.transition_draft(who, &mut draft, WorkflowState::SubmittedToZernio, &format!("submitting {} to Zernio", pr.id)).await?;
        state.sync_job(who, &draft.job_id, WorkflowState::SubmittedToZernio, "submitted to zernio").await;
    }
    pr.status = PublishStatus::SubmittedToZernio;
    pr.updated_at = Utc::now();
    state.store.save_publish_request(&pr).await?;

    tracing::info!(tenant_id = %pr.tenant_id, brand_id = %pr.brand_id, draft_id = %pr.draft_id, publish_request_id = %pr.id, zernio_profile_id = %pr.zernio_profile_id, trace_id = %who.trace_id, "submitting to zernio");
    match zernio.submit_post(&pr.zernio_request_payload, &pr.zernio_idempotency_key, &ctx.request_id).await {
        Ok(out) => {
            pr.zernio_response = Some(out.raw.clone());
            pr.status = out.status;
            pr.updated_at = Utc::now();
            state.store.save_publish_request(&pr).await?;
            match out.status {
                PublishStatus::ScheduledOrPublished => {
                    state.transition_draft(who, &mut draft, WorkflowState::ScheduledOrPublished, "zernio accepted all channels").await?;
                    state.sync_job(who, &draft.job_id, WorkflowState::ScheduledOrPublished, "published or scheduled").await;
                }
                PublishStatus::Failed => {
                    state.transition_draft(who, &mut draft, WorkflowState::Failed, "zernio failed on every channel").await?;
                }
                // Partial success: keep the draft open so the failed channels can be retried via Zernio
                // without re-posting the channels that already succeeded.
                _ => {}
            }
            metrics::counter!("zernio_publish_total", "outcome" => format!("{:?}", out.status)).increment(1);
            Ok(
                json!({ "publish_request_id": pr.id, "status": pr.status, "draft_status": draft.state, "zernio_post_id": out.post_id, "platforms": out.platforms }),
            )
        }
        Err(e) => {
            pr.zernio_response = Some(json!({ "error": { "code": e.code.as_str(), "message": e.message, "details": e.details } }));
            pr.updated_at = Utc::now();
            if e.retryable {
                // Transient: reopen for retry. The stored idempotency key makes the retry safe.
                pr.status = PublishStatus::ReadyForZernio;
                state.store.save_publish_request(&pr).await?;
                state.transition_draft(who, &mut draft, WorkflowState::ReadyForZernio, "zernio transient failure; will retry").await?;
            } else {
                pr.status = PublishStatus::Failed;
                state.store.save_publish_request(&pr).await?;
                state.transition_draft(who, &mut draft, WorkflowState::Failed, &format!("zernio rejected the request: {}", e.code.as_str())).await?;
            }
            metrics::counter!("zernio_publish_total", "outcome" => "error").increment(1);
            Err(e)
        }
    }
}
