//! Hard-Filter. Reihenfolge ist festgeschrieben, jeder Grund wird als
//! [`FilterReason`] zurückgegeben — der Entscheidungs-Trace listet später alle
//! verworfenen Kandidaten mit Grund.

use crate::norm::{NormRequest, PrivacyTag};
use crate::profile::ResolvedProfile;
use crate::registry::{ModelCandidate, PrivacyClass};

/// Reserve für die Completion, wenn der Client `max_tokens` nicht setzt.
pub const DEFAULT_COMPLETION_RESERVE: u32 = 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum FilterReason {
    ContextTooShort { needed: u32, have: u32 },
    MissingModality,
    MissingCaps,
    PrivacyMismatch { got: PrivacyClass },
    PriceOutOverBudget { price: f64, cap: f64 },
    PriceInOverBudget { price: f64, cap: f64 },
    LatencyOverBudget { p95_ms: u32, cap_ms: u32 },
    BackendNotAllowed,
    ModelNotAllowed,
    ModelDenied,
    ModelHintMismatch,
    PrivacyTagForcesLocal,
    ProviderOnlyMismatch,
    ProviderIgnored,
    IntelligenceTooLow { got: Option<f64>, cap: f64 },
}

/// `true`, wenn der Kandidat alle Hard-Filter überlebt.
pub fn passes_all(
    req: &NormRequest,
    profile: &ResolvedProfile,
    cand: &ModelCandidate,
) -> Result<(), FilterReason> {
    // 1. Model-Hint: Wenn der Client explizit ein Modell nennt (und nicht "auto"),
    //    zwingen wir exakt darauf. So bleibt manuelles Override per Body möglich.
    if let Some(hint) = req.model_hint.as_deref() {
        if !hint.is_empty() && hint != "auto" && hint != cand.id {
            return Err(FilterReason::ModelHintMismatch);
        }
    }

    // 2. Privacy-Tag aus dem Request überschreibt Profil-Einstellungen nach unten:
    //    `LocalOnly` -> nur Backend::OMlx; `Zdr` -> Local oder Zdr.
    match req.privacy_tag {
        PrivacyTag::LocalOnly => {
            if cand.privacy_class != PrivacyClass::Local {
                return Err(FilterReason::PrivacyTagForcesLocal);
            }
        }
        PrivacyTag::Zdr => {
            if !matches!(cand.privacy_class, PrivacyClass::Local | PrivacyClass::Zdr) {
                return Err(FilterReason::PrivacyMismatch { got: cand.privacy_class });
            }
        }
        PrivacyTag::Normal => {}
    }

    // 3. Backend-Allowlist des Profils.
    if !profile.backend_allowlist.is_empty()
        && !profile.backend_allowlist.contains(&cand.backend)
    {
        return Err(FilterReason::BackendNotAllowed);
    }

    // 4. Modell-Allowlist / -Denylist (Glob-Muster gegen model.id).
    if !profile.model_allowlist.is_empty()
        && !profile.model_allowlist.iter().any(|p| glob_match(p, &cand.id))
    {
        return Err(FilterReason::ModelNotAllowed);
    }
    if profile.model_denylist.iter().any(|p| glob_match(p, &cand.id)) {
        return Err(FilterReason::ModelDenied);
    }

    // 5. Provider-only / provider-ignore (für OpenRouter-Modelle).
    if !profile.provider_only.is_empty()
        && !profile.provider_only.iter().any(|s| s == &cand.provider_slug)
    {
        return Err(FilterReason::ProviderOnlyMismatch);
    }
    if profile.provider_ignore.iter().any(|s| s == &cand.provider_slug) {
        return Err(FilterReason::ProviderIgnored);
    }

    // 6. Context: Prompt + Completion-Reserve muss reinpassen.
    let reserve = req.max_tokens.unwrap_or(DEFAULT_COMPLETION_RESERVE);
    let needed = req.prompt_tokens_est.saturating_add(reserve);
    if needed > cand.context_length {
        return Err(FilterReason::ContextTooShort {
            needed,
            have: cand.context_length,
        });
    }

    // 7. Modalitäten.
    if !cand.input_modalities.covers(req.required.modalities) {
        return Err(FilterReason::MissingModality);
    }

    // 8. Capabilities (tools, structured_outputs, reasoning, json_mode).
    if !cand.supports.covers(req.required.caps) {
        return Err(FilterReason::MissingCaps);
    }

    // 9. Privacy-Requirement aus Profil.
    if !profile.require_privacy_class.is_empty()
        && !profile.require_privacy_class.contains(&cand.privacy_class)
    {
        return Err(FilterReason::PrivacyMismatch { got: cand.privacy_class });
    }

    // 10. Preis-Deckel.
    if let Some(cap) = profile.max_price_out_per_mtok {
        if cand.price_out_per_mtok > cap {
            return Err(FilterReason::PriceOutOverBudget {
                price: cand.price_out_per_mtok,
                cap,
            });
        }
    }
    if let Some(cap) = profile.max_price_in_per_mtok {
        if cand.price_in_per_mtok > cap {
            return Err(FilterReason::PriceInOverBudget {
                price: cand.price_in_per_mtok,
                cap,
            });
        }
    }

    // 11. Latenz-Deckel (nur wenn Messung vorhanden).
    if let (Some(p95), Some(cap)) = (cand.measured_p95_ms, profile.max_latency_p95_ms) {
        if p95 > cap {
            return Err(FilterReason::LatencyOverBudget { p95_ms: p95, cap_ms: cap });
        }
    }

    // 12. Intelligence-Filter: erforderlicher Mindest-Index aus AA. Modelle
    //     ohne Bewertung werden verworfen, wenn der Filter aktiv ist.
    if let Some(cap) = profile.min_intelligence_index {
        match cand.intelligence_index {
            Some(v) if v >= cap => {}
            other => return Err(FilterReason::IntelligenceTooLow { got: other, cap }),
        }
    }

    Ok(())
}

