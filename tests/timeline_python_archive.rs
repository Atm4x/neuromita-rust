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

#[test]
fn python_detached_conversion_preserves_native_fact_and_receipt_identity() {
    use neuromita::timeline::EventId;
    use neuromita::timeline::domain::{EventKind, TimelineStore};
    let source = inspect_python_archive(PYTHON_ARCHIVE).unwrap();
    let destination = TimelineStore::new();
    let bound = source
        .bind_memory(&destination, &explicit_observers())
        .unwrap();
    let prepared = bound.prepare_canonical().unwrap();
    assert_eq!(prepared.session_id(), bound.native_session());
    assert_eq!(prepared.receipt_count(), 2);
    let player = source
        .identities
        .events
        .iter()
        .find(|e| e.source == "59bcbaeb163b57fb939cc4589b16d8c4")
        .unwrap();
    let fact = prepared.fact(EventId(player.candidate_uuid)).unwrap();
    assert_eq!(fact.draft.kind, EventKind::PlayerMessage);
    assert_eq!(
        fact.draft.external_message_id.as_deref(),
        Some("message:παράδειγμα")
    );
    assert_eq!(fact.draft.actor.as_deref(), Some("Player"));
    assert!(
        fact.draft
            .observers
            .contains(&neuromita::character::CharacterId::Crazy)
    );
    assert_eq!(fact.draft.payload["content"], json!("Привет"));
    assert_eq!(fact.draft.occurred_at_unix_ms, 1794032924733);
    assert_eq!(prepared.provenance(), &bound.provenance());
    assert!(
        destination
            .session_provenance(bound.native_session())
            .is_none()
    );
}

#[test]
fn python_detached_conversion_rehydrates_python_image_attachment_as_typed_parts() {
    use neuromita::timeline::EventId;
    use neuromita::timeline::domain::{TimelineMessagePart, TimelineStore, parse_message_content};
    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let bound = source
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    let prepared = bound.prepare_canonical().unwrap();
    let source_id = source
        .identities
        .events
        .iter()
        .find(|id| id.source == "59bcbaeb163b57fb939cc4589b16d8c4")
        .unwrap();
    let fact = prepared.fact(EventId(source_id.candidate_uuid)).unwrap();
    let parts = parse_message_content(fact.draft.payload.get("content").unwrap()).unwrap();
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], TimelineMessagePart::Text { text } if text == "Привет"));
    assert!(
        matches!(&parts[1], TimelineMessagePart::Image { asset } if asset.media_type == "image/png" && asset.size == 34 && asset.digest == "aee1c651e5a199456290787d09ff5a7fa3c7e83f4715aad94dd1c6c95df13433")
    );
    assert!(!fact.draft.payload.contains_key("attachments"));
    assert_eq!(prepared.receipt_count(), 2);
}

#[test]
fn python_detached_conversion_rejects_event_observed_disclosure() {
    use neuromita::timeline::domain::TimelineStore;
    let modified = alter(|doc| {
        doc["branches"][0]["head_sequence"] = json!(4);
        let observation = json!({
            "event_id":"observation:new", "session_id":"session:русский",
            "branch_id":"child", "sequence":4, "kind":"EventObserved",
            "payload":{}, "actor_id":null, "observers":["A"],
            "links":[["observes", "59bcbaeb163b57fb939cc4589b16d8c4"]],
            "occurred_at":"2026-11-07T06:28:44.739655+00:00", "schema_version":1
        });
        doc["events"]
            .as_array_mut()
            .unwrap()
            .push(observation.clone());
        doc["operations"].as_array_mut().unwrap().push(json!({
            "branch_id":"child", "key":"observe", "event_ids":["observation:new"],
            "intent": {"command":{},"events":[{
                "event_id":"observation:new","kind":"EventObserved", "payload":{},
                "actor_id":null,"observers":["A"], "schema_version":1,
                "links":[["observes","59bcbaeb163b57fb939cc4589b16d8c4"]]
            }]}
        }));
    });
    let source = inspect_python_archive(&modified).unwrap();
    let bound = source
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    assert!(matches!(
        bound.prepare_canonical(),
        Err(PythonArchiveError::Invalid(
            "Python event semantics not representable in Rust"
        ))
    ));
}

