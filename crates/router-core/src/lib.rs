//! Deterministischer Kern des Routers: Request-Normalisierung, Modell-Registry, Hard-Filter und Scoring. Enthält keinerlei I/O — das übernimmt `router-providers`.

pub mod norm;
pub mod registry;
pub mod rules;
pub mod score;
pub mod profile;
pub mod decision;

pub use decision::{decide, Decision, DecisionTrace};
pub use norm::{
    ImagePart, ModalityKind, NormMessage, NormRequest, NormRole, PrivacyTag, ReasoningHint,
    RequiredCaps, ResponseFormat, ToolDef,
};
pub use profile::ResolvedProfile;
pub use registry::{CapsSet, ModalitySet, ModelCandidate, PrivacyClass, Registry};
