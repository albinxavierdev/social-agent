//! Obscura adapter (spec §10). Obscura (https://github.com/h4ckf0r0day/obscura) is driven through its
//! CLI (`obscura fetch`) as one short-lived OS process per job, which gives us for free:
//!
//! * a fresh cookie/session store per job (nothing persists between jobs or tenants),
//! * hard wall-clock, output-size and concurrency limits (semaphore-based browser pool),
//! * crash isolation (a wedged page cannot take down the worker).
//!
//! `--allow-private-network` is never passed, so Obscura keeps its own SSRF protection on top of ours.
//! Per-job CPU/memory ceilings must be applied by the container/cgroup the worker pool runs in.

use super::html::{lead_summary, parse};
use super::{ContentExtractor, ExtractedContent, ExtractedMetadata, ExtractionError, ExtractionRequest};
use crate::ssrf::SafeFetcher;
use crate::storage::ObjectStorage;
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;

#[derive(Debug, Clone)]
pub struct ObscuraConfig {
    pub binary: PathBuf,
    pub max_concurrency: usize,
    pub navigation_timeout: Duration,
    pub total_timeout: Duration,
    pub max_output_bytes: usize,
    pub pool_wait: Duration,
    pub proxy: Option<String>,
}

impl Default for ObscuraConfig {
    fn default() -> Self {
        Self {
            binary: "obscura".into(),
            max_concurrency: 4,
            navigation_timeout: Duration::from_secs(20),
            total_timeout: Duration::from_secs(45),
            max_output_bytes: 8 * 1024 * 1024,
            pool_wait: Duration::from_secs(10),
            proxy: None,
        }
    }
}

pub struct ObscuraExtractor {
    cfg: ObscuraConfig,
    pool: Arc<Semaphore>,
    /// Used only for the pre-flight DNS/IP check; Obscura does its own fetching.
    guard: SafeFetcher,
    storage: Arc<dyn ObjectStorage>,
}

impl ObscuraExtractor {
    pub fn new(cfg: ObscuraConfig, guard: SafeFetcher, storage: Arc<dyn ObjectStorage>) -> Self {
        let pool = Arc::new(Semaphore::new(cfg.max_concurrency.max(1)));
        Self { cfg, pool, guard, storage }
    }

    /// Browser pool utilisation in [0,1] for metrics.
    pub fn utilisation(&self) -> f64 {
        let max = self.cfg.max_concurrency.max(1) as f64;
        (max - self.pool.available_permits() as f64) / max
    }

    fn build_args(&self, url: &str, screenshot: Option<&std::path::Path>) -> Vec<String> {
        let mut a: Vec<String> = vec![];
        if let Some(p) = &self.cfg.proxy {
            a.extend(["--proxy".into(), p.clone()]);
        }
        a.extend([
            "fetch".into(),
            url.into(),
            "--dump".into(),
            "html".into(),
            "--timeout".into(),
            self.cfg.navigation_timeout.as_secs().max(1).to_string(),
            "--wait-until".into(),
            "networkidle0".into(),
        ]);
        if let Some(p) = screenshot {
            a.extend(["--screenshot".into(), p.to_string_lossy().into_owned()]);
        }
        a
    }

    async fn run(&self, url: &str, screenshot: Option<&std::path::Path>, home: &std::path::Path) -> Result<Vec<u8>, ExtractionError> {
        let mut child = Command::new(&self.cfg.binary)
            .args(self.build_args(url, screenshot))
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", home) // isolated profile/cookie dir, deleted after the job
            .env("XDG_DATA_HOME", home)
            .env("XDG_CACHE_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    ExtractionError::NotConfigured("obscura binary not found (set OBSCURA_BIN)")
                } else {
                    ExtractionError::failed(format!("failed to start obscura: {e}"), true)
                }
            })?;

        let mut out = child.stdout.take().expect("piped stdout");
        let limit = self.cfg.max_output_bytes;
        let work = async {
            let mut buf = Vec::new();
            (&mut out).take(limit as u64 + 1).read_to_end(&mut buf).await.map_err(|e| ExtractionError::failed(format!("read failed: {e}"), true))?;
            if buf.len() > limit {
                return Err(ExtractionError::failed("rendered page exceeds output limit", false));
            }
            let status = child.wait().await.map_err(|e| ExtractionError::failed(format!("wait failed: {e}"), true))?;
            if !status.success() {
                return Err(ExtractionError::failed(format!("obscura exited with {status}"), true));
            }
            Ok(buf)
        };
        match tokio::time::timeout(self.cfg.total_timeout, work).await {
            Ok(r) => r,
            Err(_) => Err(ExtractionError::failed("obscura job timed out", true)), // kill_on_drop reaps the child
        }
    }
}

