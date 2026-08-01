//! Debug-Endpunkte: Registry-Inspektion und Expertensystem-Dry-Run.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use router_core::{ModelCandidate, NormRequest};
use serde_json::{json, Value};

use crate::anthropic::anthropic_to_norm;
use crate::error::ApiError;
use crate::openai::openai_to_norm;
use crate::routing::{decide_for, headers_to_hints, parse_privacy_tag, resolve_auto_alias};
use crate::state::AppState;

/// `POST /v1/admin/restart` — startet den Router-Prozess neu: gleiches Binary,
/// gleiche Args, neue Session (überlebt Terminal-Schließen). Die Antwort wird
/// vor dem Exit zugestellt; Clients sollten anschließend `/healthz` pollen.
/// Der neue Prozess übernimmt die Config (inkl. ggf. frisch rotiertem Key aus `.env`).
pub async fn restart() -> Json<Value> {
    let Some(exe) = std::env::current_exe().ok() else {
        return Json(json!({ "status": "error", "error": "current_exe unknown" }));
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args().skip(1));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Neue Session (setsid): der Neustart überlebt ein Terminal-Schließen.
        cmd.process_group(0);
    }
    cmd.stdin(std::process::Stdio::null());
    // stdout/stderr nur erben, wenn ein echtes Terminal dahinter hängt. Bei
    // Pipe/Parent-Logging (z. B. launchd, Admin-App, diese Session) nach
    // /dev/null — sonst blockieren Log-Writes im vollen Pipe-Puffer und der
    // Runtime friert ein. Die Logs sind ohnehin im Web-UI (GET /v1/logs).
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        cmd.stdout(std::process::Stdio::null());
    }
    if !std::io::stderr().is_terminal() {
        cmd.stderr(std::process::Stdio::null());
    }
    match cmd.spawn() {
        Ok(_) => {
            // Antwort erst raus, dann Prozess beenden (Port wird dadurch frei;
            // der neue Prozess retried das Binden, siehe main.rs).
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                std::process::exit(0);
            });
            Json(json!({ "status": "restarting" }))
        }
        Err(e) => {
            tracing::error!(error = %e, "restart spawn failed");
            Json(json!({ "status": "error", "error": e.to_string() }))
        }
    }
}
/// `GET /v1/logs` — letzte Log-Einträge aus dem In-Memory-Ringbuffer (Loguru-Stil), optional `?limit=N` (default 200).
pub async fn logs(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let limit = q.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(200);
    let entries = state.logs.snapshot(limit);
    Json(json!({ "total": entries.len(), "logs": entries }))
}

/// `POST /v1/logs/clear` — leert den Log-Ringbuffer.
pub async fn logs_clear(State(state): State<AppState>) -> Json<Value> {
    let cleared = state.logs.clear();
    Json(json!({ "cleared": cleared }))
}

/// `POST /v1/admin/alerts/test` — feuert einen Test-Alert (Einstellungen-Tab / `router-admin alerts test`).
pub async fn admin_alerts_test(State(state): State<AppState>) -> Json<Value> {
    state.alerts.fire_test();
    Json(json!({ "ok": true, "message": "Test-Alert ausgelöst (Webhook/Telegram, sofern konfiguriert)" }))
}

/// `GET /v1/admin/keys` — konfigurierte API-Keys (Hash nur maskiert).
pub async fn admin_keys(State(state): State<AppState>) -> Json<Value> {
    let keys: Vec<Value> = state
        .config
        .auth
        .keys
        .iter()
        .map(|k| {
            json!({
                "name": k.name,
                "hash_prefix": k.hash.chars().take(16).collect::<String>() + "…",
                "daily_budget_usd": k.daily_budget_usd,
                "monthly_budget_usd": k.monthly_budget_usd,
            })
        })
        .collect();
    Json(json!({ "enabled": state.config.auth.enabled, "allow_ui": state.config.auth.allow_ui, "keys": keys }))
}

