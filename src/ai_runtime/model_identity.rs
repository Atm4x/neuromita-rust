//! Immutable identity of a selected model implementation across AI services.
//! The visible model name is not its process identity or its worker backend.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerImplementation {
    Python,
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AiModelIdentity {
    pub model_id: String,
    pub implementation: WorkerImplementation,
    pub revision: Option<String>,
}
