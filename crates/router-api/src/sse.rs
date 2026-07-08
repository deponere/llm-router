//! SSE-Bausteine für die OpenAI- und Anthropic-Handler. Alles kommt aus dem
//! Provider-Layer und wird hier gebündelt re-exportiert, damit die Handler nur
//! `crate::sse` importieren müssen.

pub use router_providers::sse::{find_event_boundary, parse_sse_data};
pub use router_providers::ByteStream;
