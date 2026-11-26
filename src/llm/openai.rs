//! OpenAI-compatible chat-completions provider (works with OpenAI, Azure-compatible gateways, vLLM, etc.).

use super::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct OpenAiProvider {
    base_url: String,
    api_key: String,
    model: String,
    /// USD per million tokens (input, output) for cost accounting.
    price_per_mtok: (f64, f64),
    http: reqwest::Client,
}

impl OpenAiProvider {
    pub fn new(base_url: &str, api_key: &str, model: &str, price_per_mtok: (f64, f64), timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            price_per_mtok,
            http: reqwest::Client::builder().timeout(timeout).build()?,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = json!({
            "model": self.model,
            "temperature": req.temperature,
            "max_tokens": req.max_output_tokens,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": req.system },
                { "role": "user", "content": req.user.to_string() }
            ]
        });
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transient(e.without_url().to_string()))?;
        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(LlmError::RateLimited);
        }
        if status.is_server_error() {
            return Err(LlmError::Transient(format!("status {status}")));
        }
        if !status.is_success() {
            return Err(LlmError::Permanent(format!("status {status}")));
        }
        let v: Value = resp.json().await.map_err(|e| LlmError::Transient(format!("bad body: {e}")))?;
        let text = v.pointer("/choices/0/message/content").and_then(Value::as_str).ok_or_else(|| LlmError::Permanent("empty completion".into()))?;
        let (i, o) = (
            v.pointer("/usage/prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            v.pointer("/usage/completion_tokens").and_then(Value::as_u64).unwrap_or(0),
        );
        Ok(LlmResponse {
            text: text.to_string(),
            provider: "openai".into(),
            model: v.get("model").and_then(Value::as_str).unwrap_or(&self.model).to_string(),
            input_tokens: i,
            output_tokens: o,
            cost_usd: (i as f64 * self.price_per_mtok.0 + o as f64 * self.price_per_mtok.1) / 1e6,
            latency_ms: 0,
        })
    }
}
