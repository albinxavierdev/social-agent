//! Structured JSON logs + Prometheus metrics (spec §14).
//! Traces: every log line and persisted record carries trace_id / tenant_id / brand_id / job_id / draft_id /
//! publish_request_id, so the full path (request → job → extraction → report → draft → validation → review →
//! Zernio) can be followed in any log backend. OTLP export is enabled when OTEL_EXPORTER_OTLP_ENDPOINT is set
//! (see `init`).

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init() -> PrometheusHandle {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,hyper=warn"));
    tracing_subscriber::registry().with(filter).with(fmt::layer().json().with_current_span(false).flatten_event(true)).init();

    PrometheusBuilder::new()
        .set_buckets_for_metric(Matcher::Suffix("_seconds".into()), &[0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0])
        .expect("valid buckets")
        .install_recorder()
        .expect("metrics recorder installs once")
}

/// Wait for SIGTERM/SIGINT (graceful shutdown).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}
