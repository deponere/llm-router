//! Generischer OpenAI-kompatibler Client. Eine Instanz pro konfiguriertem Backend (OpenAI, Groq, DeepSeek, xAI, Mistral, Gemini-OpenAI-Endpoint, Ollama, LM Studio, oMLX …); ruft `{base_url}/models` und `{base_url}/chat/completions` und reicht den SSE-Stream 1:1 durch.

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use router_config::{AuthConfig, BackendConfig, RegistryConfig};
use router_core::profile::ResolvedProfile;
use router_core::registry::{CapsSet, ModalitySet, ModelCandidate, PrivacyClass};
use serde::Deserialize;

use crate::provider::{resolve_secret, ByteStream, Provider, ProviderError};

#[derive(Debug, Clone)]
pub struct OpenAiCompatClient {
    id: String,
    http: Client,
    base_url: String,
    auth: AuthConfig,
    is_local: bool,
}

impl OpenAiCompatClient {
    pub fn new(id: &str, cfg: &BackendConfig) -> Self {
        // Lokale Server dürfen langsam anspringen; Cloud-Provider knapper.
        let connect = if cfg.local { 3 } else { 10 };
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(connect))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self {
            id: id.to_string(),
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            auth: cfg.auth.clone(),
            is_local: cfg.local,
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, ProviderError> {
        match resolve_secret(&self.auth)? {
            Some(k) => Ok(req.bearer_auth(k)),
            None => Ok(req),
        }
    }
}

#[async_trait]
impl Provider for OpenAiCompatClient {
    fn id(&self) -> &str { &self.id }
    fn is_local(&self) -> bool { self.is_local }

    async fn list_models(&self, cfg: &RegistryConfig) -> Result<Vec<ModelCandidate>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let resp = match self.apply_auth(self.http.get(url))?.send().await {
            Ok(r) => r,
            // Lokale Server sind oft nicht erreichbar — leise überspringen.
            Err(e) if self.is_local && (e.is_connect() || e.is_timeout()) => {
                tracing::debug!(backend = %self.id, error = %e, "backend not reachable, skipping");
                return Ok(Vec::new());
            }
            Err(e) => return Err(ProviderError::Http(e)),
        };
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream { status, body });
        }
        let parsed: ModelsResponse = resp.json().await?;
        Ok(parsed
            .data
            .into_iter()
            .map(|m| self.to_candidate(m, cfg))
            .collect())
    }

    async fn chat_completion_stream(
        &self,
        model_id: &str,
        _profile: &ResolvedProfile,
        request: serde_json::Value,
    ) -> Result<ByteStream, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = request;
        if let serde_json::Value::Object(map) = &mut body {
            map.insert("model".into(), serde_json::Value::String(model_id.into()));
            map.insert("stream".into(), serde_json::Value::Bool(true));
            // "provider"-Block ist OpenRouter-spezifisch — falls aus einem vorherigen Mapping hängengeblieben, rauswerfen.
            map.remove("provider");
        }
        let req = self
            .apply_auth(self.http.post(url).json(&body))?
            .header("Accept", "text/event-stream");
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream { status, body });
        }
        let stream = resp.bytes_stream().map(|r| r.map_err(std::io::Error::other));
        Ok(Box::pin(stream) as ByteStream)
    }
}

impl OpenAiCompatClient {
    /// Baut aus einem `/models`-Eintrag einen Kandidaten. OpenAI-compat-Server liefern oft nur `id` — Context/Modalitäten/Caps ergänzen Heuristik über die ID und (Vorrang) `[[registry.overrides]]` aus der Config.
    fn to_candidate(&self, m: ApiModel, cfg: &RegistryConfig) -> ModelCandidate {
        let ctx = m.context_length.or(m.max_context_length).unwrap_or(32_768);
        let mut input_modalities = if m.modalities.is_empty() {
            ModalitySet::text_only()
        } else {
            ModalitySet::from_strings(&m.modalities)
        };
        let mut supports = CapsSet::from_supported_parameters(&m.capabilities);

        let name_lc = m.id.to_ascii_lowercase();
        if name_lc.contains("vl") || name_lc.contains("vision") || name_lc.contains("llava") {
            input_modalities = input_modalities.with_image();
        }

        for ov in &cfg.overrides {
            if ov.backend.eq_ignore_ascii_case(&self.id)
                && name_lc.starts_with(&ov.id_prefix.to_ascii_lowercase())
            {
                if !ov.input_modalities.is_empty() {
                    input_modalities = ModalitySet::from_strings(&ov.input_modalities);
                }
                let mut caps = CapsSet::default();
                for c in &ov.caps {
                    match c.as_str() {
                        "tools" => caps = caps.with_tools(),
                        "json_mode" => caps = caps.with_json_mode(),
                        "structured_outputs" => caps = caps.with_structured_outputs(),
                        "reasoning" => caps = caps.with_reasoning(),
                        _ => {}
                    }
                }
                supports = caps;
            }
        }

        ModelCandidate {
            backend_id: self.id.clone(),
            tiebreak_priority: if self.is_local { 0 } else { 1 },
            provider_slug: m.owned_by.unwrap_or_else(|| self.id.clone()),
            context_length: ctx,
            max_completion_tokens: None,
            // Preise kennt ein generischer OpenAI-compat-Endpoint nicht; lokale Server sind gratis, Cloud-Preise trägt ggf. ein Override nach.
            price_in_per_mtok: 0.0,
            price_out_per_mtok: 0.0,
            input_modalities,
            supports,
            is_moderated: false,
            privacy_class: if self.is_local { PrivacyClass::Local } else { PrivacyClass::Standard },
            measured_p95_ms: None,
            intelligence_index: None,
            id: m.id,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    max_context_length: Option<u32>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    modalities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: &str, local: bool) -> OpenAiCompatClient {
        OpenAiCompatClient::new(
            id,
            &BackendConfig {
                enabled: true,
                kind: router_config::BackendKind::OpenaiCompat,
                base_url: "http://x/v1".into(),
                auth: AuthConfig::None,
                local,
                app_referer: None,
                app_title: None,
                anthropic_version: None,
            },
        )
    }

    #[test]
    fn vision_heuristic_and_local_privacy() {
        let m = ApiModel {
            id: "qwen2.5-vl-7b".into(),
            owned_by: None,
            context_length: Some(32_000),
            max_context_length: None,
            capabilities: vec![],
            modalities: vec![],
        };
        let c = client("omlx", true).to_candidate(m, &RegistryConfig::default());
        assert!(c.input_modalities.has_image());
        assert_eq!(c.backend_id, "omlx");
        assert_eq!(c.tiebreak_priority, 0);
        assert_eq!(c.privacy_class, PrivacyClass::Local);
    }

    #[test]
    fn override_caps_win_and_remote_is_standard() {
        let mut cfg = RegistryConfig::default();
        cfg.overrides.push(router_config::RegistryOverride {
            backend: "groq".into(),
            id_prefix: "llama".into(),
            input_modalities: vec![],
            caps: vec!["tools".into()],
        });
        let m = ApiModel {
            id: "llama-3.3-70b".into(),
            owned_by: None,
            context_length: Some(128_000),
            max_context_length: None,
            capabilities: vec![],
            modalities: vec![],
        };
        let c = client("groq", false).to_candidate(m, &cfg);
        assert!(c.supports.has_tools());
        assert_eq!(c.tiebreak_priority, 1);
        assert_eq!(c.privacy_class, PrivacyClass::Standard);
    }
}