#[test]
fn python_detached_conversion_maps_character_reply_across_a_fork() {
    use neuromita::timeline::EventId;
    use neuromita::timeline::domain::{CausalRelation, EventKind, TimelineStore};
    let modified = alter(|doc| {
        doc["branches"][0]["head_sequence"] = json!(4);
        let reply = json!({
            "event_id": "reply:character", "session_id":"session:русский",
            "branch_id":"child", "sequence":4, "kind":"CharacterMessage",
            "payload":{"content":"Ответ","message_id":"answer:1"},
            "actor_id":"A", "observers":["A"],
            "links":[["reply_to","59bcbaeb163b57fb939cc4589b16d8c4"]],
            "occurred_at":"2026-11-07T06:28:44.740555+00:00", "schema_version":1
        });
        doc["events"].as_array_mut().unwrap().push(reply);
        doc["operations"].as_array_mut().unwrap().push(json!({
            "branch_id":"child", "key":"reply", "event_ids":["reply:character"],
            "intent": {"command":{}, "events":[{
                "event_id":"reply:character", "kind":"CharacterMessage",
                "payload":{"content":"Ответ","message_id":"answer:1"},
                "actor_id":"A","observers":["A"],"schema_version":1,
                "links":[["reply_to","59bcbaeb163b57fb939cc4589b16d8c4"]]
            }]}
        }));
    });
    let source = inspect_python_archive(&modified).unwrap();
    let bound = source
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    let prepared = bound.prepare_canonical().unwrap();
    assert_eq!(prepared.receipt_count(), 3);
    let source_id = source
        .identities
        .events
        .iter()
        .find(|id| id.source == "reply:character")
        .unwrap();
    let fact = prepared.fact(EventId(source_id.candidate_uuid)).unwrap();
    assert_eq!(fact.draft.kind, EventKind::CharacterMessage);
    assert_eq!(fact.draft.actor.as_deref(), Some("Crazy"));
    assert_eq!(fact.draft.links[0].relation, CausalRelation::ReplyTo);
    let target = source
        .identities
        .events
        .iter()
        .find(|id| id.source == "59bcbaeb163b57fb939cc4589b16d8c4")
        .unwrap();
    assert_eq!(fact.draft.links[0].event_id, EventId(target.candidate_uuid));
}

#[test]
fn python_detached_conversion_rejects_unverified_native_media_part() {
    use neuromita::timeline::domain::TimelineStore;
    let source = alter(|doc| {
        doc["events"][1]["payload"]["content"] = json!([{
            "type":"image", "asset":{
                "digest":"0".repeat(64), "size":16, "media_type":"image/png"
            }
        }]);
        doc["operations"][1]["intent"]["events"][0]["payload"] =
            doc["events"][1]["payload"].clone();
    });
    let parsed = inspect_python_archive(&source).unwrap();
    let bound = parsed
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    assert!(matches!(
        bound.prepare_canonical(),
        Err(PythonArchiveError::Invalid(
            "Python media requires typed native conversion"
        ))
    ));
}

