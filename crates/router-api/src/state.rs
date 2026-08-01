use std::sync::Arc;

use router_config::Config;
use router_providers::{LatencyTracker, RegistryHandle};

use crate::history::TransactionHistory;
use crate::logbuf::LogBuffer;
use crate::rotate::Rotator;
use crate::store::Store;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    /// Pfad zur aktiven Config-Datei (für in-process `toml_edit`-Updates).
    pub config_path: Arc<std::path::PathBuf>,
    pub registry: Arc<RegistryHandle>,
    pub tracker: LatencyTracker,
    pub history: TransactionHistory,
    /// Persistente Nutzungshistorie (SQLite).
    pub store: Store,
    /// Automatische OpenRouter-Key-Rotation (prüft bei jedem Request).
    pub rotator: Arc<Rotator>,
    /// Log-Ringbuffer für das Web-Interface (Loguru-stilisiert).
    pub logs: LogBuffer,
}
