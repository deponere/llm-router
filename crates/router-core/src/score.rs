//! Scoring-Funktion. Gibt für jeden überlebenden Kandidaten einen Score in
//! [0, 1] zurück. Tiebreak ist rein deterministisch:
//! `(score desc, backend_priority asc, model_id asc)`.

use crate::norm::NormRequest;
use crate::profile::ResolvedProfile;
use crate::registry::ModelCandidate;

/// Bei keiner Messung nehmen wir diesen Wert als neutralen Platzhalter,
/// damit das Latenz-Gewicht nicht komplett Null wird.
const LATENCY_UNKNOWN_MS: u32 = 2_500;
/// Latenz-Normalisierungs-Horizont: alles >5s scored 0.
const LATENCY_NORM_MS: f64 = 5_000.0;
/// Budget-Horizont für expected_cost: alles >50 ¢ pro Request scored 0.
const COST_NORM_USD: f64 = 0.50;
/// Erwartete Output-Tokens, wenn kein `max_tokens` gesetzt ist.
const DEFAULT_EXPECTED_OUT: u32 = 800;

#[derive(Debug, Clone)]
pub struct ScoredCandidate {
    pub model: ModelCandidate,
    pub score: f64,
    pub breakdown: ScoreBreakdown,
}

#[derive(Debug, Clone, Copy)]
pub struct ScoreBreakdown {
    pub cost: f64,
    pub latency: f64,
    pub context: f64,
    pub preference: f64,
    pub quality: f64,
    pub expected_cost_usd: f64,
    pub used_p95_ms: u32,
}

pub fn score_candidate(
    req: &NormRequest,
    profile: &ResolvedProfile,
    cand: &ModelCandidate,
) -> ScoredCandidate {
    let expected_out = req.max_tokens.unwrap_or(DEFAULT_EXPECTED_OUT) as f64;
    let prompt = req.prompt_tokens_est as f64;

    let expected_cost_usd =
        (prompt * cand.price_in_per_mtok + expected_out * cand.price_out_per_mtok) / 1_000_000.0;
    let cost_score = 1.0 - clamp01(expected_cost_usd / COST_NORM_USD);

    let p95 = cand.measured_p95_ms.unwrap_or(LATENCY_UNKNOWN_MS);
    let latency_score = 1.0 - clamp01(p95 as f64 / LATENCY_NORM_MS);

    let context_score = if prompt <= 0.0 {
        1.0
    } else {
        clamp01((cand.context_length as f64 - prompt) / prompt)
    };

    let preference_score = preference_rank(&profile.preferences, &cand.id);

    // Quality kommt aus dem Artificial-Analysis-Intelligence-Index (0..100).
    // Modelle ohne Bewertung erhalten 0 — niedriger als jedes bewertete Modell,
    // aber kein Hard-Filter (dafür ist `min_intelligence_index` da).
    let quality_score = clamp01(cand.intelligence_index.unwrap_or(0.0) / 100.0);

    let w = profile.weights;
    let score = w.cost * cost_score
        + w.latency * latency_score
        + w.context * context_score
        + w.preference * preference_score
        + w.quality * quality_score;

    ScoredCandidate {
        model: cand.clone(),
        score,
        breakdown: ScoreBreakdown {
            cost: cost_score,
            latency: latency_score,
            context: context_score,
            preference: preference_score,
            quality: quality_score,
            expected_cost_usd,
            used_p95_ms: p95,
        },
    }
}

/// Höherer Rang = besser. Modelle früher in der Liste erhalten linear höheren
/// Score; nicht gelistete Modelle erhalten 0.
fn preference_rank(prefs: &[String], id: &str) -> f64 {
    if prefs.is_empty() {
        return 0.0;
    }
    match prefs.iter().position(|p| p == id) {
        None => 0.0,
        Some(idx) => 1.0 - (idx as f64 / prefs.len() as f64),
    }
}

fn clamp01(x: f64) -> f64 {
    if x.is_nan() { 0.0 }
    else if x < 0.0 { 0.0 }
    else if x > 1.0 { 1.0 }
    else { x }
}

/// Sortiert Kandidaten absteigend nach Score, mit festgelegtem Tiebreak.
pub fn rank(mut candidates: Vec<ScoredCandidate>) -> Vec<ScoredCandidate> {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.model
                    .backend
                    .tiebreak_priority()
                    .cmp(&b.model.backend.tiebreak_priority())
            })
            .then_with(|| a.model.id.cmp(&b.model.id))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Backend, CapsSet, ModalitySet, PrivacyClass};

    fn cand(
        backend: Backend,
        id: &str,
        ctx: u32,
        price_out: f64,
        p95: Option<u32>,
    ) -> ModelCandidate {
        ModelCandidate {
            backend,
            id: id.into(),
            provider_slug: id.split('/').next().unwrap_or("local").into(),
            context_length: ctx,
            max_completion_tokens: Some(4096),
            price_in_per_mtok: 1.0,
            price_out_per_mtok: price_out,
            input_modalities: ModalitySet::text_only(),
            supports: CapsSet::default(),
            is_moderated: false,
            privacy_class: PrivacyClass::Zdr,
            measured_p95_ms: p95,
            intelligence_index: None,
        }
    }

    fn balanced_profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "t".into(),
            weights: router_config::Weights {
                cost: 0.5,
                latency: 0.5,
                context: 0.0,
                preference: 0.0,
                quality: 0.0,
            },
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
    fn cheaper_wins_with_equal_latency() {
        let mut req = NormRequest::default();
        req.prompt_tokens_est = 1000;
        req.max_tokens = Some(500);
        let a = cand(Backend::OpenRouter, "a/cheap", 200_000, 1.0, Some(1000));
        let b = cand(Backend::OpenRouter, "b/expensive", 200_000, 20.0, Some(1000));
        let scored = vec![
            score_candidate(&req, &balanced_profile(), &a),
            score_candidate(&req, &balanced_profile(), &b),
        ];
        let ranked = rank(scored);
        assert_eq!(ranked[0].model.id, "a/cheap");
    }

    #[test]
    fn lexicographic_tiebreak_is_stable() {
        let mut req = NormRequest::default();
        req.prompt_tokens_est = 100;
        req.max_tokens = Some(100);
        // Gleicher Preis, gleiche Latenz -> Score identisch.
        let a = cand(Backend::OpenRouter, "z/model", 200_000, 1.0, Some(1000));
        let b = cand(Backend::OpenRouter, "a/model", 200_000, 1.0, Some(1000));
        let scored = vec![
            score_candidate(&req, &balanced_profile(), &a),
            score_candidate(&req, &balanced_profile(), &b),
        ];
        let ranked = rank(scored);
        assert_eq!(ranked[0].model.id, "a/model");
    }

    #[test]
    fn omlx_wins_tiebreak_vs_openrouter() {
        let mut req = NormRequest::default();
        req.prompt_tokens_est = 100;
        req.max_tokens = Some(100);
        let a = cand(Backend::OpenRouter, "same/model", 200_000, 0.0, Some(1000));
        let b = cand(Backend::OMlx, "same/model", 200_000, 0.0, Some(1000));
        let scored = vec![
            score_candidate(&req, &balanced_profile(), &a),
            score_candidate(&req, &balanced_profile(), &b),
        ];
        let ranked = rank(scored);
        assert_eq!(ranked[0].model.backend, Backend::OMlx);
    }
}