#[test]
fn python_detached_conversion_preserves_attachment_order_and_rejects_unknown_placement() {
    use neuromita::timeline::domain::{TimelineMessagePart, TimelineStore, parse_message_content};
    let duplicated = {
        let mut doc: Value = serde_json::from_slice(include_bytes!(
            "fixtures/python_session_archive_asset_20261107.json"
        ))
        .unwrap();
        let attachment = doc["events"][1]["payload"]["attachments"][0].clone();
        doc["events"][1]["payload"]["attachments"]
            .as_array_mut()
            .unwrap()
            .push(attachment);
        doc["operations"][1]["intent"]["events"][0]["payload"] =
            doc["events"][1]["payload"].clone();
        serde_json::to_vec(&doc).unwrap()
    };
    let parsed = inspect_python_archive(&duplicated).unwrap();
    let bound = parsed
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    let prepared = bound.prepare_canonical().unwrap();
    let fact = prepared
        .fact(neuromita::timeline::EventId(
            parsed
                .identities
                .events
                .iter()
                .find(|id| id.source == "59bcbaeb163b57fb939cc4589b16d8c4")
                .unwrap()
                .candidate_uuid,
        ))
        .unwrap();
    let parts = parse_message_content(&fact.draft.payload["content"]).unwrap();
    assert_eq!(parts.len(), 3);
    assert!(matches!(&parts[0], TimelineMessagePart::Text { text } if text == "Привет"));
    assert!(matches!(&parts[1], TimelineMessagePart::Image { .. }));
    assert_eq!(parts[1], parts[2]);

    let misplaced = inspect_python_archive(&with_binary(b"image bytes")).unwrap();
    let bound = misplaced
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    assert!(matches!(
        bound.prepare_canonical(),
        Err(PythonArchiveError::Invalid(
            "Python media requires typed native conversion"
        ))
    ));
}

fn python_presentation_fixture(source: &[u8]) -> Vec<u8> {
    let mut doc: Value = serde_json::from_slice(source).unwrap();
    let original = "59bcbaeb163b57fb939cc4589b16d8c4";
    let existing_message = doc["events"][1]["payload"]["message_id"].clone();
    let mutations = [
        (
            "revision:1",
            "MessageRevised",
            json!({
                "event_id": original,
                "message": {"content":"Исправлено", "message_id":existing_message}
            }),
        ),
        ("hidden:1", "MessageHidden", json!({"event_ids":[original]})),
        (
            "restored:1",
            "MessageRestored",
            json!({"event_id":original}),
        ),
    ];
    for (index, (id, kind, payload)) in mutations.into_iter().enumerate() {
        let event = json!({
            "event_id":id, "session_id":"session:русский", "branch_id":"main",
            "sequence": 3 + index, "kind":kind, "payload":payload,
            "actor_id":null, "observers":["A"], "links":[],
            "occurred_at": format!("2026-11-07T06:28:44.74{}917+00:00", index),
            "schema_version":1
        });
        doc["operations"].as_array_mut().unwrap().push(json!({
            "branch_id":"main", "key":format!("present:{id}"),
            "event_ids":[id], "intent":{"command":{},"events":[{
                "event_id":id, "kind":kind, "payload":event["payload"].clone(),
                "actor_id":null,"observers":["A"], "links":[], "schema_version":1
            }]}
        }));
        doc["events"].as_array_mut().unwrap().push(event);
    }
    doc["branches"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|branch| branch["branch_id"] == "main")
        .unwrap()["head_sequence"] = json!(5);
    serde_json::to_vec(&doc).unwrap()
}

#[test]
fn python_presentation_actions_preserve_canonical_targets_receipts_and_identity() {
    use neuromita::timeline::EventId;
    use neuromita::timeline::domain::{EventKind, TimelineStore};
    let source = inspect_python_archive(&python_presentation_fixture(PYTHON_ARCHIVE)).unwrap();
    let bound = source
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    let prepared = bound.prepare_canonical().unwrap();
    assert_eq!(prepared.receipt_count(), 5);
    let native = |key: &str| {
        EventId(
            source
                .identities
                .events
                .iter()
                .find(|event| event.source == key)
                .unwrap()
                .candidate_uuid,
        )
    };
    let original = native("59bcbaeb163b57fb939cc4589b16d8c4");
    let revision = &prepared.fact(native("revision:1")).unwrap().draft;
    assert_eq!(revision.kind, EventKind::MessageRevised);
    assert_eq!(revision.payload["event_id"], json!(original.0.to_string()));
    assert_eq!(revision.payload["message"]["content"], "Исправлено");
    assert!(revision.payload["message"].get("message_id").is_none());
    let hidden = &prepared.fact(native("hidden:1")).unwrap().draft;
    assert_eq!(hidden.kind, EventKind::MessageHidden);
    assert_eq!(hidden.payload["event_ids"], json!([original.0.to_string()]));
    let restored = &prepared.fact(native("restored:1")).unwrap().draft;
    assert_eq!(restored.kind, EventKind::MessageRestored);
    assert_eq!(restored.payload["event_id"], json!(original.0.to_string()));
    assert_eq!(prepared.provenance(), &bound.provenance());
}

