//! Application-owned, revision-fenced chat selection. All GUI and MCP clients
//! observe and change this state only through the same Presenter API.
use tokio::sync::{Mutex, watch};

use crate::{app::SharedSpeechCapture, character::CharacterId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ChatSelection {
    pub character: CharacterId,
    pub revision: u64,
}

pub struct ChatSelectionPresenter {
    value: watch::Sender<ChatSelection>,
    mutation: Mutex<()>,
    speech_capture: SharedSpeechCapture,
}

impl ChatSelectionPresenter {
    pub(crate) fn new(speech_capture: SharedSpeechCapture) -> Self {
        let selected = ChatSelection {
            character: CharacterId::Crazy,
            revision: 0,
        };
        let (value, _) = watch::channel(selected);
        Self {
            value,
            mutation: Mutex::new(()),
            speech_capture,
        }
    }

    pub fn snapshot(&self) -> ChatSelection {
        *self.value.borrow()
    }

    pub fn observe(&self) -> watch::Receiver<ChatSelection> {
        self.value.subscribe()
    }

    /// A capture request is fenced against the same Presenter selection used by GUI and MCP.
    /// Hold the selection mutex across capture start so a concurrent select cannot
    /// race a stale ASR start after it has stopped the previous character.
    pub async fn start_capture(
        &self,
        expected: ChatSelection,
        ffmpeg: std::path::PathBuf,
        ffplay: std::path::PathBuf,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().await;
        if self.snapshot() != expected {
            return Err("chat selection revision conflict".into());
        }
        self.speech_capture
            .start(expected.character, expected.revision, ffmpeg, ffplay)
            .await
    }

    pub async fn stop_capture(&self) {
        let _mutation = self.mutation.lock().await;
        self.speech_capture.stop().await;
    }

    pub async fn select(
        &self,
        expected: ChatSelection,
        character: CharacterId,
    ) -> Result<ChatSelection, String> {
        let _mutation = self.mutation.lock().await;
        let previous = self.snapshot();
        if previous != expected {
            return Err("chat selection revision conflict".into());
        }
        if previous.character == character {
            return Ok(previous);
        }
        let revision = previous
            .revision
            .checked_add(1)
            .ok_or("chat selection revision exhausted")?;
        self.speech_capture.stop().await;
        let updated = ChatSelection {
            character,
            revision,
        };
        self.value.send_replace(updated);
        Ok(updated)
    }
}
