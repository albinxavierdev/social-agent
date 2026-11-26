//! `review_draft` (spec §6.8) and `submit_review_decision` (spec §6.9)

use super::validate::full_validation;
use super::*;
use crate::ids::REVIEW;
use crate::model::{Compliance, Decision, Draft, DraftContent, ReviewEvent};
use crate::workflow::WorkflowState;
use serde::Deserialize;

pub(super) fn next_action(s: WorkflowState) -> &'static str {
    use WorkflowState::*;
    match s {
        DraftReady => "review_draft",
        AwaitingCustomerReview => "letwemarket_review",
        Approved => "prepare_publish_request",
        RevisionRequested => "revise_draft",
        ReadyForZernio => "submit_to_zernio",
        SubmittedToZernio => "await_zernio_result",
        _ => "none",
    }
}

#[derive(Deserialize)]
struct ReviewInput {
    draft_id: String,
    /// "submit" (default) moves the draft into review; "status" only reports the current state.
    #[serde(default)]
    action: Option<String>,
}

fn review_output(state: &AppState, d: &Draft) -> ApiResult<ToolOutput> {
    ToolOutput::new(&json!({
        "job_id": d.job_id,
        "draft_id": d.id,
        "status": d.state,
        "review_url": state.cfg.review_url(&d.brand_id, &d.id),
        "next_action": next_action(d.state),
    }))
}

pub async fn review_draft(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: ReviewInput = parse_input(input)?;
    let mut draft = state.store.get_draft(&call.scope, &input.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &input.draft_id))?;
    match input.action.as_deref().unwrap_or("submit") {
        "status" => return review_output(state, &draft),
        "submit" => {}
        other => return Err(ApiError::validation(format!("unknown action '{other}' (expected submit|status)"))),
    }
    if draft.state == WorkflowState::AwaitingCustomerReview {
        return review_output(state, &draft); // idempotent
    }
    if !matches!(draft.state, WorkflowState::DraftReady | WorkflowState::RevisionRequested) {
        return Err(ApiError::invalid_state(format!("a draft in state {} cannot be submitted for review", draft.state.as_str())));
    }

    // Strict gate: nothing reaches a human reviewer unless it passes the deterministic checks.
    // Brand-rule findings from an earlier LLM `validate_content` run stay blocking until re-validated.
    let mut rep = full_validation(state, &call.scope, &draft, false).await?;
    rep.issues.extend(draft.compliance.issues.iter().filter(|i| i.code == "BRAND_RULE_VIOLATION").cloned());
    draft.compliance = Compliance { status: rep.status().into(), warnings: rep.warnings.clone(), issues: rep.issues.clone() };
    if !rep.issues.is_empty() {
        let prior = draft.state;
        state.store.save_draft(&draft, prior).await?;
        return Err(ApiError::new(ErrorCode::ContentPolicyViolation, "draft failed validation and cannot be sent for review")
            .with_details(json!({ "draft_id": draft.id, "issues": rep.issues })));
    }
    state.transition_draft(&call.who, &mut draft, WorkflowState::AwaitingCustomerReview, "submitted for customer review").await?;
    state.sync_job(&call.who, &draft.job_id, WorkflowState::AwaitingCustomerReview, "draft awaiting review").await;
    review_output(state, &draft)
}

#[derive(Deserialize)]
struct DecisionInput {
    draft_id: String,
    decision: Decision,
    #[serde(default)]
    edited_content: Option<Value>,
    reviewer_id: String,
    #[serde(default)]
    notes: Option<String>,
}

fn decision_state(d: Decision) -> WorkflowState {
    match d {
        Decision::Approved => WorkflowState::Approved,
        Decision::Rejected => WorkflowState::Rejected,
        Decision::RevisionRequested => WorkflowState::RevisionRequested,
        Decision::Cancelled => WorkflowState::Cancelled,
    }
}

pub async fn submit_review_decision(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: DecisionInput = parse_input(input)?;
    if input.reviewer_id.trim().is_empty() || input.reviewer_id.len() > 128 {
        return Err(ApiError::validation("reviewer_id is required"));
    }
    if input.notes.as_deref().is_some_and(|n| n.len() > 5000) {
        return Err(ApiError::validation("notes must be at most 5000 characters"));
    }
    let mut draft = state.store.get_draft(&call.scope, &input.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &input.draft_id))?;
    let target = decision_state(input.decision);
    if draft.state == target {
        // Idempotent replay of the same decision.
        return ToolOutput::new(&json!({ "draft_id": draft.id, "status": draft.state, "next_action": next_action(draft.state), "replayed": true }));
    }
    if draft.state != WorkflowState::AwaitingCustomerReview {
        return Err(ApiError::invalid_state(format!(
            "decisions can only be submitted while the draft is awaiting_customer_review (is {})",
            draft.state.as_str()
        )));
    }

    // Optional reviewer edits: same channels only, never more variants than were generated.
    let edited = match input.edited_content.as_ref() {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) if m.is_empty() => None,
        Some(v) => {
            let c: DraftContent = serde_json::from_value(v.clone()).map_err(|e| ApiError::validation(format!("edited_content is invalid: {e}")))?;
            let allowed: Vec<String> = draft.content.variants.iter().map(|v| crate::channels::canonical(&v.channel)).collect();
            for v in &c.variants {
                if !allowed.contains(&crate::channels::canonical(&v.channel)) {
                    return Err(ApiError::validation(format!("edited_content adds channel '{}' that is not part of this draft", v.channel)));
                }
            }
            Some(c)
        }
    };
    if let Some(c) = &edited {
        draft.content = c.clone();
        for v in &mut draft.content.variants {
            v.channel = crate::channels::canonical(&v.channel);
        }
        let rep = full_validation(state, &call.scope, &draft, false).await?;
        if input.decision == Decision::Approved && !rep.issues.is_empty() {
            return Err(ApiError::new(ErrorCode::ContentPolicyViolation, "edited content fails validation and cannot be approved")
                .with_details(json!({ "draft_id": draft.id, "issues": rep.issues })));
        }
        draft.compliance = Compliance { status: rep.status().into(), warnings: rep.warnings, issues: rep.issues };
    }

    let event = ReviewEvent {
        id: new_id(REVIEW),
        draft_id: draft.id.clone(),
        tenant_id: draft.tenant_id.clone(),
        brand_id: draft.brand_id.clone(),
        reviewer_id: input.reviewer_id.clone(),
        decision: input.decision,
        edited_content: edited.map(|c| serde_json::to_value(c).unwrap_or(Value::Null)),
        notes: input.notes.clone(),
        created_at: chrono::Utc::now(),
    };
    let reason = format!("review decision by {}: {:?}", input.reviewer_id, input.decision);
    state.transition_draft(&call.who, &mut draft, target, &reason).await?;
    state.store.insert_review_event(event.clone()).await?;
    state.sync_job(&call.who, &draft.job_id, target, &reason).await;

    ToolOutput::new(
        &json!({ "draft_id": draft.id, "status": draft.state, "review_event_id": event.id, "next_action": next_action(draft.state), "compliance": draft.compliance }),
    )
}
