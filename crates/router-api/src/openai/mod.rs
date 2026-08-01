//! OpenAI-kompatible Handler.

use std::time::Instant;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::stream::StreamExt;
use futures::Stream;
use router_core::{NormMessage, NormRequest, NormRole};
use serde_json::{json, Value};

use crate::error::ApiError;
use crate::history::now_unix;
use crate::routing::{announce_completion, announce_decision, decide_for, headers_to_hints, parse_privacy_tag, resolve_auto_alias, stream_with_fallback};
use crate::sse::{find_event_boundary, parse_sse_data};
use crate::state::AppState;

/// `GET /v1/models` — Union aus gemergter Registry.
pub async fn list_models(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    state.rotator.maybe_rotate(&state.alerts).await;
    let snap = state
        .registry
        .enriched_snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    // Synthetic auto-routing models first, so GUI clients can pick auto-routing (and a profile) from the model dropdown; resolved in `routing::resolve_auto_alias`.
    let mut data: Vec<Value> = Vec::with_capacity(snap.models.len() + state.config.profiles.len() + 1);
    data.push(auto_model("auto", "auto-routing · default/active profile"));
    for name in state.config.profiles.keys() {
        data.push(auto_model(&format!("{name}/auto"), &format!("auto-routing · '{name}' profile")));
    }

    data.extend(snap.models.iter().map(|m| {
        // OpenRouter-IDs tragen den echten Anbieter im Slug (z. B. "anthropic"), sonst ist die Backend-Instanz der Eigentümer.
        let owned_by = if m.backend_id == "openrouter" {
            m.provider_slug.clone()
        } else {
            m.backend_id.clone()
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
            "backend": m.backend_id,
        })
    }));
    Ok(Json(json!({ "object": "list", "data": data })))
}

/// `POST /v1/chat/completions` — Streaming bevorzugt.
pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let (profile_hdr, privacy_hdr) = headers_to_hints(&headers);
    let key_name = crate::auth::lookup_key(&headers, &state.config);
    state.rotator.run_housekeeping(&state).await;
    let mut norm = openai_to_norm(&body)?;
    if norm.profile_hint.is_none() {
        norm.profile_hint = profile_hdr.clone();
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
    announce_decision("openai", &profile, &decision, &norm);

    dispatch_openai(state, profile, decision, norm, body, key_name).await
}

/// A synthetic `/v1/models` entry that maps to auto-routing.
fn auto_model(id: &str, desc: &str) -> Value {
    json!({
        "id": id,
        "object": "model",
        "created": 0,
        "owned_by": "router",
        "description": desc,
        "backend": "router",
    })
}

async fn dispatch_openai(
    state: AppState,
    profile: router_core::ResolvedProfile,
    decision: router_core::Decision,
    norm: NormRequest,
    original_body: Value,
    key_name: Option<String>,
) -> Result<Response, ApiError> {
    let stream = norm.stream;
    let started = Instant::now();

    let (winner, byte_stream) =
        stream_with_fallback(&state, &profile, decision, "openai", |_| original_body.clone())
            .await?;

    if stream {
        let event_stream = sse_from_bytes(
            byte_stream,
            state.tracker.clone(),
            state.history.clone(),
            state.store.clone(),
            key_name,
            profile.name.clone(),
            winner.backend_id.clone(),
            winner.id.clone(),
            started,
        );
        let resp = Sse::new(event_stream).keep_alive(KeepAlive::default());
        Ok(resp.into_response())
    } else {
        let bytes = collect_stream(byte_stream).await?;
        let elapsed = started.elapsed();
        state.tracker.record(&winner.backend_id, &winner.id, elapsed);
        let acc = accumulate_completion(&bytes);
        let cost = acc.cost;
        announce_completion("openai", &winner.id, elapsed, cost);
        state.history.record(crate::history::Transaction {
            unix_ts: now_unix(),
            api: "openai".into(),
            profile: profile.name.clone(),
            backend: winner.backend_id.clone(),
            model_id: winner.id.clone(),
            duration_ms: elapsed.as_millis() as u64,
            cost_usd: cost,
            tokens_out: acc.completion_tokens.unwrap_or(0),
        });
        let _ = state.store.insert(
            &crate::history::Transaction {
                unix_ts: now_unix(),
                api: "openai".into(),
                profile: profile.name.clone(),
                backend: winner.backend_id.clone(),
                model_id: winner.id.clone(),
                duration_ms: elapsed.as_millis() as u64,
                cost_usd: cost,
                tokens_out: acc.completion_tokens.unwrap_or(0),
            },
            key_name.as_deref(),
            acc.prompt_tokens.unwrap_or(0),
            None,
        );
        let aggregated = aggregate_openai_sse(&acc, &winner.id);
        Ok(Json(aggregated).into_response())
    }
}

