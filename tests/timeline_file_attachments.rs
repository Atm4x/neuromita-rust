use std::{
    fs,
    path::{Path, PathBuf},
};

use neuromita::{
    character::CharacterId,
    timeline::{
        conversation::append_conversation_message,
        domain::{AppendGuard, BranchId, SessionId, TimelineMessagePart, TimelineStore},
        file_attachments::{LocalAttachmentError, compose_local_file_message, import_local_file},
        storage::assets::{TimelineAssetError, TimelineAssetStore},
        view::TimelineMessageQueries,
    },
};
use uuid::Uuid;

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("nm-file-chat-{}", Uuid::new_v4()));
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

#[test]
fn local_file_creates_one_private_canonical_event_without_source_path() {
    let temp = TempDir::new();
    let path = temp.path().join("private-document.PNG");
    fs::write(&path, b"sample binary image").unwrap();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let part = import_local_file(&assets, &path).unwrap();
    let reference = part.asset().unwrap().clone();
    assert!(matches!(part, TimelineMessagePart::Image { .. }));
    assert_eq!(reference.media_type, "image/png");
    let message = compose_local_file_message(CharacterId::Crazy, "look".into(), part).unwrap();
    let event = message.id;
    assert!(message.content.to_string().contains("look"));
    assert!(!message.content.to_string().contains("private-document"));
    assert!(
        !message
            .content
            .to_string()
            .contains(&temp.path().display().to_string())
    );
    let mut repo = TimelineStore::new();
    let branch = BranchId::new();
    repo.create_session(SessionId::new(), branch).unwrap();
    append_conversation_message(
        &mut repo,
        branch,
        0,
        AppendGuard {
            selection_generation: 1,
            context: None,
            accepted_reply_revision: None,
        },
        message,
    )
    .unwrap();
    let visible = TimelineMessageQueries::capture(&repo, branch, CharacterId::Crazy, None).unwrap();
    let hidden = TimelineMessageQueries::capture(&repo, branch, CharacterId::Kind, None).unwrap();
    assert_eq!(visible.count(), 1);
    assert_eq!(hidden.count(), 0);
    assert_eq!(
        assets.read_visible(&visible, event, &reference).unwrap(),
        b"sample binary image"
    );
    assert!(matches!(
        assets.read_visible(&hidden, event, &reference),
        Err(TimelineAssetError::NotVisible)
    ));
}

#[test]
fn failed_file_open_or_oversize_prevents_message_creation() {
    let temp = TempDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 4).unwrap();
    assert!(matches!(
        import_local_file(&assets, &temp.path().join("absent.png")),
        Err(LocalAttachmentError::Io(_))
    ));
    let path = temp.path().join("large.mp3");
    fs::write(&path, b"12345").unwrap();
    assert!(matches!(
        import_local_file(&assets, &path),
        Err(LocalAttachmentError::Asset(TimelineAssetError::TooLarge))
    ));
    let part = assets.import("image/png", b"a".as_slice()).unwrap();
    assert!(matches!(
        compose_local_file_message(
            CharacterId::Crazy,
            "a".repeat(65537),
            TimelineMessagePart::Image { asset: part }
        ),
        Err(LocalAttachmentError::TextTooLarge)
    ));
}

#[test]
fn generic_file_and_attachment_only_message_remain_structured() {
    let temp = TempDir::new();
    let assets = TimelineAssetStore::new(temp.path().join("assets"), 1024).unwrap();
    let path = temp.path().join("notes.pdf");
    fs::write(&path, b"a pdf fixture").unwrap();
    let part = import_local_file(&assets, &path).unwrap();
    assert!(matches!(part, TimelineMessagePart::File { .. }));
    let msg = compose_local_file_message(CharacterId::Kind, String::new(), part).unwrap();
    assert_eq!(msg.content.as_array().unwrap().len(), 1);
    assert_eq!(msg.content[0]["type"], "file");
    assert_eq!(msg.audience.seen_by.len(), 1);
    assert!(msg.audience.seen_by.contains(&CharacterId::Kind));
}
