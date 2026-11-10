//! Read-only projection of the Core production installation journal.
//! Recorded stage verification never implies a loaded model or a ready worker.

use std::path::PathBuf;

use crate::ai_runtime::install_operation::{
    InstallOperation, InstallOperationError, OperationReadModel,
};

pub struct AiInstallOperationPresenter {
    ai_data: PathBuf,
}

impl AiInstallOperationPresenter {
    pub fn new(ai_data: PathBuf) -> Self {
        Self { ai_data }
    }

    pub fn read(
        &self,
        private_plan_fingerprint: &str,
    ) -> Result<Option<OperationReadModel>, InstallOperationError> {
        InstallOperation::inspect(&self.ai_data, private_plan_fingerprint)
    }

    /// Discover operations by UUID and selected model without exposing plan fingerprints.
    pub fn list(&self) -> Result<Vec<OperationReadModel>, InstallOperationError> {
        InstallOperation::inspect_all(&self.ai_data)
    }
}
