//! Application-owned speech transcript composition and submission.
//! Slint and MCP are views/controllers of the same Presenter, not event owners.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    audio::transcript_bridge::{RoutedTranscript, SpeechTranscriptHub},
    character::CharacterId,
    request::RequestHandle,
};

use super::{ChatPresenter, ChatSelection, ChatSelectionPresenter, SettingsPresenter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechDraftSnapshot {
    pub selection: ChatSelection,
    pub draft: String,
    pub revision: u64,
}

#[derive(Default)]
struct DraftState {
    per_character: HashMap<CharacterId, String>,
    revision: u64,
}

#[derive(Clone)]
pub struct SpeechChatPresenter {
    state: Arc<Mutex<DraftState>>,
    updates: watch::Sender<SpeechDraftSnapshot>,
    selection: Arc<ChatSelectionPresenter>,
    chat: ChatPresenter,
    settings: Arc<SettingsPresenter>,
    stop: CancellationToken,
}

impl SpeechChatPresenter {
    pub(crate) fn new(
        chat: ChatPresenter,
        selection: Arc<ChatSelectionPresenter>,
        settings: Arc<SettingsPresenter>,
        transcripts: Arc<SpeechTranscriptHub>,
        stop: CancellationToken,
    ) -> Self {
        let (updates, _) = watch::channel(SpeechDraftSnapshot {
            selection: selection.snapshot(),
            draft: String::new(),
            revision: 0,
        });
        let presenter = Self {
            state: Arc::new(Mutex::new(DraftState::default())),
            updates,
            selection: selection.clone(),
            chat,
            settings,
            stop: stop.clone(),
        };
        let mut incoming = transcripts.subscribe();
        let mut selected = selection.observe();
        let worker = presenter.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = stop.cancelled() => break,
                    event = incoming.recv() => match event {
                        Ok(transcript) => worker.ingest(transcript).await,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(dropped=n, "speech transcript receiver lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    changed = selected.changed() => {
                        if changed.is_err() { break; }
                        selected.borrow_and_update();
                        worker.publish_selected();
                    },
                }
            }
        });
        presenter
    }

    pub fn snapshot(&self) -> SpeechDraftSnapshot {
        self.updates.borrow().clone()
    }

    pub fn observe(&self) -> watch::Receiver<SpeechDraftSnapshot> {
        self.updates.subscribe()
    }

    fn publish_locked(&self, state: &DraftState, selection: ChatSelection) {
        self.updates.send_replace(SpeechDraftSnapshot {
            selection,
            draft: state
                .per_character
                .get(&selection.character)
                .cloned()
                .unwrap_or_default(),
            revision: state.revision,
        });
    }

    fn publish_selected(&self) {
        let state = self.state.lock().expect("speech draft mutex poisoned");
        self.publish_locked(&state, self.selection.snapshot());
    }

    pub async fn set_draft(&self, expected: ChatSelection, draft: String) -> Result<(), String> {
        if draft.len() > 65_536 {
            return Err("chat draft is too long".into());
        }
        self.selection
            .run_fenced(expected, || {
                let mut state = self.state.lock().expect("speech draft mutex poisoned");
                if state
                    .per_character
                    .get(&expected.character)
                    .is_some_and(|old| old == &draft)
                    || (draft.is_empty() && !state.per_character.contains_key(&expected.character))
                {
                    return Ok(());
                }
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or("speech draft revision exhausted")?;
                if draft.is_empty() {
                    state.per_character.remove(&expected.character);
                } else {
                    state.per_character.insert(expected.character, draft);
                }
                self.publish_locked(&state, expected);
                Ok(())
            })
            .await
    }

    pub async fn send(
        &self,
        expected: ChatSelection,
        text: String,
    ) -> Result<RequestHandle, String> {
        self.selection
            .run_fenced(expected, || {
                let handle = self
                    .chat
                    .reply(expected.character, text.clone())
                    .map_err(|error| error.to_string())?;
                let mut state = self.state.lock().expect("speech draft mutex poisoned");
                if state.per_character.get(&expected.character) == Some(&text) {
                    state.per_character.remove(&expected.character);
                    state.revision = state.revision.saturating_add(1);
                    self.publish_locked(&state, expected);
                }
                Ok(handle)
            })
            .await
    }

    pub(crate) async fn ingest(&self, event: RoutedTranscript) {
        let expected = ChatSelection {
            character: event.character,
            revision: event.selection_revision,
        };
        let text = event.transcript.text.trim();
        if text.is_empty() || text.len() > 65_536 || self.stop.is_cancelled() {
            return;
        }
        let preferences = self.settings.asr.preferences.get();
        let outcome = self
            .selection
            .run_fenced(expected, || {
                let mut state = self.state.lock().expect("speech draft mutex poisoned");
                let previous = state
                    .per_character
                    .get(&expected.character)
                    .map(String::as_str)
                    .unwrap_or("");
                let combined = compose(previous, text, preferences.merge_chat);
                if combined.len() > 65_536 {
                    return Err("speech draft length exceeded".to_owned());
                }
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or("speech draft revision exhausted")?;
                state
                    .per_character
                    .insert(expected.character, combined.clone());
                self.publish_locked(&state, expected);
                Ok((combined, state.revision))
            })
            .await;
        let Ok((combined, revision)) = outcome else {
            return;
        };
        if preferences.instant_send {
            let delay = if preferences.delayed_send {
                Duration::from_millis(u64::from(preferences.send_delay_ms))
            } else {
                Duration::ZERO
            };
            let owner = self.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = owner.stop.cancelled() => {},
                    _ = tokio::time::sleep(delay) => owner.send_if_unchanged(expected, revision, combined).await,
                }
            });
        }
    }

    async fn send_if_unchanged(&self, expected: ChatSelection, revision: u64, text: String) {
        let outcome = self
            .selection
            .run_fenced(expected, || {
                let mut state = self.state.lock().expect("speech draft mutex poisoned");
                if state.revision != revision
                    || state.per_character.get(&expected.character) != Some(&text)
                    || self.chat.snapshot().requests.iter().rev().any(|row| {
                        row.metadata.character_id == expected.character
                            && !row.snapshot.state.is_terminal()
                    })
                {
                    return Ok(());
                }
                match self.chat.reply(expected.character, text) {
                    Ok(_) => {
                        state.per_character.remove(&expected.character);
                        state.revision = state.revision.saturating_add(1);
                        self.publish_locked(&state, expected);
                    }
                    Err(error) => tracing::warn!(%error, "automatic ASR chat submission rejected"),
                }
                Ok(())
            })
            .await;
        if let Err(error) = outcome {
            tracing::debug!(%error, "stale ASR transcript rejected");
        }
    }
}

fn compose(previous: &str, recognized: &str, merge: bool) -> String {
    if previous.trim().is_empty() {
        recognized.to_owned()
    } else {
        format!("{previous}{}{recognized}", if merge { " " } else { "\n" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcript_composition_is_not_a_gui_decision() {
        assert_eq!(compose("draft", "voice", true), "draft voice");
        assert_eq!(compose("draft", "voice", false), "draft\nvoice");
        assert_eq!(compose(" ", "voice", true), "voice");
    }
}
