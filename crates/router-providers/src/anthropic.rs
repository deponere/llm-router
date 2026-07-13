//! Nativer Anthropic-Egress (`/v1/messages`): übersetzt OpenAI-Chat-Completions-Bodies nach Anthropic-Messages und den Anthropic-SSE-Stream zurück nach OpenAI-SSE, damit der Rest der Pipeline unverändert bleibt.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use router_config::{AuthConfig, BackendConfig, RegistryConfig};
use router_core::profile::ResolvedProfile;
use router_core::registry::{CapsSet, ModalitySet, ModelCandidate, PrivacyClass};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::provider::{resolve_secret, ByteStream, Provider, ProviderError};
use crate::sse::{find_event_boundary, parse_sse_data};

const DEFAULT_MAX_TOKENS: u32 = 4096;
const DEFAULT_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicClient {
    id: String,
    http: Client,
    base_url: String,
    auth: AuthConfig,
    version: String,
}

impl AnthropicClient {
    pub fn new(id: &str, cfg: &BackendConfig) -> Self {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(600))
            .build()
            .expect("reqwest client");
        Self {
            id: id.to_string(),
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            auth: cfg.auth.clone(),
            version: cfg.anthropic_version.clone().unwrap_or_else(|| DEFAULT_VERSION.into()),
        }
    }

    fn apply_headers(&self, req: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, ProviderError> {
        let req = req.header("anthropic-version", &self.version);
        // Anthropic nutzt `x-api-key`, nicht `Authorization: Bearer`.
        match resolve_secret(&self.auth)? {
            Some(k) => Ok(req.header("x-api-key", k)),
            None => Ok(req),
        }
    }
}

#[async_trait]
impl Provider for AnthropicClient {
    fn id(&self) -> &str { &self.id }
    fn is_local(&self) -> bool { false }

    async fn list_models(&self, _cfg: &RegistryConfig) -> Result<Vec<ModelCandidate>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let resp = self.apply_headers(self.http.get(url))?.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream { status, body });
        }
        let parsed: ModelsResponse = resp.json().await?;
        Ok(parsed.data.into_iter().map(|m| self.to_candidate(m.id)).collect())
    }

    async fn chat_completion_stream(
        &self,
        model_id: &str,
        _profile: &ResolvedProfile,
        request: Value,
    ) -> Result<ByteStream, ProviderError> {
        let body = openai_to_anthropic_body(&request, model_id, DEFAULT_MAX_TOKENS);
        let url = format!("{}/messages", self.base_url);
        let req = self
            .apply_headers(self.http.post(url).json(&body))?
            .header("Accept", "text/event-stream");
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body_txt = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream { status, body: body_txt });
        }
        let raw = resp.bytes_stream().map(|r| r.map_err(std::io::Error::other));
        Ok(anthropic_sse_to_openai(Box::pin(raw)))
    }
}

impl AnthropicClient {
    /// Anthropic `/models` liefert nur IDs; Claude-Modelle können durchweg Tools, Vision und Reasoning — Preise/Feinheiten trägt ein Override nach.
    fn to_candidate(&self, id: String) -> ModelCandidate {
        ModelCandidate {
            backend_id: self.id.clone(),
            tiebreak_priority: 1,
            provider_slug: "anthropic".into(),
            context_length: 200_000,
            max_completion_tokens: None,
            price_in_per_mtok: 0.0,
            price_out_per_mtok: 0.0,
            input_modalities: ModalitySet::text_only().with_image(),
            supports: CapsSet::default().with_tools().with_reasoning(),
            is_moderated: false,
            privacy_class: PrivacyClass::Standard,
            measured_p95_ms: None,
            intelligence_index: None,
            id,
        }
    }
}

// --- Request-Übersetzung: OpenAI Chat Completions -> Anthropic Messages ---

