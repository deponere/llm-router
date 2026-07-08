//! Profil-Auflösung: fertig gemergte `ResolvedProfile`-Struktur, mit der
//! Hard-Filter und Scoring arbeiten.

use std::collections::HashSet;
use std::str::FromStr;

use router_config::{Config, Profile, Weights};

use crate::registry::PrivacyClass;

#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub name: String,
    pub weights: Weights,

    pub max_price_out_per_mtok: Option<f64>,
    pub max_price_in_per_mtok: Option<f64>,
    pub max_latency_p95_ms: Option<u32>,

    pub require_privacy_class: HashSet<PrivacyClass>,
    /// Erlaubte Backend-IDs (Config-Namen). Leer = keine Einschränkung.
    pub backend_allowlist: HashSet<String>,
    pub preferences: Vec<String>,

    pub model_allowlist: Vec<String>,
    pub model_denylist: Vec<String>,

    pub provider_sort: Option<String>,
    pub provider_zdr: Option<bool>,
    pub provider_allow_fallbacks: Option<bool>,
    pub provider_require_parameters: Option<bool>,
    pub provider_data_collection: Option<String>,
    pub provider_quantizations: Vec<String>,
    pub provider_only: Vec<String>,
    pub provider_ignore: Vec<String>,

    pub min_intelligence_index: Option<f64>,
}

impl ResolvedProfile {
    pub fn from_profile(name: &str, p: &Profile) -> Self {
        let require_privacy_class: HashSet<PrivacyClass> = p
            .require_privacy_class
            .iter()
            .filter_map(|s| PrivacyClass::from_str(s).ok())
            .collect();
        // Backend-IDs case-insensitiv normalisieren, damit das Profil sowohl
        // "OpenRouter" als auch "openrouter" akzeptiert.
        let backend_allowlist: HashSet<String> = p
            .backend_allowlist
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();

        Self {
            name: name.to_string(),
            weights: p.weights.normalized(),
            max_price_out_per_mtok: p.max_price_out_per_mtok,
            max_price_in_per_mtok: p.max_price_in_per_mtok,
            max_latency_p95_ms: p.max_latency_p95_ms,
            require_privacy_class,
            backend_allowlist,
            preferences: p.preferences.clone(),
            model_allowlist: p.model_allowlist.clone(),
            model_denylist: p.model_denylist.clone(),
            provider_sort: p.provider_sort.clone(),
            provider_zdr: p.provider_zdr,
            provider_allow_fallbacks: p.provider_allow_fallbacks,
            provider_require_parameters: p.provider_require_parameters,
            provider_data_collection: p.provider_data_collection.clone(),
            provider_quantizations: p.provider_quantizations.clone(),
            provider_only: p.provider_only.clone(),
            provider_ignore: p.provider_ignore.clone(),
            min_intelligence_index: p.min_intelligence_index,
        }
    }

    /// Löst einen Profilnamen (oder `None`) gegen die Config auf. Fällt auf
    /// `default` zurück, wenn der Name unbekannt ist.
    pub fn resolve(cfg: &Config, hint: Option<&str>) -> Self {
        let name = hint.unwrap_or("default");
        let picked = cfg
            .profile(name)
            .cloned()
            .unwrap_or_else(|| cfg.default_profile());
        ResolvedProfile::from_profile(name, &picked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_profile_falls_back_to_default() {
        let cfg = Config::from_str(
            r#"
            [server]
            bind = "127.0.0.1:4000"
            [backends.openrouter]
            kind = "openrouter"
            base_url = "https://x"
            auth = { type = "api_key", env = "X" }
            [profiles.default]
            weights = { cost = 1.0, latency = 0.0, context = 0.0, preference = 0.0 }
            "#,
        )
        .unwrap();
        let r = ResolvedProfile::resolve(&cfg, Some("does-not-exist"));
        assert_eq!(r.name, "does-not-exist");
        assert!((r.weights.cost - 1.0).abs() < 1e-9);
    }
}
