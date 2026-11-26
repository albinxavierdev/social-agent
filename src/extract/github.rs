//! GitHub API extractor: repository (metadata + README), file, issue, pull request.

use super::html::{lead_summary, truncate_chars};
use super::{ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest};
use crate::urls::SourceType;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct GitHubExtractor {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl GitHubExtractor {
    pub fn new(base_url: &str, token: Option<String>, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::builder().timeout(timeout).user_agent("LetWeMarketContentAgent/1.0").build()?,
        })
    }

    async fn get(&self, path: &str, accept: &str) -> Result<reqwest::Response, ExtractionError> {
        let mut r = self.http.get(format!("{}{}", self.base_url, path)).header("accept", accept).header("x-github-api-version", "2022-11-28");
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        let resp = r.send().await.map_err(|e| ExtractionError::failed(format!("github request failed: {}", e.without_url()), true))?;
        let st = resp.status();
        if st.is_success() {
            return Ok(resp);
        }
        let rate_limited = st.as_u16() == 429 || (st.as_u16() == 403 && resp.headers().get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()) == Some("0"));
        Err(match st.as_u16() {
            404 => ExtractionError::failed("github resource not found (or private)", false),
            _ if rate_limited => ExtractionError::failed("github rate limited", true),
            401 | 403 => ExtractionError::failed(format!("github access denied ({st})"), false),
            _ => ExtractionError::failed(format!("github returned {st}"), st.is_server_error()),
        })
    }

    async fn json(&self, path: &str) -> Result<Value, ExtractionError> {
        self.get(path, "application/vnd.github+json").await?.json().await.map_err(|e| ExtractionError::failed(format!("github bad json: {e}"), false))
    }

    async fn raw(&self, path: &str) -> Result<String, ExtractionError> {
        self.get(path, "application/vnd.github.raw+json").await?.text().await.map_err(|e| ExtractionError::failed(format!("github read failed: {e}"), true))
    }
}

fn st(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned)
}

#[async_trait]
impl ContentExtractor for GitHubExtractor {
    fn name(&self) -> &'static str {
        "github"
    }

    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        let segs: Vec<String> = req.url.path_segments().map(|s| s.filter(|x| !x.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
        if segs.len() < 2 {
            return Err(ExtractionError::Unsupported("not a repository URL".into()));
        }
        let (owner, repo) = (&segs[0], segs[1].trim_end_matches(".git"));
        let repo_path = format!("/repos/{owner}/{repo}");

        let (title, author, published, text, kind, extra) = match req.source_type {
            SourceType::GithubRepository => {
                let meta = self.json(&repo_path).await?;
                let readme = self.raw(&format!("{repo_path}/readme")).await.unwrap_or_default();
                let topics = meta.get("topics").cloned().unwrap_or(json!([]));
                let header = format!(
                    "{}\n\n{}\nLanguage: {}\nStars: {}\nTopics: {}\n\n{}",
                    st(&meta, "full_name").unwrap_or_default(),
                    st(&meta, "description").unwrap_or_default(),
                    st(&meta, "language").unwrap_or_default(),
                    meta.get("stargazers_count").and_then(Value::as_u64).unwrap_or(0),
                    topics,
                    readme
                );
                (
                    st(&meta, "full_name"),
                    meta.pointer("/owner/login").and_then(Value::as_str).map(str::to_owned),
                    st(&meta, "pushed_at"),
                    header,
                    "repository",
                    json!({ "stars": meta.get("stargazers_count"), "language": meta.get("language"), "license": meta.pointer("/license/spdx_id"), "topics": topics }),
                )
            }
            SourceType::GithubFile => {
                // /owner/repo/(blob|tree)/<ref>/<path...>
                if segs.len() < 5 {
                    return Err(ExtractionError::Unsupported("file URL missing path".into()));
                }
                let git_ref = &segs[3];
                let path = segs[4..].join("/");
                let body = self.raw(&format!("{repo_path}/contents/{path}?ref={git_ref}")).await?;
                (Some(path.clone()), Some(owner.clone()), None, body, "file", json!({ "path": path, "ref": git_ref }))
            }
            SourceType::GithubIssue | SourceType::GithubPullRequest => {
                let n = segs.get(3).ok_or_else(|| ExtractionError::Unsupported("missing number".into()))?;
                let is_pr = req.source_type == SourceType::GithubPullRequest;
                let obj = self.json(&format!("{repo_path}/{}/{n}", if is_pr { "pulls" } else { "issues" })).await?;
                let body = format!("{}\n\n{}", st(&obj, "title").unwrap_or_default(), st(&obj, "body").unwrap_or_default());
                (
                    st(&obj, "title"),
                    obj.pointer("/user/login").and_then(Value::as_str).map(str::to_owned),
                    st(&obj, "created_at"),
                    body,
                    if is_pr { "pull_request" } else { "issue" },
                    json!({ "state": obj.get("state"), "number": obj.get("number"), "comments": obj.get("comments") }),
                )
            }
            _ => return Err(ExtractionError::Unsupported("not a GitHub source".into())),
        };
        if text.trim().is_empty() {
            return Err(ExtractionError::failed("github resource has no text", false));
        }
        let text = truncate_chars(&text, 120_000);
        Ok(ExtractedContent {
            title,
            author,
            published_at: published.and_then(|d| chrono::DateTime::parse_from_rfc3339(&d).ok()).map(|d| d.with_timezone(&chrono::Utc)),
            content_type: format!("github_{kind}"),
            language: "und".into(),
            summary: lead_summary(&text, None),
            text,
            metadata: ExtractedMetadata { canonical_url: Some(req.url.to_string()), extra, ..Default::default() },
            screenshot_url: None,
            extraction_method: "github_api".into(),
            extraction_status: "success".into(),
        })
    }
}
