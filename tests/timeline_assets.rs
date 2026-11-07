use std::{
    fs,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore},
        storage::assets::{TimelineAssetError, TimelineAssetRef, TimelineAssetStore},
        view::TimelineMessageQueries,
    },
};
use serde_json::json;
use uuid::Uuid;

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nm-timeline-assets-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn attachment(reference: &TimelineAssetRef, observers: &[CharacterId]) -> EventDraft {
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft.observers.extend(observers);
    draft.payload.insert(
        "content".into(),
        json!([{"type":"text", "text":"look"}, {"type":"image", "asset":reference}]),
    );
    draft
}

fn append(repo: &mut dyn TimelineRepository, branch: BranchId, drafts: Vec<EventDraft>) {
    let head = repo.branch(branch).unwrap().head_sequence;
    repo.append(
        branch,
        head,
        &format!("assets:{head}"),
        json!({"test":"assets"}),
        drafts,
    )
    .unwrap();
}

fn visibility_contract(repo: &mut dyn TimelineRepository) {
    let temp = TempDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let reference = store
        .import("image/png", Cursor::new(b"example PNG bytes"))
        .unwrap();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    let message = attachment(&reference, &[CharacterId::Crazy]);
    let event = message.id;
    append(repo, branch, vec![message]);
    let permitted =
        TimelineMessageQueries::capture(repo, branch, CharacterId::Crazy, None).unwrap();
    let excluded = TimelineMessageQueries::capture(repo, branch, CharacterId::Kind, None).unwrap();
    assert_eq!(
        store.read_visible(&permitted, event, &reference).unwrap(),
        b"example PNG bytes"
    );
    assert!(matches!(
        store.read_visible(&excluded, event, &reference),
        Err(TimelineAssetError::NotVisible)
    ));

    let mut checkpoint = EventDraft::new(EventKind::Checkpoint);
    checkpoint.payload.insert("snapshot".into(), json!({}));
    let checkpoint_id = checkpoint.id;
    append(repo, branch, vec![checkpoint]);
    let child = BranchId::new();
    repo.fork(branch, checkpoint_id, child).unwrap();
    let child_view =
        TimelineMessageQueries::capture(repo, child, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        store.read_visible(&child_view, event, &reference).unwrap(),
        b"example PNG bytes"
    );

    let mut hidden = EventDraft::new(EventKind::MessageHidden);
    hidden.observers.insert(CharacterId::Crazy);
    hidden
        .payload
        .insert("event_ids".into(), json!([event.0.to_string()]));
    append(repo, branch, vec![hidden]);
    let later = TimelineMessageQueries::capture(repo, branch, CharacterId::Crazy, None).unwrap();
    assert!(matches!(
        store.read_visible(&later, event, &reference),
        Err(TimelineAssetError::NotVisible)
    ));
    let historic =
        TimelineMessageQueries::capture(repo, branch, CharacterId::Crazy, Some(1)).unwrap();
    assert_eq!(
        store.read_visible(&historic, event, &reference).unwrap(),
        b"example PNG bytes"
    );
    assert_eq!(
        store.read_visible(&child_view, event, &reference).unwrap(),
        b"example PNG bytes"
    );
}

#[test]
fn visibility_hide_fork_and_historical_read_in_memory() {
    visibility_contract(&mut TimelineStore::new());
}

#[cfg(feature = "sqlite-system")]
#[test]
fn visibility_hide_fork_and_historical_read_sqlite_reopen() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let temp = TempDir::new();
    let path = temp.path().join("timeline.sqlite");
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        visibility_contract(&mut repo);
    }
    let _reopened = SqliteTimelineRepository::open(&path).unwrap();
}

#[test]
fn import_deduplicates_and_survives_restart_with_integrity() {
    let temp = TempDir::new();
    let root = temp.path().join("asset-root");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    let first = store.import("image/png", Cursor::new(b"alpha")).unwrap();
    let second = store.import("image/png", Cursor::new(b"alpha")).unwrap();
    assert_eq!(first, second);
    assert_ne!(
        first,
        store.import("image/jpeg", Cursor::new(b"alpha")).unwrap()
    );
    let blobs = fs::read_dir(root.join("objects"))
        .unwrap()
        .map(Result::unwrap)
        .count();
    assert!(blobs >= 1);
    drop(store);
    let restarted = TimelineAssetStore::new(&root, 1024).unwrap();
    restarted.verify(&first).unwrap();
    let physical = root
        .join("objects")
        .join(&first.digest[..2])
        .join(&first.digest);
    fs::write(&physical, b"ALPHA").unwrap();
    assert!(matches!(
        restarted.verify(&first),
        Err(TimelineAssetError::Corrupt)
    ));
    assert!(matches!(
        restarted.import("image/png", Cursor::new(b"alpha")),
        Err(TimelineAssetError::Corrupt)
    ));
    fs::remove_file(physical).unwrap();
    assert!(matches!(
        restarted.verify(&first),
        Err(TimelineAssetError::Missing)
    ));
    restarted
        .import("image/png", Cursor::new(b"alpha"))
        .unwrap();
    restarted.verify(&first).unwrap();
}

