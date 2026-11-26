//! `validate_content` (spec §6.7)

use super::*;
use crate::generation;
use crate::model::{Compliance, Draft, Issue};
use crate::validate::{validate_deterministic, ValidationReport};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    draft_id: String,
    #[serde(default = "yes")]
    check_brand_rules: bool,
}

fn yes() -> bool {
    true
}

/// Deterministic checks + (optionally) the LLM tone/rule check. LLM outages degrade to a warning.
pub(super) async fn full_validation(state: &AppState, scope: &Scope, draft: &Draft, with_llm: bool) -> ApiResult<ValidationReport> {
    let prior = state.store.list_brand_drafts(scope, 200).await?;
    let mut rep = validate_deterministic(draft, &prior, &state.validation);
    let b = &draft.brand_context;
    if with_llm && (!b.content_rules.is_empty() || b.tone_of_voice.is_some()) {
        if state.rate_limit_provider("llm").is_ok() {
            match generation::check_brand_rules(&state.llm, &draft.content, b).await {
                Ok(r) => rep.issues.extend(r.violations.into_iter().map(|v| Issue { code: "BRAND_RULE_VIOLATION".into(), channel: None, message: v })),
                Err(e) => rep.warnings.push(format!("brand rule check unavailable: {}", e.message)),
            }
        } else {
            rep.warnings.push("brand rule check skipped: LLM rate limit reached".into());
        }
    }
    Ok(rep)
}

pub async fn validate_content(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    let mut draft = state.store.get_draft(&call.scope, &input.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &input.draft_id))?;
    let rep = full_validation(state, &call.scope, &draft, input.check_brand_rules).await?;
    metrics::counter!("draft_validation_total", "status" => rep.status()).increment(1);

    draft.compliance = Compliance { status: rep.status().into(), warnings: rep.warnings.clone(), issues: rep.issues.clone() };
    draft.updated_at = chrono::Utc::now();
    let state_now = draft.state;
    state.store.save_draft(&draft, state_now).await?;

    ToolOutput::new(&json!({ "draft_id": draft.id, "status": rep.status(), "issues": rep.issues, "warnings": rep.warnings }))
}
