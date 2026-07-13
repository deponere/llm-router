//! Vereinheitlichter Modell-Katalog.

use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModalitySet {
    bits: u8,
}

impl ModalitySet {
    const TEXT: u8 = 1 << 0;
    const IMAGE: u8 = 1 << 1;
    const AUDIO: u8 = 1 << 2;
    const VIDEO: u8 = 1 << 3;
    const FILE: u8 = 1 << 4;

    pub const fn text_only() -> Self { Self { bits: Self::TEXT } }
    pub fn with_image(mut self) -> Self { self.bits |= Self::IMAGE; self }
    pub fn with_audio(mut self) -> Self { self.bits |= Self::AUDIO; self }
    pub fn with_video(mut self) -> Self { self.bits |= Self::VIDEO; self }
    pub fn with_file(mut self) -> Self { self.bits |= Self::FILE; self }
    pub fn has_text(&self) -> bool { self.bits & Self::TEXT != 0 }
    pub fn has_image(&self) -> bool { self.bits & Self::IMAGE != 0 }
    pub fn has_audio(&self) -> bool { self.bits & Self::AUDIO != 0 }
    pub fn has_video(&self) -> bool { self.bits & Self::VIDEO != 0 }
    pub fn has_file(&self) -> bool { self.bits & Self::FILE != 0 }

    /// true, wenn `self` alle Modalitäten enthält, die `needed` enthält.
    pub fn covers(&self, needed: ModalitySet) -> bool {
        (self.bits & needed.bits) == needed.bits
    }

    pub fn from_strings<I, S>(iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut m = ModalitySet::default();
        for s in iter {
            match s.as_ref() {
                "text" => m.bits |= Self::TEXT,
                "image" => m.bits |= Self::IMAGE,
                "audio" => m.bits |= Self::AUDIO,
                "video" => m.bits |= Self::VIDEO,
                "file" => m.bits |= Self::FILE,
                _ => {}
            }
        }
        // Wenn nix gesetzt wurde, defaulten wir auf text.
        if m.bits == 0 { m.bits = Self::TEXT; }
        m
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CapsSet {
    bits: u8,
}

impl CapsSet {
    const TOOLS: u8 = 1 << 0;
    const JSON_MODE: u8 = 1 << 1;
    const STRUCTURED: u8 = 1 << 2;
    const REASONING: u8 = 1 << 3;

    pub fn with_tools(mut self) -> Self { self.bits |= Self::TOOLS; self }
    pub fn with_json_mode(mut self) -> Self { self.bits |= Self::JSON_MODE; self }
    pub fn with_structured_outputs(mut self) -> Self { self.bits |= Self::STRUCTURED; self }
    pub fn with_reasoning(mut self) -> Self { self.bits |= Self::REASONING; self }

    pub fn has_tools(&self) -> bool { self.bits & Self::TOOLS != 0 }
    pub fn has_json_mode(&self) -> bool { self.bits & Self::JSON_MODE != 0 }
    pub fn has_structured_outputs(&self) -> bool { self.bits & Self::STRUCTURED != 0 }
    pub fn has_reasoning(&self) -> bool { self.bits & Self::REASONING != 0 }

    pub fn covers(&self, needed: CapsSet) -> bool {
        (self.bits & needed.bits) == needed.bits
    }

    /// Ableitung aus OpenRouters `supported_parameters`-Array.
    pub fn from_supported_parameters<I, S>(iter: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut c = CapsSet::default();
        for s in iter {
            match s.as_ref() {
                "tools" | "tool_choice" | "parallel_tool_calls" => c.bits |= Self::TOOLS,
                "response_format" => c.bits |= Self::JSON_MODE,
                "structured_outputs" => c.bits |= Self::STRUCTURED,
                "reasoning" | "reasoning_effort" | "include_reasoning" => c.bits |= Self::REASONING,
                _ => {}
            }
        }
        c
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrivacyClass {
    Local,
    Zdr,
    Standard,
}

impl FromStr for PrivacyClass {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "local" => Ok(PrivacyClass::Local),
            "zdr" => Ok(PrivacyClass::Zdr),
            "standard" => Ok(PrivacyClass::Standard),
            _ => Err(format!("unknown privacy class: {s}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelCandidate {
    /// Name der Backend-Instanz aus der Config (z. B. "openai", "groq", "anthropic", "omlx", "openrouter"), Schlüssel für Dispatch + Metrics.
    pub backend_id: String,
    /// Deterministischer Tiebreak bei Score-Gleichstand (niedriger gewinnt); lokale Backends bekommen 0, um bevorzugt zu werden.
    pub tiebreak_priority: u8,
    /// Backend-spezifischer Modell-Identifier.
    pub id: String,
    /// Der Teil vor dem `/` in OpenRouter-IDs bzw. `omlx` für lokale Modelle.
    pub provider_slug: String,
    pub context_length: u32,
    pub max_completion_tokens: Option<u32>,
    /// USD pro 1 Million Input-Tokens.
    pub price_in_per_mtok: f64,
    /// USD pro 1 Million Output-Tokens.
    pub price_out_per_mtok: f64,
    pub input_modalities: ModalitySet,
    pub supports: CapsSet,
    pub is_moderated: bool,
    pub privacy_class: PrivacyClass,
    /// Gemessene p95-Latenz (ms) aus dem lokalen Metrics-Tracker, wenn vorhanden.
    pub measured_p95_ms: Option<u32>,
    /// Artificial-Analysis-Intelligence-Index (0..100), wenn verfügbar.
    pub intelligence_index: Option<f64>,
}

/// Snapshot der gemergten Registry, in der Lauf-Instanz via `router-providers::RegistryHandle` geliefert.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub models: Vec<ModelCandidate>,
}

impl Registry {
    pub fn iter(&self) -> impl Iterator<Item = &ModelCandidate> {
        self.models.iter()
    }

    /// Suche nach Backend-ID + exakte Modell-ID.
    pub fn find(&self, backend_id: &str, id: &str) -> Option<&ModelCandidate> {
        self.models.iter().find(|m| m.backend_id == backend_id && m.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_covers_works() {
        let supports = CapsSet::default().with_tools().with_structured_outputs();
        let needed = CapsSet::default().with_tools();
        assert!(supports.covers(needed));
        let needed2 = CapsSet::default().with_reasoning();
        assert!(!supports.covers(needed2));
    }

    #[test]
    fn modality_covers_works() {
        let supports = ModalitySet::text_only().with_image();
        let needed = ModalitySet::text_only().with_image();
        assert!(supports.covers(needed));
        let needed_audio = ModalitySet::text_only().with_audio();
        assert!(!supports.covers(needed_audio));
    }

}
