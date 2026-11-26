//! Domain entities (spec §12) and shared payload types.

use crate::workflow::WorkflowState;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrandContext {
    #[serde(default)]
    pub business_description: Option<String>,
    #[serde(default)]
    pub tone_of_voice: Option<String>,
    #[serde(default)]
    pub target_audience: Option<String>,
    #[serde(default)]
    pub content_rules: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub actor_id: String,
    pub state: WorkflowState,
    pub source_type: Option<String>,
    pub requested_channels: Vec<String>,
    pub idempotency_key: Option<String>,
    pub brand_context: BrandContext,
    pub trace_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub job_id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub url: String,
    pub source_type: String,
    pub extraction_method: Option<String>,
    /// pending | success | failed
    pub extraction_status: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub language: Option<String>,
    pub raw_content_url: Option<String>,
    pub normalized_content: Option<String>,
    pub summary: Option<String>,
    pub content_type: Option<String>,
    pub metadata: Value,
    pub screenshot_url: Option<String>,
    pub content_hash: Option<String>,
    pub error: Option<Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPayload {
    pub key_points: Vec<String>,
    pub audience_takeaways: Vec<String>,
    pub recommended_angle: String,
    pub content_pillars: Vec<String>,
    pub recommended_channels: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub source_links: Vec<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub id: String,
    pub job_id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub source_ids: Vec<String>,
    pub payload: ReportPayload,
    pub brand_context: BrandContext,
    pub campaign_goal: Option<String>,
    pub model: ModelInfo,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Media {
    #[serde(rename = "type")]
    pub kind: String, // "image"
    pub url: String,
    pub alt_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Variant {
    pub channel: String,
    #[serde(default = "default_format")]
    pub format: String, // single_image | carousel | thread | text | ...
    pub caption: String,
    #[serde(default)]
    pub hashtags: Vec<String>,
    #[serde(default)]
    pub cta: Option<String>,
    #[serde(default)]
    pub media: Vec<Media>,
    /// Extra parts for thread / carousel formats (caption is part 0).
    #[serde(default)]
    pub parts: Vec<String>,
}

fn default_format() -> String {
    "text".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DraftContent {
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Compliance {
    /// passed | warnings | failed | pending
    pub status: String,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub issues: Vec<Issue>,
}

impl Compliance {
    pub fn pending() -> Self {
        Self { status: "pending".into(), warnings: vec![], issues: vec![] }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    pub code: String,
    pub channel: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub job_id: String,
    pub report_id: String,
    pub campaign_id: Option<String>,
    pub tenant_id: String,
    pub brand_id: String,
    /// Single channel name, or "multi" when the draft carries several variants.
    pub channel: String,
    pub format: String,
    pub state: WorkflowState,
    pub content: DraftContent,
    pub media_assets: Vec<ImageAsset>,
    pub source_links: Vec<String>,
    pub compliance: Compliance,
    pub brand_context: BrandContext,
    pub model: ModelInfo,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageAsset {
    pub image_id: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: String,
    pub alt_text: String,
    pub source: String, // generated | search | source_page
    pub validation_status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approved,
    Rejected,
    RevisionRequested,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewEvent {
    pub id: String,
    pub draft_id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub reviewer_id: String,
    pub decision: Decision,
    pub edited_content: Option<Value>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishAction {
    Publish,
    Schedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStatus {
    ReadyForZernio,
    SubmittedToZernio,
    ScheduledOrPublished,
    PartialSuccess,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRequest {
    pub id: String,
    pub draft_id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub zernio_profile_id: String,
    pub channels: Vec<String>,
    pub action: PublishAction,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub zernio_request_payload: Value,
    pub zernio_response: Option<Value>,
    pub status: PublishStatus,
    /// Stable key sent to Zernio as `Idempotency-Key` so retries never double-post.
    pub zernio_idempotency_key: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub actor_id: String,
    pub tool_name: String,
    pub input_hash: String,
    pub status: String, // success | error
    pub latency_ms: u64,
    pub token_usage: Option<Value>,
    pub cost: Option<f64>,
    pub error: Option<Value>,
    pub trace_id: String,
    pub request_id: String,
    pub created_at: DateTime<Utc>,
}
