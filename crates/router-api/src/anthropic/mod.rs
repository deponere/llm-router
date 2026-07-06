//! Anthropic-kompatible Handler: `/v1/messages`.
//!
//! Wir übersetzen den Anthropic-Request in einen internen `NormRequest`,
//! lassen das Expertensystem entscheiden und pipen die Antwort (die wir als
//! OpenAI-Chat-Completion abholen) als Anthropic-Event-Stream zurück.

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

type ByteStream = std::pin::Pin<Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>>;
use crate::openai::{accumulate_completion, collect_stream, Accumulated};
use crate::routing::{announce_completion, announce_decision, announce_fallback, decide_for, headers_to_hints, parse_privacy_tag, FALLBACK_MAX_ATTEMPTS};
use crate::state::AppState;

pub async fn messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    let (profile_hdr, privacy_hdr) = headers_to_hints(&headers);
    let mut norm = anthropic_to_norm(&body)?;
    if norm.profile_hint.is_none() {
        norm.profile_hint = profile_hdr.clone();
    }
    if privacy_hdr.is_some() {
        norm.privacy_tag = parse_privacy_tag(privacy_hdr.as_deref());
    }
    norm.detect_required();

    let snap = state
        .registry
        .enriched_snapshot()
        .await
        .map_err(|e| ApiError::Upstream(e.to_string()))?;
    let (profile, decision) = decide_for(&norm, &state.config, &snap)?;
    announce_decision("anthropic", &profile, &decision, &norm);

    let started = Instant::now();

    let mut candidates: Vec<router_core::ModelCandidate> = std::iter::once(decision.winner.clone())
        .chain(decision.alternatives.into_iter())
        .take(FALLBACK_MAX_ATTEMPTS)
        .collect();
    let mut byte_stream: Option<ByteStream> = None;
    let mut winner = decision.winner.clone();
    let mut last_err: Option<ApiError> = None;
    while !candidates.is_empty() {
        let cand = candidates.remove(0);
        let openai_body = norm_to_openai(&norm, &cand.id, true);
        let attempt = match cand.backend {
            Backend::OpenRouter => match state.registry.openrouter() {
                Some(client) => client
                    .chat_completion_stream(&cand.id, &profile, openai_body)
                    .await
                    .map_err(|e| ApiError::Upstream(e.to_string()))
                    .map(|s| -> std::pin::Pin<Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>> {
                        Box::pin(s.map(|r| r.map_err(std::io::Error::other)))
                    }),
                None => Err(ApiError::Internal("OpenRouter backend not configured".into())),
            },
            Backend::OMlx => match state.registry.omlx() {
                Some(client) => client
                    .chat_completion_stream(&cand.id, openai_body)
                    .await
                    .map_err(|e| ApiError::Upstream(e.to_string()))
                    .map(|s| -> std::pin::Pin<Box<dyn Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>> {
                        Box::pin(s.map(|r| r.map_err(std::io::Error::other)))
                    }),
                None => Err(ApiError::Internal("oMLX backend not configured".into())),
            },
        };
        match attempt {
            Ok(s) => {
                winner = cand;
                byte_stream = Some(s);
                break;
            }
            Err(e) => {
                let next = candidates
                    .first()
                    .map(|c| c.id.clone())
                    .unwrap_or_else(|| "<none>".into());
                announce_fallback("anthropic", &cand.id, &next, &e.to_string());
                last_err = Some(e);
            }
        }
    }
    let byte_stream = byte_stream.ok_or_else(|| {
        last_err.unwrap_or_else(|| ApiError::Internal("no candidates available".into()))
    })?;

    // Non-Stream: Upstream-SSE absammeln und zu einem Anthropic-Messages-Body
    // aggregieren (Clients mit `stream: false` erwarten JSON, kein SSE).
    if !norm.stream {
        let bytes = collect_stream(byte_stream).await?;
        let elapsed = started.elapsed();
        let acc = accumulate_completion(&bytes);
        state.tracker.record(winner.backend, &winner.id, elapsed);
        announce_completion("anthropic", &winner.id, elapsed, acc.cost);
        state.history.record(crate::history::Transaction {
            unix_ts: now_unix(),
            api: "anthropic".into(),
            profile: profile.name.clone(),
            backend: format!("{:?}", winner.backend),
            model_id: winner.id.clone(),
            duration_ms: elapsed.as_millis() as u64,
            cost_usd: acc.cost,
        });
        let body = anthropic_body_from(&acc, &winner.id);
        return Ok(Json(body).into_response());
    }

    // Stream: OpenAI-SSE -> Anthropic-Events
    let tracker = state.tracker.clone();
    let history = state.history.clone();
    let profile_name = profile.name.clone();
    let model_for_event = winner.id.clone();
    let model_for_log = winner.id.clone();
    let backend = winner.backend;
    let event_stream = openai_sse_to_anthropic(byte_stream, model_for_event, move |elapsed, cost| {
        tracker.record(backend, &winner.id, elapsed);
        announce_completion("anthropic", &model_for_log, elapsed, cost);
        history.record(crate::history::Transaction {
            unix_ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            api: "anthropic".into(),
            profile: profile_name,
            backend: format!("{backend:?}"),
            model_id: model_for_log.clone(),
            duration_ms: elapsed.as_millis() as u64,
            cost_usd: cost,
        });
        let _ = started; // nur für Klarheit
    });

    Ok(Sse::new(event_stream).keep_alive(KeepAlive::default()).into_response())
}

