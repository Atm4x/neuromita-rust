use std::{fs, path::Path, process::Command};

use ring::digest::{SHA256, digest};
use serde_json::json;
use uuid::Uuid;

fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn stopped_installation_recovers_terminated_helper_transaction() {
    let root = std::env::temp_dir().join(format!("nm-offline-recovery-{}", Uuid::new_v4()));
    let install = root.join("app");
    let workspace = install.join(".neuromita-update");
    let data = install.join("UserData");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("settings.json"), b"original user settings").unwrap();

    let gui_name = if cfg!(windows) {
        "neuromita-gui.exe"
    } else {
        "neuromita-gui"
    };
    let helper_name = if cfg!(windows) {
        "neuromita-updater.exe"
    } else {
        "neuromita-updater"
    };
    let installed = install.join(gui_name);
    let old = b"verified previous executable";

    #[cfg(target_os = "linux")]
    let new = fs::read("/bin/sleep").unwrap();
    #[cfg(not(target_os = "linux"))]
    let new = b"verified next executable".to_vec();

    fs::write(&installed, &new).unwrap();
    #[cfg(target_os = "linux")]
    fs::set_permissions(
        &installed,
        fs::metadata("/bin/sleep").unwrap().permissions(),
    )
    .unwrap();
    fs::write(workspace.join("backup"), old).unwrap();
    fs::write(
        workspace.join("activation.json"),
        serde_json::to_vec(&json!({
            "target_name": gui_name,
            "previous_size": old.len(),
            "previous_sha256": hash(old),
            "next_size": new.len(),
            "next_sha256": hash(&new),
            "health_nonce": Uuid::new_v4(),
        }))
        .unwrap(),
    )
    .unwrap();
    let helper = install.join(helper_name);
    fs::copy(env!("CARGO_BIN_EXE_neuromita-updater"), &helper).unwrap();

    #[cfg(target_os = "linux")]
    {
        let mut child = Command::new(&installed).arg("4").spawn().unwrap();
        let denied = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !denied.status.success(),
            "must not replace a live executable"
        );
        assert!(Path::new(&workspace.join("backup")).exists());
        assert_eq!(fs::read(&installed).unwrap(), new);
        fs::rename(&installed, workspace.join("candidate")).unwrap();
        let denied_during_rename = Command::new(&helper)
            .arg("--recover-interrupted-update")
            .output()
            .unwrap();
        assert!(
            !denied_during_rename.status.success(),
            "a running but renamed candidate must not be deleted"
        );
        assert!(workspace.join("backup").exists());
        assert!(!installed.exists());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    let result = Command::new(&helper)
        .arg("--recover-interrupted-update")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(&installed).unwrap(), old);
    assert_eq!(
        fs::read(data.join("settings.json")).unwrap(),
        b"original user settings"
    );
    assert!(!workspace.exists());
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(data.join("updates/last-activation.json")).unwrap())
            .unwrap();
    assert_eq!(report["state"], "interrupted-update-restored");
    let second = Command::new(&helper)
        .arg("--recover-interrupted-update")
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "already recovered update must not be replayed"
    );
    assert_eq!(fs::read(&installed).unwrap(), old);
    fs::remove_dir_all(root).unwrap();
}
