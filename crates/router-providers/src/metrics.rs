//! In-memory Latenz-Tracker. Ring-Buffer pro (Backend, Model-ID).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use router_core::Backend;

const RING_SIZE: usize = 128;

#[derive(Debug, Clone, Copy)]
pub struct LatencySample {
    pub ms: u32,
    pub at: Instant,
}

#[derive(Debug, Default)]
struct Ring {
    buf: Vec<LatencySample>,
    head: usize,
}

impl Ring {
    fn push(&mut self, sample: LatencySample) {
        if self.buf.len() < RING_SIZE {
            self.buf.push(sample);
        } else {
            self.buf[self.head] = sample;
            self.head = (self.head + 1) % RING_SIZE;
        }
    }

    fn percentile_ms(&self, p: f64) -> Option<u32> {
        if self.buf.is_empty() {
            return None;
        }
        let mut v: Vec<u32> = self.buf.iter().map(|s| s.ms).collect();
        v.sort_unstable();
        let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
        Some(v[idx.min(v.len() - 1)])
    }
}

#[derive(Debug, Default, Clone)]
pub struct LatencyTracker {
    inner: Arc<Mutex<HashMap<(Backend, String), Ring>>>,
}

impl LatencyTracker {
    pub fn new() -> Self { Self::default() }

    pub fn record(&self, backend: Backend, model_id: &str, elapsed: Duration) {
        let ms = elapsed.as_millis().min(u32::MAX as u128) as u32;
        let mut map = self.inner.lock();
        let ring = map
            .entry((backend, model_id.to_string()))
            .or_default();
        ring.push(LatencySample { ms, at: Instant::now() });
    }

    pub fn p95_ms(&self, backend: Backend, model_id: &str) -> Option<u32> {
        let map = self.inner.lock();
        map.get(&(backend, model_id.to_string()))
            .and_then(|r| r.percentile_ms(0.95))
    }

    pub fn p50_ms(&self, backend: Backend, model_id: &str) -> Option<u32> {
        let map = self.inner.lock();
        map.get(&(backend, model_id.to_string()))
            .and_then(|r| r.percentile_ms(0.5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_work() {
        let t = LatencyTracker::new();
        // Linear 100..=2000, p50 ~= 1000, p95 ~= 1900.
        for i in 1..=20u32 {
            t.record(
                Backend::OpenRouter,
                "m",
                Duration::from_millis((i * 100) as u64),
            );
        }
        let p95 = t.p95_ms(Backend::OpenRouter, "m").unwrap();
        let p50 = t.p50_ms(Backend::OpenRouter, "m").unwrap();
        assert!(p95 >= 1800, "p95 was {p95}");
        assert!((900..=1200).contains(&p50), "p50 was {p50}");
        assert!(p50 < p95);
    }
}
