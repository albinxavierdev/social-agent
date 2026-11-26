//! Token-bucket rate limiting (spec §5, §13) per tenant, brand, tool and external provider.
//! State is per process; for strict global limits across replicas back this with Redis
//! (the `RateLimiter` surface is small on purpose so it can be swapped).

use crate::error::{ApiError, ErrorCode};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one token from `key`'s bucket (capacity = `per_min`, refilled continuously).
    pub fn check(&self, key: &str, per_min: u32, what: &str) -> Result<(), ApiError> {
        if per_min == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let cap = per_min as f64;
        let rate = cap / 60.0;
        let mut g = self.buckets.lock().unwrap();
        if g.len() > 100_000 {
            g.retain(|_, b| now.duration_since(b.updated).as_secs() < 300);
        }
        let b = g.entry(key.to_string()).or_insert(Bucket { tokens: cap, updated: now });
        b.tokens = (b.tokens + now.duration_since(b.updated).as_secs_f64() * rate).min(cap);
        b.updated = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            metrics::counter!("rate_limit_hits_total", "scope" => what.to_string()).increment(1);
            Err(ApiError::new(ErrorCode::RateLimitExceeded, format!("rate limit exceeded for {what}"))
                .with_details(serde_json::json!({ "scope": what, "limit_per_minute": per_min })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_capacity_then_blocks() {
        let r = RateLimiter::new();
        for _ in 0..3 {
            assert!(r.check("k", 3, "tenant").is_ok());
        }
        let e = r.check("k", 3, "tenant").unwrap_err();
        assert_eq!(e.code, ErrorCode::RateLimitExceeded);
        assert!(e.retryable);
    }

    #[test]
    fn keys_are_independent() {
        let r = RateLimiter::new();
        assert!(r.check("a", 1, "t").is_ok());
        assert!(r.check("b", 1, "t").is_ok());
        assert!(r.check("a", 1, "t").is_err());
    }
}
