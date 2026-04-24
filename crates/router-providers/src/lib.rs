//! Egress zu konkreten LLM-Backends (OpenRouter, oMLX) und
//! Modell-Katalog-Aggregation.

pub mod metrics;
pub mod openrouter;
pub mod omlx;
pub mod registry;

pub use metrics::{LatencyTracker, LatencySample};
pub use registry::{RegistryHandle, RegistryError};
