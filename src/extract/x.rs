//! X (Twitter) post extraction through the official API v2 (bearer token). The spec also allows an
//! approved provider or a Zernio-connected X account; those can be added as further `ContentExtractor`s.

use super::html::lead_summary;
use super::{ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest};
use crate::urls::x_post_id;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct XExtractor {
    base_url: String,
    bearer: Option<String>,
    http: reqwest::Client,
}

impl XExtractor {
    pub fn new(base_url: &str, bearer: Option<String>, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self { base_url: base_url.trim_end_matches('/').to_string(), bearer, http: reqwest::Client::builder().timeout(timeout).build()? })
    }
}

#[async_trait]
impl ContentExtractor for XExtractor {
    fn name(&self) -> &'static str {
        "x"
    }

    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        let token = self.bearer.as_deref().ok_or(ExtractionError::NotConfigured("X_BEARER_TOKEN is not set"))?;
        let id = x_post_id(&req.url).ok_or_else(|| ExtractionError::Unsupported("not an X post URL".into()))?;
        let resp = self
            .http
            .get(format!("{}/2/tweets/{id}", self.base_url))
            .bearer_auth(token)
            .query(&[
                ("tweet.fields", "created_at,lang,author_id,entities,attachments,conversation_id"),
                ("expansions", "author_id,attachments.media_keys"),
                ("user.fields", "name,username"),
                ("media.fields", "url,preview_image_url,type"),
            ])
            .send()
            .await
            .map_err(|e| ExtractionError::failed(format!("x request failed: {}", e.without_url()), true))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                429 => ExtractionError::failed("x rate limited", true),
                401 | 403 => ExtractionError::failed("x access denied", false),
                404 => ExtractionError::failed("x post not found", false),
                _ => ExtractionError::failed(format!("x returned {status}"), status.is_server_error()),
            });
        }
        let body: Value = resp.json().await.map_err(|e| ExtractionError::failed(format!("x bad json: {e}"), false))?;
        let data = body.get("data").ok_or_else(|| ExtractionError::failed("x returned no post (deleted or protected)", false))?;
        let text = data.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
        if text.is_empty() {
            return Err(ExtractionError::failed("x post has no text", false));
        }
        let user = body.pointer("/includes/users/0");
        let username = user.and_then(|u| u.get("username")).and_then(Value::as_str);
        let images: Vec<String> = body
            .pointer("/includes/media")
            .and_then(Value::as_array)
            .map(|m| m.iter().filter_map(|x| x.get("url").or_else(|| x.get("preview_image_url")).and_then(Value::as_str).map(str::to_owned)).collect())
            .unwrap_or_default();
        Ok(ExtractedContent {
            title: username.map(|u| format!("Post by @{u}")),
            author: user.and_then(|u| u.get("name")).and_then(Value::as_str).map(str::to_owned),
            published_at: data
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                .map(|d| d.with_timezone(&chrono::Utc)),
            content_type: "x_post".into(),
            language: data.get("lang").and_then(Value::as_str).unwrap_or("und").to_string(),
            summary: lead_summary(&text, None),
            text,
            metadata: ExtractedMetadata {
                canonical_url: Some(req.url.to_string()),
                images,
                extra: json!({ "post_id": id, "conversation_id": data.get("conversation_id"), "username": username }),
                ..Default::default()
            },
            screenshot_url: None,
            extraction_method: "x_api".into(),
            extraction_status: "success".into(),
        })
    }
}
