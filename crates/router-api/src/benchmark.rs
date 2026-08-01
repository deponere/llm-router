//! Benchmark-Panel (Feature #5): parallele Echt-Calls an bis zu 3 Modellen,
//! misst TTFT (Time-To-First-Token), Gesamtdauer, Tokens und Kosten.
//! Achtung: echte Upstream-Calls — kostet Geld (UI zeigt einen Hinweis).

use std::time::Instant;

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::state::AppState;

/// `POST /v1/benchmark` — Body: `{messages, models: [≤3], max_tokens?, temperature?}`.
/// Response: `{results: [{model_id, backend, ttft_ms, total_ms, tokens_out, cost_usd, first_chunk}]}`.
pub async fn benchmark(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let messages = body.get("messages").cloned().unwrap_or_else(|| json!([]));
    let max_tokens = body.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(512).min(4096);
    let temperature = body.get("temperature").and_then(|v| v.as_f64()).unwrap_or(0.7);
    let models: Vec<String> = body
        .get("models")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.as_str().map(|s| s.to_string()))
                .take(3)
                .collect()
        })
        .unwrap_or_default();
    if models.is_empty() {
        return Err(ApiError::BadRequest("models[] erforderlich (max 3)".into()));
    }

    let snap = state
        .registry
        .enriched_snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let profile = router_core::profile::ResolvedProfile::resolve(&state.config, Some("default"));

    let mut tasks = Vec::new();
    for model_id in &models {
        let (s, msgs, mid, snap2, profile2) = (
            state.clone(),
            messages.clone(),
            model_id.clone(),
            snap.clone(),
            profile.clone(),
        );
        tasks.push(tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(60),
                run_one(&s, &snap2, &profile2, &mid, &msgs, max_tokens, temperature),
            )
            .await
            .unwrap_or_else(|_| {
                json!({ "model_id": mid, "error": "timeout (60 s)" })
            })
        }));
    }

    let mut results = Vec::new();
    for t in tasks {
        if let Ok(r) = t.await {
            results.push(r);
        }
    }
    Ok(Json(json!({ "results": results })))
}

async fn run_one(
    state: &AppState,
    snap: &router_core::registry::Registry,
    profile: &router_core::ResolvedProfile,
    model_id: &str,
    messages: &Value,
    max_tokens: u64,
    temperature: f64,
) -> Value {
    let Some(cand) = snap.models.iter().find(|m| m.id == model_id) else {
        return json!({ "model_id": model_id, "error": "model not in registry" });
    };
    let Some(provider) = state.registry.provider(&cand.backend_id) else {
        return json!({ "model_id": model_id, "error": format!("backend '{}' unavailable", cand.backend_id) });
    };
    let body = json!({
        "model": model_id,
        "messages": messages,
        "stream": true,
        "max_tokens": max_tokens,
        "temperature": temperature,
    });

    let started = Instant::now();
    let stream = match provider.chat_completion_stream(model_id, profile, body).await {
        Ok(s) => s,
        Err(e) => return json!({ "model_id": model_id, "error": e.to_string() }),
    };

    use futures::StreamExt;
    let mut ttft_ms: Option<u64> = None;
    let mut tokens_out: u64 = 0;
    let mut chars: usize = 0;
    let mut first_chunk = String::new();
    let mut cost: Option<f64> = None;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunks = 0usize;
    let mut parsed_events = 0usize;
    let mut first_bytes: Option<usize> = None;

    let mut stream = Box::pin(stream);
    loop {
        match stream.next().await {
            Some(Ok(bytes)) => {
                if first_bytes.is_none() {
                    first_bytes = Some(bytes.len());
                    tracing::debug!(model_id, n = bytes.len(), "benchmark first chunk bytes");
                }
                buf.extend_from_slice(&bytes);
                while let Some(pos) = find_boundary(&buf) {
                    let data = std::str::from_utf8(&buf[..pos]).unwrap_or("").to_string();
                    buf.drain(..pos + 2.min(buf.len() - pos));
                    if let Some(v) = parse_sse_json(&data) {
                        parsed_events += 1;
                        if let Some(c) = v.pointer("/usage/cost").and_then(|x| x.as_f64()) {
                            cost = Some(c);
                        }
                        if let Some(t) = v.pointer("/usage/completion_tokens").and_then(|x| x.as_u64()) {
                            tokens_out = t;
                        }
                        if let Some(delta) = v.pointer("/choices/0/delta/content").and_then(|x| x.as_str()) {
                            if !delta.is_empty() {
                                if ttft_ms.is_none() {
                                    ttft_ms = Some(started.elapsed().as_millis() as u64);
                                }
                                chars += delta.chars().count();
                                if first_chunk.is_empty() {
                                    first_chunk = delta.chars().take(120).collect();
                                }
                            }
                        }
                        // Reasoning-Modelle senden erst Denk-Tokens; das erste
                        // non-leere Delta (Content ODER Reasoning) zählt als TTFT.
                        if ttft_ms.is_none() {
                            if let Some(r) = v.pointer("/choices/0/delta/reasoning").and_then(|x| x.as_str()) {
                                if !r.is_empty() {
                                    ttft_ms = Some(started.elapsed().as_millis() as u64);
                                }
                            }
                        }
                    }
                }
                chunks += 1;
                // Nach dem ersten Token weiterlesen, bis Usage (Tokens/Kosten) da ist —
                // aber höchstens 10 s, damit der Benchmark nicht endlos läuft.
                if ttft_ms.is_some() {
                    let usage_seen = cost.is_some() || tokens_out > 0;
                    if started.elapsed().as_secs() >= 10 || (usage_seen && chunks >= 20) {
                        break;
                    }
                }
            }
            Some(Err(e)) => {
                return json!({
                    "model_id": model_id,
                    "backend": cand.backend_id,
                    "error": e.to_string(),
                });
            }
            None => break,
        }
    }
    let total_ms = started.elapsed().as_millis() as u64;
    tracing::info!(model_id, %parsed_events, %chunks, ttft = ?ttft_ms, %total_ms, "benchmark run done");
    if tokens_out == 0 && chars > 0 {
        tokens_out = (chars / 4).max(1) as u64; // grobe Schätzung ohne Tokenizer
    }
    let cost_usd = cost.or_else(|| {
        let out_cost = tokens_out as f64 / 1e6 * cand.price_out_per_mtok;
        if out_cost > 0.0 { Some(out_cost) } else { None }
    });
    json!({
        "model_id": model_id,
        "backend": cand.backend_id,
        "ttft_ms": ttft_ms,
        "total_ms": total_ms,
        "tokens_out": tokens_out,
        "cost_usd": cost_usd,
        "first_chunk": first_chunk,
    })
}

fn find_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| buf.windows(2).position(|w| w == b"\r\n"))
}

fn parse_sse_json(data: &str) -> Option<Value> {
    let d = data.strip_prefix("data:")?.trim();
    if d.is_empty() || d == "[DONE]" {
        return None;
    }
    serde_json::from_str(d).ok()
}
