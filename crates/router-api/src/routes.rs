use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Obergrenze für eingehende Request-Bodies (8 MiB). Deckt große
/// Multimodal-/Tool-Payloads ab, verhindert aber unbegrenzte Allokation aus
/// einem einzelnen Request.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub fn build(state: AppState) -> Router {
    // ponytail: allow_origin(Any) ist für den local-only-Betrieb bewusst offen,
    // damit lokale Browser-UIs auf beliebigen Ports zugreifen können. Wenn der
    // Server je an eine Netzwerkadresse gebunden wird: auf konkrete Origins
    // einschränken (AllowOrigin::list / ::predicate).
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);

    Router::new()
        .route("/v1/models", get(crate::openai::list_models))
        .route("/v1/registry", get(crate::debug::registry))
        .route("/v1/intelligence", get(crate::debug::intelligence))
        .route("/v1/transactions", get(crate::debug::transactions))
        .route("/v1/explain", post(crate::debug::explain))
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
