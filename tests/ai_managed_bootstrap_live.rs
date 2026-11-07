use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use neuromita::ai_runtime::{
    managed::ManagedRuntimeBootstrap,
    uv::{UvArchiveKind, UvDistribution, UvInstallError, UvInstaller},
};
use ring::digest::{SHA256, digest};
use tokio::process::Command;
use uuid::Uuid;

async fn make_archive(source: &Path, root: &Path) -> (PathBuf, UvArchiveKind) {
    let payload = root.join("payload");
    tokio::fs::create_dir_all(&payload).await.unwrap();
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    tokio::fs::copy(source, payload.join(name)).await.unwrap();

    if cfg!(windows) {
        let archive = root.join("uv.zip");
        let source = payload.join(name).display().to_string().replace('"', "`\"");
        let destination = archive.display().to_string().replace('"', "`\"");
        let command = format!(
            "Compress-Archive -LiteralPath \"{source}\" -DestinationPath \"{destination}\""
        );
        let output = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (archive, UvArchiveKind::Zip)
    } else {
        let archive = root.join("uv.tar.gz");
        let output = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&payload)
            .arg(name)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (archive, UvArchiveKind::TarGz)
    }
}

#[tokio::test]
#[ignore = "requires NEUROMITA_REAL_UV and a network-enabled runner to install actual CPython 3.12.10"]
async fn real_uv_installs_and_reuses_a_verified_managed_cpython() {
    let source = PathBuf::from(
        std::env::var_os("NEUROMITA_REAL_UV").expect("NEUROMITA_REAL_UV is required"),
    );
    assert!(source.is_absolute() && source.is_file());
    let version_output = Command::new(&source)
        .arg("--version")
        .output()
        .await
        .unwrap();
    assert!(version_output.status.success());
    let version_output = String::from_utf8(version_output.stdout).unwrap();
    let mut fields = version_output.split_whitespace();
    assert_eq!(fields.next(), Some("uv"));
    let uv_version = fields.next().unwrap().to_owned();
    assert!(
        uv_version
            .split('.')
            .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()))
    );

    let root = std::env::temp_dir().join(format!("neuromita-real-bootstrap-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(&root).await.unwrap();
    let (archive, archive_kind) = make_archive(&source, &root).await;
    let sha = digest(&SHA256, &tokio::fs::read(&archive).await.unwrap())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    let uv = UvInstaller {
        data_directory: root.join("AIData"),
        distribution: UvDistribution {
            version: uv_version,
            archive_url: format!("file://{}", archive.display()),
            archive_sha256: sha,
            archive_kind,
        },
        timeout: Duration::from_secs(180),
    };
    let bootstrap = ManagedRuntimeBootstrap {
        uv: uv.clone(),
        python_version: "3.12.10".to_owned(),
        python_timeout: Duration::from_secs(240),
    };

    assert!(uv.probe().await.unwrap().is_none());
    let managed = bootstrap.prepare().await.unwrap();
    assert_eq!(managed.uv_executable, uv.uv_executable());
    assert!(
        managed
            .python_executable
            .starts_with(root.join("AIData/bootstrap/python"))
    );
    assert_eq!(
        uv.probe().await.unwrap(),
        Some(managed.uv_executable.clone())
    );

    let python = Command::new(&managed.python_executable)
        .args([
            "-I", "-S", "-c",
            "import json, ssl, sys; print(json.dumps({'version': list(sys.version_info[:3]), 'implementation': sys.implementation.name, 'openssl': ssl.OPENSSL_VERSION}))",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        python.status.success(),
        "{}",
        String::from_utf8_lossy(&python.stderr)
    );
    let actual: serde_json::Value = serde_json::from_slice(&python.stdout).unwrap();
    assert_eq!(actual["version"], serde_json::json!([3, 12, 10]));
    assert_eq!(actual["implementation"], "cpython");
    assert!(actual["openssl"].as_str().unwrap().contains("OpenSSL"));

    assert_eq!(bootstrap.prepare().await.unwrap(), managed);
    assert!(!root.join("AIData/.venv").exists());

    tokio::fs::write(&managed.uv_executable, b"modified uv executable")
        .await
        .unwrap();
    assert!(matches!(uv.probe().await, Err(UvInstallError::Corrupted)));
    tokio::fs::remove_dir_all(&root).await.unwrap();
}
