//! Automatische OpenRouter-Key-Rotation im Prozess — kein Cron, kein externes Script.
//!
//! Jeder Request ruft [`Rotator::maybe_rotate`] auf (billig: In-Memory-Throttle).
//! Rotiert wird, sobald eine der Bedingungen greift:
//!   - der Prozess länger als `OPENROUTER_ROTATE_DAYS` (Default 10) läuft, oder
//!   - der aktuelle Key älter als `OPENROUTER_ROTATE_DAYS` ist
//!     (`OPENROUTER_LAST_ROTATION` in `.env`; fehlt der Eintrag, gilt der Key als alt).
//!
//! Der Management-Key kommt aus der macOS-Keychain (`security` CLI, per
//! `scripts/store-key.sh` hinterlegt) — nie als Plaintext in einer Datei.
//! Konfiguration über `.env`: `OPENROUTER_LIMIT`, `OPENROUTER_LIMIT_RESET`,
//! `OPENROUTER_ROTATE_DAYS`, `OPENROUTER_MGMT_KEY_SERVICE`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::history::now_unix;

const API_BASE: &str = "https://openrouter.ai/api/v1";
/// Echte Prüfung (Datei-Read) höchstens alle 30 s — der Request-Pfad selbst bleibt im Nanosekunden-Bereich.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);
const SECS_PER_DAY: u64 = 86_400;

/// Gibt an, warum eine Rotation fällig ist; `None` = nicht fällig.
pub fn rotation_due(
    uptime: Duration,
    last_rotation: Option<u64>,
    rotate_days: u64,
    now: u64,
) -> Option<&'static str> {
    let days = rotate_days.max(1) * SECS_PER_DAY;
    if uptime >= Duration::from_secs(days) {
        return Some("process uptime");
    }
    match last_rotation {
        None => Some("no rotation recorded"),
        Some(ts) if now.saturating_sub(ts) >= days => Some("key age"),
        Some(_) => None,
    }
}

/// Liest eine Variable aus einer dotenv-Datei (Zeile `NAME=wert`).
pub fn env_get(path: &str, name: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let needle = format!("{name}=");
    content
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .find_map(|l| l.strip_prefix(&needle).map(|v| v.trim().to_string()))
}

