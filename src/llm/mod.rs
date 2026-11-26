//! Provider-agnostic LLM layer (spec §11): retries with backoff, token limits,
//! usage/cost capture, prompt+model version stamping, and log redaction.

pub mod anthropic;
pub mod mock;
pub mod openai;

use crate::error::{ApiError, ErrorCode};
use crate::model::ModelInfo;
use crate::queue::backoff_delay;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct LlmRequest {
    /// Stable id of the task, e.g. "report", "social_draft" — also selects mock behaviour.
    pub purpose: &'static str,
    /// Bump when the system prompt changes; persisted with every artifact.
    pub prompt_version: &'static str,
    pub system: String,
    /// JSON document with the inputs. Source text is data, never instructions.
    pub user: Value,
    pub max_output_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    pub text: String,
    pub provider: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LlmError {
    #[error("transient provider error: {0}")]
    Transient(String),
    #[error("provider rate limited")]
    RateLimited,
    #[error("provider error: {0}")]
    Permanent(String),
}

impl LlmError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::RateLimited)
    }
}

#[async_trait]
pub trait LlmProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn model(&self) -> &str;
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
}

#[derive(Debug, Clone)]
pub struct LlmLimits {
    pub max_input_chars: usize,
    pub max_output_tokens: u32,
    pub max_attempts: u32,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
}

impl Default for LlmLimits {
    fn default() -> Self {
        Self {
            max_input_chars: 400_000,
            max_output_tokens: 4096,
            max_attempts: 4,
            backoff_base: Duration::from_millis(500),
            backoff_cap: Duration::from_secs(20),
        }
    }
}

pub struct LlmClient {
    provider: Arc<dyn LlmProvider>,
    pub limits: LlmLimits,
}

/// Best-effort secret scrubbing for anything we log about prompts or provider errors.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for tok in s.split_inclusive(char::is_whitespace) {
        let t = tok.trim();
        let looks_secret = t.starts_with("sk-")
            || t.starts_with("fc-")
            || t.starts_with("ghp_")
            || t.starts_with("github_pat_")
            || t.starts_with("Bearer")
            || t.starts_with("xoxb-")
            || (t.len() > 32 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        if looks_secret {
            out.push_str("[REDACTED]");
            if tok.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else {
            out.push_str(tok);
        }
    }
    regex_lite_emails(&out)
}

fn regex_lite_emails(s: &str) -> String {
    s.split_inclusive(char::is_whitespace)
        .map(|tok| {
            let t = tok.trim();
            if t.contains('@') && t.contains('.') && !t.starts_with('@') {
                tok.replace(t, "[EMAIL]")
            } else {
                tok.to_string()
            }
        })
        .collect()
}

impl LlmClient {
    pub fn new(provider: Arc<dyn LlmProvider>, limits: LlmLimits) -> Self {
        Self { provider, limits }
    }

    pub fn provider_name(&self) -> &'static str {
        self.provider.name()
    }

    pub async fn complete(&self, mut req: LlmRequest) -> Result<LlmResponse, ApiError> {
        req.max_output_tokens = req.max_output_tokens.min(self.limits.max_output_tokens);
        let size = req.system.len() + req.user.to_string().len();
        if size > self.limits.max_input_chars {
            return Err(ApiError::validation(format!("input too large for LLM ({size} chars > {})", self.limits.max_input_chars)));
        }
        let mut attempt = 0u32;
        loop {
            let started = Instant::now();
            match self.provider.complete(&req).await {
                Ok(mut r) => {
                    r.latency_ms = started.elapsed().as_millis() as u64;
                    metrics::counter!("llm_requests_total", "provider" => self.provider.name(), "purpose" => req.purpose, "outcome" => "success").increment(1);
                    metrics::counter!("llm_tokens_total", "provider" => self.provider.name(), "kind" => "input").increment(r.input_tokens);
                    metrics::counter!("llm_tokens_total", "provider" => self.provider.name(), "kind" => "output").increment(r.output_tokens);
                    metrics::counter!("llm_cost_usd_micros_total", "provider" => self.provider.name()).increment((r.cost_usd * 1e6) as u64);
                    return Ok(r);
                }
                Err(e) => {
                    attempt += 1;
                    metrics::counter!("llm_requests_total", "provider" => self.provider.name(), "purpose" => req.purpose, "outcome" => "failure").increment(1);
                    if e.retryable() && attempt < self.limits.max_attempts {
                        metrics::counter!("llm_retries_total", "provider" => self.provider.name()).increment(1);
                        let d = backoff_delay(attempt, self.limits.backoff_base, self.limits.backoff_cap);
                        tracing::warn!(purpose = req.purpose, attempt, error = %redact(&e.to_string()), "llm call failed, retrying in {:?}", d);
                        tokio::time::sleep(d).await;
                        continue;
                    }
                    return Err(ApiError::new(ErrorCode::LlmProviderError, redact(&e.to_string())).retryable(e.retryable()));
                }
            }
        }
    }

    /// Run a request and parse the model's JSON answer into `T`. One repair retry on malformed JSON.
    pub async fn complete_json<T: DeserializeOwned>(&self, req: LlmRequest) -> Result<(T, ModelInfo), ApiError> {
        let mut total_in = 0;
        let mut total_out = 0;
        let mut cost = 0.0;
        let mut latency = 0;
        let mut req = req;
        let mut last_err = String::new();
        for round in 0..2 {
            if round == 1 {
                req.system.push_str("\n\nYour previous answer was not valid JSON for the required schema. Reply with ONLY the JSON object.");
            }
            let r = self.complete(req.clone()).await?;
            total_in += r.input_tokens;
            total_out += r.output_tokens;
            cost += r.cost_usd;
            latency += r.latency_ms;
            match parse_json::<T>(&r.text) {
                Ok(v) => {
                    let info = ModelInfo {
                        provider: r.provider,
                        model: r.model,
                        prompt_version: req.prompt_version.to_string(),
                        input_tokens: total_in,
                        output_tokens: total_out,
                        cost_usd: cost,
                        latency_ms: latency,
                    };
                    return Ok((v, info));
                }
                Err(e) => last_err = e,
            }
        }
        Err(ApiError::new(ErrorCode::LlmProviderError, format!("model returned malformed output: {last_err}")).retryable(true))
    }
}

