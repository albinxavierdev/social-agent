//! Shared application state and wiring.

use crate::config::Config;
use crate::context::Scope;
use crate::error::ApiResult;
use crate::extract::{
    firecrawl::FirecrawlExtractor,
    github::GitHubExtractor,
    http::HttpExtractor,
    obscura::{ObscuraConfig, ObscuraExtractor},
    robots::RobotsChecker,
    x::XExtractor,
    youtube::YouTubeExtractor,
    ExtractorFlags, ExtractorRegistry,
};
use crate::ids::{new_id, TRACE};
use crate::images::{ImagePipeline, ImageProvider, MockImageProvider, OpenAiImageProvider, SourcePageImageProvider};
use crate::llm::{anthropic::AnthropicProvider, mock::MockProvider, openai::OpenAiProvider, LlmClient, LlmLimits, LlmProvider};
use crate::model::{Draft, Job};
use crate::queue::{MemoryQueue, PgQueue, Queue};
use crate::ratelimit::RateLimiter;
use crate::ssrf::{FetchLimits, SafeFetcher};
use crate::storage::{LocalFsStorage, MemoryStorage, ObjectStorage, S3Storage};
use crate::store::{memory::MemoryStore, postgres::PgStore, Store};
use crate::urls::UrlPolicy;
use crate::usage::{TracingUsageSink, UsageSink, WebhookUsageSink};
use crate::validate::ValidationOptions;
use crate::workflow::{Transition, WorkflowState};
use crate::zernio::ZernioClient;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub store: Arc<dyn Store>,
    pub queue: Arc<dyn Queue>,
    pub extractors: Arc<ExtractorRegistry>,
    pub llm: Arc<LlmClient>,
    pub images: Arc<ImagePipeline>,
    pub zernio: Option<Arc<ZernioClient>>,
    pub limiter: Arc<RateLimiter>,
    pub usage: Arc<dyn UsageSink>,
    pub url_policy: UrlPolicy,
    pub validation: Arc<ValidationOptions>,
    pub metrics: Option<metrics_exporter_prometheus::PrometheusHandle>,
}

/// Who/what is driving a state change — recorded on every persisted transition (spec §8).
#[derive(Debug, Clone)]
pub struct Actor {
    pub scope: Scope,
    pub actor_id: String,
    pub correlation_id: String,
    pub trace_id: String,
}

impl Actor {
    pub fn system(scope: Scope, correlation_id: &str, trace_id: &str) -> Self {
        Self { scope, actor_id: "system:worker".into(), correlation_id: correlation_id.into(), trace_id: trace_id.into() }
    }
}

impl AppState {
    fn transition_record(&self, who: &Actor, kind: &str, id: &str, from: WorkflowState, to: WorkflowState, reason: &str) -> Transition {
        Transition {
            id: new_id("tr"),
            tenant_id: who.scope.tenant_id.clone(),
            brand_id: who.scope.brand_id.clone(),
            entity_kind: kind.into(),
            entity_id: id.into(),
            actor_id: who.actor_id.clone(),
            from_state: from,
            to_state: to,
            reason: reason.into(),
            correlation_id: who.correlation_id.clone(),
            trace_id: who.trace_id.clone(),
            at: chrono::Utc::now(),
        }
    }

    /// Validate, persist (optimistically), and audit a job transition.
    pub async fn transition_job(&self, who: &Actor, job: &mut Job, to: WorkflowState, reason: &str) -> ApiResult<()> {
        let from = job.state;
        from.require_transition(to)?;
        job.state = to;
        job.updated_at = chrono::Utc::now();
        if to.is_terminal() {
            job.completed_at = Some(job.updated_at);
        }
        self.store.save_job(job, from).await?;
        self.store.record_transition(self.transition_record(who, "job", &job.id, from, to, reason)).await?;
        metrics::counter!("workflow_transitions_total", "entity" => "job", "to" => to.as_str()).increment(1);
        Ok(())
    }

    pub async fn transition_draft(&self, who: &Actor, draft: &mut Draft, to: WorkflowState, reason: &str) -> ApiResult<()> {
        let from = draft.state;
        from.require_transition(to)?;
        draft.state = to;
        draft.updated_at = chrono::Utc::now();
        self.store.save_draft(draft, from).await?;
        self.store.record_transition(self.transition_record(who, "draft", &draft.id, from, to, reason)).await?;
        metrics::counter!("workflow_transitions_total", "entity" => "draft", "to" => to.as_str()).increment(1);
        Ok(())
    }

