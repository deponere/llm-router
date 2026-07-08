//! Egress zu konkreten LLM-Backends und Modell-Katalog-Aggregation. Jede
//! Backend-Instanz implementiert [`provider::Provider`]; die [`registry`] hält
//! sie als `Arc<dyn Provider>` und dispatcht über die Backend-ID.

pub mod anthropic;
pub mod artificial_analysis;
pub mod metrics;
pub mod provider;
pub mod openai_compat;
pub mod openrouter;
pub mod registry;
pub mod sse;

pub use artificial_analysis::{AaScores, ArtificialAnalysisClient};
pub use metrics::{LatencyTracker, LatencySample};
pub use provider::{ByteStream, Provider, ProviderError};
pub use registry::{RegistryHandle, RegistryError};
