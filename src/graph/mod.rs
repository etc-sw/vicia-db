pub(crate) mod current_projection;
pub(crate) mod pending_overlay;
pub(crate) mod storage;
pub(crate) mod types;

// Re-export the one name imported via `crate::graph::FactStorage` across the codebase.
// Everything else is imported directly from `crate::graph::types::*`.
pub(crate) use storage::FactStorage;
