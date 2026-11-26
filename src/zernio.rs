//! Zernio adapter (spec §7). Zernio is the ONLY system that touches connected accounts. This service
//! holds identifiers (profile / account ids) from LetWeMarket, never OAuth tokens, and authenticates to
//! Zernio with a single service API key that only exists in the environment/secret manager.
//!
//! API shape follows https://docs.zernio.com (POST /v1/posts with `platforms[]`, `scheduledFor`,
//! `publishNow`, `Idempotency-Key`). Read endpoints (analytics / inbox / comments) use the documented
//! resource families — verify exact paths against your Zernio plan before enabling them in production.

use crate::channels;
use crate::context::{Access, Action, ConnectedAccount, RequestContext};
use crate::error::{ApiError, ApiResult, ErrorCode};
use crate::model::{Draft, PublishAction, PublishStatus};
use crate::validate::compose_text;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

/// Zernio's platform slug for one of our channel names.
pub fn zernio_platform(channel: &str) -> String {
    match channels::canonical(channel).as_str() {
        "x" => "twitter".into(),
        c => c.into(),
    }
}

/// Validate that `channels` are connected through `profile_id` with at least `needed` access,
/// and that the channel is allowed by permissions + plan (spec §7.2, §7.3).
pub fn validate_connection<'a>(ctx: &'a RequestContext, profile_id: &str, channel_names: &[String], needed: Access) -> ApiResult<Vec<&'a ConnectedAccount>> {
    let z = ctx.zernio.as_ref().ok_or_else(|| {
        ApiError::new(ErrorCode::ZernioConnectionMissing, "no Zernio connection context was provided for this brand")
            .with_details(json!({ "brand_id": ctx.brand_id }))
    })?;
    if z.zernio_profile_id != profile_id {
        return Err(ApiError::new(ErrorCode::ZernioPermissionDenied, "zernio_profile_id does not match the brand's connection")
            .with_details(json!({ "brand_id": ctx.brand_id })));
    }
    let mut out = vec![];
    for ch in channel_names {
        let ch = channels::canonical(ch);
        ctx.require_channel(&ch)?;
        let acc = z.connected_accounts.iter().find(|a| channels::canonical(&a.platform) == ch).ok_or_else(|| {
            ApiError::new(ErrorCode::ZernioConnectionMissing, format!("No connected {ch} account is available for this brand."))
                .with_details(json!({ "brand_id": ctx.brand_id, "platform": ch }))
        })?;
        // A read connection can never publish, regardless of what else is true.
        if needed == Access::Write && acc.access != Access::Write {
            return Err(ApiError::new(ErrorCode::ZernioPermissionDenied, format!("the {ch} connection is read-only; publishing requires a write connection"))
                .with_details(json!({ "platform": ch, "access": acc.access })));
        }
        out.push(acc);
    }
    Ok(out)
}

