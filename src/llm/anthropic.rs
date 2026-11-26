//! Anthropic Messages API provider.

use super::{LlmError, LlmProvider, LlmRequest, LlmResponse};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    model: String,
    price_per_mtok: (f64, f64),
    http: reqwest::Client,
}

impl AnthropicProvider {
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
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }
    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = json!({
            "model": self.model,
            "max_tokens": req.max_output_tokens,
            "temperature": req.temperature,
            "system": req.system,
            "messages": [{ "role": "user", "content": req.user.to_string() }]
        });
        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transient(e.without_url().to_string()))?;
        let status = resp.status();
        if status.as_u16() == 429 || status.as_u16() == 529 {
            return Err(LlmError::RateLimited);
        }
        if status.is_server_error() {
            return Err(LlmError::Transient(format!("status {status}")));
        }
        if !status.is_success() {
            return Err(LlmError::Permanent(format!("status {status}")));
        }
        let v: Value = resp.json().await.map_err(|e| LlmError::Transient(format!("bad body: {e}")))?;
        let text: String = v
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        if text.is_empty() {
            return Err(LlmError::Permanent("empty completion".into()));
        }
        let (i, o) =
            (v.pointer("/usage/input_tokens").and_then(Value::as_u64).unwrap_or(0), v.pointer("/usage/output_tokens").and_then(Value::as_u64).unwrap_or(0));
        Ok(LlmResponse {
            text,
            provider: "anthropic".into(),
            model: v.get("model").and_then(Value::as_str).unwrap_or(&self.model).to_string(),
            input_tokens: i,
            output_tokens: o,
            cost_usd: (i as f64 * self.price_per_mtok.0 + o as f64 * self.price_per_mtok.1) / 1e6,
            latency_ms: 0,
        })
    }
}
