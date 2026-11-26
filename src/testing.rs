//! Test support: an in-memory `AppState`, request signing, and context builders.
//! Compiled into the library (not behind `cfg(test)`) so integration tests in `tests/` can use it.
//! It is inert unless called, and constructs only in-memory components.

use crate::app::AppState;
use crate::auth;
use crate::config::Config;
use crate::context::*;
use crate::extract::{
    github::GitHubExtractor, http::HttpExtractor, x::XExtractor, youtube::YouTubeExtractor, ContentExtractor, ExtractorFlags, ExtractorRegistry,
};
use crate::images::{ImagePipeline, MockImageProvider};
use crate::llm::{mock::MockProvider, LlmClient, LlmLimits};
use crate::queue::MemoryQueue;
use crate::ratelimit::RateLimiter;
use crate::ssrf::{FetchLimits, SafeFetcher};
use crate::storage::MemoryStorage;
use crate::store::memory::MemoryStore;
use crate::urls::UrlPolicy;
use crate::usage::TracingUsageSink;
use crate::validate::ValidationOptions;
use crate::zernio::ZernioClient;
use std::sync::Arc;
use std::time::Duration;

pub const TEST_SECRET: &str = "test-secret-test-secret-test-secret-123";

pub struct Harness {
    pub state: AppState,
    pub store: Arc<MemoryStore>,
    pub queue: Arc<MemoryQueue>,
    pub storage: Arc<MemoryStorage>,
}

pub struct HarnessOptions {
    pub zernio_url: Option<String>,
    pub github_url: Option<String>,
    pub x_url: Option<String>,
    pub youtube_url: Option<String>,
    pub extra_extractors: Vec<Arc<dyn ContentExtractor>>,
    pub flags: ExtractorFlags,
    pub cfg: Config,
    /// Validate ingest URLs with the production (strict) SSRF policy instead of the test-permissive one.
    pub strict_urls: bool,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            zernio_url: None,
            github_url: None,
            x_url: None,
            youtube_url: None,
            extra_extractors: vec![],
            flags: ExtractorFlags { respect_robots: false, ..Default::default() },
            cfg: Config::for_tests(),
            strict_urls: false,
        }
    }
}

pub fn harness(opts: HarnessOptions) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let queue = Arc::new(MemoryQueue::new());
    let storage = Arc::new(MemoryStorage::default());
    // Tests talk to localhost mock servers, so the private-network guard is lifted — in tests only.
    let policy = UrlPolicy::default().dangerously_allow_private_networks();
    let fetcher = SafeFetcher::new(policy.clone(), FetchLimits { timeout: Duration::from_secs(5), ..Default::default() });
    let t = Duration::from_secs(5);
    let mut reg = ExtractorRegistry::new(opts.flags).with(Arc::new(HttpExtractor::new(fetcher.clone())));
    if let Some(u) = &opts.github_url {
        reg = reg.with(Arc::new(GitHubExtractor::new(u, None, t).unwrap()));
    }
    if let Some(u) = &opts.x_url {
        reg = reg.with(Arc::new(XExtractor::new(u, Some("bearer".into()), t).unwrap()));
    }
    if let Some(u) = &opts.youtube_url {
        reg = reg.with(Arc::new(YouTubeExtractor::new(u, Some("key".into()), t).unwrap()));
    }
    for e in opts.extra_extractors {
        reg = reg.with(e);
    }
    let state = AppState {
        cfg: Arc::new(opts.cfg),
        store: store.clone(),
        queue: queue.clone(),
        extractors: Arc::new(reg),
        llm: Arc::new(LlmClient::new(Arc::new(MockProvider), LlmLimits::default())),
        images: Arc::new(ImagePipeline { providers: vec![Arc::new(MockImageProvider { width: 1080, height: 1080 })], storage: storage.clone() }),
        zernio: opts.zernio_url.map(|u| Arc::new(ZernioClient::new(&u, "zernio-test-key", t).unwrap())),
        limiter: Arc::new(RateLimiter::new()),
        usage: Arc::new(TracingUsageSink),
        url_policy: if opts.strict_urls { UrlPolicy::default() } else { policy.clone() },
        validation: Arc::new(ValidationOptions { url_policy: policy, ..Default::default() }),
        metrics: None,
    };
    Harness { state, store, queue, storage }
}

/// A context holding every permission, for tenant `t1` / brand `b1`.
pub fn full_ctx(tenant: &str, brand: &str) -> RequestContext {
    RequestContext {
        tenant_id: tenant.into(),
        brand_id: brand.into(),
        actor_id: "user_1".into(),
        request_id: format!("req_{}", uuid::Uuid::new_v4().simple()),
        idempotency_key: None,
        source: "letwemarket_agent".into(),
        permissions: Permissions {
            allowed_tools: vec!["*".into()],
            allowed_channels: ["instagram", "facebook", "x", "linkedin"].iter().map(|s| s.to_string()).collect(),
            allowed_actions: vec![Action::Read, Action::Draft, Action::Publish, Action::Schedule],
        },
        zernio: Some(ZernioConnection {
            zernio_profile_id: "profile_1".into(),
            connected_accounts: ["instagram", "facebook", "x", "linkedin"]
                .iter()
                .map(|p| ConnectedAccount {
                    platform: p.to_string(),
                    account_id: format!("acc_{p}"),
                    access: Access::Write,
                    tenant_id: Some(tenant.into()),
                    brand_id: Some(brand.into()),
                })
                .collect(),
        }),
        plan_channels: None,
        quota_remaining: Some(1000),
    }
}

/// Sign a request exactly as LetWeMarket must.
pub fn signed_headers(method: &str, path: &str, body: &[u8]) -> Vec<(&'static str, String)> {
    let ts = chrono::Utc::now().timestamp().to_string();
    let sig = auth::sign(TEST_SECRET, &ts, method, path, body);
    vec![("x-lwm-timestamp", ts), ("x-lwm-signature", sig)]
}