/// Glob-Matching: `*` matcht beliebig viele Zeichen. Kein `?`-Support nötig.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    glob_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_bytes(pat: &[u8], txt: &[u8]) -> bool {
    match (pat.first(), txt.first()) {
        (None, None) => true,
        (Some(&b'*'), _) => {
            glob_bytes(&pat[1..], txt) || (!txt.is_empty() && glob_bytes(pat, &txt[1..]))
        }
        (None, Some(_)) | (Some(_), None) => false,
        (Some(p), Some(t)) => p == t && glob_bytes(&pat[1..], &txt[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Backend, CapsSet, ModalitySet};

    fn candidate(id: &str, ctx: u32, price_out: f64) -> ModelCandidate {
        ModelCandidate {
            backend: Backend::OpenRouter,
            id: id.into(),
            provider_slug: "anthropic".into(),
            context_length: ctx,
            max_completion_tokens: Some(4096),
            price_in_per_mtok: 1.0,
            price_out_per_mtok: price_out,
            input_modalities: ModalitySet::text_only(),
            supports: CapsSet::default(),
            is_moderated: false,
            privacy_class: PrivacyClass::Zdr,
            measured_p95_ms: None,
            intelligence_index: None,
        }
    }

    fn empty_profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "t".into(),
            weights: Default::default(),
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
        }
    }

    #[test]
    fn context_filter_rejects_too_small() {
        let req = NormRequest { prompt_tokens_est: 100_000, max_tokens: Some(500), ..Default::default() };
        let cand = candidate("x", 8_000, 1.0);
        let res = passes_all(&req, &empty_profile(), &cand);
        assert!(matches!(res, Err(FilterReason::ContextTooShort { .. })));
    }

    #[test]
    fn price_cap_rejects_too_expensive() {
        let req = NormRequest { prompt_tokens_est: 100, max_tokens: Some(10), ..Default::default() };
        let cand = candidate("x", 200_000, 20.0);
        let mut p = empty_profile();
        p.max_price_out_per_mtok = Some(5.0);
        assert!(matches!(
            passes_all(&req, &p, &cand),
            Err(FilterReason::PriceOutOverBudget { .. })
        ));
    }

    #[test]
    fn model_hint_forces_exact_match() {
        let req = NormRequest { model_hint: Some("anthropic/claude-sonnet-4-6".into()), max_tokens: Some(10), ..Default::default() };
        let cand = candidate("openai/gpt-5", 200_000, 1.0);
        assert!(matches!(
            passes_all(&req, &empty_profile(), &cand),
            Err(FilterReason::ModelHintMismatch)
        ));
    }

    #[test]
    fn privacy_tag_local_only_rejects_cloud() {
        let req = NormRequest { privacy_tag: PrivacyTag::LocalOnly, max_tokens: Some(10), ..Default::default() };
        let cand = candidate("openai/gpt-5", 200_000, 1.0);
        assert!(matches!(
            passes_all(&req, &empty_profile(), &cand),
            Err(FilterReason::PrivacyTagForcesLocal)
        ));
    }

    #[test]
    fn glob_match_patterns() {
        assert!(glob_match("anthropic/*", "anthropic/claude-sonnet-4-6"));
        assert!(glob_match("qwen3*", "qwen3-32b"));
        assert!(glob_match("*gpt*", "openai/gpt-5"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("anthropic/*", "openai/gpt-5"));
        assert!(!glob_match("qwen3*", "qwen2.5-coder-32b"));
    }

    #[test]
    fn model_allowlist_filters() {
        let req = NormRequest { max_tokens: Some(10), ..Default::default() };
        let allowed = candidate("anthropic/claude-sonnet-4-6", 200_000, 1.0);
        let blocked = candidate("openai/gpt-5", 200_000, 1.0);
        let mut p = empty_profile();
        p.model_allowlist = vec!["anthropic/*".into()];
        assert!(passes_all(&req, &p, &allowed).is_ok());
        assert!(matches!(
            passes_all(&req, &p, &blocked),
            Err(FilterReason::ModelNotAllowed)
        ));
    }

    #[test]
    fn model_denylist_filters() {
        let req = NormRequest { max_tokens: Some(10), ..Default::default() };
        let cand = candidate("openai/gpt-3.5-turbo", 200_000, 1.0);
        let mut p = empty_profile();
        p.model_denylist = vec!["openai/gpt-3*".into()];
        assert!(matches!(
            passes_all(&req, &p, &cand),
            Err(FilterReason::ModelDenied)
        ));
    }
}
