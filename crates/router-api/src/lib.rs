//! Ingress-Layer. Axum-Routen für OpenAI- und Anthropic-kompatible Clients.

pub mod state;
pub mod routes;
pub mod openai;
pub mod anthropic;
pub mod debug;
pub mod history;
pub mod error;
pub mod routing;

pub use error::ApiError;
pub use state::AppState;
