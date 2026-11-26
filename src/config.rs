//! Environment-driven configuration. Secrets are read from the environment (injected by the secret
//! manager / orchestrator) and are redacted in `Debug` output.

use anyhow::{bail, Context};
use std::collections::HashSet;
use std::time::Duration;

#[derive(Clone)]
pub struct Config {
    pub env: String,
    pub bind_addr: String,
    pub database_url: Option<String>,
    pub db_max_connections: u32,
    pub auth_secrets: Vec<String>,
    pub max_body_bytes: usize,

    pub llm_provider: String,
    pub llm_api_key: Option<String>,
    pub llm_base_url: Option<String>,
    pub llm_model: String,
    pub llm_price_in_per_mtok: f64,
    pub llm_price_out_per_mtok: f64,

    pub image_provider: String,
    pub image_model: String,

    pub zernio_base_url: String,
    pub zernio_api_key: Option<String>,
    pub firecrawl_base_url: String,
    pub firecrawl_api_key: Option<String>,
    pub github_base_url: String,
    pub github_token: Option<String>,
    pub x_base_url: String,
    pub x_bearer_token: Option<String>,
    pub youtube_base_url: String,
    pub youtube_api_key: Option<String>,

    pub obscura_bin: String,
    pub obscura_max_concurrency: usize,
    pub obscura_timeout: Duration,

    pub storage_kind: String,
    pub s3_endpoint: Option<String>,
    pub s3_region: String,
    pub s3_bucket: Option<String>,
    pub s3_access_key: Option<String>,
    pub s3_secret_key: Option<String>,
    pub s3_path_style: bool,
    pub storage_public_base_url: String,
    pub local_storage_dir: String,

    pub url_allowlist: Option<Vec<String>>,
    pub url_denylist: Vec<String>,
    pub allow_http: bool,
    pub egress_proxy: Option<String>,

    // Feature flags (spec §17)
    pub flag_http: bool,
    pub flag_obscura: bool,
    pub flag_firecrawl: bool,
    pub prefer_firecrawl: bool,
    pub respect_robots: bool,
    pub disabled_tools: HashSet<String>,

