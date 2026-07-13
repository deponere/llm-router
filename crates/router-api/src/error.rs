use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("no candidate model: {0}")]
    NoCandidate(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ApiError::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            ApiError::NoCandidate(_) => (StatusCode::SERVICE_UNAVAILABLE, "no_candidate"),
            ApiError::Upstream(_) => (StatusCode::BAD_GATEWAY, "upstream_error"),
            ApiError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        // Bei 4xx/5xx die Ursache auch auf stderr loggen — sonst sieht man nur "response failed classification=...", aber nicht warum.
        let msg = self.to_string();
        const RED: &str = "\x1b[31m";
        const DIM: &str = "\x1b[2m";
        const RESET: &str = "\x1b[0m";
        eprintln!("{RED}✗ {status} {code}{RESET} {DIM}{msg}{RESET}");
        let body = Json(json!({
            "error": {
                "message": msg,
                "type": code,
            }
        }));
        (status, body).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError::Internal(e.to_string()) }
}
