//! Debug-Endpunkte: Registry-Inspektion und Expertensystem-Dry-Run.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use router_core::{ModelCandidate, NormRequest};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::openai::openai_to_norm;
use crate::routing::{decide_for, headers_to_hints, parse_privacy_tag};
use crate::state::AppState;

/// `GET /v1/transactions` — aktuelle Session-Summe + letzte Aufrufe fürs Widget.
/// Optional `?limit=N` (default 10).
pub async fn transactions(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let limit = q.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
    let snap = state.history.snapshot(limit);
    Json(serde_json::to_value(snap).unwrap_or_else(|_| serde_json::json!({})))
}

/// `GET /v1/registry` — vollständige Modell-Liste mit allen Parametern.
pub async fn registry(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let snap = state
        .registry
        .snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let models: Vec<Value> = snap.models.iter().map(candidate_to_json).collect();
    Ok(Json(json!({ "total": models.len(), "models": models })))
}

/// `POST /v1/explain` — Dry-Run des Expertensystems ohne Egress.
///
/// Erwartet denselben Body wie `/v1/chat/completions`. Liefert den
/// vollständigen Entscheidungs-Trace: welche Modelle gefiltert wurden
/// (mit Grund) und wie die Überlebenden gerankt wurden.
pub async fn explain(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (profile_hdr, privacy_hdr) = headers_to_hints(&headers);
    let mut norm = openai_to_norm(&body)?;
    if norm.profile_hint.is_none() {
        norm.profile_hint = profile_hdr;
    }
    if privacy_hdr.is_some() {
        norm.privacy_tag = parse_privacy_tag(privacy_hdr.as_deref());
    }
    norm.detect_required();

    let snap = state
        .registry
        .snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let (profile, decision) = decide_for(&norm, &state.config, &snap)?;

    Ok(Json(json!({
        "winner": decision.winner.id,
        "winner_backend": format!("{:?}", decision.winner.backend),
        "profile": profile.name,
        "weights": {
            "cost":       profile.weights.cost,
            "latency":    profile.weights.latency,
            "context":    profile.weights.context,
            "preference": profile.weights.preference,
        },
        "constraints": {
            "max_price_out_per_mtok": profile.max_price_out_per_mtok,
            "max_price_in_per_mtok":  profile.max_price_in_per_mtok,
            "max_latency_p95_ms":     profile.max_latency_p95_ms,
            "require_privacy_class":  profile.require_privacy_class.iter()
                                          .map(|c| format!("{c:?}")).collect::<Vec<_>>(),
            "backend_allowlist":      profile.backend_allowlist.iter()
                                          .map(|b| format!("{b:?}")).collect::<Vec<_>>(),
        },
        "request": {
            "prompt_tokens_est": norm.prompt_tokens_est,
            "max_tokens":        norm.max_tokens,
            "required_modalities": required_modalities(&norm),
            "required_caps":       required_caps(&norm),
            "privacy_tag":         format!("{:?}", norm.privacy_tag),
        },
        "trace": decision.trace,
    })))
}

fn candidate_to_json(m: &ModelCandidate) -> Value {
    let modalities: Vec<&str> = {
        let mut v = vec![];
        if m.input_modalities.has_text()  { v.push("text"); }
        if m.input_modalities.has_image() { v.push("image"); }
        if m.input_modalities.has_audio() { v.push("audio"); }
        if m.input_modalities.has_video() { v.push("video"); }
        if m.input_modalities.has_file()  { v.push("file"); }
        v
    };
    let caps: Vec<&str> = {
        let mut v = vec![];
        if m.supports.has_tools()              { v.push("tools"); }
        if m.supports.has_json_mode()          { v.push("json_mode"); }
        if m.supports.has_structured_outputs() { v.push("structured_outputs"); }
        if m.supports.has_reasoning()          { v.push("reasoning"); }
        v
    };
    json!({
        "id":                    m.id,
        "backend":               format!("{:?}", m.backend),
        "provider":              m.provider_slug,
        "context_length":        m.context_length,
        "max_completion_tokens": m.max_completion_tokens,
        "pricing": {
            "input_per_mtok_usd":  m.price_in_per_mtok,
            "output_per_mtok_usd": m.price_out_per_mtok,
        },
        "input_modalities": modalities,
        "capabilities":     caps,
        "privacy_class":    format!("{:?}", m.privacy_class),
        "is_moderated":     m.is_moderated,
        "measured_p95_ms":  m.measured_p95_ms,
    })
}

fn required_modalities(norm: &NormRequest) -> Vec<&'static str> {
    let mut v = vec![];
    if norm.required.modalities.has_text()  { v.push("text"); }
    if norm.required.modalities.has_image() { v.push("image"); }
    if norm.required.modalities.has_audio() { v.push("audio"); }
    if norm.required.modalities.has_video() { v.push("video"); }
    if norm.required.modalities.has_file()  { v.push("file"); }
    v
}

fn required_caps(norm: &NormRequest) -> Vec<&'static str> {
    let mut v = vec![];
    if norm.required.caps.has_tools()              { v.push("tools"); }
    if norm.required.caps.has_json_mode()          { v.push("json_mode"); }
    if norm.required.caps.has_structured_outputs() { v.push("structured_outputs"); }
    if norm.required.caps.has_reasoning()          { v.push("reasoning"); }
    v
}
