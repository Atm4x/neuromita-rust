use neuromita::settings::{PythonPresetStage, PythonPresetStageError};
use serde_json::{Value, json};
use std::path::Path;

fn fixture() -> (Vec<u8>, Vec<u8>) {
    let templates = json!({"templates":{"3":{"name":"Google", "protocol_id":"google_gemini_default", "url":"", "default_model":"gemini-2.5-flash"}}});
    let presets = json!({"presets":{"18":{
        "id":18,"name":"Private preset","base":3,"protocol_id":"", "protocol_overrides":{},
        "url":"", "default_model":"gemini-2.5-flash","key":"private-fixture-key",
        "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},"enabled":["max_tokens","thinking_level"]}
    }},"order":[18]});
    (
        serde_json::to_vec(&presets).unwrap(),
        serde_json::to_vec(&templates).unwrap(),
    )
}

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("neuromita-stage-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh_directory() -> (TestDirectory, std::path::PathBuf) {
    let parent = TestDirectory::new();
    let destination = parent.path().join("Portable UserData imported");
    (parent, destination)
}

#[test]
fn staging_publishes_full_set_and_private_source_backups() {
    let (presets, templates) = fixture();
    let plan = PythonPresetStage::prepare(&presets, &templates).unwrap();
    assert_eq!(plan.preview.converted_ids, [18]);
    assert_eq!(plan.preview.blocked_count(), 0);
    let (_parent, destination) = fresh_directory();
    plan.stage_new_directory(&destination).unwrap();
    for filename in [
        "presets.json",
        "preset-credentials.json",
        "python-template-import.json",
        "python-source-api-presets.json",
        "python-source-api-templates.json",
    ] {
        assert!(destination.join(filename).exists(), "{filename}");
    }
    assert_eq!(
        std::fs::read(destination.join("python-source-api-presets.json")).unwrap(),
        presets
    );
    assert_eq!(
        std::fs::read(destination.join("python-source-api-templates.json")).unwrap(),
        templates
    );
    let catalog = std::fs::read_to_string(destination.join("presets.json")).unwrap();
    assert!(!catalog.contains("private-fixture-key"));
    let secrets = std::fs::read_to_string(destination.join("preset-credentials.json")).unwrap();
    assert!(secrets.contains("private-fixture-key"));
    assert!(!format!("{:?}", plan.preview).contains("private-fixture-key"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for name in ["preset-credentials.json", "python-source-api-presets.json"] {
            assert_eq!(
                std::fs::metadata(destination.join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

#[test]
fn preview_never_writes_and_existing_destination_is_not_replaced() {
    let (presets, templates) = fixture();
    let plan = PythonPresetStage::prepare(&presets, &templates).unwrap();
    let (_parent, destination) = fresh_directory();
    assert!(!destination.exists());
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("keep.txt"), b"original").unwrap();
    assert!(matches!(
        plan.stage_new_directory(&destination),
        Err(PythonPresetStageError::UnsafeDestination)
    ));
    assert_eq!(
        std::fs::read(destination.join("keep.txt")).unwrap(),
        b"original"
    );
    assert!(!destination.join("presets.json").exists());
}

#[test]
fn malformed_and_duplicate_keys_are_rejected_before_writing() {
    let (presets, templates) = fixture();
    let duplicate = br#"{"presets":{"18":{"name":"x","name":"y"}}}"#;
    assert!(PythonPresetStage::prepare(duplicate, &templates).is_err());
    assert!(PythonPresetStage::prepare(b"{broken", &templates).is_err());
    assert!(PythonPresetStage::prepare(&presets, br#"{"templates":{},"templates":{}}"#).is_err());
    assert!(PythonPresetStage::prepare(&presets, &vec![b'x'; 8 * 1024 * 1024 + 1]).is_err());
}

#[test]
fn unsupported_preset_blocks_entire_publish_with_no_partial_output() {
    let (presets, templates) = fixture();
    let mut input: Value = serde_json::from_slice(&presets).unwrap();
    input["presets"]["19"] = input["presets"]["18"].clone();
    input["presets"]["19"]["fallbacks"] = json!([{"preset_id":18}]);
    let input = serde_json::to_vec(&input).unwrap();
    let plan = PythonPresetStage::prepare(&input, &templates).unwrap();
    assert_eq!(plan.preview.converted_ids, [18]);
    assert_eq!(plan.preview.blocked_ids, [19]);
    let (_parent, destination) = fresh_directory();
    assert!(matches!(
        plan.stage_new_directory(&destination),
        Err(PythonPresetStageError::Blocked(1))
    ));
    assert!(!destination.exists());
}

#[test]
fn colliding_python_template_and_preset_ids_are_not_silently_reused() {
    let (presets, _) = fixture();
    let (_, templates) = fixture();
    let mut value: Value = serde_json::from_slice(&templates).unwrap();
    value["templates"]["18"] = value["templates"]["3"].clone();
    let plan = PythonPresetStage::prepare(&presets, &serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(plan.preview.blocked_ids, [18]);
}

#[test]
fn invalid_preset_id_is_reported_without_echoing_the_field_contents() {
    let (presets, templates) = fixture();
    let input = String::from_utf8(presets)
        .unwrap()
        .replace("\"18\":", "\"secret-in-key\":");
    let plan = PythonPresetStage::prepare(input.as_bytes(), &templates).unwrap();
    assert_eq!(plan.preview.blocked_without_id, 1);
    assert!(plan.preview.blocked_ids.is_empty());
}

#[test]
fn relative_target_and_parent_symlink_are_rejected() {
    let (presets, templates) = fixture();
    let plan = PythonPresetStage::prepare(&presets, &templates).unwrap();
    assert!(matches!(
        plan.stage_new_directory(Path::new("relative")),
        Err(PythonPresetStageError::UnsafeDestination)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let parent = TestDirectory::new();
        let actual = parent.path().join("actual");
        std::fs::create_dir(&actual).unwrap();
        let alias = parent.path().join("alias");
        symlink(&actual, &alias).unwrap();
        assert!(matches!(
            plan.stage_new_directory(&alias.join("new")),
            Err(PythonPresetStageError::UnsafeDestination)
        ));
        assert!(!actual.join("new").exists());
    }
}

#[test]
fn distinct_source_strings_that_parse_to_same_id_are_blocked() {
    let (presets, templates) = fixture();
    let mut raw: Value = serde_json::from_slice(&presets).unwrap();
    raw["presets"]["018"] = raw["presets"]["18"].clone();
    let plan = PythonPresetStage::prepare(&serde_json::to_vec(&raw).unwrap(), &templates).unwrap();
    assert_eq!(plan.preview.blocked_ids, [18, 18]);
    assert!(plan.preview.converted_ids.is_empty());
}

#[test]
fn nested_preset_id_mismatch_is_not_reinterpreted() {
    let (presets, templates) = fixture();
    let mut raw: Value = serde_json::from_slice(&presets).unwrap();
    raw["presets"]["18"]["id"] = json!(999);
    let plan = PythonPresetStage::prepare(&serde_json::to_vec(&raw).unwrap(), &templates).unwrap();
    assert_eq!(plan.preview.blocked_ids, [18]);
}

#[test]
fn numeric_template_collision_is_checked_even_when_spelled_differently() {
    let (presets, templates) = fixture();
    let mut raw: Value = serde_json::from_slice(&templates).unwrap();
    raw["templates"]["018"] = raw["templates"]["3"].clone();
    let plan = PythonPresetStage::prepare(&presets, &serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(plan.preview.blocked_ids, [18]);
}

#[test]
fn blocked_preset_reason_cannot_expose_credential_values() {
    let (presets, templates) = fixture();
    let mut raw: Value = serde_json::from_slice(&presets).unwrap();
    raw["presets"]["18"]["protocol_id"] = json!("unknown-private-credential");
    raw["presets"]["18"]["base"] = Value::Null;
    let plan = PythonPresetStage::prepare(&serde_json::to_vec(&raw).unwrap(), &templates).unwrap();
    let (_root, target) = fresh_directory();
    let error = plan.stage_new_directory(&target).unwrap_err();
    assert!(!error.to_string().contains("unknown-private-credential"));
    assert!(!target.exists());
}
