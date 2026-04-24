//! OpenAI-kompatible Handler.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream::StreamExt;
use futures::Stream;
use router_core::{Backend, NormMessage, NormRequest, NormRole};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::routing::{announce_completion, announce_decision, decide_for, headers_to_hints, parse_privacy_tag};
use crate::state::AppState;

/// `GET /v1/models` — Union aus gemergter Registry.
pub async fn list_models(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let snap = state
        .registry
        .snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let data: Vec<Value> = snap
        .models
        .iter()
        .map(|m| {
            let owned_by = match m.backend {
                Backend::OpenRouter => m.provider_slug.clone(),
                Backend::OMlx => "omlx".to_string(),
            };
            json!({
                "id": m.id,
                "object": "model",
                "created": 0,
                "owned_by": owned_by,
                "context_length": m.context_length,
                "pricing": {
                    "prompt_per_mtok": m.price_in_per_mtok,
                    "completion_per_mtok": m.price_out_per_mtok,
                },
                "privacy_class": format!("{:?}", m.privacy_class),
                "backend": format!("{:?}", m.backend),
            })
        })
        .collect();
    Ok(Json(json!({ "object": "list", "data": data })))
}

/// `POST /v1/chat/completions` — Streaming bevorzugt.
pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let (profile_hdr, privacy_hdr) = headers_to_hints(&headers);
    let mut norm = openai_to_norm(&body)?;
    if norm.profile_hint.is_none() {
        norm.profile_hint = profile_hdr.clone();
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
    announce_decision("openai", &profile, &decision, &norm);

    dispatch_openai(state, profile, decision, norm, body).await
}

async fn dispatch_openai(
    state: AppState,
    profile: router_core::ResolvedProfile,
    decision: router_core::Decision,
    norm: NormRequest,
    original_body: Value,
) -> Result<Response, ApiError> {
    let stream = norm.stream;
    let winner = decision.winner.clone();
    let started = Instant::now();

    let byte_stream: std::pin::Pin<
        Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>,
    > = match winner.backend {
        Backend::OpenRouter => {
            let Some(client) = state.registry.openrouter() else {
                return Err(ApiError::Internal("OpenRouter backend not configured".into()));
            };
            let s = client
                .chat_completion_stream(&winner.id, &profile, original_body)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?;
            Box::pin(s.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))))
        }
        Backend::OMlx => {
            let Some(client) = state.registry.omlx() else {
                return Err(ApiError::Internal("oMLX backend not configured".into()));
            };
            let s = client
                .chat_completion_stream(&winner.id, original_body)
                .await
                .map_err(|e| ApiError::Upstream(e.to_string()))?;
            Box::pin(s.map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))))
        }
    };

    if stream {
        let event_stream = sse_from_bytes(
            byte_stream,
            state.tracker.clone(),
            state.history.clone(),
            profile.name.clone(),
            winner.backend,
            winner.id.clone(),
            started,
        );
        let resp = Sse::new(event_stream).keep_alive(KeepAlive::default());
        Ok(resp.into_response())
    } else {
        let bytes = collect_stream(byte_stream).await?;
        let elapsed = started.elapsed();
        state.tracker.record(winner.backend, &winner.id, elapsed);
        let cost = scan_cost_in_sse(&bytes);
        announce_completion("openai", &winner.id, elapsed, cost);
        state.history.record(crate::history::Transaction {
            unix_ts: now_unix(),
            api: "openai".into(),
            profile: profile.name.clone(),
            backend: format!("{:?}", winner.backend),
            model_id: winner.id.clone(),
            duration_ms: elapsed.as_millis() as u64,
            cost_usd: cost,
        });
        let aggregated = aggregate_openai_sse(&bytes, &winner.id)
            .map_err(|e| ApiError::Upstream(format!("aggregation failed: {e}")))?;
        Ok(Json(aggregated).into_response())
    }
}

