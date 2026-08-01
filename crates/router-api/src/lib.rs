//! Ingress-Layer. Axum-Routen für OpenAI- und Anthropic-kompatible Clients.

pub mod state;
pub mod routes;
pub mod openai;
pub mod anthropic;
pub mod sse;
pub mod alerts;
pub mod auth;
pub mod benchmark;
pub mod configedit;
pub mod debug;
pub mod error;
pub mod history;
pub mod logbuf;
pub mod routing;
pub mod rotate;
pub mod store;
pub mod ui;

pub use error::ApiError;
pub use state::AppState;
