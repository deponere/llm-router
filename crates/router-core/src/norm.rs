//! Interne, provider-agnostische Request-Darstellung.

use serde::{Deserialize, Serialize};

use crate::registry::{CapsSet, ModalitySet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePart {
    pub url_or_b64: String,
    pub mime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormMessage {
    pub role: NormRole,
    pub text: String,
    #[serde(default)]
    pub images: Vec<ImagePart>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Named { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { schema: serde_json::Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasoningHint {
    /// "low" | "medium" | "high" (OpenAI/Anthropic style)
    pub effort: Option<String>,
    /// OpenRouter: internal_reasoning budget als Tokens
    pub budget_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivacyTag {
    #[default]
    Normal,
    /// Cloud erlaubt, aber nur Zero-Data-Retention-Provider.
    Zdr,
    /// Nur lokal (oMLX).
    LocalOnly,
}

#[derive(Debug, Clone, Copy)]
pub enum ModalityKind {
    Text,
    Image,
    Audio,
    Video,
    File,
}

#[derive(Debug, Clone, Default)]
pub struct RequiredCaps {
    pub modalities: ModalitySet,
    pub caps: CapsSet,
}

/// Der interne, vereinheitlichte Request. Baut sich aus OpenAI- oder
/// Anthropic-Eingang zusammen. Alle abgeleiteten Felder (`required`,
/// `prompt_tokens_est`) werden vom Feature-Detector gesetzt.
#[derive(Debug, Clone, Default)]
pub struct NormRequest {
    pub messages: Vec<NormMessage>,
    pub tools: Option<Vec<ToolDef>>,
    pub tool_choice: Option<ToolChoice>,
    pub response_format: Option<ResponseFormat>,
    pub reasoning: Option<ReasoningHint>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stream: bool,
    pub stop: Vec<String>,

    // Header/Body-Hints für den Router:
    pub profile_hint: Option<String>,
    pub privacy_tag: PrivacyTag,
    /// Vom Client bevorzugtes Modell, z. B. "anthropic/claude-sonnet-4-6". `"auto"` oder leer
    /// heißt: komplett dem Expertensystem überlassen.
    pub model_hint: Option<String>,

    // Abgeleitet durch [`detect_required`]:
    pub required: RequiredCaps,
    pub prompt_tokens_est: u32,
}

impl NormRequest {
    /// Füllt `required` und `prompt_tokens_est` aus dem aktuellen Inhalt.
    pub fn detect_required(&mut self) {
        let mut modalities = ModalitySet::text_only();
        for m in &self.messages {
            if !m.images.is_empty() {
                modalities = modalities.with_image();
            }
        }

        let mut caps = CapsSet::default();
        if self.tools.as_ref().map(|v| !v.is_empty()).unwrap_or(false) {
            caps = caps.with_tools();
        }
        match &self.response_format {
            Some(ResponseFormat::JsonSchema { .. }) => {
                caps = caps.with_structured_outputs();
            }
            Some(ResponseFormat::JsonObject) => {
                caps = caps.with_json_mode();
            }
            _ => {}
        }
        if self.reasoning.is_some() {
            caps = caps.with_reasoning();
        }

        self.required = RequiredCaps { modalities, caps };
        self.prompt_tokens_est = estimate_tokens(&self.messages);
    }
}

/// Rohe Zeichenanzahl / 4 als stabile Schätzung.
/// Genau genug für Context-Filter (+Reserve), weil wir eh Reserve aufschlagen.
pub fn estimate_tokens(messages: &[NormMessage]) -> u32 {
    // Wir addieren eine kleine Pauschale pro Nachricht (Rolle + Separator-Tokens).
    let per_message_overhead: u32 = 4;
    let mut total: u32 = 0;
    for m in messages {
        total = total.saturating_add(per_message_overhead);
        let len_chars = m.text.chars().count() as u32;
        // Rule of thumb: ~4 Zeichen pro Token (Englisch), konservativ gerundet.
        let est = len_chars / 4;
        total = total.saturating_add(est);
        // Bilder schätzen wir grob mit 400 Tokens pro Part.
        total = total.saturating_add(400u32 * m.images.len() as u32);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> NormMessage {
        NormMessage {
            role: NormRole::User,
            text: text.to_string(),
            images: vec![],
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn vision_is_detected() {
        let mut req = NormRequest::default();
        req.messages.push(NormMessage {
            role: NormRole::User,
            text: "describe".into(),
            images: vec![ImagePart { url_or_b64: "data:...".into(), mime: None }],
            tool_call_id: None,
            name: None,
        });
        req.detect_required();
        assert!(req.required.modalities.has_image());
    }

    #[test]
    fn tools_is_detected() {
        let mut req = NormRequest::default();
        req.messages.push(user("use a tool"));
        req.tools = Some(vec![ToolDef {
            name: "t".into(),
            description: None,
            parameters: json!({}),
        }]);
        req.detect_required();
        assert!(req.required.caps.has_tools());
    }

    #[test]
    fn structured_outputs_detected() {
        let mut req = NormRequest::default();
        req.messages.push(user("json please"));
        req.response_format = Some(ResponseFormat::JsonSchema { schema: json!({}) });
        req.detect_required();
        assert!(req.required.caps.has_structured_outputs());
    }

    #[test]
    fn token_estimate_grows_with_input() {
        let a = estimate_tokens(&[user("hi")]);
        let b = estimate_tokens(&[user(&"x".repeat(1_000))]);
        assert!(b > a);
    }
}
