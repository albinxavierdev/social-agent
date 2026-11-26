//! Pre-publication validation (spec §6.7). Deterministic checks live here; the optional LLM brand-rule
//! check is composed in `tools::validate`.

use crate::channels;
use crate::model::{Draft, Issue, Variant};
use crate::urls::{parse_and_normalize, UrlPolicy};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct ValidationOptions {
    pub prohibited_terms: Vec<String>,
    pub require_attribution: bool,
    pub url_policy: UrlPolicy,
}

impl Default for ValidationOptions {
    fn default() -> Self {
        Self {
            prohibited_terms: [
                "guaranteed returns",
                "get rich quick",
                "100% guaranteed",
                "miracle cure",
                "cure cancer",
                "click here to win",
                "no risk whatsoever",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            require_attribution: false,
            url_policy: UrlPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub issues: Vec<Issue>,
    pub warnings: Vec<String>,
}

impl ValidationReport {
    pub fn status(&self) -> &'static str {
        if self.issues.is_empty() {
            "passed"
        } else {
            "failed"
        }
    }
}

/// Full publishable text of a variant: caption, CTA, then hashtags.
pub fn compose_text(v: &Variant) -> String {
    let mut s = v.caption.trim().to_string();
    if let Some(c) = v.cta.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        s.push_str("\n\n");
        s.push_str(c);
    }
    if !v.hashtags.is_empty() {
        s.push_str("\n\n");
        s.push_str(&v.hashtags.join(" "));
    }
    s
}

pub fn find_urls(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|t| t.starts_with("http://") || t.starts_with("https://"))
        .map(|t| t.trim_end_matches(|c: char| ".,;:!?)\"'".contains(c)).to_string())
        .collect()
}

/// Length as the channel counts it: X wraps every URL to 23 chars; others count characters.
pub fn channel_len(channel: &str, text: &str) -> usize {
    let mut len = text.chars().count();
    if channels::canonical(channel) == "x" {
        for u in find_urls(text) {
            len = len - u.chars().count() + 23;
        }
    }
    len
}

fn fingerprint(s: &str) -> String {
    let norm: String = s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    hex::encode(Sha256::digest(norm.as_bytes()))
}

