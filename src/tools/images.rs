//! `find_images` (spec §6.6)

use super::*;
use crate::generation;
use crate::images::ImageQuery;
use crate::model::Media;
use crate::workflow::WorkflowState;
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    draft_id: String,
    #[serde(default = "two")]
    count: usize,
    /// Reuse images found on the source pages. Rights are not verified, so this is opt-in.
    #[serde(default)]
    allow_source_images: bool,
}

fn two() -> usize {
    2
}

pub async fn find_images(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    if !(1..=4).contains(&input.count) {
        return Err(ApiError::validation("count must be between 1 and 4"));
    }
    let mut draft = state.store.get_draft(&call.scope, &input.draft_id).await?.ok_or_else(|| ApiError::not_found("draft", &input.draft_id))?;
    if !matches!(draft.state, WorkflowState::DraftReady | WorkflowState::RevisionRequested) {
        return Err(ApiError::invalid_state(format!(
            "images can only be changed while the draft is draft_ready or revision_requested (is {})",
            draft.state.as_str()
        )));
    }
    let Some(first) = draft.content.variants.first().cloned() else {
        return Err(ApiError::validation("draft has no variants"));
    };
    let channels: Vec<String> = draft.content.variants.iter().map(|v| v.channel.clone()).collect();

    state.rate_limit_provider("llm")?;
    let ip = generation::image_prompt(&state.llm, &first, &draft.brand_context).await?;

    // Candidate images already discovered on the report's sources (used only when explicitly allowed).
    let mut source_images = vec![];
    if input.allow_source_images {
        if let Some(report) = state.store.get_report(&call.scope, &draft.report_id).await? {
            for sid in &report.source_ids {
                if let Some(s) = state.store.get_source(&call.scope, sid).await? {
                    source_images
                        .extend(s.metadata.get("images").and_then(Value::as_array).into_iter().flatten().filter_map(|u| u.as_str().map(str::to_owned)));
                }
            }
        }
    }
    let q = ImageQuery {
        prompt: ip.prompt,
        alt_text: ip.alt_text,
        source_image_urls: source_images,
        allow_source_images: input.allow_source_images,
        count: input.count,
    };
    let (assets, _rejected) = state.images.find(&call.scope, &draft.id, &q, &channels, input.count).await?;

    // Attach the best asset to every variant (respecting each channel's image limit); keep all assets on the draft.
    let best = assets[0].clone();
    for v in &mut draft.content.variants {
        let max = crate::channels::spec(&v.channel).map(|s| s.max_images).unwrap_or(1);
        v.media = std::iter::once(&best)
            .chain(assets.iter().skip(1))
            .take(max.min(if v.format == "carousel" { input.count } else { 1 }))
            .map(|a| Media { kind: "image".into(), url: a.url.clone(), alt_text: a.alt_text.clone() })
            .collect();
    }
    draft.media_assets = assets.clone();
    draft.updated_at = chrono::Utc::now();
    let prior = draft.state;
    draft.compliance = super::draft::preview_compliance(state, &call.scope, &draft).await?;
    state.store.save_draft(&draft, prior).await?;

    ToolOutput::new(&json!({ "draft_id": draft.id, "images": assets }))
}