/// `POST /v1/admin/keys` — erzeugt einen neuen Key (Plaintext wird GENAU EINMAL zurückgegeben),
/// schreibt `auth.keys` per toml_edit in die Config und aktiviert auth.
pub async fn admin_keys_create(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let name = body.get("name").and_then(|v| v.as_str()).map(|s| s.trim().to_string());
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        return Err(ApiError::BadRequest("name erforderlich".into()));
    };
    let daily = body.get("daily_budget_usd").and_then(|v| v.as_f64());
    let monthly = body.get("monthly_budget_usd").and_then(|v| v.as_f64());
    let (plain, hash) = crate::auth::generate_key();
    let path = &*state.config_path;
    let mut doc = crate::configedit::ConfigEditor::load(path)
        .map_err(|e| ApiError::Internal(format!("config read: {e}")))?;
    crate::configedit::ConfigEditor::add_auth_key(&mut doc, &name, &hash, daily, monthly)
        .map_err(|e| ApiError::BadRequest(e))?;
    crate::configedit::ConfigEditor::save(path, &doc)
        .map_err(|e| ApiError::Internal(format!("config write: {e}")))?;
    tracing::info!(name, "created api key (plaintext shown once)");
    Ok(Json(json!({
        "name": name,
        "key": plain,
        "warning": "Speichere den Key jetzt — er wird nur dieses eine Mal angezeigt.",
        "restart_required": true,
    })))
}

/// `POST /v1/admin/keys/remove` — entfernt einen Key aus der Config.
pub async fn admin_keys_remove(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let path = &*state.config_path;
    let mut doc = crate::configedit::ConfigEditor::load(path)
        .map_err(|e| ApiError::Internal(format!("config read: {e}")))?;
    let removed = crate::configedit::ConfigEditor::remove_auth_key(&mut doc, name)
        .map_err(|e| ApiError::BadRequest(e))?;
    if !removed {
        return Err(ApiError::BadRequest(format!("Key '{name}' nicht gefunden")));
    }
    crate::configedit::ConfigEditor::save(path, &doc)
        .map_err(|e| ApiError::Internal(format!("config write: {e}")))?;
    tracing::info!(name, "removed api key");
    Ok(Json(json!({ "removed": name, "restart_required": true })))
}

/// `POST /v1/admin/config` — setzt einen verschachtelten Config-Wert (dotted key)
/// per toml_edit; wirksam nach Neustart. Body: `{"set": {"alerts.webhook_url": "…"}}`.
pub async fn admin_config_set(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let sets = body.get("set").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    if sets.is_empty() {
        return Err(ApiError::BadRequest("leeres `set`-Objekt".into()));
    }
    let path = &*state.config_path;
    let mut doc = crate::configedit::ConfigEditor::load(path)
        .map_err(|e| ApiError::Internal(format!("config read: {e}")))?;
    for (k, v) in &sets {
        let value = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => return Err(ApiError::BadRequest(format!("Wert für '{k}' nicht unterstützt: {other}"))),
        };
        crate::configedit::ConfigEditor::set(&mut doc, k, &value)
            .map_err(|e| ApiError::BadRequest(e))?;
        tracing::info!(key = %k, "config value updated");
    }
    crate::configedit::ConfigEditor::save(path, &doc)
        .map_err(|e| ApiError::Internal(format!("config write: {e}")))?;
    Ok(Json(json!({ "updated": sets.keys().collect::<Vec<_>>(), "restart_required": true })))
}

/// `GET /v1/transactions` — aktuelle Session-Summe + letzte Aufrufe fürs Widget, optional `?limit=N` (default 10).
pub async fn transactions(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let limit = q.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
    let snap = state.history.snapshot(limit);
    Json(serde_json::to_value(snap).unwrap_or_else(|_| serde_json::json!({})))
}

/// `GET /v1/stats` — persistente Tages-Serie aus SQLite fürs Kosten-Dashboard.
/// Query: `days` (default 30), `key` (optionaler API-Key-Name).
pub async fn stats(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let days = q.get("days").and_then(|s| s.parse::<f64>().ok()).unwrap_or(30.0).min(365.0);
    let key = q.get("key").filter(|s| !s.is_empty()).map(|s| s.to_string());
    let from = crate::history::now_unix() as f64 - days * 86_400.0;
    let series = state.store.series(from, key.as_deref());
    let total_cost: f64 = series.iter().map(|p| p.cost_usd).sum();
    let total_calls: i64 = series.iter().map(|p| p.count).sum();
    Json(json!({
        "days": days,
        "from_unix": from,
        "total_cost_usd": total_cost,
        "total_calls": total_calls,
        "series": series,
    }))
}

