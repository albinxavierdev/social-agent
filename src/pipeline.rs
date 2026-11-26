//! Extraction pipeline shared by the `extract_content` tool and the queue worker.

use crate::app::{Actor, AppState};
use crate::error::{ApiError, ErrorCode};
use crate::extract::ExtractionRequest;
use crate::model::Source;
use crate::urls::SourceType;
use crate::workflow::WorkflowState;
use serde_json::json;
use sha2::{Digest, Sha256};

pub fn content_hash(text: &str) -> String {
    let norm: String = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    hex::encode(Sha256::digest(norm.as_bytes()))
}

/// Extract one source end-to-end: job bookkeeping, extractor chain, persistence, duplicate marking.
pub async fn extract_source(state: &AppState, who: &Actor, source_id: &str, want_screenshot: bool) -> Result<Source, ApiError> {
    let mut src = state.store.get_source(&who.scope, source_id).await?.ok_or_else(|| ApiError::not_found("source", source_id))?;
    let mut job = state.store.get_job(&who.scope, &src.job_id).await?.ok_or_else(|| ApiError::not_found("job", &src.job_id))?;

    if matches!(job.state, WorkflowState::Cancelled | WorkflowState::Failed) {
        // Say WHY: the source's own failure reason is almost always what the caller needs to see.
        let reason =
            src.error.as_ref().and_then(|e| e.get("message")).and_then(|m| m.as_str()).map(|m| format!(" — extraction failed: {m}")).unwrap_or_default();
        return Err(ApiError::invalid_state(format!("job is {}{reason}", job.state.as_str()))
            .with_details(json!({ "job_id": job.id, "job_status": job.state, "source_error": src.error }))
            .retryable(false));
    }
    if matches!(job.state, WorkflowState::Created | WorkflowState::Queued) {
        state.transition_job(who, &mut job, WorkflowState::Extracting, "extraction started").await.ok();
    }

    let url = url::Url::parse(&src.url).map_err(|e| ApiError::validation(format!("stored url is invalid: {e}")))?;
    let source_type = SourceType::parse(&src.source_type).unwrap_or(SourceType::WebArticle);
    let req =
        ExtractionRequest { source_id: src.id.clone(), tenant_id: src.tenant_id.clone(), brand_id: src.brand_id.clone(), url, source_type, want_screenshot };

    match state.extractors.extract(&req).await {
        Ok(c) => {
            let hash = content_hash(&c.text);
            let dup = state.store.find_duplicate_source(&who.scope, &src.url, Some(&hash), Some(&src.id)).await?;
            let mut meta = serde_json::to_value(&c.metadata).unwrap_or_else(|_| json!({}));
            if let Some(d) = &dup {
                meta["duplicate_of"] = json!(d.id);
            }
            src.extraction_method = Some(c.extraction_method);
            src.extraction_status = "success".into();
            src.title = c.title;
            src.author = c.author;
            src.published_at = c.published_at;
            src.language = Some(c.language);
            src.normalized_content = Some(c.text);
            src.summary = Some(c.summary);
            src.content_type = Some(c.content_type);
            src.metadata = meta;
            src.screenshot_url = c.screenshot_url;
            src.content_hash = Some(hash);
            src.error = None;
            state.store.save_source(&src).await?;
            Ok(src)
        }
        Err(e) => {
            src.extraction_status = "failed".into();
            src.error = Some(json!({ "code": e.code.as_str(), "message": e.message, "retryable": e.retryable }));
            state.store.save_source(&src).await?;
            Err(e)
        }
    }
}

/// After a permanent source failure, fail the job if no source in it can succeed any more.
pub async fn fail_job_if_all_sources_failed(state: &AppState, who: &Actor, job_id: &str) {
    let Ok(sources) = state.store.list_sources(&who.scope, job_id).await else { return };
    if !sources.is_empty() && sources.iter().all(|s| s.extraction_status == "failed") {
        if let Ok(Some(mut job)) = state.store.get_job(&who.scope, job_id).await {
            if !job.state.is_terminal() && job.state.can_transition_to(WorkflowState::Failed) {
                job.error = Some(json!({ "code": ErrorCode::ExtractionFailed.as_str(), "message": "all sources failed extraction" }));
                let _ = state.transition_job(who, &mut job, WorkflowState::Failed, "all sources failed extraction").await;
            }
        }
    }
}

pub fn source_to_contract(s: &Source) -> serde_json::Value {
    let meta = &s.metadata;
    json!({
        "source_id": s.id,
        "url": s.url,
        "title": s.title,
        "author": s.author,
        "published_at": s.published_at,
        "content_type": s.content_type,
        "language": s.language,
        "text": s.normalized_content,
        "summary": s.summary,
        "metadata": {
            "og_title": meta.get("og_title"),
            "og_description": meta.get("og_description"),
            "canonical_url": meta.get("canonical_url"),
            "images": meta.get("images").cloned().unwrap_or_else(|| json!([])),
            "duplicate_of": meta.get("duplicate_of"),
        },
        "screenshot_url": s.screenshot_url,
        "extraction_method": s.extraction_method,
        "extraction_status": s.extraction_status,
    })
}
