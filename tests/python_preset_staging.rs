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

fn existing_profile(imported: &PythonPresetStage) -> (Vec<u8>, Vec<u8>) {
    let temp = TestDirectory::new();
    let staged = temp.path().join("imported");
    imported.stage_new_directory(&staged).unwrap();
    let mut catalog: Value =
        serde_json::from_slice(&std::fs::read(staged.join("presets.json")).unwrap()).unwrap();
    let mut record = catalog["entries"]
        .as_object_mut()
        .unwrap()
        .remove("18")
        .unwrap();
    record["name"] = json!("Preserved existing profile");
    let definition = &mut record["definition"]["configuration"]["credential"];
    if definition.is_object() {
        *definition = json!({"source":"local","name":"imported_7"});
    }
    catalog["entries"]["7"] = record;
    catalog["order"] = json!([7]);
    catalog["next_id"] = json!(8);
    (
        serde_json::to_vec(&catalog).unwrap(),
        serde_json::to_vec(&json!({"imported_7":"old-private-fixture"})).unwrap(),
    )
}

#[test]
fn merged_review_preserves_old_record_credential_and_source_backups() {
    let (presets, templates) = fixture();
    let imported = PythonPresetStage::prepare(&presets, &templates).unwrap();
    let (old_catalog, old_credentials) = existing_profile(&imported);
    let merged = imported
        .merge_with_existing(Some(&old_catalog), Some(&old_credentials))
        .unwrap();
    assert_eq!(merged.preview.preserved_ids, [7]);
    let (_parent, dest) = fresh_directory();
    merged.stage_new_directory(&dest).unwrap();
    let catalog: Value =
        serde_json::from_slice(&std::fs::read(dest.join("presets.json")).unwrap()).unwrap();
    let credentials: Value =
        serde_json::from_slice(&std::fs::read(dest.join("preset-credentials.json")).unwrap())
            .unwrap();
    assert_eq!(catalog["entries"].as_object().unwrap().len(), 2);
    assert_eq!(
        catalog["entries"]["7"]["name"],
        "Preserved existing profile"
    );
    assert_eq!(catalog["entries"]["18"]["name"], "Private preset");
    assert_eq!(catalog["next_id"], 19);
    assert_eq!(credentials["imported_7"], "old-private-fixture");
    assert_eq!(credentials["python_preset_18"], "private-fixture-key");
    assert_eq!(
        std::fs::read(dest.join("existing-source-presets.json")).unwrap(),
        old_catalog
    );
    assert_eq!(
        std::fs::read(dest.join("existing-source-preset-credentials.json")).unwrap(),
        old_credentials
    );
    let no_secret_catalog = std::fs::read_to_string(dest.join("presets.json")).unwrap();
    assert!(!no_secret_catalog.contains("old-private-fixture"));
    assert!(!no_secret_catalog.contains("private-fixture-key"));
    let catalog = neuromita::generation::PresetCatalog::open(
        Default::default(),
        std::sync::Arc::new(neuromita::generation::JsonPresetRepository::new(
            dest.join("presets.json"),
        )),
        std::sync::Arc::new(neuromita::generation::ApplicationPresetCredentials::new(
            dest.join("preset-credentials.json"),
        )),
    )
    .unwrap();
    assert_eq!(catalog.list().len(), 2);
    assert!(
        catalog
            .get(neuromita::generation::PresetId(7))
            .unwrap()
            .is_some()
    );
    assert!(
        catalog
            .get(neuromita::generation::PresetId(18))
            .unwrap()
            .is_some()
    );
}

#[test]
fn merge_rejects_existing_and_retired_identifier_collisions_without_publication() {
    let (presets, templates) = fixture();
    let imported = PythonPresetStage::prepare(&presets, &templates).unwrap();
    let (mut old_catalog, old_credentials) = existing_profile(&imported);
    let mut catalog: Value = serde_json::from_slice(&old_catalog).unwrap();
    catalog["next_id"] = json!(19);
    old_catalog = serde_json::to_vec(&catalog).unwrap();
    assert!(
        imported
            .merge_with_existing(Some(&old_catalog), Some(&old_credentials))
            .is_err()
    );
    catalog["next_id"] = json!(20);
    catalog["entries"]["18"] = catalog["entries"]["7"].clone();
    old_catalog = serde_json::to_vec(&catalog).unwrap();
    assert!(
        imported
            .merge_with_existing(Some(&old_catalog), Some(&old_credentials))
            .is_err()
    );
}

#[test]
fn merge_rejects_credential_collision_even_if_values_are_equal() {
    let (presets, templates) = fixture();
    let imported = PythonPresetStage::prepare(&presets, &templates).unwrap();
    let old = json!({"python_preset_18":"private-fixture-key"});
    let old = serde_json::to_vec(&old).unwrap();
    assert!(imported.merge_with_existing(None, Some(&old)).is_err());
}

