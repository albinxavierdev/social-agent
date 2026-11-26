//! `generate_social_draft` (spec §6.4) and shared draft persistence.

use super::*;
use crate::generation::{self, Target};
use crate::ids::DRAFT;
use crate::model::{BrandContext, Compliance, Draft, DraftContent, ModelInfo, Report, Variant};
use crate::validate::validate_deterministic;
use crate::workflow::WorkflowState;
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    report_id: String,
    channels: Vec<String>,
    #[serde(default = "single_image")]
    post_type: String,
    #[serde(default)]
    brand_context: Option<BrandContext>,
    #[serde(default = "yes")]
    include_hashtags: bool,
    #[serde(default = "yes")]
    include_cta: bool,
}

fn single_image() -> String {
    "single_image".into()
}
fn yes() -> bool {
    true
}

const POST_TYPES: &[&str] = &["single_image", "carousel", "thread", "text", "video", "story", "ad"];

pub(super) fn check_post_type(t: &str) -> ApiResult<()> {
    if POST_TYPES.contains(&t) {
        Ok(())
    } else {
        Err(ApiError::validation(format!("post_type must be one of {POST_TYPES:?}")))
    }
}

/// Compliance computed at creation time. A missing image is expected before `find_images`, so it is
/// downgraded to a warning here; `review_draft` re-validates strictly.
pub(super) async fn preview_compliance(state: &AppState, scope: &Scope, draft: &Draft) -> ApiResult<Compliance> {
    let prior = state.store.list_brand_drafts(scope, 200).await?;
    let mut rep = validate_deterministic(draft, &prior, &state.validation);
    let (media, real): (Vec<_>, Vec<_>) = rep.issues.drain(..).partition(|i| i.code == "MEDIA_REQUIRED");
    rep.issues = real;
    for m in media {
        rep.warnings.push(format!("[{}] media is required before review: call find_images", m.channel.unwrap_or_default()));
    }
    Ok(Compliance { status: rep.status().into(), warnings: rep.warnings, issues: rep.issues })
}

pub(super) async fn persist_draft(
    state: &AppState,
    call: &Call<'_>,
    report: &Report,
    variants: Vec<Variant>,
    campaign_id: Option<String>,
    info: &ModelInfo,
    brand: &BrandContext,
) -> ApiResult<Draft> {
    let now = chrono::Utc::now();
    let channel = if variants.len() == 1 { variants[0].channel.clone() } else { "multi".to_string() };
    let format = variants.first().map(|v| v.format.clone()).unwrap_or_else(|| "text".into());
    let mut draft = Draft {
        id: new_id(DRAFT),
        job_id: report.job_id.clone(),
        report_id: report.id.clone(),
        campaign_id,
        tenant_id: call.scope.tenant_id.clone(),
        brand_id: call.scope.brand_id.clone(),
        channel,
        format,
        state: WorkflowState::DraftReady,
        content: DraftContent { variants },
        media_assets: vec![],
        source_links: report.payload.source_links.clone(),
        compliance: Compliance::pending(),
        brand_context: brand.clone(),
        model: info.clone(),
        created_at: now,
        updated_at: now,
    };
    draft.compliance = preview_compliance(state, &call.scope, &draft).await?;
    state.store.insert_draft(draft.clone()).await?;
    // Audit the creation as Created -> DraftReady.
    state
        .store
        .record_transition(crate::workflow::Transition {
            id: new_id("tr"),
            tenant_id: draft.tenant_id.clone(),
            brand_id: draft.brand_id.clone(),
            entity_kind: "draft".into(),
            entity_id: draft.id.clone(),
            actor_id: call.who.actor_id.clone(),
            from_state: WorkflowState::Created,
            to_state: WorkflowState::DraftReady,
            reason: "draft generated".into(),
            correlation_id: call.who.correlation_id.clone(),
            trace_id: call.trace_id.clone(),
            at: now,
        })
        .await?;
    state.sync_job(&call.who, &draft.job_id, WorkflowState::DraftReady, "draft generated").await;
    Ok(draft)
}

pub(super) fn draft_variants_json(d: &Draft) -> Value {
    json!(d
        .content
        .variants
        .iter()
        .map(|v| json!({
            "channel": v.channel, "format": v.format, "caption": v.caption, "hashtags": v.hashtags, "cta": v.cta, "parts": v.parts, "media": v.media,
        }))
        .collect::<Vec<_>>())
}

pub async fn generate_social_draft(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    require_len("channels", &input.channels, 1, 8)?;
    check_post_type(&input.post_type)?;
    let channels = resolve_channels(call.ctx, &input.channels)?;
    let report = state.store.get_report(&call.scope, &input.report_id).await?.ok_or_else(|| ApiError::not_found("report", &input.report_id))?;
    let brand = input.brand_context.unwrap_or_else(|| report.brand_context.clone());

    state.rate_limit_provider("llm")?;
    let targets: Vec<Target> = channels.iter().map(|c| Target { channel: c.clone(), format: input.post_type.clone() }).collect();
    let (variants, info) =
        generation::generate_variants(&state.llm, &report.payload, &targets, &input.post_type, &brand, input.include_hashtags, input.include_cta).await?;
    let draft = persist_draft(state, call, &report, variants, None, &info, &brand).await?;

    ToolOutput::new(&json!({
        "draft_id": draft.id,
        "report_id": draft.report_id,
        "status": draft.state,
        "variants": draft_variants_json(&draft),
        "source_links": draft.source_links,
        "compliance": { "status": draft.compliance.status, "warnings": draft.compliance.warnings, "issues": draft.compliance.issues },
        "model": { "provider": info.provider, "model": info.model, "prompt_version": info.prompt_version },
    }))
    .map(|o| o.with_usage(&info))
}