/// Parse JSON that may be wrapped in a markdown fence or surrounded by prose.
pub fn parse_json<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<T>(t) {
        return Ok(v);
    }
    let unfenced = t.trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    if let Ok(v) = serde_json::from_str::<T>(unfenced) {
        return Ok(v);
    }
    if let (Some(a), Some(b)) = (t.find('{'), t.rfind('}')) {
        if a < b {
            return serde_json::from_str::<T>(&t[a..=b]).map_err(|e| e.to_string());
        }
    }
    Err("no JSON object found".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn parses_fenced_and_prose_wrapped_json() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct X {
            a: i32,
        }
        assert_eq!(parse_json::<X>("{\"a\":1}").unwrap(), X { a: 1 });
        assert_eq!(parse_json::<X>("```json\n{\"a\":2}\n```").unwrap(), X { a: 2 });
        assert_eq!(parse_json::<X>("Sure! Here you go: {\"a\":3} hope that helps").unwrap(), X { a: 3 });
        assert!(parse_json::<X>("nope").is_err());
    }

    #[test]
    fn redacts_secrets_and_emails() {
        let r = redact("key sk-abc123 and mail bob@example.com ok ghp_xxxxxxxx");
        assert!(!r.contains("sk-abc123") && !r.contains("bob@example.com") && !r.contains("ghp_xxxxxxxx"), "{r}");
    }

    struct Flaky {
        calls: AtomicU32,
        fail_first: u32,
        permanent: bool,
    }
    #[async_trait]
    impl LlmProvider for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        fn model(&self) -> &str {
            "m1"
        }
        async fn complete(&self, _: &LlmRequest) -> Result<LlmResponse, LlmError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first {
                return Err(if self.permanent { LlmError::Permanent("bad key".into()) } else { LlmError::Transient("503".into()) });
            }
            Ok(LlmResponse {
                text: "{\"ok\":true}".into(),
                provider: "flaky".into(),
                model: "m1".into(),
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            })
        }
    }

    fn req() -> LlmRequest {
        LlmRequest { purpose: "test", prompt_version: "v1", system: "s".into(), user: serde_json::json!({}), max_output_tokens: 100, temperature: 0.0 }
    }

    fn fast() -> LlmLimits {
        LlmLimits { backoff_base: Duration::from_millis(1), backoff_cap: Duration::from_millis(2), ..Default::default() }
    }

    #[tokio::test]
    async fn retries_transient_errors_then_succeeds() {
        let p = Arc::new(Flaky { calls: AtomicU32::new(0), fail_first: 2, permanent: false });
        let c = LlmClient::new(p.clone(), fast());
        assert!(c.complete(req()).await.is_ok());
        assert_eq!(p.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_permanent_errors() {
        let p = Arc::new(Flaky { calls: AtomicU32::new(0), fail_first: 5, permanent: true });
        let c = LlmClient::new(p.clone(), fast());
        let e = c.complete(req()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::LlmProviderError);
        assert!(!e.retryable);
        assert_eq!(p.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let p = Arc::new(Flaky { calls: AtomicU32::new(0), fail_first: 100, permanent: false });
        let c = LlmClient::new(p.clone(), fast());
        assert!(c.complete(req()).await.unwrap_err().retryable);
        assert_eq!(p.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn enforces_input_limit() {
        let p = Arc::new(Flaky { calls: AtomicU32::new(0), fail_first: 0, permanent: false });
        let c = LlmClient::new(p.clone(), LlmLimits { max_input_chars: 10, ..fast() });
        let mut r = req();
        r.user = serde_json::json!({ "text": "x".repeat(100) });
        assert_eq!(c.complete(r).await.unwrap_err().code, ErrorCode::ValidationError);
        assert_eq!(p.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn complete_json_stamps_versions() {
        let p = Arc::new(Flaky { calls: AtomicU32::new(0), fail_first: 0, permanent: false });
        let c = LlmClient::new(p, fast());
        let (v, info): (serde_json::Value, _) = c.complete_json(req()).await.unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!((info.provider.as_str(), info.model.as_str(), info.prompt_version.as_str()), ("flaky", "m1", "v1"));
        assert_eq!(info.input_tokens, 10);
    }
}
