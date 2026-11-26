//! Persistence boundary. Every method is scoped by (tenant, brand): there is no
//! way to read or write a row without naming both (tenant & brand isolation).

pub mod memory;
pub mod postgres;

use crate::context::Scope;
use crate::error::ApiResult;
use crate::model::*;
use crate::workflow::{Transition, WorkflowState};
use async_trait::async_trait;

#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Idempotent create keyed by (tenant, brand, idempotency_key).
    /// Returns `(job, created)`; `created == false` means an existing job was returned.
    async fn create_job(&self, job: Job) -> ApiResult<(Job, bool)>;
    async fn get_job(&self, s: &Scope, id: &str) -> ApiResult<Option<Job>>;
    /// Optimistic update: fails with INVALID_STATE if the stored state != `expected`.
    async fn save_job(&self, job: &Job, expected: WorkflowState) -> ApiResult<()>;

    async fn insert_source(&self, src: Source) -> ApiResult<()>;
    async fn get_source(&self, s: &Scope, id: &str) -> ApiResult<Option<Source>>;
    async fn save_source(&self, src: &Source) -> ApiResult<()>;
    async fn list_sources(&self, s: &Scope, job_id: &str) -> ApiResult<Vec<Source>>;
    /// Duplicate-source detection (same normalised URL, or same content hash).
    async fn find_duplicate_source(&self, s: &Scope, url: &str, content_hash: Option<&str>, exclude_id: Option<&str>) -> ApiResult<Option<Source>>;

    async fn insert_report(&self, r: Report) -> ApiResult<()>;
    async fn get_report(&self, s: &Scope, id: &str) -> ApiResult<Option<Report>>;

    async fn insert_draft(&self, d: Draft) -> ApiResult<()>;
    async fn get_draft(&self, s: &Scope, id: &str) -> ApiResult<Option<Draft>>;
    async fn save_draft(&self, d: &Draft, expected: WorkflowState) -> ApiResult<()>;
    /// Most recent drafts for a brand (for duplicate-content checks).
    async fn list_brand_drafts(&self, s: &Scope, limit: usize) -> ApiResult<Vec<Draft>>;

    async fn insert_review_event(&self, e: ReviewEvent) -> ApiResult<()>;
    async fn list_review_events(&self, s: &Scope, draft_id: &str) -> ApiResult<Vec<ReviewEvent>>;

    async fn insert_publish_request(&self, p: PublishRequest) -> ApiResult<()>;
    async fn get_publish_request(&self, s: &Scope, id: &str) -> ApiResult<Option<PublishRequest>>;
    async fn save_publish_request(&self, p: &PublishRequest) -> ApiResult<()>;
    async fn list_publish_requests(&self, s: &Scope, draft_id: &str) -> ApiResult<Vec<PublishRequest>>;

    async fn record_tool_invocation(&self, i: ToolInvocation) -> ApiResult<()>;
    async fn record_transition(&self, t: Transition) -> ApiResult<()>;
    async fn list_transitions(&self, s: &Scope, entity_id: &str) -> ApiResult<Vec<Transition>>;

    /// Retention (spec §13): strip extracted text/raw content/screenshot references from sources created
    /// before `cutoff`. Returns the number of sources purged. Source rows and audit data are kept.
    async fn purge_source_content(&self, cutoff: chrono::DateTime<chrono::Utc>) -> ApiResult<u64>;

    async fn ping(&self) -> ApiResult<()>;
}
