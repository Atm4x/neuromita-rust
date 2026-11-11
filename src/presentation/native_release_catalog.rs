//! Pure projection of publisher-authorized releases onto installed revision metadata.
//! A signed approval is not an installation receipt or a live inference session.

use crate::ai_runtime::{
    native_revisions::NativeRevisionOverview, native_trust::NativeAuthorizedRelease,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeReleaseRow {
    pub model_id: String,
    pub publisher_id: String,
    pub manifest_sha256: String,
    pub installed: bool,
    pub display_name: Option<String>,
    pub package_version: Option<String>,
    pub speech_services: Vec<crate::ai_runtime::native_speech::NativeSpeechKind>,
}

pub struct NativeReleaseCatalogPresenter;

impl NativeReleaseCatalogPresenter {
    pub fn project(
        releases: Vec<NativeAuthorizedRelease>,
        installed: &NativeRevisionOverview,
    ) -> Vec<NativeReleaseRow> {
        releases
            .into_iter()
            .map(|release| {
                let revision = installed.revisions.iter().find(|revision| {
                    revision.model_id == release.model_id
                        && revision.manifest_sha256 == release.manifest_sha256
                });
                NativeReleaseRow {
                    model_id: release.model_id,
                    publisher_id: release.publisher_id,
                    manifest_sha256: release
                        .manifest_sha256
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                    installed: revision.is_some(),
                    display_name: revision.map(|revision| revision.display_name.clone()),
                    package_version: revision.map(|revision| revision.package_version.clone()),
                    speech_services: revision
                        .map_or_else(Vec::new, |revision| revision.speech_services.clone()),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_runtime::native_revisions::NativeRevisionMetadata;

    #[test]
    fn only_matching_exact_model_and_digest_is_installed() {
        let approved = vec![
            NativeAuthorizedRelease {
                model_id: "voice".into(),
                manifest_sha256: [1; 32],
                publisher_id: "author".into(),
            },
            NativeAuthorizedRelease {
                model_id: "voice".into(),
                manifest_sha256: [2; 32],
                publisher_id: "author".into(),
            },
            NativeAuthorizedRelease {
                model_id: "other".into(),
                manifest_sha256: [1; 32],
                publisher_id: "author".into(),
            },
        ];
        let inventory = NativeRevisionOverview {
            revisions: vec![NativeRevisionMetadata {
                model_id: "voice".into(),
                display_name: "Voice v1".into(),
                package_version: "1".into(),
                manifest_sha256: [1; 32],
                backends: vec![],
                speech_services: vec![],
            }],
            selections: vec![],
        };
        let rows = NativeReleaseCatalogPresenter::project(approved, &inventory);
        assert!(rows[0].installed);
        assert_eq!(rows[0].display_name.as_deref(), Some("Voice v1"));
        assert!(!rows[1].installed);
        assert!(!rows[2].installed);
        assert!(rows[1].display_name.is_none());
    }
}
