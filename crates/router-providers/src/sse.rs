//! Low-Level-SSE-Parsing, geteilt zwischen den Egress-Clients (Anthropic-
//! Übersetzer) und dem API-Layer.

/// Position des nächsten Event-Trenners (`\n\n` oder `\r\n\r\n`) im Puffer.
pub fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .or_else(|| buf.windows(4).position(|w| w == b"\r\n\r\n"))
}

/// Fügt die `data:`-Zeilen eines SSE-Event-Blocks zu einem String zusammen.
pub fn parse_sse_data(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data: ").or_else(|| line.strip_prefix("data:")) {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
    }
    if data.is_empty() {
        None
    } else {
        Some(data)
    }
}