/// Baut aus einem OpenAI-Chat-Completions-Body einen Anthropic-`/v1/messages`-Body: hebt `system`-Rollen hoch, mappt tool_calls/tool zu tool_use/tool_result, setzt `max_tokens` falls fehlend.
fn openai_to_anthropic_body(body: &Value, model_id: &str, default_max_tokens: u32) -> Value {
    let mut system = String::new();
    // (role, content_blocks) mit Merge aufeinanderfolgender gleicher Rollen — Anthropic erlaubt keine zwei consecutive messages derselben Rolle.
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();
    let mut push_blocks = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        match turns.last_mut() {
            Some((r, existing)) if r == role => existing.extend(blocks),
            _ => turns.push((role.to_string(), blocks)),
        }
    };

    if let Some(msgs) = body.get("messages").and_then(|v| v.as_array()) {
        for m in msgs {
            let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            match role {
                "system" => {
                    if let Some(t) = content_to_text(m.get("content")) {
                        if !system.is_empty() {
                            system.push('\n');
                        }
                        system.push_str(&t);
                    }
                }
                "tool" => {
                    // OpenAI tool-Result -> Anthropic user/tool_result-Block.
                    let tool_use_id = m.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or("");
                    let content = content_to_text(m.get("content")).unwrap_or_default();
                    push_blocks("user", vec![json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                    })]);
                }
                "assistant" => {
                    let mut blocks = Vec::new();
                    if let Some(t) = content_to_text(m.get("content")) {
                        if !t.is_empty() {
                            blocks.push(json!({ "type": "text", "text": t }));
                        }
                    }
                    if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            let f = tc.get("function");
                            let name = f.and_then(|f| f.get("name")).and_then(|v| v.as_str()).unwrap_or("");
                            let args = f
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            let input: Value = serde_json::from_str(args).unwrap_or_else(|_| json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                                "name": name,
                                "input": input,
                            }));
                        }
                    }
                    push_blocks("assistant", blocks);
                }
                _ => {
                    // user (oder unbekannt): Text + Bilder als content-Blöcke.
                    push_blocks("user", content_to_blocks(m.get("content")));
                }
            }
        }
    }

    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();

    let max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(default_max_tokens as u64);

    let mut out = json!({
        "model": model_id,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });
    let obj = out.as_object_mut().unwrap();
    if !system.is_empty() {
        obj.insert("system".into(), json!(system));
    }
    if let Some(t) = body.get("temperature").and_then(|v| v.as_f64()) {
        obj.insert("temperature".into(), json!(t));
    }
    if let Some(t) = body.get("top_p").and_then(|v| v.as_f64()) {
        obj.insert("top_p".into(), json!(t));
    }
    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let f = t.get("function").unwrap_or(t);
                let name = f.get("name").and_then(|v| v.as_str())?;
                Some(json!({
                    "name": name,
                    "description": f.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    "input_schema": f.get("parameters").cloned().unwrap_or_else(|| json!({ "type": "object" })),
                }))
            })
            .collect();
        if !mapped.is_empty() {
            obj.insert("tools".into(), json!(mapped));
        }
    }
    out
}

/// Reiner Text aus einem OpenAI-`content` (String oder Parts-Array).
fn content_to_text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let mut out = String::new();
            for p in parts {
                if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// OpenAI-`content` -> Anthropic content-Blöcke (Text + Bilder).
fn content_to_blocks(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::String(s)) => vec![json!({ "type": "text", "text": s })],
        Some(Value::Array(parts)) => {
            let mut blocks = Vec::new();
            for p in parts {
                match p.get("type").and_then(|x| x.as_str()) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(|x| x.as_str()) {
                            blocks.push(json!({ "type": "text", "text": t }));
                        }
                    }
                    Some("image_url") => {
                        if let Some(url) = p.get("image_url").and_then(|o| o.get("url")).and_then(|s| s.as_str()) {
                            blocks.push(image_block(url));
                        }
                    }
                    _ => {}
                }
            }
            blocks
        }
        _ => Vec::new(),
    }
}

/// Baut einen Anthropic-Image-Block aus einer OpenAI-`image_url` (data-URI oder http-URL).
fn image_block(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        // data:<mime>;base64,<data>
        if let Some((meta, data)) = rest.split_once(',') {
            let media_type = meta.split(';').next().unwrap_or("image/png");
            return json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            });
        }
    }
    json!({ "type": "image", "source": { "type": "url", "url": url } })
}

