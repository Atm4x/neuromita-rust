use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

#[cfg(feature = "sqlite-system")]
use neuromita::timeline::storage::{
    SqliteTimelineRepository,
    archive::{export_sqlite, import_sqlite},
};
use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore},
        storage::{
            archive::{
                SessionArchiveError, export_memory, import_memory, retention_manifest_memory,
            },
            assets::{TimelineAssetRef, TimelineAssetStore},
        },
        view::TimelineMessageQueries,
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nm-session-archive-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn build_session(
    repo: &mut dyn TimelineRepository,
    assets: &TimelineAssetStore,
) -> (
    SessionId,
    BranchId,
    BranchId,
    TimelineAssetRef,
    EventDraft,
    Vec<neuromita::timeline::domain::TimelineFact>,
) {
    let session = SessionId::new();
    let root = BranchId::new();
    repo.create_session(session, root).unwrap();
    let binary = assets
        .import("image/png", Cursor::new(b"hello image payload"))
        .unwrap();
    let mut first = EventDraft::new(EventKind::PlayerMessage);
    first.actor = Some("player".into());
    first.external_message_id = Some("python-msg:abc/α".into());
    first.observers.insert(CharacterId::Crazy);
    first.payload.insert(
        "content".into(),
        json!([{"type":"text","text":"start"},{"type":"image","asset":binary}]),
    );
    let original = first.clone();
    let initial = repo
        .append(
            root,
            0,
            "message:1",
            json!({"command":"write"}),
            vec![first],
        )
        .unwrap();
    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    repo.append(root, 1, "checkpoint:1", json!({}), vec![checkpoint])
        .unwrap();
    let fork = BranchId::new();
    repo.fork(root, checkpoint_id, fork).unwrap();
    let mut continuation = EventDraft::new(EventKind::PlayerMessage);
    continuation.actor = Some("player".into());
    continuation.observers.insert(CharacterId::Crazy);
    continuation.payload.insert(
        "content".into(),
        json!([{"type":"text","text":"branch only"}]),
    );
    continuation
        .links
        .push(neuromita::timeline::domain::CausalLink {
            relation: neuromita::timeline::domain::CausalRelation::CausedBy,
            event_id: original.id,
        });
    repo.append(fork, 3, "message:fork", json!({}), vec![continuation])
        .unwrap();
    (session, root, fork, binary, original, initial)
}

fn bytes_with_change(bytes: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn genuine_python_archive_is_rejected_without_lossy_implicit_conversion() {
    let fixture = include_bytes!("fixtures/python_session_archive_20261103.json");
    let document: Value = serde_json::from_slice(fixture).unwrap();
    assert_eq!(document["archive_version"], 1);
    assert_eq!(document["session_id"], "session:русский");
    assert_eq!(document["root_branch_id"], "main");
    assert!(
        document["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|branch| {
                branch["branch_id"] == "child" && branch["parent_branch_id"] == "main"
            })
    );
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("target"), 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let existing = BranchId::new();
    timeline.create_session(SessionId::new(), existing).unwrap();
    assert!(import_memory(&mut timeline, fixture, &assets).is_err());
    assert_eq!(timeline.branch(existing).unwrap().head_sequence, 0);
    assert!(timeline.world(existing).unwrap().is_empty());
    assert!(assets.inventory(&[]).unwrap().retained.is_empty());
}

