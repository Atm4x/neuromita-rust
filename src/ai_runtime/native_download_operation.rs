//! Application-owned cancellable native package installation operation.
//! The download task owns its stage through durable publication or cleanup.

use std::{path::PathBuf, time::Duration};

use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    native_https::{
        NativeHttpsError, NativeHttpsOrigin, NativeHttpsPackageRequest, NativeHttpsPackageTransfer,
        NativeTransferProgress,
    },
    native_model::NativeBackend,
    native_package_install::NativePackageInstallResult,
    native_registry::{parse_sha256, valid_identifier},
    native_revisions::NativeRevisionStore,
    native_trust::NativeReleaseTrust,
};

#[derive(Debug, Clone)]
pub struct NativeDownloadSelection {
    pub model_id: String,
    pub manifest_sha256: String,
    pub backend: NativeBackend,
}

#[derive(Debug)]
pub enum NativeDownloadEvent {
    Progress {
        id: Uuid,
        progress: NativeTransferProgress,
    },
    Finished {
        id: Uuid,
        result: Result<NativePackageInstallResult, NativeHttpsError>,
    },
}

impl NativeDownloadEvent {
    pub fn id(&self) -> Uuid {
        match self {
            Self::Progress { id, .. } | Self::Finished { id, .. } => *id,
        }
    }
}

pub struct NativeDownloadOperation {
    id: Uuid,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl NativeDownloadOperation {
    /// The requested digest only identifies a signed release. This method
    /// checks the independent publisher policy before spawning any network work.
    pub fn start(
        store: NativeRevisionStore,
        trust: NativeReleaseTrust,
        origin: NativeHttpsOrigin,
        request: NativeDownloadSelection,
        staging_root: PathBuf,
        file_timeout: Duration,
        events: mpsc::UnboundedSender<NativeDownloadEvent>,
    ) -> Result<Self, NativeHttpsError> {
        if !valid_identifier(&request.model_id) {
            return Err(NativeHttpsError::Invalid("invalid model ID"));
        }
        let digest = parse_sha256(&request.manifest_sha256)?;
        trust.verify(&request.model_id, &digest)?;
        store.verify_download_staging_root(&staging_root)?;
        let transfer = NativeHttpsPackageTransfer::new(file_timeout)?;
        let id = Uuid::new_v4();
        let cancellation = CancellationToken::new();
        let task_cancel = cancellation.clone();
        let task = tokio::spawn(async move {
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();
            let worker = tokio::spawn(async move {
                transfer
                    .download_and_install_observed(
                        &store,
                        &trust,
                        NativeHttpsPackageRequest {
                            model_id: &request.model_id,
                            manifest_sha256: digest,
                            backend: request.backend,
                            origin: &origin,
                        },
                        &staging_root,
                        &task_cancel,
                        &progress_tx,
                    )
                    .await
            });
            while let Some(progress) = progress_rx.recv().await {
                let _ = events.send(NativeDownloadEvent::Progress { id, progress });
            }
            let result = match worker.await {
                Ok(result) => result,
                Err(_) => Err(NativeHttpsError::InstallerTask),
            };
            let _ = events.send(NativeDownloadEvent::Finished { id, result });
        });
        Ok(Self {
            id,
            cancellation,
            task: Some(task),
        })
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Await release of the staging owner before application shutdown. Dropping
    /// the operation alone is not a confirmation that publication has ended.
    pub async fn join(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for NativeDownloadOperation {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unapproved_release_is_denied_before_creating_an_operation() {
        let root = std::env::temp_dir().join(format!("native-operation-{}", Uuid::new_v4()));
        let (events, mut receiver) = mpsc::unbounded_channel();
        let result = NativeDownloadOperation::start(
            NativeRevisionStore::new(root.join("revisions"), root.join("runtime")),
            NativeReleaseTrust::new(root.join("untrusted-policy"), root.join("approvals"), None),
            NativeHttpsOrigin::parse("https://releases.example.org/native/").unwrap(),
            NativeDownloadSelection {
                model_id: "voice".into(),
                manifest_sha256: "aa".repeat(32),
                backend: NativeBackend::Cpu,
            },
            root.join("staging"),
            Duration::from_secs(5),
            events,
        );
        assert!(matches!(result, Err(NativeHttpsError::Trust(_))));
        assert!(receiver.try_recv().is_err());
        assert!(!root.exists());
    }

    #[test]
    fn progress_event_correlation_is_stable() {
        let id = Uuid::new_v4();
        let event = NativeDownloadEvent::Progress {
            id,
            progress: NativeTransferProgress {
                phase: super::super::native_https::NativeTransferPhase::Manifest,
                transferred_bytes: 0,
                planned_bytes: None,
            },
        };
        assert_eq!(event.id(), id);
    }
}
