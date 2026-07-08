//! In-Memory-Ring-Buffer aller abgeschlossenen Requests. Dient dem
//! xbar-/Dashboard-Widget zur Kostenverfolgung.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;

const CAPACITY: usize = 100;

#[derive(Debug, Clone, Serialize)]
pub struct Transaction {
    pub unix_ts: u64,
    pub api: String,
    pub profile: String,
    pub backend: String,
    pub model_id: String,
    pub duration_ms: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug)]
struct Inner {
    entries: VecDeque<Transaction>,
    session_start_unix: u64,
}

#[derive(Debug, Clone)]
pub struct TransactionHistory(Arc<Mutex<Inner>>);

impl TransactionHistory {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Inner {
            entries: VecDeque::with_capacity(CAPACITY),
            session_start_unix: now_unix(),
        })))
    }

    pub fn record(&self, tx: Transaction) {
        let mut g = self.0.lock();
        if g.entries.len() >= CAPACITY {
            g.entries.pop_front();
        }
        g.entries.push_back(tx);
    }

    pub fn snapshot(&self, limit: usize) -> Snapshot {
        let g = self.0.lock();
        let session_start = g.session_start_unix;
        let today_start = unix_utc_day_start();
        let session = totals_since(&g.entries, session_start);
        let today = totals_since(&g.entries, today_start);
        let recent: Vec<Transaction> = g
            .entries
            .iter()
            .rev()
            .take(limit.max(1))
            .cloned()
            .collect();
        Snapshot {
            session_start_unix: session_start,
            today_start_unix: today_start,
            totals_session: session,
            totals_today_utc: today,
            recent,
        }
    }
}

impl Default for TransactionHistory {
    fn default() -> Self { Self::new() }
}

#[derive(Debug, Serialize)]
pub struct Totals {
    pub cost_usd: f64,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub session_start_unix: u64,
    pub today_start_unix: u64,
    pub totals_session: Totals,
    pub totals_today_utc: Totals,
    pub recent: Vec<Transaction>,
}

fn totals_since(entries: &VecDeque<Transaction>, since: u64) -> Totals {
    let mut cost = 0.0;
    let mut count = 0;
    for e in entries.iter().filter(|t| t.unix_ts >= since) {
        cost += e.cost_usd.unwrap_or(0.0);
        count += 1;
    }
    Totals { cost_usd: cost, count }
}

/// Aktuelle Unix-Zeit in Sekunden. Geteilt von den Handlern fürs
/// Transaction-Logging.
pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn unix_utc_day_start() -> u64 {
    let now = now_unix();
    now - (now % 86_400)
}
