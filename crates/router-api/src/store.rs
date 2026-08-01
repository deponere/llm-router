//! Persistente Nutzungshistorie (SQLite, bundled). Spiegel der In-Memory-`TransactionHistory`:
//! jede abgeschlossene Transaction landet hier, damit Kosten-/Nutzungs-Analysen Router-Restarts
//! überleben. Alle Operationen sind kurz und laufen über ein `Arc<Mutex<Connection>>`.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::history::Transaction;

#[derive(Clone)]
pub struct Store(Arc<Mutex<Connection>>);

impl Store {
    /// Öffnet (und initialisiert) die SQLite-Datenbank. Legt fehlende Elternverzeichnisse an.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let conn = Connection::open(path)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS transactions(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                unix_ts REAL NOT NULL,
                api TEXT NOT NULL,
                profile TEXT NOT NULL,
                backend TEXT NOT NULL,
                model_id TEXT NOT NULL,
                key_name TEXT,
                tokens_in INTEGER NOT NULL DEFAULT 0,
                tokens_out INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_tx_ts ON transactions(unix_ts);
            CREATE INDEX IF NOT EXISTS idx_tx_key ON transactions(key_name);",
        )?;
        Ok(Self(Arc::new(Mutex::new(conn))))
    }

    /// Fügt eine abgeschlossene Transaction hinzu. `key` = Router-API-Key (attribuiert Kosten),
    /// `tokens_in`/`error` optional (Stream-Pfade haben sie teils nicht).
    pub fn insert(
        &self,
        tx: &Transaction,
        key: Option<&str>,
        tokens_in: u64,
        error: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self.0.lock();
        conn.execute(
            "INSERT INTO transactions(unix_ts, api, profile, backend, model_id, key_name, tokens_in, tokens_out, cost_usd, duration_ms, error)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                tx.unix_ts as f64,
                tx.api,
                tx.profile,
                tx.backend,
                tx.model_id,
                key,
                tokens_in as i64,
                tx.tokens_out as i64,
                tx.cost_usd,
                tx.duration_ms as i64,
                error
            ],
        )?;
        Ok(())
    }

    /// Kosten-Summe (USD) seit `since_unix`, optional gefiltert auf einen Key — Basis für Budget-Checks.
    pub fn spend_since(&self, since_unix: f64, key: Option<&str>) -> f64 {
        let conn = self.0.lock();
        conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0.0) FROM transactions WHERE unix_ts >= ?1 AND (?2 IS NULL OR key_name = ?2)",
            params![since_unix, key],
            |r| r.get(0),
        )
        .unwrap_or(0.0)
    }

    /// UTC-Tagessumme eines Keys — für `daily_budget_usd`.
    pub fn spend_today_utc(&self, key: &str) -> f64 {
        self.spend_since(crate::history::unix_utc_day_start() as f64, Some(key))
    }

    /// UTC-Monatssumme eines Keys — für `monthly_budget_usd` (Monatsbeginn = 1. des Monats 00:00 UTC).
    pub fn spend_this_month_utc(&self, key: &str) -> f64 {
        let now = chrono_like::utc_month_start();
        self.spend_since(now as f64, Some(key))
    }

    /// Letzte `limit` Einträge (neueste zuerst), optional key-gefiltert.
    pub fn recent(&self, limit: usize, key: Option<&str>) -> Vec<TxRow> {
        let conn = self.0.lock();
        let mut stmt = conn
            .prepare(
                "SELECT unix_ts, api, profile, backend, model_id, key_name, tokens_in, tokens_out, cost_usd, duration_ms, error
                 FROM transactions WHERE (?2 IS NULL OR key_name = ?2)
                 ORDER BY unix_ts DESC LIMIT ?1",
            )
            .unwrap();
        let rows = stmt
            .query_map(params![limit as i64, key], |r| {
                Ok(TxRow {
                    unix_ts: r.get(0)?,
                    api: r.get(1)?,
                    profile: r.get(2)?,
                    backend: r.get(3)?,
                    model_id: r.get(4)?,
                    key_name: r.get(5)?,
                    tokens_in: r.get(6)?,
                    tokens_out: r.get(7)?,
                    cost_usd: r.get(8)?,
                    duration_ms: r.get(9)?,
                    error: r.get(10)?,
                })
            })
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Tages-Serie (Kosten/Aufrufe/Tokens pro UTC-Tag) ab `from_unix`.
    pub fn series(&self, from_unix: f64, key: Option<&str>) -> Vec<SeriesPoint> {
        let conn = self.0.lock();
        let mut stmt = conn
            .prepare(
                "SELECT CAST(unix_ts / 86400.0 AS INTEGER) * 86400 AS day,
                        COALESCE(SUM(cost_usd), 0.0), COUNT(*), COALESCE(SUM(tokens_out), 0)
                 FROM transactions WHERE unix_ts >= ?1 AND (?2 IS NULL OR key_name = ?2)
                 GROUP BY day ORDER BY day",
            )
            .unwrap();
        let rows = stmt
            .query_map(params![from_unix, key], |r| {
                Ok(SeriesPoint {
                    day_unix: r.get(0)?,
                    cost_usd: r.get(1)?,
                    count: r.get(2)?,
                    tokens_out: r.get(3)?,
                })
            })
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Summen gruppiert nach einer Spalte (Whitelist gegen SQL-Injection).
    pub fn breakdown(&self, from_unix: f64, by: &str, key: Option<&str>, limit: usize) -> Vec<BreakdownRow> {
        let col = match by {
            "profile" => "profile",
            "backend" => "backend",
            "model" => "model_id",
            "key" => "key_name",
            _ => return Vec::new(),
        };
        let conn = self.0.lock();
        let sql = format!(
            "SELECT {col} AS grp, COALESCE(SUM(cost_usd), 0.0), COUNT(*)
             FROM transactions WHERE unix_ts >= ?1 AND (?2 IS NULL OR key_name = ?2)
             GROUP BY grp ORDER BY 2 DESC LIMIT ?3"
        );
        let mut stmt = conn.prepare(&sql).unwrap();
        let rows = stmt
            .query_map(params![from_unix, key, limit as i64], |r| {
                Ok(BreakdownRow {
                    group: r.get::<_, Option<String>>(0)?.unwrap_or_else(|| "(none)".into()),
                    cost_usd: r.get(1)?,
                    count: r.get(2)?,
                })
            })
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    }

    /// Löscht Einträge älter als `older_than_unix` (Retention) und liefert die Anzahl.
    pub fn purge(&self, older_than_unix: f64) -> usize {
        let conn = self.0.lock();
        conn.execute("DELETE FROM transactions WHERE unix_ts < ?1", params![older_than_unix])
            .unwrap_or(0)
    }

    /// Alle Transactions seit `from_unix` (für Alerts/Tagescheck), key-optional.
    pub fn count_since(&self, from_unix: f64, key: Option<&str>) -> usize {
        let conn = self.0.lock();
        conn.query_row(
            "SELECT COUNT(*) FROM transactions WHERE unix_ts >= ?1 AND (?2 IS NULL OR key_name = ?2)",
            params![from_unix, key],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }
}

impl Default for Store {
    fn default() -> Self {
        // Nur für Tests/Struct-Literale; echte Instanz kommt aus `Store::open`.
        Self(Arc::new(Mutex::new(Connection::open_in_memory().unwrap())))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TxRow {
    pub unix_ts: f64,
    pub api: String,
    pub profile: String,
    pub backend: String,
    pub model_id: String,
    pub key_name: Option<String>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cost_usd: Option<f64>,
    pub duration_ms: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SeriesPoint {
    /// UTC-Tagesbeginn als Unix-Sekunden.
    pub day_unix: i64,
    pub cost_usd: f64,
    pub count: i64,
    pub tokens_out: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BreakdownRow {
    pub group: String,
    pub cost_usd: f64,
    pub count: i64,
}

/// Kleiner Helfer für Monatsbeginn in UTC (ohne chrono-Dependency).
mod chrono_like {
    /// Unix-Sekunden des 1. des aktuellen UTC-Monats, 00:00.
    pub fn utc_month_start() -> u64 {
        let now = crate::history::now_unix();
        let days = now / 86400;
        // Tag des Monats ausrechnen (grobe Näherung reicht nicht — wir brauchen den echten Monatsbeginn).
        let (y, m, d) = civil_from_days(days as i64);
        let _ = d;
        let days_since_epoch = days_from_civil(y, m, 1);
        (days_since_epoch as u64) * 86400
    }

    // Howard-Hinnant-Algorithmen (public domain) für Kalenderumrechnung.
    fn civil_from_days(z: i64) -> (i64, u64, u64) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u64;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    fn days_from_civil(y: i64, m: u64, d: u64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = (y - era * 400) as u64;
        let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe as i64 - 719_468
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Transaction;

    fn tx(ts: u64, cost: Option<f64>, key: Option<&str>) {
        let store = test_store();
        store
            .insert(
                &Transaction {
                    unix_ts: ts,
                    api: "openai".into(),
                    profile: "default".into(),
                    backend: "deepseek".into(),
                    model_id: "deepseek/deepseek-v4-pro".into(),
                    duration_ms: 100,
                    cost_usd: cost,
                    tokens_out: 10,
                },
                key,
                5,
                None,
            )
            .unwrap();
    }

    fn test_store() -> Store {
        Store::open(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn insert_and_recent() {
        let s = test_store();
        s.insert(
            &Transaction {
                unix_ts: 1_000,
                api: "openai".into(),
                profile: "default".into(),
                backend: "deepseek".into(),
                model_id: "m".into(),
                duration_ms: 10,
                cost_usd: Some(0.001),
                tokens_out: 7,
            },
            Some("pi"),
            3,
            None,
        )
        .unwrap();
        let r = s.recent(10, None);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key_name.as_deref(), Some("pi"));
        assert_eq!(r[0].tokens_in, 3);
        assert!((s.spend_since(0.0, Some("pi")) - 0.001).abs() < 1e-9);
        assert_eq!(s.spend_since(0.0, Some("other")), 0.0);
    }

    #[test]
    fn series_buckets_by_day() {
        let s = test_store();
        for (i, ts) in [1_700_000_000u64, 1_700_000_000 + 3600, 1_700_086_400].into_iter().enumerate() {
            s.insert(
                &Transaction {
                    unix_ts: ts,
                    api: "openai".into(),
                    profile: "default".into(),
                    backend: "deepseek".into(),
                    model_id: "m".into(),
                    duration_ms: 10,
                    cost_usd: Some(i as f64 * 0.001),
                    tokens_out: 1,
                },
                None,
                0,
                None,
            )
            .unwrap();
        }
        let ser = s.series(1_700_000_000.0, None);
        assert_eq!(ser.len(), 2, "zwei verschiedene UTC-Tage");
        assert_eq!(ser[0].count, 2);
        assert_eq!(ser[1].count, 1);
        assert_eq!(ser[0].day_unix + 86_400, ser[1].day_unix);
    }

    #[test]
    fn breakdown_and_purge() {
        let s = test_store();
        let ins = |ts: u64, cost: f64, key: &str| {
            s.insert(
                &Transaction {
                    unix_ts: ts,
                    api: "openai".into(),
                    profile: "default".into(),
                    backend: "deepseek".into(),
                    model_id: "deepseek/deepseek-v4-pro".into(),
                    duration_ms: 100,
                    cost_usd: Some(cost),
                    tokens_out: 10,
                },
                Some(key),
                5,
                None,
            )
            .unwrap();
        };
        ins(1_700_000_000, 0.5, "a");
        ins(1_700_000_100, 0.3, "b");
        let rows = s.breakdown(0.0, "key", None, 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group, "a");
        let purged = s.purge(1_700_000_100.0);
        assert_eq!(purged, 1);
        assert_eq!(s.count_since(0.0, None), 1);
    }
}
