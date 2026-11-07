use neuromita::settings::{PythonPresetStage, PythonPresetStageError};
use serde_json::{Value, json};

fn inputs() -> (Vec<u8>, Vec<u8>) {
    let templates = json!({"templates":{"3":{"name":"Google", "protocol_id":"google_gemini_default", "url":"", "default_model":"gemini-2.5-flash"}}});
    let record = json!({
        "id":18,"name":"Private preset","base":3,"protocol_id":"", "protocol_overrides":{},
        "url":"","default_model":"gemini-2.5-flash","key":"do-not-print-credential",
        "model_settings":{"values":{"temperature":1.0,"max_tokens":3000,"thinking_level":"minimal"},"enabled":["max_tokens","thinking_level"]}
    });
    let mut second = record.clone();
    second["id"] = json!(19);
    second["name"] = json!("Other user preset");
    (
        serde_json::to_vec(&json!({"presets":{"18":record,"19":second},"order":[19]})).unwrap(),
        serde_json::to_vec(&templates).unwrap(),
    )
}

fn plan() -> PythonPresetStage {
    let (presets, templates) = inputs();
    let plan = PythonPresetStage::prepare(&presets, &templates).unwrap();
    assert_eq!(plan.preview.converted_ids, [18, 19]);
    plan
}

#[test]
fn valid_selected_user_id_and_python_order_completion() {
    let review = plan()
        .preview_preferences(
            br#"{"LAST_API_PRESET_ID":"19", "UNRELATED_SECRET":"do-not-print-credential"}"#,
        )
        .unwrap();
    assert_eq!(review.selected_user_id, Some(19));
    assert!(review.selection_explicit);
    assert_eq!(review.user_order, [19, 18]);
    assert!(review.order_explicit);
    assert!(review.activation_not_performed);
    assert!(!format!("{review:?}").contains("do-not-print-credential"));
}

