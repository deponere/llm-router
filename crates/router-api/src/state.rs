use std::sync::Arc;

use router_config::Config;
use router_providers::{LatencyTracker, RegistryHandle};

use crate::history::TransactionHistory;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub registry: Arc<RegistryHandle>,
    pub tracker: LatencyTracker,
    pub history: TransactionHistory,
}
