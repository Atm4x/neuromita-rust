use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::{CharacterApi, MemoryTransferApi, MemoryTransferApiError},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::memory_transfer::{MAX_ARCHIVE_BYTES, MemoryTransferError},
    memory::{
        MemoryCommand, MemoryId, MemoryIsland, MemoryKind, MemoryPriority, MemoryRecord,
        MemoryState,
    },
    timeline::TimelineConfig,
};
use tokio_util::sync::CancellationToken;

static APPLICATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct NoGeneration;
impl GenerationProvider for NoGeneration {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }
    fn generate(&self, _: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async { Err(GenerationError::Transport("unused".into())) })
    }
}

fn config() -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "fixture",
            "unused",
            GenerationSettings::new(0.5, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let cap = |n| NonZeroUsize::new(n).unwrap();
    ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([Arc::new(NoGeneration) as Arc<dyn GenerationProvider>]).unwrap(),
        RuntimeLimits {
            request_queue: cap(4),
            discovery_buffer: cap(8),
            character_commands: cap(16),
            presenter_updates: cap(8),
            retained_finished: cap(8),
        },
        "127.0.0.1:0".parse().unwrap(),
    )
}

fn records() -> Vec<MemoryRecord> {
    let mut protected = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Critical,
        "Protect an exact identity",
    );
    protected.protected = true;
    protected.entities = vec!["player".to_string()];
    protected.expires_at_unix_ms = Some(1_900_000_000_000);
    let mut forgotten = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Low,
        "Archived forgotten detail",
    );
    forgotten.state = MemoryState::Forgotten;
    let mut deleted = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::Normal,
        "Soft-deleted detail",
    );
    deleted.state = MemoryState::Deleted;
    let mut rag = MemoryRecord::fact(MemoryId::new(), MemoryPriority::High, "Only for retrieval");
    rag.kind = MemoryKind::RagOnly;
    let mut island = MemoryRecord::fact(MemoryId::new(), MemoryPriority::High, "An opinion island");
    island.kind = MemoryKind::Island(MemoryIsland::Opinion);
    vec![protected, forgotten, deleted, rag, island]
}

