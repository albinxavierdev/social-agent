//! Lightweight static extraction: SSRF-safe HTTP fetch + readability-style HTML parsing.

use super::html::{lead_summary, parse, redirect_stub_target};
use super::{ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest};
use crate::ssrf::SafeFetcher;
use crate::urls::parse_and_normalize;
use async_trait::async_trait;

pub struct HttpExtractor {
    fetcher: SafeFetcher,
}

impl HttpExtractor {
    pub fn new(fetcher: SafeFetcher) -> Self {
        Self { fetcher }
    }
}

/// Below this many characters of body text we consider the static result insufficient.
const MIN_STATIC_TEXT: usize = 200;

#[async_trait]
impl ContentExtractor for HttpExtractor {
    fn name(&self) -> &'static str {
        "http"
    }

    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        let mut page = self.fetcher.fetch(&req.url).await?;
        // Follow up to two HTML "meta refresh" stubs (tiny pages that just forward somewhere else). Every hop
        // goes through the same URL policy + SSRF checks as the first request.
        let mut parsed;
        let mut hops = 0;
        loop {
            if page.status == 401 || page.status == 403 {
                // Never try to bypass auth/paywalls (spec §10); let the chain fall back to a
                // managed provider that has its own ToS posture, or fail.
                return Err(ExtractionError::failed(format!("access denied ({})", page.status), false));
            }
            if page.status >= 400 {
                return Err(ExtractionError::failed(format!("upstream returned {}", page.status), page.status >= 500 || page.status == 429));
            }
            let ct = page.content_type.clone().unwrap_or_default().to_ascii_lowercase();
            if !(ct.is_empty() || ct.contains("html") || ct.contains("xml") || ct.starts_with("text/")) {
                return Err(ExtractionError::Unsupported(format!(
                    "content-type '{ct}' is not HTML; only web pages (HTML/text) can be read, not files like PDFs or images"
                )));
            }
            let html = String::from_utf8_lossy(&page.body).into_owned();
            parsed = parse(&html, &page.final_url);
            if parsed.text.chars().count() < MIN_STATIC_TEXT && hops < 2 {
                if let Some(target) = redirect_stub_target(&html, &page.final_url) {
                    let target = parse_and_normalize(target.as_str(), &self.fetcher.policy)?;
                    page = self.fetcher.fetch(&target).await?;
                    hops += 1;
                    continue;
                }
            }
            break;
        }
        let p = parsed;

        if p.text.chars().count() < MIN_STATIC_TEXT && (p.has_spa_root || p.script_count >= 2) {
            return Err(ExtractionError::NeedsRendering);
        }
        if p.text.trim().is_empty() {
            return Err(ExtractionError::failed("no extractable text", false));
        }
        let summary = lead_summary(&p.text, p.og_description.as_deref());
        Ok(ExtractedContent {
            title: p.og_title.clone().or(p.title.clone()),
            author: p.author,
            published_at: p.published_at,
            content_type: "article".into(),
            language: p.language.unwrap_or_else(|| "und".into()),
            summary,
            text: p.text,
            metadata: ExtractedMetadata {
                og_title: p.og_title,
                og_description: p.og_description,
                canonical_url: p.canonical_url,
                images: p.images,
                extra: serde_json::json!({ "final_url": page.final_url.as_str(), "http_status": page.status, "redirect_stubs_followed": hops }),
            },
            screenshot_url: None,
            extraction_method: "http".into(),
            extraction_status: "success".into(),
        })
    }
}
