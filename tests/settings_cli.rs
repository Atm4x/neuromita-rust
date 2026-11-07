use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, SettingsRepository, StoredSettings, Volume,
};
use uuid::Uuid;

const BINARY: &str = env!("CARGO_BIN_EXE_neuromita");

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("neuromita-settings-cli-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn stored(revision: u64, volume: u8) -> StoredSettings {
    let mut settings = AppSettings::initial_profile();
    settings.voice.volume = Volume::new(volume).unwrap();
    StoredSettings { revision, settings }
}

fn run(arguments: &[&str]) -> Output {
    Command::new(BINARY)
        .args(arguments)
        .env_remove("GEMINI_API_KEY")
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY")
        .env_remove("ALL_PROXY")
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn offline_export_and_import_roundtrip_without_api_key() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let backup_path = directory.path("backup.json");
    let restored_path = directory.path("restored.json");
    let settings = stored(5, 33);
    JsonSettingsRepository::new(store_path.clone())
        .save(&settings)
        .unwrap();

    let exported = run(&[
        "settings",
        "export",
        "--source",
        store_path.to_str().unwrap(),
        "--destination",
        backup_path.to_str().unwrap(),
    ]);
    assert!(exported.status.success(), "{}", stderr(&exported));
    assert!(
        stdout(&exported).contains("Exported full Rust AppSettings (revision 5)"),
        "{}",
        stdout(&exported)
    );
    let backup = fs::read(&backup_path).unwrap();

    let exported_again = run(&[
        "settings",
        "export",
        "--source",
        store_path.to_str().unwrap(),
        "--destination",
        backup_path.to_str().unwrap(),
    ]);
    assert!(exported_again.status.success());
    assert_eq!(fs::read(&backup_path).unwrap(), backup);

    let imported = run(&[
        "settings",
        "import",
        "--source",
        backup_path.to_str().unwrap(),
        "--destination",
        restored_path.to_str().unwrap(),
    ]);
    assert!(imported.status.success(), "{}", stderr(&imported));
    assert!(
        stdout(&imported).contains("Imported full Rust AppSettings (revision 5)"),
        "{}",
        stdout(&imported)
    );
    assert_eq!(
        JsonSettingsRepository::new(restored_path).load().unwrap(),
        settings
    );
}

