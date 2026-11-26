//! URL validation, normalisation, and source-type detection (spec §6.1, §13).

use crate::error::{ApiError, ApiResult, ErrorCode};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    WebArticle,
    WebPage, // product / landing / marketing pages — often JS-rendered
    GithubRepository,
    GithubFile,
    GithubIssue,
    GithubPullRequest,
    XPost,
    YoutubeVideo,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WebArticle => "web_article",
            Self::WebPage => "web_page",
            Self::GithubRepository => "github_repository",
            Self::GithubFile => "github_file",
            Self::GithubIssue => "github_issue",
            Self::GithubPullRequest => "github_pull_request",
            Self::XPost => "x_post",
            Self::YoutubeVideo => "youtube_video",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "web_article" => Self::WebArticle,
            "web_page" => Self::WebPage,
            "github_repository" => Self::GithubRepository,
            "github_file" => Self::GithubFile,
            "github_issue" => Self::GithubIssue,
            "github_pull_request" => Self::GithubPullRequest,
            "x_post" => Self::XPost,
            "youtube_video" => Self::YoutubeVideo,
            _ => return None,
        })
    }

    pub fn is_web(self) -> bool {
        matches!(self, Self::WebArticle | Self::WebPage)
    }

    /// Human-readable extraction plan returned from `ingest_url`.
    pub fn extraction_strategy(self) -> &'static str {
        match self {
            Self::WebArticle => "http_or_obscura",
            Self::WebPage => "obscura_or_firecrawl",
            Self::GithubRepository | Self::GithubFile | Self::GithubIssue | Self::GithubPullRequest => "github_api",
            Self::XPost => "x_api",
            Self::YoutubeVideo => "youtube_api",
        }
    }
}

const TRACKING_PARAMS: &[&str] = &["fbclid", "gclid", "mc_cid", "mc_eid", "igshid", "ref_src", "yclid", "msclkid"];

#[derive(Debug, Clone, Default)]
pub struct UrlPolicy {
    pub allow_http: bool,
    pub allowlist_hosts: Option<Vec<String>>,
    pub denylist_hosts: Vec<String>,
    /// Test-only escape hatch so integration tests can hit localhost mocks.
    /// Never wired to configuration or environment variables.
    allow_private_networks: bool,
}

impl UrlPolicy {
    /// DANGEROUS: disables private-network blocking. Tests only.
    pub fn dangerously_allow_private_networks(mut self) -> Self {
        self.allow_private_networks = true;
        self.allow_http = true;
        self
    }
    pub fn private_networks_allowed(&self) -> bool {
        self.allow_private_networks
    }
}

fn blocked(msg: impl Into<String>, url: &str) -> ApiError {
    ApiError::new(ErrorCode::UrlNotAllowed, msg).with_details(serde_json::json!({ "url": url }))
}

fn host_matches(host: &str, rule: &str) -> bool {
    let rule = rule.trim_start_matches("*.").to_ascii_lowercase();
    host == rule || host.ends_with(&format!(".{rule}"))
}