#[test]
fn memory_roundtrip_retains_branch_graph_receipt_external_id_and_assets() {
    let temp = TestDir::new();
    let first_assets = TimelineAssetStore::new(temp.path().join("origin-assets"), 1024).unwrap();
    let mut origin = TimelineStore::new();
    let (session, root, fork, asset, original, receipt) = build_session(&mut origin, &first_assets);
    let archive = export_memory(&origin, session, &first_assets).unwrap();
    let mut target = TimelineStore::new();
    let target_assets = TimelineAssetStore::new(temp.path().join("dest-assets"), 1024).unwrap();
    assert_eq!(
        import_memory(&mut target, &archive, &target_assets).unwrap(),
        session
    );
    assert_eq!(origin.world(root).unwrap(), target.world(root).unwrap());
    assert_eq!(origin.world(fork).unwrap(), target.world(fork).unwrap());
    assert_eq!(target.branch(fork).unwrap().parent, Some(root));
    assert_eq!(
        target
            .event(fork, original.id)
            .unwrap()
            .draft
            .external_message_id
            .as_deref(),
        Some("python-msg:abc/α")
    );
    let selected =
        TimelineMessageQueries::capture(&target, fork, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        target_assets
            .read_visible(&selected, original.id, &asset)
            .unwrap(),
        b"hello image payload"
    );
    let again = target
        .append(
            root,
            0,
            "message:1",
            json!({"command":"write"}),
            vec![original],
        )
        .unwrap();
    assert_eq!(again, receipt);
    assert_eq!(
        export_memory(&target, session, &target_assets).unwrap(),
        archive
    );
    assert!(matches!(
        import_memory(&mut target, &archive, &target_assets),
        Err(SessionArchiveError::Conflict)
    ));
}

#[test]
fn retention_manifest_preserves_hidden_and_revised_facts_and_pinned_archives() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let (session, root, fork, original, first, _) = build_session(&mut timeline, &assets);
    let revised = assets
        .import("image/png", Cursor::new(b"revised image"))
        .unwrap();
    let unreferenced = assets
        .import("image/png", Cursor::new(b"orphaned"))
        .unwrap();

    let mut edit = EventDraft::new(EventKind::MessageRevised);
    edit.observers.insert(CharacterId::Crazy);
    edit.payload
        .insert("event_id".into(), json!(first.id.0.to_string()));
    edit.payload.insert(
        "message".into(),
        json!({"content": [
            {"type":"image", "asset":revised}
        ]}),
    );
    timeline
        .append(root, 2, "revision", json!({}), vec![edit])
        .unwrap();
    let mut hidden = EventDraft::new(EventKind::MessageHidden);
    hidden.observers.insert(CharacterId::Crazy);
    hidden
        .payload
        .insert("event_ids".into(), json!([first.id.0.to_string()]));
    timeline
        .append(root, 3, "hide", json!({}), vec![hidden])
        .unwrap();

    let live = retention_manifest_memory(&timeline, &[]).unwrap();
    assert_eq!(live.len(), 2);
    assert!(live.contains(&original));
    assert!(live.contains(&revised));
    assert!(!live.contains(&unreferenced));
    assert_eq!(timeline.branch(fork).unwrap().parent, Some(root));

    let exported = export_memory(&timeline, session, &assets).unwrap();
    let empty = TimelineStore::new();
    let pinned = retention_manifest_memory(&empty, &[&exported]).unwrap();
    assert_eq!(pinned, live);
    let corrupted = bytes_with_change(&exported, |value| {
        value["assets"][0]["data_base64"] = json!("YmFk");
    });
    assert!(retention_manifest_memory(&empty, &[&corrupted]).is_err());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_retention_manifest_enumerates_all_canonical_branches() {
    use neuromita::timeline::storage::archive::retention_manifest_sqlite;
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut repository = SqliteTimelineRepository::open(&temp.path().join("timeline.db")).unwrap();
    let (_, _, _, reference, _, _) = build_session(&mut repository, &assets);
    assert_eq!(
        retention_manifest_sqlite(&repository, &[]).unwrap(),
        vec![reference]
    );
}

#[test]
fn revision_with_shadow_content_is_not_a_valid_archived_fact_or_receipt() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut origin = TimelineStore::new();
    let (session, root, _, reference, original, _) = build_session(&mut origin, &assets);
    let mut revision = EventDraft::new(EventKind::MessageRevised);
    revision.observers.insert(CharacterId::Crazy);
    revision
        .payload
        .insert("event_id".into(), json!(original.id.0.to_string()));
    let revised = json!([{"type":"text","text":"revised"},{"type":"image","asset":reference}]);
    revision
        .payload
        .insert("message".into(), json!({"content":revised.clone()}));
    origin
        .append(root, 2, "revision", json!({}), vec![revision])
        .unwrap();
    let valid = export_memory(&origin, session, &assets).unwrap();
    let forged = bytes_with_change(&valid, |envelope| {
        let branch = envelope["branches"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|branch| branch["info"]["id"] == json!(root))
            .unwrap();
        let event = branch["events"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|fact| {
                fact["draft"]["kind"] == serde_json::to_value(EventKind::MessageRevised).unwrap()
            })
            .unwrap();
        event["draft"]["payload"]["content"] = revised.clone();
        let receipt = branch["receipts"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|receipt| receipt["operation_key"] == "revision")
            .unwrap();
        receipt["drafts"][0]["payload"]["content"] = revised.clone();
        receipt["result"][0]["draft"]["payload"]["content"] = revised;
    });
    let destination_assets =
        TimelineAssetStore::new(temp.path().join("destination"), 1024).unwrap();
    let mut destination = TimelineStore::new();
    assert!(import_memory(&mut destination, &forged, &destination_assets).is_err());
    assert!(destination.branch(root).is_err());
    assert_eq!(
        import_memory(&mut destination, &valid, &destination_assets).unwrap(),
        session
    );
}

