//! Orchestriert Hard-Filter + Scoring und liefert eine [`Decision`] mit
//! nachvollziehbarem Trace zurück.

use serde::Serialize;

use crate::norm::NormRequest;
use crate::profile::ResolvedProfile;
use crate::registry::{ModelCandidate, Registry};
use crate::rules::{passes_all, FilterReason};
use crate::score::{rank, score_candidate, ScoreBreakdown, ScoredCandidate};

#[derive(Debug, Clone, Serialize)]
pub struct RejectedEntry {
    pub backend: String,
    pub model_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcceptedEntry {
    pub backend: String,
    pub model_id: String,
    pub score: f64,
    pub cost_score: f64,
    pub latency_score: f64,
    pub context_score: f64,
    pub preference_score: f64,
    pub quality_score: f64,
    pub expected_cost_usd: f64,
    pub used_p95_ms: u32,
    pub intelligence_index: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionTrace {
    pub profile: String,
    pub total_candidates: usize,
    pub rejected: Vec<RejectedEntry>,
    pub ranked: Vec<AcceptedEntry>,
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub winner: ModelCandidate,
    /// Alle anderen ranked Kandidaten in absteigendem Score, ohne den Winner.
    /// Wird vom Egress als Fallback-Cascade durchprobiert.
    pub alternatives: Vec<ModelCandidate>,
    pub trace: DecisionTrace,
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    #[error("no candidate survived the hard filters (profile={profile}, candidates={candidates})")]
    NoCandidate { profile: String, candidates: usize },
}

pub fn decide(
    req: &NormRequest,
    profile: &ResolvedProfile,
    registry: &Registry,
) -> Result<Decision, DecisionError> {
    let mut rejected: Vec<RejectedEntry> = Vec::new();
    let mut survivors: Vec<ScoredCandidate> = Vec::new();

    for cand in registry.iter() {
        match passes_all(req, profile, cand) {
            Ok(()) => {
                survivors.push(score_candidate(req, profile, cand));
            }
            Err(reason) => {
                rejected.push(RejectedEntry {
                    backend: format!("{:?}", cand.backend),
                    model_id: cand.id.clone(),
                    reason: format_reason(&reason),
                });
            }
        }
    }

    let total = survivors.len() + rejected.len();
    if survivors.is_empty() {
        return Err(DecisionError::NoCandidate {
            profile: profile.name.clone(),
            candidates: total,
        });
    }

    let ranked = rank(survivors);
    let accepted: Vec<AcceptedEntry> = ranked.iter().map(to_accepted).collect();
    let winner = ranked[0].model.clone();
    let alternatives: Vec<ModelCandidate> =
        ranked.iter().skip(1).map(|s| s.model.clone()).collect();
    let trace = DecisionTrace {
        profile: profile.name.clone(),
        total_candidates: total,
        rejected,
        ranked: accepted,
    };

    tracing::debug!(
        profile = %trace.profile,
        total = trace.total_candidates,
        winner = %winner.id,
        "router decision"
    );

    Ok(Decision { winner, alternatives, trace })
}

fn to_accepted(s: &ScoredCandidate) -> AcceptedEntry {
    let ScoreBreakdown {
        cost,
        latency,
        context,
        preference,
        quality,
        expected_cost_usd,
        used_p95_ms,
    } = s.breakdown;
    AcceptedEntry {
        backend: format!("{:?}", s.model.backend),
        model_id: s.model.id.clone(),
        score: s.score,
        cost_score: cost,
        latency_score: latency,
        context_score: context,
        preference_score: preference,
        quality_score: quality,
        expected_cost_usd,
        used_p95_ms,
        intelligence_index: s.model.intelligence_index,
    }
}

fn format_reason(r: &FilterReason) -> String {
    match r {
        FilterReason::ContextTooShort { needed, have } => {
            format!("context_too_short (need {needed}, have {have})")
        }
        FilterReason::MissingModality => "missing_modality".into(),
        FilterReason::MissingCaps => "missing_caps".into(),
        FilterReason::PrivacyMismatch { got } => format!("privacy_mismatch (class={got:?})"),
        FilterReason::PriceOutOverBudget { price, cap } => {
            format!("price_out_over_budget ({price:.4} > {cap:.4} $/Mtok)")
        }
        FilterReason::PriceInOverBudget { price, cap } => {
            format!("price_in_over_budget ({price:.4} > {cap:.4} $/Mtok)")
        }
        FilterReason::LatencyOverBudget { p95_ms, cap_ms } => {
            format!("latency_over_budget ({p95_ms}ms > {cap_ms}ms)")
        }
        FilterReason::BackendNotAllowed => "backend_not_allowed".into(),
        FilterReason::ModelNotAllowed => "model_not_in_allowlist".into(),
        FilterReason::ModelDenied => "model_in_denylist".into(),
        FilterReason::ModelHintMismatch => "model_hint_mismatch".into(),
        FilterReason::PrivacyTagForcesLocal => "privacy_tag_forces_local".into(),
        FilterReason::ProviderOnlyMismatch => "provider_only_mismatch".into(),
        FilterReason::ProviderIgnored => "provider_ignored".into(),
        FilterReason::IntelligenceTooLow { got, cap } => match got {
            Some(v) => format!("intelligence_too_low ({v:.1} < {cap:.1})"),
            None => format!("intelligence_unknown (cap {cap:.1})"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{Backend, CapsSet, ModalitySet, PrivacyClass};
    use router_config::Weights;

    fn cand(backend: Backend, id: &str, ctx: u32, price_out: f64) -> ModelCandidate {
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
            privacy_class: if backend == Backend::OMlx {
                PrivacyClass::Local
            } else {
                PrivacyClass::Zdr
            },
            measured_p95_ms: None,
            intelligence_index: None,
        }
    }

    fn cheap_profile() -> ResolvedProfile {
        ResolvedProfile {
            name: "cheap".into(),
            weights: Weights { cost: 1.0, latency: 0.0, context: 0.0, preference: 0.0, quality: 0.0 },
            max_price_out_per_mtok: Some(5.0),
            max_price_in_per_mtok: None,
            max_latency_p95_ms: None,
            require_privacy_class: Default::default(),
            backend_allowlist: Default::default(),
            preferences: vec![],
            provider_sort: Some("price".into()),
            provider_zdr: None,
            provider_allow_fallbacks: None,
            provider_require_parameters: Some(true),
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
    fn cheap_profile_picks_cheapest_under_cap() {
        let req = NormRequest { prompt_tokens_est: 500, max_tokens: Some(200), ..Default::default() };
        let registry = Registry {
            models: vec![
                cand(Backend::OpenRouter, "a/cheap", 200_000, 1.0),
                cand(Backend::OpenRouter, "b/midrange", 200_000, 3.0),
                cand(Backend::OpenRouter, "c/expensive", 200_000, 20.0),
            ],
        };
        let d = decide(&req, &cheap_profile(), &registry).unwrap();
        assert_eq!(d.winner.id, "a/cheap");
        // Teures Modell wurde wegen Price-Cap rausgefiltert.
        assert!(d
            .trace
            .rejected
            .iter()
            .any(|r| r.model_id == "c/expensive" && r.reason.starts_with("price_out_over_budget")));
    }

    #[test]
    fn error_when_no_candidate_survives() {
        let req = NormRequest { prompt_tokens_est: 1_000_000, max_tokens: Some(200), ..Default::default() };
        let registry = Registry {
            models: vec![cand(Backend::OpenRouter, "a/cheap", 8_000, 1.0)],
        };
        let res = decide(&req, &cheap_profile(), &registry);
        assert!(matches!(res, Err(DecisionError::NoCandidate { .. })));
    }
}
