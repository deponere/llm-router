//! Kleine Helfer, die den Entscheidungs-Flow zwischen NormRequest und
//! konkreten Backends kapseln.

use router_core::{
    decide, profile::ResolvedProfile, registry::Registry, Decision, ModelCandidate, NormRequest,
};
use serde_json::Value;

use crate::error::ApiError;
use crate::sse::ByteStream;
use crate::state::AppState;

pub fn decide_for(
    req: &NormRequest,
    cfg: &router_config::Config,
    registry: &Registry,
) -> Result<(ResolvedProfile, Decision), ApiError> {
    let profile = ResolvedProfile::resolve(cfg, req.profile_hint.as_deref());
    let decision = decide(req, &profile, registry)
        .map_err(|e| ApiError::NoCandidate(e.to_string()))?;
    Ok((profile, decision))
}

/// Resolves the synthetic auto-routing models that `/v1/models` advertises:
/// `auto` routes with the header/body profile, and `<profile>/auto` routes with
/// that profile. This lets GUI clients (Msty etc.) pick auto-routing — and a
/// profile — from the model dropdown instead of setting a header. Rewrites
/// `model_hint` back to plain `"auto"` so the hard filter doesn't try to pin it.
pub fn resolve_auto_alias(norm: &mut NormRequest, cfg: &router_config::Config) {
    let Some(hint) = norm.model_hint.as_deref() else { return };
    if let Some(prof) = hint.strip_suffix("/auto") {
        if cfg.profiles.contains_key(prof) {
            // Model choice is the deliberate GUI selection — it wins over a header.
            norm.profile_hint = Some(prof.to_string());
            norm.model_hint = Some("auto".into());
        }
    }
}

/// Extrahiert `x-route-profile` / `x-route-privacy` Header.
pub fn headers_to_hints(headers: &axum::http::HeaderMap) -> (Option<String>, Option<String>) {
    let prof = headers
        .get("x-route-profile")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let priv_ = headers
        .get("x-route-privacy")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    (prof, priv_)
}

pub fn parse_privacy_tag(s: Option<&str>) -> router_core::PrivacyTag {
    match s.unwrap_or("").to_ascii_lowercase().as_str() {
        "zdr" => router_core::PrivacyTag::Zdr,
        "local" | "local_only" => router_core::PrivacyTag::LocalOnly,
        _ => router_core::PrivacyTag::Normal,
    }
}

/// Druckt eine einzeilige, farbige Routing-Entscheidung direkt auf stdout.
/// Unabhängig vom `RUST_LOG`-Level — damit der Betreiber jede Anfrage
/// sofort mitlesen kann.
pub fn announce_decision(api: &str, profile: &ResolvedProfile, decision: &Decision, req: &NormRequest) {
    const RESET: &str = "\x1b[0m";
    const DIM:   &str = "\x1b[2m";
    const BOLD:  &str = "\x1b[1m";
    const CYAN:  &str = "\x1b[36m";
    const YELLOW:&str = "\x1b[33m";
    const GREEN: &str = "\x1b[32m";
    const MAGENTA:&str = "\x1b[35m";

    let backend = decision.winner.backend_id.clone();
    let cost_usd = decision
        .trace
        .ranked
        .first()
        .map(|r| r.expected_cost_usd)
        .unwrap_or(0.0);
    let p95 = decision
        .trace
        .ranked
        .first()
        .map(|r| r.used_p95_ms)
        .unwrap_or(0);
    let now = chrono_like_hhmmss();

    println!(
        "{DIM}{now}{RESET} {CYAN}→{RESET} {DIM}[{api}]{RESET} \
         {YELLOW}{profile}{RESET} → {GREEN}{BOLD}{model}{RESET} \
         {DIM}({backend} · {tokens} tok · ~{cost:.4} $ · p95 {p95}ms · tag {tag:?}){RESET}{MAGENTA}{RESET}",
        profile = profile.name,
        model   = decision.winner.id,
        tokens  = req.prompt_tokens_est,
        cost    = cost_usd,
        tag     = req.privacy_tag,
    );
}

/// Druckt eine Fallback-Zeile, wenn ein Modell beim Setup mit Upstream-Fehler
/// scheitert und der Router auf das nächstbeste Modell ausweicht.
pub fn announce_fallback(api: &str, failed: &str, next: &str, error: &str) {
    const RESET:  &str = "\x1b[0m";
    const DIM:    &str = "\x1b[2m";
    const RED:    &str = "\x1b[31m";
    const YELLOW: &str = "\x1b[33m";
    const GREEN:  &str = "\x1b[32m";
    let now = chrono_like_hhmmss();
    let trimmed: String = error.chars().take(140).collect();
    println!(
        "{DIM}{now}{RESET} {YELLOW}↻{RESET} {DIM}[{api}]{RESET} {RED}{failed}{RESET} → {GREEN}{next}{RESET} {DIM}({trimmed}){RESET}",
    );
}

