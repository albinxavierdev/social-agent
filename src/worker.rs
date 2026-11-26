//! Queue worker (spec §9): leases tasks, runs idempotent handlers, retries with exponential backoff and
//! jitter, dead-letters permanent failures, honours cancellation, and drains in-flight work on shutdown.

use crate::app::{Actor, AppState};
use crate::context::RequestContext;
use crate::error::ApiError;
use crate::queue::{backoff_delay, FailOutcome, Task, KIND_EXTRACT_SOURCE, KIND_SUBMIT_ZERNIO};
use crate::workflow::WorkflowState;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const BACKOFF_BASE: Duration = Duration::from_secs(2);
const BACKOFF_CAP: Duration = Duration::from_secs(300);
const IDLE_POLL: Duration = Duration::from_millis(500);

pub async fn run(state: AppState, shutdown: CancellationToken) {
    let concurrency = state.cfg.worker_concurrency.max(1);
    tracing::info!(concurrency, "worker started");
    let mut set = JoinSet::new();
    for n in 0..concurrency {
        let (st, sd) = (state.clone(), shutdown.clone());
        set.spawn(async move { worker_loop(st, sd, n).await });
    }
    // Queue-depth gauges for dashboards/alerts.
    let (st, sd) = (state.clone(), shutdown.clone());
    set.spawn(async move {
        while !sd.is_cancelled() {
            if let Ok(s) = st.queue.stats().await {
                metrics::gauge!("queue_depth").set(s.pending as f64);
                metrics::gauge!("queue_oldest_pending_age_seconds").set(s.oldest_pending_age_secs as f64);
                metrics::gauge!("queue_dead_letters").set(s.dead as f64);
            }
            tokio::select! { _ = sd.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
        }
    });
    if let Some(days) = state.cfg.retention_source_days {
        let (st, sd) = (state.clone(), shutdown.clone());
        set.spawn(async move {
            while !sd.is_cancelled() {
                let cutoff = chrono::Utc::now() - chrono::Duration::days(days as i64);
                match st.store.purge_source_content(cutoff).await {
                    Ok(n) if n > 0 => tracing::info!(purged = n, retention_days = days, "retention: purged extracted source content"),
                    Ok(_) => {}
                    Err(e) => tracing::error!(error = %e.message, "retention purge failed"),
                }
                tokio::select! { _ = sd.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(3600)) => {} }
            }
        });
    }
    while set.join_next().await.is_some() {}
    tracing::info!("worker drained and stopped");
}

async fn worker_loop(state: AppState, shutdown: CancellationToken, n: usize) {
    let lease_for = state.cfg.tool_timeout + Duration::from_secs(30);
    while !shutdown.is_cancelled() {
        match state.queue.lease(lease_for).await {
            Ok(Some(task)) => {
                // A leased task always runs to completion: shutdown only stops *new* leases (job draining).
                process(&state, task).await;
            }
            Ok(None) => {
                tokio::select! { _ = shutdown.cancelled() => break, _ = tokio::time::sleep(IDLE_POLL) => {} }
            }
            Err(e) => {
                tracing::error!(worker = n, error = %e.message, "lease failed");
                tokio::select! { _ = shutdown.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
            }
        }
    }
}

/// Process one leased task. Public so tests can drive the worker deterministically.
pub async fn process(state: &AppState, task: Task) {
    let started = std::time::Instant::now();
    let who = Actor::system(task.scope(), &task.correlation_id, &task.trace_id);
    let result = tokio::time::timeout(state.cfg.tool_timeout, handle(state, &task, &who))
        .await
        .unwrap_or_else(|_| Err(ApiError::new(crate::error::ErrorCode::InternalError, "task timed out").retryable(true)));
    metrics::histogram!("queue_task_duration_seconds", "kind" => task.kind.clone()).record(started.elapsed().as_secs_f64());

    match result {
        Ok(()) => {
            metrics::counter!("queue_tasks_total", "kind" => task.kind.clone(), "outcome" => "success").increment(1);
            if let Err(e) = state.queue.complete(&task.id).await {
                tracing::error!(task_id = %task.id, error = %e.message, "failed to complete task");
            }
        }
        Err(e) => {
            // A cancelled/failed job is not an error: the work is simply no longer wanted.
            if e.code == crate::error::ErrorCode::InvalidState && !e.retryable && task.kind == KIND_EXTRACT_SOURCE {
                tracing::info!(task_id = %task.id, "task discarded: {}", e.message);
                let _ = state.queue.complete(&task.id).await;
                return;
            }
            let delay = backoff_delay(task.attempts.max(1) as u32, BACKOFF_BASE, BACKOFF_CAP);
            match state.queue.fail(&task.id, &e.message, e.retryable, delay).await {
                Ok(FailOutcome::Retrying { run_after, attempt }) => {
                    metrics::counter!("queue_retries_total", "kind" => task.kind.clone()).increment(1);
                    tracing::warn!(task_id = %task.id, attempt, %run_after, error = %e.message, "task failed; will retry");
                }
                Ok(FailOutcome::DeadLettered) => {
                    metrics::counter!("queue_tasks_total", "kind" => task.kind.clone(), "outcome" => "dead_letter").increment(1);
                    tracing::error!(task_id = %task.id, tenant_id = %task.tenant_id, job_id = ?task.job_id, error = %e.message, "task dead-lettered");
                    if let (KIND_EXTRACT_SOURCE, Some(job_id)) = (task.kind.as_str(), task.job_id.as_deref()) {
                        crate::pipeline::fail_job_if_all_sources_failed(state, &who, job_id).await;
                    }
                }
                Err(qe) => tracing::error!(task_id = %task.id, error = %qe.message, "failed to record task failure"),
            }
        }
    }
}

async fn handle(state: &AppState, task: &Task, who: &Actor) -> Result<(), ApiError> {
    match task.kind.as_str() {
        KIND_EXTRACT_SOURCE => {
            let source_id = task.payload.get("source_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::validation("task is missing source_id"))?;
            let shot = task.payload.get("want_screenshot").and_then(|v| v.as_bool()).unwrap_or(false);
            crate::pipeline::extract_source(state, who, source_id, shot).await.map(|_| ())
        }
        KIND_SUBMIT_ZERNIO => {
            let id =
                task.payload.get("publish_request_id").and_then(|v| v.as_str()).ok_or_else(|| ApiError::validation("task is missing publish_request_id"))?;
            let ctx: RequestContext = serde_json::from_value(task.payload.get("context").cloned().unwrap_or_default())
                .map_err(|e| ApiError::validation(format!("bad context: {e}")))?;
            if ctx.tenant_id != task.tenant_id || ctx.brand_id != task.brand_id {
                return Err(ApiError::forbidden("task context does not match its tenant/brand"));
            }
            let who = Actor { scope: ctx.scope(), actor_id: ctx.actor_id.clone(), correlation_id: ctx.request_id.clone(), trace_id: task.trace_id.clone() };
            crate::tools::submit_publish_request(state, &ctx, &who, id).await.map(|_| ())
        }
        other => Err(ApiError::validation(format!("unknown task kind '{other}'")).retryable(false)),
    }
}

/// Used by tests/diagnostics: is the job still wanted?
pub fn job_is_active(s: WorkflowState) -> bool {
    !matches!(s, WorkflowState::Cancelled | WorkflowState::Failed)
}
