//! SSRF protection (spec §13): DNS-resolved IP checks, pinned connections,
//! manual redirect following with re-validation, size and time limits.

use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::urls::{parse_and_normalize, UrlPolicy};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use url::Url;

pub fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => forbidden_v4(v4),
        IpAddr::V6(v6) => forbidden_v6(v6),
    }
}

fn forbidden_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local() // 169.254/16 incl. cloud metadata
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || (o[0] == 100 && (64..=127).contains(&o[1])) // CGNAT 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0/24
        || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
        || o[0] >= 240 // reserved
        || o[0] == 0
}

fn forbidden_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return forbidden_v4(v4);
    }
    let seg = ip.segments();
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || (seg[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        || (seg[0] == 0x2001 && seg[1] == 0x0db8) // documentation
        || (seg[0] == 0x0064 && seg[1] == 0xff9b) // NAT64 — may embed internal v4
        || seg[0] == 0x0100 && seg[1] == 0 // discard prefix
}

#[derive(Debug, Clone)]
pub struct FetchLimits {
    pub timeout: Duration,
    pub max_redirects: usize,
    pub max_body_bytes: usize,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self { timeout: Duration::from_secs(15), max_redirects: 5, max_body_bytes: 5 * 1024 * 1024 }
    }
}

#[derive(Debug)]
pub struct FetchedPage {
    pub final_url: Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct SafeFetcher {
    pub policy: UrlPolicy,
    pub limits: FetchLimits,
    pub user_agent: String,
    pub proxy: Option<String>,
}

impl SafeFetcher {
    pub fn new(policy: UrlPolicy, limits: FetchLimits) -> Self {
        Self { policy, limits, user_agent: "LetWeMarketContentAgent/1.0 (+internal)".into(), proxy: None }
    }

    /// Resolve DNS and make sure every answer is a public address.
    pub async fn resolve_public(&self, url: &Url) -> ApiResult<Vec<SocketAddr>> {
        let host = url.host_str().ok_or_else(|| ApiError::validation("url has no host"))?;
        let port = url.port_or_known_default().unwrap_or(443);
        let addrs: Vec<SocketAddr> = match url.host() {
            Some(url::Host::Ipv4(ip)) => vec![SocketAddr::new(ip.into(), port)],
            Some(url::Host::Ipv6(ip)) => vec![SocketAddr::new(ip.into(), port)],
            _ => tokio::time::timeout(self.limits.timeout, tokio::net::lookup_host((host, port)))
                .await
                .map_err(|_| ApiError::new(ErrorCode::ExtractionFailed, "dns resolution timed out").retryable(true))?
                .map_err(|e| ApiError::new(ErrorCode::ExtractionFailed, format!("dns resolution failed: {e}")))?
                .collect(),
        };
        if addrs.is_empty() {
            return Err(ApiError::new(ErrorCode::ExtractionFailed, "host did not resolve"));
        }
        if !self.policy.private_networks_allowed() {
            // Reject if ANY answer is non-public: prevents mixed-record rebinding tricks.
            if let Some(bad) = addrs.iter().find(|a| is_forbidden_ip(a.ip())) {
                return Err(ApiError::new(ErrorCode::UrlNotAllowed, "host resolves to a non-public address")
                    .with_details(serde_json::json!({ "host": host, "ip": bad.ip().to_string() })));
            }
        }
        Ok(addrs)
    }

    /// Validate-then-fetch with the connection pinned to the validated address
    /// (defeats DNS rebinding) and redirects followed manually with re-validation.
    pub async fn fetch(&self, start: &Url) -> ApiResult<FetchedPage> {
        let mut current = start.clone();
        for hop in 0..=self.limits.max_redirects {
            let addrs = self.resolve_public(&current).await?;
            let host = current.host_str().unwrap_or_default().to_string();
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(self.limits.timeout)
                .user_agent(&self.user_agent)
                .resolve(&host, addrs[0]);
            if let Some(p) = &self.proxy {
                builder = builder.proxy(reqwest::Proxy::all(p).map_err(ApiError::internal)?);
            }
            let client = builder.build().map_err(ApiError::internal)?;
            let mut resp = client.get(current.clone()).send().await.map_err(|e| {
                let retryable = e.is_timeout() || e.is_connect();
                ApiError::new(ErrorCode::ExtractionFailed, format!("request failed: {}", e.without_url())).retryable(retryable)
            })?;

            let status = resp.status();
            if status.is_redirection() {
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| ApiError::new(ErrorCode::ExtractionFailed, "redirect without location"))?;
                if hop == self.limits.max_redirects {
                    return Err(ApiError::new(ErrorCode::ExtractionFailed, "too many redirects"));
                }
                let next = current.join(loc).map_err(|e| ApiError::new(ErrorCode::ExtractionFailed, format!("bad redirect: {e}")))?;
                // Re-run full URL policy on every hop.
                current = parse_and_normalize(next.as_str(), &self.policy)?;
                continue;
            }

            let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_owned);
            if let Some(len) = resp.content_length() {
                if len as usize > self.limits.max_body_bytes {
                    return Err(ApiError::new(ErrorCode::ExtractionFailed, "response too large"));
                }
            }
            let mut body = Vec::new();
            while let Some(chunk) =
                resp.chunk().await.map_err(|e| ApiError::new(ErrorCode::ExtractionFailed, format!("body read failed: {}", e.without_url())).retryable(true))?
            {
                if body.len() + chunk.len() > self.limits.max_body_bytes {
                    return Err(ApiError::new(ErrorCode::ExtractionFailed, "response too large"));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(FetchedPage { final_url: current, status: status.as_u16(), content_type, body });
        }
        Err(ApiError::new(ErrorCode::ExtractionFailed, "too many redirects"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_non_public_ranges() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.5.5",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "ff02::1",
        ] {
            assert!(is_forbidden_ip(ip.parse().unwrap()), "{ip} should be blocked");
        }
    }

    #[test]
    fn allows_public_addresses() {
        for ip in ["1.1.1.1", "8.8.8.8", "93.184.216.34", "2606:4700:4700::1111", "::ffff:8.8.8.8"] {
            assert!(!is_forbidden_ip(ip.parse().unwrap()), "{ip} should be allowed");
        }
    }

    #[tokio::test]
    async fn resolve_rejects_literal_private_ip() {
        let f = SafeFetcher::new(UrlPolicy::default(), FetchLimits::default());
        let url = Url::parse("https://127.0.0.1/").unwrap();
        assert_eq!(f.resolve_public(&url).await.unwrap_err().code, ErrorCode::UrlNotAllowed);
    }
}
