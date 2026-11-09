use super::*;
use crate::updater::{UpdateAsset, UpdateStager, check::native_asset_name};
use ring::digest::{SHA256, digest};
use std::{
    env,
    ffi::OsString,
    fs::File,
    io::{Read, Seek, SeekFrom},
};
use tokio_util::sync::CancellationToken;

const TEST_KEY: &str = "NM_D56_ISOLATED_REGISTRY_KEY";
const TEST_ARGS: &str = "NM_D56_HELPER_ARGS";
const PARENT: &str = "updater::handoff::windows_acceptance::installed_parent_probe";
const HELPER: &str = "updater::handoff::windows_acceptance::installed_helper_probe";
const UPDATED: &str = "updater::handoff::windows_acceptance::updated_gui_probe";
const RESTORED: &str = "updater::handoff::windows_acceptance::restored_gui_probe";
const GOOD: &[u8] = b"NM_D56_GOOD_000";
const BAD: &[u8] = b"NM_D56_FAIL_000";

struct IsolatedHandoffRuntime<'a> {
    lease: reboot_guard::RecoveryLease<'a>,
}

impl<'a> IsolatedHandoffRuntime<'a> {
    fn new(key: &'a str) -> Self {
        Self {
            lease: reboot_guard::RecoveryLease::isolated(key),
        }
    }
}

fn probe_command(executable: &Path, test: &str) -> Command {
    let mut command = Command::new(executable);
    command.args(["--exact", test, "--ignored", "--nocapture"]);
    command
}

impl HandoffRuntime for IsolatedHandoffRuntime<'_> {
    fn arm(&self, helper: &Path) -> io::Result<bool> {
        self.lease.arm(helper)
    }

    fn disarm(&self, helper: &Path) -> io::Result<()> {
        self.lease.disarm(helper)
    }

    fn is_armed(&self, helper: &Path) -> io::Result<bool> {
        self.lease.is_armed(helper)
    }

    fn helper_command(&self, helper: &Path, pid: u32, identity: u64, nonce: Uuid) -> Command {
        let mut command = probe_command(helper, HELPER);
        command.env(TEST_ARGS, format!("{pid} {identity} {nonce}"));
        command
    }

    fn new_gui_command(&self, executable: &Path) -> Command {
        probe_command(executable, UPDATED)
    }

    fn restored_gui_command(&self, executable: &Path) -> Command {
        probe_command(executable, RESTORED)
    }
}

#[test]
#[ignore = "subprocess role used exclusively by D56 Windows handoff acceptance"]
fn installed_parent_probe() {
    let executable = env::current_exe().unwrap();
    let data_path = executable.parent().unwrap().join("UserData");
    let data = PortableDataRoot::resolve(Some(&data_path)).unwrap();
    let activator =
        SingleFileActivator::new(executable.parent().unwrap(), &data, "neuromita-gui.exe").unwrap();
    let key = env::var(TEST_KEY).unwrap();
    schedule_after_shutdown_with(&activator, &IsolatedHandoffRuntime::new(&key)).unwrap();
}

#[test]
#[ignore = "subprocess role used exclusively by D56 Windows handoff acceptance"]
fn installed_helper_probe() {
    let args = env::var(TEST_ARGS).unwrap();
    let parts: Vec<&str> = args.split(' ').collect();
    assert_eq!(parts.len(), 3);
    let argv = [
        OsString::from(HELPER_COMMAND),
        OsString::from(parts[0]),
        OsString::from(parts[1]),
        OsString::from(parts[2]),
    ];
    let key = env::var(TEST_KEY).unwrap();
    let runtime = IsolatedHandoffRuntime::new(&key);
    run_apply_helper(&argv, &env::current_exe().unwrap(), &runtime).unwrap();
}

#[test]
#[ignore = "subprocess role used exclusively by D56 Windows handoff acceptance"]
fn updated_gui_probe() {
    let mut file = File::open(env::current_exe().unwrap()).unwrap();
    file.seek(SeekFrom::End(-(GOOD.len() as i64))).unwrap();
    let mut footer = vec![0u8; GOOD.len()];
    file.read_exact(&mut footer).unwrap();
    assert_ne!(
        footer, BAD,
        "intentionally failed next GUI before readiness"
    );
    assert_eq!(footer, GOOD, "unrecognized update executable bytes");
    assert!(acknowledge_gui_ready().unwrap());
    fs::write(
        env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("new-gui-ready"),
        b"ready",
    )
    .unwrap();
    thread::sleep(Duration::from_secs(3));
}

#[test]
#[ignore = "subprocess role used exclusively by D56 Windows handoff acceptance"]
fn restored_gui_probe() {
    fs::write(
        env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("old-gui-relaunched"),
        b"restarted",
    )
    .unwrap();
}

