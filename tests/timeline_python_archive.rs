use neuromita::timeline::storage::python_archive::{PythonArchiveError, inspect_python_archive};
use serde_json::{Value, json};

const PYTHON_ARCHIVE: &[u8] = include_bytes!("fixtures/python_session_archive_20261103.json");

fn alter(f: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut doc: Value = serde_json::from_slice(PYTHON_ARCHIVE).unwrap();
    f(&mut doc);
    serde_json::to_vec(&doc).unwrap()
}

#[test]
fn python_archive_retains_unicode_identity_exact_timestamp_and_roundtrip_json() {
    let parsed = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    assert_eq!(parsed.external_session_id(), "session:русский");
    assert_eq!(parsed.event_count(), 3);
    assert_eq!(parsed.identities.branches.len(), 2);
    assert_eq!(parsed.identities.events.len(), 3);
    assert_eq!(
        parsed.identities.external_message_ids,
        vec!["message:παράδειγμα"]
    );
    assert_eq!(parsed.identities.observer_ids, vec!["A"]);
    assert!(parsed.identities.ensure_unoccupied([]).is_ok());
    assert!(
        parsed
            .identities
            .ensure_unoccupied([parsed.identities.session.candidate_uuid])
            .is_err()
    );
    assert_eq!(parsed.identities.branches[0].source, "child");
    let event = &parsed.event_times["59bcbaeb163b57fb939cc4589b16d8c4"];
    assert_eq!(event.source_iso8601, "2026-11-07T06:28:44.733917+00:00");
    assert_eq!(event.unix_microseconds % 1000, 917);
    let rendered: Value = serde_json::from_slice(&parsed.to_python_json().unwrap()).unwrap();
    let input: Value = serde_json::from_slice(PYTHON_ARCHIVE).unwrap();
    assert_eq!(rendered, input);
    assert_eq!(
        inspect_python_archive(&parsed.to_python_json().unwrap())
            .unwrap()
            .identities,
        parsed.identities
    );
}

#[test]
fn different_unicode_external_ids_do_not_alias_in_native_identity_plan() {
    let a = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let changed = alter(|doc| {
        doc["session_id"] = json!("session:русский/β");
        for branch in doc["branches"].as_array_mut().unwrap() {
            branch["session_id"] = json!("session:русский/β");
        }
        for event in doc["events"].as_array_mut().unwrap() {
            event["session_id"] = json!("session:русский/β");
        }
    });
    let b = inspect_python_archive(&changed).unwrap();
    assert_ne!(
        a.identities.session.candidate_uuid,
        b.identities.session.candidate_uuid
    );
    assert_ne!(a.identities.branches, b.identities.branches);
    assert_eq!(b.external_session_id(), "session:русский/β");
}

#[test]
fn rejects_incompatible_version_and_unknown_top_level_field() {
    for bytes in [
        alter(|doc| doc["archive_version"] = json!(2)),
        alter(|doc| doc["unknown"] = json!(true)),
    ] {
        assert!(inspect_python_archive(&bytes).is_err());
    }
}

#[test]
fn rejects_duplicate_branch_event_and_broken_fork_identity() {
    for bytes in [
        alter(|doc| {
            let second = doc["branches"][0].clone();
            doc["branches"].as_array_mut().unwrap().push(second);
        }),
        alter(|doc| {
            let second = doc["events"][0].clone();
            doc["events"].as_array_mut().unwrap().push(second);
        }),
        alter(|doc| doc["branches"][0]["checkpoint_id"] = json!("invalid-checkpoint")),
        alter(|doc| doc["events"][0]["links"] = json!([["caused_by", "unseen"]])),
    ] {
        assert!(inspect_python_archive(&bytes).is_err());
    }
}

#[test]
fn rejects_forged_receipt_and_invalid_temporal_precision() {
    for bytes in [
        alter(|doc| doc["operations"][0]["intent"]["events"][0]["payload"] = json!({"fake":true})),
        alter(|doc| doc["events"][1]["occurred_at"] = json!("2026-11-07T06:28:44")),
        alter(|doc| doc["operations"][0]["event_ids"] = json!(["not-an-event"])),
    ] {
        assert!(inspect_python_archive(&bytes).is_err());
    }
}