#[test]
fn invalid_archive_is_rejected_before_session_publication() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("origin"), 1024).unwrap();
    let mut origin = TimelineStore::new();
    let (session, root, fork, _, _, _) = build_session(&mut origin, &assets);
    let archive = export_memory(&origin, session, &assets).unwrap();
    let destination_assets = TimelineAssetStore::new(temp.path().join("target"), 1024).unwrap();
    let cases: [Box<dyn FnOnce(&mut Value)>; 8] = [
        Box::new(|x| x["version"] = json!(99)),
        Box::new(|x| x["unrecognized"] = json!(true)),
        Box::new(|x| x["created_at_utc"] = json!("2026-01-01T00:00:00+04:00")),
        Box::new(|x| x["assets"][0]["data_base64"] = json!("eHZj")),
        Box::new(|x| x["assets"] = json!([])),
        Box::new(|x| x["branches"][0]["events"][0]["draft"]["id"] = json!(Uuid::new_v4())),
        Box::new(|x| {
            let first = x["branches"][0]["events"][0].clone();
            x["branches"][0]["events"]
                .as_array_mut()
                .unwrap()
                .push(first);
        }),
        Box::new(|x| x["branches"][1]["info"]["checkpoint"] = json!(Uuid::new_v4())),
    ];
    for (number, case) in cases.into_iter().enumerate() {
        let corrupted = bytes_with_change(&archive, case);
        let mut target = TimelineStore::new();
        assert!(
            import_memory(&mut target, &corrupted, &destination_assets).is_err(),
            "case {number}"
        );
        assert!(target.branch(root).is_err(), "case {number} published root");
        assert!(target.branch(fork).is_err(), "case {number} published fork");
    }
}

