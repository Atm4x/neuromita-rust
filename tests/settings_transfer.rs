use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, SETTINGS_SCHEMA_VERSION, SettingsError,
    SettingsRepository, SettingsTransfer, SettingsTransferError, StoredSettings, Volume,
};
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("neuromita-settings-transfer-{}", Uuid::new_v4()));
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
    StoredSettings {
        schema_version: SETTINGS_SCHEMA_VERSION,
        revision,
        settings,
    }
}

fn settings_json(volume: u32) -> String {
    let mut settings = serde_json::to_value(AppSettings::initial_profile()).unwrap();
    settings["voice"]["volume"] = serde_json::json!(volume);
    serde_json::to_string(&settings).unwrap()
}

fn envelope(schema: u32, revision: u64, settings: &str) -> String {
    format!("{{\"schema_version\":{schema},\"revision\":{revision},\"settings\":{settings}}}")
}

fn complete(schema: u32, revision: u64) -> String {
    envelope(schema, revision, &settings_json(80))
}

fn temporary_files(directory: &Path) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".tmp"))
        })
        .collect()
}

#[test]
fn export_is_a_complete_canonical_envelope() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let store = JsonSettingsRepository::new(store_path.clone());
    let settings = stored(12, 42);
    store.save(&settings).unwrap();

    let backup_path = directory.path("backup.json");
    let transfer = store.export_to(&backup_path).unwrap();
    assert_eq!(transfer.stored(), &settings);

    let backup = fs::read(&backup_path).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&backup).unwrap();
    let object = document.as_object().unwrap();
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["revision", "schema_version", "settings"]);
    let mut section_keys: Vec<&str> = object["settings"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    section_keys.sort_unstable();
    assert_eq!(
        section_keys,
        [
            "capture",
            "characters",
            "interface",
            "presets",
            "reminders",
            "voice"
        ]
    );
    assert_eq!(object["schema_version"], 3);
    assert_eq!(object["revision"], 12);

    assert_eq!(fs::read(&store_path).unwrap(), backup);
    store.export_to(&directory.path("backup-2.json")).unwrap();
    assert_eq!(
        fs::read(directory.path("backup-2.json")).unwrap(),
        backup,
        "export is deterministic"
    );
    assert_eq!(
        JsonSettingsRepository::new(backup_path).load().unwrap(),
        settings,
        "an exported backup is itself a valid store"
    );
}

#[test]
fn import_replaces_the_whole_store_idempotently() {
    let directory = TempDir::new();
    let backup = directory.path("backup.json");
    JsonSettingsRepository::new(directory.path("source.json"))
        .save(&stored(9, 33))
        .unwrap();
    JsonSettingsRepository::new(directory.path("source.json"))
        .export_to(&backup)
        .unwrap();

    let destination_path = directory.path("destination.json");
    let destination = JsonSettingsRepository::new(destination_path.clone());
    destination.save(&stored(3, 70)).unwrap();

    let bytes = fs::read(&backup).unwrap();
    let imported = destination.import(&bytes).unwrap();
    assert_eq!(imported.stored().revision, 9);
    assert_eq!(destination.load().unwrap(), stored(9, 33));
    let first = fs::read(&destination_path).unwrap();
    assert_eq!(first, bytes);

    destination.import(&bytes).unwrap();
    assert_eq!(fs::read(&destination_path).unwrap(), first);
    assert_eq!(destination.load().unwrap().revision, 9);
}