#[test]
fn unknown_character_observer_is_not_mapped_to_builtin_character() {
    let parsed = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let value: Value = serde_json::from_slice(&parsed.to_python_json().unwrap()).unwrap();
    assert_eq!(value["events"][1]["observers"], json!(["A"]));
    assert!(neuromita::character::CharacterId::parse("A").is_none());
}

#[test]
fn rejects_broken_binary_reference_before_any_destination_write() {
    let bytes = alter(|doc| {
        doc["events"][1]["payload"]["content"] = json!({
            "type": "image_ref",
            "asset_id": format!("sha256:{}", "0".repeat(64)),
            "media_type": "image/png",
            "byte_size": 4
        });
        doc["operations"][1]["intent"]["events"][0]["payload"] =
            doc["events"][1]["payload"].clone();
    });
    assert!(matches!(
        inspect_python_archive(&bytes),
        Err(PythonArchiveError::Invalid(_))
    ));
}

fn with_binary(bytes: &[u8]) -> Vec<u8> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ring::digest::{Context, SHA256};
    let mut hash = Context::new(&SHA256);
    hash.update(b"image/png\0");
    hash.update(bytes);
    let id = format!(
        "sha256:{}",
        hash.finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    alter(|doc| {
        doc["assets"] = json!([{
            "asset_id": id,
            "media_type": "image/png",
            "base64": STANDARD.encode(bytes),
        }]);
        doc["events"][1]["payload"]["embedded_image"] = json!({
            "type": "image_ref",
            "asset_id": id,
            "media_type": "image/png",
            "byte_size": bytes.len(),
        });
        doc["operations"][1]["intent"]["events"][0]["payload"] =
            doc["events"][1]["payload"].clone();
    })
}

#[test]
fn python_asset_closure_hash_and_base64_roundtrip() {
    let original = with_binary(b"binary payload\x00\x01");
    let inspected = inspect_python_archive(&original).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&inspected.to_python_json().unwrap()).unwrap(),
        serde_json::from_slice::<Value>(&original).unwrap()
    );
    let mut forged: Value = serde_json::from_slice(&original).unwrap();
    forged["assets"][0]["base64"] = json!("YmFk");
    assert!(inspect_python_archive(&serde_json::to_vec(&forged).unwrap()).is_err());
    forged["assets"][0]["base64"] = json!("???");
    assert!(inspect_python_archive(&serde_json::to_vec(&forged).unwrap()).is_err());
}

#[test]
fn python_archive_rejects_missing_duplicate_and_inconsistent_binary() {
    let valid = with_binary(b"image bytes");
    let mut doc: Value = serde_json::from_slice(&valid).unwrap();
    let asset = doc["assets"][0].clone();
    doc["assets"].as_array_mut().unwrap().push(asset);
    assert!(inspect_python_archive(&serde_json::to_vec(&doc).unwrap()).is_err());
    doc["assets"] = json!([]);
    assert!(inspect_python_archive(&serde_json::to_vec(&doc).unwrap()).is_err());
    doc["assets"] = serde_json::from_slice::<Value>(&valid).unwrap()["assets"].clone();
    doc["assets"][0]["media_type"] = json!("image/jpeg");
    assert!(inspect_python_archive(&serde_json::to_vec(&doc).unwrap()).is_err());
}

#[test]
fn timezones_and_microseconds_remain_exact_across_transport_serialization() {
    let modified = alter(|doc| {
        doc["events"][1]["occurred_at"] = json!("2026-11-07T09:28:44.733917+03:00");
    });
    let parsed = inspect_python_archive(&modified).unwrap();
    let timestamp = &parsed.event_times["59bcbaeb163b57fb939cc4589b16d8c4"];
    assert_eq!(timestamp.source_iso8601, "2026-11-07T09:28:44.733917+03:00");
    assert_eq!(timestamp.unix_microseconds % 1000, 917);
    assert_eq!(
        serde_json::from_slice::<Value>(&parsed.to_python_json().unwrap()).unwrap(),
        serde_json::from_slice::<Value>(&modified).unwrap()
    );
}

#[test]
fn forged_checkpoint_with_matching_receipt_is_rejected_semantically() {
    let modified = alter(|doc| {
        doc["events"][2]["payload"] = json!({"label":"not-a-checkpoint"});
        doc["operations"][0]["intent"]["events"][0]["payload"] =
            doc["events"][2]["payload"].clone();
    });
    assert!(inspect_python_archive(&modified).is_err());
}