#[async_trait]
impl ContentExtractor for ObscuraExtractor {
    fn name(&self) -> &'static str {
        "obscura"
    }

    async fn extract(&self, req: &ExtractionRequest) -> Result<ExtractedContent, ExtractionError> {
        // SSRF pre-flight: resolve and reject non-public targets before spawning a browser.
        self.guard.resolve_public(&req.url).await?;

        let permit = tokio::time::timeout(self.cfg.pool_wait, self.pool.clone().acquire_owned())
            .await
            .map_err(|_| ExtractionError::failed("browser pool saturated", true))?
            .map_err(|_| ExtractionError::failed("browser pool closed", true))?;
        metrics::gauge!("browser_pool_utilisation").set(self.utilisation());

        let home = std::env::temp_dir().join(format!("obscura-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&home).await.map_err(|e| ExtractionError::failed(format!("tmp dir: {e}"), true))?;
        let shot_path = home.join("page.png");
        let want_shot = req.want_screenshot;

        let result = self.run(req.url.as_str(), want_shot.then_some(shot_path.as_path()), &home).await;
        let shot_bytes = if want_shot && result.is_ok() { tokio::fs::read(&shot_path).await.ok() } else { None };
        // Always wipe the per-job profile: cookies and session state never outlive the job.
        let _ = tokio::fs::remove_dir_all(&home).await;
        drop(permit);
        metrics::gauge!("browser_pool_utilisation").set(self.utilisation());

        let html_bytes = result?;
        let html = String::from_utf8_lossy(&html_bytes);
        let p = parse(&html, &req.url);
        if p.text.trim().is_empty() {
            return Err(ExtractionError::failed("rendered page has no extractable text", false));
        }

        let screenshot_url = match shot_bytes {
            Some(b) if !b.is_empty() => {
                let key = format!("{}/{}/screenshots/{}.png", req.tenant_id, req.brand_id, req.source_id);
                Some(self.storage.put(&key, b, "image/png").await?)
            }
            _ => None,
        };
        if want_shot && screenshot_url.is_none() {
            return Err(ExtractionError::failed("screenshot was requested but not produced", true));
        }

        Ok(ExtractedContent {
            summary: lead_summary(&p.text, p.og_description.as_deref()),
            title: p.og_title.clone().or(p.title.clone()),
            author: p.author,
            published_at: p.published_at,
            content_type: "article".into(),
            language: p.language.unwrap_or_else(|| "und".into()),
            text: p.text,
            metadata: ExtractedMetadata {
                og_title: p.og_title,
                og_description: p.og_description,
                canonical_url: p.canonical_url,
                images: p.images,
                extra: serde_json::json!({ "rendered": true }),
            },
            screenshot_url,
            extraction_method: "obscura".into(),
            extraction_status: "success".into(),
        })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ssrf::FetchLimits;
    use crate::storage::MemoryStorage;
    use crate::urls::{SourceType, UrlPolicy};
    use std::os::unix::fs::PermissionsExt;

    /// A fake `obscura` that prints a JS-rendered page and writes a "screenshot" when asked.
    fn fake_bin(dir: &std::path::Path, body: &str) -> PathBuf {
        let p = dir.join("obscura");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn req(url: &str, shot: bool) -> ExtractionRequest {
        ExtractionRequest {
            source_id: "src_1".into(),
            tenant_id: "t1".into(),
            brand_id: "b1".into(),
            url: url::Url::parse(url).unwrap(),
            source_type: SourceType::WebPage,
            want_screenshot: shot,
        }
    }

    fn extractor(bin: PathBuf, storage: Arc<MemoryStorage>, total: Duration) -> ObscuraExtractor {
        let guard = SafeFetcher::new(UrlPolicy::default().dangerously_allow_private_networks(), FetchLimits::default());
        ObscuraExtractor::new(ObscuraConfig { binary: bin, total_timeout: total, ..Default::default() }, guard, storage)
    }

    const HTML: &str = "<html lang=en><head><title>Rendered</title></head><body><main><p>This content only exists after JavaScript ran in the browser and it is long enough to pass the minimum text threshold for extraction results.</p><p>More rendered content follows here to be safe and sound.</p></main></body></html>";

    #[tokio::test]
    async fn extracts_rendered_html_and_screenshot_and_cleans_home() {
        let tmp = tempdir();
        let script = format!(
            "for a in \"$@\"; do if [ \"$prev\" = \"--screenshot\" ]; then printf PNGDATA > \"$a\"; fi; prev=\"$a\"; done\necho '{HTML}'\necho \"$HOME\" > \"{}/home.txt\"",
            tmp.display()
        );
        let bin = fake_bin(&tmp, &script);
        let storage = Arc::new(MemoryStorage::default());
        let ex = extractor(bin, storage.clone(), Duration::from_secs(10));
        let out = ex.extract(&req("http://127.0.0.1/app", true)).await.unwrap();
        assert_eq!(out.extraction_method, "obscura");
        assert!(out.text.contains("only exists after JavaScript"));
        assert_eq!(out.screenshot_url.as_deref(), Some("https://storage.test/t1/b1/screenshots/src_1.png"));
        assert_eq!(storage.objects.lock().unwrap()["t1/b1/screenshots/src_1.png"].0, b"PNGDATA");
        let home = std::fs::read_to_string(tmp.join("home.txt")).unwrap();
        assert!(!std::path::Path::new(home.trim()).exists(), "per-job profile must be wiped");
    }

    #[tokio::test]
    async fn times_out_and_reports_retryable() {
        let tmp = tempdir();
        let ex = extractor(fake_bin(&tmp, "sleep 30"), Arc::new(MemoryStorage::default()), Duration::from_millis(300));
        let err = ex.extract(&req("http://127.0.0.1/slow", false)).await.unwrap_err();
        assert!(err.retryable(), "{err}");
    }

    #[tokio::test]
    async fn missing_binary_is_reported_as_not_configured() {
        let ex = extractor("/nonexistent/obscura".into(), Arc::new(MemoryStorage::default()), Duration::from_secs(1));
        assert!(matches!(ex.extract(&req("http://127.0.0.1/", false)).await.unwrap_err(), ExtractionError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn blocks_private_targets_before_spawning() {
        let guard = SafeFetcher::new(UrlPolicy::default(), FetchLimits::default()); // private NOT allowed
        let ex = ObscuraExtractor::new(ObscuraConfig::default(), guard, Arc::new(MemoryStorage::default()));
        assert!(matches!(ex.extract(&req("https://127.0.0.1/", false)).await.unwrap_err(), ExtractionError::Api(_)));
    }

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("obscura-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
