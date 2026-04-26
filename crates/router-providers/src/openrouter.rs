//! OpenRouter-Client: Modell-Katalog und Chat-Completions.

use std::time::Duration;

use bytes::Bytes;
use futures::Stream;
use reqwest::Client;
use router_config::OpenRouterBackendConfig;
use router_core::profile::ResolvedProfile;
use router_core::registry::{Backend, CapsSet, ModalitySet, ModelCandidate, PrivacyClass};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum OpenRouterError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("missing api key env var {0}")]
    MissingApiKey(String),
    #[error("upstream returned {status}: {body}")]
    Upstream { status: u16, body: String },
}

#[derive(Debug, Clone)]
pub struct OpenRouterClient {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    app_referer: Option<String>,
    app_title: Option<String>,
}

impl OpenRouterClient {
    pub fn new(cfg: &OpenRouterBackendConfig) -> Self {
        let api_key = std::env::var(&cfg.api_key_env).ok();
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key,
            app_referer: cfg.app_referer.clone(),
            app_title: cfg.app_title.clone(),
        }
    }

    pub fn has_api_key(&self) -> bool { self.api_key.is_some() }

    fn apply_headers(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(k) = &self.api_key {
            req = req.bearer_auth(k);
        }
        if let Some(r) = &self.app_referer {
            req = req.header("HTTP-Referer", r);
        }
        if let Some(t) = &self.app_title {
            req = req.header("X-Title", t);
        }
        req
    }

    /// Holt die vollständige Modell-Liste.
    pub async fn list_models(&self) -> Result<Vec<ModelCandidate>, OpenRouterError> {
        let url = format!("{}/models", self.base_url);
        let req = self.apply_headers(self.http.get(url));
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(OpenRouterError::Upstream { status, body });
        }
        let parsed: ModelsResponse = resp.json().await?;
        Ok(parsed.data.into_iter().map(from_api_model).collect())
    }

    /// Baut den Body für `/chat/completions` inklusive `provider`-Block aus dem
    /// aufgelösten Profil und liefert den Response-Stream (SSE) zurück.
    pub async fn chat_completion_stream(
        &self,
        model_id: &str,
        profile: &ResolvedProfile,
        request: serde_json::Value,
    ) -> Result<impl Stream<Item = reqwest::Result<Bytes>>, OpenRouterError> {
        let Some(_) = &self.api_key else {
            return Err(OpenRouterError::MissingApiKey(
                "OPENROUTER_API_KEY".into(),
            ));
        };
        let url = format!("{}/chat/completions", self.base_url);
        let mut body = request;
        if let serde_json::Value::Object(map) = &mut body {
            map.insert("model".into(), serde_json::Value::String(model_id.into()));
            map.insert("stream".into(), serde_json::Value::Bool(true));
            // Kosten auch im Stream mitliefern lassen — OpenRouter haengt ein
            // zusaetzliches usage-Event mit `cost` an, sobald das hier true ist.
            map.insert(
                "usage".into(),
                serde_json::json!({ "include": true }),
            );
            if let Some(provider) = build_provider_block(profile) {
                map.insert("provider".into(), provider);
            }
        }

        tracing::debug!(
            target: "router_providers::openrouter",
            model = %model_id,
            body = %serde_json::to_string(&body).unwrap_or_default(),
            "→ openrouter POST /chat/completions"
        );
        let req = self
            .apply_headers(self.http.post(url).json(&body))
            .header("Accept", "text/event-stream");
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_txt = resp.text().await.unwrap_or_default();
            tracing::warn!(
                target: "router_providers::openrouter",
                status,
                response = %body_txt,
                sent_body = %serde_json::to_string(&body).unwrap_or_default(),
                "openrouter rejected request"
            );
            return Err(OpenRouterError::Upstream { status, body: body_txt });
        }
        Ok(resp.bytes_stream())
    }
}