#[test]
fn missing_selection_is_not_replaced_with_first_or_template_id() {
    let review = plan().preview_preferences(br#"{"LANGUAGE":"RU"}"#).unwrap();
    assert_eq!(review.selected_user_id, None);
    assert!(!review.selection_explicit);
    assert_eq!(review.user_order, [19, 18]);
}

#[test]
fn missing_order_uses_python_sorted_user_id_completion() {
    let (presets, templates) = inputs();
    let mut value: Value = serde_json::from_slice(&presets).unwrap();
    value.as_object_mut().unwrap().remove("order");
    let plan =
        PythonPresetStage::prepare(&serde_json::to_vec(&value).unwrap(), &templates).unwrap();
    let review = plan
        .preview_preferences(br#"{"LAST_API_PRESET_ID":18}"#)
        .unwrap();
    assert!(!review.order_explicit);
    assert_eq!(review.user_order, [18, 19]);
}

#[test]
fn templates_or_unknown_user_id_cannot_be_selected() {
    for source in [
        br#"{"LAST_API_PRESET_ID":3}"#.as_slice(),
        br#"{"LAST_API_PRESET_ID":999}"#,
    ] {
        assert!(matches!(
            plan().preview_preferences(source),
            Err(PythonPresetStageError::Invalid(_))
        ));
    }
}

#[test]
fn malformed_or_duplicate_settings_never_pass_review() {
    for source in [
        br#"{"LAST_API_PRESET_ID":18,"LAST_API_PRESET_ID":19}"#.as_slice(),
        br#"{"LAST_API_PRESET_ID":true}"#,
        br#"{"LAST_API_PRESET_ID":-1}"#,
        br#"{"LAST_API_PRESET_ID":18.5}"#,
        br#"[18]"#,
        br#"not json"#,
    ] {
        assert!(plan().preview_preferences(source).is_err());
    }
}

#[test]
fn malformed_order_never_silently_normalizes_unknowns_or_duplicates() {
    let (presets, templates) = inputs();
    for bad in [json!([19, 19]), json!([19, 999]), json!([3]), json!("19")] {
        let mut value: Value = serde_json::from_slice(&presets).unwrap();
        value["order"] = bad;
        let plan =
            PythonPresetStage::prepare(&serde_json::to_vec(&value).unwrap(), &templates).unwrap();
        assert!(
            plan.preview_preferences(br#"{"LAST_API_PRESET_ID":19}"#)
                .is_err()
        );
    }
}

#[test]
fn preferences_review_and_exact_source_are_only_in_new_private_stage_directory() {
    let source = br#"{"LAST_API_PRESET_ID":"19", "API_KEY":"private-secret"}"#;
    let (plan, review) = plan().with_preferences(source).unwrap();
    assert_eq!(review.selected_user_id, Some(19));
    let root = std::env::temp_dir().join(format!("neuromita-pref-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let target = root.join("Unicode Каталог review");
    plan.stage_new_directory(&target).unwrap();
    assert_eq!(
        std::fs::read(target.join("python-source-settings.json")).unwrap(),
        source
    );
    let review_file =
        std::fs::read_to_string(target.join("python-preset-preferences-review.json")).unwrap();
    assert!(!review_file.contains("private-secret"));
    assert!(review_file.contains("\"selected_user_id\": 19"));
    assert!(!target.join("settings.json").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(target.join("python-source-settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_selection_causes_zero_publication() {
    let parent = std::env::temp_dir().join(format!("neuromita-pref-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&parent).unwrap();
    let target = parent.join("new-review");
    assert!(
        plan()
            .with_preferences(br#"{"LAST_API_PRESET_ID":999}"#)
            .is_err()
    );
    assert!(!target.exists());
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn explicit_null_selection_is_distinct_from_missing_selection() {
    let review = plan()
        .preview_preferences(br#"{"LAST_API_PRESET_ID":null}"#)
        .unwrap();
    assert!(review.selection_explicit);
    assert_eq!(review.selected_user_id, None);
}

#[test]
fn rust_settings_envelope_is_not_accepted_as_python_settings() {
    assert!(
        plan()
            .preview_preferences(br#"{"revision":7,"settings":{}}"#)
            .is_err()
    );
}

#[test]
fn repeated_review_attachment_fails_explicitly_without_publishing() {
    let (once, _) = plan()
        .with_preferences(br#"{"LAST_API_PRESET_ID":18}"#)
        .unwrap();
    assert!(
        once.with_preferences(br#"{"LAST_API_PRESET_ID":19}"#)
            .is_err()
    );
}

#[test]
fn offline_two_file_activation_does_not_apply_reviewed_selection_or_order() {
    use neuromita::settings::PresetActivation;

    let root = std::env::temp_dir().join(format!("neuromita-pref-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let review_dir = root.join("review");
    let active_dir = root.join("UserData");
    std::fs::create_dir(&active_dir).unwrap();

    let settings = br#"{"revision":9,"settings":"existing opaque bytes"}"#;
    std::fs::write(active_dir.join("settings.json"), settings).unwrap();
    let (review, prefs) = plan()
        .with_preferences(br#"{"LAST_API_PRESET_ID":19}"#)
        .unwrap();
    assert_eq!(prefs.selected_user_id, Some(19));
    review.stage_new_directory(&review_dir).unwrap();
    PresetActivation::apply(&review_dir, &active_dir).unwrap();

    assert_eq!(
        std::fs::read(active_dir.join("settings.json")).unwrap(),
        settings
    );
    assert!(
        !active_dir
            .join("python-preset-preferences-review.json")
            .exists()
    );
    assert!(active_dir.join("presets.json").exists());
    assert!(active_dir.join("preset-credentials.json").exists());
    assert!(
        review_dir
            .join("python-preset-preferences-review.json")
            .exists()
    );
    std::fs::remove_dir_all(root).unwrap();
}