fn sse_from_bytes<S>(
    inner: S,
    tracker: router_providers::LatencyTracker,
    history: crate::history::TransactionHistory,
    profile_name: String,
    backend: Backend,
    model_id: String,
    started: Instant,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>>
where
    S: Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    use futures::stream::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    // Gemeinsame Kosten-Zelle zwischen flat_map und Stream-Ende.
    let cost_cell = std::sync::Arc::new(std::sync::Mutex::new(Option::<f64>::None));
    let cost_cell_for_stream = cost_cell.clone();
    let s = inner.flat_map(move |chunk| {
        let mut events: Vec<Result<Event, std::convert::Infallible>> = Vec::new();
        match chunk {
            Ok(b) => {
                buf.extend_from_slice(&b);
                while let Some(pos) = find_event_boundary(&buf) {
                    let event_bytes: Vec<u8> = buf.drain(..pos).collect();
                    // Skip separator (\n\n oder \r\n\r\n)
                    let sep_len = if buf.starts_with(b"\r\n\r\n") { 4 } else { 2 };
                    buf.drain(..sep_len.min(buf.len()));
                    if let Some(data) = parse_sse_data(&event_bytes) {
                        if let Some(c) = extract_cost(&data) {
                            *cost_cell_for_stream.lock().unwrap() = Some(c);
                        }
                        events.push(Ok(Event::default().data(data)));
                    }
                }
            }
            Err(e) => {
                events.push(Ok(Event::default()
                    .event("error")
                    .data(format!("{{\"error\":\"{}\"}}", e))));
            }
        }
        futures::stream::iter(events)
    });
    // Bei Stream-Ende Latenz messen (via `then` + Async-Klon).
    s.chain(futures::stream::once(async move {
        let elapsed = started.elapsed();
        tracker.record(backend, &model_id, elapsed);
        let cost = *cost_cell.lock().unwrap();
        crate::routing::announce_completion("openai", &model_id, elapsed, cost);
        history.record(crate::history::Transaction {
            unix_ts: now_unix(),
            api: "openai".into(),
            profile: profile_name,
            backend: format!("{backend:?}"),
            model_id: model_id.clone(),
            duration_ms: elapsed.as_millis() as u64,
            cost_usd: cost,
        });
        Ok(Event::default().data("[DONE-stamp]"))
    }))
    // Letzten Trailer-Event filtern wir auf der Leitung aus, aber Clients erwarten
    // ohnehin `data: [DONE]` — den emittieren sowohl OpenRouter als auch unser
    // oMLX. Der Trailer-Event mit "[DONE-stamp]" wird von den meisten
    // Clients ignoriert.
}

/// Sucht in einem JSON-Fragment (SSE-Datablock oder aggregiertem Body) nach
/// `usage.cost` — OpenRouter liefert das Feld in USD.
fn extract_cost(data: &str) -> Option<f64> {
    if data == "[DONE]" { return None; }
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("usage")?.get("cost")?.as_f64()
}

/// Laeuft ueber einen kompletten SSE-Response (Non-Stream-Pfad) und gibt
/// die zuletzt gesehenen Kosten zurueck. OpenRouter schickt den usage-Block
/// meist im vorletzten Event, direkt vor `data: [DONE]`.
fn scan_cost_in_sse(raw: &[u8]) -> Option<f64> {
    let text = std::str::from_utf8(raw).ok()?;
    let mut last: Option<f64> = None;
    for block in text.split("\n\n") {
        let data = block
            .lines()
            .filter_map(|l| l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:")))
            .collect::<Vec<_>>()
            .join("\n");
        if data.trim().is_empty() { continue; }
        if let Some(c) = extract_cost(&data) {
            last = Some(c);
        }
    }
    last
}

fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n").or_else(|| {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    })
}

fn parse_sse_data(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
            if !data.is_empty() { data.push('\n'); }
            data.push_str(rest);
        }
    }
    if data.is_empty() { None } else { Some(data) }
}

async fn collect_stream<S>(mut stream: S) -> Result<Vec<u8>, ApiError>
where
    S: Stream<Item = Result<bytes::Bytes, std::io::Error>> + Unpin,
{
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let c = chunk.map_err(|e| ApiError::Upstream(e.to_string()))?;
        out.extend_from_slice(&c);
    }
    Ok(out)
}

