//! DLP-Guard: erkennt und REDAKTIERT Secrets in Prompt-Inhalten, bevor der Router sie an
//! Backends schickt. Vier Ebenen (Vault-Radar-inspiriert):
//! 1. PEM-Private-Keys (inkl. SPIFFE-SVID-Key), 2. bekannte Token-Prefixe,
//! 3. strukturierte Formate (Kreditkarten/Luhn, IBAN/mod-97), 4. Shannon-Entropy-Scan.
//! Erkannte Secrets werden durch `[REDACTED]` ersetzt — der Prompt läuft dadurch weiter,
//! statt blockiert zu werden. Es wird NUR das Label gezählt, nie der Secret-Wert geloggt.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// Ersetzt alle erkannten Secrets in `text` durch `[REDACTED]`; liefert (Text, Anzahl).
pub fn redact_text(text: &str) -> (String, usize) {
    let mut spans: Vec<(usize, usize)> = Vec::new();

    // 1. PEM-Private-Keys (ganzer Block inkl. Base64-Körper).
    if text.contains("PRIVATE KEY-----") {
        for m in PEM_RE.find_iter(text) {
            spans.push((m.start(), m.end()));
        }
    }
    // 2. Bekannte Token-Prefixe (ganzes Token, nicht nur der Prefix).
    for m in TOKEN_RE.find_iter(text) {
        spans.push((m.start(), m.end()));
    }
    // 3. Kreditkarten (Luhn) + IBAN (mod-97) — nur Checksum-validierte Treffer.
    for m in CC_RE.find_iter(text) {
        if is_luhn(m.as_str()) {
            spans.push((m.start(), m.end()));
        }
    }
    for m in IBAN_RE.find_iter(text) {
        if is_iban(m.as_str()) {
            spans.push((m.start(), m.end()));
        }
    }
    // 4. Entropy-Scan für unbekannte Formate.
    entropy_spans(text, &mut spans);

    if spans.is_empty() {
        return (text.to_string(), 0);
    }
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (s, e) in spans {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }

    let mut out = String::with_capacity(text.len() + merged.len() * 9);
    let mut prev = 0;
    for (s, e) in &merged {
        out.push_str(&text[prev..*s]);
        out.push_str("[REDACTED]");
        prev = *e;
    }
    out.push_str(&text[prev..]);
    (out, merged.len())
}

/// Redaktiert rekursiv alle String-Werte in einem JSON-Body; liefert die Anzahl der Ersetzungen.
pub fn redact_json(value: &mut Value) -> usize {
    match value {
        Value::String(s) => {
            let (out, n) = redact_text(s);
            if n > 0 {
                *s = out;
            }
            n
        }
        Value::Array(arr) => arr.iter_mut().map(redact_json).sum(),
        Value::Object(map) => map.values_mut().map(redact_json).sum(),
        _ => 0,
    }
}

/// Ganzes PEM-Private-Key (RSA/EC/DSA/OPENSSH/ENCRYPTED/…) inkl. Base64-Körper.
static PEM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.{0,8192}?-----END [A-Z ]*PRIVATE KEY-----")
        .expect("pem regex")
});

/// Volle Token-Muster für bekannte Anbieter-Prefixe (min. Längen, sonst Redaktion unvollständig).
static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
          sk-ant-api03-[A-Za-z0-9_-]{10,}
        | sk-ant-[A-Za-z0-9_-]{10,}
        | sk-proj-[A-Za-z0-9_-]{10,}
        | sk-svcacct-[A-Za-z0-9_-]{10,}
        | sk-admin-[A-Za-z0-9_-]{10,}
        | sk-or-[A-Za-z0-9_-]{10,}
        | sk_live_[A-Za-z0-9]{20,}
        | sk_test_[A-Za-z0-9]{20,}
        | github_pat_[A-Za-z0-9_]{22,}
        | ghp_[A-Za-z0-9]{20,}
        | gho_[A-Za-z0-9]{20,}
        | ghu_[A-Za-z0-9]{20,}
        | ghs_[A-Za-z0-9]{20,}
        | ghr_[A-Za-z0-9]{20,}
        | glpat-[A-Za-z0-9_-]{20,}
        | AKIA[0-9A-Z]{16,}
        | ASIA[0-9A-Z]{16,}
        | AIza[0-9A-Za-z_-]{30,}
        | xox[baprs]-[0-9A-Za-z-]{10,}
        | hf_[A-Za-z0-9]{20,}
        | gsk_[A-Za-z0-9]{20,}
        | eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}
        ",
    )
    .expect("token regex")
});

/// Kreditkarte: 13–19 Ziffern (mit optionalen Leer-/Bindestrichen), Luhn-validiert.
static CC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d[\d \-]{12,21}\d\b").expect("cc regex"));

