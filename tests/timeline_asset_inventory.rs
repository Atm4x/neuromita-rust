use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineRepository, TimelineStore},
        storage::{
            archive::{
                PinnedExternalArchive, SessionArchiveError, export_memory, inventory_memory,
                inventory_pinned_memory,
            },
            assets::{TimelineAssetError, TimelineAssetRef, TimelineAssetStore},
        },
    },
};
use serde_json::json;
use uuid::Uuid;

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("timeline-asset-inventory-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn attach(
    store: &TimelineAssetStore,
    timeline: &mut TimelineStore,
) -> (SessionId, TimelineAssetRef) {
    let session = SessionId::new();
    let root = BranchId::new();
    timeline.create_session(session, root).unwrap();
    let reference = store
        .import("image/png", Cursor::new(b"canonical content"))
        .unwrap();
    let mut draft = EventDraft::new(EventKind::PlayerMessage);
    draft.actor = Some("player".into());
    draft.observers.insert(CharacterId::Crazy);
    draft.payload.insert(
        "content".into(),
        json!([{"type":"image","asset":reference}]),
    );
    timeline
        .append(root, 0, "asset-inventory", json!({}), vec![draft])
        .unwrap();
    (session, reference)
}

#[test]
fn inventory_marks_only_unretained_objects_and_never_removes_them() {
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let (_, retained) = attach(&store, &mut timeline);
    let orphan = store
        .import("audio/wav", Cursor::new(b"abandoned upload"))
        .unwrap();
    let other = store
        .import("image/png", Cursor::new(b"other abandoned upload"))
        .unwrap();
    let inventory = inventory_memory(&timeline, &store, &[]).unwrap();
    assert_eq!(inventory.retained, vec![retained.clone()]);
    assert_eq!(inventory.orphan_candidates.len(), 2);
    let digests: Vec<_> = inventory
        .orphan_candidates
        .iter()
        .map(|candidate| candidate.digest.clone())
        .collect();
    let mut expected = vec![orphan.digest.clone(), other.digest.clone()];
    expected.sort();
    assert_eq!(digests, expected);
    assert!(
        inventory
            .orphan_candidates
            .iter()
            .all(|candidate| candidate.size > 0)
    );
    for reference in [&retained, &orphan, &other] {
        store.verify(reference).unwrap();
    }
    assert_eq!(inventory_memory(&timeline, &store, &[]).unwrap(), inventory);
}

#[test]
fn caller_supplied_verified_archive_retains_objects_even_without_local_session() {
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let (session, retained) = attach(&store, &mut timeline);
    let archive = export_memory(&timeline, session, &store).unwrap();
    let empty = TimelineStore::new();
    let unpinned = inventory_memory(&empty, &store, &[]).unwrap();
    assert_eq!(unpinned.orphan_candidates[0].digest, retained.digest);
    let pinned = inventory_memory(&empty, &store, &[&archive]).unwrap();
    assert_eq!(pinned.retained, vec![retained.clone()]);
    assert!(pinned.orphan_candidates.is_empty());
    let mut corrupted = archive;
    corrupted[0] ^= 0xff;
    assert!(matches!(
        inventory_memory(&empty, &store, &[&corrupted]),
        Err(SessionArchiveError::Invalid(_)) | Err(SessionArchiveError::Encoding(_))
    ));
    store.verify(&retained).unwrap();
}

#[test]
fn inventory_fails_closed_for_missing_or_corrupt_retained_content() {
    let temp = TestDir::new();
    let root = temp.path().join("assets");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    let mut timeline = TimelineStore::new();
    let (_, retained) = attach(&store, &mut timeline);
    let physical = root
        .join("objects")
        .join(&retained.digest[..2])
        .join(&retained.digest);
    fs::write(&physical, b"changed").unwrap();
    assert!(matches!(
        inventory_memory(&timeline, &store, &[]),
        Err(SessionArchiveError::Asset(TimelineAssetError::Corrupt))
    ));
    fs::remove_file(&physical).unwrap();
    assert!(matches!(
        inventory_memory(&timeline, &store, &[]),
        Err(SessionArchiveError::Asset(TimelineAssetError::Missing))
    ));
}