/// Maximale Anzahl von Modellen, die der Router pro Request durchprobiert,
/// bevor er den letzten Upstream-Fehler an den Caller zurückreicht.
pub const FALLBACK_MAX_ATTEMPTS: usize = 3;

/// Öffnet den Byte-Stream eines einzelnen Kandidaten beim passenden Backend.
async fn open_stream(
    state: &AppState,
    profile: &ResolvedProfile,
    cand: &ModelCandidate,
    body: Value,
) -> Result<ByteStream, ApiError> {
    let provider = state.registry.provider(&cand.backend_id).ok_or_else(|| {
        ApiError::Internal(format!("backend not configured: {}", cand.backend_id))
    })?;
    provider
        .chat_completion_stream(&cand.id, profile, body)
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))
}

/// Probiert Winner + Alternativen der Reihe nach durch, bis ein Backend-Stream
/// aufgeht. `make_body` baut den Upstream-Body pro Kandidat (OpenAI reicht den
/// Original-Body durch, Anthropic übersetzt erst nach OpenAI-Schema).
pub(crate) async fn stream_with_fallback(
    state: &AppState,
    profile: &ResolvedProfile,
    decision: Decision,
    api: &str,
    make_body: impl Fn(&ModelCandidate) -> Value,
) -> Result<(ModelCandidate, ByteStream), ApiError> {
    let mut candidates: Vec<ModelCandidate> = std::iter::once(decision.winner)
        .chain(decision.alternatives)
        .take(FALLBACK_MAX_ATTEMPTS)
        .collect();
    let mut last_err: Option<ApiError> = None;
    while !candidates.is_empty() {
        let cand = candidates.remove(0);
        match open_stream(state, profile, &cand, make_body(&cand)).await {
            Ok(s) => return Ok((cand, s)),
            Err(e) => {
                let next = candidates
                    .first()
                    .map(|c| c.id.clone())
                    .unwrap_or_else(|| "<none>".into());
                announce_fallback(api, &cand.id, &next, &e.to_string());
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| ApiError::Internal("no candidates available".into())))
}

/// Druckt eine Abschluss-Zeile mit Gesamtdauer (und ggf. Ist-Kosten) auf stdout.
pub fn announce_completion(
    api: &str,
    model_id: &str,
    elapsed: std::time::Duration,
    actual_cost_usd: Option<f64>,
) {
    const RESET:  &str = "\x1b[0m";
    const DIM:    &str = "\x1b[2m";
    const GREEN:  &str = "\x1b[32m";
    const YELLOW: &str = "\x1b[33m";
    const RED:    &str = "\x1b[31m";
    const CYAN:   &str = "\x1b[36m";

    let ms = elapsed.as_millis();
    let color = if ms < 1500 { GREEN } else if ms < 5000 { YELLOW } else { RED };
    let duration = if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.2}s", elapsed.as_secs_f64())
    };
    let now = chrono_like_hhmmss();
    let cost_part = match actual_cost_usd {
        Some(c) => format!(" {CYAN}· {c:.6} ${RESET}"),
        None => String::new(),
    };
    println!(
        "{DIM}{now}{RESET} {DIM}←{RESET} {DIM}[{api}]{RESET} {model}  {color}{duration}{RESET}{cost_part}",
        model = model_id,
    );
}

fn chrono_like_hhmmss() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> router_config::Config {
        r#"
        [server]
        bind = "127.0.0.1:4000"
        [backends.omlx]
        kind = "openai_compat"
        base_url = "http://127.0.0.1:8000/v1"
        [profiles.local]
        [profiles.cheap]
        "#
        .parse()
        .unwrap()
    }

    fn norm(model: &str) -> NormRequest {
        NormRequest { model_hint: Some(model.into()), ..Default::default() }
    }

    #[test]
    fn profile_suffix_selects_profile_and_routes() {
        let mut n = norm("local/auto");
        resolve_auto_alias(&mut n, &cfg());
        assert_eq!(n.model_hint.as_deref(), Some("auto"));
        assert_eq!(n.profile_hint.as_deref(), Some("local"));
    }

    #[test]
    fn plain_auto_is_left_alone() {
        let mut n = norm("auto");
        resolve_auto_alias(&mut n, &cfg());
        assert_eq!(n.model_hint.as_deref(), Some("auto"));
        assert_eq!(n.profile_hint, None);
    }

    #[test]
    fn unknown_profile_suffix_is_not_treated_as_auto() {
        // A real model that happens to end in /auto must still be pinned.
        let mut n = norm("vendor/auto");
        resolve_auto_alias(&mut n, &cfg());
        assert_eq!(n.model_hint.as_deref(), Some("vendor/auto"));
        assert_eq!(n.profile_hint, None);
    }
}