#[test]
fn python_revision_rejects_ambiguous_layout_without_publishing_native_facts() {
    use neuromita::timeline::domain::TimelineStore;
    let mut doc: Value =
        serde_json::from_slice(&python_presentation_fixture(PYTHON_ARCHIVE)).unwrap();
    for event in doc["events"].as_array_mut().unwrap() {
        if event["kind"] == "MessageRevised" {
            event["payload"]["content"] = json!("contradictory");
        }
    }
    for operation in doc["operations"].as_array_mut().unwrap() {
        if operation["key"] == "present:revision:1" {
            operation["intent"]["events"][0]["payload"]["content"] = json!("contradictory");
        }
    }
    let source = inspect_python_archive(&serde_json::to_vec(&doc).unwrap()).unwrap();
    let destination = TimelineStore::new();
    let bound = source
        .bind_memory(&destination, &explicit_observers())
        .unwrap();
    assert!(matches!(
        bound.prepare_canonical(),
        Err(PythonArchiveError::Invalid(
            "unsupported Python presentation payload"
        ))
    ));
    assert!(
        destination
            .session_provenance(bound.native_session())
            .is_none()
    );
}

#[test]
fn python_revision_converts_verified_binary_attachments_without_cross_character_disclosure() {
    use neuromita::timeline::EventId;
    use neuromita::timeline::domain::{TimelineMessagePart, TimelineStore, parse_message_content};
    let mut doc: Value = serde_json::from_slice(&python_presentation_fixture(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    )))
    .unwrap();
    let image = doc["events"][1]["payload"]["attachments"][0].clone();
    for event in doc["events"].as_array_mut().unwrap() {
        if event["kind"] == "MessageRevised" {
            event["payload"]["message"]["attachments"] = json!([image]);
        }
    }
    let revision_payload = doc["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["kind"] == "MessageRevised")
        .unwrap()["payload"]
        .clone();
    for operation in doc["operations"].as_array_mut().unwrap() {
        if operation["key"] == "present:revision:1" {
            operation["intent"]["events"][0]["payload"] = revision_payload.clone();
        }
    }
    let source = inspect_python_archive(&serde_json::to_vec(&doc).unwrap()).unwrap();
    let bound = source
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    let prepared = bound.prepare_canonical().unwrap();
    let revision = prepared
        .fact(EventId(
            source
                .identities
                .events
                .iter()
                .find(|event| event.source == "revision:1")
                .unwrap()
                .candidate_uuid,
        ))
        .unwrap();
    let parts = parse_message_content(&revision.draft.payload["message"]["content"]).unwrap();
    assert_eq!(parts.len(), 2);
    assert!(matches!(&parts[0], TimelineMessagePart::Text { text } if text == "Исправлено"));
    assert!(
        matches!(&parts[1], TimelineMessagePart::Image { asset } if asset.media_type == "image/png" && asset.size == 34)
    );
    assert_eq!(revision.draft.observers.len(), 1);
    assert!(
        revision
            .draft
            .observers
            .contains(&neuromita::character::CharacterId::Crazy)
    );
}