// --- Response-Übersetzung: Anthropic-SSE -> OpenAI-Chat-Completions-SSE ---

#[derive(Default)]
struct TransState {
    buf: Vec<u8>,
    /// Anthropic-content-block-Index -> OpenAI-tool_call-Index (nur tool_use).
    tool_slots: HashMap<u64, usize>,
    next_tool_slot: usize,
    input_tokens: u64,
}

/// Übersetzt einen Anthropic-`/v1/messages`-SSE-Stream in einen OpenAI-Chat-Completions-SSE-Stream, abgeschlossen mit `data: [DONE]`.
fn anthropic_sse_to_openai(inner: ByteStream) -> ByteStream {
    let mut st = TransState::default();
    let s = inner.flat_map(move |chunk| {
        let mut out: Vec<Result<bytes::Bytes, std::io::Error>> = Vec::new();
        match chunk {
            Ok(b) => {
                st.buf.extend_from_slice(&b);
                while let Some(pos) = find_event_boundary(&st.buf) {
                    let event_bytes: Vec<u8> = st.buf.drain(..pos).collect();
                    let sep = if st.buf.starts_with(b"\r\n\r\n") { 4 } else { 2 };
                    st.buf.drain(..sep.min(st.buf.len()));
                    let Some(data) = parse_sse_data(&event_bytes) else { continue };
                    let Ok(ev) = serde_json::from_str::<Value>(&data) else { continue };
                    for chunk_json in translate_event(&ev, &mut st) {
                        out.push(Ok(sse_bytes(&chunk_json)));
                    }
                }
            }
            Err(e) => {
                out.push(Ok(sse_bytes(&json!({ "error": { "message": e.to_string() } }))));
            }
        }
        futures::stream::iter(out)
    });
    // `data: [DONE]` als Abschluss, wie es OpenAI-Clients erwarten.
    let done = futures::stream::once(async {
        Ok(bytes::Bytes::from_static(b"data: [DONE]\n\n"))
    });
    Box::pin(s.chain(done)) as ByteStream
}

fn sse_bytes(v: &Value) -> bytes::Bytes {
    bytes::Bytes::from(format!("data: {}\n\n", v))
}

/// Übersetzt ein einzelnes Anthropic-Event in 0..n OpenAI-Chat-Completions-Chunks (als JSON-Werte).
fn translate_event(ev: &Value, st: &mut TransState) -> Vec<Value> {
    match ev.get("type").and_then(|v| v.as_str()) {
        Some("message_start") => {
            st.input_tokens = ev
                .get("message")
                .and_then(|m| m.get("usage"))
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            vec![]
        }
        Some("content_block_start") => {
            let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            let cb = ev.get("content_block");
            match cb.and_then(|c| c.get("type")).and_then(|v| v.as_str()) {
                Some("tool_use") => {
                    let slot = st.next_tool_slot;
                    st.next_tool_slot += 1;
                    st.tool_slots.insert(idx, slot);
                    let id = cb.and_then(|c| c.get("id")).and_then(|v| v.as_str()).unwrap_or("");
                    let name = cb.and_then(|c| c.get("name")).and_then(|v| v.as_str()).unwrap_or("");
                    vec![delta_chunk(json!({
                        "tool_calls": [{
                            "index": slot,
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": "" }
                        }]
                    }))]
                }
                _ => vec![],
            }
        }
        Some("content_block_delta") => {
            let delta = ev.get("delta");
            match delta.and_then(|d| d.get("type")).and_then(|v| v.as_str()) {
                Some("text_delta") => {
                    let text = delta.and_then(|d| d.get("text")).and_then(|v| v.as_str()).unwrap_or("");
                    vec![delta_chunk(json!({ "content": text }))]
                }
                Some("input_json_delta") => {
                    let idx = ev.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                    let slot = *st.tool_slots.get(&idx).unwrap_or(&0);
                    let partial = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    vec![delta_chunk(json!({
                        "tool_calls": [{
                            "index": slot,
                            "function": { "arguments": partial }
                        }]
                    }))]
                }
                _ => vec![],
            }
        }
        Some("message_delta") => {
            let stop_reason = ev
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|v| v.as_str());
            let output_tokens = ev
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            vec![json!({
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": map_finish_reason(stop_reason),
                }],
                "usage": {
                    "prompt_tokens": st.input_tokens,
                    "completion_tokens": output_tokens,
                    "total_tokens": st.input_tokens + output_tokens,
                }
            })]
        }
        // message_stop / ping / content_block_stop: kein eigener OpenAI-Chunk.
        _ => vec![],
    }
}