/// Setzt/ergänzt eine Variable in einer dotenv-Datei — atomar (Temp-Datei + rename), 0600.
pub fn env_set(path: &str, name: &str, val: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    let needle = format!("{name}=");
    let mut found = false;
    for l in lines.iter_mut() {
        if l.starts_with(&needle) {
            *l = format!("{name}={val}");
            found = true;
        }
    }
    if !found {
        lines.push(format!("{name}={val}"));
    }
    let mut content = lines.join("\n");
    content.push('\n');
    let tmp = format!("{path}.tmp{}", std::process::id());
    std::fs::write(&tmp, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

#[derive(Clone)]
pub struct Rotator {
    inner: Arc<RotatorInner>,
}

struct RotatorInner {
    env_file: String,
    mgmt_service: String,
    limit: Option<f64>,
    limit_reset: Option<String>,
    rotate_days: u64,
    uptime_start: Instant,
    last_real_check: Mutex<Instant>,
    rotating: AtomicBool,
    http: reqwest::Client,
}

impl Rotator {
    /// Liest die Konfiguration aus der Prozess-Umgebung (dotenvy hat `.env` bereits geladen).
    pub fn from_env() -> Self {
        let env_file = std::env::var("ROUTER_ENV_FILE").unwrap_or_else(|_| "./.env".into());
        let limit = std::env::var("OPENROUTER_LIMIT")
            .ok()
            .and_then(|s| s.parse::<f64>().ok());
        let rotate_days = std::env::var("OPENROUTER_ROTATE_DAYS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(10);
        if limit.is_none() {
            tracing::warn!(
                "OPENROUTER_LIMIT ist in .env nicht gesetzt — Rotation erzeugt Keys ohne Limit"
            );
        }
        Self {
            inner: Arc::new(RotatorInner {
                env_file,
                mgmt_service: std::env::var("OPENROUTER_MGMT_KEY_SERVICE")
                    .unwrap_or_else(|_| "openrouter-management-key".into()),
                limit,
                limit_reset: std::env::var("OPENROUTER_LIMIT_RESET").ok(),
                rotate_days,
                uptime_start: Instant::now(),
                last_real_check: Mutex::new(Instant::now()),
                rotating: AtomicBool::new(false),
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()
                    .expect("reqwest client"),
            }),
        }
    }

    /// Wird bei jedem Request aufgerufen. Tut nichts, bis eine Rotation fällig ist.
    pub async fn maybe_rotate(&self) {
        let inner = &self.inner;
        // Billiger Throttle: echte Prüfung höchstens alle CHECK_INTERVAL.
        {
            let mut last = inner.last_real_check.lock().unwrap();
            if last.elapsed() < CHECK_INTERVAL {
                return;
            }
            *last = Instant::now();
        }
        let last_rotation = env_get(&inner.env_file, "OPENROUTER_LAST_ROTATION")
            .and_then(|s| s.parse::<u64>().ok());
        let reason = rotation_due(
            inner.uptime_start.elapsed(),
            last_rotation,
            inner.rotate_days,
            now_unix(),
        );
        let Some(reason) = reason else { return };
        tracing::info!(reason, "openrouter key rotation due");

        // Nur eine Rotation gleichzeitig; Verlierer überspringen (nächster Request prüft erneut).
        if inner.rotating.swap(true, Ordering::SeqCst) {
            return;
        }
        let res = self.rotate_inner().await;
        inner.rotating.store(false, Ordering::SeqCst);

        match res {
            Ok(summary) => tracing::info!(%summary, "openrouter key rotated"),
            Err(e) => tracing::warn!(error = %e, "openrouter key rotation failed (retry later)"),
        }
    }

    /// Management-Key aus der macOS-Keychain (security CLI), nur bei fälliger Rotation.
    async fn keychain_mgmt(&self) -> Option<String> {
        let user = std::env::var("USER").unwrap_or_default();
        let out = tokio::process::Command::new("security")
            .args([
                "find-generic-password",
                "-a",
                &user,
                "-s",
                &self.inner.mgmt_service,
                "-w",
            ])
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
    }

    async fn rotate_inner(&self) -> Result<String, String> {
        let inner = &self.inner;
        let mgmt = self
            .keychain_mgmt()
            .await
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                format!(
                    "Management-Key fehlt in Keychain ('{}'). Erst: ./scripts/store-key.sh",
                    inner.mgmt_service
                )
            })?;

        // 1. Neuen Key erzeugen (Name + optionales Limit).
        let name = format!("router-auto-{}", now_unix());
        let mut payload = serde_json::Map::new();
        payload.insert("name".into(), json!(name));
        if let Some(limit) = inner.limit {
            payload.insert("limit".into(), json!(limit));
        }
        let resp = inner
            .http
            .post(format!("{API_BASE}/keys"))
            .bearer_auth(&mgmt)
            .json(&payload)
            .send()
            .await
            .map_err(|e| format!("create key request failed: {e}"))?;
        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("create key response unreadable: {e}"))?;
        if !status.is_success() {
            return Err(format!("create key HTTP {status}: {body}"));
        }
        let new_key = body["data"]["key"]
            .as_str()
            .ok_or_else(|| format!("create key response without data.key: {body}"))?
            .to_string();
        let new_hash = body["data"]["id"].as_str().unwrap_or_default().to_string();

        // 2. Neuen Key verifizieren.
        if let Ok(v) = inner
            .http
            .get(format!("{API_BASE}/auth/key"))
            .bearer_auth(&new_key)
            .send()
            .await
        {
            if !v.status().is_success() {
                tracing::warn!(status = %v.status(), hash = %new_hash, "new key verification failed");
            }
        }

        // 3. Limit-Reset best-effort (PATCH, Fallback PUT).
        if let (Some(reset), false) = (&inner.limit_reset, new_hash.is_empty()) {
            let ok = inner
                .http
                .patch(format!("{API_BASE}/keys/{new_hash}"))
                .bearer_auth(&mgmt)
                .json(&json!({ "limit_reset": reset }))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
                || inner
                    .http
                    .put(format!("{API_BASE}/keys/{new_hash}"))
                    .bearer_auth(&mgmt)
                    .json(&json!({ "limit_reset": reset }))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
            if !ok {
                tracing::warn!(%reset, hash = %new_hash, "limit_reset konnte nicht gesetzt werden");
            }
        }

        // 4. Aktivieren: Prozess-Env sofort, .env für den nächsten Start.
        std::env::set_var("OPENROUTER_API_KEY", &new_key);
        let now = now_unix().to_string();
        for (k, v) in [
            ("OPENROUTER_API_KEY", new_key.as_str()),
            ("OPENROUTER_LAST_ROTATION", now.as_str()),
        ] {
            if let Err(e) = env_set(&inner.env_file, k, v) {
                tracing::warn!(%k, error = %e, ".env update failed (in-memory key bleibt aktiv)");
            }
        }
        if !new_hash.is_empty() {
            let _ = env_set(&inner.env_file, "OPENROUTER_KEY_HASH", &new_hash);
        }

        // 5. Alten Key löschen (Hash aus letzter Rotation, sonst Label-Heuristik).
        let stored_hash = env_get(&inner.env_file, "OPENROUTER_KEY_HASH").filter(|h| !h.is_empty());
        let old_hash = if let Some(h) = stored_hash {
            Some(h)
        } else {
            // Fallback nur, wenn .env noch keinen Hash kennt (erste Rotation).
            let list = inner
                .http
                .get(format!("{API_BASE}/keys"))
                .bearer_auth(&mgmt)
                .send()
                .await
                .ok();
            let list: Option<serde_json::Value> = match list {
                Some(r) => r.json().await.ok(),
                None => None,
            };
            list.and_then(|list| {
                list["data"].as_array()?.iter().find(|k| {
                    k["label"]
                        .as_str()
                        .map(|l| l.starts_with("router-"))
                        .unwrap_or(false)
                        && k["id"].as_str() != Some(new_hash.as_str())
                })
                .and_then(|k| k["id"].as_str().map(str::to_string))
            })
        };
        if let Some(old) = old_hash {
            match inner
                .http
                .delete(format!("{API_BASE}/keys/{old}"))
                .bearer_auth(&mgmt)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    tracing::info!(hash = %old, "alter OpenRouter-Key gelöscht");
                }
                Ok(r) => tracing::warn!(hash = %old, status = %r.status(), "alter Key konnte nicht gelöscht werden"),
                Err(e) => tracing::warn!(hash = %old, error = %e, "alter Key konnte nicht gelöscht werden"),
            }
        } else {
            tracing::warn!("kein alter Key identifiziert — im Dashboard prüfen");
        }

        Ok(format!(
            "{name} (hash {new_hash}, limit {:?}{})",
            inner.limit,
            inner
                .limit_reset
                .as_deref()
                .map(|r| format!(", reset {r}"))
                .unwrap_or_default()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn due_on_uptime() {
        assert_eq!(
            rotation_due(secs(11 * 86400), Some(1), 10, 2),
            Some("process uptime")
        );
    }

    #[test]
    fn due_on_key_age() {
        let now = 1_000_000;
        assert_eq!(
            rotation_due(secs(60), Some(now - 10 * 86400), 10, now),
            Some("key age")
        );
        // genau an der Grenze: fällig
        assert_eq!(
            rotation_due(secs(60), Some(now - 10 * 86400), 10, now),
            Some("key age")
        );
    }

    #[test]
    fn not_due_when_fresh() {
        let now = 1_000_000;
        assert_eq!(rotation_due(secs(60), Some(now - 60), 10, now), None);
    }

    #[test]
    fn unset_last_rotation_is_due() {
        assert_eq!(rotation_due(secs(60), None, 10, 42), Some("no rotation recorded"));
    }

    #[test]
    fn env_set_roundtrip_preserves_other_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("rot-test-{}.env", std::process::id()));
        std::fs::write(&path, "A=1\nB=2\n").unwrap();

        env_set(path.to_str().unwrap(), "B", "neu").unwrap();
        env_set(path.to_str().unwrap(), "C", "3").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("A=1\n"));
        assert!(content.contains("B=neu\n"));
        assert!(content.contains("C=3\n"));
        assert_eq!(env_get(path.to_str().unwrap(), "B").as_deref(), Some("neu"));
        assert_eq!(env_get(path.to_str().unwrap(), "C").as_deref(), Some("3"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, ".env muss 0600 sein");
        }
        let _ = std::fs::remove_file(&path);
    }
}