    pub rate_tenant_per_min: u32,
    pub rate_brand_per_min: u32,
    pub rate_tool_per_min: u32,
    pub rate_provider_per_min: u32,
    pub tool_timeout: Duration,
    pub review_url_template: String,
    pub usage_webhook_url: Option<String>,
    pub usage_webhook_secret: Option<String>,
    pub otlp_endpoint: Option<String>,
    pub worker_concurrency: usize,
    /// Purge extracted content/screenshots older than this many days (None = keep).
    pub retention_source_days: Option<u32>,
    /// Serve the developer console at /console (development/staging only).
    pub console_enabled: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("env", &self.env)
            .field("bind_addr", &self.bind_addr)
            .field("database", &self.database_url.as_ref().map(|_| "<set>"))
            .field("llm_provider", &self.llm_provider)
            .field("storage_kind", &self.storage_kind)
            .field("secrets", &"<redacted>")
            .finish()
    }
}

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}
fn var_or(k: &str, d: &str) -> String {
    var(k).unwrap_or_else(|| d.to_string())
}
fn flag(k: &str, d: bool) -> bool {
    var(k).map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")).unwrap_or(d)
}
fn num<T: std::str::FromStr>(k: &str, d: T) -> anyhow::Result<T> {
    match var(k) {
        None => Ok(d),
        Some(v) => v.parse().map_err(|_| anyhow::anyhow!("{k} must be a valid number")),
    }
}
fn list(k: &str) -> Vec<String> {
    var(k).map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let env = var_or("APP_ENV", "development");
        let production = env == "production" || env == "staging";
        let auth_secrets = list("INTERNAL_AUTH_SECRETS");
        if auth_secrets.is_empty() {
            bail!("INTERNAL_AUTH_SECRETS is required (comma-separated, supports rotation)");
        }
        if auth_secrets.iter().any(|s| s.len() < 32) {
            bail!("every INTERNAL_AUTH_SECRETS entry must be at least 32 characters");
        }
        let cfg = Self {
            bind_addr: var_or("BIND_ADDR", "0.0.0.0:8080"),
            database_url: var("DATABASE_URL"),
            db_max_connections: num("DB_MAX_CONNECTIONS", 10)?,
            auth_secrets,
            max_body_bytes: num("MAX_BODY_BYTES", 1_048_576)?,
            llm_provider: var_or("LLM_PROVIDER", "mock"),
            llm_api_key: var("LLM_API_KEY"),
            llm_base_url: var("LLM_BASE_URL"),
            llm_model: var_or("LLM_MODEL", "claude-sonnet-5-5"),
            llm_price_in_per_mtok: num("LLM_PRICE_IN_PER_MTOK", 0.0)?,
            llm_price_out_per_mtok: num("LLM_PRICE_OUT_PER_MTOK", 0.0)?,
            image_provider: var_or("IMAGE_PROVIDER", "mock"),
            image_model: var_or("IMAGE_MODEL", "gpt-image-1"),
            zernio_base_url: var_or("ZERNIO_BASE_URL", "https://zernio.com/api"),
            zernio_api_key: var("ZERNIO_API_KEY"),
            firecrawl_base_url: var_or("FIRECRAWL_BASE_URL", "https://api.firecrawl.dev"),
            firecrawl_api_key: var("FIRECRAWL_API_KEY"),
            github_base_url: var_or("GITHUB_BASE_URL", "https://api.github.com"),
            github_token: var("GITHUB_TOKEN"),
            x_base_url: var_or("X_BASE_URL", "https://api.x.com"),
            x_bearer_token: var("X_BEARER_TOKEN"),
            youtube_base_url: var_or("YOUTUBE_BASE_URL", "https://www.googleapis.com"),
            youtube_api_key: var("YOUTUBE_API_KEY"),
            obscura_bin: var_or("OBSCURA_BIN", "obscura"),
            obscura_max_concurrency: num("OBSCURA_MAX_CONCURRENCY", 4)?,
            obscura_timeout: Duration::from_secs(num("OBSCURA_TIMEOUT_SECS", 45)?),
            storage_kind: var_or("STORAGE_KIND", if production { "s3" } else { "local" }),
            s3_endpoint: var("S3_ENDPOINT"),
            s3_region: var_or("S3_REGION", "us-east-1"),
            s3_bucket: var("S3_BUCKET"),
            s3_access_key: var("S3_ACCESS_KEY"),
            s3_secret_key: var("S3_SECRET_KEY"),
            s3_path_style: flag("S3_PATH_STYLE", true),
            storage_public_base_url: var_or("STORAGE_PUBLIC_BASE_URL", "http://localhost:8080/assets"),
            local_storage_dir: var_or("LOCAL_STORAGE_DIR", "./.local-assets"),
            url_allowlist: var("URL_ALLOWLIST").map(|_| list("URL_ALLOWLIST")),
            url_denylist: list("URL_DENYLIST"),
            allow_http: flag("ALLOW_HTTP", false),
            egress_proxy: var("EGRESS_PROXY"),
            flag_http: flag("EXTRACTOR_HTTP_ENABLED", true),
            flag_obscura: flag("EXTRACTOR_OBSCURA_ENABLED", true),
            flag_firecrawl: flag("EXTRACTOR_FIRECRAWL_ENABLED", true),
            prefer_firecrawl: flag("PREFER_FIRECRAWL", false),
            respect_robots: flag("RESPECT_ROBOTS", true),
            disabled_tools: list("DISABLED_TOOLS").into_iter().collect(),
            rate_tenant_per_min: num("RATE_TENANT_PER_MIN", 600)?,
            rate_brand_per_min: num("RATE_BRAND_PER_MIN", 300)?,
            rate_tool_per_min: num("RATE_TOOL_PER_MIN", 120)?,
            rate_provider_per_min: num("RATE_PROVIDER_PER_MIN", 600)?,
            tool_timeout: Duration::from_secs(num("TOOL_TIMEOUT_SECS", 90)?),
            review_url_template: var_or("REVIEW_URL_TEMPLATE", "https://app.letwemarket.com/brands/{brand_id}/content/review/{draft_id}"),
            usage_webhook_url: var("USAGE_WEBHOOK_URL"),
            usage_webhook_secret: var("USAGE_WEBHOOK_SECRET"),
            otlp_endpoint: var("OTEL_EXPORTER_OTLP_ENDPOINT"),
            worker_concurrency: num("WORKER_CONCURRENCY", 4)?,
            console_enabled: flag("CONSOLE_ENABLED", false),
            retention_source_days: var("RETENTION_SOURCE_DAYS")
                .map(|v| v.parse())
                .transpose()
                .map_err(|_| anyhow::anyhow!("RETENTION_SOURCE_DAYS must be a number"))?,
            env,
        };
        cfg.validate(production)?;
        Ok(cfg)
    }

    fn validate(&self, production: bool) -> anyhow::Result<()> {
        if self.console_enabled && self.env == "production" {
            bail!("CONSOLE_ENABLED must be off in production");
        }
        if production {
            if self.database_url.is_none() {
                bail!("DATABASE_URL is required in {}", self.env);
            }
            if self.llm_provider == "mock" {
                bail!("LLM_PROVIDER=mock is not allowed in {}", self.env);
            }
            if self.image_provider == "mock" {
                bail!("IMAGE_PROVIDER=mock is not allowed in {}", self.env);
            }
            if self.storage_kind != "s3" {
                bail!("STORAGE_KIND must be s3 in {}", self.env);
            }
            if self.allow_http {
                bail!("ALLOW_HTTP must be off in {}", self.env);
            }
        }
        if self.storage_kind == "s3" && (self.s3_endpoint.is_none() || self.s3_bucket.is_none() || self.s3_access_key.is_none() || self.s3_secret_key.is_none())
        {
            bail!("S3_ENDPOINT, S3_BUCKET, S3_ACCESS_KEY and S3_SECRET_KEY are required for STORAGE_KIND=s3");
        }
        if matches!(self.llm_provider.as_str(), "anthropic" | "openai") && self.llm_api_key.is_none() {
            bail!("LLM_API_KEY is required for LLM_PROVIDER={}", self.llm_provider);
        }
        Ok(())
    }

    pub fn is_production(&self) -> bool {
        self.env == "production"
    }

    pub fn review_url(&self, brand_id: &str, draft_id: &str) -> String {
        self.review_url_template.replace("{brand_id}", brand_id).replace("{draft_id}", draft_id)
    }

    /// Minimal config for tests: nothing external, nothing persistent.
    pub fn for_tests() -> Self {
        let e = |_: &str| None::<String>;
        let _ = e;
        Self {
            env: "test".into(),
            bind_addr: "127.0.0.1:0".into(),
            database_url: None,
            db_max_connections: 2,
            auth_secrets: vec!["test-secret-test-secret-test-secret-123".into()],
            max_body_bytes: 1_048_576,
            llm_provider: "mock".into(),
            llm_api_key: None,
            llm_base_url: None,
            llm_model: "mock-1".into(),
            llm_price_in_per_mtok: 0.0,
            llm_price_out_per_mtok: 0.0,
            image_provider: "mock".into(),
            image_model: "x".into(),
            zernio_base_url: "http://zernio.invalid".into(),
            zernio_api_key: None,
            firecrawl_base_url: "http://firecrawl.invalid".into(),
            firecrawl_api_key: None,
            github_base_url: "http://github.invalid".into(),
            github_token: None,
            x_base_url: "http://x.invalid".into(),
            x_bearer_token: None,
            youtube_base_url: "http://yt.invalid".into(),
            youtube_api_key: None,
            obscura_bin: "obscura".into(),
            obscura_max_concurrency: 2,
            obscura_timeout: Duration::from_secs(10),
            storage_kind: "memory".into(),
            s3_endpoint: None,
            s3_region: "us-east-1".into(),
            s3_bucket: None,
            s3_access_key: None,
            s3_secret_key: None,
            s3_path_style: true,
            storage_public_base_url: "https://storage.test".into(),
            local_storage_dir: "./.local-assets".into(),
            url_allowlist: None,
            url_denylist: vec![],
            allow_http: false,
            egress_proxy: None,
            flag_http: true,
            flag_obscura: true,
            flag_firecrawl: true,
            prefer_firecrawl: false,
            respect_robots: false,
            disabled_tools: HashSet::new(),
            rate_tenant_per_min: 10_000,
            rate_brand_per_min: 10_000,
            rate_tool_per_min: 10_000,
            rate_provider_per_min: 10_000,
            tool_timeout: Duration::from_secs(30),
            review_url_template: "https://app.letwemarket.com/brands/{brand_id}/content/review/{draft_id}".into(),
            usage_webhook_url: None,
            usage_webhook_secret: None,
            otlp_endpoint: None,
            worker_concurrency: 2,
            retention_source_days: None,
            console_enabled: false,
        }
    }
}

pub fn require<T>(v: Option<T>, name: &str) -> anyhow::Result<T> {
    v.with_context(|| format!("{name} is not set"))
}
