//! Usage and credit-accounting hook (spec §5). LetWeMarket owns billing/quotas; this service emits
//! one event per tool invocation. Delivery is best-effort and never blocks or fails the tool call.

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

#[derive(Debug, Clone, Serialize)]
pub struct UsageEvent {
    pub tenant_id: String,
    pub brand_id: String,
    pub actor_id: String,
    pub tool: String,
    pub request_id: String,
    pub trace_id: String,
    pub status: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

#[async_trait]
pub trait UsageSink: Send + Sync {
    async fn record(&self, ev: UsageEvent);
}

pub struct TracingUsageSink;

#[async_trait]
impl UsageSink for TracingUsageSink {
    async fn record(&self, ev: UsageEvent) {
        tracing::info!(target: "usage", tenant_id = %ev.tenant_id, brand_id = %ev.brand_id, tool = %ev.tool, status = %ev.status,
            input_tokens = ev.input_tokens, output_tokens = ev.output_tokens, cost_usd = ev.cost_usd, "usage");
    }
}

/// POSTs each event to LetWeMarket, signed with HMAC-SHA256 so the receiver can authenticate it.
pub struct WebhookUsageSink {
    pub url: String,
    pub secret: String,
    pub http: reqwest::Client,
}

#[async_trait]
impl UsageSink for WebhookUsageSink {
    async fn record(&self, ev: UsageEvent) {
        let body = match serde_json::to_vec(&ev) {
            Ok(b) => b,
            Err(_) => return,
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(self.secret.as_bytes()).expect("hmac key");
        mac.update(&body);
        let sig = hex::encode(mac.finalize().into_bytes());
        let res = self.http.post(&self.url).header("content-type", "application/json").header("x-usage-signature", sig).body(body).send().await;
        if let Err(e) = res {
            tracing::warn!(error = %e.without_url(), "usage webhook delivery failed");
        }
    }
}
