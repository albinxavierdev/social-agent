//! API error contract (spec §15): one envelope, stable machine codes.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
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
    // Extensions beyond the spec's required list, needed for a usable API.
    NotFound,
    InvalidState,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        use ErrorCode::*;
        match self {
            ValidationError => "VALIDATION_ERROR",
            Unauthorized => "UNAUTHORIZED",
            Forbidden => "FORBIDDEN",
            TenantNotFound => "TENANT_NOT_FOUND",
            BrandNotFound => "BRAND_NOT_FOUND",
            RateLimitExceeded => "RATE_LIMIT_EXCEEDED",
            QuotaExceeded => "QUOTA_EXCEEDED",
            UrlNotAllowed => "URL_NOT_ALLOWED",
            ExtractionFailed => "EXTRACTION_FAILED",
            LlmProviderError => "LLM_PROVIDER_ERROR",
            ContentPolicyViolation => "CONTENT_POLICY_VIOLATION",
            DuplicateSource => "DUPLICATE_SOURCE",
            ZernioConnectionMissing => "ZERNIO_CONNECTION_MISSING",
            ZernioPermissionDenied => "ZERNIO_PERMISSION_DENIED",
            ZernioPublishFailed => "ZERNIO_PUBLISH_FAILED",
            ZernioRateLimited => "ZERNIO_RATE_LIMITED",
            InternalError => "INTERNAL_ERROR",
            NotFound => "NOT_FOUND",
            InvalidState => "INVALID_STATE",
        }
    }

    pub fn status(self) -> StatusCode {
        use ErrorCode::*;
        match self {
            ValidationError | UrlNotAllowed | ContentPolicyViolation => StatusCode::BAD_REQUEST,
            Unauthorized => StatusCode::UNAUTHORIZED,
            Forbidden | ZernioPermissionDenied => StatusCode::FORBIDDEN,
            TenantNotFound | BrandNotFound | NotFound => StatusCode::NOT_FOUND,
            RateLimitExceeded | ZernioRateLimited => StatusCode::TOO_MANY_REQUESTS,
            QuotaExceeded => StatusCode::PAYMENT_REQUIRED,
            DuplicateSource | InvalidState => StatusCode::CONFLICT,
            ZernioConnectionMissing => StatusCode::UNPROCESSABLE_ENTITY,
            ExtractionFailed | LlmProviderError | ZernioPublishFailed => StatusCode::BAD_GATEWAY,
            InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn default_retryable(self) -> bool {
        use ErrorCode::*;
        matches!(self, RateLimitExceeded | ZernioRateLimited | LlmProviderError | InternalError)
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub retryable: bool,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), details: json!({}), retryable: code.default_retryable() }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
    pub fn validation(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationError, msg)
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::Forbidden, msg)
    }
    pub fn not_found(what: &str, id: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("{what} not found")).with_details(json!({ "resource": what, "id": id }))
    }
    pub fn invalid_state(msg: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidState, msg)
    }
    pub fn internal(err: impl std::fmt::Display) -> Self {
        // Log the detail server-side; never leak it to the caller.
        tracing::error!(error = %err, "internal error");
        Self::new(ErrorCode::InternalError, "internal error")
    }

    pub fn envelope(&self) -> Value {
        json!({
            "error": {
                "code": self.code.as_str(),
                "message": self.message,
                "details": self.details,
                "retryable": self.retryable,
                "documentation_url": format!(
                    "https://internal.docs.letwemarket.com/errors/{}", self.code.as_str()
                ),
            }
        })
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.code.status();
        let mut resp = (status, Json(self.envelope())).into_response();
        if self.code == ErrorCode::RateLimitExceeded || self.code == ErrorCode::ZernioRateLimited {
            resp.headers_mut().insert("retry-after", axum::http::HeaderValue::from_static("1"));
        }
        resp
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::validation(format!("invalid JSON: {e}"))
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::internal(e)
    }
}