/// Parse, validate, and normalise a user-supplied URL. Does not touch the network.
pub fn parse_and_normalize(raw: &str, policy: &UrlPolicy) -> ApiResult<Url> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err(ApiError::validation("url must be 1..=2048 characters"));
    }
    let mut url = Url::parse(raw).map_err(|e| ApiError::validation(format!("invalid url: {e}")))?;

    match url.scheme() {
        "https" => {}
        "http" if policy.allow_http => {}
        s => return Err(blocked(format!("scheme '{s}' is not allowed"), raw)),
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(blocked("urls with embedded credentials are not allowed", raw));
    }
    let host = url.host_str().ok_or_else(|| ApiError::validation("url has no host"))?.to_ascii_lowercase();

    if !policy.allow_private_networks {
        if let Some(port) = url.port() {
            if port != 80 && port != 443 {
                return Err(blocked("non-standard ports are not allowed", raw));
            }
        }
        const BAD_SUFFIXES: &[&str] = &[".local", ".localhost", ".internal", ".lan", ".home", ".corp"];
        if host == "localhost" || BAD_SUFFIXES.iter().any(|s| host.ends_with(s)) || host == "metadata.google.internal" {
            return Err(blocked("internal hostnames are not allowed", raw));
        }
        // IP literals are checked immediately; hostnames are checked after DNS resolution.
        if let Some(url::Host::Ipv4(ip)) = url.host() {
            if crate::ssrf::is_forbidden_ip(ip.into()) {
                return Err(blocked("non-public IP addresses are not allowed", raw));
            }
        }
        if let Some(url::Host::Ipv6(ip)) = url.host() {
            if crate::ssrf::is_forbidden_ip(ip.into()) {
                return Err(blocked("non-public IP addresses are not allowed", raw));
            }
        }
    }
    if policy.denylist_hosts.iter().any(|r| host_matches(&host, r)) {
        return Err(blocked("host is on the deny list", raw));
    }
    if let Some(allow) = &policy.allowlist_hosts {
        if !allow.iter().any(|r| host_matches(&host, r)) {
            return Err(blocked("host is not on the allow list", raw));
        }
    }

    // Normalise: drop fragment, default port, tracking params; sort remaining params.
    url.set_fragment(None);
    if (url.scheme() == "https" && url.port() == Some(443)) || (url.scheme() == "http" && url.port() == Some(80)) {
        let _ = url.set_port(None);
    }
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !(k.starts_with("utm_") || TRACKING_PARAMS.contains(&k.as_str()))
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    pairs.sort();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    if url.path() == "" {
        url.set_path("/");
    }
    Ok(url)
}

/// Detect what kind of source a URL points at. `hint` is the caller's `content_type`.
pub fn detect_source_type(url: &Url, hint: &str) -> SourceType {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let segs: Vec<&str> = url.path_segments().map(|s| s.filter(|x| !x.is_empty()).collect()).unwrap_or_default();

    if host == "github.com" || host == "www.github.com" {
        return match segs.as_slice() {
            [_, _, "issues", _, ..] => SourceType::GithubIssue,
            [_, _, "pull", _, ..] => SourceType::GithubPullRequest,
            [_, _, "blob", ..] | [_, _, "tree", ..] if segs.len() > 3 => SourceType::GithubFile,
            _ => SourceType::GithubRepository,
        };
    }
    if matches!(host.as_str(), "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com" | "mobile.twitter.com") && segs.len() >= 3 && segs[1] == "status" {
        return SourceType::XPost;
    }
    if matches!(host.as_str(), "youtube.com" | "www.youtube.com" | "m.youtube.com") && (segs.first() == Some(&"watch") || segs.first() == Some(&"shorts"))
        || host == "youtu.be"
    {
        return SourceType::YoutubeVideo;
    }
    match hint {
        "product_page" | "landing_page" | "web_page" => SourceType::WebPage,
        "article" | "blog" | "web_article" => SourceType::WebArticle,
        _ => {
            let looks_like_article = segs.iter().any(|s| matches!(*s, "blog" | "news" | "article" | "articles" | "posts" | "post" | "docs" | "stories"))
                || segs.last().is_some_and(|s| s.contains('-') && s.len() > 12);
            if looks_like_article || segs.len() >= 2 {
                SourceType::WebArticle
            } else {
                SourceType::WebPage
            }
        }
    }
}

pub fn youtube_video_id(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    if host == "youtu.be" {
        return url.path_segments()?.next().map(str::to_owned).filter(|s| !s.is_empty());
    }
    let mut segs = url.path_segments()?;
    match segs.next()? {
        "watch" => url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned()),
        "shorts" => segs.next().map(str::to_owned),
        _ => None,
    }
}