#[test]
fn decode_classifies_rejections() {
    let unknown_top = envelope(2, 4, &settings_json(80)).replacen("{", "{\"presets\":[],", 1);
    let unknown_nested = envelope(
        2,
        4,
        &settings_json(80).replace("\"voice\":{", "\"voice\":{\"pitch\":1,"),
    );
    let missing_section = envelope(2, 4, "{}");
    let duplicate_top = format!(
        "{{\"schema_version\":2,\"revision\":4,\"revision\":5,\"settings\":{}}}",
        settings_json(80)
    );
    let duplicate_nested = envelope(
        2,
        4,
        &settings_json(80).replace("\"volume\":80", "\"volume\":80,\"volume\":70"),
    );
    let invalid_volume = envelope(2, 4, &settings_json(101));
    let empty_voice = envelope(
        2,
        4,
        &settings_json(80).replace("\"voice\":\"default\"", "\"voice\":\"  \""),
    );
    let invalid_font = envelope(
        2,
        4,
        &settings_json(80).replace("\"font_size\":14", "\"font_size\":5"),
    );
    let invalid_interval = envelope(
        2,
        4,
        &settings_json(80).replace("\"interval_ms\":1000", "\"interval_ms\":0"),
    );
    let wrong_type = format!(
        "{{\"schema_version\":2,\"revision\":4,\"settings\":{}}}",
        settings_json(80).replace("\"volume\":80", "\"volume\":\"80\"")
    );

    assert!(matches!(
        SettingsTransfer::decode(b"not json"),
        Err(SettingsTransferError::MalformedJson(_))
    ));
    assert!(matches!(
        SettingsTransfer::decode(br#"{"volume":80}"#),
        Err(SettingsTransferError::NotFullSettingsTransfer(_))
    ));
    assert!(matches!(
        SettingsTransfer::decode(br#"{"schema_version":2,"revision":1}"#),
        Err(SettingsTransferError::NotFullSettingsTransfer(_))
    ));
    assert!(matches!(
        SettingsTransfer::decode(complete(4, 4).as_bytes()),
        Err(SettingsTransferError::UnsupportedSchema { found: 4 })
    ));
    assert!(matches!(
        SettingsTransfer::decode(complete(0, 4).as_bytes()),
        Err(SettingsTransferError::Migration(
            neuromita::migration::MigrationError::MissingStep { from: 0, .. }
        ))
    ));
    for fixture in [
        unknown_top,
        unknown_nested,
        missing_section,
        duplicate_top,
        duplicate_nested,
        wrong_type,
        invalid_volume,
    ] {
        assert!(
            matches!(
                SettingsTransfer::decode(fixture.as_bytes()),
                Err(SettingsTransferError::Migration(_))
            ),
            "fixture must be rejected as an invalid envelope: {fixture}"
        );
    }
    for fixture in [empty_voice, invalid_font, invalid_interval] {
        assert!(
            matches!(
                SettingsTransfer::decode(fixture.as_bytes()),
                Err(SettingsTransferError::Migration(_))
            ),
            "fixture must be rejected as invalid values: {fixture}"
        );
    }
}

#[cfg(windows)]
#[test]
fn locked_destination_preserves_bytes_and_succeeds_after_unlock() {
    use std::os::windows::fs::OpenOptionsExt;

    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let store = JsonSettingsRepository::new(store_path.clone());
    store.save(&stored(1, 40)).unwrap();
    let before = fs::read(&store_path).unwrap();

    let lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&store_path)
        .unwrap();
    assert!(store.save(&stored(2, 41)).is_err());
    assert!(
        store
            .import(&serde_json::to_vec(&stored(2, 41)).unwrap())
            .is_err()
    );
    drop(lock);

    assert_eq!(fs::read(&store_path).unwrap(), before);
    assert!(temporary_files(&directory.path).is_empty());
    store.save(&stored(2, 41)).unwrap();
    assert_eq!(store.load().unwrap(), stored(2, 41));
    assert!(temporary_files(&directory.path).is_empty());
}

#[test]
fn invalid_imports_leave_the_destination_untouched() {
    let directory = TempDir::new();
    let destination_path = directory.path("store.json");
    let destination = JsonSettingsRepository::new(destination_path.clone());
    destination.save(&stored(5, 60)).unwrap();
    let before = fs::read(&destination_path).unwrap();

    let fixtures = [
        complete(4, 5),
        complete(0, 5),
        complete(2, 5).replace("\"volume\":80", "\"volume\":101"),
        complete(2, 5).replace("\"voice\":{", "\"voice\":{\"pitch\":1,"),
        format!(
            "{{\"schema_version\":2,\"revision\":5,\"revision\":6,\"settings\":{}}}",
            settings_json(80)
        ),
        "{\"volume\":80}".to_owned(),
        "{\"schema_version\":2,".to_owned(),
    ];
    for fixture in fixtures {
        assert!(
            destination.import(fixture.as_bytes()).is_err(),
            "import must fail for {fixture}"
        );
        assert_eq!(fs::read(&destination_path).unwrap(), before);
        assert!(temporary_files(&directory.path).is_empty());
    }
    assert_eq!(destination.load().unwrap(), stored(5, 60));
}

#[test]
fn interrupted_save_orphan_is_ignored_and_preserved() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let store = JsonSettingsRepository::new(store_path.clone());
    store.save(&stored(1, 80)).unwrap();

    let orphan = directory.path("store.json.deadbeef.tmp");
    fs::write(&orphan, br#"{"schema_version":2,"revision"#).unwrap();
    assert_eq!(store.load().unwrap(), stored(1, 80));

    store.save(&stored(2, 20)).unwrap();
    assert_eq!(store.load().unwrap(), stored(2, 20));
    assert_eq!(
        fs::read(&orphan).unwrap(),
        br#"{"schema_version":2,"revision"#
    );
    assert_eq!(temporary_files(&directory.path), vec![orphan]);
}

#[test]
fn replacement_failure_preserves_destination_then_saves_succeed() {
    let directory = TempDir::new();
    let destination_path = directory.path("store.json");
    let destination = JsonSettingsRepository::new(destination_path.clone());
    fs::create_dir(&destination_path).unwrap();
    let marker = destination_path.join("marker");
    fs::write(&marker, "keep").unwrap();

    assert!(destination.save(&stored(1, 50)).is_err());
    assert_eq!(fs::read_to_string(&marker).unwrap(), "keep");
    assert!(temporary_files(&directory.path).is_empty());

    let source = JsonSettingsRepository::new(directory.path("source.json"));
    source.save(&stored(1, 50)).unwrap();
    assert!(
        source.export_to(&destination_path).is_err(),
        "export must not replace a directory"
    );
    assert_eq!(fs::read_to_string(&marker).unwrap(), "keep");
    assert!(temporary_files(&directory.path).is_empty());
    assert_eq!(source.load().unwrap(), stored(1, 50));

    fs::remove_file(&marker).unwrap();
    fs::remove_dir(&destination_path).unwrap();
    destination.save(&stored(2, 51)).unwrap();
    destination.save(&stored(3, 52)).unwrap();
    assert_eq!(destination.load().unwrap(), stored(3, 52));
    assert!(temporary_files(&directory.path).is_empty());
}

#[test]
fn repository_save_rejects_invalid_settings_without_writing() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let store = JsonSettingsRepository::new(store_path.clone());
    store.save(&stored(1, 80)).unwrap();
    let before = fs::read(&store_path).unwrap();

    let mut invalid = stored(2, 81);
    invalid.settings.voice.voice.clear();
    assert!(matches!(
        store.save(&invalid),
        Err(SettingsError::Validation(_))
    ));
    assert_eq!(fs::read(&store_path).unwrap(), before);

    let mut future = stored(2, 81);
    future.schema_version = 4;
    assert!(matches!(
        store.save(&future),
        Err(SettingsError::Storage(message)) if message.contains("unsupported settings schema 4")
    ));
    assert_eq!(fs::read(&store_path).unwrap(), before);
    assert!(temporary_files(&directory.path).is_empty());
}

