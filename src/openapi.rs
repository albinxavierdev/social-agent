//! OpenAPI 3.1 document, generated from the tool registry so docs cannot drift from the code.

use crate::tools::{ToolSpec, TOOLS};
use serde_json::{json, Value};

fn s() -> Value {
    json!({ "type": "string" })
}
fn ns() -> Value {
    json!({ "type": ["string", "null"] })
}
fn nts() -> Value {
    json!({ "type": ["string", "null"], "format": "date-time" })
}
fn arr(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}
fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn brand_context() -> Value {
    json!({ "$ref": "#/components/schemas/BrandContext" })
}

/// Request `input` schema per tool.
pub fn input_schema(tool: &str) -> Value {
    match tool {
        "ingest_url" => obj(
            json!({
            "urls": { "type": "array", "items": s(), "minItems": 1, "maxItems": 20 }, "content_type": s(), "requested_channels": arr(s()),
            "brand_context": brand_context(), "want_screenshot": { "type": "boolean" }, "allow_duplicates": { "type": "boolean" } }),
            &["urls"],
        ),
        "extract_content" => obj(json!({ "source_id": s(), "force": { "type": "boolean" }, "want_screenshot": { "type": "boolean" } }), &["source_id"]),
        "generate_content_report" => {
            obj(json!({ "source_ids": arr(s()), "brand_context": brand_context(), "campaign_goal": s(), "requested_channels": arr(s()) }), &["source_ids"])
        }
        "generate_social_draft" => obj(
            json!({ "report_id": s(), "channels": arr(s()), "post_type": s(), "brand_context": brand_context(),
            "include_hashtags": { "type": "boolean" }, "include_cta": { "type": "boolean" } }),
            &["report_id", "channels"],
        ),
        "repurpose_content" => obj(
            json!({ "source_id": s(), "report_id": s(), "draft_id": s(),
            "targets": arr(obj(json!({ "channel": s(), "format": s() }), &["channel", "format"])), "brand_context": brand_context() }),
            &["targets"],
        ),
        "find_images" => obj(
            json!({ "draft_id": s(), "count": { "type": "integer", "minimum": 1, "maximum": 4 }, "allow_source_images": { "type": "boolean" } }),
            &["draft_id"],
        ),
        "validate_content" => obj(json!({ "draft_id": s(), "check_brand_rules": { "type": "boolean" } }), &["draft_id"]),
        "review_draft" => obj(json!({ "draft_id": s(), "action": { "enum": ["submit", "status"] } }), &["draft_id"]),
        "submit_review_decision" => obj(
            json!({ "draft_id": s(), "decision": { "enum": ["approved", "rejected", "revision_requested", "cancelled"] },
            "edited_content": { "type": "object" }, "reviewer_id": s(), "notes": s() }),
            &["draft_id", "decision", "reviewer_id"],
        ),
        "prepare_publish_request" => obj(
            json!({ "draft_id": s(), "zernio_profile_id": s(), "channels": arr(s()), "action": { "enum": ["publish", "schedule"] },
            "scheduled_at": { "type": "string", "format": "date-time" } }),
            &["draft_id", "zernio_profile_id", "channels", "action"],
        ),
        "publish_draft" | "schedule_draft" => obj(json!({ "publish_request_id": s(), "async": { "type": "boolean" } }), &["publish_request_id"]),
        "read_x_post" | "read_youtube_video" => obj(json!({ "url": s() }), &["url"]),
        "read_comments" | "read_inbox" | "read_performance" => {
            obj(json!({ "zernio_profile_id": s(), "platform": s(), "limit": { "type": "integer" }, "cursor": s() }), &["zernio_profile_id"])
        }
        "get_job" => obj(json!({ "job_id": s() }), &["job_id"]),
        "cancel_job" => obj(json!({ "job_id": s(), "reason": s() }), &["job_id"]),
        "get_draft" => obj(json!({ "draft_id": s() }), &["draft_id"]),
        _ => json!({ "type": "object" }),
    }
}

