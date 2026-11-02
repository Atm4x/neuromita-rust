use crate::timeline::domain::CharacterStateDelta;

/// A typed change proposed by a validated model response. No world mutation
/// occurs while these values are being assembled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ResponseEffect {
    ChangeCharacterState(CharacterStateDelta),
}
