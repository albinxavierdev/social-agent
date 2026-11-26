//! `ingest_url` (spec §6.1)

use super::*;
use crate::ids::{JOB, SOURCE};
use crate::model::{BrandContext, Job, Source};
use crate::queue::{NewTask, KIND_EXTRACT_SOURCE};
use crate::urls::{detect_source_type, parse_and_normalize};
use crate::workflow::WorkflowState;
use chrono::Utc;
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    urls: Vec<String>,
    #[serde(default = "auto")]
    content_type: String,
    #[serde(default)]
    requested_channels: Vec<String>,
    #[serde(default)]
    brand_context: BrandContext,
    /// Ask for a reviewer screenshot (forces the Obscura extractor).
    #[serde(default)]
    want_screenshot: bool,
    #[serde(default)]
    allow_duplicates: bool,
}

fn auto() -> String {
    "auto".into()
}

const MAX_URLS: usize = 20;

fn sources_json(sources: &[Source]) -> Vec<Value> {
    sources
        .iter()
        .map(|s| {
            let st = crate::urls::SourceType::parse(&s.source_type);
            json!({ "source_id": s.id, "url": s.url, "source_type": s.source_type, "extraction_strategy": st.map(|t| t.extraction_strategy()).unwrap_or("http_or_obscura") })
        })
        .collect()
}

pub async fn ingest_url(state: &AppState, call: &Call<'_>, input: Value) -> ApiResult<ToolOutput> {
    let input: Input = parse_input(input)?;
    require_len("urls", &input.urls, 1, MAX_URLS)?;
    let channels = resolve_channels(call.ctx, &input.requested_channels)?;

    // Validate + normalise every URL up front: one bad URL rejects the request before any state exists.
    let mut urls = vec![];
    for raw in &input.urls {
        let u = parse_and_normalize(raw, &state.url_policy)?;
        if !urls.iter().any(|x: &url::Url| x == &u) {
            urls.push(u);
        }
    }

    let now = Utc::now();
    let types: Vec<_> = urls.iter().map(|u| detect_source_type(u, &input.content_type)).collect();
    let uniform = types.windows(2).all(|w| w[0] == w[1]);
    let job = Job {
        id: new_id(JOB),
        tenant_id: call.scope.tenant_id.clone(),
        brand_id: call.scope.brand_id.clone(),
        actor_id: call.ctx.actor_id.clone(),
        state: WorkflowState::Created,
        source_type: Some(if uniform { types[0].as_str().into() } else { "mixed".to_string() }),
        requested_channels: channels,
        idempotency_key: call.ctx.idempotency_key.clone(),
        brand_context: input.brand_context,
        trace_id: call.trace_id.clone(),
        created_at: now,
        updated_at: now,
        completed_at: None,
        error: None,
    };
    let (mut job, created) = state.store.create_job(job).await?;
    if !created {
        // Idempotent replay: same key → same job, nothing new is queued.
        let sources = state.store.list_sources(&call.scope, &job.id).await?;
        return ToolOutput::new(&json!({ "job_id": job.id, "status": job.state, "sources": sources_json(&sources) }));
    }

    // Duplicate-source detection against already-extracted sources for this brand.
    if !input.allow_duplicates {
        for u in &urls {
            if let Some(existing) = state.store.find_duplicate_source(&call.scope, u.as_str(), None, None).await? {
                let err = ApiError::new(ErrorCode::DuplicateSource, "this URL has already been ingested for this brand")
                    .with_details(json!({ "url": u.as_str(), "existing_source_id": existing.id, "existing_job_id": existing.job_id }));
                state.transition_job(&call.who, &mut job, WorkflowState::Failed, "duplicate source").await?;
                return Err(err);
            }
        }
    }

    let mut sources = vec![];
    for (u, st) in urls.iter().zip(types.iter()) {
        let src = Source {
            id: new_id(SOURCE),
            job_id: job.id.clone(),
            tenant_id: job.tenant_id.clone(),
            brand_id: job.brand_id.clone(),
            url: u.to_string(),
            source_type: st.as_str().into(),
            extraction_method: None,
            extraction_status: "pending".into(),
            title: None,
            author: None,
            published_at: None,
            language: None,
            raw_content_url: None,
            normalized_content: None,
            summary: None,
            content_type: None,
            metadata: json!({}),
            screenshot_url: None,
            content_hash: None,
            error: None,
            created_at: now,
        };
        state.store.insert_source(src.clone()).await?;
        sources.push(src);
    }

    state.transition_job(&call.who, &mut job, WorkflowState::Queued, "ingestion queued").await?;
    for s in &sources {
        state
            .queue
            .enqueue(NewTask::new(
                KIND_EXTRACT_SOURCE,
                &call.scope,
                Some(job.id.clone()),
                json!({ "source_id": s.id, "want_screenshot": input.want_screenshot }),
                &call.ctx.request_id,
            ))
            .await?;
    }
    ToolOutput::new(&json!({ "job_id": job.id, "status": job.state, "sources": sources_json(&sources) }))
}