#[test]
fn load_rejects_future_foreign_and_duplicate_documents() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let store = JsonSettingsRepository::new(store_path.clone());
    let fixtures = [
        complete(4, 1),
        complete(2, 1).replace("\"voice\":{", "\"voice\":{\"pitch\":1,"),
        format!(
            "{{\"schema_version\":2,\"revision\":1,\"revision\":2,\"settings\":{}}}",
            settings_json(80)
        ),
        "{\"revision\":1}".to_owned(),
    ];
    for fixture in fixtures {
        fs::write(&store_path, &fixture).unwrap();
        assert!(
            matches!(store.load(), Err(SettingsError::Storage(_))),
            "load must reject {fixture}"
        );
    }
}

#[test]
fn concurrent_writers_publish_whole_documents_only() {
    let directory = TempDir::new();
    let store_path = directory.path("store.json");
    let first = JsonSettingsRepository::new(store_path.clone());
    let second = JsonSettingsRepository::new(store_path.clone());
    let left = stored(10, 11);
    let right = stored(20, 22);
    let left = Arc::new(left);
    let right = Arc::new(right);

    let left_thread = {
        let left = Arc::clone(&left);
        thread::spawn(move || {
            (0..10)
                .map(|_| first.save(&left).is_ok())
                .collect::<Vec<_>>()
        })
    };
    let right_thread = {
        let right = Arc::clone(&right);
        thread::spawn(move || {
            (0..10)
                .map(|_| second.save(&right).is_ok())
                .collect::<Vec<_>>()
        })
    };
    let left_results = left_thread.join().unwrap();
    let right_results = right_thread.join().unwrap();
    assert!(left_results.iter().any(|saved| *saved));
    assert!(right_results.iter().any(|saved| *saved));

    let final_store = JsonSettingsRepository::new(store_path);
    let loaded = final_store.load().unwrap();
    assert!(
        loaded == *left || loaded == *right,
        "destination must be exactly one complete document"
    );
    assert!(temporary_files(&directory.path).is_empty());
}
