//! Exact release archive selection. Archive staging never authorizes installation.

use super::{CheckedRelease, UpdateAsset};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveComponent {
    Python,
    Unity,
}

impl ArchiveComponent {
    pub fn from_ui_id(id: &str) -> Option<Self> {
        match id {
            "python" => Some(Self::Python),
            "unity" => Some(Self::Unity),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Python => "Python ZIP",
            Self::Unity => "Unity ZIP",
        }
    }

    pub fn asset_name(self, tag: &str) -> String {
        match self {
            Self::Python => format!("PythonBuild-{tag}.zip"),
            Self::Unity => format!("UnityBuild-{tag}.zip"),
        }
    }
}

/// Select only the exact archive matching the published release tag. The result
/// may be passed to `UpdateStager::stage`, never to `SingleFileActivator`.
pub fn published_archive(
    release: &CheckedRelease,
    component: ArchiveComponent,
) -> Option<&UpdateAsset> {
    if release.tag.is_empty()
        || release.tag.len() > 128
        || !release
            .tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return None;
    }
    let filename = component.asset_name(&release.tag);
    let mut matches = release
        .assets
        .iter()
        .filter(|asset| asset.name() == filename);
    let selected = matches.next()?;
    matches.next().is_none().then_some(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::UpdateAsset;

    fn asset(name: &str) -> UpdateAsset {
        UpdateAsset::new(
            name,
            "https://github.com/Atm4x/NeuroMita/releases/download/v2026.11.01/test.zip",
            40,
            &format!("sha256:{}", "0".repeat(64)),
        )
        .unwrap()
    }

    #[test]
    fn exact_python_and_unity_zip_names_only() {
        let release = CheckedRelease {
            tag: "v2026.11.01".to_owned(),
            assets: vec![
                asset("PythonBuild-v2026.11.01.zip"),
                asset("UnityBuild-v2026.11.01.zip"),
                asset("releases-manifest.json"),
                asset("neuromita-gui.exe"),
            ],
        };
        assert_eq!(
            published_archive(&release, ArchiveComponent::Python)
                .unwrap()
                .name(),
            "PythonBuild-v2026.11.01.zip"
        );
        assert_eq!(
            published_archive(&release, ArchiveComponent::Unity)
                .unwrap()
                .name(),
            "UnityBuild-v2026.11.01.zip"
        );
    }

    #[test]
    fn mismatches_never_select_anything() {
        let release = CheckedRelease {
            tag: "v2026.11.01".into(),
            assets: vec![
                asset("PythonBuild-v2026.11.00.zip"),
                asset("PythonBuild-v2026.11.01.7z"),
                asset("UnityBuild-v2026.11.01.zip.exe"),
            ],
        };
        assert!(published_archive(&release, ArchiveComponent::Python).is_none());
        assert!(published_archive(&release, ArchiveComponent::Unity).is_none());
        assert_eq!(ArchiveComponent::from_ui_id("Python"), None);
        assert_eq!(ArchiveComponent::from_ui_id("rust"), None);
    }

    #[test]
    fn reject_duplicate_asset_and_unsafe_tag() {
        let duplicate = CheckedRelease {
            tag: "v2026.11.01".into(),
            assets: vec![
                asset("PythonBuild-v2026.11.01.zip"),
                asset("PythonBuild-v2026.11.01.zip"),
            ],
        };
        assert!(published_archive(&duplicate, ArchiveComponent::Python).is_none());
        let unsafe_release = CheckedRelease {
            tag: "../v2026.11.01".into(),
            assets: vec![asset("PythonBuild-v2026.11.01.zip")],
        };
        assert!(published_archive(&unsafe_release, ArchiveComponent::Python).is_none());
    }
}
