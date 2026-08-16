//! Router-eigene API-Keys (Feature #1): SHA-256-Hash-Verifikation gegen `[server.auth]`,
//! Key-Extraktion aus Headern für die Kosten-Attribution. Die eigentliche Middleware
//! (Budget-Enforcement) liegt in `middleware()` weiter unten; `lookup_key` wird auch von
//! den Handlern für die Store-Attribution genutzt.

use axum::http::HeaderMap;
use axum::response::IntoResponse;
use router_config::Config;

use crate::error::ApiError;

/// SHA-256 als Hex-String.
pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Erzeugt einen neuen Plaintext-Key (`rk_` + 24 Zeichen Base62) samt Hash.
pub fn generate_key() -> (String, String) {
    use rand_like::alphanumeric;
    let plain = format!("rk_{}", alphanumeric(24));
    let hash = format!("sha256:{}", sha256_hex(&plain));
    (plain, hash)
}

/// Liest den präsentierten Key aus `x-api-key` oder `Authorization: Bearer …`.
pub fn key_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-api-key") {
        if let Ok(s) = v.to_str() {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = v.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                let rest = rest.trim();
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
        }
    }
    None
}

/// Ordnet einen präsentierten Key einem konfigurierten Key zu (Hash-Vergleich).
/// `None`, wenn auth aus ist oder kein Key passt.
pub fn find_key<'a>(headers: &HeaderMap, cfg: &'a Config) -> Option<&'a router_config::AuthKey> {
    if !cfg.auth.enabled || cfg.auth.keys.is_empty() {
        return None;
    }
    let presented = key_from_headers(headers)?;
    let hash = sha256_hex(&presented);
    cfg.auth.keys.iter().find(|k| k.hash == format!("sha256:{hash}"))
}

/// Name des präsentierten Keys (für Kosten-Attribution), `None` wenn keiner passt.
pub fn lookup_key(headers: &HeaderMap, cfg: &Config) -> Option<String> {
    find_key(headers, cfg).map(|k| k.name.clone())
}

/// Server-erzwungenes Profil des präsentierten Keys (Key→Profil-Pinning).
/// `Ok(None)` = kein Pin (Auth aus oder Key ohne `profile`), Client wählt frei.
/// `Ok(Some(pin))` = Handler MÜSSEN `profile_hint` auf dieses Profil setzen und
/// `x-route-profile` / `route_profile` / `<profile>/auto` ignorieren.
/// `Err` = Pin gesetzt, aber Profil fehlt in der Config — fail-closed statt stillem
/// Fallback auf `default`, sonst liefe der Agent doch über Cloud-Backends.
pub fn forced_profile(headers: &HeaderMap, cfg: &Config) -> Result<Option<String>, ApiError> {
    let Some(pin) = find_key(headers, cfg).and_then(|k| k.profile.clone()) else {
        return Ok(None);
    };
    if cfg.profiles.contains_key(&pin) {
        Ok(Some(pin))
    } else {
        Err(ApiError::Internal(format!(
            "api key is pinned to profile '{pin}', but that profile is not configured"
        )))
    }
}

/// Key-Name im Request-Kontext (gesetzt von der Middleware nach erfolgreichem Check).
#[derive(Debug, Clone)]
pub struct AuthKeyName(pub String);

/// Axum-Middleware: prüft `x-api-key`/`Bearer` gegen `[server.auth]`, erzwingt
/// Tages-/Monatsbudgets und legt den Key-Namen für die Attribution in die Extensions.
pub async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::StatusCode;
    let cfg = &state.config;
    if !cfg.auth.enabled {
        return next.run(req).await;
    }
    // Web-UI (Browser, gleiche Origin) bleibt Admin-Surface ohne Key.
    if cfg.auth.allow_ui && is_ui_request(&req, &cfg.server.bind) {
        return next.run(req).await;
    }
    match find_key(req.headers(), cfg) {
        Some(k) => {
            let day = k.daily_budget_usd.filter(|d| *d > 0.0);
            let month = k.monthly_budget_usd.filter(|m| *m > 0.0);
            if let Some(d) = day {
                if state.store.spend_today_utc(&k.name) >= d {
                    return budget_exceeded("daily", d);
                }
            }
            if let Some(m) = month {
                if state.store.spend_this_month_utc(&k.name) >= m {
                    return budget_exceeded("monthly", m);
                }
            }
            req.extensions_mut().insert(AuthKeyName(k.name.clone()));
            tracing::debug!(key = %k.name, "api key authorized");
            next.run(req).await
        }
        None => {
            let body = serde_json::json!({
                "error": {
                    "message": "invalid or missing API key — send `x-api-key` or `Authorization: Bearer <key>`",
                    "type": "authentication_error",
                }
            });
            (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
        }
    }
}

fn budget_exceeded(period: &str, budget: f64) -> axum::response::Response {
    tracing::warn!(%period, budget, "api key budget exhausted");
    let body = serde_json::json!({
        "error": {
            "message": format!("API key {period} budget of ${budget:.2} exhausted"),
            "type": "budget_exceeded",
        }
    });
    (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        axum::Json(body),
    )
        .into_response()
}

