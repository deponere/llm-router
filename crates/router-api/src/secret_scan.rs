//! DLP-Guard: erkennt Secrets in Prompt-Inhalten, damit der Router sie nicht an
//! Cloud-Backends schickt. Vier Ebenen (Vault-Radar-inspiriert):
//!
//! 1. PEM-Private-Keys (inkl. SPIFFE-SVID-Key) — Marker `PRIVATE KEY-----`.
//! 2. Bekannte Token-Prefixe (sk-, ghp_, AKIA, AIza, …) — hohe Präzision.
//! 3. Strukturierte Formate mit Prüfsumme — Kreditkarten (Luhn), IBAN (mod-97).
//! 4. Shannon-Entropy-Scan — unbekannte hoch-entropische Tokens (base64/base62/hex),
//!    wie Vault Radar sie als „unidentified high-entropy"-Kandidaten flaggt.

use std::sync::LazyLock;

use regex::Regex;

/// Liefert eine menschenlesbare Bezeichnung des ersten gefundenen Secrets, oder `None`.
/// Reihenfolge: billigste Checks zuerst; der zurückgegebene Wert ist NUR ein Label,
/// niemals der Secret-Wert selbst (darf nicht geloggt werden).
pub fn find_secret(text: &str) -> Option<&'static str> {
    // 1. PEM-Private-Keys: RSA/EC/DSA/OPENSSH/ENCRYPTED/… — auch der Private-Key-Anteil
    //    eines SPIFFE-SVID (das Zertifikat ist öffentlich, der Key nicht).
    if text.contains("PRIVATE KEY-----") {
        return Some("pem_private_key");
    }

    // 2. Bekannte Token-Prefixe.
    const PREFIXES: &[(&str, &str)] = &[
        ("sk-ant-api03-", "anthropic_api_key"),
        ("sk-ant-", "anthropic_api_key"),
        ("sk-proj-", "openai_api_key"),
        ("sk-svcacct-", "openai_api_key"),
        ("sk-admin-", "openai_api_key"),
        ("sk-or-", "openrouter_api_key"),
        ("sk_live_", "stripe_secret_key"),
        ("sk_test_", "stripe_secret_key"),
        ("github_pat_", "github_pat"),
        ("ghp_", "github_pat"),
        ("gho_", "github_token"),
        ("ghu_", "github_token"),
        ("ghs_", "github_token"),
        ("ghr_", "github_token"),
        ("glpat-", "gitlab_pat"),
        ("AKIA", "aws_access_key_id"),
        ("ASIA", "aws_session_token"),
        ("AIza", "google_api_key"),
        ("xoxb-", "slack_bot_token"),
        ("xoxp-", "slack_user_token"),
        ("xoxr-", "slack_refresh_token"),
        ("xoxa-", "slack_app_token"),
        ("hf_", "huggingface_token"),
        ("gsk_", "groq_api_key"),
        ("eyJ", "jwt"),
    ];
    if let Some((_, label)) = PREFIXES.iter().find(|(p, _)| text.contains(p)) {
        return Some(label);
    }

    // 3. Strukturierte Formate mit Prüfsumme (Regex-Kandidat → Checksum-Validierung).
    if CC_RE.find_iter(text).any(|m| is_luhn(&m.as_str())) {
        return Some("credit_card");
    }
    if IBAN_RE.find_iter(text).any(|m| is_iban(&m.as_str())) {
        return Some("iban");
    }

    // 4. Entropy-Scan für unbekannte Token-Formate.
    find_high_entropy(text)
}

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

/// Shannon-Entropie in Bit/Zeichen (0.0 bei lauter gleichen Zeichen, max log2(Alphabet)).
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

/// Flaggt unbekannte hoch-entropische Tokens (base64/base62/alphanumerisch ≥ 24 Zeichen
/// mit H ≥ 4.5, oder lange Hex-Runs ≥ 40 Zeichen mit H ≥ 3.5 — private Keys/Fingerprints).
fn find_high_entropy(text: &str) -> Option<&'static str> {
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
        if h >= 4.5 {
            return Some("high_entropy_token");
        }
        if run.len() >= 40 && run.bytes().all(|b| b.is_ascii_hexdigit()) && h >= 3.5 {
            return Some("high_entropy_hex");
        }
    }
    None
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_pem_private_keys() {
        assert_eq!(find_secret("-----BEGIN RSA PRIVATE KEY-----"), Some("pem_private_key"));
        assert_eq!(find_secret("-----BEGIN OPENSSH PRIVATE KEY-----"), Some("pem_private_key"));
        assert_eq!(find_secret("-----BEGIN EC PRIVATE KEY-----"), Some("pem_private_key"));
    }

    #[test]
    fn finds_known_token_prefixes() {
        assert_eq!(find_secret("key is sk-proj-abc123"), Some("openai_api_key"));
        assert_eq!(find_secret("key is sk-ant-api03-xyz"), Some("anthropic_api_key"));
        assert_eq!(find_secret("github_pat_11AA"), Some("github_pat"));
        assert_eq!(find_secret("AKIAIOSFODNN7EXAMPLE"), Some("aws_access_key_id"));
        assert_eq!(find_secret("xoxb-123"), Some("slack_bot_token"));
        assert_eq!(find_secret("eyJhbGciOiJIUzI1NiJ9"), Some("jwt"));
    }

    #[test]
    fn finds_valid_credit_cards_via_luhn() {
        // Visa-Testnummer, Luhn-gültig.
        assert_eq!(find_secret("Karte 4111 1111 1111 1111 danke"), Some("credit_card"));
        // Luhn-ungültig → kein Treffer.
        assert_eq!(find_secret("Karte 4111 1111 1111 1112 danke"), None);
    }

    #[test]
    fn finds_valid_iban_via_mod97() {
        assert_eq!(find_secret("IBAN DE89 3704 0044 0532 0130 00"), Some("iban"));
        // Prüfziffer verdreht → kein Treffer.
        assert_eq!(find_secret("IBAN DE88 3704 0044 0532 0130 00"), None);
    }

    #[test]
    fn shannon_entropy_bounds() {
        assert_eq!(shannon_entropy("aaaaaaaa"), 0.0);
        // 16 verschiedene Zeichen → exakt 4.0 bit/Zeichen.
        assert!((shannon_entropy("0123456789abcdef") - 4.0).abs() < 1e-9);
    }

    #[test]
    fn finds_high_entropy_hex() {
        // SHA-256(leerer String): 64 Hex-Zeichen, H ≈ 3.99 → high_entropy_hex.
        let sha = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(find_secret(sha), Some("high_entropy_hex"));
    }

    #[test]
    fn finds_high_entropy_alnum_token() {
        // 33 Zeichen, gemischte Groß-/Kleinschreibung + Ziffern → H ≥ 4.5.
        let token = "Qx4mN8vR2tY7wZ3pL6dF1hJ0kS5aB9cE";
        assert!(shannon_entropy(token) >= 4.5);
        assert_eq!(find_secret(token), Some("high_entropy_token"));
    }

    #[test]
    fn clean_text_is_none() {
        assert_eq!(find_secret("Bitte fasse diese Aufgabe zusammen."), None);
        assert_eq!(find_secret("Der Task dauert drei Stunden."), None);
        // UUIDs (32 hex, niedrige Entropie + Bindestriche) sollen NICHT flaggen.
        assert_eq!(find_secret("id: 123e4567-e89b-12d3-a456-426614174000"), None);
    }
}