/// Response schema per tool (the contracts in the requirements document).
pub fn output_schema(tool: &str) -> Value {
    match tool {
        "ingest_url" => obj(
            json!({ "job_id": s(), "status": s(), "sources": arr(obj(json!({ "source_id": s(), "url": s(), "source_type": s(), "extraction_strategy": s() }), &["source_id"])) }),
            &["job_id", "status", "sources"],
        ),
        "extract_content" => obj(
            json!({ "source_id": s(), "url": s(), "title": ns(), "author": ns(), "published_at": nts(), "content_type": ns(),
            "language": ns(), "text": ns(), "summary": ns(), "metadata": { "type": "object" }, "screenshot_url": ns(), "extraction_method": ns(), "extraction_status": s() }),
            &["source_id", "extraction_status"],
        ),
        "generate_content_report" => obj(
            json!({ "report_id": s(), "source_ids": arr(s()), "key_points": arr(s()), "audience_takeaways": arr(s()), "recommended_angle": s(),
            "content_pillars": arr(s()), "recommended_channels": arr(s()), "risks": arr(s()), "source_links": arr(s()), "confidence": { "type": "number" } }),
            &["report_id"],
        ),
        "generate_social_draft" => obj(
            json!({ "draft_id": s(), "report_id": s(), "status": s(), "variants": arr(json!({ "$ref": "#/components/schemas/Variant" })),
            "source_links": arr(s()), "compliance": obj(json!({ "status": s(), "warnings": arr(s()) }), &["status"]) }),
            &["draft_id", "status", "variants"],
        ),
        "repurpose_content" => obj(
            json!({ "campaign_id": s(), "variants": arr(obj(json!({ "channel": s(), "format": s(), "draft_id": s() }), &["draft_id"])) }),
            &["campaign_id", "variants"],
        ),
        "find_images" => obj(
            json!({ "draft_id": s(), "images": arr(obj(json!({ "image_id": s(), "url": s(), "width": { "type": "integer" }, "height": { "type": "integer" },
            "aspect_ratio": s(), "alt_text": s(), "source": s(), "validation_status": s() }), &["image_id", "url"])) }),
            &["draft_id", "images"],
        ),
        "validate_content" => obj(
            json!({ "draft_id": s(), "status": { "enum": ["passed", "failed"] }, "issues": arr(json!({ "$ref": "#/components/schemas/Issue" })), "warnings": arr(s()) }),
            &["draft_id", "status"],
        ),
        "review_draft" => obj(json!({ "job_id": s(), "draft_id": s(), "status": s(), "review_url": s(), "next_action": s() }), &["draft_id", "status"]),
        "submit_review_decision" => obj(json!({ "draft_id": s(), "status": s(), "review_event_id": s(), "next_action": s() }), &["draft_id", "status"]),
        "prepare_publish_request" => obj(
            json!({ "publish_request_id": s(), "zernio_profile_id": s(), "action": s(), "scheduled_at": nts(),
            "payload": obj(json!({ "channels": arr(s()), "content": { "type": "object" }, "media": { "type": "array" }, "zernio_request": { "type": "object" } }), &[]), "status": s() }),
            &["publish_request_id", "status"],
        ),
        _ => json!({ "type": "object" }),
    }
}

