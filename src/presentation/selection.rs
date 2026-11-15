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

    /// Capture admission and push-to-talk are one selection-fenced operation.
    /// The mutex is held until the microphone controller has accepted the entire request.
    pub async fn start_capture(
        &self,
        expected: ChatSelection,
        ffmpeg: std::path::PathBuf,
        ffplay: std::path::PathBuf,
    ) -> Result<(), String> {
        self.start_capture_and_press(expected, ffmpeg, ffplay, false)
            .await
    }

    pub async fn start_capture_and_press(
        &self,
        expected: ChatSelection,
        ffmpeg: std::path::PathBuf,
        ffplay: std::path::PathBuf,
        press_on_start: bool,
    ) -> Result<(), String> {
        let _mutation = self.mutation.lock().await;
        if self.snapshot() != expected {
            return Err("chat selection revision conflict".into());
        }
        self.speech_capture
            .start(expected.character, expected.revision, ffmpeg, ffplay)
            .await?;
        if press_on_start {
            if let Err(error) = self.speech_capture.press(true).await {
                self.speech_capture.stop().await;
                return Err(error);
            }
        }
        Ok(())
    }

    pub async fn press(&self, pressed: bool) -> Result<(), String> {
        let _mutation = self.mutation.lock().await;
        self.speech_capture.press(pressed).await
    }

    pub async fn stop_capture(&self) {
        let _mutation = self.mutation.lock().await;
        self.speech_capture.stop().await;
    }

    /// Execute a synchronous mutation while the selected-character CAS gate is held.
    pub(crate) async fn run_fenced<T>(
        &self,
        expected: ChatSelection,
        action: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _mutation = self.mutation.lock().await;
        if self.snapshot() != expected {
            return Err("chat selection revision conflict".into());
        }
        action()
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
