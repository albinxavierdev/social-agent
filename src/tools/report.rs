//! `generate_content_report` (spec §6.3)

use super::*;
use crate::generation;
use crate::ids::REPORT;
use crate::model::{BrandContext, ModelInfo, Report, Source};
use crate::workflow::WorkflowState;
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    source_ids: Vec<String>,
    #[serde(default)]
    brand_context: Option<BrandContext>,
    #[serde(default)]
    campaign_goal: Option<String>,
    #[serde(default)]
    requested_channels: Vec<String>,
}

pub(super) async fn load_extracted_sources(state: &AppState, scope: &Scope, ids: &[String]) -> ApiResult<Vec<Source>> {
    let mut out = vec![];
    for id in ids {
        let s = state.store.get_source(scope, id).await?.ok_or_else(|| ApiError::not_found("source", id))?;
        if s.extraction_status != "success" || s.normalized_content.as_deref().unwrap_or("").trim().is_empty() {
            return Err(ApiError::invalid_state(format!("source {id} has not been extracted successfully (status: {})", s.extraction_status))
                .with_details(json!({ "source_id": id, "extraction_status": s.extraction_status })));
        }
        if !out.iter().any(|x: &Source| x.id == s.id) {
            out.push(s);
        }
    }
    Ok(out)
}

/// Shared by `generate_content_report` and `repurpose_content` (source → report → variants).
pub(super) async fn create_report(
    state: &AppState,
    call: &Call<'_>,
    source_ids: &[String],
    brand_override: Option<BrandContext>,
    goal: Option<String>,
    channels: Vec<String>,
) -> ApiResult<(Report, ModelInfo)> {
    require_len("source_ids", source_ids, 1, 10)?;
    let sources = load_extracted_sources(state, &call.scope, source_ids).await?;
    let job_id = sources[0].job_id.clone();
    let job = state.store.get_job(&call.scope, &job_id).await?.ok_or_else(|| ApiError::not_found("job", &job_id))?;
    let brand = brand_override.unwrap_or_else(|| job.brand_context.clone());

    state.rate_limit_provider("llm")?;
    let combined: String = sources.iter().filter_map(|s| s.normalized_content.as_deref()).collect::<Vec<_>>().join("\n\n");
    // Relevance is advisory: a failure here must not block the report.
    let relevance = generation::classify_relevance(&state.llm, &combined, &brand).await.ok();

    let (mut payload, mut info) = generation::generate_report(&state.llm, &sources, &brand, goal.as_deref(), &channels).await?;
    if let Some((r, ri)) = relevance {
        if !r.relevant {
            payload.risks.push(format!(
                "Content may not be relevant to this brand's audience ({:.0}% relevance){}",
                r.score * 100.0,
                if r.reason.is_empty() { String::new() } else { format!(": {}", r.reason) }
            ));
        }
        info.input_tokens += ri.input_tokens;
        info.output_tokens += ri.output_tokens;
        info.cost_usd += ri.cost_usd;
    }
    if sources.iter().any(|s| s.metadata.get("duplicate_of").is_some_and(|d| !d.is_null())) {
        payload.risks.push("One or more sources duplicate previously ingested content".into());
    }

    let report = Report {
        id: new_id(REPORT),
        job_id: job_id.clone(),
        tenant_id: call.scope.tenant_id.clone(),
        brand_id: call.scope.brand_id.clone(),
        source_ids: sources.iter().map(|s| s.id.clone()).collect(),
        payload,
        brand_context: brand,
        campaign_goal: goal,
        model: info.clone(),
        created_at: chrono::Utc::now(),
    };
    state.store.insert_report(report.clone()).await?;
    state.sync_job(&call.who, &job_id, WorkflowState::ReportReady, "content report generated").await;
    Ok((report, info))
}

pub async fn generate_content_report(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    let channels = resolve_channels(call.ctx, &input.requested_channels)?;
    let (report, info) = create_report(state, call, &input.source_ids, input.brand_context, input.campaign_goal, channels).await?;
    let p = &report.payload;
    ToolOutput::new(&json!({
        "report_id": report.id,
        "source_ids": report.source_ids,
        "key_points": p.key_points,
        "audience_takeaways": p.audience_takeaways,
        "recommended_angle": p.recommended_angle,
        "content_pillars": p.content_pillars,
        "recommended_channels": p.recommended_channels,
        "risks": p.risks,
        "source_links": p.source_links,
        "confidence": p.confidence,
        "model": { "provider": info.provider, "model": info.model, "prompt_version": info.prompt_version },
    }))
    .map(|o| o.with_usage(&info))
}
