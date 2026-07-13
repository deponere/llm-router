//! Gemergter Modell-Katalog. Fetcht von allen konfigurierten Backends, legt das Ergebnis in einen `moka`-TTL-Cache und reicht es als Snapshot (`Registry`) raus.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use router_config::{BackendConfig, BackendKind, Config, RegistryConfig};
use router_core::registry::{ModelCandidate, Registry};

use crate::artificial_analysis::ArtificialAnalysisClient;
use crate::metrics::LatencyTracker;
use crate::anthropic::AnthropicClient;
use crate::openai_compat::OpenAiCompatClient;
use crate::openrouter::OpenRouterClient;
use crate::provider::Provider;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("provider: {0}")]
    Provider(#[from] crate::provider::ProviderError),
}

#[derive(Clone)]
pub struct RegistryHandle {
    providers: BTreeMap<String, Arc<dyn Provider>>,
    registry_cfg: RegistryConfig,
    cache: Cache<&'static str, Arc<Registry>>,
    tracker: LatencyTracker,
    aa: ArtificialAnalysisClient,
}

const CACHE_KEY: &str = "registry";

impl RegistryHandle {
    pub fn new(config: &Config, tracker: LatencyTracker) -> Self {
        let providers: BTreeMap<String, Arc<dyn Provider>> = config
            .backends
            .iter()
            .filter(|(_, c)| c.enabled)
            .filter_map(|(id, c)| build_provider(id, c).map(|p| (id.clone(), p)))
            .collect();
        let cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(Duration::from_secs(5 * 60))
            .build();
        let aa = ArtificialAnalysisClient::new(&config.registry.intelligence);
        Self {
            providers,
            registry_cfg: config.registry.clone(),
            cache,
            tracker,
            aa,
        }
    }

    /// Provider-Instanz für eine Backend-ID (Dispatch im Egress).
    pub fn provider(&self, backend_id: &str) -> Option<&Arc<dyn Provider>> {
        self.providers.get(backend_id)
    }
    pub fn tracker(&self) -> &LatencyTracker { &self.tracker }
    pub fn artificial_analysis(&self) -> &ArtificialAnalysisClient { &self.aa }

    /// Liefert den aktuellen Snapshot (mit frischen Latenz-Messungen).
    pub async fn snapshot(&self) -> Result<Arc<Registry>, RegistryError> {
        let cached = self
            .cache
            .try_get_with(CACHE_KEY, async { self.build().await.map(Arc::new) })
            .await
            .map_err(|e: Arc<RegistryError>| match Arc::try_unwrap(e) {
                Ok(err) => err,
                Err(arc) => RegistryError::Provider(crate::provider::ProviderError::Upstream {
                    status: 0,
                    body: format!("{arc}"),
                }),
            })?;
        // Latenz-Stempel pro Snapshot frisch aufpressen (Cache kann älter sein).
        Ok(Arc::new(self.attach_metrics(&cached)))
    }

    async fn build(&self) -> Result<Registry, RegistryError> {
        let mut models: Vec<ModelCandidate> = Vec::new();
        // Ein fehlschlagendes Backend darf den Gesamtkatalog nicht kippen.
        for (id, provider) in &self.providers {
            match provider.list_models(&self.registry_cfg).await {
                Ok(mut list) => models.append(&mut list),
                Err(e) => {
                    tracing::warn!(backend=%id, error=%e, "catalog fetch failed, continuing without");
                }
            }
        }
        Ok(Registry { models })
    }

    fn attach_metrics(&self, reg: &Registry) -> Registry {
        let mut out = reg.clone();
        for m in &mut out.models {
            m.measured_p95_ms = self.tracker.p95_ms(&m.backend_id, &m.id);
        }
        out
    }

    /// Versorgt einen Snapshot mit Artificial-Analysis-Scores. Bei Fehler oder disabled bleibt das Feld `intelligence_index` einfach `None`.
    pub async fn enriched_snapshot(&self) -> Result<Arc<Registry>, RegistryError> {
        let snap = self.snapshot().await?;
        if !self.aa.enabled() {
            return Ok(snap);
        }
        let index = match self.aa.snapshot().await {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!(error=%e, "artificial-analysis fetch failed, continuing without quality scores");
                return Ok(snap);
            }
        };
        let mut out = (*snap).clone();
        for m in &mut out.models {
            if let Some(scores) = self.aa.lookup(&index, &m.id) {
                m.intelligence_index = scores.intelligence_index;
            }
        }
        Ok(Arc::new(out))
    }
}

/// Baut die passende Provider-Impl für eine Backend-Instanz. Gibt `None`, wenn der Kind (noch) nicht unterstützt wird.
fn build_provider(id: &str, cfg: &BackendConfig) -> Option<Arc<dyn Provider>> {
    match cfg.kind {
        BackendKind::OpenaiCompat => Some(Arc::new(OpenAiCompatClient::new(id, cfg))),
        BackendKind::Openrouter => Some(Arc::new(OpenRouterClient::new(id, cfg))),
        BackendKind::Anthropic => Some(Arc::new(AnthropicClient::new(id, cfg))),
    }
}
