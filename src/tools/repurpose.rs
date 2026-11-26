//! `repurpose_content` (spec §6.5)

use super::draft::{check_post_type, persist_draft};
use super::report::{create_report, load_extracted_sources};
use super::*;
use crate::generation::{self, RepurposeInput, Target};
use crate::ids::CAMPAIGN;
use crate::model::{BrandContext, Report};
use serde::Deserialize;

#[derive(Deserialize)]
struct TargetIn {
    channel: String,
    format: String,
}

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    report_id: Option<String>,
    #[serde(default)]
    draft_id: Option<String>,
    targets: Vec<TargetIn>,
    #[serde(default)]
    brand_context: Option<BrandContext>,
}

pub async fn repurpose_content(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    require_len("targets", &input.targets, 1, 12)?;
    let given = [input.source_id.is_some(), input.report_id.is_some(), input.draft_id.is_some()].iter().filter(|b| **b).count();
    if given != 1 {
        return Err(ApiError::validation("provide exactly one of source_id, report_id, draft_id"));
    }
    let mut targets = vec![];
    for t in &input.targets {
        check_post_type_or_format(&t.format)?;
        let ch = resolve_channels(call.ctx, std::slice::from_ref(&t.channel))?.remove(0);
        targets.push(Target { channel: ch, format: t.format.clone() });
    }
    state.rate_limit_provider("llm")?;

    let mut total = crate::model::ModelInfo {
        provider: String::new(),
        model: String::new(),
        prompt_version: String::new(),
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
        latency_ms: 0,
    };
    let (report, brand, variants, info): (Report, BrandContext, Vec<crate::model::Variant>, crate::model::ModelInfo) = if let Some(sid) = &input.source_id {
        let sources = load_extracted_sources(state, &call.scope, std::slice::from_ref(sid)).await?;
        let channels: Vec<String> = targets.iter().map(|t| t.channel.clone()).collect();
        let (report, rinfo) = create_report(state, call, std::slice::from_ref(sid), input.brand_context.clone(), None, channels).await?;
        total.input_tokens += rinfo.input_tokens;
        total.output_tokens += rinfo.output_tokens;
        total.cost_usd += rinfo.cost_usd;
        let brand = report.brand_context.clone();
        let text = sources[0].normalized_content.clone().unwrap_or_default();
        let (v, info) = generation::repurpose(&state.llm, RepurposeInput::Text { title: sources[0].title.as_deref(), text: &text }, &targets, &brand).await?;
        (report, brand, v, info)
    } else if let Some(rid) = &input.report_id {
        let report = state.store.get_report(&call.scope, rid).await?.ok_or_else(|| ApiError::not_found("report", rid))?;
        let brand = input.brand_context.clone().unwrap_or_else(|| report.brand_context.clone());
        let (v, info) = generation::repurpose(&state.llm, RepurposeInput::Report(&report.payload), &targets, &brand).await?;
        (report, brand, v, info)
    } else {
        let did = input.draft_id.as_ref().expect("exactly one provided");
        let draft = state.store.get_draft(&call.scope, did).await?.ok_or_else(|| ApiError::not_found("draft", did))?;
        let report = state.store.get_report(&call.scope, &draft.report_id).await?.ok_or_else(|| ApiError::not_found("report", &draft.report_id))?;
        let brand = input.brand_context.clone().unwrap_or_else(|| draft.brand_context.clone());
        let (v, info) = generation::repurpose(&state.llm, RepurposeInput::Draft(&draft), &targets, &brand).await?;
        (report, brand, v, info)
    };

    // One draft per requested (channel, format) so each can be reviewed and scheduled independently.
    let campaign_id = new_id(CAMPAIGN);
    let mut out = vec![];
    for v in variants {
        let (channel, format) = (v.channel.clone(), v.format.clone());
        let d = persist_draft(state, call, &report, vec![v], Some(campaign_id.clone()), &info, &brand).await?;
        out.push(json!({ "channel": channel, "format": format, "draft_id": d.id }));
    }
    total.provider = info.provider.clone();
    total.model = info.model.clone();
    total.input_tokens += info.input_tokens;
    total.output_tokens += info.output_tokens;
    total.cost_usd += info.cost_usd;
    ToolOutput::new(&json!({ "campaign_id": campaign_id, "variants": out })).map(|o| o.with_usage(&total))
}

fn check_post_type_or_format(f: &str) -> ApiResult<()> {
    // Repurpose formats are a superset of post types (e.g. "ad_copy", "linkedin_post").
    if f.is_empty() || f.len() > 40 || !f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(ApiError::validation("format must be 1..=40 chars of [a-z0-9_-]"));
    }
    let _ = check_post_type;
    Ok(())
}