#[test]
fn python_verified_archive_imports_atomically_into_memory_and_refuses_reimport() {
    use neuromita::timeline::domain::TimelineStore;
    use neuromita::timeline::storage::archive::export_memory;
    use neuromita::timeline::storage::assets::TimelineAssetStore;
    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let mut destination = TimelineStore::new();
    let bound = source
        .bind_memory(&destination, &explicit_observers())
        .unwrap();
    let root = std::env::temp_dir().join(format!("nm-python-b31-memory-{}", uuid::Uuid::new_v4()));
    let store = TimelineAssetStore::new(&root, 16 * 1024 * 1024).unwrap();
    let session = bound.import_memory(&mut destination, &store).unwrap();
    assert_eq!(session, bound.native_session());
    assert_eq!(
        destination.session_provenance(session),
        Some(&bound.provenance())
    );
    let native_archive = export_memory(&destination, session, &store).unwrap();
    assert!(!native_archive.is_empty());
    assert!(bound.import_memory(&mut destination, &store).is_err());
    assert_eq!(
        destination.session_provenance(session),
        Some(&bound.provenance())
    );
    assert_eq!(
        export_memory(&destination, session, &store).unwrap(),
        native_archive
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(feature = "sqlite-system")]
#[test]
fn python_verified_archive_imports_into_sqlite_and_survives_reopen() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    use neuromita::timeline::storage::archive::export_sqlite;
    use neuromita::timeline::storage::assets::TimelineAssetStore;
    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let root = std::env::temp_dir().join(format!("nm-python-b31-sqlite-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("timeline.sqlite");
    let store = TimelineAssetStore::new(&root.join("assets"), 16 * 1024 * 1024).unwrap();
    let session;
    let native_archive;
    {
        let mut destination = SqliteTimelineRepository::open(&path).unwrap();
        let bound = source
            .bind_sqlite(&destination, &explicit_observers())
            .unwrap();
        session = bound.import_sqlite(&mut destination, &store).unwrap();
        native_archive = export_sqlite(&destination, session, &store).unwrap();
        assert!(bound.import_sqlite(&mut destination, &store).is_err());
    }
    {
        let reopened = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(
            export_sqlite(&reopened, session, &store).unwrap(),
            native_archive
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(feature = "sqlite-system")]
#[test]
fn python_sqlite_import_rolls_back_events_receipts_and_provenance_on_write_faults() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    use neuromita::timeline::storage::archive::export_sqlite;
    use neuromita::timeline::storage::assets::TimelineAssetStore;

    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let root =
        std::env::temp_dir().join(format!("nm-python-sqlite-fault-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("timeline.sqlite");
    let assets = TimelineAssetStore::new(root.join("assets"), 16 * 1024 * 1024).unwrap();
    let mut destination = SqliteTimelineRepository::open(&path).unwrap();
    let bound = source
        .bind_sqlite(&destination, &explicit_observers())
        .unwrap();
    let session = bound.native_session();

    for (name, trigger) in [
        (
            "fail_event",
            "CREATE TRIGGER fail_event BEFORE INSERT ON timeline_events BEGIN SELECT RAISE(ABORT, 'test event failure'); END;",
        ),
        (
            "fail_provenance",
            "CREATE TRIGGER fail_provenance BEFORE INSERT ON timeline_session_provenance BEGIN SELECT RAISE(ABORT, 'test provenance failure'); END;",
        ),
    ] {
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(trigger)
            .unwrap();
        assert!(bound.import_sqlite(&mut destination, &assets).is_err());
        let connection = rusqlite::Connection::open(&path).unwrap();
        for table in [
            "timeline_branches",
            "timeline_events",
            "timeline_observers",
            "timeline_receipts",
            "timeline_session_provenance",
        ] {
            let rows: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(rows, 0, "partial write to {table} after {name}");
        }
        connection
            .execute_batch(&format!("DROP TRIGGER {name}"))
            .unwrap();
        drop(connection);
        let reopened = SqliteTimelineRepository::open(&path).unwrap();
        assert!(export_sqlite(&reopened, session, &assets).is_err());
    }

    assert_eq!(
        bound.import_sqlite(&mut destination, &assets).unwrap(),
        session
    );
    let expected = export_sqlite(&destination, session, &assets).unwrap();
    drop(destination);
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert_eq!(
        export_sqlite(&reopened, session, &assets).unwrap(),
        expected
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(feature = "sqlite-system")]
#[test]
fn python_sqlite_import_stale_binding_cannot_overwrite_another_writer() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    use neuromita::timeline::storage::archive::export_sqlite;
    use neuromita::timeline::storage::assets::TimelineAssetStore;

    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let root =
        std::env::temp_dir().join(format!("nm-python-sqlite-writers-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("timeline.sqlite");
    let assets = TimelineAssetStore::new(root.join("assets"), 16 * 1024 * 1024).unwrap();
    let mut first = SqliteTimelineRepository::open(&path).unwrap();
    let mut second = SqliteTimelineRepository::open(&path).unwrap();
    let stale = source.bind_sqlite(&second, &explicit_observers()).unwrap();
    let fresh = source.bind_sqlite(&first, &explicit_observers()).unwrap();
    let session = fresh.import_sqlite(&mut first, &assets).unwrap();
    let original = export_sqlite(&first, session, &assets).unwrap();
    assert!(stale.import_sqlite(&mut second, &assets).is_err());
    assert_eq!(export_sqlite(&second, session, &assets).unwrap(), original);
    drop(first);
    drop(second);
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert_eq!(
        export_sqlite(&reopened, session, &assets).unwrap(),
        original
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(feature = "sqlite-system")]
#[test]
fn python_sqlite_import_reexports_losslessly_through_native_memory_and_sqlite() {
    use neuromita::timeline::domain::TimelineStore;
    use neuromita::timeline::storage::SqliteTimelineRepository;
    use neuromita::timeline::storage::archive::{
        export_memory, export_sqlite, import_memory as import_native_memory,
        import_sqlite as import_native_sqlite,
    };
    use neuromita::timeline::storage::assets::TimelineAssetStore;

    let source = inspect_python_archive(include_bytes!(
        "fixtures/python_session_archive_asset_20261107.json"
    ))
    .unwrap();
    let root =
        std::env::temp_dir().join(format!("nm-python-cross-storage-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let assets = TimelineAssetStore::new(root.join("assets"), 16 * 1024 * 1024).unwrap();
    let original_path = root.join("original.sqlite");
    let mut original = SqliteTimelineRepository::open(&original_path).unwrap();
    let bound = source
        .bind_sqlite(&original, &explicit_observers())
        .unwrap();
    let provenance = bound.provenance();
    let session = bound.import_sqlite(&mut original, &assets).unwrap();
    let archive = export_sqlite(&original, session, &assets).unwrap();

    let mut memory = TimelineStore::new();
    assert_eq!(
        import_native_memory(&mut memory, &archive, &assets).unwrap(),
        session
    );
    assert_eq!(memory.session_provenance(session), Some(&provenance));
    assert_eq!(export_memory(&memory, session, &assets).unwrap(), archive);
    assert!(import_native_memory(&mut memory, &archive, &assets).is_err());

    let cloned_path = root.join("cloned.sqlite");
    let mut cloned = SqliteTimelineRepository::open(&cloned_path).unwrap();
    assert_eq!(
        import_native_sqlite(&mut cloned, &archive, &assets).unwrap(),
        session
    );
    assert_eq!(export_sqlite(&cloned, session, &assets).unwrap(), archive);
    drop(original);
    drop(cloned);
    let reopened = SqliteTimelineRepository::open(&cloned_path).unwrap();
    assert_eq!(export_sqlite(&reopened, session, &assets).unwrap(), archive);
    std::fs::remove_dir_all(root).unwrap();
}

fn append_disclosure_audit_event(
    archive: &mut Value,
    branch: &str,
    sequence: u64,
    id: &str,
    kind: &str,
    observers: &[&str],
    links: Value,
) {
    let event = json!({
        "event_id":id,
        "session_id":"session:русский",
        "branch_id":branch,
        "sequence":sequence,
        "kind":kind,
        "payload":{},
        "actor_id":null,
        "observers":observers,
        "links":links,
        "occurred_at":"2026-11-07T06:28:45.123456+00:00",
        "schema_version":1
    });
    let mut intent_event = event.clone();
    for field in ["session_id", "branch_id", "sequence", "occurred_at"] {
        intent_event.as_object_mut().unwrap().remove(field);
    }
    intent_event["observers"]
        .as_array_mut()
        .unwrap()
        .sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    archive["events"].as_array_mut().unwrap().push(event);
    archive["operations"].as_array_mut().unwrap().push(json!({
        "branch_id":branch,
        "key":id,
        "event_ids":[id],
        "intent":{"command":{},"events":[intent_event]}
    }));
}

#[test]
fn python_disclosure_audit_distinguishes_prior_knowledge_and_context_reset() {
    let original = "59bcbaeb163b57fb939cc4589b16d8c4";
    let modified = alter(|archive| {
        archive["branches"][1]["head_sequence"] = json!(6);
        append_disclosure_audit_event(
            archive,
            "main",
            3,
            "disclose:initial",
            "EventObserved",
            &["B", "A"],
            json!([["observes", original]]),
        );
        append_disclosure_audit_event(
            archive,
            "main",
            4,
            "disclose:repeated",
            "EventObserved",
            &["B"],
            json!([["observes", original]]),
        );
        append_disclosure_audit_event(
            archive,
            "main",
            5,
            "reset:B",
            "CharacterContextReset",
            &["B"],
            json!([]),
        );
        append_disclosure_audit_event(
            archive,
            "main",
            6,
            "disclose:after-reset",
            "EventObserved",
            &["B"],
            json!([["observes", original]]),
        );
    });
    let inspected = inspect_python_archive(&modified).unwrap();
    let sites = inspected.disclosure_sites();
    assert_eq!(sites.len(), 3);
    assert_eq!(sites[0].branch_id, "main");
    assert_eq!(sites[0].sequence, 3);
    assert_eq!(sites[0].disclosure_event_id, "disclose:initial");
    assert_eq!(sites[0].target_event_id, original);
    assert_eq!(sites[0].witnesses.len(), 2);
    assert_eq!(sites[0].witnesses[0].observer_id, "A");
    assert!(sites[0].witnesses[0].target_known_before);
    assert_eq!(sites[0].witnesses[1].observer_id, "B");
    assert!(!sites[0].witnesses[1].target_known_before);
    assert!(sites[1].witnesses[0].target_known_before);
    assert!(!sites[2].witnesses[0].target_known_before);
    assert!(
        inspect_python_archive(PYTHON_ARCHIVE)
            .unwrap()
            .disclosure_sites()
            .is_empty()
    );
}

#[test]
fn python_disclosure_audit_does_not_inherit_parent_events_after_fork() {
    let original = "59bcbaeb163b57fb939cc4589b16d8c4";
    let modified = alter(|archive| {
        archive["branches"][1]["head_sequence"] = json!(3);
        archive["branches"][0]["head_sequence"] = json!(4);
        append_disclosure_audit_event(
            archive,
            "main",
            3,
            "disclose:parent",
            "EventObserved",
            &["B"],
            json!([["observes", original]]),
        );
        append_disclosure_audit_event(
            archive,
            "child",
            4,
            "disclose:child",
            "EventObserved",
            &["B"],
            json!([["observes", original]]),
        );
    });
    let inspected = inspect_python_archive(&modified).unwrap();
    let sites = inspected.disclosure_sites();
    assert_eq!(sites.len(), 2);
    assert_eq!(sites[0].branch_id, "child");
    assert_eq!(sites[1].branch_id, "main");
    assert!(
        sites
            .iter()
            .all(|site| !site.witnesses[0].target_known_before)
    );
}

#[test]
fn python_redundant_disclosure_is_still_rejected_by_native_conversion() {
    use neuromita::timeline::domain::TimelineStore;
    let original = "59bcbaeb163b57fb939cc4589b16d8c4";
    let modified = alter(|archive| {
        archive["branches"][1]["head_sequence"] = json!(3);
        append_disclosure_audit_event(
            archive,
            "main",
            3,
            "disclose:redundant",
            "EventObserved",
            &["A"],
            json!([["observes", original]]),
        );
    });
    let inspected = inspect_python_archive(&modified).unwrap();
    assert!(inspected.disclosure_sites()[0].witnesses[0].target_known_before);
    let bound = inspected
        .bind_memory(&TimelineStore::new(), &explicit_observers())
        .unwrap();
    assert!(matches!(
        bound.prepare_canonical(),
        Err(PythonArchiveError::Invalid(
            "Python event semantics not representable in Rust"
        ))
    ));
}
