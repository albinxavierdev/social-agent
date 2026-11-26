use content_agent::{api, app::AppState, config::Config, observability, openapi};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--print-openapi") {
        println!("{}", serde_json::to_string_pretty(&openapi::spec())?);
        return Ok(());
    }
    let metrics = observability::init();
    let cfg = Config::from_env()?;
    tracing::info!(config = ?cfg, "starting content-agent-api");
    let addr: SocketAddr = cfg.bind_addr.parse()?;
    let state = AppState::build(cfg, Some(metrics)).await?;
    if std::env::args().any(|a| a == "--migrate-only") {
        tracing::info!("migrations applied; exiting (--migrate-only)");
        return Ok(());
    }
    let app = api::router(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, app).with_graceful_shutdown(observability::shutdown_signal()).await?;
    tracing::info!("api stopped");
    Ok(())
}
