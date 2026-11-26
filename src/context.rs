//! Request context (spec §5): every call carries tenant/brand/actor and the
//! permissions LetWeMarket granted. The service enforces them; it never invents them.

use crate::error::{ApiError, ApiResult, ErrorCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    Draft,
    Publish,
    Schedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub allowed_channels: Vec<String>,
    #[serde(default)]
    pub allowed_actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectedAccount {
    pub platform: String,
    pub account_id: String,
    pub access: Access,
    /// When present, must match the request's tenant/brand (spec §7.2).
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub brand_id: Option<String>,
}

/// Limited Zernio connection context. Contains identifiers only — never OAuth tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZernioConnection {
    pub zernio_profile_id: String,
    #[serde(default)]
    pub connected_accounts: Vec<ConnectedAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestContext {
    pub tenant_id: String,
    pub brand_id: String,
    pub actor_id: String,
    pub request_id: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default = "default_source")]
    pub source: String,
    pub permissions: Permissions,
    #[serde(default)]
    pub zernio: Option<ZernioConnection>,
    /// Channels enabled by the customer's plan (LetWeMarket-owned). `None` = no plan restriction.
    #[serde(default)]
    pub plan_channels: Option<Vec<String>>,
    /// Remaining credits/quota as computed by LetWeMarket. `Some(n <= 0)` blocks the call.
    #[serde(default)]
    pub quota_remaining: Option<i64>,
}

fn default_source() -> String {
    "letwemarket_agent".into()
}

const MAX_ID_LEN: usize = 128;

fn check_id(field: &str, v: &str, code: ErrorCode) -> ApiResult<()> {
    if v.trim().is_empty() {
        return Err(ApiError::new(code, format!("{field} is required")));
    }
    if v.len() > MAX_ID_LEN || v.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ApiError::validation(format!("{field} is malformed")));
    }
    Ok(())
}

/// Tenant + brand pair that scopes every storage query (tenant & brand isolation).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    pub tenant_id: String,
    pub brand_id: String,
}

impl RequestContext {
    pub fn validate(&self) -> ApiResult<()> {
        check_id("tenant_id", &self.tenant_id, ErrorCode::TenantNotFound)?;
        check_id("brand_id", &self.brand_id, ErrorCode::BrandNotFound)?;
        check_id("actor_id", &self.actor_id, ErrorCode::ValidationError)?;
        check_id("request_id", &self.request_id, ErrorCode::ValidationError)?;
        if let Some(k) = &self.idempotency_key {
            check_id("idempotency_key", k, ErrorCode::ValidationError)?;
        }
        if let Some(z) = &self.zernio {
            for a in &z.connected_accounts {
                if a.tenant_id.as_deref().is_some_and(|t| t != self.tenant_id) || a.brand_id.as_deref().is_some_and(|b| b != self.brand_id) {
                    return Err(ApiError::forbidden("connected account does not belong to this tenant/brand"));
                }
            }
        }
        Ok(())
    }

    pub fn scope(&self) -> Scope {
        Scope { tenant_id: self.tenant_id.clone(), brand_id: self.brand_id.clone() }
    }

    pub fn require_tool(&self, tool: &str) -> ApiResult<()> {
        if self.permissions.allowed_tools.iter().any(|t| t == tool || t == "*") {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!("tool '{tool}' is not permitted")).with_details(serde_json::json!({ "tool": tool })))
        }
    }

    pub fn require_action(&self, action: Action) -> ApiResult<()> {
        if self.permissions.allowed_actions.contains(&action) {
            Ok(())
        } else {
            Err(ApiError::forbidden(format!("action '{action:?}' is not permitted")).with_details(serde_json::json!({ "action": action })))
        }
    }

    /// A channel is usable only if both the permission grant and the plan allow it.
    pub fn require_channel(&self, channel: &str) -> ApiResult<()> {
        let granted = self.permissions.allowed_channels.iter().any(|c| c == channel || c == "*");
        if !granted {
            return Err(ApiError::forbidden(format!("channel '{channel}' is not permitted")).with_details(serde_json::json!({ "channel": channel })));
        }
        if let Some(plan) = &self.plan_channels {
            if !plan.iter().any(|c| c == channel) {
                return Err(ApiError::forbidden(format!("channel '{channel}' is not enabled for the customer plan"))
                    .with_details(serde_json::json!({ "channel": channel })));
            }
        }
        Ok(())
    }

    pub fn require_quota(&self) -> ApiResult<()> {
        match self.quota_remaining {
            Some(n) if n <= 0 => Err(ApiError::new(ErrorCode::QuotaExceeded, "usage quota exhausted for this brand")),
            _ => Ok(()),
        }
    }
}

pub fn is_known_channel(c: &str) -> bool {
    matches!(c, "instagram" | "facebook" | "x" | "twitter" | "linkedin" | "tiktok" | "youtube" | "threads" | "pinterest" | "bluesky")
}
