//! Service-to-service authentication (spec §5): HMAC-SHA256 request signing.
//!
//! LetWeMarket sends:
//!   x-lwm-timestamp: unix seconds
//!   x-lwm-signature: hex( HMAC-SHA256(secret, "{timestamp}\n{METHOD}\n{path_and_query}\n{sha256_hex(body)}") )
//! Any of `INTERNAL_AUTH_SECRETS` may verify (secret rotation). Requests older than 5 minutes
//! (or from the future) are rejected, which bounds replay of captured requests.

use crate::app::AppState;
use crate::error::{ApiError, ErrorCode};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

pub const MAX_SKEW_SECS: i64 = 300;

pub fn string_to_sign(timestamp: &str, method: &str, path_and_query: &str, body: &[u8]) -> String {
    format!("{timestamp}\n{}\n{path_and_query}\n{}", method.to_ascii_uppercase(), hex::encode(Sha256::digest(body)))
}

pub fn sign(secret: &str, timestamp: &str, method: &str, path_and_query: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac accepts any key length");
    mac.update(string_to_sign(timestamp, method, path_and_query, body).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn unauthorized(msg: &str) -> ApiError {
    ApiError::new(ErrorCode::Unauthorized, msg)
}

pub fn verify(
    secrets: &[String],
    now_unix: i64,
    timestamp: &str,
    signature_hex: &str,
    method: &str,
    path_and_query: &str,
    body: &[u8],
) -> Result<(), ApiError> {
    let ts: i64 = timestamp.parse().map_err(|_| unauthorized("invalid x-lwm-timestamp"))?;
    if (now_unix - ts).abs() > MAX_SKEW_SECS {
        return Err(unauthorized("request timestamp outside the allowed window"));
    }
    let sig = hex::decode(signature_hex).map_err(|_| unauthorized("invalid x-lwm-signature"))?;
    let msg = string_to_sign(timestamp, method, path_and_query, body);
    // Verify against every active secret; verify_slice is constant-time.
    let ok = secrets.iter().any(|s| {
        let mut mac = HmacSha256::new_from_slice(s.as_bytes()).expect("hmac key");
        mac.update(msg.as_bytes());
        mac.verify_slice(&sig).is_ok()
    });
    if ok {
        Ok(())
    } else {
        Err(unauthorized("signature verification failed"))
    }
}

pub async fn require_internal_auth(State(state): State<AppState>, req: Request, next: Next) -> Result<Response, ApiError> {
    let (parts, body) = req.into_parts();
    let ts = parts.headers.get("x-lwm-timestamp").and_then(|v| v.to_str().ok()).ok_or_else(|| unauthorized("missing x-lwm-timestamp"))?.to_string();
    let sig = parts.headers.get("x-lwm-signature").and_then(|v| v.to_str().ok()).ok_or_else(|| unauthorized("missing x-lwm-signature"))?.to_string();
    let bytes = to_bytes(body, state.cfg.max_body_bytes).await.map_err(|_| ApiError::validation("request body too large"))?;
    // Behind `nest`, `parts.uri` has the prefix stripped; the client signed the full original path.
    let uri = parts.extensions.get::<axum::extract::OriginalUri>().map(|o| o.0.clone()).unwrap_or_else(|| parts.uri.clone());
    let pq = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    verify(&state.cfg.auth_secrets, chrono::Utc::now().timestamp(), &ts, &sig, parts.method.as_str(), &pq, &bytes).inspect_err(|_| {
        metrics::counter!("auth_failures_total").increment(1);
    })?;
    Ok(next.run(Request::from_parts(parts, Body::from(bytes))).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = "test-secret-test-secret-test-secret-123";

    #[test]
    fn valid_signature_verifies_and_tamper_fails() {
        let sig = sign(S, "1000", "POST", "/internal/v1/tools/x", b"{\"a\":1}");
        let secrets = vec![S.to_string()];
        assert!(verify(&secrets, 1000, "1000", &sig, "POST", "/internal/v1/tools/x", b"{\"a\":1}").is_ok());
        assert!(verify(&secrets, 1000, "1000", &sig, "POST", "/internal/v1/tools/x", b"{\"a\":2}").is_err());
        assert!(verify(&secrets, 1000, "1000", &sig, "POST", "/internal/v1/tools/y", b"{\"a\":1}").is_err());
        assert!(verify(&secrets, 1000, "1000", &sig, "GET", "/internal/v1/tools/x", b"{\"a\":1}").is_err());
    }

    #[test]
    fn rejects_stale_future_and_garbage() {
        let sig = sign(S, "1000", "POST", "/p", b"");
        let secrets = vec![S.to_string()];
        assert!(verify(&secrets, 1000 + 301, "1000", &sig, "POST", "/p", b"").is_err());
        assert!(verify(&secrets, 1000 - 301, "1000", &sig, "POST", "/p", b"").is_err());
        assert!(verify(&secrets, 1000, "abc", &sig, "POST", "/p", b"").is_err());
        assert!(verify(&secrets, 1000, "1000", "zz", "POST", "/p", b"").is_err());
    }

    #[test]
    fn supports_secret_rotation() {
        let old = "old-secret-old-secret-old-secret-1234";
        let sig = sign(old, "5", "POST", "/p", b"");
        assert!(verify(&["new-secret-new-secret-new-secret-1234".into(), old.into()], 5, "5", &sig, "POST", "/p", b"").is_ok());
        assert!(verify(&["new-secret-new-secret-new-secret-1234".into()], 5, "5", &sig, "POST", "/p", b"").is_err());
    }
}