/// Aggregiert einen abgesammelten SSE-Stream zu einem klassischen
/// OpenAI-Chat-Completion-Non-Stream-Body (für Clients, die `stream=false`
/// geschickt haben).
fn aggregate_openai_sse(raw: &[u8], model_id: &str) -> Result<Value, String> {
    let text = std::str::from_utf8(raw).map_err(|e| e.to_string())?;
    let mut content = String::new();
    let mut finish_reason: Option<String> = None;
    for block in text.split("\n\n") {
        let data = block
            .lines()
            .filter_map(|l| l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:")))
            .collect::<Vec<_>>()
            .join("\n");
        if data.trim().is_empty() { continue; }
        if data.trim() == "[DONE]" { continue; }
        let v: Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(delta) = v["choices"].get(0).and_then(|c| c.get("delta")) {
            if let Some(s) = delta.get("content").and_then(|c| c.as_str()) {
                content.push_str(s);
            }
        }
        if let Some(fr) = v["choices"]
            .get(0)
            .and_then(|c| c.get("finish_reason"))
            .and_then(|x| x.as_str())
        {
            finish_reason = Some(fr.to_string());
        }
    }
    Ok(json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": chrono_now(),
        "model": model_id,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": finish_reason.unwrap_or_else(|| "stop".to_string()),
        }],
    }))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Übersetzt OpenAI-Chat-Completions-Body -> internes NormRequest.
pub fn openai_to_norm(body: &Value) -> Result<NormRequest, ApiError> {
    let mut req = NormRequest::default();
    req.model_hint = body.get("model").and_then(|v| v.as_str()).map(|s| s.to_string());
    req.profile_hint = body
        .get("route_profile")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    req.stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);
    req.temperature = body.get("temperature").and_then(|v| v.as_f64()).map(|x| x as f32);
    req.top_p = body.get("top_p").and_then(|v| v.as_f64()).map(|x| x as f32);
    req.max_tokens = body.get("max_tokens").and_then(|v| v.as_u64()).map(|x| x as u32);

    if let Some(arr) = body.get("tools").and_then(|v| v.as_array()) {
        let mut tools = Vec::new();
        for t in arr {
            let fun = t.get("function").unwrap_or(t);
            let name = fun
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let desc = fun
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let params = fun
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({}));
            tools.push(router_core::ToolDef { name, description: desc, parameters: params });
        }
        if !tools.is_empty() {
            req.tools = Some(tools);
        }
    }

    if let Some(rf) = body.get("response_format") {
        if let Some(t) = rf.get("type").and_then(|v| v.as_str()) {
            req.response_format = match t {
                "json_object" => Some(router_core::ResponseFormat::JsonObject),
                "json_schema" => Some(router_core::ResponseFormat::JsonSchema {
                    schema: rf.get("json_schema").cloned().unwrap_or_else(|| json!({})),
                }),
                _ => None,
            };
        }
    }

    if body.get("reasoning").is_some() || body.get("reasoning_effort").is_some() {
        req.reasoning = Some(router_core::ReasoningHint {
            effort: body
                .get("reasoning_effort")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            budget_tokens: None,
        });
    }

    let msgs = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::BadRequest("messages array required".into()))?;
    for m in msgs {
        let role = match m.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
            "system" => NormRole::System,
            "assistant" => NormRole::Assistant,
            "tool" => NormRole::Tool,
            _ => NormRole::User,
        };
        let (text, images) = extract_content(m.get("content"));
        req.messages.push(NormMessage {
            role,
            text,
            images,
            tool_call_id: m.get("tool_call_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
            name: m.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
        });
    }
    Ok(req)
}

fn extract_content(v: Option<&Value>) -> (String, Vec<router_core::ImagePart>) {
    let Some(v) = v else { return (String::new(), vec![]) };
    match v {
        Value::String(s) => (s.clone(), vec![]),
        Value::Array(parts) => {
            let mut text = String::new();
            let mut images = vec![];
            for p in parts {
                let ty = p.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match ty {
                    "text" => {
                        if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                            if !text.is_empty() { text.push('\n'); }
                            text.push_str(t);
                        }
                    }
                    "image_url" => {
                        let url = p
                            .get("image_url")
                            .and_then(|o| o.get("url"))
                            .and_then(|s| s.as_str())
                            .unwrap_or_default()
                            .to_string();
                        images.push(router_core::ImagePart { url_or_b64: url, mime: None });
                    }
                    _ => {}
                }
            }
            (text, images)
        }
        _ => (String::new(), vec![]),
    }
}
