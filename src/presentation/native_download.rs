//! Pure transfer telemetry for UI. Bytes received do not indicate package admission.

use crate::ai_runtime::native_https::{NativeTransferPhase, NativeTransferProgress};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeDownloadView {
    pub phase: NativeTransferPhase,
    pub percent: Option<u8>,
    pub transferred_bytes: u64,
    pub planned_bytes: Option<u64>,
}

pub struct NativeDownloadPresenter;

impl NativeDownloadPresenter {
    pub fn project(progress: NativeTransferProgress) -> NativeDownloadView {
        let percent = match progress.phase {
            NativeTransferPhase::Manifest => None,
            NativeTransferPhase::Downloading => progress
                .planned_bytes
                .filter(|total| *total > 0)
                .map(|total| {
                    (progress.transferred_bytes.saturating_mul(100) / total).min(99) as u8
                }),
            NativeTransferPhase::Publishing => Some(99),
        };
        NativeDownloadView {
            phase: progress.phase,
            percent,
            transferred_bytes: progress.transferred_bytes,
            planned_bytes: progress.planned_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_never_claims_known_progress() {
        let view = NativeDownloadPresenter::project(NativeTransferProgress {
            phase: NativeTransferPhase::Manifest,
            transferred_bytes: 0,
            planned_bytes: None,
        });
        assert_eq!(view.percent, None);
    }

    #[test]
    fn fully_received_bytes_do_not_claim_installation() {
        for phase in [
            NativeTransferPhase::Downloading,
            NativeTransferPhase::Publishing,
        ] {
            let view = NativeDownloadPresenter::project(NativeTransferProgress {
                phase,
                transferred_bytes: 12,
                planned_bytes: Some(12),
            });
            assert_eq!(view.percent, Some(99));
        }
    }

    #[test]
    fn bounded_partial_progress_is_derived_without_rounding_to_ready() {
        let view = NativeDownloadPresenter::project(NativeTransferProgress {
            phase: NativeTransferPhase::Downloading,
            transferred_bytes: 50,
            planned_bytes: Some(200),
        });
        assert_eq!(view.percent, Some(25));
    }
}
