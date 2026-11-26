//! Minimal RFC 9309 robots.txt support (spec §10: "respect robots directives").

use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::ssrf::SafeFetcher;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use url::Url;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rules {
    /// (is_allow, path_prefix)
    entries: Vec<(bool, String)>,
}

impl Rules {
    /// Parse the group that applies to `agent` (falling back to `*`).
    pub fn parse(body: &str, agent: &str) -> Self {
        let agent = agent.to_ascii_lowercase();
        type Group = (Vec<String>, Vec<(bool, String)>);
        let mut groups: Vec<Group> = vec![];
        let mut cur_agents: Vec<String> = vec![];
        let mut cur_rules: Vec<(bool, String)> = vec![];
        let mut in_rules = false;
        for line in body.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once(':') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
            match k.as_str() {
                "user-agent" => {
                    if in_rules {
                        groups.push((std::mem::take(&mut cur_agents), std::mem::take(&mut cur_rules)));
                        in_rules = false;
                    }
                    cur_agents.push(v.to_ascii_lowercase());
                }
                "allow" | "disallow" => {
                    in_rules = true;
                    if !v.is_empty() || k == "allow" {
                        cur_rules.push((k == "allow", v));
                    }
                }
                _ => {}
            }
        }
        groups.push((cur_agents, cur_rules));
        let pick = |name: &str| groups.iter().find(|(a, _)| a.iter().any(|x| x == name || (name != "*" && agent.contains(x.as_str()) && x != "*")));
        let chosen = pick(&agent).or_else(|| pick("*"));
        Self { entries: chosen.map(|(_, r)| r.clone()).unwrap_or_default() }
    }

    /// Longest-match wins; Allow beats Disallow on ties.
    pub fn allows(&self, path_and_query: &str) -> bool {
        let mut best: Option<(usize, bool)> = None;
        for (allow, pat) in &self.entries {
            if pat.is_empty() {
                continue;
            }
            if matches_pattern(pat, path_and_query) {
                let len = pat.len();
                best = match best {
                    Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                    _ => Some((len, *allow)),
                };
            }
        }
        best.map(|(_, a)| a).unwrap_or(true)
    }
}

fn matches_pattern(pat: &str, path: &str) -> bool {
    let anchored = pat.ends_with('$');
    let pat = pat.trim_end_matches('$');
    let parts: Vec<&str> = pat.split('*').collect();
    let mut pos = 0usize;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !path.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if let Some(idx) = path[pos..].find(part) {
            pos += idx + part.len();
        } else {
            return false;
        }
    }
    !anchored || pos == path.len() || parts.last().is_some_and(|l| l.is_empty())
}

pub struct RobotsChecker {
    fetcher: SafeFetcher,
    agent: String,
    ttl: Duration,
    cache: Mutex<HashMap<String, (Instant, Rules)>>,
}

impl RobotsChecker {
    pub fn new(fetcher: SafeFetcher, agent: &str) -> Self {
        Self { fetcher, agent: agent.to_string(), ttl: Duration::from_secs(3600), cache: Mutex::new(HashMap::new()) }
    }

    pub async fn ensure_allowed(&self, url: &Url) -> ApiResult<()> {
        let origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
        let key = match url.port() {
            Some(p) => format!("{origin}:{p}"),
            None => origin.clone(),
        };
        let cached = self.cache.lock().unwrap().get(&key).filter(|(t, _)| t.elapsed() < self.ttl).map(|(_, r)| r.clone());
        let rules = match cached {
            Some(r) => r,
            None => {
                let robots_url = url.join("/robots.txt").map_err(ApiError::internal)?;
                let rules = match self.fetcher.fetch(&robots_url).await {
                    Ok(p) if p.status == 200 => Rules::parse(&String::from_utf8_lossy(&p.body), &self.agent),
                    // 4xx: no robots file → everything allowed (RFC 9309 §2.3.1.3).
                    Ok(p) if (400..500).contains(&p.status) => Rules::default(),
                    // 5xx: treat as temporarily unavailable; caller may retry.
                    Ok(p) => return Err(ApiError::new(ErrorCode::ExtractionFailed, format!("robots.txt unavailable ({})", p.status)).retryable(true)),
                    Err(e) if e.code == ErrorCode::UrlNotAllowed => return Err(e),
                    Err(e) => return Err(e.retryable(true)),
                };
                self.cache.lock().unwrap().insert(key, (Instant::now(), rules.clone()));
                rules
            }
        };
        let target = match url.query() {
            Some(q) => format!("{}?{}", url.path(), q),
            None => url.path().to_string(),
        };
        if rules.allows(&target) {
            Ok(())
        } else {
            Err(ApiError::new(ErrorCode::UrlNotAllowed, "robots.txt disallows fetching this URL").with_details(serde_json::json!({ "url": url.as_str() })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disallow_and_allow_precedence() {
        let r = Rules::parse("User-agent: *\nDisallow: /private\nAllow: /private/public\nDisallow: /*.pdf$\n", "bot");
        assert!(r.allows("/blog"));
        assert!(!r.allows("/private/x"));
        assert!(r.allows("/private/public/page"));
        assert!(!r.allows("/files/a.pdf"));
        assert!(r.allows("/files/a.pdf.html"));
    }

    #[test]
    fn specific_agent_group_wins() {
        let body = "User-agent: *\nDisallow: /\n\nUser-agent: letwemarketcontentagent\nDisallow: /admin\n";
        let r = Rules::parse(body, "LetWeMarketContentAgent");
        assert!(r.allows("/blog"));
        assert!(!r.allows("/admin/x"));
        assert!(!Rules::parse(body, "otherbot").allows("/blog"));
    }

    #[test]
    fn empty_disallow_allows_all() {
        assert!(Rules::parse("User-agent: *\nDisallow:\n", "bot").allows("/anything"));
    }
}