struct Interrupted;
impl Read for Interrupted {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        buffer[0] = b'x';
        Err(io::Error::other("injected source failure"))
    }
}

#[test]
fn failed_or_oversize_upload_never_publishes_blob_or_leaks_staging() {
    let temp = TempDir::new();
    let store = TimelineAssetStore::new(temp.path(), 4).unwrap();
    assert!(matches!(
        store.import("image/png", Cursor::new(b"12345")),
        Err(TimelineAssetError::TooLarge)
    ));
    assert!(matches!(
        store.import("image/png", Interrupted),
        Err(TimelineAssetError::Io(_))
    ));
    assert_eq!(
        fs::read_dir(temp.path().join("staging")).unwrap().count(),
        0
    );
    assert!(!temp.path().join("objects").exists());
}

#[test]
fn malformed_references_cannot_escape_root() {
    let temp = TempDir::new();
    let store = TimelineAssetStore::new(temp.path(), 100).unwrap();
    let mut reference = store.import("image/png", Cursor::new(b"valid")).unwrap();
    reference.digest = "../../private".into();
    assert!(matches!(
        store.verify(&reference),
        Err(TimelineAssetError::InvalidReference)
    ));
    reference.digest = "a".repeat(64);
    reference.media_type = "../../private".into();
    assert!(matches!(
        store.verify(&reference),
        Err(TimelineAssetError::InvalidReference)
    ));
}

#[test]
fn concurrent_imports_produce_one_valid_blob() {
    let temp = TempDir::new();
    let store = Arc::new(TimelineAssetStore::new(temp.path(), 2048).unwrap());
    let handles: Vec<_> = (0..12)
        .map(|_| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                store
                    .import("audio/wav", Cursor::new(vec![42; 512]))
                    .unwrap()
            })
        })
        .collect();
    let refs: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert!(refs.iter().all(|reference| reference == &refs[0]));
    store.verify(&refs[0]).unwrap();
    assert_eq!(
        fs::read_dir(temp.path().join("staging")).unwrap().count(),
        0
    );
}

#[test]
fn append_checked_rejects_dangling_assets_without_timeline_receipt() {
    let temp = TempDir::new();
    let store = TimelineAssetStore::new(temp.path(), 1024).unwrap();
    let reference = store.import("image/png", Cursor::new(b"good")).unwrap();
    let mut corrupted = reference.clone();
    corrupted.digest = "f".repeat(64);
    let branch = BranchId::new();
    let mut timeline = TimelineStore::new();
    timeline.create_session(SessionId::new(), branch).unwrap();
    let invalid = attachment(&corrupted, &[CharacterId::Crazy]);
    assert!(
        store
            .append_checked(
                &mut timeline,
                branch,
                0,
                "rejected",
                json!({}),
                vec![invalid]
            )
            .is_err()
    );
    assert_eq!(timeline.branch(branch).unwrap().head_sequence, 0);
    let original = attachment(&reference, &[CharacterId::Crazy]);
    store
        .append_checked(
            &mut timeline,
            branch,
            0,
            "accepted",
            json!({}),
            vec![original],
        )
        .unwrap();
    assert_eq!(timeline.branch(branch).unwrap().head_sequence, 1);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_and_asset_store_reopen_retains_visible_message() {
    use neuromita::timeline::storage::SqliteTimelineRepository;
    let temp = TempDir::new();
    let asset_path = temp.path().join("assets");
    let db_path = temp.path().join("events.db");
    let branch = BranchId::new();
    let session = SessionId::new();
    let (reference, event) = {
        let store = TimelineAssetStore::new(&asset_path, 1024).unwrap();
        let reference = store
            .import("image/png", Cursor::new(b"persisted"))
            .unwrap();
        let mut timeline = SqliteTimelineRepository::open(&db_path).unwrap();
        timeline.create_session(session, branch).unwrap();
        let message = attachment(&reference, &[CharacterId::Crazy]);
        let event = message.id;
        store
            .append_checked(
                &mut timeline,
                branch,
                0,
                "persist",
                json!({}),
                vec![message],
            )
            .unwrap();
        (reference, event)
    };
    let store = TimelineAssetStore::new(&asset_path, 1024).unwrap();
    let timeline = SqliteTimelineRepository::open(&db_path).unwrap();
    let view =
        TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        store.read_visible(&view, event, &reference).unwrap(),
        b"persisted"
    );
}