pub fn anthropic_to_norm(body: &Value) -> Result<NormRequest, ApiError> {
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

    // system kann string oder array sein
    if let Some(sys) = body.get("system") {
        let text = match sys {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|x| x.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !text.is_empty() {
            req.messages.push(NormMessage {
                role: NormRole::System,
                text,
                images: vec![],
                tool_call_id: None,
                name: None,
            });
        }
    }

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let mut out = Vec::new();
        for t in tools {
            let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let desc = t.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());
            let params = t
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({}));
            out.push(router_core::ToolDef { name, description: desc, parameters: params });
        }
        if !out.is_empty() { req.tools = Some(out); }
    }

    let msgs = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ApiError::BadRequest("messages array required".into()))?;
    for m in msgs {
        let role = match m.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
            "assistant" => NormRole::Assistant,
            _ => NormRole::User,
        };
        let (text, images) = anthropic_content(m.get("content"));
        req.messages.push(NormMessage {
            role,
            text,
            images,
            tool_call_id: None,
            name: None,
        });
    }
    Ok(req)
}

fn anthropic_content(v: Option<&Value>) -> (String, Vec<router_core::ImagePart>) {
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
                    "image" => {
                        let src = p.get("source").cloned().unwrap_or_default();
                        let data = src
                            .get("data")
                            .and_then(|x| x.as_str())
                            .or_else(|| src.get("url").and_then(|x| x.as_str()))
                            .unwrap_or_default()
                            .to_string();
                        let mime = src
                            .get("media_type")
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string());
                        images.push(router_core::ImagePart { url_or_b64: data, mime });
                    }
                    _ => {}
                }
            }
            (text, images)
        }
        _ => (String::new(), vec![]),
    }
}

/// Baut aus dem internen `NormRequest` einen OpenAI-Chat-Completions-Body.
/// Das ist, was OpenRouter und oMLX beide konsumieren.
fn norm_to_openai(req: &NormRequest, model_id: &str, stream: bool) -> Value {
    let mut messages = Vec::new();
    for m in &req.messages {
        let role = match m.role {
            NormRole::System => "system",
            NormRole::User => "user",
            NormRole::Assistant => "assistant",
            NormRole::Tool => "tool",
        };
        if m.images.is_empty() {
            messages.push(json!({ "role": role, "content": m.text }));
        } else {
            let mut parts = vec![json!({ "type": "text", "text": m.text })];
            for img in &m.images {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": img.url_or_b64 },
                }));
            }
            messages.push(json!({ "role": role, "content": parts }));
        }
    }
    let mut body = json!({
        "model": model_id,
        "messages": messages,
        "stream": stream,
    });
    let obj = body.as_object_mut().unwrap();
    if let Some(t) = req.temperature { obj.insert("temperature".into(), json!(t)); }
    if let Some(t) = req.top_p { obj.insert("top_p".into(), json!(t)); }
    if let Some(m) = req.max_tokens { obj.insert("max_tokens".into(), json!(m)); }
    if let Some(tools) = &req.tools {
        let mut arr = Vec::new();
        for t in tools {
            arr.push(json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                }
            }));
        }
        obj.insert("tools".into(), json!(arr));
    }
    body
}

