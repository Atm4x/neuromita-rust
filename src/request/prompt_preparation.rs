use std::sync::Arc;

use thiserror::Error;

use crate::{
    character::{CharacterContext, CharacterError, CharacterHandle},
    generation::PreparedPrompt,
    identity::RequestId,
    interaction::TurnIntent,
    prompts::{PostDslProgram, PromptError, PromptSetSnapshot, RenderedPrompt},
    timeline::{ContextPosition, selection::TimelineAdmission},
};

#[derive(Debug, Error)]
pub(crate) enum PromptPreparationError {
    #[error(transparent)]
    Character(#[from] CharacterError),
    #[error(transparent)]
    Catalog(#[from] PromptError),
    #[error("prompt initialization: {0}")]
    Initialization(String),
    #[error("prompt filesystem task: {0}")]
    FileTask(String),
}

pub(crate) struct PreparedCharacterTurn {
    pub context: CharacterContext,
    pub prompt: Option<PreparedPrompt>,
    pub post_dsl: Option<PostDslProgram>,
}

#[derive(Clone, Default)]
pub(crate) struct PromptPreparation;

impl PromptPreparation {
    pub(crate) fn new() -> Self {
        Self
    }

    /// Builds a character-owned context, applies declarative seed commands via
    /// the character's Timeline owner, and renders the provider-neutral prompt.
    /// The catalog never mutates a character or stores canonical memory.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare(
        &self,
        character: &CharacterHandle,
        request: RequestId,
        guidance: Arc<str>,
        intent: TurnIntent,
        at: ContextPosition,
        admission: TimelineAdmission,
        captured: Option<Arc<PromptSetSnapshot>>,
    ) -> Result<PreparedCharacterTurn, PromptPreparationError> {
        let mut context = character
            .prepare(request, Arc::clone(&guidance), intent, at, admission)
            .await?;
        // The selected program was captured at admission. Execution never
        // observes later changes to config, templates, or PostDSL files.
        let Some(captured) = captured else {
            return Ok(PreparedCharacterTurn {
                context,
                prompt: None,
                post_dsl: None,
            });
        };
        let variables = context.variables.clone();
        let template = if matches!(intent, TurnIntent::React(_)) {
            "react_template.txt"
        } else {
            "main_template.txt"
        };
        let post_dsl = captured.post_dsl();
        let rendered = tokio::task::spawn_blocking(move || captured.render(variables, template))
            .await
            .map_err(|error| PromptPreparationError::FileTask(error.to_string()))??;
        if !rendered.initialization.is_empty() {
            if !matches!(at, ContextPosition::Latest) {
                return Err(PromptPreparationError::Initialization(
                    "initialization is forbidden at a historical context".into(),
                ));
            }
            let commands = super::prompt_initialization::commands(&rendered.initialization)
                .map_err(PromptPreparationError::Initialization)?;
            let (updated, _) = character
                .initialize_and_prepare(request, guidance, intent, at, admission, commands)
                .await?;
            context = updated;
        }
        Ok(PreparedCharacterTurn {
            context,
            prompt: Some(convert(rendered)),
            post_dsl,
        })
    }
}

fn convert(rendered: RenderedPrompt) -> PreparedPrompt {
    PreparedPrompt::new(rendered.set_name, rendered.stable, rendered.context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        character::{CharacterId, CharacterRegistry},
        identity::RequestId,
        prompts::PromptCatalog,
        timeline::SessionTimeline,
    };
    use std::{fs, sync::Mutex};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn initializes_through_character_owner_and_renders_from_refreshed_context() {
        let root = std::env::temp_dir().join(format!("neuro-prep-{}", uuid::Uuid::new_v4()));
        let set = root.join("Crazy/Default");
        fs::create_dir_all(&set).unwrap();
        fs::write(
            set.join("config.json"),
            r#"{"variables":{"attitude":5,"player_name":"Guest"}}"#,
        )
        .unwrap();
        fs::write(
            set.join("main_template.txt"),
            "[<identity.txt>]\n[<@state.script>]\n[<init.script>]",
        )
        .unwrap();
        fs::write(set.join("identity.txt"), "Player [{player_name}]").unwrap();
        fs::write(
            set.join("state.script"),
            "IF attitude < 40 THEN\n RETURN 'bad'\nELSE\n RETURN 'good'\nENDIF",
        )
        .unwrap();
        fs::write(
            set.join("init.script"),
            "SEED_MEMORY High | First prompt fact ENTITIES Mita\n",
        )
        .unwrap();

        let timeline = Arc::new(Mutex::new(SessionTimeline::new()));
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(Arc::clone(&timeline), stop.clone(), 8);
        let character = registry.get(CharacterId::Crazy).unwrap();
        let operation = PromptPreparation::new();
        let guidance: Arc<str> = "message from player".into();
        let admission = {
            let timeline = timeline.lock().unwrap();
            timeline
                .admission(CharacterId::Crazy, timeline.selection())
                .unwrap()
        };
        let result = operation
            .prepare(
                &character,
                RequestId::new(),
                Arc::clone(&guidance),
                TurnIntent::Initiate,
                ContextPosition::Latest,
                admission,
                PromptCatalog::new(&root)
                    .capture(CharacterId::Crazy, None)
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            result.context.variables["CURRENT_INPUT"],
            "message from player"
        );
        assert_eq!(result.context.variables["attitude"], 60.0);
        assert!(
            result
                .context
                .memories
                .iter()
                .any(|m| m.content == "First prompt fact")
        );
        assert_eq!(
            result.prompt.as_ref().unwrap().stable.as_ref(),
            &[Arc::from("Player Guest")]
        );
        assert_eq!(
            result.prompt.as_ref().unwrap().dynamic.as_ref(),
            &[Arc::from("good")]
        );
        assert_eq!(
            result.context.prompt.as_ref(),
            crate::character::character_definition(CharacterId::Crazy).prompt
        );
        let conversation =
            crate::generation::LlmConversation::from_context_with_memory(&result.context, &None);
        let segments = conversation.system_segments(&result.context, result.prompt.as_ref());
        assert!(segments.iter().any(|segment| segment == "Player Guest"));
        assert!(segments.iter().any(|segment| segment == "good"));
        assert!(
            !segments
                .iter()
                .any(|segment| segment.contains("Отвечай от лица"))
        );
        let legacy = conversation.system_segments(&result.context, None);
        assert_eq!(legacy.len(), 1);
        assert!(legacy[0].contains(result.context.prompt.as_ref()));

        let admission = {
            let timeline = timeline.lock().unwrap();
            timeline
                .admission(CharacterId::Crazy, timeline.selection())
                .unwrap()
        };
        let second = operation
            .prepare(
                &character,
                RequestId::new(),
                guidance,
                TurnIntent::Initiate,
                ContextPosition::Latest,
                admission,
                PromptCatalog::new(&root)
                    .capture(CharacterId::Crazy, Some("Default"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.context.memories, second.context.memories);
        assert_eq!(result.context.anchor, second.context.anchor);
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn historical_context_cannot_initialize_memory() {
        let root = std::env::temp_dir().join(format!("neuro-prep-{}", uuid::Uuid::new_v4()));
        let set = root.join("Crazy/Default");
        fs::create_dir_all(&set).unwrap();
        fs::write(set.join("main_template.txt"), "[<init.script>]").unwrap();
        fs::write(
            set.join("init.script"),
            "SEED_MEMORY High | Historical seed\n",
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::new()));
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(Arc::clone(&timeline), stop.clone(), 8);
        let character = registry.get(CharacterId::Crazy).unwrap();
        let (admission, at) = {
            let timeline = timeline.lock().unwrap();
            (
                timeline
                    .admission(CharacterId::Crazy, timeline.selection())
                    .unwrap(),
                ContextPosition::At(timeline.anchor()),
            )
        };
        let result = PromptPreparation::new()
            .prepare(
                &character,
                RequestId::new(),
                "".into(),
                TurnIntent::Initiate,
                at,
                admission,
                PromptCatalog::new(&root)
                    .capture(CharacterId::Crazy, None)
                    .unwrap(),
            )
            .await;
        assert!(matches!(
            result,
            Err(PromptPreparationError::Initialization(_))
        ));
        assert!(character.memories().await.unwrap().records().is_empty());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn bad_prompt_does_not_change_character_memory() {
        let root = std::env::temp_dir().join(format!("neuro-prep-{}", uuid::Uuid::new_v4()));
        let set = root.join("Crazy/Default");
        fs::create_dir_all(&set).unwrap();
        fs::write(
            set.join("main_template.txt"),
            "[<init.script>]\n[<missing.txt>]",
        )
        .unwrap();
        fs::write(
            set.join("init.script"),
            "SEED_MEMORY High | Do not commit\n",
        )
        .unwrap();
        let timeline = Arc::new(Mutex::new(SessionTimeline::new()));
        let stop = CancellationToken::new();
        let (registry, tasks) = CharacterRegistry::spawn(Arc::clone(&timeline), stop.clone(), 8);
        let character = registry.get(CharacterId::Crazy).unwrap();
        let admission = {
            let timeline = timeline.lock().unwrap();
            timeline
                .admission(CharacterId::Crazy, timeline.selection())
                .unwrap()
        };
        let result = PromptPreparation::new()
            .prepare(
                &character,
                RequestId::new(),
                "".into(),
                TurnIntent::Initiate,
                ContextPosition::Latest,
                admission,
                PromptCatalog::new(&root)
                    .capture(CharacterId::Crazy, None)
                    .unwrap(),
            )
            .await;
        assert!(matches!(result, Err(PromptPreparationError::Catalog(_))));
        assert!(character.memories().await.unwrap().records().is_empty());
        stop.cancel();
        for task in tasks {
            task.await.unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }
}
