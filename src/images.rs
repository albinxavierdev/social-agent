//! Image pipeline (spec §6.6): source candidates → validate (format, size, aspect, safety) → rank → store.

use crate::channels::{self, aspect_label};
use crate::context::Scope;
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::ids::{new_id, IMAGE};
use crate::model::ImageAsset;
use crate::ssrf::SafeFetcher;
use crate::storage::ObjectStorage;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use url::Url;

pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
pub const MIN_DIMENSION: u32 = 320;
pub const MAX_DIMENSION: u32 = 8192;

#[derive(Debug, Clone)]
pub struct ImageCandidate {
    pub bytes: Vec<u8>,
    pub source: &'static str, // generated | source_page | search
    pub alt_text: String,
    pub origin_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ImageQuery {
    pub prompt: String,
    pub alt_text: String,
    pub source_image_urls: Vec<String>,
    pub allow_source_images: bool,
    pub count: usize,
}

#[async_trait]
pub trait ImageProvider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn candidates(&self, q: &ImageQuery) -> ApiResult<Vec<ImageCandidate>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageCheck {
    pub width: u32,
    pub height: u32,
    pub format: &'static str,
    pub issues: Vec<String>,
}

impl ImageCheck {
    pub fn passed(&self) -> bool {
        self.issues.is_empty()
    }
}

/// Sniff the real format from magic bytes (never trust declared content-type).
pub fn sniff_format(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpeg")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("webp")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("gif")
    } else {
        None
    }
}

/// Unsafe-file scan hook. The built-in scanner catches polyglot/active content; wire ClamAV or a
/// vendor scanner behind this trait in production (spec §13 "malware and unsafe-file scanning").
pub fn basic_scan(bytes: &[u8]) -> Result<(), String> {
    let head = &bytes[..bytes.len().min(4096)];
    let tail = &bytes[bytes.len().saturating_sub(4096)..];
    for needle in [&b"<script"[..], b"<?php", b"<svg", b"MZ\x90\x00", b"\x7fELF", b"<html", b"%PDF"] {
        let found = |h: &[u8]| h.windows(needle.len()).any(|w| w.eq_ignore_ascii_case(needle));
        if found(head) || found(tail) {
            return Err("embedded active or executable content".into());
        }
    }
    Ok(())
}

/// Validate an image against the strictest requirements of the target channels.
pub fn validate_image(bytes: &[u8], target_channels: &[String]) -> ImageCheck {
    let mut issues = vec![];
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        issues.push(format!("file size must be 1..={} bytes", MAX_IMAGE_BYTES));
    }
    let Some(format) = sniff_format(bytes) else {
        issues.push("unsupported or unrecognised image format (png, jpeg, webp, gif only)".into());
        return ImageCheck { width: 0, height: 0, format: "unknown", issues };
    };
    if let Err(e) = basic_scan(bytes) {
        issues.push(format!("failed safety scan: {e}"));
    }
    let (w, h) = match imagesize::blob_size(bytes) {
        Ok(s) => (s.width as u32, s.height as u32),
        Err(_) => {
            issues.push("image header is corrupt".into());
            return ImageCheck { width: 0, height: 0, format, issues };
        }
    };
    if w < MIN_DIMENSION || h < MIN_DIMENSION {
        issues.push(format!("image too small ({w}x{h}); minimum {MIN_DIMENSION}px per side"));
    }
    if w > MAX_DIMENSION || h > MAX_DIMENSION {
        issues.push(format!("image too large ({w}x{h}); maximum {MAX_DIMENSION}px per side"));
    }
    if h > 0 {
        let ratio = w as f64 / h as f64;
        for c in target_channels {
            if let Some(s) = channels::spec(c) {
                if ratio < s.aspect_min - 1e-6 || ratio > s.aspect_max + 1e-6 {
                    issues.push(format!("aspect ratio {} is not accepted by {}", aspect_label(w, h), s.name));
                }
            }
        }
    }
    ImageCheck { width: w, height: h, format, issues }
}

fn ext(format: &str) -> &'static str {
    match format {
        "png" => "png",
        "jpeg" => "jpg",
        "webp" => "webp",
        "gif" => "gif",
        _ => "bin",
    }
}

