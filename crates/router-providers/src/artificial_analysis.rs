//! Artificial-Analysis-Anbindung: holt den Intelligence-Index pro Modell vom öffentlichen API-Endpoint und cacht ihn als `HashMap<String, AaScores>` zur Anreicherung von [`ModelCandidate`]; rein opt-in (`registry.intelligence.enabled`), fehlt Key oder Aufruf, bleibt die Map leer und das Routing läuft wie vorher.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use reqwest::Client;
use router_config::IntelligenceConfig;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum AaError {
    #[error("artificial-analysis api key missing (env {0})")]
    MissingApiKey(String),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("artificial-analysis upstream {status}: {body}")]
    Upstream { status: u16, body: String },
}

/// Wichtige Metriken pro Modell. Felder sind alle optional, weil die API für neue/seltene Modelle nicht alle Indizes liefert.
#[derive(Debug, Clone, Default)]
pub struct AaScores {
    pub intelligence_index: Option<f64>,
    pub coding_index: Option<f64>,
    pub math_index: Option<f64>,
    pub median_output_tokens_per_second: Option<f64>,
    pub median_time_to_first_token_seconds: Option<f64>,
}

/// Lookup-Tabelle: Modell-Slug (lowercase) -> Scores. Slug ist der `id`-Wert aus der AA-API; das Mapping zur Router-Modell-ID übernimmt `match_router_id`.
pub type AaIndex = Arc<HashMap<String, AaScores>>;

#[derive(Clone)]
pub struct ArtificialAnalysisClient {
    http: Client,
    cfg: IntelligenceConfig,
    api_key: Option<String>,
    cache: Cache<&'static str, AaIndex>,
    aliases: BTreeMap<String, String>,
}

const CACHE_KEY: &str = "aa_index";

impl ArtificialAnalysisClient {
    pub fn new(cfg: &IntelligenceConfig) -> Self {
        let api_key = std::env::var(&cfg.api_key_env).ok();
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        let cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(Duration::from_secs(cfg.ttl_seconds.max(60)))
            .build();
        Self {
            http,
            cfg: cfg.clone(),
            api_key,
            cache,
            aliases: cfg.aliases.clone(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled && self.api_key.is_some()
    }

    /// Liefert den AA-Slug für eine Router-Modell-ID: erst `registry.intelligence.aliases`, sonst Suffix nach dem letzten `/` (OpenRouter-Format) mit abgestripptem Tag-Suffix, lowercase, Punkt -> Bindestrich.
    pub fn aa_slug_for(&self, router_id: &str) -> String {
        if let Some(alias) = self.aliases.get(router_id) {
            return alias.to_lowercase();
        }
        let tail = router_id.rsplit('/').next().unwrap_or(router_id);
        let tail = tail.split(':').next().unwrap_or(tail);
        tail.to_lowercase().replace('.', "-")
    }

    /// Liefert den Score für eine Router-Modell-ID, oder `None`.
    pub fn lookup<'a>(&self, index: &'a AaIndex, router_id: &str) -> Option<&'a AaScores> {
        let slug = self.aa_slug_for(router_id);
        index.get(&slug)
    }

    /// Liefert den gecachten Index. Bei deaktivierter Integration eine leere Map.
    pub async fn snapshot(&self) -> Result<AaIndex, AaError> {
        if !self.cfg.enabled {
            return Ok(Arc::new(HashMap::new()));
        }
        let key = self
            .api_key
            .clone()
            .ok_or_else(|| AaError::MissingApiKey(self.cfg.api_key_env.clone()))?;
        let cached = self
            .cache
            .try_get_with(CACHE_KEY, async { self.fetch(&key).await.map(Arc::new) })
            .await
            .map_err(|e: Arc<AaError>| match Arc::try_unwrap(e) {
                Ok(err) => err,
                Err(arc) => AaError::Upstream {
                    status: 0,
                    body: format!("{arc}"),
                },
            })?;
        Ok(cached)
    }

    async fn fetch(&self, api_key: &str) -> Result<HashMap<String, AaScores>, AaError> {
        let url = format!(
            "{}/data/llms/models",
            self.cfg.base_url.trim_end_matches('/')
        );
        let resp = self
            .http
            .get(&url)
            .header("x-api-key", api_key)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(AaError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: ApiResponse = serde_json::from_str(&body).map_err(|e| AaError::Upstream {
            status: status.as_u16(),
            body: format!("malformed json: {e}"),
        })?;

        let mut out = HashMap::with_capacity(parsed.data.len());
        for m in parsed.data {
            // AA's `id` ist eine UUID; der menschenlesbare Slug steht in `slug`.
            let slug = m.slug.unwrap_or(m.id).to_lowercase();
            let evals = m.evaluations.unwrap_or_default();
            let scores = AaScores {
                intelligence_index: evals.artificial_analysis_intelligence_index,
                coding_index: evals.artificial_analysis_coding_index,
                math_index: evals.artificial_analysis_math_index,
                median_output_tokens_per_second: m.median_output_tokens_per_second,
                median_time_to_first_token_seconds: m.median_time_to_first_token_seconds,
            };
            out.insert(slug, scores);
        }
        Ok(out)
    }
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(default)]
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    evaluations: Option<ApiEvaluations>,
    #[serde(default)]
    median_output_tokens_per_second: Option<f64>,
    #[serde(default)]
    median_time_to_first_token_seconds: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct ApiEvaluations {
    #[serde(default)]
    artificial_analysis_intelligence_index: Option<f64>,
    #[serde(default)]
    artificial_analysis_coding_index: Option<f64>,
    #[serde(default)]
    artificial_analysis_math_index: Option<f64>,
}
