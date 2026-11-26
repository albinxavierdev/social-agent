//! FireCrawl managed extraction (https://docs.firecrawl.dev) — fallback / large-scale path.

use super::html::lead_summary;
use super::{ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct FirecrawlExtractor {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl FirecrawlExtractor {
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            http: reqwest::Client::builder().timeout(timeout).build()?,
        })
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned)
}

#[async_trait]
impl ContentExtractor for FirecrawlExtractor {
    fn name(&self) -> &'static str {
        "firecrawl"
    }

    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        let resp = self
            .http
            .post(format!("{}/v1/scrape", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&json!({ "url": req.url.as_str(), "formats": ["markdown"], "onlyMainContent": true }))
            .send()
            .await
            .map_err(|e| ExtractionError::failed(format!("firecrawl request failed: {}", e.without_url()), true))?;
        let status = resp.status();
        if !status.is_success() {
            let retryable = status.as_u16() == 429 || status.is_server_error();
            return Err(ExtractionError::failed(format!("firecrawl returned {status}"), retryable));
        }
        let body: Value = resp.json().await.map_err(|e| ExtractionError::failed(format!("firecrawl bad json: {e}"), false))?;
        if body.get("success").and_then(Value::as_bool) == Some(false) {
            return Err(ExtractionError::failed("firecrawl reported failure", false));
        }
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        let text = s(&data, "markdown").ok_or_else(|| ExtractionError::failed("firecrawl returned no content", false))?;
        let meta = data.get("metadata").cloned().unwrap_or(Value::Null);
        let og_desc = s(&meta, "ogDescription").or_else(|| s(&meta, "description"));
        Ok(ExtractedContent {
            title: s(&meta, "ogTitle").or_else(|| s(&meta, "title")),
            author: s(&meta, "author"),
            published_at: s(&meta, "publishedTime")
                .or_else(|| s(&meta, "article:published_time"))
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(&d).ok())
                .map(|d| d.with_timezone(&chrono::Utc)),
            content_type: "article".into(),
            language: s(&meta, "language").map(|l| l.split(['-', '_']).next().unwrap_or("und").to_ascii_lowercase()).unwrap_or_else(|| "und".into()),
            summary: lead_summary(&text, og_desc.as_deref()),
            text,
            metadata: ExtractedMetadata {
                og_title: s(&meta, "ogTitle"),
                og_description: og_desc,
                canonical_url: s(&meta, "sourceURL"),
                images: s(&meta, "ogImage").into_iter().collect(),
                extra: json!({ "firecrawl_status": meta.get("statusCode") }),
            },
            screenshot_url: None,
            extraction_method: "firecrawl".into(),
            extraction_status: "success".into(),
        })
    }
}
