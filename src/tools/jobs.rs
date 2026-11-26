//! Operational tools: `get_job`, `get_draft`, `cancel_job`.

use super::*;
use crate::workflow::WorkflowState;
use serde::Deserialize;

#[derive(Deserialize)]
struct JobInput {
    job_id: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
struct DraftInput {
    draft_id: String,
}

pub async fn get_job(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: JobInput = parse_input(input)?;
    let job = state.store.get_job(&call.scope, &input.job_id).await?.ok_or_else(|| ApiError::not_found("job", &input.job_id))?;
    let sources = state.store.list_sources(&call.scope, &job.id).await?;
    let transitions = state.store.list_transitions(&call.scope, &job.id).await?;
    ToolOutput::new(&json!({
        "job_id": job.id, "status": job.state, "source_type": job.source_type, "requested_channels": job.requested_channels,
        "created_at": job.created_at, "updated_at": job.updated_at, "completed_at": job.completed_at, "error": job.error,
        "sources": sources.iter().map(|s| json!({ "source_id": s.id, "url": s.url, "source_type": s.source_type, "extraction_status": s.extraction_status, "extraction_method": s.extraction_method, "title": s.title, "error": s.error })).collect::<Vec<_>>(),
        "events": transitions.iter().map(|t| json!({ "from": t.from_state, "to": t.to_state, "reason": t.reason, "actor_id": t.actor_id, "at": t.at, "trace_id": t.trace_id })).collect::<Vec<_>>(),
    }))
}

pub async fn get_draft(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: DraftInput = parse_input(input)?;
    let d = state.store.get_draft(&call.scope, &input.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &input.draft_id))?;
    let reviews = state.store.list_review_events(&call.scope, &d.id).await?;
    let publish = state.store.list_publish_requests(&call.scope, &d.id).await?;
    let transitions = state.store.list_transitions(&call.scope, &d.id).await?;
    ToolOutput::new(&json!({
        "draft_id": d.id, "job_id": d.job_id, "report_id": d.report_id, "campaign_id": d.campaign_id, "status": d.state,
        "variants": super::draft::draft_variants_json(&d), "media_assets": d.media_assets, "source_links": d.source_links, "compliance": d.compliance,
        "model": { "provider": d.model.provider, "model": d.model.model, "prompt_version": d.model.prompt_version },
        "reviews": reviews.iter().map(|r| json!({ "review_event_id": r.id, "reviewer_id": r.reviewer_id, "decision": r.decision, "notes": r.notes, "at": r.created_at })).collect::<Vec<_>>(),
        "publish_requests": publish.iter().map(|p| json!({ "publish_request_id": p.id, "status": p.status, "action": p.action, "channels": p.channels, "scheduled_at": p.scheduled_at })).collect::<Vec<_>>(),
        "events": transitions.iter().map(|t| json!({ "from": t.from_state, "to": t.to_state, "reason": t.reason, "actor_id": t.actor_id, "at": t.at, "trace_id": t.trace_id })).collect::<Vec<_>>(),
    }))
}

pub async fn cancel_job(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: JobInput = parse_input(input)?;
    let mut job = state.store.get_job(&call.scope, &input.job_id).await?.ok_or_else(|| ApiError::not_found("job", &input.job_id))?;
    if job.state == WorkflowState::Cancelled {
        return ToolOutput::new(&json!({ "job_id": job.id, "status": job.state, "cancelled_tasks": 0 }));
        // idempotent
    }
    let reason = input.reason.unwrap_or_else(|| "cancelled by caller".into());
    state.transition_job(&call.who, &mut job, WorkflowState::Cancelled, &reason).await?;
    // Queued work is cancelled; an in-flight extraction finishes but its result is discarded by the worker's job-state check.
    let n = state.queue.cancel_job(&call.scope, &job.id).await?;
    ToolOutput::new(&json!({ "job_id": job.id, "status": job.state, "cancelled_tasks": n }))
}
