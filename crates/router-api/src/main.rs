//! Router-API-Binary: Axum-Server mit OpenAI- und Anthropic-kompatiblen Endpunkten.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use router_api::rotate::Rotator;
use router_api::state::AppState;
use router_config::Config;
use router_providers::{LatencyTracker, RegistryHandle};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env gewinnt gegenueber Shell-Env — so bleibt die Router-Config selbsterklaerend und unabhaengig von globalen zshrc-Variablen.
    let _ = dotenvy::dotenv_override();
    let logbuf = router_api::logbuf::LogBuffer::new(500);
    init_tracing(logbuf.clone());

    let cfg_path = std::env::var("ROUTER_CONFIG")
        .unwrap_or_else(|_| "config/router.toml".to_string());
    tracing::info!(%cfg_path, "loading config");
    let config = Config::load(&cfg_path)?;
    let bind: SocketAddr = config.server.bind.parse()?;

    let tracker = LatencyTracker::new();
    let registry = RegistryHandle::new(&config, tracker.clone());
    let state = AppState {
        config: Arc::new(config),
        registry: Arc::new(registry),
        tracker,
        history: router_api::history::TransactionHistory::new(),
        rotator: Arc::new(Rotator::from_env()),
        logs: logbuf,
    };

    let app: Router = router_api::routes::build(state);

    tracing::info!(%bind, "router listening");
    let listener = bind_with_retry(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Bindet mit kurzem Retry — nötig nach `POST /v1/admin/restart`: der neue
/// Prozess startet, bevor der alte den Port freigegeben hat.
async fn bind_with_retry(bind: SocketAddr) -> anyhow::Result<tokio::net::TcpListener> {
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..5 {
        match tokio::net::TcpListener::bind(bind).await {
            Ok(l) => return Ok(l),
            Err(e) => {
                tracing::warn!(%bind, attempt, error = %e, "bind failed, retrying");
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("bind failed")).into())
}

fn init_tracing(logbuf: router_api::logbuf::LogBuffer) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,router_api=debug,router_core=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(router_api::logbuf::LoguruLayer::new(logbuf))
        .init();
}
