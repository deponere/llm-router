//! AlertService (Feature #4): Webhook- + Telegram-Benachrichtigungen für
//! Rotation-Fehler, Backend-Down, niedrige Balance und Tageskosten-Schwelle.
//! Pro Event-Typ throttled (Default 1/h), nie blockierend — Fehler landen nur im Log.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use router_config::AlertsConfig;
use serde_json::json;

/// Mindestabstand zwischen zwei Alerts desselben Event-Typs.
const THROTTLE_SECS: u64 = 3600;

#[derive(Clone)]
pub struct AlertService(Arc<Inner>);

struct Inner {
    cfg: AlertsConfig,
    client: reqwest::Client,
    last_fire: Mutex<HashMap<String, u64>>,
    /// UTC-Tag, an dem die Kosten-Schwelle zuletzt gefeuert hat (einmal pro Tag).
    day_alert_fired: Mutex<Option<u64>>,
}

impl AlertService {
    pub fn new(cfg: AlertsConfig) -> Self {
        Self(Arc::new(Inner {
            cfg,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
            last_fire: Mutex::new(HashMap::new()),
            day_alert_fired: Mutex::new(None),
        }))
    }

    fn enabled(&self, event: &str) -> bool {
        let e = &self.0.cfg.events;
        match event {
            "rotation_failed" => e.rotation_failed,
            "rotation_succeeded" => e.rotation_succeeded,
            "backend_down" => e.backend_down,
            "balance_low" => e.balance_low,
            "daily_cost_threshold" => e.daily_cost_threshold,
            _ => true, // Test-Alert immer erlauben
        }
    }

    /// `true`, wenn das Event gedrosselt ist (innerhalb THROTTLE_SECS schon gefeuert).
    fn throttled(&self, event: &str) -> bool {
        let now = now();
        let mut g = self.0.last_fire.lock();
        match g.get(event) {
            Some(last) if now.saturating_sub(*last) < THROTTLE_SECS => true,
            _ => {
                g.insert(event.into(), now);
                false
            }
        }
    }

    /// Feuert ein Event an Webhook und/oder Telegram (Best-Effort, nie blockierend).
    pub fn fire(&self, event: &str, level: &str, message: String) {
        if !self.enabled(event) {
            return;
        }
        if self.throttled(event) {
            return;
        }
        tracing::warn!(%event, %level, %message, "alert fired");
        let this = self.clone();
        let (event, level, message) = (event.to_string(), level.to_string(), message);
        tokio::spawn(async move {
            this.deliver(&event, &level, &message).await;
        });
    }

    async fn deliver(&self, event: &str, level: &str, message: &str) {
        let payload = json!({ "event": event, "level": level, "message": message, "ts": now() });
        let cfg = &self.0.cfg;
        if !cfg.webhook_url.is_empty() {
            if let Err(e) = self.0.client.post(&cfg.webhook_url).json(&payload).send().await {
                tracing::warn!(error = %e, "alert webhook delivery failed");
            }
        }
        let token = if cfg.telegram_token_env.is_empty() {
            String::new()
        } else {
            std::env::var(&cfg.telegram_token_env).unwrap_or_default()
        };
        if !token.is_empty() && !cfg.telegram_chat_id.is_empty() {
            let text = format!("[{event}] {message}");
            if let Err(e) = self
                .0
                .client
                .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
                .json(&json!({ "chat_id": cfg.telegram_chat_id, "text": text }))
                .send()
                .await
            {
                tracing::warn!(error = %e, "alert telegram delivery failed");
            }
        }
    }

    /// Tageskosten-Schwelle: feuert höchstens einmal pro UTC-Tag.
    pub fn check_daily_cost(&self, today_cost: f64, threshold: f64) {
        if threshold <= 0.0 {
            return;
        }
        let today = crate::auth::utc_day_key();
        let mut g = self.0.day_alert_fired.lock();
        if *g == Some(today) {
            return;
        }
        if today_cost >= threshold {
            *g = Some(today);
            drop(g);
            self.fire(
                "daily_cost_threshold",
                "warning",
                format!("UTC-Tageskosten ${today_cost:.2} ≥ Schwelle ${threshold:.2}"),
            );
        }
    }

    /// Test-Event (Einstellungen-Tab / `router-admin alerts test`).
    pub fn fire_test(&self) {
        self.fire("test", "info", "Test-Alert vom LLM-Router".into());
    }
}

impl Default for AlertService {
    fn default() -> Self {
        Self::new(AlertsConfig::default())
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_blocks_rapid_fires() {
        let s = AlertService::new(AlertsConfig::default());
        // Events feuern asynchron — wir testen nur den Throttle-Zustand.
        assert!(!s.throttled("rotation_failed"));
        assert!(s.throttled("rotation_failed"));
        assert!(!s.throttled("backend_down"));
    }

    #[test]
    fn disabled_events_are_skipped() {
        let mut cfg = AlertsConfig::default();
        cfg.events.rotation_failed = false;
        let s = AlertService::new(cfg);
        assert!(!s.enabled("rotation_failed"));
        assert!(s.enabled("backend_down"));
    }

    #[tokio::test]
    async fn daily_cost_fires_once_per_day() {
        let s = AlertService::new(AlertsConfig::default());
        let threshold = 5.0;
        s.check_daily_cost(6.0, threshold);
        // zweiter Check am selben Tag: throttled durch day_alert_fired
        let fired = *s.0.day_alert_fired.lock();
        assert!(fired.is_some());
        s.check_daily_cost(6.0, threshold);
        assert_eq!(*s.0.day_alert_fired.lock(), fired);
        // unter Schwelle am Folgetag: kein weiterer Alarm, Merker bleibt beim Vortag
        let mut g = s.0.day_alert_fired.lock();
        *g = Some(fired.unwrap() - 86_400);
        drop(g);
        s.check_daily_cost(1.0, threshold);
        assert_eq!(*s.0.day_alert_fired.lock(), Some(fired.unwrap() - 86_400));
    }
}