    /// Keep the parent job's state aligned with draft progress. Illegal/no-op moves are ignored on
    /// purpose: a job fans out to many drafts and must not fail because one is behind another.
    pub async fn sync_job(&self, who: &Actor, job_id: &str, to: WorkflowState, reason: &str) {
        let Ok(Some(mut job)) = self.store.get_job(&who.scope, job_id).await else { return };
        if job.state.can_transition_to(to) {
            if let Err(e) = self.transition_job(who, &mut job, to, reason).await {
                tracing::debug!(job_id, error = %e.message, "job sync skipped");
            }
        }
    }

    pub fn new_trace_id() -> String {
        new_id(TRACE)
    }

    pub fn rate_limit_provider(&self, provider: &str) -> ApiResult<()> {
        self.limiter.check(&format!("provider:{provider}"), self.cfg.rate_provider_per_min, &format!("provider:{provider}"))
    }
}

pub fn build_fetcher(cfg: &Config) -> (UrlPolicy, SafeFetcher) {
    let mut policy = UrlPolicy::default();
    policy.allow_http = cfg.allow_http;
    policy.allowlist_hosts = cfg.url_allowlist.clone();
    policy.denylist_hosts = cfg.url_denylist.clone();
    let mut f = SafeFetcher::new(policy.clone(), FetchLimits::default());
    f.proxy = cfg.egress_proxy.clone();
    (policy, f)
}

pub fn build_storage(cfg: &Config) -> anyhow::Result<Arc<dyn ObjectStorage>> {
    Ok(match cfg.storage_kind.as_str() {
        "s3" => Arc::new(S3Storage::new(
            cfg.s3_endpoint.as_deref().unwrap_or_default(),
            &cfg.s3_region,
            cfg.s3_bucket.as_deref().unwrap_or_default(),
            cfg.s3_access_key.as_deref().unwrap_or_default(),
            cfg.s3_secret_key.as_deref().unwrap_or_default(),
            &cfg.storage_public_base_url,
            cfg.s3_path_style,
        )?),
        "local" => Arc::new(LocalFsStorage { root: cfg.local_storage_dir.clone().into(), public_base_url: cfg.storage_public_base_url.clone() }),
        "memory" => Arc::new(MemoryStorage::default()),
        other => anyhow::bail!("unknown STORAGE_KIND '{other}'"),
    })
}

pub fn build_llm(cfg: &Config) -> anyhow::Result<Arc<LlmClient>> {
    let price = (cfg.llm_price_in_per_mtok, cfg.llm_price_out_per_mtok);
    let timeout = Duration::from_secs(60);
    let provider: Arc<dyn LlmProvider> = match cfg.llm_provider.as_str() {
        "mock" => Arc::new(MockProvider),
        "anthropic" => Arc::new(AnthropicProvider::new(
            cfg.llm_base_url.as_deref().unwrap_or("https://api.anthropic.com"),
            cfg.llm_api_key.as_deref().unwrap_or_default(),
            &cfg.llm_model,
            price,
            timeout,
        )?),
        "openai" => Arc::new(OpenAiProvider::new(
            cfg.llm_base_url.as_deref().unwrap_or("https://api.openai.com/v1"),
            cfg.llm_api_key.as_deref().unwrap_or_default(),
            &cfg.llm_model,
            price,
            timeout,
        )?),
        other => anyhow::bail!("unknown LLM_PROVIDER '{other}'"),
    };
    Ok(Arc::new(LlmClient::new(provider, LlmLimits::default())))
}