fn sse_from_bytes<S>(
    inner: S,
    tracker: router_providers::LatencyTracker,
    history: crate::history::TransactionHistory,
    store: crate::store::Store,
    key_name: Option<String>,
    profile_name: String,
    backend_id: String,
    model_id: String,
    started: Instant,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>>
where
    S: Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
{
    use futures::stream::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    // Gemeinsame Kosten-/Token-Zellen zwischen flat_map und Stream-Ende.
    let cost_cell = std::sync::Arc::new(std::sync::Mutex::new(Option::<f64>::None));
    let cost_cell_for_stream = cost_cell.clone();
    let tokens_cell = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let tokens_cell_for_stream = tokens_cell.clone();
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
                        if let Some(t) = extract_out_tokens(&data) {
                            *tokens_cell_for_stream.lock().unwrap() = t;
                        }
                        events.push(Ok(Event::default().data(data)));
                    }
                }
            }
            Err(e) => {
                events.push(Ok(Event::default()
                    .event("error")
                    .data(json!({ "error": e.to_string() }).to_string())));
            }
        }
        futures::stream::iter(events)
    });
    // Bei Stream-Ende Latenz messen (via `then` + Async-Klon).
    s.chain(futures::stream::once(async move {
        let elapsed = started.elapsed();
        tracker.record(&backend_id, &model_id, elapsed);
        let cost = *cost_cell.lock().unwrap();
        let tokens_out = *tokens_cell.lock().unwrap();
        crate::routing::announce_completion("openai", &model_id, elapsed, cost);
        history.record(crate::history::Transaction {
            unix_ts: now_unix(),
            api: "openai".into(),
            profile: profile_name.clone(),
            backend: backend_id.clone(),
            model_id: model_id.clone(),
            duration_ms: elapsed.as_millis() as u64,
            cost_usd: cost,
            tokens_out,
        });
        let _ = store.insert(
            &crate::history::Transaction {
                unix_ts: now_unix(),
                api: "openai".into(),
                profile: profile_name,
                backend: backend_id.clone(),
                model_id: model_id.clone(),
                duration_ms: elapsed.as_millis() as u64,
                cost_usd: cost,
                tokens_out,
            },
            key_name.as_deref(),
            0,
            None,
        );
        // SSE-Comment (`: ...`) statt `data:`-Event, damit kein Client es als JSON fehlinterpretiert — `data: [DONE]` kam schon vom Upstream.
        Ok(Event::default().comment("done"))
    }))
}

/// Sucht in einem JSON-Fragment (SSE-Datablock oder aggregiertem Body) nach `usage.cost` — OpenRouter liefert das Feld in USD.
fn extract_cost(data: &str) -> Option<f64> {
    if data == "[DONE]" { return None; }
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("usage")?.get("cost")?.as_f64()
}

/// Liest `usage.completion_tokens` aus einem SSE-Datablock (finaler Usage-Event).
fn extract_out_tokens(data: &str) -> Option<u64> {
    if data == "[DONE]" { return None; }
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("usage")?.get("completion_tokens")?.as_u64()
}

pub(crate) async fn collect_stream<S>(mut stream: S) -> Result<Vec<u8>, ApiError>
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

/// Ein zusammengebauter Tool-Call aus den `delta.tool_calls`-Fragmenten eines OpenAI-SSE-Streams; `arguments` wird über mehrere Chunks konkateniert.
#[derive(Default, Clone)]
pub(crate) struct ToolAccum {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Das komplette Ergebnis eines abgesammelten OpenAI-Chat-Completion-Streams.
#[derive(Default)]
pub(crate) struct Accumulated {
    pub content: String,
    pub tool_calls: Vec<ToolAccum>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cost: Option<f64>,
}

/// Läuft über einen kompletten OpenAI-SSE-Body und baut Text, Tool-Calls, finish_reason und Usage zusammen (CRLF/LF, auch reine Usage-Events ohne `delta`).
pub(crate) fn accumulate_completion(raw: &[u8]) -> Accumulated {
    let mut acc = Accumulated::default();
    let Ok(text) = std::str::from_utf8(raw) else { return acc };
    // CRLF → LF normalisieren, damit `split("\n\n")` beide Upstream-Varianten trifft.
    let normalized = text.replace("\r\n", "\n");
    for block in normalized.split("\n\n") {
        let data = block
            .lines()
            .filter_map(|l| l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:")))
            .collect::<Vec<_>>()
            .join("\n");
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" { continue; }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        if let Some(u) = v.get("usage") {
            if let Some(p) = u.get("prompt_tokens").and_then(|x| x.as_u64()) { acc.prompt_tokens = Some(p); }
            if let Some(c) = u.get("completion_tokens").and_then(|x| x.as_u64()) { acc.completion_tokens = Some(c); }
            if let Some(c) = u.get("cost").and_then(|x| x.as_f64()) { acc.cost = Some(c); }
        }
        let Some(choice) = v["choices"].get(0) else { continue };
        // OpenRouter/oMLX streamen immer (stream=true erzwungen), also `delta`; `message` fangen wir als Fallback ab, falls ein Upstream doch einen Vollblock schickt.
        let node = choice.get("delta").or_else(|| choice.get("message"));
        if let Some(node) = node {
            if let Some(s) = node.get("content").and_then(|c| c.as_str()) {
                acc.content.push_str(s);
            }
            if let Some(tcs) = node.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    while acc.tool_calls.len() <= idx { acc.tool_calls.push(ToolAccum::default()); }
                    let slot = &mut acc.tool_calls[idx];
                    if let Some(id) = tc.get("id").and_then(|x| x.as_str()) {
                        if !id.is_empty() { slot.id = id.to_string(); }
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").and_then(|x| x.as_str()) {
                            if !n.is_empty() { slot.name = n.to_string(); }
                        }
                        if let Some(a) = f.get("arguments").and_then(|x| x.as_str()) {
                            slot.arguments.push_str(a);
                        }
                    }
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            acc.finish_reason = Some(fr.to_string());
        }
    }
    acc
}