#[test]
fn merge_rejects_invalid_existing_json_and_missing_local_credentials() {
    let (presets, templates) = fixture();
    let imported = PythonPresetStage::prepare(&presets, &templates).unwrap();
    let (catalog, credentials) = existing_profile(&imported);
    assert!(
        imported
            .merge_with_existing(Some(br#"{"entries":{},"entries":{}}"#), Some(&credentials))
            .is_err()
    );
    assert!(
        imported
            .merge_with_existing(
                Some(&catalog),
                Some(br#"{"imported_7":"x","imported_7":"y"}"#)
            )
            .is_err()
    );
    assert!(imported.merge_with_existing(Some(&catalog), None).is_err());
}

#[test]
fn historical_python_layouts_preserve_order_credentials_and_exact_source() {
    let (current, templates) = fixture();
    let source: Value = serde_json::from_slice(&current).unwrap();
    let record = source["presets"]["18"].clone();
    for (historical, has_explicit_order) in [
        (json!({"presets": [record.clone()], "order": [18]}), true),
        (
            json!({"custom": {"legacy-source-key": record.clone()}, "custom_order": [18]}),
            true,
        ),
        (json!([record.clone()]), false),
    ] {
        let original = serde_json::to_vec(&historical).unwrap();
        let stage = PythonPresetStage::prepare(&original, &templates).unwrap();
        assert_eq!(stage.preview.converted_ids, [18]);
        assert_eq!(stage.preview.blocked_count(), 0);
        assert_eq!(stage.preview.python_order_imported, has_explicit_order);
        let preferences = stage
            .preview_preferences(br#"{"LAST_API_PRESET_ID":18}"#)
            .unwrap();
        assert_eq!(preferences.selected_user_id, Some(18));
        assert_eq!(preferences.user_order, [18]);
        assert_eq!(preferences.order_explicit, has_explicit_order);

        let (_parent, destination) = fresh_directory();
        stage.stage_new_directory(&destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join("python-source-api-presets.json")).unwrap(),
            original
        );
        let catalog = std::fs::read_to_string(destination.join("presets.json")).unwrap();
        assert!(!catalog.contains("private-fixture-key"));
        assert!(catalog.contains("Private preset"));
        let keys = std::fs::read_to_string(destination.join("preset-credentials.json")).unwrap();
        assert!(keys.contains("private-fixture-key"));
    }
}

#[test]
fn historical_python_layouts_reject_ambiguous_ids_and_invalid_order() {
    let (current, templates) = fixture();
    let source: Value = serde_json::from_slice(&current).unwrap();
    let record = source["presets"]["18"].clone();
    let mut duplicate = record.clone();
    duplicate["name"] = json!("other entry same ID");
    for historical in [
        json!({"presets": [record.clone(), duplicate.clone()]}),
        json!({"custom": {"a": record.clone(), "b": duplicate}}),
        json!({"custom": {"a": {"name": "missing id"}}}),
        json!([{"id": 0, "name": "zero"}]),
        json!([{"id": 18.5, "name": "fractional"}]),
        json!([record.clone(), null]),
        json!({"presets": [record.clone()], "custom": {}}),
        json!({"custom": {"a": record.clone()}, "custom_order": [18, 18]}),
        json!({"custom": {"a": record.clone()}, "custom_order": [999]}),
        json!({"custom": {"a": record.clone()}, "custom_order": "18"}),
        json!({"custom": {"a": record.clone()}, "order": [18]}),
        json!({"custom": null}),
        json!({"custom": {"a": record.clone(), "b": {"id": 19}}}),
    ] {
        let bytes = serde_json::to_vec(&historical).unwrap();
        let result = PythonPresetStage::prepare(&bytes, &templates);
        assert!(
            result.is_err(),
            "invalid historical source was accepted: {historical}"
        );
    }
}

#[test]
fn python_dictionary_ids_respect_inner_numeric_string_selection() {
    let (current, templates) = fixture();
    let mut source: Value = serde_json::from_slice(&current).unwrap();
    source["presets"]["18"]["id"] = json!("18");
    let source_bytes = serde_json::to_vec(&source).unwrap();
    let accepted = PythonPresetStage::prepare(&source_bytes, &templates).unwrap();
    assert_eq!(accepted.preview.converted_ids, [18]);
    source["presets"]["18"]["id"] = json!("19");
    let source_bytes = serde_json::to_vec(&source).unwrap();
    let blocked = PythonPresetStage::prepare(&source_bytes, &templates).unwrap();
    assert_eq!(blocked.preview.blocked_ids, [18]);
    assert_eq!(blocked.preview.blocked_count(), 1);
    source["presets"]["18"]["id"] = json!(false);
    let blocked =
        PythonPresetStage::prepare(&serde_json::to_vec(&source).unwrap(), &templates).unwrap();
    assert_eq!(blocked.preview.blocked_ids, [18]);
}

#[test]
fn historical_list_without_explicit_order_preserves_python_list_position() {
    let (source, templates) = fixture();
    let mut first: Value = serde_json::from_slice(&source).unwrap();
    first = first["presets"]["18"].clone();
    let mut second = first.clone();
    second["id"] = json!(19);
    second["name"] = json!("first in Python list");
    for value in [
        json!({"presets": [second.clone(), first.clone()]}),
        json!([second.clone(), first.clone()]),
    ] {
        let stage =
            PythonPresetStage::prepare(&serde_json::to_vec(&value).unwrap(), &templates).unwrap();
        assert!(!stage.preview.python_order_imported);
        let review = stage
            .preview_preferences(br#"{"LAST_API_PRESET_ID":19}"#)
            .unwrap();
        assert!(!review.order_explicit);
        assert_eq!(review.user_order, [19, 18]);
    }
}

#[test]
fn python_semantics_without_a_rust_preset_equivalent_block_entire_stage() {
    let (presets, templates) = fixture();
    let changes = [
        (
            "model_settings.schema_override",
            json!({"id": "user-schema", "fields": []}),
        ),
        ("model_settings.sparse_response", json!(false)),
        (
            "model_settings.support_overrides.tools_native",
            json!(false),
        ),
        ("model_settings.support_overrides.streaming", json!(false)),
        ("openrouter_routing", json!({"providers": ["first"]})),
        ("reserve_keys_distribute", json!(true)),
        ("model_profiles", json!([{"model": "gemini-2.5-flash"}])),
        ("known_models", json!(["gemini-2.5-flash"])),
        ("preferred_model", json!("gemini-2.5-pro")),
        ("test_url", json!("https://custom-test.invalid")),
        (
            "model_settings.enabled",
            json!(["max_tokens", "max_tokens"]),
        ),
        ("model_settings", Value::Null),
    ];
    for (path, value) in changes {
        let mut document: Value = serde_json::from_slice(&presets).unwrap();
        let mut field = &mut document["presets"]["18"];
        for segment in path
            .split('.')
            .collect::<Vec<_>>()
            .iter()
            .take(path.split('.').count() - 1)
        {
            field = &mut field[*segment];
        }
        let last = path.rsplit('.').next().unwrap();
        field[last] = value;
        let changed = serde_json::to_vec(&document).unwrap();
        let review = PythonPresetStage::prepare(&changed, &templates).unwrap();
        assert_eq!(review.preview.blocked_ids, [18], "{path}");
        assert_eq!(review.preview.blocked_count(), 1, "{path}");
        let field_name = if path.starts_with("model_settings.support_overrides.") {
            "model_settings.support_overrides"
        } else {
            path
        };
        assert_eq!(
            review.preview.unsupported_fields_by_id[&18],
            [field_name],
            "{path}"
        );
        assert!(!format!("{:?}", review.preview).contains("private-fixture-key"));
        let (_parent, destination) = fresh_directory();
        assert!(
            matches!(
                review.stage_new_directory(&destination),
                Err(PythonPresetStageError::Blocked(1))
            ),
            "{path}"
        );
        assert!(!destination.exists(), "{path}");
    }
}

#[test]
fn supported_capability_selection_and_empty_optional_fields_still_import() {
    let (presets, templates) = fixture();
    let mut document: Value = serde_json::from_slice(&presets).unwrap();
    let preset = &mut document["presets"]["18"];
    preset["model_settings"]["support_overrides"] = json!({"structured_output": false});
    preset["model_settings"]["schema_override"] = json!({});
    preset["openrouter_routing"] = json!({});
    preset["model_profiles"] = json!([]);
    preset["known_models"] = Value::Null;
    preset["reserve_keys_distribute"] = json!(false);
    preset["preferred_model"] = json!("gemini-2.5-flash");
    preset["test_url"] = json!("");
    let review =
        PythonPresetStage::prepare(&serde_json::to_vec(&document).unwrap(), &templates).unwrap();
    assert_eq!(review.preview.blocked_count(), 0);
    assert_eq!(review.preview.converted_ids, [18]);
    assert!(review.preview.unsupported_fields_by_id.is_empty());
    let (_parent, destination) = fresh_directory();
    review.stage_new_directory(&destination).unwrap();
    let catalog = std::fs::read_to_string(destination.join("presets.json")).unwrap();
    assert!(!catalog.contains("private-fixture-key"));
    assert!(catalog.contains("structured_output"));
}
