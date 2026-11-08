//! Read-only check for an update of the Rust desktop executable.
//! Python/Unity release feeds are deliberately not considered native application releases.

use reqwest::StatusCode;

use super::{CheckedRelease, GithubUpdateCatalog, UpdateError};

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

pub async fn check_native_update() -> Result<NativeUpdateResult, UpdateError> {
    let Some(expected_asset) = native_asset_name(std::env::consts::OS, std::env::consts::ARCH)
    else {
        return Ok(NativeUpdateResult::UnsupportedPlatform);
    };
    let catalog = GithubUpdateCatalog::new(NATIVE_REPOSITORY)?;
    match catalog.latest_release().await {
        Ok(release) => Ok(assess_native_release(
            env!("CARGO_PKG_VERSION"),
            expected_asset,
            &release,
        )),
        Err(UpdateError::Network(error)) if error.status() == Some(StatusCode::NOT_FOUND) => {
            Ok(NativeUpdateResult::NotPublished)
        }
        Err(error) => Err(error),
    }
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