/// `GET /v1/breakdown` — Kosten-/Aufruf-Summen gruppiert nach Spalte.
/// Query: `days` (default 7), `by` = `profile|backend|model|key`, `limit` (default 10), `key`.
pub async fn breakdown(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<Value> {
    let days = q.get("days").and_then(|s| s.parse::<f64>().ok()).unwrap_or(7.0).min(365.0);
    let by = q.get("by").map(|s| s.to_string()).unwrap_or_else(|| "backend".into());
    let limit = q.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(10);
    let key = q.get("key").filter(|s| !s.is_empty()).map(|s| s.to_string());
    let from = crate::history::now_unix() as f64 - days * 86_400.0;
    let rows = state.store.breakdown(from, &by, key.as_deref(), limit);
    Json(json!({ "by": by, "days": days, "rows": rows }))
}

/// `GET /v1/intelligence` — Bewertungs-Übersicht pro Router-Modell, mergt Katalog mit Artificial-Analysis-Scores; Query: `sort`, `min_intelligence`, `backend`, `limit`, `unrated`.
pub async fn intelligence(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let aa = state.registry.artificial_analysis();
    let aa_index = if aa.enabled() {
        match aa.snapshot().await {
            Ok(idx) => idx,
            Err(e) => return Err(ApiError::Upstream(e.to_string())),
        }
    } else {
        return Ok(Json(json!({
            "enabled": false,
            "hint": "set [registry.intelligence] enabled = true and export AA_API_KEY (https://artificialanalysis.ai/documentation)",
            "models": [],
            "summary": null,
        })));
    };

    let snap = state
        .registry
        .snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;

    let backend_filter = q.get("backend").cloned();
    let min_intel = q.get("min_intelligence").and_then(|s| s.parse::<f64>().ok());
    let unrated = q.get("unrated").map(|s| s == "true").unwrap_or(false);
    let sort_key = q.get("sort").map(|s| s.as_str()).unwrap_or("intelligence").to_string();
    let limit = q.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(100);

    let mut rows: Vec<Value> = Vec::new();
    for m in snap.models.iter() {
        if let Some(b) = &backend_filter {
            if !m.backend_id.eq_ignore_ascii_case(b) { continue; }
        }
        let aa_slug = aa.aa_slug_for(&m.id);
        let scores = aa.lookup(&aa_index, &m.id);
        let intel = scores.and_then(|s| s.intelligence_index);
        if !unrated && intel.is_none() { continue; }
        if let (Some(cap), Some(v)) = (min_intel, intel) {
            if v < cap { continue; }
        }
        rows.push(json!({
            "router_id":           m.id,
            "backend":             m.backend_id,
            "aa_slug":             aa_slug,
            "rated":               intel.is_some(),
            "intelligence_index":  intel,
            "coding_index":        scores.and_then(|s| s.coding_index),
            "math_index":          scores.and_then(|s| s.math_index),
            "tps":                 scores.and_then(|s| s.median_output_tokens_per_second),
            "ttft_seconds":        scores.and_then(|s| s.median_time_to_first_token_seconds),
            "price_in_per_mtok":   m.price_in_per_mtok,
            "price_out_per_mtok":  m.price_out_per_mtok,
            "context_length":      m.context_length,
            "privacy_class":       format!("{:?}", m.privacy_class),
        }));
    }

    let key = |r: &Value, field: &str| -> f64 {
        r.get(field).and_then(|v| v.as_f64()).unwrap_or(f64::NEG_INFINITY)
    };
    match sort_key.as_str() {
        "cost" => rows.sort_by(|a, b| {
            let pa = a.get("price_out_per_mtok").and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY);
            let pb = b.get("price_out_per_mtok").and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY);
            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "tps" => rows.sort_by(|a, b| key(b, "tps").partial_cmp(&key(a, "tps")).unwrap_or(std::cmp::Ordering::Equal)),
        "ttft" => rows.sort_by(|a, b| {
            let ta = a.get("ttft_seconds").and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY);
            let tb = b.get("ttft_seconds").and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY);
            ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
        }),
        "none" => {}
        _ => rows.sort_by(|a, b| {
            key(b, "intelligence_index")
                .partial_cmp(&key(a, "intelligence_index"))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
    }

    let total_models = snap.models.len();
    let rated_total = snap.models.iter().filter(|m| {
        aa.lookup(&aa_index, &m.id).and_then(|s| s.intelligence_index).is_some()
    }).count();
    let truncated = rows.len() > limit;
    rows.truncate(limit);

    Ok(Json(json!({
        "enabled": true,
        "summary": {
            "total_router_models":   total_models,
            "rated_router_models":   rated_total,
            "aa_index_size":         aa_index.len(),
            "sort":                  sort_key,
            "returned":              rows.len(),
            "truncated":             truncated,
            "ttl_seconds":           86400_u64,
        },
        "methodology": {
            "source": "https://artificialanalysis.ai/",
            "endpoint": "/api/v2/data/llms/models",
            "intelligence_index_evals": [
                "GDPval-AA", "Terminal-Bench Hard", "τ²-Bench Telecom", "SciCode",
                "AA-LCR", "AA-Omniscience", "IFBench", "Humanity's Last Exam",
                "GPQA Diamond", "CritPt"
            ],
            "scoring_use": "quality_score = intelligence_index / 100, applied with profile weight 'quality'",
            "matching": "registry.intelligence.aliases takes precedence; fallback: lowercase suffix after last '/' with '.'→'-'",
            "attribution_required": true
        },
        "models": rows,
    })))
}

/// `GET /v1/registry` — vollständige Modell-Liste mit allen Parametern.
pub async fn registry(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let snap = state
        .registry
        .enriched_snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let models: Vec<Value> = snap.models.iter().map(candidate_to_json).collect();
    Ok(Json(json!({ "total": models.len(), "models": models })))
}

/// `POST /v1/explain` — Dry-Run des Expertensystems ohne Egress; Body wie `/v1/chat/completions` oder `/v1/messages`, Format wird an `system`/`thinking` erkannt oder via `?format=anthropic|openai` erzwungen.
pub async fn explain(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let (profile_hdr, privacy_hdr) = headers_to_hints(&headers);
    let format = q.get("format").map(|s| s.as_str()).unwrap_or_else(|| {
        if body.get("system").is_some() || body.get("thinking").is_some() {
            "anthropic"
        } else {
            "openai"
        }
    });
    let mut norm = match format {
        "anthropic" => anthropic_to_norm(&body)?,
        _ => openai_to_norm(&body)?,
    };
    if norm.profile_hint.is_none() {
        norm.profile_hint = profile_hdr;
    }
    if privacy_hdr.is_some() {
        norm.privacy_tag = parse_privacy_tag(privacy_hdr.as_deref());
    }
    norm.detect_required();
    resolve_auto_alias(&mut norm, &state.config);

    let snap = state
        .registry
        .enriched_snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let (profile, decision) = decide_for(&norm, &state.config, &snap)?;

    Ok(Json(json!({
        "winner": decision.winner.id,
        "winner_backend": decision.winner.backend_id,
        "profile": profile.name,
        "weights": {
            "cost":       profile.weights.cost,
            "latency":    profile.weights.latency,
            "context":    profile.weights.context,
            "preference": profile.weights.preference,
            "quality":    profile.weights.quality,
        },
        "constraints": {
            "max_price_out_per_mtok": profile.max_price_out_per_mtok,
            "max_price_in_per_mtok":  profile.max_price_in_per_mtok,
            "max_latency_p95_ms":     profile.max_latency_p95_ms,
            "min_intelligence_index": profile.min_intelligence_index,
            "require_privacy_class":  profile.require_privacy_class.iter()
                                          .map(|c| format!("{c:?}")).collect::<Vec<_>>(),
            "backend_allowlist":      profile.backend_allowlist.iter()
                                          .cloned().collect::<Vec<_>>(),
        },
        "request": {
            "format":              format,
            "prompt_tokens_est":   norm.prompt_tokens_est,
            "max_tokens":          norm.max_tokens,
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
        "backend":               m.backend_id,
        "provider":              m.provider_slug,
        "context_length":        m.context_length,
        "max_completion_tokens": m.max_completion_tokens,
        "pricing": {
            "input_per_mtok_usd":  m.price_in_per_mtok,
            "output_per_mtok_usd": m.price_out_per_mtok,
        },
        "input_modalities":   modalities,
        "capabilities":       caps,
        "privacy_class":      format!("{:?}", m.privacy_class),
        "is_moderated":       m.is_moderated,
        "measured_p95_ms":    m.measured_p95_ms,
        "intelligence_index": m.intelligence_index,
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
