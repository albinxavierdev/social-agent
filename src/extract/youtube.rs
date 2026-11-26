//! YouTube extraction via the YouTube Data API v3 (metadata, description, tags, thumbnails).
//! Transcripts are NOT available through the Data API without owner OAuth; for transcript-level
//! understanding use a Vertex AI (Gemini) video adapter or a Zernio-connected YouTube account.

use super::html::lead_summary;
use super::{ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest};
use crate::urls::youtube_video_id;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct YouTubeExtractor {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl YouTubeExtractor {
    pub fn new(base_url: &str, api_key: Option<String>, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self { base_url: base_url.trim_end_matches('/').to_string(), api_key, http: reqwest::Client::builder().timeout(timeout).build()? })
    }
}

#[async_trait]
impl ContentExtractor for YouTubeExtractor {
    fn name(&self) -> &'static str {
        "youtube"
    }

    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        let key = self.api_key.as_deref().ok_or(ExtractionError::NotConfigured("YOUTUBE_API_KEY is not set"))?;
        let id = youtube_video_id(&req.url).ok_or_else(|| ExtractionError::Unsupported("not a YouTube video URL".into()))?;
        let resp = self
            .http
            .get(format!("{}/youtube/v3/videos", self.base_url))
            .query(&[("part", "snippet,contentDetails,statistics"), ("id", id.as_str()), ("key", key)])
            .send()
            .await
            .map_err(|e| ExtractionError::failed(format!("youtube request failed: {}", e.without_url()), true))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                403 | 429 => ExtractionError::failed("youtube quota exceeded or access denied", status.as_u16() == 429),
                _ => ExtractionError::failed(format!("youtube returned {status}"), status.is_server_error()),
            });
        }
        let body: Value = resp.json().await.map_err(|e| ExtractionError::failed(format!("youtube bad json: {e}"), false))?;
        let item = body.pointer("/items/0").ok_or_else(|| ExtractionError::failed("video not found or private", false))?;
        let sn = item.get("snippet").cloned().unwrap_or(Value::Null);
        let title = sn.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
        let desc = sn.get("description").and_then(Value::as_str).unwrap_or_default().to_string();
        let tags: Vec<String> =
            sn.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
        let text = format!("{title}\n\n{desc}\n\nTags: {}", tags.join(", "));
        let thumb = ["maxres", "standard", "high", "medium", "default"]
            .iter()
            .find_map(|k| sn.pointer(&format!("/thumbnails/{k}/url")).and_then(Value::as_str))
            .map(str::to_owned);
        Ok(ExtractedContent {
            title: Some(title).filter(|t| !t.is_empty()),
            author: sn.get("channelTitle").and_then(Value::as_str).map(str::to_owned),
            published_at: sn
                .get("publishedAt")
                .and_then(Value::as_str)
                .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                .map(|d| d.with_timezone(&chrono::Utc)),
            content_type: "youtube_video".into(),
            language: sn
                .get("defaultAudioLanguage")
                .or_else(|| sn.get("defaultLanguage"))
                .and_then(Value::as_str)
                .map(|l| l.split('-').next().unwrap_or("und").to_string())
                .unwrap_or_else(|| "und".into()),
            summary: lead_summary(&desc, None),
            text,
            metadata: ExtractedMetadata {
                canonical_url: Some(format!("https://www.youtube.com/watch?v={id}")),
                images: thumb.into_iter().collect(),
                extra: json!({ "video_id": id, "duration": item.pointer("/contentDetails/duration"), "view_count": item.pointer("/statistics/viewCount"), "transcript_available": false }),
                ..Default::default()
            },
            screenshot_url: None,
            extraction_method: "youtube_api".into(),
            extraction_status: "success".into(),
        })
    }
}