/// Build the body for Zernio `POST /v1/posts`. Pure and deterministic (unit-tested).
pub fn build_post_payload(draft: &Draft, accounts: &[&ConnectedAccount], action: PublishAction, scheduled_at: Option<DateTime<Utc>>) -> ApiResult<Value> {
    let mut platforms = vec![];
    let mut media_items: Vec<Value> = vec![];
    for acc in accounts {
        let ch = channels::canonical(&acc.platform);
        let v = draft
            .content
            .variants
            .iter()
            .find(|v| channels::canonical(&v.channel) == ch)
            .ok_or_else(|| ApiError::validation(format!("draft has no variant for channel '{ch}'")))?;
        for m in &v.media {
            let item = json!({ "type": m.kind, "url": m.url });
            if !media_items.contains(&item) {
                media_items.push(item);
            }
        }
        let mut entry = json!({ "platform": zernio_platform(&ch), "accountId": acc.account_id, "customContent": compose_text(v) });
        if !v.parts.is_empty() {
            // Thread/carousel parts travel as platform-specific data; Zernio applies them per platform.
            entry["platformSpecificData"] = json!({ "parts": v.parts });
        }
        platforms.push(entry);
    }
    let mut body = json!({ "platforms": platforms, "mediaItems": media_items, "timezone": "UTC" });
    match action {
        PublishAction::Publish => body["publishNow"] = json!(true),
        PublishAction::Schedule => {
            let at = scheduled_at.ok_or_else(|| ApiError::validation("scheduled_at is required when action = schedule"))?;
            if at <= Utc::now() {
                return Err(ApiError::validation("scheduled_at must be in the future"));
            }
            body["scheduledFor"] = json!(at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
    }
    Ok(body)
}

pub fn action_required(a: PublishAction) -> Action {
    match a {
        PublishAction::Publish => Action::Publish,
        PublishAction::Schedule => Action::Schedule,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformResult {
    pub platform: String,
    pub status: String,
    pub url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitOutcome {
    pub post_id: Option<String>,
    pub status: PublishStatus,
    pub platforms: Vec<PlatformResult>,
    pub raw: Value,
}

/// Interpret Zernio's response, including partial success across channels (spec §7.4).
pub fn parse_submit_response(body: &Value) -> SubmitOutcome {
    let post = body.get("post").unwrap_or(body);
    let platforms: Vec<PlatformResult> = post
        .get("platforms")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|p| PlatformResult {
                    platform: p.get("platform").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                    status: p.get("status").and_then(Value::as_str).unwrap_or("unknown").to_string(),
                    url: p.get("platformPostUrl").and_then(Value::as_str).map(str::to_owned),
                    error: p.get("errorMessage").and_then(Value::as_str).map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    let failed = platforms.iter().filter(|p| matches!(p.status.as_str(), "failed" | "error")).count();
    let status = if platforms.is_empty() {
        match post.get("status").and_then(Value::as_str) {
            Some("failed") => PublishStatus::Failed,
            _ => PublishStatus::SubmittedToZernio,
        }
    } else if failed == platforms.len() {
        PublishStatus::Failed
    } else if failed > 0 {
        PublishStatus::PartialSuccess
    } else {
        PublishStatus::ScheduledOrPublished
    };
    SubmitOutcome { post_id: post.get("_id").or_else(|| post.get("id")).and_then(Value::as_str).map(str::to_owned), status, platforms, raw: body.clone() }
}

#[derive(Clone)]
pub struct ZernioClient {
    base_url: String,
    api_key: String,
    http: reqwest::Client,
}

impl ZernioClient {
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self { base_url: base_url.trim_end_matches('/').into(), api_key: api_key.into(), http: reqwest::Client::builder().timeout(timeout).build()? })
    }

    fn map_error(status: reqwest::StatusCode, body: &Value) -> ApiError {
        let code_str = body.get("code").or_else(|| body.get("error")).and_then(Value::as_str).unwrap_or_default().to_string();
        let msg = body.get("message").or_else(|| body.get("error")).and_then(Value::as_str).unwrap_or("zernio request failed").to_string();
        let details = json!({ "zernio_status": status.as_u16(), "zernio_code": code_str });
        match status.as_u16() {
            401 => {
                ApiError::new(ErrorCode::ZernioPermissionDenied, "Zernio rejected the credentials or the connection is invalid/expired").with_details(details)
            }
            403 => ApiError::new(ErrorCode::ZernioPermissionDenied, msg).with_details(details),
            402 => ApiError::new(ErrorCode::ZernioPublishFailed, "Zernio balance is insufficient for this publish").with_details(details),
            404 => ApiError::new(ErrorCode::ZernioConnectionMissing, "Zernio profile or account was not found").with_details(details),
            409 if code_str.contains("idempotency") => {
                ApiError::new(ErrorCode::ZernioPublishFailed, "an identical submission is still being processed").with_details(details).retryable(true)
            }
            409 if code_str.contains("conflict") || msg.to_lowercase().contains("conflict") => {
                ApiError::new(ErrorCode::ZernioPublishFailed, "scheduling conflict").with_details(details)
            }
            409 => ApiError::new(ErrorCode::ZernioPublishFailed, "duplicate content was rejected by Zernio").with_details(details),
            400 | 422 => {
                ApiError::new(ErrorCode::ZernioPublishFailed, format!("Zernio rejected the request (media or content validation): {msg}")).with_details(details)
            }
            429 => ApiError::new(ErrorCode::ZernioRateLimited, "Zernio rate limit reached").with_details(details),
            s if s >= 500 => ApiError::new(ErrorCode::ZernioPublishFailed, "Zernio is unavailable").with_details(details).retryable(true),
            _ => ApiError::new(ErrorCode::ZernioPublishFailed, msg).with_details(details),
        }
    }

    fn map_transport(e: reqwest::Error) -> ApiError {
        let kind = if e.is_timeout() { "timeout" } else { "unreachable" };
        ApiError::new(ErrorCode::ZernioPublishFailed, format!("Zernio {kind}")).retryable(true)
    }

    /// Submit a prepared payload. `idempotency_key` makes retries safe (Zernio dedups on it for 24h).
    pub async fn submit_post(&self, payload: &Value, idempotency_key: &str, request_id: &str) -> ApiResult<SubmitOutcome> {
        let resp = self
            .http
            .post(format!("{}/v1/posts", self.base_url))
            .bearer_auth(&self.api_key)
            .header("Idempotency-Key", idempotency_key)
            .header("x-request-id", request_id)
            .json(payload)
            .send()
            .await
            .map_err(Self::map_transport)?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        metrics::counter!("zernio_requests_total", "op" => "submit_post", "outcome" => if status.is_success() { "success" } else { "failure" }).increment(1);
        if !status.is_success() {
            return Err(Self::map_error(status, &body));
        }
        Ok(parse_submit_response(&body))
    }

    /// Read-only GET against an allow-listed resource family, scoped to one Zernio profile.
    pub async fn read(&self, resource: ReadResource, profile_id: &str, query: &[(&str, &str)]) -> ApiResult<Value> {
        let mut q: Vec<(&str, &str)> = vec![("profileId", profile_id)];
        q.extend_from_slice(query);
        let resp =
            self.http.get(format!("{}{}", self.base_url, resource.path())).bearer_auth(&self.api_key).query(&q).send().await.map_err(Self::map_transport)?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        metrics::counter!("zernio_requests_total", "op" => resource.name(), "outcome" => if status.is_success() { "success" } else { "failure" }).increment(1);
        if !status.is_success() {
            return Err(Self::map_error(status, &body));
        }
        Ok(body)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ReadResource {
    Comments,
    Inbox,
    Performance,
}

impl ReadResource {
    fn path(self) -> &'static str {
        match self {
            Self::Comments => "/v1/inbox/comments",
            Self::Inbox => "/v1/inbox/conversations",
            Self::Performance => "/v1/analytics",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Comments => "read_comments",
            Self::Inbox => "read_inbox",
            Self::Performance => "read_performance",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Permissions, ZernioConnection};
    use crate::model::*;
    use crate::workflow::WorkflowState;

    fn ctx(access: Access) -> RequestContext {
        RequestContext {
            tenant_id: "t1".into(),
            brand_id: "b1".into(),
            actor_id: "u1".into(),
            request_id: "r1".into(),
            idempotency_key: None,
            source: "x".into(),
            permissions: Permissions {
                allowed_tools: vec!["*".into()],
                allowed_channels: vec!["instagram".into(), "x".into(), "facebook".into()],
                allowed_actions: vec![Action::Publish, Action::Schedule],
            },
            zernio: Some(ZernioConnection {
                zernio_profile_id: "profile_1".into(),
                connected_accounts: vec![
                    ConnectedAccount {
                        platform: "instagram".into(),
                        account_id: "acc_ig".into(),
                        access,
                        tenant_id: Some("t1".into()),
                        brand_id: Some("b1".into()),
                    },
                    ConnectedAccount { platform: "twitter".into(), account_id: "acc_x".into(), access: Access::Write, tenant_id: None, brand_id: None },
                ],
            }),
            plan_channels: Some(vec!["instagram".into(), "x".into()]),
            quota_remaining: Some(10),
        }
    }

    fn draft() -> Draft {
        let v = |ch: &str| Variant {
            channel: ch.into(),
            format: "text".into(),
            caption: format!("Hello {ch}"),
            hashtags: vec!["#a".into()],
            cta: Some("Go".into()),
            media: vec![Media { kind: "image".into(), url: "https://cdn/x.png".into(), alt_text: "alt".into() }],
            parts: vec![],
        };
        Draft {
            id: "draft_1".into(),
            job_id: "j".into(),
            report_id: "r".into(),
            campaign_id: None,
            tenant_id: "t1".into(),
            brand_id: "b1".into(),
            channel: "multi".into(),
            format: "text".into(),
            state: WorkflowState::Approved,
            content: DraftContent { variants: vec![v("instagram"), v("x")] },
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn read_connection_cannot_publish() {
        let c = ctx(Access::Read);
        let e = validate_connection(&c, "profile_1", &["instagram".into()], Access::Write).unwrap_err();
        assert_eq!(e.code, ErrorCode::ZernioPermissionDenied);
        assert!(validate_connection(&c, "profile_1", &["instagram".into()], Access::Read).is_ok());
    }

    #[test]
    fn missing_account_wrong_profile_and_plan_are_rejected() {
        let c = ctx(Access::Write);
        assert_eq!(validate_connection(&c, "profile_1", &["facebook".into()], Access::Write).unwrap_err().code, ErrorCode::Forbidden); // not in plan
        let mut c2 = ctx(Access::Write);
        c2.plan_channels = None;
        assert_eq!(validate_connection(&c2, "profile_1", &["facebook".into()], Access::Write).unwrap_err().code, ErrorCode::ZernioConnectionMissing);
        assert_eq!(validate_connection(&c, "profile_other", &["instagram".into()], Access::Write).unwrap_err().code, ErrorCode::ZernioPermissionDenied);
        let mut c3 = ctx(Access::Write);
        c3.zernio = None;
        assert_eq!(validate_connection(&c3, "profile_1", &["instagram".into()], Access::Read).unwrap_err().code, ErrorCode::ZernioConnectionMissing);
    }

    #[test]
    fn builds_schedule_payload_per_platform() {
        let c = ctx(Access::Write);
        let accts = validate_connection(&c, "profile_1", &["instagram".into(), "x".into()], Access::Write).unwrap();
        let at = Utc::now() + chrono::Duration::days(2);
        let p = build_post_payload(&draft(), &accts, PublishAction::Schedule, Some(at)).unwrap();
        assert_eq!(p["platforms"][0]["platform"], "instagram");
        assert_eq!(p["platforms"][1]["platform"], "twitter");
        assert_eq!(p["platforms"][1]["accountId"], "acc_x");
        assert_eq!(p["platforms"][0]["customContent"], "Hello instagram\n\nGo\n\n#a");
        assert_eq!(p["mediaItems"].as_array().unwrap().len(), 1, "identical media is de-duplicated");
        assert!(p.get("publishNow").is_none() && p["scheduledFor"].is_string());
        // never carries credentials
        assert!(!p.to_string().to_lowercase().contains("token"));
    }

    #[test]
    fn schedule_requires_future_time_and_publish_sets_publish_now() {
        let c = ctx(Access::Write);
        let accts = validate_connection(&c, "profile_1", &["instagram".into()], Access::Write).unwrap();
        assert!(build_post_payload(&draft(), &accts, PublishAction::Schedule, None).is_err());
        assert!(build_post_payload(&draft(), &accts, PublishAction::Schedule, Some(Utc::now() - chrono::Duration::hours(1))).is_err());
        assert_eq!(build_post_payload(&draft(), &accts, PublishAction::Publish, None).unwrap()["publishNow"], true);
    }

    #[test]
    fn parses_partial_success_and_failure() {
        let ok = parse_submit_response(
            &json!({ "post": { "_id": "p1", "platforms": [{ "platform": "instagram", "status": "published", "platformPostUrl": "https://i/1" }, { "platform": "twitter", "status": "scheduled" }] } }),
        );
        assert_eq!(ok.status, PublishStatus::ScheduledOrPublished);
        let partial = parse_submit_response(
            &json!({ "post": { "_id": "p1", "platforms": [{ "platform": "instagram", "status": "published" }, { "platform": "twitter", "status": "failed", "errorMessage": "nope" }] } }),
        );
        assert_eq!(partial.status, PublishStatus::PartialSuccess);
        assert_eq!(partial.platforms[1].error.as_deref(), Some("nope"));
        let failed = parse_submit_response(&json!({ "post": { "platforms": [{ "platform": "twitter", "status": "failed" }] } }));
        assert_eq!(failed.status, PublishStatus::Failed);
    }

    #[test]
    fn maps_http_errors() {
        use reqwest::StatusCode as S;
        let m = |s: S, b: Value| ZernioClient::map_error(s, &b);
        assert_eq!(m(S::TOO_MANY_REQUESTS, json!({})).code, ErrorCode::ZernioRateLimited);
        assert!(m(S::TOO_MANY_REQUESTS, json!({})).retryable);
        assert_eq!(m(S::UNAUTHORIZED, json!({})).code, ErrorCode::ZernioPermissionDenied);
        assert_eq!(m(S::NOT_FOUND, json!({})).code, ErrorCode::ZernioConnectionMissing);
        assert!(m(S::BAD_GATEWAY, json!({})).retryable);
        assert!(m(S::CONFLICT, json!({ "code": "idempotency_conflict" })).retryable);
        assert!(!m(S::CONFLICT, json!({ "error": "duplicate" })).retryable);
        assert_eq!(m(S::UNPROCESSABLE_ENTITY, json!({ "message": "bad media" })).code, ErrorCode::ZernioPublishFailed);
    }
}
