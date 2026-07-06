//! Typisierter Loader für `config/router.toml`.
//!
//! Der Loader ist bewusst nur passiv: er liest TOML und stellt typisierte
//! Strukturen bereit. Auflösungslogik (Profil mergen, Env-Variablen ziehen)
//! passiert im Konsumenten (`router-core` / `router-api`).

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file not readable: {0}")]
    Io(#[from] std::io::Error),
    #[error("config file malformed: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub server: ServerConfig,
    pub backends: BackendsConfig,
    #[serde(default)]
    pub registry: RegistryConfig,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub bind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackendsConfig {
    #[serde(default)]
    pub openrouter: Option<OpenRouterBackendConfig>,
    #[serde(default)]
    pub omlx: Option<OMlxBackendConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenRouterBackendConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub api_key_env: String,
    pub base_url: String,
    #[serde(default)]
    pub app_referer: Option<String>,
    #[serde(default)]
    pub app_title: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OMlxBackendConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub base_url_env: Option<String>,
    pub base_url_default: String,
    /// Optionaler API-Key, falls oMLX hinter einem Reverse-Proxy mit Auth läuft.
    #[serde(default)]
    pub api_key_env: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RegistryConfig {
    #[serde(default)]
    pub overrides: Vec<RegistryOverride>,
    #[serde(default)]
    pub privacy: PrivacyMap,
    #[serde(default)]
    pub intelligence: IntelligenceConfig,
}

/// Konfiguration für die Artificial-Analysis-Anbindung.
/// Wenn `enabled = false` (Default) oder kein API-Key gesetzt, wird der
/// Score-Term `quality` für alle Modelle 0.0 — bestehender Betrieb bleibt
/// unverändert.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct IntelligenceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_aa_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_aa_base_url")]
    pub base_url: String,
    /// Cache-TTL in Sekunden. Default: 24 h.
    #[serde(default = "default_aa_ttl")]
    pub ttl_seconds: u64,
    /// Optionales explizites Mapping: Router-Modell-ID -> Artificial-Analysis-Slug.
    /// Hat Vorrang vor Heuristiken (Suffix-Match nach `/`, Punkt-zu-Bindestrich).
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

fn default_aa_api_key_env() -> String { "AA_API_KEY".into() }
fn default_aa_base_url() -> String { "https://artificialanalysis.ai/api/v2".into() }
fn default_aa_ttl() -> u64 { 86_400 }

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PrivacyMap {
    #[serde(default)]
    pub local: Vec<String>,
    #[serde(default)]
    pub zdr: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegistryOverride {
    pub backend: String,
    pub id_prefix: String,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub caps: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Profile {
    #[serde(default)]
    pub weights: Weights,
    #[serde(default)]
    pub max_price_out_per_mtok: Option<f64>,
    #[serde(default)]
    pub max_price_in_per_mtok: Option<f64>,
    #[serde(default)]
    pub max_latency_p95_ms: Option<u32>,
    #[serde(default)]
    pub require_privacy_class: Vec<String>,
    #[serde(default)]
    pub backend_allowlist: Vec<String>,
    #[serde(default)]
    pub preferences: Vec<String>,

    // Lokale Modell-Filterung (Glob-Muster gegen model.id):
    #[serde(default)]
    pub model_allowlist: Vec<String>,
    #[serde(default)]
    pub model_denylist: Vec<String>,

    // Durchgereichte OpenRouter-Provider-Parameter:
    #[serde(default)]
    pub provider_sort: Option<String>,
    #[serde(default)]
    pub provider_zdr: Option<bool>,
    #[serde(default)]
    pub provider_allow_fallbacks: Option<bool>,
    #[serde(default)]
    pub provider_require_parameters: Option<bool>,
    #[serde(default)]
    pub provider_data_collection: Option<String>,
    #[serde(default)]
    pub provider_quantizations: Vec<String>,
    #[serde(default)]
    pub provider_only: Vec<String>,
    #[serde(default)]
    pub provider_ignore: Vec<String>,

    /// Hard-Filter: Modell muss einen Artificial-Analysis-Intelligence-Index
    /// >= diesem Wert haben. Modelle ohne Bewertung werden ebenfalls gefiltert.
    /// > `None` = kein Filter.
    #[serde(default)]
    pub min_intelligence_index: Option<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct Weights {
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub latency: f64,
    #[serde(default)]
    pub context: f64,
    #[serde(default)]
    pub preference: f64,
    /// Gewicht für den Artificial-Analysis-Intelligence-Index.
    /// Modelle ohne Bewertung scoren 0 in diesem Term.
    #[serde(default)]
    pub quality: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self { cost: 0.25, latency: 0.25, context: 0.25, preference: 0.25, quality: 0.0 }
    }
}

impl Weights {
    pub fn sum(&self) -> f64 {
        self.cost + self.latency + self.context + self.preference + self.quality
    }

    /// Gibt die Gewichte so zurück, dass sie auf 1.0 summieren. Wenn alle Null sind,
    /// fällt auf gleichverteilt zurück.
    pub fn normalized(&self) -> Self {
        let s = self.sum();
        if s <= f64::EPSILON {
            Self::default()
        } else {
            Self {
                cost: self.cost / s,
                latency: self.latency / s,
                context: self.context / s,
                preference: self.preference / s,
                quality: self.quality / s,
            }
        }
    }
}

fn default_true() -> bool {
    true
}

impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(toml::from_str(s)?)
    }
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&content)?)
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn default_profile(&self) -> Profile {
        self.profiles
            .get("default")
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config() {
        let cfg = Config::from_str(
            r#"
            [server]
            bind = "127.0.0.1:4000"

            [backends.openrouter]
            api_key_env = "OPENROUTER_API_KEY"
            base_url = "https://openrouter.ai/api/v1"

            [backends.omlx]
            base_url_default = "http://127.0.0.1:8000"

            [profiles.default]
            weights = { cost = 0.5, latency = 0.25, context = 0.1, preference = 0.15 }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.bind, "127.0.0.1:4000");
        assert!(cfg.backends.openrouter.is_some());
        assert!(cfg.default_profile().weights.sum() > 0.9);
    }

    #[test]
    fn weights_normalize() {
        let w = Weights { cost: 2.0, latency: 2.0, context: 2.0, preference: 2.0, quality: 2.0 };
        let n = w.normalized();
        assert!((n.sum() - 1.0).abs() < 1e-9);
    }
}
