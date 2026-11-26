//! Content extraction adapters (spec §10) behind the `ContentExtractor` trait, plus the
//! registry that picks the extractor chain per source type (spec §6.2 table).

pub mod firecrawl;
pub mod github;
pub mod html;
pub mod http;
pub mod obscura;
pub mod robots;
pub mod x;
pub mod youtube;

use crate::error::{ApiError, ErrorCode};
use crate::urls::SourceType;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use url::Url;

#[derive(Debug, Clone)]
pub struct ExtractionRequest {
    pub source_id: String,
    pub tenant_id: String,
    pub brand_id: String,
    pub url: Url,
    pub source_type: SourceType,
    /// Reviewers need a screenshot: only a browser can provide one.
    pub want_screenshot: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractedMetadata {
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub canonical_url: Option<String>,
    pub images: Vec<String>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedContent {
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub content_type: String,
    pub language: String,
    pub text: String,
    pub summary: String,
    pub metadata: ExtractedMetadata,
    pub screenshot_url: Option<String>,
    pub extraction_method: String,
    pub extraction_status: String,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ExtractionError {
    /// Static extraction found an empty JS shell; escalate to a browser.
    #[error("page requires JavaScript rendering")]
    NeedsRendering,
    #[error("unsupported content: {0}")]
    Unsupported(String),
    #[error("extractor not configured: {0}")]
    NotConfigured(&'static str),
    #[error("blocked: {0}")]
    Blocked(String),
    #[error("{message}")]
    Failed { message: String, retryable: bool },
    /// A policy/SSRF/robots error that must surface to the caller unchanged.
    #[error("{0}")]
    Api(ApiError),
}

impl ExtractionError {
    pub fn failed(msg: impl Into<String>, retryable: bool) -> Self {
        Self::Failed { message: msg.into(), retryable }
    }
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Failed { retryable: true, .. })
    }
    pub fn into_api(self) -> ApiError {
        match self {
            Self::Api(e) => e,
            Self::Blocked(m) => ApiError::new(ErrorCode::UrlNotAllowed, m),
            // A missing key/binary is a setup problem, not a bad URL: say so and say how to fix it.
            Self::NotConfigured(why) => ApiError::new(
                ErrorCode::ExtractionFailed,
                format!("This extractor isn't set up on this server: {why}. Add it to the server's environment (see .env.example) and restart."),
            )
            .retryable(false)
            .with_details(serde_json::json!({ "reason": "extractor_not_configured", "setup": why })),
            e => {
                let retryable = e.retryable();
                ApiError::new(ErrorCode::ExtractionFailed, e.to_string()).retryable(retryable)
            }
        }
    }
}

impl From<ApiError> for ExtractionError {
    fn from(e: ApiError) -> Self {
        match e.code {
            // Network-ish failures are plain failures so the chain can fall back.
            ErrorCode::ExtractionFailed => Self::Failed { message: e.message.clone(), retryable: e.retryable },
            _ => Self::Api(e),
        }
    }
}

#[async_trait]
pub trait ContentExtractor: Send + Sync {
    fn name(&self) -> &'static str;
    async fn extract(&self, request: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError>;
}

#[derive(Debug, Clone)]
pub struct ExtractorFlags {
    pub http_enabled: bool,
    pub obscura_enabled: bool,
    pub firecrawl_enabled: bool,
    /// Large-scale mode: Firecrawl first, Obscura as fallback (spec table row 8).
    pub prefer_firecrawl: bool,
    pub respect_robots: bool,
}

impl Default for ExtractorFlags {
    fn default() -> Self {
        Self { http_enabled: true, obscura_enabled: true, firecrawl_enabled: true, prefer_firecrawl: false, respect_robots: true }
    }
}

pub struct ExtractorRegistry {
    extractors: HashMap<&'static str, Arc<dyn ContentExtractor>>,
    flags: ExtractorFlags,
    robots: Option<Arc<robots::RobotsChecker>>,
}

impl ExtractorRegistry {
    pub fn new(flags: ExtractorFlags) -> Self {
        Self { extractors: HashMap::new(), flags, robots: None }
    }
    pub fn with(mut self, e: Arc<dyn ContentExtractor>) -> Self {
        self.extractors.insert(e.name(), e);
        self
    }
    pub fn with_robots(mut self, r: Arc<robots::RobotsChecker>) -> Self {
        self.robots = Some(r);
        self
    }

    /// Ordered extractor names to try for a request. Pure — unit-tested.
    pub fn plan(&self, req: &ExtractionRequest) -> Vec<&'static str> {
        let f = &self.flags;
        let mut chain: Vec<&'static str> = match req.source_type {
            SourceType::GithubRepository | SourceType::GithubFile | SourceType::GithubIssue | SourceType::GithubPullRequest => vec!["github"],
            SourceType::XPost => vec!["x"],
            SourceType::YoutubeVideo => vec!["youtube"],
            SourceType::WebArticle | SourceType::WebPage => {
                if req.want_screenshot {
                    // Only a browser can screenshot; no fallback (spec table).
                    vec!["obscura"]
                } else if f.prefer_firecrawl {
                    vec!["firecrawl", "obscura"]
                } else {
                    // Lightweight first; Obscura only if static extraction is insufficient.
                    vec!["http", "obscura", "firecrawl"]
                }
            }
        };
        chain.retain(|n| match *n {
            "http" => f.http_enabled,
            "obscura" => f.obscura_enabled,
            "firecrawl" => f.firecrawl_enabled,
            _ => true,
        });
        chain.retain(|n| self.extractors.contains_key(n));
        chain
    }

    pub async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ApiError> {
        if req.source_type.is_web() && self.flags.respect_robots {
            if let Some(r) = &self.robots {
                r.ensure_allowed(&req.url).await?;
            }
        }
        let chain = self.plan(req);
        if chain.is_empty() {
            return Err(ApiError::new(ErrorCode::ExtractionFailed, "no extractor is enabled for this source type"));
        }
        let mut first_real: Option<ExtractionError> = None;
        let mut unavailable: Vec<String> = vec![];
        let mut last: Option<ExtractionError> = None;
        for name in chain {
            let ex = &self.extractors[name];
            let started = std::time::Instant::now();
            let res = ex.extract(req).await;
            let ok = res.is_ok();
            metrics::counter!("extraction_attempts_total", "extractor" => name, "outcome" => if ok { "success" } else { "failure" }).increment(1);
            metrics::histogram!("extraction_duration_seconds", "extractor" => name).record(started.elapsed().as_secs_f64());
            match res {
                Ok(c) => return Ok(c),
                // Policy violations are never retried via another extractor.
                Err(e @ (ExtractionError::Api(_) | ExtractionError::Blocked(_))) => return Err(e.into_api()),
                Err(e) => {
                    tracing::warn!(extractor = name, error = %e, "extractor failed, trying next");
                    match &e {
                        ExtractionError::NotConfigured(why) => unavailable.push(format!("{name}: {why}")),
                        _ if first_real.is_none() => first_real = Some(e.clone()),
                        _ => {}
                    }
                    last = Some(e);
                }
            }
        }
        // Report the first REAL cause. A later "extractor not installed" must not hide why the cheap path failed.
        let err = match first_real {
            Some(ExtractionError::NeedsRendering) => ExtractionError::failed(
                if unavailable.is_empty() {
                    "page requires JavaScript rendering and the renderers also failed".to_string()
                } else {
                    format!("page requires JavaScript rendering, but no renderer is available ({})", unavailable.join("; "))
                },
                false,
            ),
            Some(e) => e,
            None => last.unwrap_or_else(|| ExtractionError::failed("extraction failed", false)),
        };
        Err(err.into_api())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy(&'static str);
    #[async_trait]
    impl ContentExtractor for Dummy {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn extract(&self, _: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
            Err(ExtractionError::failed("dummy", false))
        }
    }

    fn registry(flags: ExtractorFlags) -> ExtractorRegistry {
        ["http", "obscura", "firecrawl", "github", "x", "youtube"].into_iter().fold(ExtractorRegistry::new(flags), |r, n| r.with(Arc::new(Dummy(n))))
    }

    fn req(t: SourceType, shot: bool) -> ExtractionRequest {
        ExtractionRequest {
            source_id: "s".into(),
            tenant_id: "t".into(),
            brand_id: "b".into(),
            url: Url::parse("https://example.com/a").unwrap(),
            source_type: t,
            want_screenshot: shot,
        }
    }

    #[test]
    fn static_first_then_browser_then_managed() {
        let r = registry(ExtractorFlags::default());
        assert_eq!(r.plan(&req(SourceType::WebArticle, false)), vec!["http", "obscura", "firecrawl"]);
    }

    #[test]
    fn screenshots_require_obscura_only() {
        let r = registry(ExtractorFlags::default());
        assert_eq!(r.plan(&req(SourceType::WebPage, true)), vec!["obscura"]);
    }

    #[test]
    fn dedicated_integrations_have_no_fallback() {
        let r = registry(ExtractorFlags::default());
        assert_eq!(r.plan(&req(SourceType::GithubRepository, false)), vec!["github"]);
        assert_eq!(r.plan(&req(SourceType::XPost, false)), vec!["x"]);
        assert_eq!(r.plan(&req(SourceType::YoutubeVideo, false)), vec!["youtube"]);
    }

    #[test]
    fn large_scale_prefers_firecrawl_and_flags_disable() {
        let mut f = ExtractorFlags::default();
        f.prefer_firecrawl = true;
        assert_eq!(registry(f.clone()).plan(&req(SourceType::WebArticle, false)), vec!["firecrawl", "obscura"]);
        f.obscura_enabled = false;
        assert_eq!(registry(f).plan(&req(SourceType::WebArticle, false)), vec!["firecrawl"]);
    }

    #[test]
    fn unregistered_extractors_are_skipped() {
        let r = ExtractorRegistry::new(ExtractorFlags::default()).with(Arc::new(Dummy("http")));
        assert_eq!(r.plan(&req(SourceType::WebArticle, false)), vec!["http"]);
    }
}
