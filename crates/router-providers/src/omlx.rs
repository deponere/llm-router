//! oMLX-Client (<https://omlx.ai/>): lokaler MLX-Server für Apple Silicon.
//!
//! oMLX ist OpenAI-kompatibel (`/v1/models`, `/v1/chat/completions` mit SSE,
//! `/v1/messages`) — wir können den Chat-Stream 1:1 durchreichen und brauchen
//! keine JSONL-Übersetzung.

use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use reqwest::Client;
use router_config::{OMlxBackendConfig, RegistryConfig};
use router_core::registry::{Backend, CapsSet, ModalitySet, ModelCandidate, PrivacyClass};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum OMlxError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstream returned {status}: {body}")]
    Upstream { status: u16, body: String },
}

#[derive(Debug, Clone)]
pub struct OMlxClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
}

impl OMlxClient {
    pub fn new(cfg: &OMlxBackendConfig) -> Self {
        let base = cfg
            .base_url_env
            .as_deref()
            .and_then(|k| std::env::var(k).ok())
            .unwrap_or_else(|| cfg.base_url_default.clone());
        let api_key = cfg
            .api_key_env
            .as_deref()
            .and_then(|k| std::env::var(k).ok());
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: base.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        req
    }

    pub async fn list_models(
        &self,
        registry_cfg: &RegistryConfig,
    ) -> Result<Vec<ModelCandidate>, OMlxError> {
        let url = format!("{}/v1/models", self.base_url);
        let req = self.apply_headers(self.http.get(url));
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_connect() || e.is_timeout() => {
                tracing::debug!(error=%e, "omlx not reachable, skipping");
                return Ok(Vec::new());
            }
            Err(e) => return Err(OMlxError::Http(e)),
        };
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OMlxError::Upstream { status, body });
        }
        let parsed: ModelsResponse = resp.json().await?;
        Ok(parsed
            .data
            .into_iter()
            .map(|m| from_api_model(m, registry_cfg))
            .collect())
    }

    /// Pipet einen OpenAI-Chat-Completions-Body gegen oMLX. Antwort kommt
    /// bereits als OpenAI-SSE-Byte-Stream zurück und wird ungekapselt
    /// weitergereicht.
    pub async fn chat_completion_stream(
        &self,
        model_id: &str,
        request: serde_json::Value,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, OMlxError> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut body = request;
        if let serde_json::Value::Object(map) = &mut body {
            map.insert("model".into(), serde_json::Value::String(model_id.into()));
            map.insert("stream".into(), serde_json::Value::Bool(true));
            // "provider"-Block ist OpenRouter-spezifisch — falls er aus einem
            // vorherigen Mapping im Body hängt, rauswerfen.
            map.remove("provider");
        }
        let req = self
            .apply_headers(self.http.post(url).json(&body))
            .header("Accept", "text/event-stream");
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OMlxError::Upstream { status, body });
        }
        Ok(resp.bytes_stream())
    }
}

// --- API-Types ---

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
    /// oMLX liefert in Custom-Builds teilweise Zusatzfelder; wir nehmen, was da ist.
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    max_context_length: Option<u32>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    modalities: Vec<String>,
}

fn from_api_model(m: ApiModel, cfg: &RegistryConfig) -> ModelCandidate {
    // Default-Context 32k, wenn oMLX kein Feld mitliefert. Bei modernen
    // MLX-Modellen (Qwen3, Llama3) ist 32k eine sichere Untergrenze; exakte
    // Werte überschreibt das TOML-Overlay.
    let ctx = m
        .context_length
        .or(m.max_context_length)
        .unwrap_or(32_768);
    let mut input_modalities = if m.modalities.is_empty() {
        ModalitySet::text_only()
    } else {
        ModalitySet::from_strings(&m.modalities)
    };
    let mut supports = CapsSet::from_supported_parameters(&m.capabilities);

    // Heuristik über ID — greift, wenn das oMLX-Deployment keine Metadaten
    // mitliefert. Overrides im TOML gewinnen gegenüber beidem.
    let name_lc = m.id.to_ascii_lowercase();
    if name_lc.contains("vl") || name_lc.contains("vision") || name_lc.contains("llava") {
        input_modalities = input_modalities.with_image();
    }

    for ov in &cfg.overrides {
        if ov.backend.eq_ignore_ascii_case("omlx") && m.id.starts_with(&ov.id_prefix) {
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
        backend: Backend::OMlx,
        provider_slug: m.owned_by.unwrap_or_else(|| "omlx".into()),
        context_length: ctx,
        max_completion_tokens: None,
        price_in_per_mtok: 0.0,
        price_out_per_mtok: 0.0,
        input_modalities,
        supports,
        is_moderated: false,
        privacy_class: PrivacyClass::Local,
        measured_p95_ms: None,
        intelligence_index: None,
        id: m.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_heuristic_detects_vl_suffix() {
        let m = ApiModel {
            id: "qwen2.5-vl-7b".into(),
            owned_by: None,
            context_length: Some(32_000),
            max_context_length: None,
            capabilities: vec![],
            modalities: vec![],
        };
        let c = from_api_model(m, &RegistryConfig::default());
        assert!(c.input_modalities.has_image());
        assert_eq!(c.backend, Backend::OMlx);
        assert_eq!(c.privacy_class, PrivacyClass::Local);
    }

    #[test]
    fn override_caps_win() {
        let mut cfg = RegistryConfig::default();
        cfg.overrides.push(router_config::RegistryOverride {
            backend: "OMlx".into(),
            id_prefix: "qwen3".into(),
            input_modalities: vec![],
            caps: vec!["tools".into()],
        });
        let m = ApiModel {
            id: "qwen3-32b".into(),
            owned_by: None,
            context_length: Some(32_000),
            max_context_length: None,
            capabilities: vec![],
            modalities: vec![],
        };
        let c = from_api_model(m, &cfg);
        assert!(c.supports.has_tools());
    }
}
