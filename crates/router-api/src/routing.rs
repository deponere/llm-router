//! Kleine Helfer, die den Entscheidungs-Flow zwischen NormRequest und
//! konkreten Backends kapseln.

use router_core::{
    decide, profile::ResolvedProfile, registry::Registry, Decision, NormRequest,
};

use crate::error::ApiError;

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

    let backend = format!("{:?}", decision.winner.backend);
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