fn operation(t: &ToolSpec) -> Value {
    json!({
        "operationId": t.name,
        "summary": t.description,
        "tags": ["tools"],
        "x-zernio-connection": t.zernio_connection,
        "x-zernio-access": t.zernio_access,
        "x-required-action": t.required_action,
        "security": [{ "hmacSignature": [] }],
        "requestBody": { "required": true, "content": { "application/json": { "schema": {
            "type": "object", "required": ["context", "input"],
            "properties": { "context": { "$ref": "#/components/schemas/RequestContext" }, "input": input_schema(t.name) } } } } },
        "responses": {
            "200": { "description": "Tool result", "content": { "application/json": { "schema": output_schema(t.name) } } },
            "default": { "description": "Error", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ErrorEnvelope" } } } }
        }
    })
}

pub fn spec() -> Value {
    let mut paths = serde_json::Map::new();
    for t in TOOLS {
        paths.insert(format!("/internal/v1/tools/{}", t.name), json!({ "post": operation(t) }));
    }
    paths.insert("/internal/v1/tools".into(), json!({ "get": { "operationId": "list_tools", "summary": "List the tool registry", "security": [{ "hmacSignature": [] }], "responses": { "200": { "description": "Registry" } } } }));
    paths.insert("/healthz".into(), json!({ "get": { "operationId": "healthz", "summary": "Liveness", "responses": { "200": { "description": "ok" } } } }));
    paths.insert("/readyz".into(), json!({ "get": { "operationId": "readyz", "summary": "Readiness (database + queue)", "responses": { "200": { "description": "ready" }, "503": { "description": "unavailable" } } } }));
    paths.insert(
        "/metrics".into(),
        json!({ "get": { "operationId": "metrics", "summary": "Prometheus metrics", "responses": { "200": { "description": "text exposition" } } } }),
    );

    let codes = [
        "VALIDATION_ERROR",
        "UNAUTHORIZED",
        "FORBIDDEN",
        "TENANT_NOT_FOUND",
        "BRAND_NOT_FOUND",
        "RATE_LIMIT_EXCEEDED",
        "QUOTA_EXCEEDED",
        "URL_NOT_ALLOWED",
        "EXTRACTION_FAILED",
        "LLM_PROVIDER_ERROR",
        "CONTENT_POLICY_VIOLATION",
        "DUPLICATE_SOURCE",
        "ZERNIO_CONNECTION_MISSING",
        "ZERNIO_PERMISSION_DENIED",
        "ZERNIO_PUBLISH_FAILED",
        "ZERNIO_RATE_LIMITED",
        "INTERNAL_ERROR",
        "NOT_FOUND",
        "INVALID_STATE",
    ];
    json!({
        "openapi": "3.1.0",
        "info": { "title": "LetWeMarket Content Agent Tool API", "version": "1.0.0",
            "description": "Internal, tool-based content-intelligence API. Authenticated with HMAC-SHA256 request signing (x-lwm-timestamp, x-lwm-signature): signature = hex(HMAC(secret, \"{timestamp}\\n{METHOD}\\n{path_and_query}\\n{sha256_hex(body)}\"))." },
        "servers": [{ "url": "https://content-agent.internal.letwemarket.com" }],
        "paths": paths,
        "components": {
            "securitySchemes": { "hmacSignature": { "type": "apiKey", "in": "header", "name": "x-lwm-signature", "description": "Also requires x-lwm-timestamp (unix seconds, ±5 min)." } },
            "schemas": {
                "BrandContext": obj(json!({ "business_description": s(), "tone_of_voice": s(), "target_audience": s(), "content_rules": arr(s()) }), &[]),
                "Issue": obj(json!({ "code": s(), "channel": s(), "message": s() }), &["code", "message"]),
                "Variant": obj(json!({ "channel": s(), "format": s(), "caption": s(), "hashtags": arr(s()), "cta": ns(), "parts": arr(s()),
                    "media": arr(obj(json!({ "type": s(), "url": s(), "alt_text": s() }), &["type", "url"])) }), &["channel", "caption"]),
                "RequestContext": obj(json!({
                    "tenant_id": s(), "brand_id": s(), "actor_id": s(), "request_id": s(), "idempotency_key": s(), "source": s(),
                    "permissions": obj(json!({ "allowed_tools": arr(s()), "allowed_channels": arr(s()), "allowed_actions": arr(json!({ "enum": ["read", "draft", "publish", "schedule"] })) }), &[]),
                    "zernio": obj(json!({ "zernio_profile_id": s(), "connected_accounts": arr(obj(json!({ "platform": s(), "account_id": s(), "access": { "enum": ["read", "write"] }, "tenant_id": s(), "brand_id": s() }), &["platform", "account_id", "access"])) }), &["zernio_profile_id"]),
                    "plan_channels": arr(s()), "quota_remaining": { "type": "integer" } }), &["tenant_id", "brand_id", "actor_id", "request_id", "permissions"]),
                "ErrorEnvelope": obj(json!({ "error": obj(json!({ "code": { "enum": codes }, "message": s(), "details": { "type": "object" }, "retryable": { "type": "boolean" }, "documentation_url": s() }), &["code", "message", "retryable"]) }), &["error"]),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_tool_is_documented() {
        let doc = spec();
        for t in TOOLS {
            assert!(doc["paths"][format!("/internal/v1/tools/{}", t.name)]["post"]["operationId"] == t.name, "{} missing", t.name);
        }
        assert_eq!(doc["openapi"], "3.1.0");
    }

    #[test]
    fn all_error_codes_are_documented() {
        let doc = spec();
        let listed: Vec<&str> = doc["components"]["schemas"]["ErrorEnvelope"]["properties"]["error"]["properties"]["code"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        use crate::error::ErrorCode::*;
        for c in [
            ValidationError,
            Unauthorized,
            Forbidden,
            TenantNotFound,
            BrandNotFound,
            RateLimitExceeded,
            QuotaExceeded,
            UrlNotAllowed,
            ExtractionFailed,
            LlmProviderError,
            ContentPolicyViolation,
            DuplicateSource,
            ZernioConnectionMissing,
            ZernioPermissionDenied,
            ZernioPublishFailed,
            ZernioRateLimited,
            InternalError,
            NotFound,
            InvalidState,
        ] {
            assert!(listed.contains(&c.as_str()), "{} undocumented", c.as_str());
        }
    }
}