fn is_luhn(s: &str) -> bool {
    let digits: Vec<u8> = s.bytes().filter(|b| b.is_ascii_digit()).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let mut sum = 0u32;
    let mut double = false;
    for &b in digits.iter().rev() {
        let mut d = (b - b'0') as u32;
        if double {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        double = !double;
    }
    sum % 10 == 0
}

/// IBAN: 2 Buchstaben + 2 Prüfziffern + 11–30 alphanumerisch, mod-97 == 1.
static IBAN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[A-Z]{2}\d{2}[A-Z0-9 ]{11,32}").expect("iban regex"));

fn is_iban(s: &str) -> bool {
    let iban: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&iban.len()) {
        return false;
    }
    // Erste 4 Zeichen ans Ende, Buchstaben → Ziffern (A=10…Z=35), mod 97 == 1.
    let mut n = 0u32;
    for c in iban.chars().skip(4).chain(iban.chars().take(4)) {
        n = match c {
            '0'..='9' => (n * 10 + (c as u8 - b'0') as u32) % 97,
            'A'..='Z' => (n * 100 + (c as u8 - b'A') as u32 + 10) % 97,
            _ => return false,
        };
    }
    n == 1
}

/// Shannon-Entropie in Bit/Zeichen.
fn shannon_entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let n = s.len() as f64;
    let mut h = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * p.log2();
        }
    }
    h
}

/// Flaggt unbekannte hoch-entropische Tokens (alnum/base64 ≥ 24 Zeichen & H ≥ 4.5,
/// oder Hex ≥ 40 Zeichen & H ≥ 3.5) als Spans.
fn entropy_spans(text: &str, out: &mut Vec<(usize, usize)>) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if !is_token_char(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_token_char(bytes[i]) {
            i += 1;
        }
        let run = &text[start..i];
        if run.len() < 24 {
            continue;
        }
        let h = shannon_entropy(run);
        if h >= 4.5 || (run.len() >= 40 && run.bytes().all(|b| b.is_ascii_hexdigit()) && h >= 3.5) {
            out.push((start, i));
        }
    }
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baut einen Fake-Secret zur Laufzeit zusammen, damit kein echter Secret-String
    /// literal im Quelltext steht (GitHub Push Protection blockt den Push sonst).
    fn fake(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }

    #[test]
    fn redacts_pem_private_key_block() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        let (out, n) = redact_text(pem);
        assert!(n >= 1);
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("PRIVATE KEY"));
    }

    #[test]
    fn redacts_token_prefixes() {
        let (out, n) = redact_text(&format!("key: {}", fake("sk-proj-", "abcdef1234567890ABCDEFxyz")));
        assert_eq!(n, 1);
        assert!(!out.contains("sk-proj-"));
        assert!(out.contains("[REDACTED]"));
        // Stripe + GitHub.
        let (_, n2) = redact_text(&fake("sk_live_", "51ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        assert_eq!(n2, 1);
        let (_, n3) = redact_text(&fake("github_pat_", "11AA22BB33CC44DD55EE66FF77"));
        assert_eq!(n3, 1);
    }

    #[test]
    fn redacts_credit_card_and_iban() {
        let (out, n) = redact_text("Karte 4111 1111 1111 1111 und IBAN DE89 3704 0044 0532 0130 00");
        assert_eq!(n, 2);
        assert!(!out.contains("4111") && !out.contains("DE89"));
        // Luhn-ungültige Karte + mod-97-ungültige IBAN bleiben stehen.
        let (out2, n2) = redact_text("Karte 4111 1111 1111 1112, IBAN DE88 3704 0044 0532 0130 00");
        assert_eq!(n2, 0);
        assert!(out2.contains("4111"));
    }

    #[test]
    fn redacts_high_entropy_tokens() {
        // 33 alphanumerische Zeichen, H ≥ 4.5.
        let (out, n) = redact_text("Qx4mN8vR2tY7wZ3pL6dF1hJ0kS5aB9cE");
        assert_eq!(n, 1);
        assert_eq!(out, "[REDACTED]");
        // SHA-256(leerer String): 64 Hex-Zeichen, H ≈ 3.99 → high_entropy_hex.
        let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let (out2, n2) = redact_text(sha);
        assert_eq!(n2, 1);
        assert_eq!(out2, "[REDACTED]");
    }

    #[test]
    fn leaves_clean_text_and_uuids_alone() {
        assert_eq!(redact_text("Bitte fasse diese Aufgabe zusammen.").1, 0);
        assert_eq!(redact_text("Der Task dauert drei Stunden.").1, 0);
        // UUIDs (32 hex, niedrige Entropie + Bindestriche) sollen NICHT flaggen.
        assert_eq!(redact_text("id: 123e4567-e89b-12d3-a456-426614174000").1, 0);
    }

    #[test]
    fn redact_json_walks_all_string_values() {
        let content = format!("Mein Key: {}, Karte 4111111111111111", fake("sk-proj-", "abcdef1234567890ABCDEF"));
        let mut v = serde_json::json!({
            "messages": [{ "role": "user", "content": content }],
            "model": "auto"
        });
        let n = redact_json(&mut v);
        assert_eq!(n, 2);
        let s = v.to_string();
        assert!(!s.contains("sk-proj-") && !s.contains("4111"));
        assert!(s.contains("[REDACTED]"));
    }
}