#[test]
fn broken_causal_link_and_forged_receipt_are_rejected() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("origin"), 1024).unwrap();
    let mut origin = TimelineStore::new();
    let (session, root, _, _, _, _) = build_session(&mut origin, &assets);
    let archive = export_memory(&origin, session, &assets).unwrap();
    let target_assets = TimelineAssetStore::new(temp.path().join("dest"), 1024).unwrap();
    let bad_link = bytes_with_change(&archive, |x| {
        let fork_branch = x["branches"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|b| b["info"]["parent"] == json!(root))
            .unwrap();
        fork_branch["events"][1]["draft"]["links"][0]["event_id"] = json!(Uuid::new_v4());
    });
    assert!(import_memory(&mut TimelineStore::new(), &bad_link, &target_assets).is_err());
    let bad_receipt = bytes_with_change(&archive, |x| {
        x["branches"][0]["receipts"][0]["command"] = json!([])
    });
    assert!(import_memory(&mut TimelineStore::new(), &bad_receipt, &target_assets).is_err());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_restore_reopen_and_reexport_match_byte_for_byte() {
    let temp = TestDir::new();
    let asset1 = TimelineAssetStore::new(temp.path().join("assets1"), 1024).unwrap();
    let session;
    let root;
    let branch;
    let archive;
    {
        let mut origin =
            SqliteTimelineRepository::open(&temp.path().join("origin.sqlite")).unwrap();
        (session, root, branch, _, _, _) = build_session(&mut origin, &asset1);
        archive = export_sqlite(&origin, session, &asset1).unwrap();
    }
    let asset2 = TimelineAssetStore::new(temp.path().join("assets2"), 1024).unwrap();
    {
        let mut restored =
            SqliteTimelineRepository::open(&temp.path().join("restored.sqlite")).unwrap();
        assert_eq!(
            import_sqlite(&mut restored, &archive, &asset2).unwrap(),
            session
        );
        assert_eq!(restored.branch(root).unwrap().head_sequence, 2);
        assert_eq!(restored.branch(branch).unwrap().head_sequence, 4);
    }
    let reopened = SqliteTimelineRepository::open(&temp.path().join("restored.sqlite")).unwrap();
    assert_eq!(export_sqlite(&reopened, session, &asset2).unwrap(), archive);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_invalid_archive_rolls_back_without_partial_session() {
    let temp = TestDir::new();
    let first = TimelineAssetStore::new(temp.path().join("a"), 1024).unwrap();
    let mut source = TimelineStore::new();
    let (session, root, _, _, _, _) = build_session(&mut source, &first);
    let bytes = export_memory(&source, session, &first).unwrap();
    let corrupt = bytes_with_change(&bytes, |x| {
        x["branches"][1]["events"][0]["sequence"] = json!(999)
    });
    let target_assets = TimelineAssetStore::new(temp.path().join("b"), 1024).unwrap();
    let path = temp.path().join("db.sqlite");
    {
        let mut target = SqliteTimelineRepository::open(&path).unwrap();
        assert!(import_sqlite(&mut target, &corrupt, &target_assets).is_err());
        assert!(target.branch(root).is_err());
    }
    let mut reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert!(reopened.branch(root).is_err());
    import_sqlite(&mut reopened, &bytes, &target_assets).unwrap();
    assert_eq!(reopened.branch(root).unwrap().session_id, session);
}

#[test]
fn unknown_nested_fields_and_foreign_event_ids_are_refused() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("source-assets"), 1024).unwrap();
    let mut source = TimelineStore::new();
    let (session, root, _, _, _, _) = build_session(&mut source, &assets);
    let archive = export_memory(&source, session, &assets).unwrap();
    let destination_assets =
        TimelineAssetStore::new(temp.path().join("destination-assets"), 1024).unwrap();
    for corrupt in [
        bytes_with_change(&archive, |x| {
            x["branches"][0]["info"]["unknown"] = json!(false)
        }),
        bytes_with_change(&archive, |x| {
            x["branches"][0]["events"][0]["draft"]["unexpected"] = json!(false)
        }),
        bytes_with_change(&archive, |x| {
            x["branches"][0]["receipts"][0]["unrecognized"] = json!(false)
        }),
    ] {
        assert!(import_memory(&mut TimelineStore::new(), &corrupt, &destination_assets).is_err());
    }
    let mut other = TimelineStore::new();
    let foreign_session = SessionId::new();
    let foreign_root = BranchId::new();
    other.create_session(foreign_session, foreign_root).unwrap();
    let existing_id = source
        .event(root, source.world(root).unwrap()[0].draft.id)
        .unwrap()
        .draft
        .id;
    let mut foreign = EventDraft::new(EventKind::PlayerMessage);
    foreign.id = existing_id;
    foreign.actor = Some("player".into());
    foreign.observers.insert(CharacterId::Kind);
    foreign
        .payload
        .insert("content".into(), json!("different session"));
    other
        .append(foreign_root, 0, "foreign", json!({}), vec![foreign])
        .unwrap();
    assert!(import_memory(&mut other, &archive, &destination_assets).is_err());
    assert!(other.branch(root).is_err());
    assert_eq!(other.branch(foreign_root).unwrap().head_sequence, 1);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_sql_failure_in_middle_of_import_rolls_back_every_branch() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("original"), 1024).unwrap();
    let mut source = TimelineStore::new();
    let (session, root, child, _, _, _) = build_session(&mut source, &assets);
    let archive = export_memory(&source, session, &assets).unwrap();
    let db_path = temp.path().join("injected-failure.sqlite");
    let mut destination = SqliteTimelineRepository::open(&db_path).unwrap();
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TRIGGER fail_import BEFORE INSERT ON timeline_events WHEN CAST(NEW.sequence AS INTEGER)=4 BEGIN SELECT RAISE(ABORT, 'test failure'); END;").unwrap();
    }
    let target_assets = TimelineAssetStore::new(temp.path().join("target"), 1024).unwrap();
    assert!(import_sqlite(&mut destination, &archive, &target_assets).is_err());
    assert!(destination.branch(root).is_err());
    assert!(destination.branch(child).is_err());
    drop(destination);
    let reopened = SqliteTimelineRepository::open(&db_path).unwrap();
    assert!(reopened.branch(root).is_err());
    assert!(reopened.branch(child).is_err());
}

