use super::{EffectPhase, RequestMetadata, RequestState};
use crate::character::CharacterContext;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RequestTransition {
    pub revision: u64,
    pub phase: EffectPhase,
    pub state: RequestState,
}
#[derive(Debug, Clone)]
pub struct RequestTrace {
    pub metadata: RequestMetadata,
    pub guidance: Arc<str>,
    pub preset_description: String,
    pub context: Option<CharacterContext>,
    pub generated_text: Option<Arc<str>>,
    pub transitions: Vec<RequestTransition>,
}