#[test]
fn invalid_cli_import_preserves_the_destination() {
    let directory = TempDir::new();
    let destination_path = directory.path("store.json");
    JsonSettingsRepository::new(destination_path.clone())
        .save(&stored(2, 70))
        .unwrap();
    let before = fs::read(&destination_path).unwrap();

    let invalid_source = directory.path("future.json");
    let mut invalid = serde_json::to_value(stored(9, 10)).unwrap();
    invalid["schema_version"] = serde_json::json!(5);
    fs::write(
        &invalid_source,
        serde_json::to_vec_pretty(&invalid).unwrap(),
    )
    .unwrap();
    let rejected = run(&[
        "settings",
        "import",
        "--source",
        invalid_source.to_str().unwrap(),
        "--destination",
        destination_path.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("invalid settings transfer"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(fs::read(&destination_path).unwrap(), before);

    let partial_source = directory.path("partial.json");
    fs::write(&partial_source, br#"{"volume":70}"#).unwrap();
    let rejected = run(&[
        "settings",
        "import",
        "--source",
        partial_source.to_str().unwrap(),
        "--destination",
        destination_path.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("not a complete envelope"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(fs::read(&destination_path).unwrap(), before);
}

#[test]
fn help_documents_offline_commands_without_api_key() {
    let help = run(&["--help"]);
    assert!(help.status.success(), "{}", stderr(&help));
    let text = stdout(&help);
    assert!(text.contains("neuromita settings export --source"));
    assert!(text.contains("neuromita settings import --source"));
    assert!(text.contains("no GEMINI_API_KEY"));
}

#[test]
fn python_cli_preview_is_read_only_then_import_is_reversible_and_repeated_import_is_noop() {
    let directory = TempDir::new();
    let source = directory.path("python-source.json");
    let target = directory.path("rust-settings.json");
    let starting = stored(22, 90);
    JsonSettingsRepository::new(target.clone())
        .save(&starting)
        .unwrap();
    let original = fs::read(&target).unwrap();
    let python_source = br#"{"LANGUAGE":"EN","HIDE_PRIVATE":false,"VOICEOVER_LOCAL_VOLUME":45,"LAST_API_PRESET_ID":87,"API_KEY":"test-secret-value"}"#;
    fs::write(&source, python_source).unwrap();

    let execute = |extra: &[&str]| {
        Command::new(BINARY)
            .args(["settings", "import", "--format", "python", "--source"])
            .arg(&source)
            .arg("--destination")
            .arg(&target)
            .args(extra)
            .output()
            .unwrap()
    };
    let dry_run = execute(&["--dry-run"]);
    assert!(dry_run.status.success(), "{}", stderr(&dry_run));
    assert!(stdout(&dry_run).contains("no destination or backup files were written"));
    assert!(stdout(&dry_run).contains("LAST_API_PRESET_ID"));
    assert!(!stdout(&dry_run).contains("test-secret-value"));
    assert_eq!(fs::read(&target).unwrap(), original);

    let rejected = execute(&[]);
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("--allow-unmapped"));
    assert_eq!(fs::read(&target).unwrap(), original);

    let unacknowledged = execute(&["--allow-unmapped"]);
    assert!(!unacknowledged.status.success());
    assert_eq!(fs::read(&target).unwrap(), original);

    let partial = execute(&["--allow-unmapped", "--accept-skipped-key", "API_KEY"]);
    assert!(!partial.status.success());
    assert_eq!(fs::read(&target).unwrap(), original);

    let extra = execute(&[
        "--allow-unmapped",
        "--accept-skipped-key",
        "API_KEY",
        "--accept-skipped-key",
        "LAST_API_PRESET_ID",
        "--accept-skipped-key",
        "UNEXPECTED_KEY",
    ]);
    assert!(!extra.status.success());
    assert_eq!(fs::read(&target).unwrap(), original);

    let applied = execute(&[
        "--allow-unmapped",
        "--accept-skipped-key",
        "LAST_API_PRESET_ID",
        "--accept-skipped-key",
        "API_KEY",
    ]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert!(stdout(&applied).contains("Previous Rust settings backup:"));
    assert_eq!(fs::read(&source).unwrap(), python_source);
    let imported = JsonSettingsRepository::new(target.clone()).load().unwrap();
    assert_eq!(imported.revision, starting.revision + 1);
    assert_eq!(imported.settings.voice.volume, Volume::new(45).unwrap());
    assert_eq!(imported.settings.presets, starting.settings.presets);
    assert_eq!(
        fs::read_dir(&directory.path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("pre-python-import"))
            .count(),
        2
    );
    let second = execute(&[
        "--allow-unmapped",
        "--accept-skipped-key",
        "API_KEY",
        "--accept-skipped-key",
        "LAST_API_PRESET_ID",
    ]);
    assert!(second.status.success());
    assert!(stdout(&second).contains("changed=false"));
    assert_eq!(
        JsonSettingsRepository::new(target).load().unwrap(),
        imported
    );
}

#[test]
fn python_cli_never_overwrites_the_python_source_even_via_alias() {
    let directory = TempDir::new();
    let source = directory.path("python.json");
    let original = br#"{"LANGUAGE":"EN"}"#;
    fs::write(&source, original).unwrap();
    let output = run(&[
        "settings",
        "import",
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        source.to_str().unwrap(),
    ]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("must be different files"));
    assert_eq!(fs::read(&source).unwrap(), original);
}

#[test]
fn python_preview_alias_and_source_backup_stay_with_portable_rust_target() {
    let directory = TempDir::new();
    let python_dir = directory.path("python");
    let rust_dir = directory.path("rust");
    fs::create_dir_all(&python_dir).unwrap();
    fs::create_dir_all(&rust_dir).unwrap();
    let source = python_dir.join("settings.json");
    let target = rust_dir.join("settings.json");
    let input = br#"{"LANGUAGE":"EN","REMINDERS_ENABLED":false}"#;
    fs::write(&source, input).unwrap();
    let preview = run(&[
        "settings",
        "import",
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
        "--preview",
    ]);
    assert!(preview.status.success(), "{}", stderr(&preview));
    assert!(!target.exists());
    assert_eq!(fs::read_dir(&python_dir).unwrap().count(), 1);
    let applied = run(&[
        "settings",
        "import",
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
    ]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert_eq!(fs::read_dir(&python_dir).unwrap().count(), 1);
    let source_backup = fs::read_dir(&rust_dir)
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("python-source-settings.json.pre-python-import")
        })
        .unwrap()
        .path();
    assert_eq!(fs::read(&source_backup).unwrap(), input);
    assert!(
        !JsonSettingsRepository::new(target.clone())
            .load()
            .unwrap()
            .settings
            .reminders
            .enabled
    );
    let again = run(&[
        "settings",
        "import",
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
    ]);
    assert!(again.status.success(), "{}", stderr(&again));
    assert_eq!(
        fs::read_dir(&rust_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains("pre-python-import"))
            .count(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&source_backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
