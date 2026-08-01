//! Typisierter, bewusst passiver Loader für `config/router.toml`: liest TOML und stellt Strukturen bereit, Auflösungslogik (Profil-Merge, Env-Variablen) passiert im Konsumenten (`router-core` / `router-api`).

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
    /// Optionaler Schutz der LLM-Endpoints (API-Keys + Budgets).
    #[serde(default)]
    pub auth: ServerAuthConfig,
    /// Persistente Nutzungshistorie (SQLite).
    #[serde(default)]
    pub storage: StorageConfig,
    /// Webhook-/Telegram-Benachrichtigungen.
    #[serde(default)]
    pub alerts: AlertsConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    pub bind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerAuthConfig {
    /// `true` = LLM-Endpoints verlangen `x-api-key` (bzw. Bearer) eines konfigurierten Keys.
    #[serde(default)]
    pub enabled: bool,
    /// Web-UI (localhost) bleibt als Admin-Surface ohne Key erreichbar.
    #[serde(default = "default_true")]
    pub allow_ui: bool,
    /// Konfigurierte Schlüssel — nur SHA-256-Hashes, nie Plaintext.
    #[serde(default)]
    pub keys: Vec<AuthKey>,
}

impl Default for ServerAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_ui: true,
            keys: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AuthKey {
    pub name: String,
    /// `sha256:<hex>` des Plaintext-Keys (`rk_…`).
    pub hash: String,
    /// USD-Budget pro UTC-Tag (optional).
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
    /// USD-Budget pro UTC-Monat (optional).
    #[serde(default)]
    pub monthly_budget_usd: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StorageConfig {
    /// Pfad zur SQLite-Datenbank; relativ wird gegen das Config-Verzeichnis aufgelöst.
    #[serde(default = "default_db_path")]
    pub db_path: String,
    /// Aufbewahrungsdauer der Transactions in Tagen (alter als das wird beim Start gelöscht; 0 = nie).
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            retention_days: default_retention_days(),
        }
    }
}

fn default_db_path() -> String { "data/router.sqlite".into() }
fn default_retention_days() -> u32 { 90 }

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AlertsConfig {
    /// Optionaler generischer Webhook (POST JSON `{event, message, ts, level}`).
    #[serde(default)]
    pub webhook_url: String,
    /// Env-Var, die das Telegram-Bot-Token hält; leer = Telegram aus.
    #[serde(default = "default_telegram_token_env")]
    pub telegram_token_env: String,
    /// Numerische Chat-ID für sendMessage.
    #[serde(default)]
    pub telegram_chat_id: String,
    /// USD-Schwelle: wenn die UTC-Tagessumme sie überschreitet, einmal täglich alerten. 0 = aus.
    #[serde(default)]
    pub daily_cost_threshold_usd: f64,
    #[serde(default)]
    pub events: AlertEvents,
}

fn default_telegram_token_env() -> String { "TELEGRAM_BOT_TOKEN".into() }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AlertEvents {
    #[serde(default = "default_true")]
    pub rotation_failed: bool,
    #[serde(default)]
    pub rotation_succeeded: bool,
    #[serde(default = "default_true")]
    pub backend_down: bool,
    #[serde(default = "default_true")]
    pub balance_low: bool,
    #[serde(default = "default_true")]
    pub daily_cost_threshold: bool,
}

impl Default for AlertEvents {
    fn default() -> Self {
        Self {
            rotation_failed: true,
            rotation_succeeded: false,
            backend_down: true,
            balance_low: true,
            daily_cost_threshold: true,
        }
    }
}