#[test]
fn unknown_filesystem_layout_and_conflicting_refs_do_not_produce_candidates() {
    let temp = TestDir::new();
    let root = temp.path().join("assets");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    let original = store.import("image/png", Cursor::new(b"present")).unwrap();
    let mut conflicted = original.clone();
    conflicted.media_type = "image/jpeg".into();
    assert!(matches!(
        store.inventory(&[original.clone(), conflicted]),
        Err(TimelineAssetError::InvalidReference)
    ));
    fs::write(root.join("objects").join("unknown-object"), b"invalid").unwrap();
    assert!(matches!(
        store.inventory(&[]),
        Err(TimelineAssetError::InvalidReference)
    ));
    fs::remove_file(root.join("objects").join("unknown-object")).unwrap();
    assert_eq!(
        store
            .inventory(&[original])
            .unwrap()
            .orphan_candidates
            .len(),
        0
    );
}

#[test]
fn staging_files_are_not_orphan_objects_and_empty_store_is_valid() {
    let temp = TestDir::new();
    let root = temp.path().join("assets");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    assert!(store.inventory(&[]).unwrap().orphan_candidates.is_empty());
    fs::create_dir(root.join("staging")).unwrap();
    let staged = root.join("staging").join("uncommitted.tmp");
    fs::write(&staged, b"not a published object").unwrap();
    let orphan = store
        .import("image/png", Cursor::new(b"actual object"))
        .unwrap();
    let candidates = store.inventory(&[]).unwrap().orphan_candidates;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].digest, orphan.digest);
    assert_eq!(fs::read(&staged).unwrap(), b"not a published object");
}

#[cfg(unix)]
#[test]
fn unexpected_symlink_shard_blocks_the_inventory() {
    use std::os::unix::fs::symlink;
    let temp = TestDir::new();
    let root = temp.path().join("assets");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    let retained = store
        .import("image/png", Cursor::new(b"safe object"))
        .unwrap();
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    symlink(&other, root.join("objects").join("ab")).unwrap();
    assert!(matches!(
        store.inventory(&[retained]),
        Err(TimelineAssetError::InvalidReference)
    ));
}

#[cfg(unix)]
#[test]
fn symlink_object_is_never_classified_as_an_orphan_candidate() {
    use std::os::unix::fs::symlink;
    let temp = TestDir::new();
    let root = temp.path().join("assets");
    let store = TimelineAssetStore::new(&root, 1024).unwrap();
    let orphan = store
        .import("image/png", Cursor::new(b"external payload"))
        .unwrap();
    let object = root
        .join("objects")
        .join(&orphan.digest[..2])
        .join(&orphan.digest);
    let outside = temp.path().join("external");
    fs::rename(&object, &outside).unwrap();
    symlink(&outside, &object).unwrap();
    assert!(matches!(
        store.inventory(&[]),
        Err(TimelineAssetError::Corrupt)
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"external payload");
}

#[cfg(feature = "sqlite-system")]
#[test]
fn sqlite_inventory_includes_reopened_canonical_roots() {
    use neuromita::timeline::storage::{SqliteTimelineRepository, archive::inventory_sqlite};
    let temp = TestDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let retained = assets
        .import("image/png", Cursor::new(b"sqlite object"))
        .unwrap();
    let orphan = assets
        .import("audio/wav", Cursor::new(b"unrelated"))
        .unwrap();
    let path = temp.path().join("timeline.db");
    let branch = BranchId::new();
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(SessionId::new(), branch).unwrap();
        let mut event = EventDraft::new(EventKind::PlayerMessage);
        event.actor = Some("player".into());
        event.observers.insert(CharacterId::Crazy);
        event
            .payload
            .insert("content".into(), json!([{"type":"image","asset":retained}]));
        repo.append(branch, 0, "import", json!({}), vec![event])
            .unwrap();
    }
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    let inventory = inventory_sqlite(&reopened, &assets, &[]).unwrap();
    assert_eq!(inventory.retained, vec![retained]);
    assert_eq!(inventory.orphan_candidates.len(), 1);
    assert_eq!(inventory.orphan_candidates[0].digest, orphan.digest);
}

#[test]
fn pinned_archive_on_disk_retains_branch_binary_and_rechecks_every_inventory() {
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut original = TimelineStore::new();
    let (session, reference) = attach(&store, &mut original);
    let archive = export_memory(&original, session, &store).unwrap();
    let saved = temp.path().join("pinned-session.json");
    fs::write(&saved, &archive).unwrap();
    let pin = PinnedExternalArchive::for_export(&saved, &archive).unwrap();
    let empty = TimelineStore::new();

    assert_eq!(
        inventory_memory(&empty, &store, &[])
            .unwrap()
            .orphan_candidates
            .len(),
        1
    );
    let inventory = inventory_pinned_memory(&empty, &store, &[pin.clone()]).unwrap();
    assert_eq!(inventory.retained, vec![reference.clone()]);
    assert!(inventory.orphan_candidates.is_empty());

    fs::write(&saved, b"valid path, replaced bytes").unwrap();
    assert!(matches!(
        inventory_pinned_memory(&empty, &store, &[pin.clone()]),
        Err(SessionArchiveError::PinnedArchiveChanged)
    ));
    store.verify(&reference).unwrap();
    fs::remove_file(&saved).unwrap();
    assert!(matches!(
        inventory_pinned_memory(&empty, &store, &[pin]),
        Err(SessionArchiveError::MissingPinnedArchive)
    ));
    store.verify(&reference).unwrap();
}