fn mime(format: &str) -> &'static str {
    match format {
        "png" => "image/png",
        "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

/// Alt text must exist and stay within what channels accept.
pub fn check_alt_text(alt: &str) -> Option<String> {
    let n = alt.trim().chars().count();
    if n == 0 {
        Some("alt text is required for accessibility".into())
    } else if n > 420 {
        Some("alt text is longer than 420 characters".into())
    } else {
        None
    }
}

pub struct ImagePipeline {
    pub providers: Vec<Arc<dyn ImageProvider>>,
    pub storage: Arc<dyn ObjectStorage>,
}

impl ImagePipeline {
    /// Gather candidates from every provider, validate, rank, and persist the best `limit` passing assets.
    /// Failures of individual providers/candidates degrade gracefully; only "nothing usable" is an error.
    pub async fn find(&self, scope: &Scope, draft_id: &str, q: &ImageQuery, target_channels: &[String], limit: usize) -> ApiResult<(Vec<ImageAsset>, usize)> {
        let mut all: Vec<ImageCandidate> = vec![];
        let mut provider_errors = vec![];
        for p in &self.providers {
            match p.candidates(q).await {
                Ok(mut c) => all.append(&mut c),
                Err(e) => {
                    tracing::warn!(provider = p.name(), error = %e.message, "image provider failed");
                    provider_errors.push(e);
                }
            }
        }
        let mut scored: Vec<(f64, ImageCandidate, ImageCheck)> = vec![];
        let mut rejected = 0usize;
        for c in all {
            let check = validate_image(&c.bytes, target_channels);
            if !check.passed() {
                rejected += 1;
                metrics::counter!("image_validation_total", "outcome" => "failed").increment(1);
                continue;
            }
            metrics::counter!("image_validation_total", "outcome" => "passed").increment(1);
            // Rank: generated/owned > source page; closer to square; higher resolution (capped).
            let ratio = check.width as f64 / check.height as f64;
            let square = 1.0 - (ratio - 1.0).abs().min(1.0);
            let res = ((check.width.min(2048) * check.height.min(2048)) as f64) / (2048.0 * 2048.0);
            let origin = if c.source == "generated" { 0.3 } else { 0.0 };
            scored.push((origin + square * 0.5 + res * 0.2, c, check));
        }
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut out = vec![];
        for (_, c, check) in scored.into_iter().take(limit.max(1)) {
            let id = new_id(IMAGE);
            let key = format!("{}/{}/images/{}/{}.{}", scope.tenant_id, scope.brand_id, draft_id, id, ext(check.format));
            let url = self.storage.put(&key, c.bytes, mime(check.format)).await?;
            out.push(ImageAsset {
                image_id: id,
                url,
                width: check.width,
                height: check.height,
                aspect_ratio: aspect_label(check.width, check.height),
                alt_text: if c.alt_text.trim().is_empty() { q.alt_text.clone() } else { c.alt_text },
                source: c.source.to_string(),
                validation_status: "passed".into(),
            });
        }
        if out.is_empty() {
            let retryable = !provider_errors.is_empty() && provider_errors.iter().all(|e| e.retryable);
            return Err(ApiError::new(ErrorCode::ExtractionFailed, "no valid image candidates were found")
                .with_details(json!({ "rejected": rejected, "provider_errors": provider_errors.len() }))
                .retryable(retryable));
        }
        Ok((out, rejected))
    }
}

// --------------------------------------------------------------- providers

/// Reuses images found on the source pages. Rights are NOT verified, so this is opt-in per request.
pub struct SourcePageImageProvider {
    pub fetcher: SafeFetcher,
}

#[async_trait]
impl ImageProvider for SourcePageImageProvider {
    fn name(&self) -> &'static str {
        "source_page"
    }
    async fn candidates(&self, q: &ImageQuery) -> ApiResult<Vec<ImageCandidate>> {
        if !q.allow_source_images {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for raw in q.source_image_urls.iter().take(q.count.max(3)) {
            let Ok(url) = crate::urls::parse_and_normalize(raw, &self.fetcher.policy) else { continue };
            match self.fetcher.fetch(&url).await {
                Ok(p) if p.status == 200 => {
                    out.push(ImageCandidate { bytes: p.body, source: "source_page", alt_text: q.alt_text.clone(), origin_url: Some(url.to_string()) })
                }
                _ => continue,
            }
        }
        Ok(out)
    }
}

/// OpenAI-compatible image generation (`/images/generations`, base64 response).
pub struct OpenAiImageProvider {
    base_url: String,
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl OpenAiImageProvider {
    pub fn new(base_url: &str, api_key: &str, model: &str, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').into(),
            api_key: api_key.into(),
            model: model.into(),
            http: reqwest::Client::builder().timeout(timeout).build()?,
        })
    }
}

#[async_trait]
impl ImageProvider for OpenAiImageProvider {
    fn name(&self) -> &'static str {
        "openai_images"
    }
    async fn candidates(&self, q: &ImageQuery) -> ApiResult<Vec<ImageCandidate>> {
        use base64::Engine;
        let resp = self
            .http
            .post(format!("{}/images/generations", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&json!({ "model": self.model, "prompt": q.prompt, "n": q.count.clamp(1, 4), "size": "1024x1024" }))
            .send()
            .await
            .map_err(|e| ApiError::new(ErrorCode::ExtractionFailed, format!("image provider unreachable: {}", e.without_url())).retryable(true))?;
        let status = resp.status();
        if status.as_u16() == 400 {
            // Provider-side safety refusal or bad prompt: not retryable.
            return Err(ApiError::new(ErrorCode::ContentPolicyViolation, "image provider rejected the prompt"));
        }
        if !status.is_success() {
            return Err(ApiError::new(ErrorCode::ExtractionFailed, format!("image provider returned {status}"))
                .retryable(status.as_u16() == 429 || status.is_server_error()));
        }
        let v: serde_json::Value = resp.json().await.map_err(ApiError::internal)?;
        let engine = base64::engine::general_purpose::STANDARD;
        Ok(v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| d["b64_json"].as_str())
                    .filter_map(|b| engine.decode(b).ok())
                    .map(|bytes| ImageCandidate { bytes, source: "generated", alt_text: q.alt_text.clone(), origin_url: None })
                    .collect()
            })
            .unwrap_or_default())
    }
}

