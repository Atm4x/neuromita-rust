use std::{collections::HashMap, num::NonZeroUsize, path::PathBuf, thread};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::{CharacterId, CharacterMemorySnapshot},
    memory::{MemoryCommand, MemoryId, MemoryIsland, MemoryKind, MemoryPriority, MemoryState},
    presentation::CharactersPresenter,
};
use slint::{ModelRc, VecModel, Weak};
use tokio::sync::mpsc;
use uuid::Uuid;

use super::view::{AppWindow, MemoryAction, MemoryItem};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditAction {
    Add,
    Update,
    Forget,
    Restore,
    SoftDelete,
    Protect,
    Unprotect,
    SetIsland,
}

impl EditAction {
    pub fn from_view(action: MemoryAction) -> Option<Self> {
        Some(match action {
            MemoryAction::Add => Self::Add,
            MemoryAction::Update => Self::Update,
            MemoryAction::Forget => Self::Forget,
            MemoryAction::Restore => Self::Restore,
            MemoryAction::SoftDelete => Self::SoftDelete,
            MemoryAction::Protect => Self::Protect,
            MemoryAction::Unprotect => Self::Unprotect,
            MemoryAction::SetIsland => Self::SetIsland,
            MemoryAction::Refresh => return None,
        })
    }

    fn command(
        self,
        id: &str,
        text: String,
        snapshot: Option<&CharacterMemorySnapshot>,
    ) -> Result<MemoryCommand, String> {
        let content = text.trim().to_owned();
        if matches!(self, Self::Add | Self::Update | Self::SetIsland)
            && (content.is_empty() || content.chars().count() > 2000)
        {
            return Err("Текст должен содержать от 1 до 2000 символов".into());
        }
        if self == Self::Add {
            return Ok(MemoryCommand::Add {
                priority: MemoryPriority::Normal,
                content,
            });
        }
        if self == Self::SetIsland {
            let island = match id {
                "relationship" => MemoryIsland::Relationship,
                "opinion" => MemoryIsland::Opinion,
                "preferences" => MemoryIsland::Preferences,
                "commitments_conflicts" => MemoryIsland::CommitmentsConflicts,
                "language" => MemoryIsland::Language,
                _ => return Err("Неизвестная область памяти".into()),
            };
            return Ok(MemoryCommand::SetIsland { island, content });
        }
        let id = MemoryId(Uuid::parse_str(id).map_err(|_| "Некорректный Memory ID")?);
        Ok(match self {
            Self::Update => {
                let kind = snapshot
                    .and_then(|view| view.records().iter().find(|record| record.id == id))
                    .ok_or("Воспоминание не найдено в текущем снимке")?
                    .kind;
                match kind {
                    MemoryKind::Fact => MemoryCommand::Update {
                        id,
                        priority: None,
                        content,
                    },
                    MemoryKind::Island(island) => MemoryCommand::SetIsland { island, content },
                }
            }
            Self::Forget => MemoryCommand::Forget { id },
            Self::Restore => MemoryCommand::Restore { id },
            Self::SoftDelete => MemoryCommand::SoftDelete { id },
            Self::Protect => MemoryCommand::SetProtection {
                id,
                protected: true,
            },
            Self::Unprotect => MemoryCommand::SetProtection {
                id,
                protected: false,
            },
            Self::Add | Self::SetIsland => unreachable!(),
        })
    }
}

pub enum MemoryIntent {
    Load(CharacterId),
    Edit {
        character: CharacterId,
        action: EditAction,
        id: String,
        text: String,
    },
    Quit,
}

pub struct MemoryUi {
    sender: mpsc::UnboundedSender<MemoryIntent>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MemoryUi {
    pub fn new(window: Weak<AppWindow>, data_directory: PathBuf) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let thread = thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    post(
                        &window,
                        None,
                        Vec::new(),
                        false,
                        format!("Ошибка runtime: {error}"),
                        false,
                    );
                    return;
                }
            };
            runtime.block_on(run_memory_owner(window, data_directory, receiver));
        });
        Self {
            sender,
            thread: Some(thread),
        }
    }

    pub fn sender(&self) -> mpsc::UnboundedSender<MemoryIntent> {
        self.sender.clone()
    }
}