pub fn validate_deterministic(draft: &Draft, prior: &[Draft], opts: &ValidationOptions) -> ValidationReport {
    let mut r = ValidationReport::default();
    let mut issue =
        |code: &str, channel: Option<&str>, msg: String| r.issues.push(Issue { code: code.into(), channel: channel.map(str::to_owned), message: msg });
    let mut warnings: Vec<String> = vec![];

    if draft.content.variants.is_empty() {
        issue("EMPTY_DRAFT", None, "draft has no variants".into());
    }
    for v in &draft.content.variants {
        let ch = v.channel.as_str();
        let Some(spec) = channels::spec(ch) else {
            issue("UNSUPPORTED_CHANNEL", Some(ch), format!("channel '{ch}' is not supported"));
            continue;
        };
        if v.caption.trim().is_empty() {
            issue("EMPTY_CAPTION", Some(ch), "caption is empty".into());
        }

        // Character limits (caption + cta + hashtags as published).
        let text = compose_text(v);
        let len = channel_len(ch, &text);
        if len > spec.max_chars {
            issue("CHAR_LIMIT_EXCEEDED", Some(ch), format!("{len} characters exceeds the {} limit of {}", spec.name, spec.max_chars));
        }
        for (i, part) in v.parts.iter().enumerate() {
            if channel_len(ch, part) > spec.max_chars {
                issue("CHAR_LIMIT_EXCEEDED", Some(ch), format!("part {} exceeds the {} limit of {}", i + 1, spec.name, spec.max_chars));
            }
        }

        // Hashtag rules.
        if v.hashtags.len() > spec.max_hashtags {
            issue("TOO_MANY_HASHTAGS", Some(ch), format!("{} hashtags exceeds the limit of {}", v.hashtags.len(), spec.max_hashtags));
        }
        let mut seen = std::collections::HashSet::new();
        for h in &v.hashtags {
            let valid = h.starts_with('#') && h.len() > 1 && h[1..].chars().all(|c| c.is_alphanumeric() || c == '_');
            if !valid {
                issue("INVALID_HASHTAG", Some(ch), format!("'{h}' is not a valid hashtag"));
            }
            if !seen.insert(h.to_lowercase()) {
                issue("DUPLICATE_HASHTAG", Some(ch), format!("'{h}' is repeated"));
            }
        }

        // Media requirements, dimensions and accessibility.
        let media: Vec<_> = v.media.iter().collect();
        if spec.requires_media && media.is_empty() && draft.media_assets.is_empty() {
            issue("MEDIA_REQUIRED", Some(ch), format!("{} posts require media", spec.name));
        }
        if media.len() > spec.max_images {
            issue("TOO_MANY_MEDIA", Some(ch), format!("{} media items exceeds the limit of {}", media.len(), spec.max_images));
        }
        for m in &media {
            if let Some(msg) = crate::images::check_alt_text(&m.alt_text) {
                issue("ALT_TEXT_INVALID", Some(ch), msg);
            }
            match draft.media_assets.iter().find(|a| a.url == m.url) {
                None => warnings.push(format!("[{ch}] media {} is not a managed asset; dimensions could not be verified", m.url)),
                Some(a) => {
                    if a.validation_status != "passed" {
                        issue("IMAGE_UNSAFE_OR_INVALID", Some(ch), format!("image {} failed validation", a.image_id));
                    }
                    let ratio = a.width as f64 / a.height.max(1) as f64;
                    if ratio < spec.aspect_min - 1e-6 || ratio > spec.aspect_max + 1e-6 {
                        issue("IMAGE_ASPECT_RATIO", Some(ch), format!("image {} ({}) is not accepted by {}", a.image_id, a.aspect_ratio, spec.name));
                    }
                    if a.source == "source_page" {
                        warnings.push(format!("[{ch}] image {} was taken from a source page; usage rights are not verified", a.image_id));
                    }
                }
            }
        }

        // Link validity (syntax + SSRF-safe policy; no network calls here).
        for u in find_urls(&text) {
            if let Err(e) = parse_and_normalize(&u, &opts.url_policy) {
                issue("INVALID_LINK", Some(ch), format!("link '{u}' is not valid/allowed: {}", e.message));
            }
        }

        // Prohibited content.
        let lower = text.to_lowercase();
        for term in &opts.prohibited_terms {
            if lower.contains(&term.to_lowercase()) {
                issue("PROHIBITED_CONTENT", Some(ch), format!("contains prohibited phrase '{term}'"));
            }
        }

        // Duplicate content against the brand's earlier drafts.
        let fp = fingerprint(&v.caption);
        let dup = prior
            .iter()
            .filter(|d| {
                d.id != draft.id
                    && !matches!(
                        d.state,
                        crate::workflow::WorkflowState::Cancelled | crate::workflow::WorkflowState::Rejected | crate::workflow::WorkflowState::Failed
                    )
            })
            .any(|d| d.content.variants.iter().any(|pv| channels::canonical(&pv.channel) == channels::canonical(ch) && fingerprint(&pv.caption) == fp));
        if dup {
            warnings.push(format!("[{ch}] caption duplicates an existing draft for this brand"));
        }

        // Source attribution.
        if !draft.source_links.is_empty() {
            let attributed = draft.source_links.iter().any(|l| text.contains(l.as_str()));
            if !attributed {
                if opts.require_attribution {
                    issue("SOURCE_ATTRIBUTION_MISSING", Some(ch), "variant must credit/link its source".into());
                } else {
                    warnings.push(format!("[{ch}] variant does not link or credit its source"));
                }
            }
        }
    }
    r.warnings = warnings;
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::workflow::WorkflowState;

    fn draft(variants: Vec<Variant>) -> Draft {
        Draft {
            id: "draft_1".into(),
            job_id: "j".into(),
            report_id: "r".into(),
            campaign_id: None,
            tenant_id: "t".into(),
            brand_id: "b".into(),
            channel: "multi".into(),
            format: "text".into(),
            state: WorkflowState::DraftReady,
            content: DraftContent { variants },
            media_assets: vec![],
            source_links: vec![],
            compliance: Compliance::pending(),
            brand_context: BrandContext::default(),
            model: ModelInfo {
                provider: "m".into(),
                model: "m".into(),
                prompt_version: "v".into(),
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
                latency_ms: 0,
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn v(channel: &str, caption: &str) -> Variant {
        Variant { channel: channel.into(), format: "text".into(), caption: caption.into(), hashtags: vec![], cta: None, media: vec![], parts: vec![] }
    }

    fn codes(r: &ValidationReport) -> Vec<&str> {
        r.issues.iter().map(|i| i.code.as_str()).collect()
    }

    #[test]
    fn clean_draft_passes() {
        let r = validate_deterministic(&draft(vec![v("linkedin", "A solid post.")]), &[], &ValidationOptions::default());
        assert_eq!(r.status(), "passed", "{:?}", r.issues);
    }

    #[test]
    fn enforces_character_limits_with_x_url_weighting() {
        let long = "a".repeat(281);
        assert!(codes(&validate_deterministic(&draft(vec![v("x", &long)]), &[], &ValidationOptions::default())).contains(&"CHAR_LIMIT_EXCEEDED"));
        // A 100-char URL counts as 23 on X.
        let url = format!("https://example.com/{}", "p".repeat(80));
        let caption = format!("{} {}", "a".repeat(250), url);
        let r = validate_deterministic(&draft(vec![v("twitter", &caption)]), &[], &ValidationOptions::default());
        assert!(!codes(&r).contains(&"CHAR_LIMIT_EXCEEDED"), "{:?}", r.issues);
        assert_eq!(channel_len("x", &caption), 250 + 1 + 23);
    }

    #[test]
    fn checks_hashtags() {
        let mut x = v("x", "hello");
        x.hashtags = vec!["#a".into(), "#A".into(), "bad".into(), "#b".into(), "#c".into(), "#d".into()];
        let c: Vec<String> = validate_deterministic(&draft(vec![x]), &[], &ValidationOptions::default()).issues.into_iter().map(|i| i.code).collect();
        for want in ["TOO_MANY_HASHTAGS", "INVALID_HASHTAG", "DUPLICATE_HASHTAG"] {
            assert!(c.iter().any(|x| x == want), "missing {want}: {c:?}");
        }
    }

    #[test]
    fn instagram_requires_media() {
        let r = validate_deterministic(&draft(vec![v("instagram", "pic")]), &[], &ValidationOptions::default());
        assert!(codes(&r).contains(&"MEDIA_REQUIRED"));
    }

    #[test]
    fn flags_prohibited_and_bad_links() {
        let r = validate_deterministic(&draft(vec![v("linkedin", "Guaranteed Returns! see http://localhost/admin")]), &[], &ValidationOptions::default());
        assert!(codes(&r).contains(&"PROHIBITED_CONTENT"));
        assert!(codes(&r).contains(&"INVALID_LINK"));
    }

    #[test]
    fn duplicate_and_attribution_are_warnings_by_default() {
        let prior = draft(vec![v("linkedin", "Same words here")]);
        let mut cur = draft(vec![v("linkedin", "same   WORDS, here!")]);
        cur.id = "draft_2".into();
        cur.source_links = vec!["https://example.com/a".into()];
        let r = validate_deterministic(&cur, &[prior], &ValidationOptions::default());
        assert_eq!(r.status(), "passed");
        assert!(r.warnings.iter().any(|w| w.contains("duplicates")));
        assert!(r.warnings.iter().any(|w| w.contains("source")));
        let strict = ValidationOptions { require_attribution: true, ..Default::default() };
        assert!(codes(&validate_deterministic(&cur, &[], &strict)).contains(&"SOURCE_ATTRIBUTION_MISSING"));
    }
}