/// Alle konfigurierten Backend-Instanzen, Key = Backend-ID (z. B. "openai", "groq", "anthropic"); dient als `backend_id` für Dispatch und Metrics.
pub type BackendsConfig = BTreeMap<String, BackendConfig>;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackendConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Bestimmt Protokoll + Header-Schema (siehe [`BackendKind`]).
    pub kind: BackendKind,
    /// Voller URL-Prefix inkl. Versionssegment, an den der Client `/models` bzw. `/chat/completions` anhängt (z. B. OpenAI, OpenRouter, Ollama, Gemini).
    pub base_url: String,
    #[serde(default)]
    pub auth: AuthConfig,
    /// Lokales Backend (Ollama/LM Studio/oMLX): gewinnt bei Score-Gleichstand und bekommt standardmäßig die Privacy-Klasse `Local`.
    #[serde(default)]
    pub local: bool,
    /// Nur `kind = "openrouter"`: optionale Attributions-Header.
    #[serde(default)]
    pub app_referer: Option<String>,
    #[serde(default)]
    pub app_title: Option<String>,
    /// Nur `kind = "anthropic"`: `anthropic-version`-Header (Default 2023-06-01).
    #[serde(default)]
    pub anthropic_version: Option<String>,
    /// Optionaler Balance-Watchdog (z. B. DeepSeek: GET /user/balance). 
    #[serde(default)]
    pub watchdog: Option<WatchdogConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchdogConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Balance-Schwelle; Währung siehe `balance_currency`.
    #[serde(default = "default_min_balance")]
    pub min_balance: f64,
    /// Währung, in der die API die Balance liefert („USD" = Dollar, sonst z. B. „CNY").
    #[serde(default = "default_balance_currency")]
    pub balance_currency: String,
    /// Prüfintervall in Sekunden (zählt nur bei fälligen Router-Checks, d. h. bei Requests).
    #[serde(default = "default_watchdog_interval")]
    pub check_interval_secs: u64,
}

fn default_min_balance() -> f64 { 10.0 }
fn default_balance_currency() -> String { "USD".into() }
fn default_watchdog_interval() -> u64 { 3600 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Generischer OpenAI-kompatibler Server (OpenAI, Groq, DeepSeek, xAI, Mistral, Ollama, oMLX …).
    OpenaiCompat,
    /// OpenRouter-Aggregator: reiches `/models`-Schema + `provider`-Block.
    Openrouter,
    /// Anthropic nativ (`/v1/messages`): Egress-Client übersetzt bidirektional OpenAI ↔ Anthropic.
    Anthropic,
}

/// Auth-Verfahren pro Backend; OAuth fehlt bewusst noch, die Enum ist per `#[serde(tag = "type")]` trivial um eine `oauth`-Variante erweiterbar, sobald ein Provider sie braucht.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    /// Keine Auth (lokale Server ohne Key).
    #[default]
    None,
    /// Secret aus einer Umgebungsvariable; Header-Schema bestimmt der `BackendKind` (Bearer bei OpenAI-compat/OpenRouter, `x-api-key` bei Anthropic).
    ApiKey { env: String },
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

/// Konfiguration für die Artificial-Analysis-Anbindung: ohne `enabled = true` oder API-Key bleibt der Score-Term `quality` 0.0, bestehender Betrieb unverändert.
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
    /// Optionales explizites Mapping Router-Modell-ID -> Artificial-Analysis-Slug, hat Vorrang vor Heuristiken (Suffix-Match, Punkt-zu-Bindestrich).
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

    /// Hard-Filter: Modell muss Artificial-Analysis-Intelligence-Index >= diesem Wert haben (unbewertete werden gefiltert); `None` = kein Filter.
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
    /// Gewicht für den Artificial-Analysis-Intelligence-Index; unbewertete Modelle scoren 0 in diesem Term.
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

    /// Gibt die Gewichte normalisiert auf Summe 1.0 zurück, fällt bei lauter Null auf gleichverteilt zurück.
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
            bind = "127.0.0.1:4123"

            [backends.openrouter]
            kind = "openrouter"
            base_url = "https://openrouter.ai/api/v1"
            auth = { type = "api_key", env = "OPENROUTER_API_KEY" }

            [backends.omlx]
            kind = "openai_compat"
            base_url = "http://127.0.0.1:8000/v1"
            local = true

            [profiles.default]
            weights = { cost = 0.5, latency = 0.25, context = 0.1, preference = 0.15 }
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.bind, "127.0.0.1:4123");
        assert!(cfg.backends.contains_key("openrouter"));
        assert_eq!(cfg.backends["openrouter"].kind, BackendKind::Openrouter);
        assert!(cfg.backends["omlx"].local);
        assert!(cfg.default_profile().weights.sum() > 0.9);
    }

    #[test]
    fn weights_normalize() {
        let w = Weights { cost: 2.0, latency: 2.0, context: 2.0, preference: 2.0, quality: 2.0 };
        let n = w.normalized();
        assert!((n.sum() - 1.0).abs() < 1e-9);
    }
}
