//! Gemeinsame Abstraktion über alle Egress-Backends. Jede konfigurierte
//! Backend-Instanz (OpenAI, Groq, Anthropic, oMLX, OpenRouter …) implementiert
//! diesen Trait; die Registry hält sie als `Arc<dyn Provider>` und dispatcht
//! über die Backend-ID.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use router_config::{AuthConfig, RegistryConfig};
use router_core::profile::ResolvedProfile;
use router_core::registry::ModelCandidate;

/// Backend-Byte-Stream (SSE), auf `std::io::Error` normalisiert.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("missing credential in env var {0}")]
    MissingCredential(String),
    #[error("upstream returned {status}: {body}")]
    Upstream { status: u16, body: String },
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// Backend-ID (Config-Name), zugleich `backend_id` aller Modelle.
    fn id(&self) -> &str;
    /// Lokales Backend (Tiebreak-Vorrang + Privacy-Default `Local`).
    fn is_local(&self) -> bool;
    /// Katalog dieses Backends. `cfg` liefert Overrides/Heuristik-Regeln.
    async fn list_models(&self, cfg: &RegistryConfig) -> Result<Vec<ModelCandidate>, ProviderError>;
    /// Öffnet den Chat-Completions-SSE-Stream für ein Modell. `body` ist ein
    /// OpenAI-Chat-Completions-Request; Nicht-OpenAI-Backends übersetzen intern.
    async fn chat_completion_stream(
        &self,
        model_id: &str,
        profile: &ResolvedProfile,
        body: serde_json::Value,
    ) -> Result<ByteStream, ProviderError>;
}

/// Löst das Auth-Secret einer Backend-Instanz auf. `None` bei `AuthConfig::None`;
/// `Err`, wenn eine env-Variable erwartet, aber nicht gesetzt ist.
pub fn resolve_secret(auth: &AuthConfig) -> Result<Option<String>, ProviderError> {
    match auth {
        AuthConfig::None => Ok(None),
        AuthConfig::ApiKey { env } => match std::env::var(env) {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            _ => Err(ProviderError::MissingCredential(env.clone())),
        },
    }
}
