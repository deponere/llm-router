use std::sync::Arc;

use router_config::Config;
use router_providers::{LatencyTracker, RegistryHandle};

use crate::history::TransactionHistory;
use crate::logbuf::LogBuffer;
use crate::rotate::Rotator;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub registry: Arc<RegistryHandle>,
    pub tracker: LatencyTracker,
    pub history: TransactionHistory,
    /// Automatische OpenRouter-Key-Rotation (prüft bei jedem Request).
    pub rotator: Arc<Rotator>,
    /// Log-Ringbuffer für das Web-Interface (Loguru-stilisiert).
    pub logs: LogBuffer,
}