/// Offline provider that renders a solid-colour PNG. Dev/test only.
pub struct MockImageProvider {
    pub width: u32,
    pub height: u32,
}

pub fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        let row: Vec<u8> = rgb.iter().copied().cycle().take(w as usize * 3).collect();
        let data: Vec<u8> = row.iter().copied().cycle().take(row.len() * h as usize).collect();
        writer.write_image_data(&data).expect("png data");
    }
    out
}

#[async_trait]
impl ImageProvider for MockImageProvider {
    fn name(&self) -> &'static str {
        "mock_images"
    }
    async fn candidates(&self, q: &ImageQuery) -> ApiResult<Vec<ImageCandidate>> {
        Ok((0..q.count.max(1))
            .map(|i| ImageCandidate {
                bytes: solid_png(self.width, self.height, [40 + i as u8 * 30, 90, 160]),
                source: "generated",
                alt_text: q.alt_text.clone(),
                origin_url: None,
            })
            .collect())
    }
}

pub fn parse_image_url(s: &str) -> Option<Url> {
    Url::parse(s).ok().filter(|u| matches!(u.scheme(), "http" | "https"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    fn chans(c: &[&str]) -> Vec<String> {
        c.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn accepts_square_png_and_reports_dimensions() {
        let c = validate_image(&solid_png(1080, 1080, [1, 2, 3]), &chans(&["instagram", "facebook"]));
        assert!(c.passed(), "{:?}", c.issues);
        assert_eq!((c.width, c.height, c.format), (1080, 1080, "png"));
    }

    #[test]
    fn rejects_wrong_aspect_for_channel() {
        let c = validate_image(&solid_png(1600, 400, [1, 2, 3]), &chans(&["instagram"]));
        assert!(c.issues.iter().any(|i| i.contains("aspect ratio")), "{:?}", c.issues);
    }

    #[test]
    fn rejects_small_non_image_and_active_content() {
        assert!(!validate_image(&solid_png(100, 100, [0; 3]), &[]).passed());
        assert!(!validate_image(b"<svg onload=alert(1)></svg>", &[]).passed());
        let mut polyglot = solid_png(400, 400, [9, 9, 9]);
        polyglot.extend_from_slice(b"<?php system($_GET['c']); ?>");
        assert!(validate_image(&polyglot, &[]).issues.iter().any(|i| i.contains("safety")));
        assert!(!validate_image(&[], &[]).passed());
    }

    #[test]
    fn alt_text_rules() {
        assert!(check_alt_text("  ").is_some());
        assert!(check_alt_text("A chart").is_none());
        assert!(check_alt_text(&"a".repeat(500)).is_some());
    }

    #[tokio::test]
    async fn pipeline_stores_ranked_valid_assets_only() {
        struct Mixed;
        #[async_trait]
        impl ImageProvider for Mixed {
            fn name(&self) -> &'static str {
                "mixed"
            }
            async fn candidates(&self, _: &ImageQuery) -> ApiResult<Vec<ImageCandidate>> {
                let mk = |b: Vec<u8>| ImageCandidate { bytes: b, source: "generated", alt_text: String::new(), origin_url: None };
                Ok(vec![mk(b"not an image".to_vec()), mk(solid_png(600, 600, [1, 1, 1])), mk(solid_png(1080, 1080, [2, 2, 2]))])
            }
        }
        let storage = Arc::new(MemoryStorage::default());
        let p = ImagePipeline { providers: vec![Arc::new(Mixed)], storage: storage.clone() };
        let scope = Scope { tenant_id: "t1".into(), brand_id: "b1".into() };
        let q = ImageQuery { prompt: "p".into(), alt_text: "Alt".into(), source_image_urls: vec![], allow_source_images: false, count: 3 };
        let (assets, rejected) = p.find(&scope, "draft_1", &q, &chans(&["instagram"]), 1).await.unwrap();
        assert_eq!(rejected, 1);
        assert_eq!(assets.len(), 1);
        assert_eq!((assets[0].width, assets[0].aspect_ratio.as_str(), assets[0].alt_text.as_str()), (1080, "1:1", "Alt"));
        assert!(storage.objects.lock().unwrap().keys().all(|k| k.starts_with("t1/b1/images/draft_1/")));
    }

    #[tokio::test]
    async fn pipeline_errors_when_nothing_valid() {
        let p = ImagePipeline { providers: vec![Arc::new(MockImageProvider { width: 50, height: 50 })], storage: Arc::new(MemoryStorage::default()) };
        let scope = Scope { tenant_id: "t".into(), brand_id: "b".into() };
        let q = ImageQuery { prompt: "p".into(), alt_text: "a".into(), source_image_urls: vec![], allow_source_images: false, count: 1 };
        assert!(p.find(&scope, "d", &q, &[], 1).await.is_err());
    }
}