#[test]
fn append_checked_validates_assets_in_revisions_and_session_notes() {
    let temp = TempDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let original = assets
        .import("image/png", Cursor::new(b"original"))
        .unwrap();
    let unavailable = assets
        .import("image/png", Cursor::new(b"unavailable"))
        .unwrap();
    let missing_path = temp
        .path()
        .join("assets/objects")
        .join(&unavailable.digest[..2])
        .join(&unavailable.digest);
    fs::remove_file(missing_path).unwrap();
    let branch = BranchId::new();
    let mut timeline = TimelineStore::new();
    timeline.create_session(SessionId::new(), branch).unwrap();
    let message = attachment(&original, &[CharacterId::Crazy]);
    let id = message.id;
    assets
        .append_checked(
            &mut timeline,
            branch,
            0,
            "original",
            json!({}),
            vec![message],
        )
        .unwrap();

    let mut revision = EventDraft::new(EventKind::MessageRevised);
    revision.observers.insert(CharacterId::Crazy);
    revision
        .payload
        .insert("event_id".into(), json!(id.0.to_string()));
    revision.payload.insert(
        "message".into(),
        json!({"content":[{"type":"image","asset":unavailable}]}),
    );
    assert!(
        assets
            .append_checked(
                &mut timeline,
                branch,
                1,
                "bad-revision",
                json!({}),
                vec![revision]
            )
            .is_err()
    );
    assert_eq!(timeline.branch(branch).unwrap().head_sequence, 1);
    let view =
        TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        assets.read_visible(&view, id, &original).unwrap(),
        b"original"
    );

    let mut note = EventDraft::new(EventKind::SessionNote);
    note.observers.insert(CharacterId::Crazy);
    note.payload.insert(
        "content".into(),
        json!([{"type":"image","asset":unavailable}]),
    );
    assert!(
        assets
            .append_checked(&mut timeline, branch, 1, "bad-note", json!({}), vec![note])
            .is_err()
    );
    assert_eq!(timeline.branch(branch).unwrap().head_sequence, 1);
}

#[cfg(unix)]
#[test]
fn asset_object_symlink_is_not_a_valid_blob_even_with_correct_digest() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let reference = store
        .import("image/png", Cursor::new(b"private-file"))
        .unwrap();
    let object = temp
        .path()
        .join("assets/objects")
        .join(&reference.digest[..2])
        .join(&reference.digest);
    let mut timeline = TimelineStore::new();
    let branch = BranchId::new();
    timeline.create_session(SessionId::new(), branch).unwrap();
    let message = attachment(&reference, &[CharacterId::Crazy]);
    let id = message.id;
    store
        .append_checked(
            &mut timeline,
            branch,
            0,
            "initial",
            json!({}),
            vec![message],
        )
        .unwrap();
    let view =
        TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None).unwrap();
    let outside = temp.path().join("outside-file");
    fs::write(&outside, b"private-file").unwrap();
    fs::remove_file(&object).unwrap();
    symlink(&outside, &object).unwrap();
    assert!(matches!(
        store.verify(&reference),
        Err(TimelineAssetError::Corrupt)
    ));
    assert!(matches!(
        store.read_visible(&view, id, &reference),
        Err(TimelineAssetError::Corrupt)
    ));
    assert!(
        store
            .append_checked(
                &mut timeline,
                branch,
                1,
                "symlinked-ref",
                json!({}),
                vec![attachment(&reference, &[CharacterId::Crazy])],
            )
            .is_err()
    );
    assert_eq!(timeline.branch(branch).unwrap().head_sequence, 1);
    assert!(matches!(
        store.import("image/png", Cursor::new(b"private-file")),
        Err(TimelineAssetError::Corrupt)
    ));
    fs::remove_file(&object).unwrap();
    assert_eq!(
        store
            .import("image/png", Cursor::new(b"private-file"))
            .unwrap(),
        reference
    );
}

#[cfg(unix)]
#[test]
fn staging_and_object_directory_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new();
    let root = temp.path().join("assets");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    let reference = store.import("image/png", Cursor::new(b"blob")).unwrap();
    let staging = root.join("staging");
    let other = temp.path().join("other");
    fs::create_dir_all(&other).unwrap();
    fs::remove_dir(&staging).unwrap();
    symlink(&other, &staging).unwrap();
    assert!(matches!(
        store.import("image/png", Cursor::new(b"next")),
        Err(TimelineAssetError::InvalidReference)
    ));
    fs::remove_file(&staging).unwrap();
    let shard = root.join("objects").join(&reference.digest[..2]);
    let object = shard.join(&reference.digest);
    fs::remove_file(&object).unwrap();
    fs::remove_dir(&shard).unwrap();
    symlink(&other, &shard).unwrap();
    assert!(matches!(
        store.verify(&reference),
        Err(TimelineAssetError::InvalidReference)
    ));
    assert!(matches!(
        store.import("image/png", Cursor::new(b"blob")),
        Err(TimelineAssetError::InvalidReference)
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_asset_root_is_rejected_at_creation() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new();
    let real = temp.path().join("actual");
    fs::create_dir_all(&real).unwrap();
    let link = temp.path().join("root-link");
    symlink(&real, &link).unwrap();
    assert!(matches!(
        TimelineAssetStore::new(&link, 1024),
        Err(TimelineAssetError::InvalidReference)
    ));
}

#[test]
fn an_absent_shard_reports_missing_not_io_failure() {
    let temp = TempDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let reference = store.import("image/png", Cursor::new(b"data")).unwrap();
    let shard = temp
        .path()
        .join("assets/objects")
        .join(&reference.digest[..2]);
    fs::remove_file(shard.join(&reference.digest)).unwrap();
    fs::remove_dir(shard).unwrap();
    assert!(matches!(
        store.verify(&reference),
        Err(TimelineAssetError::Missing)
    ));
}