pub fn x_post_id(url: &Url) -> Option<String> {
    let segs: Vec<_> = url.path_segments()?.collect();
    (segs.len() >= 3 && segs[1] == "status").then(|| segs[2].chars().take_while(|c| c.is_ascii_digit()).collect::<String>()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> UrlPolicy {
        UrlPolicy::default()
    }

    #[test]
    fn rejects_bad_schemes_and_credentials() {
        for u in ["ftp://example.com/x", "file:///etc/passwd", "javascript:alert(1)", "https://user:pw@example.com/"] {
            let e = parse_and_normalize(u, &p()).unwrap_err();
            assert!(matches!(e.code, ErrorCode::UrlNotAllowed | ErrorCode::ValidationError), "{u}");
        }
    }

    #[test]
    fn rejects_internal_targets() {
        for u in [
            "https://localhost/",
            "https://127.0.0.1/",
            "https://10.1.2.3/",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/",
            "https://metadata.google.internal/",
            "https://foo.internal/",
            "https://example.com:8443/",
            "https://192.168.0.1/",
            "https://[fd00::1]/",
        ] {
            assert_eq!(parse_and_normalize(u, &p()).unwrap_err().code, ErrorCode::UrlNotAllowed, "{u}");
        }
    }

    #[test]
    fn http_only_when_enabled() {
        assert!(parse_and_normalize("http://example.com/a", &p()).is_err());
        let mut pol = p();
        pol.allow_http = true;
        assert!(parse_and_normalize("http://example.com/a", &pol).is_ok());
    }

    #[test]
    fn normalizes_tracking_fragment_and_case() {
        let u = parse_and_normalize("HTTPS://Example.COM:443/Blog/Post?utm_source=x&b=2&a=1&fbclid=z#frag", &p()).unwrap();
        assert_eq!(u.as_str(), "https://example.com/Blog/Post?a=1&b=2");
    }

    #[test]
    fn deny_and_allow_lists() {
        let mut pol = p();
        pol.denylist_hosts = vec!["evil.com".into()];
        assert!(parse_and_normalize("https://sub.evil.com/", &pol).is_err());
        pol.denylist_hosts.clear();
        pol.allowlist_hosts = Some(vec!["good.com".into()]);
        assert!(parse_and_normalize("https://good.com/x", &pol).is_ok());
        assert!(parse_and_normalize("https://other.com/x", &pol).is_err());
    }

    #[test]
    fn detects_source_types() {
        let t = |u: &str| detect_source_type(&Url::parse(u).unwrap(), "auto");
        assert_eq!(t("https://github.com/rust-lang/rust"), SourceType::GithubRepository);
        assert_eq!(t("https://github.com/rust-lang/rust/issues/1"), SourceType::GithubIssue);
        assert_eq!(t("https://github.com/rust-lang/rust/pull/2"), SourceType::GithubPullRequest);
        assert_eq!(t("https://github.com/rust-lang/rust/blob/master/README.md"), SourceType::GithubFile);
        assert_eq!(t("https://x.com/jack/status/20"), SourceType::XPost);
        assert_eq!(t("https://www.youtube.com/watch?v=abc"), SourceType::YoutubeVideo);
        assert_eq!(t("https://youtu.be/abc"), SourceType::YoutubeVideo);
        assert_eq!(t("https://example.com/blog/my-first-post"), SourceType::WebArticle);
        assert_eq!(t("https://example.com/"), SourceType::WebPage);
        assert_eq!(detect_source_type(&Url::parse("https://example.com/p/1").unwrap(), "product_page"), SourceType::WebPage);
    }

    #[test]
    fn extracts_ids() {
        assert_eq!(youtube_video_id(&Url::parse("https://youtu.be/abc123").unwrap()).as_deref(), Some("abc123"));
        assert_eq!(youtube_video_id(&Url::parse("https://www.youtube.com/watch?v=zzz").unwrap()).as_deref(), Some("zzz"));
        assert_eq!(x_post_id(&Url::parse("https://x.com/a/status/12345?s=20").unwrap()).as_deref(), Some("12345"));
    }
}