/// Baut einen OpenAI-Streaming-Chunk mit gegebenem `delta`-Objekt.
fn delta_chunk(delta: Value) -> Value {
    json!({ "choices": [{ "index": 0, "delta": delta }] })
}

/// Anthropic-`stop_reason` -> OpenAI-`finish_reason`.
fn map_finish_reason(anthropic: Option<&str>) -> &'static str {
    match anthropic {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        _ => "stop",
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ApiModel>,
}

#[derive(Debug, Deserialize)]
struct ApiModel {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lifts_system_and_sets_max_tokens() {
        let body = json!({
            "model": "ignored",
            "messages": [
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": "hi" }
            ]
        });
        let out = openai_to_anthropic_body(&body, "claude-x", 1234);
        assert_eq!(out["system"], "be brief");
        assert_eq!(out["max_tokens"], 1234);
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(out["stream"], true);
    }

    #[test]
    fn request_maps_tools_and_tool_roundtrip() {
        let body = json!({
            "messages": [
                { "role": "user", "content": "weather?" },
                { "role": "assistant", "content": null, "tool_calls": [
                    { "id": "call_1", "type": "function",
                      "function": { "name": "get_weather", "arguments": "{\"city\":\"Berlin\"}" } }
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "sunny" }
            ],
            "tools": [
                { "type": "function", "function": {
                    "name": "get_weather", "description": "w",
                    "parameters": { "type": "object", "properties": {} } } }
            ]
        });
        let out = openai_to_anthropic_body(&body, "claude-x", 100);
        assert_eq!(out["tools"][0]["name"], "get_weather");
        assert_eq!(out["tools"][0]["input_schema"]["type"], "object");
        // assistant tool_use
        assert_eq!(out["messages"][1]["role"], "assistant");
        assert_eq!(out["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(out["messages"][1]["content"][0]["input"]["city"], "Berlin");
        // tool result -> user/tool_result
        assert_eq!(out["messages"][2]["role"], "user");
        assert_eq!(out["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(out["messages"][2]["content"][0]["tool_use_id"], "call_1");
    }

    fn translate_all(events: &[Value]) -> Vec<Value> {
        let mut st = TransState::default();
        events.iter().flat_map(|e| translate_event(e, &mut st)).collect()
    }

    #[test]
    fn sse_text_and_finish() {
        let events = vec![
            json!({ "type": "message_start", "message": { "usage": { "input_tokens": 5 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hi" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 2 } }),
            json!({ "type": "message_stop" }),
        ];
        let chunks = translate_all(&events);
        // text chunk
        assert_eq!(chunks[0]["choices"][0]["delta"]["content"], "Hi");
        // finish + usage chunk
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
        assert_eq!(last["usage"]["prompt_tokens"], 5);
        assert_eq!(last["usage"]["completion_tokens"], 2);
    }

    #[test]
    fn sse_tool_use_maps_to_openai_tool_calls() {
        let events = vec![
            json!({ "type": "content_block_start", "index": 0, "content_block":
                { "type": "tool_use", "id": "toolu_1", "name": "lookup" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta":
                { "type": "input_json_delta", "partial_json": "{\"q\":" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta":
                { "type": "input_json_delta", "partial_json": "\"x\"}" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 3 } }),
        ];
        let chunks = translate_all(&events);
        assert_eq!(chunks[0]["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(chunks[0]["choices"][0]["delta"]["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(chunks[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "lookup");
        assert_eq!(chunks[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "{\"q\":");
        assert_eq!(chunks[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"], "\"x\"}");
        assert_eq!(chunks.last().unwrap()["choices"][0]["finish_reason"], "tool_calls");
    }
}