#[test]
fn export_is_scoped_to_selected_session_not_adjacent_timeline() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("all-assets"), 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let (selected, root, _, reference, _, _) = build_session(&mut timeline, &assets);
    let (other, other_root, _, other_reference, _, _) = build_session(&mut timeline, &assets);
    let archive = export_memory(&timeline, selected, &assets).unwrap();
    let envelope: Value = serde_json::from_slice(&archive).unwrap();
    assert_eq!(envelope["session_id"], json!(selected));
    assert_ne!(selected, other);
    assert_eq!(envelope["assets"].as_array().unwrap().len(), 1);
    assert_eq!(
        envelope["assets"][0]["reference"]["digest"],
        json!(reference.digest)
    );
    assert_eq!(reference, other_reference);
    let mut target = TimelineStore::new();
    let target_assets = TimelineAssetStore::new(temp.path().join("target"), 1024).unwrap();
    import_memory(&mut target, &archive, &target_assets).unwrap();
    assert_eq!(target.branch(root).unwrap().session_id, selected);
    assert!(target.branch(other_root).is_err());
}

#[test]
fn export_aborts_when_referenced_binary_was_tampered() {
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let (session, _, _, reference, _, _) = build_session(&mut timeline, &assets);
    let file = temp
        .path()
        .join("assets/objects")
        .join(&reference.digest[..2])
        .join(&reference.digest);
    fs::write(file, b"not the image").unwrap();
    assert!(matches!(
        export_memory(&timeline, session, &assets),
        Err(SessionArchiveError::Asset(_))
    ));
}

fn with_external_provenance(original: &[u8]) -> Vec<u8> {
    use chrono::{DateTime, SecondsFormat, Utc};
    let mut document: Value = serde_json::from_slice(original).unwrap();
    let mut branches = serde_json::Map::new();
    let mut events = serde_json::Map::new();
    for branch in document["branches"].as_array().unwrap() {
        let id = branch["info"]["id"].as_str().unwrap();
        branches.insert(id.into(), json!(format!("исходная-ветка:{id}")));
        for fact in branch["events"].as_array().unwrap() {
            let id = fact["draft"]["id"].as_str().unwrap();
            let ms = fact["draft"]["occurred_at_unix_ms"].as_u64().unwrap() as i64;
            let micros = ms * 1000 + 917;
            events.insert(
                id.into(),
                json!({
                    "external_event_id": format!("событие:{id}"),
                    "occurred_at_iso8601": DateTime::<Utc>::from_timestamp_micros(micros)
                        .unwrap().to_rfc3339_opts(SecondsFormat::Micros, true),
                    "occurred_at_unix_us": micros,
                }),
            );
        }
    }
    document["provenance"] = json!({
        "source": "python_session_archive",
        "external_session_id": "сессия:παράδειγμα",
        "external_branches": branches,
        "external_events": events,
        "observer_bindings": {"Пользователь А": "Crazy"}
    });
    serde_json::to_vec(&document).unwrap()
}

