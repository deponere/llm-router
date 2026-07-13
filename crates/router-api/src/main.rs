//! Router-API-Binary: Axum-Server mit OpenAI- und Anthropic-kompatiblen Endpunkten.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use router_api::state::AppState;
use router_config::Config;
use router_providers::{LatencyTracker, RegistryHandle};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // .env gewinnt gegenueber Shell-Env — so bleibt die Router-Config selbsterklaerend und unabhaengig von globalen zshrc-Variablen.
    let _ = dotenvy::dotenv_override();
    init_tracing();

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
    };

    let app: Router = router_api::routes::build(state);

    tracing::info!(%bind, "router listening");
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,router_api=debug,router_core=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
}
