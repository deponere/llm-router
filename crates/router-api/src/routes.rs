use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Obergrenze für eingehende Request-Bodies (8 MiB) — deckt große Multimodal-/Tool-Payloads ab, verhindert aber unbegrenzte Allokation pro Request.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub fn build(state: AppState) -> Router {
    // ponytail: allow_origin(Any) ist für local-only bewusst offen, bei Netzwerk-Bind auf konkrete Origins einschränken (AllowOrigin::list / ::predicate).
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    Router::new()
        .route("/", get(crate::ui::index))
        .route("/ui", get(crate::ui::index))
        .route("/v1/models", get(crate::openai::list_models))
        .route("/v1/registry", get(crate::debug::registry))
        .route("/v1/intelligence", get(crate::debug::intelligence))
        .route("/v1/transactions", get(crate::debug::transactions))
        .route("/v1/logs", get(crate::debug::logs))
        .route("/v1/logs/clear", post(crate::debug::logs_clear))
        .route("/v1/explain", post(crate::debug::explain))
        .route("/v1/admin/restart", post(crate::debug::restart))
        .route(
            "/v1/chat/completions",
            post(crate::openai::chat_completions),
        )
        .route("/v1/messages", post(crate::anthropic::messages))
        .route("/healthz", get(healthz))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(state)
}

async fn healthz() -> &'static str { "ok" }
