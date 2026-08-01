//! Eingebettetes Web-Interface (LightLLM-artig) — Single-File-SPA, die die
//! bestehenden JSON-Endpoints spricht. Keine neuen Dependencies, kein Build-Step.

pub async fn index() -> impl axum::response::IntoResponse {
    axum::response::Html(include_str!("ui/index.html"))
}