/// Setzt den OpenRouter-`provider`-Block aus dem Profil. Gibt `None` zurück,
/// wenn keiner der Schalter gesetzt ist.
pub fn build_provider_block(profile: &ResolvedProfile) -> Option<serde_json::Value> {
    let mut obj = serde_json::Map::new();
    if let Some(b) = profile.provider_require_parameters {
        obj.insert("require_parameters".into(), serde_json::Value::Bool(b));
    }
    if let Some(b) = profile.provider_allow_fallbacks {
        obj.insert("allow_fallbacks".into(), serde_json::Value::Bool(b));
    }
    if let Some(b) = profile.provider_zdr {
        obj.insert("zdr".into(), serde_json::Value::Bool(b));
    }
    if let Some(v) = &profile.provider_data_collection {
        obj.insert(
            "data_collection".into(),
            serde_json::Value::String(v.clone()),
        );
    }
    if let Some(v) = &profile.provider_sort {
        obj.insert("sort".into(), serde_json::Value::String(v.clone()));
    }
    if !profile.provider_quantizations.is_empty() {
        obj.insert(
            "quantizations".into(),
            serde_json::Value::Array(
                profile
                    .provider_quantizations
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
    }
    if !profile.provider_only.is_empty() {
        obj.insert(
            "only".into(),
            serde_json::Value::Array(
                profile
                    .provider_only
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
    }
    if !profile.provider_ignore.is_empty() {
        obj.insert(
            "ignore".into(),
            serde_json::Value::Array(
                profile
                    .provider_ignore
                    .iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(cap) = profile.max_price_out_per_mtok {
        let mut mp = serde_json::Map::new();
        mp.insert("completion".into(), serde_json::json!(cap));
        if let Some(cap_in) = profile.max_price_in_per_mtok {
            mp.insert("prompt".into(), serde_json::json!(cap_in));
        }
        obj.insert("max_price".into(), serde_json::Value::Object(mp));
    }

    if obj.is_empty() { None } else { Some(serde_json::Value::Object(obj)) }
}

// --- API-Types (Teil-Deserialisierung, nur was wir brauchen) ---

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    architecture: Option<ApiArchitecture>,
    #[serde(default)]
    pricing: Option<ApiPricing>,
    #[serde(default)]
    supported_parameters: Vec<String>,
    #[serde(default)]
    top_provider: Option<ApiTopProvider>,
}

#[derive(Debug, Deserialize, Default)]
struct ApiArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
    #[serde(default)]
    output_modalities: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ApiPricing {
    #[serde(default)]
    prompt: Option<serde_json::Value>,
    #[serde(default)]
    completion: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct ApiTopProvider {
    #[serde(default)]
    is_moderated: Option<bool>,
    #[serde(default)]
    max_completion_tokens: Option<u32>,
    #[serde(default)]
    context_length: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct PublicModelEntry {
    pub id: String,
    pub context_length: u32,
    pub price_in_per_mtok: f64,
    pub price_out_per_mtok: f64,
}

/// Wandelt das Pricing-Feld (USD pro Token, als String oder Zahl) in
/// USD pro 1 Million Tokens um.
fn pricing_to_per_mtok(v: &Option<serde_json::Value>) -> f64 {
    let Some(val) = v else { return 0.0 };
    let per_token = match val {
        serde_json::Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        _ => 0.0,
    };
    per_token * 1_000_000.0
}

fn provider_slug(id: &str) -> String {
    id.split('/').next().unwrap_or(id).to_string()
}

fn from_api_model(m: ApiModel) -> ModelCandidate {
    let arch = m.architecture.unwrap_or_default();
    let pricing = m.pricing.unwrap_or_default();
    let top = m.top_provider.unwrap_or_default();
    let ctx_len = m
        .context_length
        .or(top.context_length)
        .unwrap_or(0);
    let input_modalities = if arch.input_modalities.is_empty() {
        ModalitySet::text_only()
    } else {
        ModalitySet::from_strings(&arch.input_modalities)
    };
    let supports = CapsSet::from_supported_parameters(&m.supported_parameters);
    let slug = provider_slug(&m.id);
    ModelCandidate {
        backend: Backend::OpenRouter,
        provider_slug: slug,
        price_in_per_mtok: pricing_to_per_mtok(&pricing.prompt),
        price_out_per_mtok: pricing_to_per_mtok(&pricing.completion),
        context_length: ctx_len,
        max_completion_tokens: top.max_completion_tokens,
        input_modalities,
        supports,
        is_moderated: top.is_moderated.unwrap_or(false),
        // Privacy-Klasse setzt die Registry, nicht der Client (dafür braucht's
        // den Privacy-Overlay aus der Config).
        privacy_class: PrivacyClass::Standard,
        measured_p95_ms: None,
        intelligence_index: None,
        id: m.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use router_config::Weights;

    fn profile_with(flags: impl FnOnce(&mut ResolvedProfile)) -> ResolvedProfile {
        let mut p = ResolvedProfile {
            name: "t".into(),
            weights: Weights::default(),
            max_price_out_per_mtok: None,
            max_price_in_per_mtok: None,
            max_latency_p95_ms: None,
            require_privacy_class: Default::default(),
            backend_allowlist: Default::default(),
            preferences: vec![],
            provider_sort: None,
            provider_zdr: None,
            provider_allow_fallbacks: None,
            provider_require_parameters: None,
            provider_data_collection: None,
            provider_quantizations: vec![],
            provider_only: vec![],
            provider_ignore: vec![],
            model_allowlist: vec![],
            model_denylist: vec![],
            min_intelligence_index: None,
        };
        flags(&mut p);
        p
    }

    #[test]
    fn provider_block_respects_profile() {
        let p = profile_with(|p| {
            p.provider_require_parameters = Some(true);
            p.provider_zdr = Some(true);
            p.provider_allow_fallbacks = Some(false);
            p.provider_sort = Some("price".into());
            p.provider_quantizations = vec!["fp16".into(), "bf16".into()];
            p.max_price_out_per_mtok = Some(2.0);
        });
        let block = build_provider_block(&p).unwrap();
        assert_eq!(block["require_parameters"], serde_json::json!(true));
        assert_eq!(block["zdr"], serde_json::json!(true));
        assert_eq!(block["allow_fallbacks"], serde_json::json!(false));
        assert_eq!(block["sort"], serde_json::json!("price"));
        assert_eq!(
            block["quantizations"],
            serde_json::json!(["fp16", "bf16"])
        );
        assert_eq!(block["max_price"]["completion"], serde_json::json!(2.0));
    }

    #[test]
    fn pricing_parses_string_and_number() {
        assert!((pricing_to_per_mtok(&Some(serde_json::json!("0.000003"))) - 3.0).abs() < 1e-9);
        assert!((pricing_to_per_mtok(&Some(serde_json::json!(0.000002))) - 2.0).abs() < 1e-9);
    }
}
