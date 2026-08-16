//! DLP-Guard: erkennt Secret-Muster (API-Tokens, PEM-Private-Keys inkl. SPIFFE-SVID-Keys)
//! in Prompt-Inhalten, damit der Router sie nicht an Cloud-Backends schickt.
//!
//! Bewusst dependency-frei (prefix-`contains` statt gitleaks/trufflehog-Entropy-Scan):
//! Prompts sind kurz, und bekannte Token-Prefixe + PEM-Marker decken die relevanten
//! Fälle ab. `eyJ` (JWT-Header) ist ein grober Heuristik-Match.

/// Liefert eine menschenlesbare Bezeichnung des ersten gefundenen Secrets, oder `None`.
pub fn find_secret(text: &str) -> Option<&'static str> {
    // PEM-Private-Keys: RSA/EC/DSA/OPENSSH/ENCRYPTED/… — auch der Private-Key-Anteil
    // eines SPIFFE-SVID (das Zertifikat selbst ist öffentlich, der Key nicht).
    if text.contains("PRIVATE KEY-----") {
        return Some("pem_private_key");
    }
    // ponytail: bekanntes-Prefix-Liste statt Entropy-Scan; hoch-entropische Tokens ohne
    // Prefix (z. B. veraltete `sk-`+48hex OpenAI-Keys) fallen durch — gitleaks-artiger
    // Scan, wenn das je ein echtes Problem wird.
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
    PREFIXES
        .iter()
        .find(|(p, _)| text.contains(p))
        .map(|(_, label)| *label)
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
    fn clean_text_is_none() {
        assert_eq!(find_secret("Bitte fasse diese Aufgabe zusammen."), None);
        assert_eq!(find_secret("Der Task dauert drei Stunden."), None); // "task" enthält kein "sk-"-Präfix
    }
}