#[tokio::test]
async fn scoped_roundtrip_is_exact_no_overwrite_and_repeat_is_noop() {
    let _single = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let kind = CharacterApi::get(CharacterId::Kind).unwrap();
    let source = records();
    crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            source
                .iter()
                .cloned()
                .map(|record| MemoryCommand::ImportRecord { record })
                .collect(),
        )
        .await
        .unwrap();
    let bytes = MemoryTransferApi::export(CharacterId::Crazy).await.unwrap();
    let preview = MemoryTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    assert_eq!(preview.source_character(), CharacterId::Crazy);
    assert_eq!(preview.target_character(), CharacterId::Kind);
    assert_eq!(preview.new_records(), source.len());
    assert_eq!(preview.identical_records(), 0);
    assert!(matches!(
        MemoryTransferApi::apply_confirmed(
            CharacterId::Kind,
            preview.clone(),
            false,
            &CancellationToken::new()
        )
        .await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::ConfirmationRequired
        )),
    ));
    assert!(matches!(
        MemoryTransferApi::apply_confirmed(
            CharacterId::Crazy,
            preview.clone(),
            true,
            &CancellationToken::new()
        )
        .await,
        Err(MemoryTransferApiError::ForeignPreview),
    ));
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        MemoryTransferApi::apply_confirmed(CharacterId::Kind, preview.clone(), true, &cancelled)
            .await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::Cancelled
        )),
    ));
    assert!(kind.memories().await.unwrap().records().is_empty());
    let accepted = MemoryTransferApi::apply_confirmed(
        CharacterId::Kind,
        preview,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(accepted.applied.len(), source.len());
    let mut source_sorted = source.clone();
    source_sorted.sort_by_key(|r| r.id);
    assert_eq!(accepted.snapshot.records(), source_sorted);
    assert_eq!(crazy.memories().await.unwrap().records(), source_sorted);
    let repeated = MemoryTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    assert_eq!(repeated.new_records(), 0);
    assert_eq!(repeated.identical_records(), source.len());
    let result = MemoryTransferApi::apply_confirmed(
        CharacterId::Kind,
        repeated,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(result.applied.is_empty());
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_duplicates_conflicts_and_stale_preview_never_write_partially() {
    let _single = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let source = records();
    crazy
        .edit_memories(
            crazy.memories().await.unwrap(),
            source
                .iter()
                .cloned()
                .map(|record| MemoryCommand::ImportRecord { record })
                .collect(),
        )
        .await
        .unwrap();
    let bytes = MemoryTransferApi::export(CharacterId::Crazy).await.unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let entries = json["records"].as_array_mut().unwrap();
    entries.push(entries[0].clone());
    let duplicated = serde_json::to_vec(&json).unwrap();
    assert!(matches!(
        MemoryTransferApi::preview_import(CharacterId::Kind, &duplicated).await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::DuplicateId
        )),
    ));
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["version"] = serde_json::json!(2);
    assert!(matches!(
        MemoryTransferApi::preview_import(CharacterId::Kind, &serde_json::to_vec(&json).unwrap())
            .await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::UnsupportedFormat
        )),
    ));
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["untrusted"] = serde_json::json!(true);
    assert!(matches!(
        MemoryTransferApi::preview_import(CharacterId::Kind, &serde_json::to_vec(&json).unwrap())
            .await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::Malformed(_)
        )),
    ));
    assert!(matches!(
        MemoryTransferApi::preview_import(CharacterId::Kind, &vec![b' '; MAX_ARCHIVE_BYTES + 1])
            .await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::LimitExceeded
        )),
    ));
    assert!(
        MemoryTransferApi::preview_import(CharacterId::Kind, b"truncated")
            .await
            .is_err()
    );
    assert!(
        CharacterApi::get(CharacterId::Kind)
            .unwrap()
            .memories()
            .await
            .unwrap()
            .records()
            .is_empty()
    );

    let kind = CharacterApi::get(CharacterId::Kind).unwrap();
    let plan = MemoryTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    kind.edit_memories(
        kind.memories().await.unwrap(),
        vec![MemoryCommand::Add {
            priority: MemoryPriority::Normal,
            content: "Intervening user edit".into(),
        }],
    )
    .await
    .unwrap();
    assert!(matches!(
        MemoryTransferApi::apply_confirmed(
            CharacterId::Kind,
            plan,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::StalePreview
        )),
    ));
    assert_eq!(kind.memories().await.unwrap().records().len(), 1);

    let other_same_id = MemoryRecord::fact(
        source[0].id,
        MemoryPriority::Normal,
        "Conflicting archive ID",
    );
    kind.edit_memories(
        kind.memories().await.unwrap(),
        vec![MemoryCommand::ImportRecord {
            record: other_same_id,
        }],
    )
    .await
    .unwrap();
    assert!(matches!(
        MemoryTransferApi::preview_import(CharacterId::Kind, &bytes).await,
        Err(MemoryTransferApiError::Transfer(
            MemoryTransferError::InvalidRecord(_)
        )),
    ));
    assert_eq!(kind.memories().await.unwrap().records().len(), 2);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn sqlite_reopen_preserves_exact_import_and_idempotent_preview() {
    let _single = APPLICATION.lock().await;
    let root = std::env::temp_dir().join(format!(
        "neuromita-memory-transfer-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let make_config = || {
        let mut cfg = config();
        cfg.timeline = TimelineConfig::Persistent {
            database: root.join("timeline.sqlite3"),
        };
        cfg
    };
    let original = records();
    let bytes;
    {
        let app = ApplicationLifecycle::start(make_config()).await.unwrap();
        let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
        crazy
            .edit_memories(
                crazy.memories().await.unwrap(),
                original
                    .iter()
                    .cloned()
                    .map(|record| MemoryCommand::ImportRecord { record })
                    .collect(),
            )
            .await
            .unwrap();
        bytes = MemoryTransferApi::export(CharacterId::Crazy).await.unwrap();
        let plan = MemoryTransferApi::preview_import(CharacterId::Kind, &bytes)
            .await
            .unwrap();
        assert_eq!(
            MemoryTransferApi::apply_confirmed(
                CharacterId::Kind,
                plan,
                true,
                &CancellationToken::new()
            )
            .await
            .unwrap()
            .applied
            .len(),
            original.len()
        );
        app.shutdown().await.unwrap();
    }
    {
        let app = ApplicationLifecycle::start(make_config()).await.unwrap();
        let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
        let kind = CharacterApi::get(CharacterId::Kind).unwrap();
        assert_eq!(
            crazy.memories().await.unwrap().records(),
            kind.memories().await.unwrap().records()
        );
        assert_eq!(
            MemoryTransferApi::preview_import(CharacterId::Kind, &bytes)
                .await
                .unwrap()
                .new_records(),
            0
        );
        app.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}