#[test]
fn pinned_archive_rejects_relative_paths_and_conflicting_pins() {
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    assert!(PinnedExternalArchive::new("relative.json", "0".repeat(64)).is_err());
    assert!(PinnedExternalArchive::new(temp.path().join("archive"), "ABC").is_err());
    let mut original = TimelineStore::new();
    let (session, reference) = attach(&store, &mut original);
    let archive = export_memory(&original, session, &store).unwrap();
    let saved = temp.path().join("archive.json");
    fs::write(&saved, &archive).unwrap();
    let valid = PinnedExternalArchive::for_export(&saved, &archive).unwrap();
    let other = PinnedExternalArchive::new(&saved, "0".repeat(64)).unwrap();
    assert!(matches!(
        inventory_pinned_memory(&TimelineStore::new(), &store, &[valid, other]),
        Err(SessionArchiveError::Invalid(
            "conflicting external archive pins"
        ))
    ));
    store.verify(&reference).unwrap();
}

#[test]
fn pinned_archive_valid_digest_still_requires_valid_session_document() {
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let saved = temp.path().join("invalid.json");
    fs::write(&saved, b"not a session archive").unwrap();
    let pin = PinnedExternalArchive::for_export(&saved, b"not a session archive").unwrap();
    assert!(inventory_pinned_memory(&TimelineStore::new(), &store, &[pin]).is_err());
}

#[cfg(unix)]
#[test]
fn pinned_archive_symlink_never_bypasses_disk_archive_verification() {
    use std::os::unix::fs::symlink;
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut original = TimelineStore::new();
    let (session, _) = attach(&store, &mut original);
    let archive = export_memory(&original, session, &store).unwrap();
    let actual = temp.path().join("actual.json");
    let link = temp.path().join("linked.json");
    fs::write(&actual, &archive).unwrap();
    symlink(&actual, &link).unwrap();
    let pin = PinnedExternalArchive::for_export(&link, &archive).unwrap();
    assert!(matches!(
        inventory_pinned_memory(&TimelineStore::new(), &store, &[pin]),
        Err(SessionArchiveError::Invalid("unsafe external archive file"))
    ));
}

#[cfg(feature = "sqlite-system")]
#[test]
fn reopened_sqlite_with_pinned_archive_preserves_all_roots() {
    use neuromita::timeline::storage::{
        SqliteTimelineRepository, archive::inventory_pinned_sqlite,
    };
    let temp = TestDir::new();
    let store = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let mut origin = TimelineStore::new();
    let (session, pinned_reference) = attach(&store, &mut origin);
    let archive = export_memory(&origin, session, &store).unwrap();
    let saved = temp.path().join("saved-session.json");
    fs::write(&saved, &archive).unwrap();
    let pin = PinnedExternalArchive::for_export(&saved, &archive).unwrap();
    let local_reference = store
        .import("image/png", Cursor::new(b"local branch asset"))
        .unwrap();
    let path = temp.path().join("timeline.db");
    let branch = BranchId::new();
    {
        let mut sqlite = SqliteTimelineRepository::open(&path).unwrap();
        sqlite.create_session(SessionId::new(), branch).unwrap();
        let mut draft = EventDraft::new(EventKind::PlayerMessage);
        draft.actor = Some("player".into());
        draft.observers.insert(CharacterId::Crazy);
        draft.payload.insert(
            "content".into(),
            json!([{"type":"image","asset":local_reference}]),
        );
        sqlite
            .append(branch, 0, "local", json!({}), vec![draft])
            .unwrap();
    }
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    let inventory = inventory_pinned_sqlite(&reopened, &store, &[pin]).unwrap();
    assert_eq!(inventory.retained.len(), 2);
    assert!(inventory.retained.contains(&pinned_reference));
    assert!(inventory.retained.contains(&local_reference));
    assert!(inventory.orphan_candidates.is_empty());
}
