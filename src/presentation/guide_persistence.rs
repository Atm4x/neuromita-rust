//! GUI and MCP share the same failed Guide-change retry slot.
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::{onboarding::GuideChange, settings::SettingsApi};

#[derive(Debug, Clone)]
pub enum GuideSaveError {
    Failed(String),
    Rejected(String),
}
impl std::fmt::Display for GuideSaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(message) | Self::Rejected(message) => f.write_str(message),
        }
    }
}

#[derive(Clone, Default)]
pub struct GuidePersistencePresenter {
    failed: Arc<Mutex<Option<GuideChange>>>,
}

impl GuidePersistencePresenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn has_pending(&self) -> bool {
        self.failed.lock().await.is_some()
    }

    pub async fn change(&self, change: GuideChange) -> Result<(), GuideSaveError> {
        let mut pending = self.failed.lock().await;
        if pending.is_some() {
            return Err(GuideSaveError::Failed(
                "A previous Guide change is not persisted; retry it first".into(),
            ));
        }
        change
            .apply()
            .map_err(|e| GuideSaveError::Rejected(e.to_string()))?;
        if let Err(error) = SettingsApi::flush().await {
            *pending = Some(change);
            return Err(GuideSaveError::Failed(error.to_string()));
        }
        if change
            .matches_current()
            .map_err(|e| GuideSaveError::Rejected(e.to_string()))?
        {
            Ok(())
        } else {
            Err(GuideSaveError::Rejected(
                "Guide changed concurrently; reopen to reconcile".into(),
            ))
        }
    }

    pub async fn retry(&self) -> Result<GuideChange, GuideSaveError> {
        let mut pending = self.failed.lock().await;
        let change = pending
            .clone()
            .ok_or_else(|| GuideSaveError::Rejected("No failed Guide change to retry".into()))?;
        SettingsApi::retry_save().map_err(|e| GuideSaveError::Failed(e.to_string()))?;
        SettingsApi::flush()
            .await
            .map_err(|e| GuideSaveError::Failed(e.to_string()))?;
        *pending = None;
        if change
            .matches_current()
            .map_err(|e| GuideSaveError::Rejected(e.to_string()))?
        {
            Ok(change)
        } else {
            Err(GuideSaveError::Rejected(
                "Guide changed concurrently; restart to reconcile".into(),
            ))
        }
    }
}