fn sha256(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn wait_for_report(data: &Path) -> io::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(35);
    let path = data.join("updates").join(RESULT_NAME);
    loop {
        match fs::read(&path) {
            Ok(bytes) => {
                let report: serde_json::Value = serde_json::from_slice(&bytes)?;
                return report["state"].as_str().map(str::to_owned).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "activation status missing")
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        assert!(
            Instant::now() < deadline,
            "installed helper did not write a report"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn installed_case(success: bool) -> Result<(), Box<dyn std::error::Error>> {
    let root = env::temp_dir().join(format!("nm-d56-native-handoff-{}", Uuid::new_v4()));
    let install = root.join("install");
    let data_path = install.join("UserData");
    fs::create_dir_all(&data_path)?;
    fs::write(
        data_path.join("settings.json"),
        b"original private settings",
    )?;
    let source = env::current_exe()?;
    let mut old = fs::read(&source)?;
    old.extend_from_slice(b"NM_D56_PREVIOUS");
    let mut next = fs::read(&source)?;
    next.extend_from_slice(if success { GOOD } else { BAD });
    let installed_gui = install.join("neuromita-gui.exe");
    let installed_helper = install.join("neuromita-updater.exe");
    fs::write(&installed_gui, &old)?;
    fs::copy(&source, &installed_helper)?;

    let data = PortableDataRoot::resolve(Some(&data_path))?;
    let digest = sha256(&next);
    let asset = UpdateAsset::new(
        native_asset_name("windows", env::consts::ARCH).ok_or("unsupported architecture")?,
        "https://example.invalid/test-only-native-exe",
        next.len() as u64,
        &format!("sha256:{digest}"),
    )?;
    let stage = data_path.join("updates/staged");
    fs::create_dir_all(&stage)?;
    fs::write(stage.join(format!("sha256-{digest}.artifact")), &next)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let artifact =
        runtime.block_on(UpdateStager::new(&data)?.stage(&asset, &CancellationToken::new()))?;
    assert!(artifact.reused());
    let activator = SingleFileActivator::new(&install, &data, "neuromita-gui.exe")?;
    activator.prepare_native_gui(&asset, &artifact)?;
    let subkey = format!("Software\\NeuroMita\\UpdaterLeaseTests\\{}", Uuid::new_v4());
    let lease = IsolatedHandoffRuntime::new(&subkey);
    assert!(!lease.is_armed(&installed_helper)?);

    let parent = probe_command(&installed_gui, PARENT)
        .env(TEST_KEY, &subkey)
        .stdin(Stdio::null())
        .output()?;
    assert!(
        parent.status.success(),
        "installed parent failed: {}",
        String::from_utf8_lossy(&parent.stderr)
    );
    let state = wait_for_report(&data_path)?;
    let expected = if success {
        "updated-and-restarted"
    } else {
        "rolled-back-after-startup-failure"
    };
    assert_eq!(state, expected);
    assert_eq!(
        fs::read(&installed_gui)?.as_slice(),
        if success {
            next.as_slice()
        } else {
            old.as_slice()
        },
    );
    assert_eq!(
        fs::read(data_path.join("settings.json"))?,
        b"original private settings"
    );
    assert!(!install.join(".neuromita-update").exists());
    assert!(!lease.is_armed(&installed_helper)?);
    reboot_guard::remove_isolated_key(&subkey)?;
    let marker = install.join(if success {
        "new-gui-ready"
    } else {
        "old-gui-relaunched"
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            Instant::now() < deadline,
            "new or restored GUI was not observed"
        );
        thread::sleep(Duration::from_millis(50));
    }
    thread::sleep(Duration::from_secs(3));
    for _ in 0..15 {
        match fs::remove_dir_all(&root) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                thread::sleep(Duration::from_millis(200))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err("unable to remove disposable Windows installation after child exit".into())
}

#[test]
fn installed_helper_rejects_missing_isolated_recovery_lease() {
    let install = env::temp_dir().join(format!("nm-d56-guard-refusal-{}", Uuid::new_v4()));
    fs::create_dir_all(&install).unwrap();
    let helper = install.join("neuromita-updater.exe");
    fs::copy(env::current_exe().unwrap(), &helper).unwrap();
    let subkey = format!("Software\\NeuroMita\\UpdaterLeaseTests\\{}", Uuid::new_v4());
    let args = [
        OsString::from(HELPER_COMMAND),
        OsString::from(process::id().wrapping_add(1000).to_string()),
        OsString::from("1234"),
        OsString::from(Uuid::new_v4().to_string()),
    ];
    let error =
        run_apply_helper(&args, &helper, &IsolatedHandoffRuntime::new(&subkey)).unwrap_err();
    assert!(matches!(error, HandoffError::MissingRecoveryGuard));
    fs::remove_dir_all(install).unwrap();
}

#[test]
fn installed_windows_helper_handoff_with_isolated_guard() {
    installed_case(true).unwrap();
    installed_case(false).unwrap();
}
