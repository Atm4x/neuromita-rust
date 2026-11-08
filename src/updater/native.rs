//! Read-only native executable update checks, independent of Unity/Python release feeds.

use reqwest::StatusCode;

use super::{CheckedRelease, GithubUpdateCatalog, UpdateAsset, UpdateError};

pub const NATIVE_RELEASE_REPOSITORY: &str = "Atm4x/neuromita-rust";

#[derive(Debug)]
pub enum NativeUpdateStatus {
    NoPublishedRelease {
        current: String,
    },
    Current {
        current: String,
        latest: String,
    },
    Available {
        current: String,
        latest: String,
        asset: UpdateAsset,
    },
    MissingExecutableAsset {
        current: String,
        latest: String,
        executable: String,
    },
}

pub async fn check_native_update(
    current_version: &str,
    executable_name: &str,
) -> Result<NativeUpdateStatus, UpdateError> {
    parse_version(current_version)?;
    validate_executable_name(executable_name)?;

    let catalog = GithubUpdateCatalog::new(NATIVE_RELEASE_REPOSITORY)?;
    match catalog.latest_release().await {
        Ok(release) => evaluate_release(current_version, executable_name, release),
        Err(UpdateError::Network(error)) if error.status() == Some(StatusCode::NOT_FOUND) => {
            Ok(NativeUpdateStatus::NoPublishedRelease {
                current: current_version.into(),
            })
        }
        Err(error) => Err(error),
    }
}

pub fn evaluate_release(
    current_version: &str,
    executable_name: &str,
    release: CheckedRelease,
) -> Result<NativeUpdateStatus, UpdateError> {
    let current = parse_version(current_version)?;
    let latest = parse_version(&release.tag)?;
    validate_executable_name(executable_name)?;

    if latest <= current {
        return Ok(NativeUpdateStatus::Current {
            current: current_version.into(),
            latest: release.tag,
        });
    }

    match release
        .assets
        .into_iter()
        .find(|asset| asset.name() == executable_name)
    {
        Some(asset) => Ok(NativeUpdateStatus::Available {
            current: current_version.into(),
            latest: release.tag,
            asset,
        }),
        None => Ok(NativeUpdateStatus::MissingExecutableAsset {
            current: current_version.into(),
            latest: release.tag,
            executable: executable_name.into(),
        }),
    }
}

fn validate_executable_name(name: &str) -> Result<(), UpdateError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.len() > 255
    {
        return Err(UpdateError::Metadata(
            "invalid native executable file name".into(),
        ));
    }
    Ok(())
}

fn parse_version(value: &str) -> Result<[u64; 3], UpdateError> {
    let version = value.strip_prefix('v').unwrap_or(value);
    let segments = version.split('.').collect::<Vec<_>>();
    if segments.len() != 3
        || segments.iter().any(|part| {
            part.is_empty()
                || part.len() > 1 && part.starts_with('0')
                || !part.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(UpdateError::Metadata(format!(
            "unsupported native release version: {value}"
        )));
    }
    let parsed = segments
        .into_iter()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| UpdateError::Metadata(format!("invalid native release version: {value}")))?;
    Ok([parsed[0], parsed[1], parsed[2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(name: &str) -> UpdateAsset {
        UpdateAsset::new(
            name,
            "https://github.com/Atm4x/neuromita-rust/releases/download/v0.2.0/neuromita-gui.exe",
            123,
            &format!("sha256:{}", "ab".repeat(32)),
        )
        .unwrap()
    }

    fn release(tag: &str, names: &[&str]) -> CheckedRelease {
        CheckedRelease {
            tag: tag.into(),
            assets: names.iter().map(|name| executable(name)).collect(),
        }
    }

    #[test]
    fn newer_release_with_exact_native_executable_is_available() {
        let status = evaluate_release(
            "0.1.0",
            "neuromita-gui.exe",
            release("v0.2.0", &["NeuroMita-Unity.zip", "neuromita-gui.exe"]),
        )
        .unwrap();
        assert!(
            matches!(status, NativeUpdateStatus::Available { asset, .. } if asset.name() == "neuromita-gui.exe")
        );
    }

    #[test]
    fn python_or_unity_only_release_is_not_executable_update() {
        let status = evaluate_release(
            "0.1.0",
            "neuromita-gui.exe",
            release("v0.2.0", &["NeuroMita-Unity.zip", "NeuroMita.pyz"]),
        )
        .unwrap();
        assert!(matches!(
            status,
            NativeUpdateStatus::MissingExecutableAsset { .. }
        ));
    }

    #[test]
    fn older_and_equal_release_does_not_offer_downgrade() {
        for tag in ["0.1.0", "v0.0.9"] {
            let status = evaluate_release(
                "0.1.0",
                "neuromita-gui.exe",
                release(tag, &["neuromita-gui.exe"]),
            )
            .unwrap();
            assert!(matches!(status, NativeUpdateStatus::Current { .. }));
        }
    }

    #[test]
    fn invalid_versions_and_executable_paths_fail_closed() {
        for version in ["", "v0.1", "v0.2.0-beta", "v0.02.0", "v0.2.0.1"] {
            assert!(evaluate_release("0.1.0", "neuromita-gui.exe", release(version, &[])).is_err());
        }
        for name in ["", "../neuromita-gui.exe", "C:\\game.exe", "."] {
            assert!(evaluate_release("0.1.0", name, release("v0.2.0", &[])).is_err());
        }
    }
}
