use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, Language, PythonSettingsMigration, SettingsRepository,
    StoredSettings,
};
use uuid::Uuid;

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("python-settings-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn files(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_neuromita"))
        .arg("settings")
        .arg("import")
        .args(args)
        .env_remove("GEMINI_API_KEY")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .output()
        .unwrap()
}

fn report(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn preview_is_non_mutating_and_reports_only_field_names() {
    let dir = Workspace::new();
    let source = dir.file("python settings.json");
    let target = dir.file("rust-settings.json");
    fs::write(
        &source,
        br#"{"LANGUAGE":"EN","API_KEY":"private-secret","HIDE_PRIVATE":false}"#,
    )
    .unwrap();
    let before = dir.files();
    let output = run(&[
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
        "--preview",
    ]);
    assert!(output.status.success(), "{}", report(&output));
    let report = report(&output);
    assert!(report.contains("LANGUAGE") && report.contains("API_KEY"));
    assert!(!report.contains("private-secret"));
    assert!(report.contains("no settings or backups were written"));
    assert_eq!(dir.files(), before);
}

#[test]
fn import_requires_explicit_unmapped_approval_and_backs_up_both_files() {
    let dir = Workspace::new();
    let source = dir.file("python.json");
    let target = dir.file("rust.json");
    let python = br#"{"LANGUAGE":"EN","HIDE_PRIVATE":"false","REMINDERS_ENABLED":false,"VOICE_LANGUAGE":"en","LOCAL_VOICE_LOAD_LAST":true,"LOCAL_VOICE_INIT_ON_REQUEST":false,"UNMAPPED_ONLY":12}"#;
    fs::write(&source, python).unwrap();
    let mut existing = AppSettings::initial_profile();
    existing.voice.voice = "keep-this-user-choice".into();
    existing.speech.asr.input_mode = "push_to_talk".into();
    JsonSettingsRepository::new(target.clone())
        .save(&StoredSettings {
            revision: 7,
            settings: existing,
        })
        .unwrap();
    let original_target = fs::read(&target).unwrap();
    let base_args = [
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        target.to_str().unwrap(),
    ];
    let rejected = run(&base_args);
    assert!(!rejected.status.success(), "{}", report(&rejected));
    assert!(report(&rejected).contains("UNMAPPED_ONLY"));
    assert_eq!(fs::read(&target).unwrap(), original_target);
    assert_eq!(dir.files().len(), 3);

    let applied = run(&[base_args.as_slice(), &["--allow-unmapped"]].concat());
    assert!(applied.status.success(), "{}", report(&applied));
    let new = JsonSettingsRepository::new(target.clone()).load().unwrap();
    assert_eq!(new.revision, 8);
    assert_eq!(new.settings.interface.language, Language::English);
    assert!(!new.settings.interface.hide_private);
    assert!(!new.settings.reminders.enabled);
    assert_eq!(new.settings.speech.tts.local_language, "en");
    assert!(new.settings.speech.tts.local_auto_load);
    assert!(!new.settings.speech.tts.local_lazy_init);
    assert_eq!(new.settings.voice.voice, "keep-this-user-choice");
    assert_eq!(new.settings.speech.asr.input_mode, "push_to_talk");
    let backups: Vec<_> = dir
        .files()
        .into_iter()
        .filter(|p| p.extension().is_some_and(|ext| ext == "bak"))
        .collect();
    assert_eq!(backups.len(), 2);
    let data: Vec<_> = backups.iter().map(|p| fs::read(p).unwrap()).collect();
    assert!(data.contains(&python.to_vec()));
    assert!(data.contains(&original_target));

    let bytes = fs::read(&target).unwrap();
    let repeated = run(&[base_args.as_slice(), &["--allow-unmapped"]].concat());
    assert!(repeated.status.success(), "{}", report(&repeated));
    assert!(report(&repeated).contains("Already imported"));
    assert_eq!(dir.files().len(), 5);
    assert_eq!(fs::read(target).unwrap(), bytes);
}

#[test]
fn invalid_values_json_and_collisions_leave_user_settings_untouched() {
    let dir = Workspace::new();
    let source = dir.file("source.json");
    let target = dir.file("target.json");
    let stored = StoredSettings {
        revision: 12,
        settings: AppSettings::initial_profile(),
    };
    JsonSettingsRepository::new(target.clone())
        .save(&stored)
        .unwrap();
    let before = fs::read(&target).unwrap();
    let malformed = [
        br#"{"LANGUAGE":"DE"}"#.as_slice(),
        br#"{"HIDE_PRIVATE":"not-sure"}"#.as_slice(),
        br#"{"LANGUAGE":"EN","LANGUAGE":"RU"}"#.as_slice(),
        br#"{"LANGUAGE":"EN""#.as_slice(),
        br#"{"LANGUAGE":null}"#.as_slice(),
    ];
    for bytes in malformed {
        fs::write(&source, bytes).unwrap();
        let result = run(&[
            "--format",
            "python",
            "--source",
            source.to_str().unwrap(),
            "--destination",
            target.to_str().unwrap(),
            "--allow-unmapped",
        ]);
        assert!(!result.status.success(), "{}", report(&result));
        assert_eq!(fs::read(&target).unwrap(), before);
        assert_eq!(dir.files().len(), 3);
    }
    assert!(PythonSettingsMigration::preview(br#"{"LANGUAGE":"RU","X":5}"#).is_ok());
}

#[test]
fn cannot_replace_same_source_or_invalid_rust_destination() {
    let dir = Workspace::new();
    let source = dir.file("settings.json");
    fs::write(&source, br#"{"LANGUAGE":"EN"}"#).unwrap();
    let args = [
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        source.to_str().unwrap(),
    ];
    let result = run(&args);
    assert!(!result.status.success(), "{}", report(&result));
    assert_eq!(dir.files().len(), 1);
    let destination = dir.file("invalid-rust.json");
    fs::write(&destination, b"broken").unwrap();
    let result = run(&[
        "--format",
        "python",
        "--source",
        source.to_str().unwrap(),
        "--destination",
        destination.to_str().unwrap(),
    ]);
    assert!(!result.status.success(), "{}", report(&result));
    assert_eq!(fs::read(&destination).unwrap(), b"broken");
    assert_eq!(dir.files().len(), 2);
}