#[test]
fn external_provenance_is_atomic_and_lossless_in_memory_and_sqlite() {
    let tmp = TestDir::new();
    let assets = TimelineAssetStore::new(tmp.path().join("assets"), 16 * 1024 * 1024).unwrap();
    let mut original = TimelineStore::new();
    let (session, _, _, _, _, _) = build_session(&mut original, &assets);
    let native = export_memory(&original, session, &assets).unwrap();
    let with_source = with_external_provenance(&native);
    let mut memory = TimelineStore::new();
    assert_eq!(
        import_memory(&mut memory, &with_source, &assets).unwrap(),
        session
    );
    let reexport = export_memory(&memory, session, &assets).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&reexport).unwrap(),
        serde_json::from_slice::<Value>(&with_source).unwrap()
    );
    assert_eq!(
        memory
            .session_provenance(session)
            .unwrap()
            .external_session_id,
        "сессия:παράδειγμα"
    );

    #[cfg(feature = "sqlite-system")]
    {
        let path = tmp.path().join("timeline.db");
        let mut sql = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(
            import_sqlite(&mut sql, &with_source, &assets).unwrap(),
            session
        );
        drop(sql);
        let sql = SqliteTimelineRepository::open(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&export_sqlite(&sql, session, &assets).unwrap())
                .unwrap(),
            serde_json::from_slice::<Value>(&with_source).unwrap()
        );
    }
}

#[test]
fn rejected_provenance_never_publishes_a_partial_session() {
    let tmp = TestDir::new();
    let assets = TimelineAssetStore::new(tmp.path().join("assets"), 16 * 1024 * 1024).unwrap();
    let mut original = TimelineStore::new();
    let (session, root, _, _, _, _) = build_session(&mut original, &assets);
    let with_source =
        with_external_provenance(&export_memory(&original, session, &assets).unwrap());
    for corrupt in [
        bytes_with_change(&with_source, |doc| {
            let provenance = doc["provenance"]["external_events"]
                .as_object_mut()
                .unwrap();
            let key = provenance.keys().next().unwrap().clone();
            provenance[&key]["occurred_at_unix_us"] = json!(0);
        }),
        bytes_with_change(&with_source, |doc| {
            doc["provenance"]["external_branches"] = json!({});
        }),
        bytes_with_change(&with_source, |doc| {
            doc["provenance"]["external_events"].as_object_mut().unwrap().insert(
                Uuid::new_v4().to_string(),
                json!({"external_event_id":"forged", "occurred_at_iso8601":"2026-01-01T00:00:00Z", "occurred_at_unix_us":1767225600000000_i64}),
            );
        }),
    ] {
        let mut memory = TimelineStore::new();
        assert!(import_memory(&mut memory, &corrupt, &assets).is_err());
        assert!(memory.branch(root).is_err());
        assert_eq!(
            import_memory(&mut memory, &with_source, &assets).unwrap(),
            session
        );

        #[cfg(feature = "sqlite-system")]
        {
            let path = tmp.path().join(format!("invalid-{}.db", Uuid::new_v4()));
            let mut sql = SqliteTimelineRepository::open(&path).unwrap();
            assert!(import_sqlite(&mut sql, &corrupt, &assets).is_err());
            drop(sql);
            let mut sql = SqliteTimelineRepository::open(&path).unwrap();
            assert_eq!(
                import_sqlite(&mut sql, &with_source, &assets).unwrap(),
                session
            );
        }
    }
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_provenance_write_failure_rolls_back_branch_facts_and_receipts() {
    let tmp = TestDir::new();
    let assets = TimelineAssetStore::new(tmp.path().join("assets"), 16 * 1024 * 1024).unwrap();
    let mut origin = TimelineStore::new();
    let (session, _, _, _, _, _) = build_session(&mut origin, &assets);
    let archive = with_external_provenance(&export_memory(&origin, session, &assets).unwrap());
    let db = tmp.path().join("provenance-failure.sqlite");
    let mut target = SqliteTimelineRepository::open(&db).unwrap();
    {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_provenance BEFORE INSERT ON timeline_session_provenance BEGIN SELECT RAISE(ABORT, 'forced failure'); END;").unwrap();
    }
    assert!(import_sqlite(&mut target, &archive, &assets).is_err());
    drop(target);
    let snapshot = SqliteTimelineRepository::open(&db).unwrap();
    assert!(export_sqlite(&snapshot, session, &assets).is_err());
    drop(snapshot);
    {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch("DROP TRIGGER fail_provenance")
            .unwrap();
    }
    let mut target = SqliteTimelineRepository::open(&db).unwrap();
    assert_eq!(
        import_sqlite(&mut target, &archive, &assets).unwrap(),
        session
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&export_sqlite(&target, session, &assets).unwrap())
            .unwrap(),
        serde_json::from_slice::<Value>(&archive).unwrap()
    );
}