impl Drop for MemoryUi {
    fn drop(&mut self) {
        let _ = self.sender.send(MemoryIntent::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn limits() -> RuntimeLimits {
    RuntimeLimits {
        request_queue: NonZeroUsize::new(16).unwrap(),
        discovery_buffer: NonZeroUsize::new(32).unwrap(),
        character_commands: NonZeroUsize::new(32).unwrap(),
        presenter_updates: NonZeroUsize::new(32).unwrap(),
        retained_finished: NonZeroUsize::new(8).unwrap(),
    }
}

async fn run_memory_owner(
    window: Weak<AppWindow>,
    data_directory: PathBuf,
    mut receiver: mpsc::UnboundedReceiver<MemoryIntent>,
) {
    let mut app: Option<ApplicationLifecycle> = None;
    let mut snapshots = HashMap::<CharacterId, CharacterMemorySnapshot>::new();
    while let Some(intent) = receiver.recv().await {
        if matches!(intent, MemoryIntent::Quit) {
            break;
        }
        if app.is_none() {
            let startup = match ApplicationConfig::memory_workspace(limits(), &data_directory) {
                Ok(config) => ApplicationLifecycle::start(config).await,
                Err(error) => Err(error),
            };
            match startup {
                Ok(owner) => app = Some(owner),
                Err(error) => {
                    post(
                        &window,
                        None,
                        Vec::new(),
                        false,
                        format!("Core недоступен: {error}"),
                        false,
                    );
                    continue;
                }
            }
        }
        let presenter = &app.as_ref().expect("initialized").presenter().characters;
        match intent {
            MemoryIntent::Load(character) => {
                load_memory(presenter, &mut snapshots, &window, character, true).await;
            }
            MemoryIntent::Edit {
                character,
                action,
                id,
                text,
            } => {
                let Some(snapshot) = snapshots.get(&character).cloned() else {
                    load_memory(presenter, &mut snapshots, &window, character, true).await;
                    continue;
                };
                let command = match action.command(&id, text, Some(&snapshot)) {
                    Ok(command) => command,
                    Err(error) => {
                        post(
                            &window,
                            Some(character),
                            as_rows(&snapshot),
                            true,
                            error,
                            false,
                        );
                        continue;
                    }
                };
                match presenter
                    .edit_memories(character, snapshot.clone(), vec![command])
                    .await
                {
                    Ok(result) => {
                        let message = if result.applied.is_empty() {
                            "Без изменений".to_owned()
                        } else {
                            "Изменения сохранены в Timeline".to_owned()
                        };
                        post(
                            &window,
                            Some(character),
                            as_rows(&result.snapshot),
                            true,
                            message,
                            true,
                        );
                        snapshots.insert(character, result.snapshot);
                    }
                    Err(error) => {
                        let message = format!("Изменение отклонено: {error}. Обновите данные.");
                        match presenter.memories(character).await {
                            Ok(current) => {
                                post(
                                    &window,
                                    Some(character),
                                    as_rows(&current),
                                    true,
                                    message,
                                    false,
                                );
                                snapshots.insert(character, current);
                            }
                            Err(read_error) => {
                                post(
                                    &window,
                                    Some(character),
                                    as_rows(&snapshot),
                                    false,
                                    format!("{message} Ошибка чтения: {read_error}"),
                                    false,
                                );
                            }
                        }
                    }
                }
            }
            MemoryIntent::Quit => break,
        }
    }
    if let Some(owner) = app
        && let Err(error) = owner.shutdown().await
    {
        tracing::warn!(%error, "memory GUI shutdown failed");
    }
}

async fn load_memory(
    presenter: &CharactersPresenter,
    snapshots: &mut HashMap<CharacterId, CharacterMemorySnapshot>,
    window: &Weak<AppWindow>,
    character: CharacterId,
    reset_editor: bool,
) {
    match presenter.memories(character).await {
        Ok(snapshot) => {
            let count = snapshot.records().len();
            post(
                window,
                Some(character),
                as_rows(&snapshot),
                true,
                format!("Загружено воспоминаний: {count}"),
                reset_editor,
            );
            snapshots.insert(character, snapshot);
        }
        Err(error) => post(
            window,
            Some(character),
            Vec::new(),
            false,
            format!("Ошибка чтения: {error}"),
            false,
        ),
    }
}

#[derive(Clone)]
struct Row {
    id: String,
    content: String,
    state: String,
    kind: String,
    priority: String,
    protected: bool,
}

fn as_rows(snapshot: &CharacterMemorySnapshot) -> Vec<Row> {
    snapshot
        .records()
        .iter()
        .map(|item| Row {
            id: item.id.0.to_string(),
            content: item.content.clone(),
            state: match item.state {
                MemoryState::Active => "active",
                MemoryState::Forgotten => "forgotten",
                MemoryState::Deleted => "deleted",
            }
            .into(),
            kind: match item.kind {
                MemoryKind::Fact => "fact".to_owned(),
                MemoryKind::Island(kind) => format!("island:{}", kind.as_str()),
            },
            priority: format!("{:?}", item.priority).to_lowercase(),
            protected: item.protected,
        })
        .collect()
}

fn post(
    window: &Weak<AppWindow>,
    character: Option<CharacterId>,
    rows: Vec<Row>,
    ready: bool,
    status: String,
    reset_editor: bool,
) {
    let _ = window.upgrade_in_event_loop(move |window| {
        if character.is_some_and(|character| {
            CharacterId::parse(&window.get_memory_character()) != Some(character)
        }) {
            return;
        }
        let rows = rows
            .into_iter()
            .map(|row| MemoryItem {
                id: row.id.into(),
                content: row.content.into(),
                state: row.state.into(),
                kind: row.kind.into(),
                priority: row.priority.into(),
                protected: row.protected,
            })
            .collect::<Vec<_>>();
        window.set_memory_items(ModelRc::new(VecModel::from(rows)));
        window.set_memory_ready(ready);
        window.set_memory_busy(false);
        window.set_memory_status(status.into());
        if reset_editor {
            window.set_memory_selected_id("".into());
            window.set_memory_selected_state("".into());
            window.set_memory_selected_protected(false);
            window.set_memory_draft("".into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_does_not_allow_an_implicit_hard_delete_or_unbounded_text() {
        assert!(matches!(
            EditAction::SoftDelete.command(&MemoryId::new().0.to_string(), String::new(), None),
            Ok(MemoryCommand::SoftDelete { .. })
        ));
        assert!(EditAction::Add.command("", " ".into(), None).is_err());
        assert!(EditAction::Add.command("", "x".repeat(2001), None).is_err());
        assert!(
            EditAction::Update
                .command("invalid", "text".into(), None)
                .is_err()
        );
        assert!(EditAction::from_view(MemoryAction::Refresh).is_none());
    }
}
