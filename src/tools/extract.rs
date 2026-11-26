//! `extract_content` (spec §6.2)

use super::*;
use crate::pipeline::{extract_source, fail_job_if_all_sources_failed, source_to_contract};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    source_id: String,
    /// Re-extract even if a successful extraction already exists.
    #[serde(default)]
    force: bool,
    #[serde(default)]
    want_screenshot: bool,
}

pub async fn extract_content(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    let existing = state.store.get_source(&call.scope, &input.source_id).await?.ok_or_else(|| ApiError::not_found("source", &input.source_id))?;
    if existing.extraction_status == "success" && !input.force && !input.want_screenshot {
        return ToolOutput::new(&source_to_contract(&existing));
    }
    match extract_source(state, &call.who, &input.source_id, input.want_screenshot).await {
        Ok(src) => ToolOutput::new(&source_to_contract(&src)),
        Err(e) => {
            if !e.retryable {
                fail_job_if_all_sources_failed(state, &call.who, &existing.job_id).await;
            }
            Err(e)
        }
    }
}
