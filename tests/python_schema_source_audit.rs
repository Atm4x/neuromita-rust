use neuromita::settings::{
    PythonPresetStage, PythonSchemaAuditError, PythonSchemaDirectoryReview, PythonSchemaFileKind,
};
use serde_json::json;
use std::{fs, path::Path};

struct TestRoot(std::path::PathBuf);

impl TestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "neuromita-python-schema-audit-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn schema_path(&self) -> std::path::PathBuf {
        self.0.join("APIModelSchemas")
    }
    fn publish_path(&self) -> std::path::PathBuf {
        self.0.join("stage")
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stage() -> PythonPresetStage {
    let preset = json!({"presets":{"18":{
        "id":18,"name":"Local","base":3,"protocol_id":"","protocol_overrides":{},
        "url":"","default_model":"gemini-2.5-flash","key":"test-secret-keep-private",
        "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},
        "enabled":["max_tokens","thinking_level"]}
    }},"order":[18]});
    let templates = json!({"templates":{"3":{
        "name":"Google","protocol_id":"google_gemini_default", "url":"",
        "default_model":"gemini-2.5-flash"
    }}});
    PythonPresetStage::prepare(
        &serde_json::to_vec(&preset).unwrap(),
        &serde_json::to_vec(&templates).unwrap(),
    )
    .unwrap()
}

fn factory_bytes() -> &'static [u8] {
    include_bytes!("fixtures/python_bundled_chatgpt_plan.json")
}

#[test]
fn absent_source_is_attested_not_created_and_absence_is_fenced() {
    let root = TestRoot::new();
    let source = root.schema_path();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert!(!review.directory_present);
    assert!(review.files.is_empty());
    assert!(!source.exists());
    review.require_representable().unwrap();
    review.verify_unchanged().unwrap();
    fs::create_dir(&source).unwrap();
    assert!(matches!(
        review.verify_unchanged(),
        Err(PythonSchemaAuditError::SourceChanged)
    ));
    let dest = root.publish_path();
    assert!(
        stage()
            .stage_new_directory_with_schemas(&dest, &review)
            .is_err()
    );
    assert!(!dest.exists());
}

#[test]
fn empty_directory_is_safe_but_new_file_invalidates_preview() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert!(review.directory_present);
    review.require_representable().unwrap();
    fs::write(source.join("new.json"), br#"{"dialect":"g4f"}"#).unwrap();
    assert!(matches!(
        review.verify_unchanged(),
        Err(PythonSchemaAuditError::SourceChanged)
    ));
}

#[test]
fn canonical_python_factory_copy_is_not_a_user_override() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("chatgpt-plan.json"), factory_bytes()).unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].kind, PythonSchemaFileKind::BundledCopy);
    review.require_representable().unwrap();
    let dest = root.publish_path();
    stage()
        .stage_new_directory_with_schemas(&dest, &review)
        .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(
        &fs::read(dest.join("python-source-schema-inventory.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(inventory["directory_present"], true);
    assert_eq!(inventory["files"][0]["kind"], "bundled_copy");
    assert!(!inventory.to_string().contains("test-secret-keep-private"));
    assert!(!dest.join("APIModelSchemas").exists());
}

#[test]
fn reordered_json_and_utf8_bom_still_match_python_factory_fingerprint() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(factory_bytes()).unwrap();
    let entries = value.as_object_mut().unwrap();
    let fields = entries.remove("fields").unwrap();
    entries.insert("fields".into(), fields);
    let mut data = vec![0xef, 0xbb, 0xbf];
    data.extend(serde_json::to_vec(&value).unwrap());
    fs::write(source.join("bundled.json"), data).unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert_eq!(review.files[0].kind, PythonSchemaFileKind::BundledCopy);
}

#[test]
fn edited_bundled_schema_is_rejected_even_with_the_same_id() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(factory_bytes()).unwrap();
    value["revision"] = json!(100);
    fs::write(
        source.join("chatgpt-plan.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert_eq!(review.unrepresented_names(), ["chatgpt-plan.json"]);
    assert!(matches!(
        review.require_representable(),
        Err(PythonSchemaAuditError::Unrepresented(1))
    ));
    let dest = root.publish_path();
    assert!(
        stage()
            .stage_new_directory_with_schemas(&dest, &review)
            .is_err()
    );
    assert!(!dest.exists());
}

#[test]
fn g4f_ignored_by_the_python_loader_is_inventoried_not_installed() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("old.json"),
        br#"{"dialect":"g4f","secret":"do-not-log"}"#,
    )
    .unwrap();
    fs::write(source.join("not-a-schema.tmp"), b"not a json definition").unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert_eq!(review.files.len(), 1);
    assert_eq!(review.files[0].kind, PythonSchemaFileKind::IgnoredG4f);
    review.require_representable().unwrap();
}

