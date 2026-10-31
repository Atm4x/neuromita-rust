use crate::{
    character::{
        CharacterError, CharacterHandle, CharacterId, CharacterRegistry, CharacterState,
        CharacterView,
    },
    request::{InputError, UserInput},
    timeline::{ContextAnchor, EventId},
};

#[derive(Clone)]
pub struct CharactersPresenter {
    registry: CharacterRegistry,
}
impl CharactersPresenter {
    pub(crate) fn new(registry: CharacterRegistry) -> Self {
        Self { registry }
    }
    fn get(&self, id: CharacterId) -> Result<CharacterHandle, CharacterError> {
        self.registry.get(id).ok_or(CharacterError::Unavailable)
    }
    pub fn available(&self) -> &'static [CharacterId] {
        &CharacterId::ALL
    }
    pub async fn state(&self, id: CharacterId) -> Result<CharacterState, CharacterError> {
        self.get(id)?.state().await
    }
    pub async fn context(
        &self,
        id: CharacterId,
        at: ContextAnchor,
    ) -> Result<CharacterView, CharacterError> {
        self.get(id)?.get_context(at).await
    }
    pub async fn observe(
        &self,
        id: CharacterId,
        input: UserInput,
    ) -> Result<EventId, CharacterError> {
        self.get(id)?.observe(input).await
    }
    pub fn input(&self, text: String) -> Result<UserInput, InputError> {
        UserInput::new(text)
    }
}