#[test]
fn python_archive_does_not_disclose_an_unknown_message_via_hidden_edit() {
    let modified = alter(|doc| {
        doc["events"][1]["kind"] = json!("MessageHidden");
        doc["events"][1]["actor_id"] = Value::Null;
        doc["events"][1]["payload"] = json!({"event_ids":["unseen-message"]});
        let event = doc["events"][1].clone();
        doc["operations"][1]["intent"]["events"][0]["kind"] = event["kind"].clone();
        doc["operations"][1]["intent"]["events"][0]["actor_id"] = Value::Null;
        doc["operations"][1]["intent"]["events"][0]["payload"] = event["payload"].clone();
    });
    assert!(inspect_python_archive(&modified).is_err());
}

#[test]
fn real_python_asset_archive_fixture_cross_language_roundtrip() {
    let source = include_bytes!("fixtures/python_session_archive_asset_20261107.json");
    let document: Value = serde_json::from_slice(source).unwrap();
    assert_eq!(document["assets"].as_array().unwrap().len(), 1);
    assert_eq!(
        document["events"][1]["payload"]["attachments"][0]["type"],
        "image_ref"
    );
    let parsed = inspect_python_archive(source).unwrap();
    assert_eq!(parsed.external_session_id(), "session:русский");
    assert_eq!(parsed.identities.observer_ids, ["A"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&parsed.to_python_json().unwrap()).unwrap(),
        document
    );
    let mut corrupted = document;
    corrupted["assets"][0]["base64"] = json!("YmFk");
    assert!(inspect_python_archive(&serde_json::to_vec(&corrupted).unwrap()).is_err());
}

fn explicit_observers() -> std::collections::BTreeMap<String, neuromita::character::CharacterId> {
    std::collections::BTreeMap::from([("A".to_owned(), neuromita::character::CharacterId::Crazy)])
}

#[test]
fn python_import_binding_requires_explicit_observers_and_preserves_source_identity() {
    use neuromita::timeline::domain::TimelineStore;
    let source = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let target = TimelineStore::new();
    assert!(source.bind_memory(&target, &Default::default()).is_err());
    let mut extraneous = explicit_observers();
    extraneous.insert("Ghost".into(), neuromita::character::CharacterId::Ghost);
    assert!(source.bind_memory(&target, &extraneous).is_err());
    let bound = source.bind_memory(&target, &explicit_observers()).unwrap();
    assert_eq!(
        bound.native_session().0,
        source.identities.session.candidate_uuid
    );
    assert_eq!(bound.identities(), &source.identities);
    assert_eq!(bound.source.event_times, source.event_times);
    assert_eq!(bound.source.external_session_id(), "session:русский");
}

#[test]
fn python_import_binding_rejects_native_session_and_event_collisions() {
    use neuromita::timeline::EventId;
    use neuromita::timeline::domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore};
    let source = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let mut target = TimelineStore::new();
    target
        .create_session(
            SessionId(source.identities.session.candidate_uuid),
            BranchId::new(),
        )
        .unwrap();
    assert!(source.bind_memory(&target, &explicit_observers()).is_err());

    let mut target = TimelineStore::new();
    let branch = BranchId::new();
    target.create_session(SessionId::new(), branch).unwrap();
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.id = EventId(source.identities.events[0].candidate_uuid);
    draft.actor = Some("player".into());
    draft
        .observers
        .insert(neuromita::character::CharacterId::Crazy);
    draft.payload.insert("content".into(), json!("native"));
    target
        .append(branch, 0, "native:message", json!({}), vec![draft])
        .unwrap();
    assert!(source.bind_memory(&target, &explicit_observers()).is_err());
    assert_eq!(target.branch(branch).unwrap().head_sequence, 1);
}