#[test]
fn invalid_json_and_unknown_schema_are_both_fail_closed() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("broken.json"), b"{broken").unwrap();
    fs::write(source.join("custom.json"), br#"{"schema_version":1,"id":"custom","dialect":"openai_responses","revision":1,"fields":[]}"#).unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert_eq!(review.unrepresented_names(), ["broken.json", "custom.json"]);
    assert!(matches!(
        review.require_representable(),
        Err(PythonSchemaAuditError::Unrepresented(2))
    ));
}

#[test]
fn duplicate_json_keys_are_never_classified_as_factory() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    fs::write(
        source.join("duplicate.json"),
        br#"{"dialect":"g4f","dialect":"g4f"}"#,
    )
    .unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    assert_eq!(review.files[0].kind, PythonSchemaFileKind::Unrepresented);
}

#[test]
fn value_only_changes_and_deletions_invalidate_the_exact_inventory() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    let file = source.join("old.json");
    fs::write(&file, br#"{"dialect":"g4f","data":"old"}"#).unwrap();
    let review = PythonSchemaDirectoryReview::inspect(&source).unwrap();
    fs::write(&file, br#"{"dialect":"g4f","data":"new"}"#).unwrap();
    assert!(matches!(
        review.verify_unchanged(),
        Err(PythonSchemaAuditError::SourceChanged)
    ));
    let dest = root.publish_path();
    assert!(
        stage()
            .stage_new_directory_with_schemas(&dest, &review)
            .is_err()
    );
    assert!(!dest.exists());
    fs::remove_file(&file).unwrap();
    assert!(matches!(
        review.verify_unchanged(),
        Err(PythonSchemaAuditError::SourceChanged)
    ));
}

#[test]
fn directory_parent_must_be_regular_and_explicit() {
    let root = TestRoot::new();
    assert!(matches!(
        PythonSchemaDirectoryReview::inspect(Path::new("APIModelSchemas")),
        Err(PythonSchemaAuditError::UnsafeDirectory)
    ));
    fs::write(root.0.join("notadir"), b"text").unwrap();
    assert!(matches!(
        PythonSchemaDirectoryReview::inspect(&root.0.join("notadir").join("schemas")),
        Err(PythonSchemaAuditError::UnsafeDirectory)
    ));
    fs::write(root.schema_path(), b"not a directory").unwrap();
    assert!(matches!(
        PythonSchemaDirectoryReview::inspect(&root.schema_path()),
        Err(PythonSchemaAuditError::UnsafeDirectory)
    ));
}

#[test]
fn oversized_schema_is_not_loaded_into_memory_without_bound() {
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("huge.json"), vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(matches!(
        PythonSchemaDirectoryReview::inspect(&source),
        Err(PythonSchemaAuditError::TooLarge)
    ));
}

#[cfg(unix)]
#[test]
fn symlink_schema_file_is_rejected() {
    use std::os::unix::fs::symlink;
    let root = TestRoot::new();
    let source = root.schema_path();
    fs::create_dir(&source).unwrap();
    fs::write(root.0.join("private.json"), factory_bytes()).unwrap();
    symlink(root.0.join("private.json"), source.join("copy.json")).unwrap();
    assert!(matches!(
        PythonSchemaDirectoryReview::inspect(&source),
        Err(PythonSchemaAuditError::UnsafeEntry)
    ));
}
