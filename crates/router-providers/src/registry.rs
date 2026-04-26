//! Gemergter Modell-Katalog. Fetcht von OpenRouter + oMLX, legt das Ergebnis
//! in einen `moka`-TTL-Cache und reicht es als Snapshot (`Registry`) raus.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use router_config::{Config, RegistryConfig};
use router_core::registry::{Backend, ModelCandidate, PrivacyClass, Registry};

use crate::artificial_analysis::ArtificialAnalysisClient;
use crate::metrics::LatencyTracker;
use crate::omlx::OMlxClient;
use crate::openrouter::OpenRouterClient;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("openrouter: {0}")]
    OpenRouter(#[from] crate::openrouter::OpenRouterError),
    #[error("omlx: {0}")]
    OMlx(#[from] crate::omlx::OMlxError),
}

#[derive(Clone)]
pub struct RegistryHandle {
    openrouter: Option<OpenRouterClient>,
    omlx: Option<OMlxClient>,
    registry_cfg: RegistryConfig,
    cache: Cache<&'static str, Arc<Registry>>,
    tracker: LatencyTracker,
    aa: ArtificialAnalysisClient,
}

const CACHE_KEY: &str = "registry";

impl RegistryHandle {
    pub fn new(config: &Config, tracker: LatencyTracker) -> Self {
        let openrouter = config
            .backends
            .openrouter
            .as_ref()
            .filter(|c| c.enabled)
            .map(OpenRouterClient::new);
        let omlx = config
            .backends
            .omlx
            .as_ref()
            .filter(|c| c.enabled)
            .map(OMlxClient::new);
        let cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(Duration::from_secs(5 * 60))
            .build();
        let aa = ArtificialAnalysisClient::new(&config.registry.intelligence);
        Self {
            openrouter,
            omlx,
            registry_cfg: config.registry.clone(),
            cache,
            tracker,
            aa,
        }
    }

    pub fn openrouter(&self) -> Option<&OpenRouterClient> { self.openrouter.as_ref() }
    pub fn omlx(&self) -> Option<&OMlxClient> { self.omlx.as_ref() }
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
                Err(arc) => RegistryError::OpenRouter(crate::openrouter::OpenRouterError::Upstream {
                    status: 0,
                    body: format!("{arc}"),
                }),
            })?;
        // Latenz-Stempel pro Snapshot frisch aufpressen (Cache kann älter sein).
        Ok(Arc::new(self.attach_metrics(&cached)))
    }

    async fn build(&self) -> Result<Registry, RegistryError> {
        let mut models: Vec<ModelCandidate> = Vec::new();
        if let Some(or) = &self.openrouter {
            match or.list_models().await {
                Ok(mut list) => {
                    apply_privacy_overlay(&mut list, &self.registry_cfg);
                    models.append(&mut list);
                }
                Err(e) => {
                    tracing::warn!(error=%e, "openrouter catalog fetch failed, continuing without");
                }
            }
        }
        if let Some(ol) = &self.omlx {
            match ol.list_models(&self.registry_cfg).await {
                Ok(mut list) => models.append(&mut list),
                Err(e) => {
                    tracing::warn!(error=%e, "omlx catalog fetch failed, continuing without");
                }
            }
        }
        Ok(Registry { models })
    }

    fn attach_metrics(&self, reg: &Registry) -> Registry {
        let mut out = reg.clone();
        for m in &mut out.models {
            m.measured_p95_ms = self.tracker.p95_ms(m.backend, &m.id);
        }
        out
    }

    /// Versorgt einen Snapshot mit Artificial-Analysis-Scores. Bei Fehler oder
    /// disabled bleibt das Feld `intelligence_index` einfach `None`.
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

fn apply_privacy_overlay(models: &mut [ModelCandidate], cfg: &RegistryConfig) {
    for m in models.iter_mut() {
        if m.backend == Backend::OpenRouter {
            if cfg.privacy.local.iter().any(|s| s == &m.provider_slug) {
                m.privacy_class = PrivacyClass::Local;
            } else if cfg.privacy.zdr.iter().any(|s| s == &m.provider_slug) {
                m.privacy_class = PrivacyClass::Zdr;
            } else {
                m.privacy_class = PrivacyClass::Standard;
            }
        }
    }
}
