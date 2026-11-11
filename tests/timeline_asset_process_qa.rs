use std::{
    env, fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use neuromita::{
    character::CharacterId,
    timeline::{
        domain::{BranchId, EventDraft, EventKind, SessionId, TimelineStore},
        storage::assets::TimelineAssetStore,
        view::TimelineMessageQueries,
    },
};
use serde_json::json;
use uuid::Uuid;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let path = env::temp_dir().join(format!("nm-q1-asset-multiprocess-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn upload(store: &TimelineAssetStore) -> neuromita::timeline::storage::assets::TimelineAssetRef {
    store
        .import("image/png", Cursor::new(vec![0xa7; 2048]))
        .unwrap()
}

#[test]
fn q1_asset_import_process_worker() {
    let Some(root) = env::var_os("NEUROMITA_Q1_ASSET_WORKER_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let marker = env::var("NEUROMITA_Q1_ASSET_WORKER_ID").unwrap();
    fs::write(root.join(format!("ready-{marker}")), b"ready").unwrap();
    for _ in 0..3000 {
        if root.join("go").exists() {
            let store = TimelineAssetStore::new(&root, 4096).unwrap();
            let reference = upload(&store);
            store.verify(&reference).unwrap();
            println!("Q1_DIGEST:{}", reference.digest);
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("parent never released the asset import gate");
}

fn wait_for_ready(root: &Path, workers: usize) -> bool {
    for _ in 0..1500 {
        if (0..workers).all(|id| root.join(format!("ready-{id}")).exists()) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn independent_writers_and_upload_before_receipt_keep_one_recoverable_object() {
    let temp = TempRoot::new();
    let store = TimelineAssetStore::new(&temp.0, 4096).unwrap();
    let binary = env::current_exe().unwrap();
    let mut workers: Vec<_> = (0..2)
        .map(|id| {
            Command::new(&binary)
                .arg("--exact")
                .arg("q1_asset_import_process_worker")
                .arg("--nocapture")
                .env("NEUROMITA_Q1_ASSET_WORKER_ROOT", &temp.0)
                .env("NEUROMITA_Q1_ASSET_WORKER_ID", id.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();

    if !wait_for_ready(&temp.0, workers.len()) {
        for worker in &mut workers {
            let _ = worker.kill();
            let _ = worker.wait();
        }
        panic!("the independent asset writers did not become ready");
    }
    fs::write(temp.0.join("go"), b"go").unwrap();
    let outputs: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.wait_with_output().unwrap())
        .collect();
    let reference = upload(&store);
    for output in outputs {
        assert!(
            output.status.success(),
            "worker stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("Q1_DIGEST:{}", reference.digest))
        );
    }

    store.verify(&reference).unwrap();
    assert_eq!(fs::read_dir(temp.0.join("staging")).unwrap().count(), 0);
    let objects = temp.0.join("objects").join(&reference.digest[..2]);
    assert_eq!(fs::read_dir(objects).unwrap().count(), 1);

    let orphan = store.inventory(&[]).unwrap();
    assert!(orphan.retained.is_empty());
    assert_eq!(orphan.orphan_candidates.len(), 1);
    assert_eq!(orphan.orphan_candidates[0].digest, reference.digest);

    let branch = BranchId::new();
    let mut timeline = TimelineStore::new();
    timeline.create_session(SessionId::new(), branch).unwrap();
    assert_eq!(timeline.branch(branch).unwrap().head_sequence, 0);
    let mut message = EventDraft::new(EventKind::PlayerMessage);
    message.actor = Some("player".into());
    message.observers.insert(CharacterId::Crazy);
    message.payload.insert(
        "content".into(),
        json!([{"type":"image", "asset": reference}]),
    );
    let event = message.id;
    store
        .append_checked(
            &mut timeline,
            branch,
            0,
            "first-receipt",
            json!({}),
            vec![message],
        )
        .unwrap();
    let view =
        TimelineMessageQueries::capture(&timeline, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        store.read_visible(&view, event, &reference).unwrap(),
        vec![0xa7; 2048]
    );
}

#[test]
fn q1_asset_exit_before_receipt_worker() {
    let Some(root) = env::var_os("NEUROMITA_Q1_ASSET_EXIT_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let store = TimelineAssetStore::new(root.join("assets"), 4096).unwrap();
    let reference = upload(&store);
    let marker = root.join("published.json");
    fs::write(&marker, serde_json::to_vec(&reference).unwrap()).unwrap();
    fs::File::open(&marker).unwrap().sync_all().unwrap();
    std::process::exit(47);
}

#[cfg(feature = "sqlite-system")]
#[test]
fn crash_after_asset_publish_before_sqlite_receipt_is_recoverable() {
    use neuromita::timeline::{
        domain::TimelineRepository,
        storage::{SqliteTimelineRepository, assets::TimelineAssetRef},
    };

    let temp = TempRoot::new();
    let binary = env::current_exe().unwrap();
    let output = Command::new(&binary)
        .arg("--exact")
        .arg("q1_asset_exit_before_receipt_worker")
        .arg("--nocapture")
        .env("NEUROMITA_Q1_ASSET_EXIT_ROOT", &temp.0)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(47));

    let reference: TimelineAssetRef =
        serde_json::from_slice(&fs::read(temp.0.join("published.json")).unwrap()).unwrap();
    let assets = TimelineAssetStore::new(temp.0.join("assets"), 4096).unwrap();
    assets.verify(&reference).unwrap();
    let orphan = assets.inventory(&[]).unwrap();
    assert_eq!(orphan.orphan_candidates.len(), 1);
    assert_eq!(orphan.orphan_candidates[0].digest, reference.digest);

    let path = temp.0.join("events.sqlite");
    let branch = BranchId::new();
    let session = SessionId::new();
    let event = EventDraft::new(EventKind::PlayerMessage).id;
    {
        let mut repo = SqliteTimelineRepository::open(&path).unwrap();
        repo.create_session(session, branch).unwrap();
        let mut message = EventDraft::new(EventKind::PlayerMessage);
        message.id = event;
        message.actor = Some("player".into());
        message.observers.insert(CharacterId::Crazy);
        message.payload.insert(
            "content".into(),
            json!([{"type":"image","asset":reference}]),
        );
        assert!(
            assets
                .append_checked(
                    &mut repo,
                    branch,
                    1,
                    "recovered-receipt",
                    json!({}),
                    vec![message.clone()]
                )
                .is_err()
        );
        assert_eq!(repo.branch(branch).unwrap().head_sequence, 0);
        assets
            .append_checked(
                &mut repo,
                branch,
                0,
                "recovered-receipt",
                json!({}),
                vec![message],
            )
            .unwrap();
    }
    let reopened = SqliteTimelineRepository::open(&path).unwrap();
    assert_eq!(reopened.branch(branch).unwrap().head_sequence, 1);
    let view =
        TimelineMessageQueries::capture(&reopened, branch, CharacterId::Crazy, None).unwrap();
    assert_eq!(
        assets.read_visible(&view, event, &reference).unwrap(),
        vec![0xa7; 2048]
    );
    let retained = assets.inventory(&[reference]).unwrap();
    assert_eq!(retained.retained.len(), 1);
    assert!(retained.orphan_candidates.is_empty());
}