#[test]
fn python_import_binding_rejects_aliasing_and_unknown_character_actors() {
    use neuromita::timeline::domain::TimelineStore;
    let modified = alter(|doc| {
        doc["events"][1]["observers"] = json!(["A", "B"]);
        doc["operations"][1]["intent"]["events"][0]["observers"] = json!(["A", "B"]);
    });
    let source = inspect_python_archive(&modified).unwrap();
    let same = std::collections::BTreeMap::from([
        ("A".into(), neuromita::character::CharacterId::Crazy),
        ("B".into(), neuromita::character::CharacterId::Crazy),
    ]);
    assert!(source.bind_memory(&TimelineStore::new(), &same).is_err());
    let different = std::collections::BTreeMap::from([
        ("A".into(), neuromita::character::CharacterId::Crazy),
        ("B".into(), neuromita::character::CharacterId::Kind),
    ]);
    assert!(
        source
            .bind_memory(&TimelineStore::new(), &different)
            .is_ok()
    );
}

#[cfg(feature = "sqlite-system")]
#[test]
fn python_import_binding_detects_sqlite_collision_after_reopen() {
    use neuromita::timeline::domain::{BranchId, SessionId, TimelineRepository};
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let source = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let path =
        std::env::temp_dir().join(format!("nm-python-binding-{}.sqlite", uuid::Uuid::new_v4()));
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        assert!(source.bind_sqlite(&repo, &explicit_observers()).is_ok());
        repo.create_session(
            SessionId(source.identities.session.candidate_uuid),
            BranchId::new(),
        )
        .unwrap();
    }
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert!(
        source
            .bind_sqlite(&reopened, &explicit_observers())
            .is_err()
    );
    std::fs::remove_file(&path).ok();
    std::fs::remove_file(path.with_extension("sqlite-wal")).ok();
    std::fs::remove_file(path.with_extension("sqlite-shm")).ok();
}

#[test]
fn python_import_binding_rejects_unrecognized_player_author_before_mutation() {
    use neuromita::timeline::domain::TimelineStore;
    let modified = alter(|doc| {
        doc["events"][1]["actor_id"] = json!("unknown-player-alias");
        doc["operations"][1]["intent"]["events"][0]["actor_id"] = json!("unknown-player-alias");
    });
    let source = inspect_python_archive(&modified).unwrap();
    let target = TimelineStore::new();
    assert!(source.bind_memory(&target, &explicit_observers()).is_err());
    assert!(source.bind_memory(&target, &explicit_observers()).is_err());
}

#[test]
fn python_import_stages_real_archive_asset_into_existing_content_store() {
    use neuromita::timeline::domain::TimelineStore;
    use neuromita::timeline::storage::assets::TimelineAssetStore;
    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let bindings = std::collections::BTreeMap::from([(
        "A".to_string(),
        neuromita::character::CharacterId::Crazy,
    )]);
    let plan = source
        .bind_memory(&TimelineStore::new(), &bindings)
        .unwrap();
    let root = std::env::temp_dir().join(format!("nm-b25-staged-{}", uuid::Uuid::new_v4()));
    let store = TimelineAssetStore::new(&root, 16 * 1024 * 1024).unwrap();
    let first = plan.stage_assets(&store).unwrap();
    assert_eq!(first.len(), 1);
    let second = plan.stage_assets(&store).unwrap();
    assert_eq!(first, second);
    for (external, reference) in &first {
        assert_eq!(external, &format!("sha256:{}", reference.digest));
        store.verify(reference).unwrap();
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn python_provenance_keeps_exact_time_and_observer_identity_without_history_copy() {
    use neuromita::timeline::domain::{ExternalSessionSource, TimelineStore};
    let source = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let binding = source
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    let provenance = binding.provenance();
    assert_eq!(
        provenance.source,
        ExternalSessionSource::PythonSessionArchive
    );
    assert_eq!(provenance.external_session_id, "session:русский");
    assert_eq!(provenance.external_branches.len(), 2);
    assert_eq!(provenance.external_events.len(), 3);
    assert_eq!(provenance.observer_bindings, explicit_observers());
    let original = &provenance.external_events[&source
        .identities
        .events
        .iter()
        .find(|id| id.source == "59bcbaeb163b57fb939cc4589b16d8c4")
        .unwrap()
        .candidate_uuid];
    assert_eq!(
        original.occurred_at_iso8601,
        "2026-11-07T06:28:44.733917+00:00"
    );
    assert_eq!(original.occurred_at_unix_us % 1000, 917);
    let json = serde_json::to_vec(&provenance).unwrap();
    let restored: neuromita::timeline::domain::SessionProvenance =
        serde_json::from_slice(&json).unwrap();
    assert_eq!(restored, provenance);
}