/// Aggregiert einen abgesammelten SSE-Stream zu einem klassischen OpenAI-Chat-Completion-Non-Stream-Body (für Clients mit `stream=false`).
fn aggregate_openai_sse(acc: &Accumulated, model_id: &str) -> Value {
    let mut message = json!({ "role": "assistant" });
    let obj = message.as_object_mut().unwrap();
    if acc.tool_calls.is_empty() {
        obj.insert("content".into(), json!(acc.content));
    } else {
        // OpenAI setzt `content` bei reinen Tool-Calls auf null.
        obj.insert(
            "content".into(),
            if acc.content.is_empty() { Value::Null } else { json!(acc.content) },
        );
        let tcs: Vec<Value> = acc
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, t)| {
                json!({
                    "id": if t.id.is_empty() { format!("call_{i}") } else { t.id.clone() },
                    "type": "function",
                    "function": { "name": t.name, "arguments": t.arguments },
                })
            })
            .collect();
        obj.insert("tool_calls".into(), json!(tcs));
    }

    let finish_reason = acc.finish_reason.clone().unwrap_or_else(|| {
        if acc.tool_calls.is_empty() { "stop".to_string() } else { "tool_calls".to_string() }
    });

    json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": now_unix() as i64,
        "model": model_id,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": acc.prompt_tokens.unwrap_or(0),
            "completion_tokens": acc.completion_tokens.unwrap_or(0),
            "total_tokens": acc.prompt_tokens.unwrap_or(0) + acc.completion_tokens.unwrap_or(0),
        },
    })
}

/// Übersetzt OpenAI-Chat-Completions-Body -> internes NormRequest.
pub fn openai_to_norm(body: &Value) -> Result<NormRequest, ApiError> {
    let mut req = NormRequest {
        model_hint: body.get("model").and_then(|v| v.as_str()).map(|s| s.to_string()),
        profile_hint: body
            .get("route_profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        stream: body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false),
        temperature: body.get("temperature").and_then(|v| v.as_f64()).map(|x| x as f32),
        top_p: body.get("top_p").and_then(|v| v.as_f64()).map(|x| x as f32),
        max_tokens: body.get("max_tokens").and_then(|v| v.as_u64()).map(|x| x as u32),
        ..Default::default()
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_text_and_usage_crlf() {
        // CRLF-separierte Events + Usage-Trailer.
        let raw = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\r\n\r\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\r\n\r\n\
                   data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\r\n\r\n\
                   data: [DONE]\r\n\r\n";
        let acc = accumulate_completion(raw.as_bytes());
        assert_eq!(acc.content, "Hello");
        assert_eq!(acc.finish_reason.as_deref(), Some("stop"));
        assert_eq!(acc.prompt_tokens, Some(5));
        assert_eq!(acc.completion_tokens, Some(2));
        assert!(acc.tool_calls.is_empty());
    }

    #[test]
    fn accumulate_tool_call_across_chunks() {
        // id/name im ersten Chunk, arguments über zwei Chunks gestückelt.
        let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\"}}]}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"Berlin\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
                   data: [DONE]\n\n";
        let acc = accumulate_completion(raw.as_bytes());
        assert_eq!(acc.tool_calls.len(), 1);
        assert_eq!(acc.tool_calls[0].id, "call_1");
        assert_eq!(acc.tool_calls[0].name, "get_weather");
        assert_eq!(acc.tool_calls[0].arguments, "{\"city\":\"Berlin\"}");
    }

    #[test]
    fn aggregate_openai_puts_tool_calls_in_message() {
        let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_9\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
                   data: [DONE]\n\n";
        let v = aggregate_openai_sse(&accumulate_completion(raw.as_bytes()), "m/x");
        let msg = &v["choices"][0]["message"];
        assert!(msg["content"].is_null());
        assert_eq!(msg["tool_calls"][0]["function"]["name"], "f");
        assert_eq!(v["choices"][0]["finish_reason"], "tool_calls");
        assert!(v["usage"].is_object());
    }
}