pub fn build_extractors(cfg: &Config, fetcher: &SafeFetcher, storage: Arc<dyn ObjectStorage>) -> anyhow::Result<ExtractorRegistry> {
    let flags = ExtractorFlags {
        http_enabled: cfg.flag_http,
        obscura_enabled: cfg.flag_obscura,
        firecrawl_enabled: cfg.flag_firecrawl,
        prefer_firecrawl: cfg.prefer_firecrawl,
        respect_robots: cfg.respect_robots,
    };
    let t = Duration::from_secs(20);
    let mut reg = ExtractorRegistry::new(flags)
        .with(Arc::new(HttpExtractor::new(fetcher.clone())))
        .with(Arc::new(ObscuraExtractor::new(
            ObscuraConfig {
                binary: cfg.obscura_bin.clone().into(),
                max_concurrency: cfg.obscura_max_concurrency,
                total_timeout: cfg.obscura_timeout,
                proxy: cfg.egress_proxy.clone(),
                ..Default::default()
            },
            fetcher.clone(),
            storage,
        )))
        .with(Arc::new(GitHubExtractor::new(&cfg.github_base_url, cfg.github_token.clone(), t)?))
        .with(Arc::new(XExtractor::new(&cfg.x_base_url, cfg.x_bearer_token.clone(), t)?))
        .with(Arc::new(YouTubeExtractor::new(&cfg.youtube_base_url, cfg.youtube_api_key.clone(), t)?))
        .with_robots(Arc::new(RobotsChecker::new(fetcher.clone(), "LetWeMarketContentAgent")));
    if let Some(key) = &cfg.firecrawl_api_key {
        reg = reg.with(Arc::new(FirecrawlExtractor::new(&cfg.firecrawl_base_url, key, Duration::from_secs(60))?));
    }
    Ok(reg)
}

pub fn build_images(cfg: &Config, fetcher: &SafeFetcher, storage: Arc<dyn ObjectStorage>) -> anyhow::Result<ImagePipeline> {
    let mut providers: Vec<Arc<dyn ImageProvider>> = vec![Arc::new(SourcePageImageProvider { fetcher: fetcher.clone() })];
    match cfg.image_provider.as_str() {
        "mock" => providers.push(Arc::new(MockImageProvider { width: 1080, height: 1080 })),
        "openai" => providers.push(Arc::new(OpenAiImageProvider::new(
            cfg.llm_base_url.as_deref().unwrap_or("https://api.openai.com/v1"),
            cfg.llm_api_key.as_deref().unwrap_or_default(),
            &cfg.image_model,
            Duration::from_secs(90),
        )?)),
        "none" => {}
        other => anyhow::bail!("unknown IMAGE_PROVIDER '{other}'"),
    }
    Ok(ImagePipeline { providers, storage })
}

impl AppState {
    pub async fn build(cfg: Config, metrics: Option<metrics_exporter_prometheus::PrometheusHandle>) -> anyhow::Result<Self> {
        let (policy, fetcher) = build_fetcher(&cfg);
        let storage = build_storage(&cfg)?;
        let (store, queue): (Arc<dyn Store>, Arc<dyn Queue>) = match &cfg.database_url {
            Some(url) => {
                let pg = PgStore::connect(url, cfg.db_max_connections).await?;
                pg.migrate().await?;
                let q = PgQueue::new(pg.pool.clone());
                (Arc::new(pg), Arc::new(q))
            }
            None => {
                tracing::warn!("DATABASE_URL not set: using in-memory store and queue (state is lost on restart; development only)");
                (Arc::new(MemoryStore::new()), Arc::new(MemoryQueue::new()))
            }
        };
        let usage: Arc<dyn UsageSink> = match (&cfg.usage_webhook_url, &cfg.usage_webhook_secret) {
            (Some(url), Some(secret)) => Arc::new(WebhookUsageSink {
                url: url.clone(),
                secret: secret.clone(),
                http: reqwest::Client::builder().timeout(Duration::from_secs(5)).build()?,
            }),
            _ => Arc::new(TracingUsageSink),
        };
        let zernio = match &cfg.zernio_api_key {
            Some(k) => Some(Arc::new(ZernioClient::new(&cfg.zernio_base_url, k, Duration::from_secs(30))?)),
            None => None,
        };
        let validation = ValidationOptions { url_policy: policy.clone(), ..Default::default() };
        Ok(Self {
            extractors: Arc::new(build_extractors(&cfg, &fetcher, storage.clone())?),
            llm: build_llm(&cfg)?,
            images: Arc::new(build_images(&cfg, &fetcher, storage)?),
            zernio,
            limiter: Arc::new(RateLimiter::new()),
            usage,
            url_policy: policy,
            validation: Arc::new(validation),
            metrics,
            cfg: Arc::new(cfg),
            store,
            queue,
        })
    }
}
