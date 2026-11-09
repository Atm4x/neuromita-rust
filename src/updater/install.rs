//! Native GUI release activation orchestration. No generic archive or cross-product fallback.

use std::{ffi::OsStr, io};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    UpdateError, UpdateStager,
    activation::{ActivationError, SingleFileActivator},
    check::{NativeUpdateResult, assess_native_release, latest_native_release, native_asset_name},
    handoff::{self, HandoffError},
};
use crate::portable::PortableDataRoot;

#[derive(Debug, Error)]
pub enum NativeInstallError {
    #[error("native update may only be prepared by the installed GUI executable")]
    NotInstalledGui,
    #[error("application installation path is invalid")]
    InvalidInstallation,
    #[error("native update I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("native update metadata or staging failed: {0}")]
    Update(#[from] UpdateError),
    #[error("native update preparation failed: {0}")]
    Activation(#[from] ActivationError),
    #[error("native updater helper is unavailable: {0}")]
    Handoff(#[from] HandoffError),
    #[error(
        "updater handoff failed ({handoff}); prepared transaction could not be discarded ({cleanup})"
    )]
    PendingTransaction {
        handoff: HandoffError,
        cleanup: ActivationError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeInstallOutcome {
    NotEligible(NativeUpdateResult),
    Prepared { tag: String, asset_name: String },
}

fn running_gui_activator(
    data_root: &PortableDataRoot,
) -> Result<SingleFileActivator, NativeInstallError> {
    let executable = std::env::current_exe()?;
    let expected_name = if cfg!(windows) {
        "neuromita-gui.exe"
    } else {
        "neuromita-gui"
    };
    if executable.file_name() != Some(OsStr::new(expected_name)) {
        return Err(NativeInstallError::NotInstalledGui);
    }
    let directory = executable
        .parent()
        .ok_or(NativeInstallError::InvalidInstallation)?;
    let activator = SingleFileActivator::new(directory, data_root, expected_name)?;
    Ok(activator)
}

/// Explicit installation request; stages the latest release and prepares a fully verified
/// transaction. Only the GUI event-loop owner may perform the later helper handoff.
pub async fn prepare_latest_native_update(
    data_root: &PortableDataRoot,
    cancel: &CancellationToken,
) -> Result<NativeInstallOutcome, NativeInstallError> {
    let Some(expected_asset) = native_asset_name(std::env::consts::OS, std::env::consts::ARCH)
    else {
        return Ok(NativeInstallOutcome::NotEligible(
            NativeUpdateResult::UnsupportedPlatform,
        ));
    };
    let activator = running_gui_activator(data_root)?;
    handoff::validate_handoff_environment(&activator)?;
    if cancel.is_cancelled() {
        return Err(UpdateError::Cancelled.into());
    }
    let Some(release) = latest_native_release().await? else {
        return Ok(NativeInstallOutcome::NotEligible(
            NativeUpdateResult::NotPublished,
        ));
    };
    let assessment = assess_native_release(env!("CARGO_PKG_VERSION"), expected_asset, &release);
    let NativeUpdateResult::Available { tag, asset_name } = assessment else {
        return Ok(NativeInstallOutcome::NotEligible(assessment));
    };
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name() == expected_asset)
        .ok_or_else(|| UpdateError::Metadata("native asset changed after validation".into()))?;
    let artifact = UpdateStager::new(data_root)?.stage(asset, cancel).await?;
    if cancel.is_cancelled() {
        return Err(UpdateError::Cancelled.into());
    }
    activator.prepare_native_gui(asset, &artifact)?;
    Ok(NativeInstallOutcome::Prepared { tag, asset_name })
}

/// Called only on the real GUI thread after preparation and explicit user consent.
/// The helper waits for the exact GUI process identity to exit before replacing its EXE.
pub fn schedule_prepared_native_update(
    data_root: &PortableDataRoot,
) -> Result<u32, NativeInstallError> {
    let activator = running_gui_activator(data_root)?;
    match handoff::schedule_after_shutdown(&activator) {
        Ok(pid) => Ok(pid),
        Err(handoff) => match activator.recover() {
            Ok(super::activation::RecoveryOutcome::NoTransaction) => Err(handoff.into()),
            Ok(super::activation::RecoveryOutcome::Prepared) => {
                match activator.discard_prepared() {
                    Ok(()) => Err(handoff.into()),
                    Err(cleanup) => {
                        Err(NativeInstallError::PendingTransaction { handoff, cleanup })
                    }
                }
            }
            Ok(_) => Err(NativeInstallError::PendingTransaction {
                handoff,
                cleanup: ActivationError::ExistingTransaction,
            }),
            Err(cleanup) => Err(NativeInstallError::PendingTransaction { handoff, cleanup }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::{CheckedRelease, UpdateAsset};

    #[test]
    fn release_without_native_asset_cannot_be_prepared() {
        let release = CheckedRelease {
            tag: "v2.0.0".into(),
            assets: vec![
                UpdateAsset::new(
                    "UnityBuild.zip",
                    "https://example.org/unity.zip",
                    1024,
                    &format!("sha256:{}", "a".repeat(64)),
                )
                .unwrap(),
            ],
        };
        let result = assess_native_release(
            "0.1.0",
            native_asset_name("windows", "x86_64").unwrap(),
            &release,
        );
        assert!(matches!(
            result,
            NativeUpdateResult::NoCompatibleArtifact { .. }
        ));
    }

    #[test]
    fn installer_never_prepares_a_different_running_binary() {
        let temp = std::env::temp_dir();
        let root = PortableDataRoot::resolve(Some(&temp)).unwrap();
        assert!(matches!(
            running_gui_activator(&root),
            Err(NativeInstallError::NotInstalledGui)
        ));
    }
}