/// Pipet einen OpenAI-SSE-Byte-Stream in Anthropic-`/v1/messages`-Events.
/// Wir emittieren eine einfache Delta-Spur: `message_start`, `content_block_start`,
/// `content_block_delta` pro Text-Chunk, `content_block_stop`, `message_delta`,
/// `message_stop`.
fn openai_sse_to_anthropic<S, F>(
    inner: S,
    model_id: String,
    on_done: F,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> + Send
where
    S: Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static,
    F: FnOnce(std::time::Duration, Option<f64>) + Send + 'static,
{
    use futures::stream::StreamExt;

    let started = Instant::now();
    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
    let cost_cell = std::sync::Arc::new(std::sync::Mutex::new(Option::<f64>::None));
    let cost_cell_for_closure = cost_cell.clone();

    #[derive(Default)]
    struct State {
        buf: Vec<u8>,
        started: bool,
        block_started: bool,
    }
    let mut state = State::default();
    let msg_id_for_start = msg_id.clone();
    let model_for_start = model_id.clone();
    let started_event = async move {
        Ok(sse_event(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": msg_id_for_start,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": model_for_start,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                }
            }),
        ))
    };

    let mid_stream = inner.flat_map(move |chunk| {
        let mut events: Vec<Result<Event, std::convert::Infallible>> = Vec::new();
        match chunk {
            Ok(b) => {
                state.buf.extend_from_slice(&b);
                while let Some(pos) = find_event_boundary(&state.buf) {
                    let event_bytes: Vec<u8> = state.buf.drain(..pos).collect();
                    let sep_len = if state.buf.starts_with(b"\r\n\r\n") { 4 } else { 2 };
                    state.buf.drain(..sep_len.min(state.buf.len()));
                    let Some(data) = parse_sse_data(&event_bytes) else { continue };
                    if data.trim() == "[DONE]" { continue; }
                    let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
                    if let Some(c) = v.get("usage").and_then(|u| u.get("cost")).and_then(|x| x.as_f64()) {
                        *cost_cell_for_closure.lock().unwrap() = Some(c);
                    }
                    let delta_text = v["choices"]
                        .get(0)
                        .and_then(|c| c.get("delta"))
                        .and_then(|d| d.get("content"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    // finish_reason aus OpenAI ignorieren wir für Anthropic-Stream,
                    // weil `message_stop` bereits impliziert, dass der Stream endet.
                    if !state.started {
                        state.started = true;
                    }
                    if !delta_text.is_empty() {
                        if !state.block_started {
                            events.push(Ok(sse_event(
                                "content_block_start",
                                json!({
                                    "type": "content_block_start",
                                    "index": 0,
                                    "content_block": { "type": "text", "text": "" }
                                }),
                            )));
                            state.block_started = true;
                        }
                        events.push(Ok(sse_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": 0,
                                "delta": { "type": "text_delta", "text": delta_text }
                            }),
                        )));
                    }
                }
            }
            Err(e) => {
                events.push(Ok(sse_event(
                    "error",
                    json!({ "type": "error", "error": { "type": "api_error", "message": e.to_string() } }),
                )));
            }
        }
        futures::stream::iter(events)
    });

    // Abschluss-Events sammeln wir nach dem inneren Stream.
    let closing = futures::stream::once(async move {
        let cost = *cost_cell.lock().unwrap();
        on_done(started.elapsed(), cost);
        Ok(sse_event(
            "message_stop",
            json!({ "type": "message_stop" }),
        ))
    });

    futures::stream::once(started_event).chain(mid_stream).chain(closing)
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

fn sse_event(name: &str, payload: Value) -> Event {
    Event::default().event(name).data(payload.to_string())
}

/// Baut aus einem abgesammelten OpenAI-Completion-Stream einen
/// Anthropic-`/v1/messages`-Non-Stream-Body.
fn anthropic_body_from(acc: &Accumulated, model_id: &str) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !acc.content.is_empty() {
        content.push(json!({ "type": "text", "text": acc.content }));
    }
    for (i, t) in acc.tool_calls.iter().enumerate() {
        let input: Value = serde_json::from_str(&t.arguments).unwrap_or_else(|_| json!({}));
        content.push(json!({
            "type": "tool_use",
            "id": if t.id.is_empty() { format!("toolu_{i}") } else { t.id.clone() },
            "name": t.name,
            "input": input,
        }));
    }
    let stop_reason = if !acc.tool_calls.is_empty() {
        "tool_use"
    } else {
        match acc.finish_reason.as_deref() {
            Some("length") => "max_tokens",
            _ => "end_turn",
        }
    };
    json!({
        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
        "type": "message",
        "role": "assistant",
        "model": model_id,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {
            "input_tokens": acc.prompt_tokens.unwrap_or(0),
            "output_tokens": acc.completion_tokens.unwrap_or(0),
        },
    })
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_stream_default_is_false() {
        // Anthropic-API defaultet auf non-stream, wenn `stream` fehlt.
        let req = anthropic_to_norm(&json!({
            "model": "x",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .unwrap();
        assert!(!req.stream);
    }

    #[test]
    fn body_from_text_completion() {
        let raw = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n\
                   data: [DONE]\n\n";
        let acc = accumulate_completion(raw.as_bytes());
        let v = anthropic_body_from(&acc, "m/x");
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][0]["text"], "hi");
        assert_eq!(v["stop_reason"], "end_turn");
    }

    #[test]
    fn body_from_tool_call_yields_tool_use_block() {
        let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"q\\\":\\\"x\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
                   data: [DONE]\n\n";
        let acc = accumulate_completion(raw.as_bytes());
        let v = anthropic_body_from(&acc, "m/x");
        assert_eq!(v["content"][0]["type"], "tool_use");
        assert_eq!(v["content"][0]["name"], "lookup");
        assert_eq!(v["content"][0]["input"]["q"], "x");
        assert_eq!(v["stop_reason"], "tool_use");
    }
}
