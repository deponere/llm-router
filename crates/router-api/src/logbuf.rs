//! In-Memory-Log-Ringbuffer + tracing-Layer. Die UI rendert die Einträge im
//! Loguru-Stil: `YYYY-MM-DD HH:MM:SS.mmm | LEVEL     | target:line - message`.

use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    /// Unix-Millisekunden (lokal gerendert in der UI).
    pub ts_ms: u64,
    /// Loguru-Level-Name: TRACE / DEBUG / INFO / WARNING / ERROR.
    pub level: String,
    /// Modul-Pfad (tracing-Target), z. B. `router_api::rotate`.
    pub target: String,
    pub line: Option<u32>,
    /// Nachricht + Felder im fmt-Stil: `msg key=value key2=value2`.
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct LogBuffer(Arc<Mutex<Inner>>);

#[derive(Debug)]
struct Inner {
    entries: VecDeque<LogEntry>,
    max: usize,
}

impl LogBuffer {
    pub fn new(max: usize) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            entries: VecDeque::with_capacity(max.min(16)),
            max,
        })))
    }

    pub fn push(&self, e: LogEntry) {
        let mut g = self.0.lock();
        if g.entries.len() >= g.max {
            g.entries.pop_front();
        }
        g.entries.push_back(e);
    }

    /// Neueste zuerst, höchstens `limit` Einträge.
    pub fn snapshot(&self, limit: usize) -> Vec<LogEntry> {
        let g = self.0.lock();
        g.entries.iter().rev().take(limit.max(1)).cloned().collect()
    }

    pub fn clear(&self) -> usize {
        let mut g = self.0.lock();
        let n = g.entries.len();
        g.entries.clear();
        n
    }
}

fn level_name(l: &Level) -> &'static str {
    match *l {
        Level::TRACE => "TRACE",
        Level::DEBUG => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARNING",
        Level::ERROR => "ERROR",
    }
}

/// tracing-Layer: formatiert jedes Event wie loguru und legt es in den Buffer.
pub struct LoguruLayer {
    buf: LogBuffer,
}

impl LoguruLayer {
    pub fn new(buf: LogBuffer) -> Self {
        Self { buf }
    }
}

impl<S: Subscriber> Layer<S> for LoguruLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // reqwests log-Bridge ("log" als Target) doppelt die Connect-Zeilen mit
        // log.target=/log.file=-Feldern — raus, damit die Ansicht sauber bleibt.
        if meta.target() == "log" {
            return;
        }
        let mut v = FieldCollector::default();
        event.record(&mut v);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let message = match (v.msg.is_empty(), v.fields.is_empty()) {
            (true, true) => String::new(),
            (true, false) => v.fields.join(" "),
            (false, true) => v.msg,
            (false, false) => format!("{} {}", v.msg, v.fields.join(" ")),
        };
        self.buf.push(LogEntry {
            ts_ms,
            level: level_name(meta.level()).to_string(),
            target: meta.target().to_string(),
            line: meta.line(),
            message,
        });
    }
}

#[derive(Default)]
struct FieldCollector {
    msg: String,
    fields: Vec<String>,
}

impl FieldCollector {
    fn push(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.msg = value;
        } else {
            self.fields.push(format!("{}={}", field.name(), value));
        }
    }
}

impl Visit for FieldCollector {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.push(field, value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.push(field, value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.push(field, value.to_string());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.push(field, value.to_string());
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.push(field, value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_keeps_max_and_snapshot_newest_first() {
        let b = LogBuffer::new(3);
        for i in 0..5 {
            b.push(LogEntry {
                ts_ms: i,
                level: "INFO".into(),
                target: "t".into(),
                line: None,
                message: format!("m{i}"),
            });
        }
        let snap = b.snapshot(10);
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].message, "m4");
        assert_eq!(snap[2].message, "m2");
        assert_eq!(b.snapshot(2).len(), 2);
    }

    #[test]
    fn clear_empties() {
        let b = LogBuffer::new(10);
        b.push(LogEntry { ts_ms: 1, level: "INFO".into(), target: "t".into(), line: None, message: "x".into() });
        assert_eq!(b.clear(), 1);
        assert!(b.snapshot(10).is_empty());
    }
}
