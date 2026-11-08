//! Read-only check for an update of the Rust desktop executable.
//! Python/Unity release feeds are deliberately not considered native application releases.

use reqwest::StatusCode;
use tokio_util::sync::CancellationToken;

use crate::portable::PortableDataRoot;

use super::{CheckedRelease, GithubUpdateCatalog, UpdateError, UpdateStager};

pub const NATIVE_REPOSITORY: &str = "Atm4x/neuromita-rust";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeUpdateResult {
    NotPublished,
    UnsupportedPlatform,
    NoCompatibleArtifact { tag: String },
    UncomparableVersion { tag: String },
    Current { tag: String },
    OlderPublished { tag: String },
    Available { tag: String, asset_name: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeDownloadResult {
    NotEligible(NativeUpdateResult),
    Verified {
        tag: String,
        asset_name: String,
        sha256: String,
        size: u64,
        reused: bool,
    },
}

pub(crate) async fn latest_native_release() -> Result<Option<CheckedRelease>, UpdateError> {
    let catalog = GithubUpdateCatalog::new(NATIVE_REPOSITORY)?;
    match catalog.latest_release().await {
        Ok(release) => Ok(Some(release)),
        Err(UpdateError::Network(error)) if error.status() == Some(StatusCode::NOT_FOUND) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub async fn download_native_update(
    data_root: &PortableDataRoot,
    cancel: &CancellationToken,
) -> Result<NativeDownloadResult, UpdateError> {
    let Some(expected_asset) = native_asset_name(std::env::consts::OS, std::env::consts::ARCH)
    else {
        return Ok(NativeDownloadResult::NotEligible(
            NativeUpdateResult::UnsupportedPlatform,
        ));
    };
    if cancel.is_cancelled() {
        return Err(UpdateError::Cancelled);
    }
    let Some(release) = latest_native_release().await? else {
        return Ok(NativeDownloadResult::NotEligible(
            NativeUpdateResult::NotPublished,
        ));
    };
    stage_assessed_release(
        data_root,
        cancel,
        env!("CARGO_PKG_VERSION"),
        expected_asset,
        release,
    )
    .await
}

async fn stage_assessed_release(
    data_root: &PortableDataRoot,
    cancel: &CancellationToken,
    installed_version: &str,
    expected_asset: &str,
    release: CheckedRelease,
) -> Result<NativeDownloadResult, UpdateError> {
    let assessment = assess_native_release(installed_version, expected_asset, &release);
    let NativeUpdateResult::Available { tag, asset_name } = assessment else {
        return Ok(NativeDownloadResult::NotEligible(assessment));
    };
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name() == expected_asset)
        .ok_or_else(|| UpdateError::Metadata("eligible native asset disappeared".into()))?;
    let staged = UpdateStager::new(data_root)?.stage(asset, cancel).await?;
    Ok(NativeDownloadResult::Verified {
        tag,
        asset_name,
        sha256: staged.sha256().to_owned(),
        size: staged.size(),
        reused: staged.reused(),
    })
}

pub async fn check_native_update() -> Result<NativeUpdateResult, UpdateError> {
    let Some(expected_asset) = native_asset_name(std::env::consts::OS, std::env::consts::ARCH)
    else {
        return Ok(NativeUpdateResult::UnsupportedPlatform);
    };
    Ok(match latest_native_release().await? {
        Some(release) => assess_native_release(env!("CARGO_PKG_VERSION"), expected_asset, &release),
        None => NativeUpdateResult::NotPublished,
    })
}

pub fn native_asset_name(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("neuromita-gui-windows-x86_64.exe"),
        ("linux", "x86_64") => Some("neuromita-gui-linux-x86_64"),
        ("linux", "aarch64") => Some("neuromita-gui-linux-aarch64"),
        _ => None,
    }
}

pub fn assess_native_release(
    installed_version: &str,
    expected_asset: &str,
    release: &CheckedRelease,
) -> NativeUpdateResult {
    let tag = release.tag.clone();
    let Some(asset) = release
        .assets
        .iter()
        .find(|asset| asset.name() == expected_asset)
    else {
        return NativeUpdateResult::NoCompatibleArtifact { tag };
    };
    let (Some(installed), Some(published)) = (
        parse_simple_version(installed_version),
        parse_simple_version(&release.tag),
    ) else {
        return NativeUpdateResult::UncomparableVersion { tag };
    };
    use std::cmp::Ordering;
    match published.cmp(&installed) {
        Ordering::Less => NativeUpdateResult::OlderPublished { tag },
        Ordering::Equal => NativeUpdateResult::Current { tag },
        Ordering::Greater => NativeUpdateResult::Available {
            tag,
            asset_name: asset.name().to_owned(),
        },
    }
}

fn parse_simple_version(raw: &str) -> Option<(u64, u64, u64)> {
    let raw = raw.strip_prefix('v').unwrap_or(raw);
    let mut parts = raw.split('.');
    let parse = |part: &str| {
        (!part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0')))
        .then(|| part.parse::<u64>().ok())
        .flatten()
    };
    let version = (
        parse(parts.next()?)?,
        parse(parts.next()?)?,
        parse(parts.next()?)?,
    );
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::UpdateAsset;

    fn fixture(tag: &str, asset: &str) -> CheckedRelease {
        CheckedRelease {
            tag: tag.into(),
            assets: vec![
                UpdateAsset::new(
                    asset,
                    "https://example.com/asset",
                    128,
                    &format!("sha256:{}", "a".repeat(64)),
                )
                .unwrap(),
            ],
        }
    }

    #[test]
    fn native_release_only_promotes_exact_publisher_verified_platform_asset() {
        let target = native_asset_name("windows", "x86_64").unwrap();
        assert_eq!(
            assess_native_release("0.1.0", target, &fixture("v0.2.0", "PythonBuild.zip")),
            NativeUpdateResult::NoCompatibleArtifact {
                tag: "v0.2.0".into()
            }
        );
        assert_eq!(
            assess_native_release("0.1.0", target, &fixture("v0.2.0", "neuromita-gui.exe")),
            NativeUpdateResult::NoCompatibleArtifact {
                tag: "v0.2.0".into()
            }
        );
        assert_eq!(
            assess_native_release("0.1.0", target, &fixture("v0.2.0", target)),
            NativeUpdateResult::Available {
                tag: "v0.2.0".into(),
                asset_name: target.into(),
            }
        );
    }

    #[tokio::test]
    async fn native_download_verifies_publisher_digest_and_revalidates_cached_bytes() {
        use ring::digest::{SHA256, digest};
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };

        let payload = b"genuine-native-binary";
        let address = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/native", address.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = address.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(payload).await.unwrap();
        });
        let directory =
            std::env::temp_dir().join(format!("nm-native-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let data_root = PortableDataRoot::resolve(Some(&directory)).unwrap();
        let settings = directory.join("settings.json");
        std::fs::write(&settings, b"existing-private-settings").unwrap();
        let hash = digest(&SHA256, payload);
        let sha256 = hash
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let asset_name = native_asset_name("windows", "x86_64").unwrap();
        let release = CheckedRelease {
            tag: "v9.1.0".into(),
            assets: vec![
                UpdateAsset::new(
                    asset_name,
                    &url,
                    payload.len() as u64,
                    &format!("sha256:{sha256}"),
                )
                .unwrap(),
            ],
        };
        let token = CancellationToken::new();
        let first =
            stage_assessed_release(&data_root, &token, "0.1.0", asset_name, release.clone())
                .await
                .unwrap();
        assert!(matches!(
            first,
            NativeDownloadResult::Verified { reused: false, .. }
        ));
        let second =
            stage_assessed_release(&data_root, &token, "0.1.0", asset_name, release.clone())
                .await
                .unwrap();
        assert!(matches!(
            second,
            NativeDownloadResult::Verified { reused: true, .. }
        ));
        let staged = directory
            .join("updates/staged")
            .join(format!("sha256-{sha256}.artifact"));
        assert_eq!(std::fs::read(&staged).unwrap(), payload);
        std::fs::write(&staged, b"tampered-cache").unwrap();
        assert!(matches!(
            stage_assessed_release(&data_root, &token, "0.1.0", asset_name, release).await,
            Err(UpdateError::CorruptStage(_))
        ));
        assert_eq!(
            std::fs::read(&settings).unwrap(),
            b"existing-private-settings"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn native_download_refuses_old_release_without_creating_stage() {
        let directory =
            std::env::temp_dir().join(format!("nm-native-old-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let data_root = PortableDataRoot::resolve(Some(&directory)).unwrap();
        let asset_name = native_asset_name("windows", "x86_64").unwrap();
        let result = stage_assessed_release(
            &data_root,
            &CancellationToken::new(),
            "5.0.0",
            asset_name,
            fixture("v4.9.0", asset_name),
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            NativeDownloadResult::NotEligible(NativeUpdateResult::OlderPublished {
                tag: "v4.9.0".into()
            })
        );
        assert!(!directory.join("updates").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_versions_never_treat_unknown_or_old_release_as_newer() {
        let target = native_asset_name("windows", "x86_64").unwrap();
        for (tag, expected) in [
            (
                "v0.1.0",
                NativeUpdateResult::Current {
                    tag: "v0.1.0".into(),
                },
            ),
            (
                "v0.0.9",
                NativeUpdateResult::OlderPublished {
                    tag: "v0.0.9".into(),
                },
            ),
            (
                "v0.2.0-rc1",
                NativeUpdateResult::UncomparableVersion {
                    tag: "v0.2.0-rc1".into(),
                },
            ),
            (
                "v18446744073709551616.0.0",
                NativeUpdateResult::UncomparableVersion {
                    tag: "v18446744073709551616.0.0".into(),
                },
            ),
        ] {
            assert_eq!(
                assess_native_release("0.1.0", target, &fixture(tag, target)),
                expected
            );
        }
        assert_eq!(parse_simple_version("v01.2.3"), None);
        assert_eq!(parse_simple_version("v1.2.3.4"), None);
        assert_eq!(native_asset_name("windows", "aarch64"), None);
    }
}
