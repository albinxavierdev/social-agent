use content_agent::{app::AppState, config::Config, observability, worker};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let metrics = observability::init();
    let cfg = Config::from_env()?;
    if cfg.database_url.is_none() {
        anyhow::bail!("the worker requires DATABASE_URL: an in-memory queue cannot be shared with the API process");
    }
    tracing::info!(config = ?cfg, "starting content-agent-worker");
    let state = AppState::build(cfg, Some(metrics)).await?;

    let shutdown = CancellationToken::new();
    let s = shutdown.clone();
    tokio::spawn(async move {
        observability::shutdown_signal().await;
        tracing::info!("shutdown signal received: draining in-flight tasks");
        s.cancel();
    });
    worker::run(state, shutdown).await;
    Ok(())
}
