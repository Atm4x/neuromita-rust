use crate::timeline::domain::{CharacterStateDelta, CharacterWorkingState, MemoryCommand};

/// A typed change proposed by a validated model response. No world mutation
/// occurs while these values are being assembled.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseEffect {
    ChangeCharacterState(CharacterStateDelta),
    ReplaceWorkingState(CharacterWorkingState),
    MutateMemory(Vec<MemoryCommand>),
}