/// Erkennt Browser-Requests aus dem Web-UI (gleiche Origin bzw. `Sec-Fetch-Site: same-origin`).
fn is_ui_request(req: &axum::http::Request<axum::body::Body>, bind: &str) -> bool {
    if let Some(v) = req.headers().get("sec-fetch-site") {
        if let Ok(s) = v.to_str() {
            if s.eq_ignore_ascii_case("same-origin") || s.eq_ignore_ascii_case("none") {
                return true;
            }
        }
    }
    let port = bind.rsplit(':').next().unwrap_or("4123");
    if let Some(v) = req.headers().get("origin") {
        if let Ok(s) = v.to_str() {
            let host = s
                .trim_start_matches("http://")
                .trim_start_matches("https://")
                .split('/')
                .next()
                .unwrap_or("");
            let ok = host == "localhost" || host == "127.0.0.1" || host == format!("localhost:{port}")
                || host == format!("127.0.0.1:{port}");
            if ok {
                return true;
            }
        }
    }
    false
}

/// Schlüssel des UTC-Tagesbeginns (für den Alert-Throttle „einmal pro Tag").
pub fn utc_day_key() -> u64 {
    crate::history::unix_utc_day_start()
}

/// Winziger Base62-Zufallsgenerator ohne zusätzliche Dependency (kein krypto-RNG nötig
/// für API-Keys dieser Klasse — sha2-Hashing des Ergebnisses reicht für den Config-Abgleich).
mod rand_like {
    use std::time::{SystemTime, UNIX_EPOCH};

    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    /// Deterministisch anmutender, aber ausreichend zufälliger String (Xorshift + Zeit-Seed).
    pub fn alphanumeric(len: usize) -> String {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9)
            ^ std::process::id() as u64;
        let mut state = seed.max(1);
        let mut out = String::with_capacity(len);
        for _ in 0..len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push(CHARS[(state % 62) as usize] as char);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_roundtrip() {
        let (plain, hash) = generate_key();
        assert!(plain.starts_with("rk_"));
        assert_eq!(hash, format!("sha256:{}", sha256_hex(&plain)));
        assert_eq!(plain.len(), 27);
    }

    #[test]
    fn key_from_both_headers() {
        let mut h = HeaderMap::new();
        assert_eq!(key_from_headers(&h), None);
        h.insert("x-api-key", "rk_test".parse().unwrap());
        assert_eq!(key_from_headers(&h).as_deref(), Some("rk_test"));
        h.remove("x-api-key");
        h.insert("authorization", "Bearer rk_bearer".parse().unwrap());
        assert_eq!(key_from_headers(&h).as_deref(), Some("rk_bearer"));
    }

    #[test]
    fn lookup_matches_configured_key() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            bind = "127.0.0.1:4123"
            [backends.openrouter]
            kind = "openrouter"
            base_url = "https://x"
            auth = { type = "api_key", env = "X" }
            [auth]
            enabled = true
            [[auth.keys]]
            name = "pi"
            hash = "PLACEHOLDER"
            "#,
        )
        .unwrap();
        let (plain, hash) = generate_key();
        let mut cfg = cfg;
        cfg.auth.keys[0].hash = hash;
        let mut h = HeaderMap::new();
        h.insert("x-api-key", plain.parse().unwrap());
        assert_eq!(lookup_key(&h, &cfg).as_deref(), Some("pi"));
        h.insert("x-api-key", "wrong".parse().unwrap());
        assert_eq!(lookup_key(&h, &cfg), None);
    }

    #[test]
    fn forced_profile_pins_and_fails_closed() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            bind = "127.0.0.1:4123"
            [backends.openrouter]
            kind = "openrouter"
            base_url = "https://x"
            auth = { type = "api_key", env = "X" }
            [auth]
            enabled = true
            [[auth.keys]]
            name = "agent"
            hash = "PLACEHOLDER"
            profile = "local"
            [profiles.local]
            backend_allowlist = ["omlx"]
            "#,
        )
        .unwrap();
        let (plain, hash) = generate_key();
        let mut cfg = cfg;
        cfg.auth.keys[0].hash = hash;
        let mut h = HeaderMap::new();
        h.insert("x-api-key", plain.parse().unwrap());

        // Pin vorhanden + Profil konfiguriert → erzwungen.
        assert_eq!(forced_profile(&h, &cfg).unwrap().as_deref(), Some("local"));

        // Key ohne Profil-Pin → keine Erzwingung (Client wählt frei).
        let (plain2, hash2) = generate_key();
        cfg.auth.keys[0] = router_config::AuthKey {
            name: "free".into(),
            hash: hash2,
            daily_budget_usd: None,
            monthly_budget_usd: None,
            profile: None,
        };
        h.insert("x-api-key", plain2.parse().unwrap());
        assert_eq!(forced_profile(&h, &cfg).unwrap(), None);

        // Pin auf ein nicht konfiguriertes Profil → Err (fail-closed, kein Fallback auf default).
        let (plain3, hash3) = generate_key();
        cfg.auth.keys[0] = router_config::AuthKey {
            name: "broken".into(),
            hash: hash3,
            daily_budget_usd: None,
            monthly_budget_usd: None,
            profile: Some("typo".into()),
        };
        h.insert("x-api-key", plain3.parse().unwrap());
        assert!(forced_profile(&h, &cfg).is_err());
    }
}
