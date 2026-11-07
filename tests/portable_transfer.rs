use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc, time::Duration};

#[cfg(feature = "sqlite-system")]
use neuromita::timeline::TimelineConfig;
use neuromita::{
    api::{CharacterApi, PortableTransferApi, PortableTransferApiError},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::portable_transfer::{MAX_PORTABLE_ARCHIVE_BYTES, PortableTransferError},
    memory::{MemoryCommand, MemoryId, MemoryPriority, MemoryRecord},
};
use serde_json::json;
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
async fn populate_source() -> (MemoryId, BTreeMap<String, serde_json::Value>) {
    let source = CharacterApi::get(CharacterId::Crazy).unwrap();
    let mut fact = MemoryRecord::fact(
        MemoryId::new(),
        MemoryPriority::High,
        "Exact portable memory",
    );
    fact.protected = true;
    let id = fact.id;
    source
        .edit_memories(
            source.memories().await.unwrap(),
            vec![MemoryCommand::ImportRecord { record: fact }],
        )
        .await
        .unwrap();
    let variables = BTreeMap::from([
        ("user_locale".to_owned(), json!("ru")),
        ("trained".to_owned(), json!(true)),
    ]);
    source
        .import_variables(source.user_variables().await.unwrap(), variables.clone())
        .await
        .unwrap();
    (id, variables)
}

#[tokio::test]
async fn composite_restores_both_domains_as_one_guarded_batch_and_replay_is_idempotent() {
    let _serial = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let (id, variables) = populate_source().await;
    let target = CharacterApi::get(CharacterId::Kind).unwrap();
    let initial = target.portable_snapshot().await.unwrap();
    let bytes = PortableTransferApi::export(CharacterId::Crazy)
        .await
        .unwrap();
    let preview = PortableTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    assert_eq!(preview.source_character(), CharacterId::Crazy);
    assert_eq!(preview.target_character(), CharacterId::Kind);
    assert_eq!(preview.new_memories(), 1);
    assert_eq!(preview.new_variables(), 2);
    assert!(matches!(
        PortableTransferApi::apply_confirmed(
            CharacterId::Kind,
            preview.clone(),
            false,
            &CancellationToken::new()
        )
        .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::ConfirmationRequired
        ))
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        PortableTransferApi::apply_confirmed(CharacterId::Kind, preview.clone(), true, &cancel)
            .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::Cancelled
        ))
    ));
    assert!(matches!(
        PortableTransferApi::apply_confirmed(
            CharacterId::Crazy,
            preview.clone(),
            true,
            &CancellationToken::new()
        )
        .await,
        Err(PortableTransferApiError::ForeignPreview)
    ));
    assert!(target.memories().await.unwrap().records().is_empty());
    assert!(target.user_variables().await.unwrap().values().is_empty());
    let committed = PortableTransferApi::apply_confirmed(
        CharacterId::Kind,
        preview,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        (committed.applied_memories, committed.applied_variables),
        (1, 2)
    );
    assert_eq!(
        committed.snapshot.memory.anchor().through,
        initial.memory.anchor().through + 2
    );
    assert_eq!(committed.snapshot.memory.records()[0].id, id);
    assert!(committed.snapshot.memory.records()[0].protected);
    assert_eq!(committed.snapshot.variables.values(), &variables);
    let repeated = PortableTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    assert_eq!((repeated.new_memories(), repeated.new_variables()), (0, 0));
    let noop = PortableTransferApi::apply_confirmed(
        CharacterId::Kind,
        repeated,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!((noop.applied_memories, noop.applied_variables), (0, 0));
    assert_eq!(
        noop.snapshot.memory.anchor(),
        committed.snapshot.memory.anchor()
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn composite_rejects_malformed_conflicting_and_stale_input_without_partial_mutations() {
    let _serial = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    populate_source().await;
    let bytes = PortableTransferApi::export(CharacterId::Crazy)
        .await
        .unwrap();
    let target = CharacterApi::get(CharacterId::Kind).unwrap();
    assert!(matches!(
        PortableTransferApi::preview_import(
            CharacterId::Kind,
            &vec![b' '; MAX_PORTABLE_ARCHIVE_BYTES + 1]
        )
        .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::LimitExceeded
        ))
    ));
    let mut archive: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    archive["memory_archive"] = json!("changed payload");
    assert!(matches!(
        PortableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&archive).unwrap()
        )
        .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::IntegrityMismatch
        ))
    ));
    // A freshly recomputed outer checksum must not bypass the strict inner validators.
    let mut archive: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let variable_payload = archive["variables_archive"].as_str().unwrap();
    let original = "\"trained\": true";
    assert!(variable_payload.contains(original));
    archive["variables_archive"] =
        json!(variable_payload.replace(original, "\"trained\": true, \"trained\": true"));
    let checksum_payload = serde_json::to_vec(&(
        archive["format"].as_str().unwrap(),
        archive["version"].as_u64().unwrap() as u32,
        archive["scope"].as_str().unwrap(),
        CharacterId::Crazy,
        archive["memory_archive"].as_str().unwrap(),
        archive["variables_archive"].as_str().unwrap(),
    ))
    .unwrap();
    let hash: String = ring::digest::digest(&ring::digest::SHA256, &checksum_payload)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    archive["checksum_sha256"] = json!(hash);
    assert!(matches!(
        PortableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&archive).unwrap()
        )
        .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::Variables(_)
        ))
    ));
    assert!(target.memories().await.unwrap().records().is_empty());
    let mut archive: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    archive["version"] = json!(42);
    assert!(matches!(
        PortableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&archive).unwrap()
        )
        .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::UnsupportedFormat
        ))
    ));
    let stale = PortableTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    target
        .import_variables(
            target.user_variables().await.unwrap(),
            BTreeMap::from([("intervening".into(), json!(2))]),
        )
        .await
        .unwrap();
    assert!(matches!(
        PortableTransferApi::apply_confirmed(
            CharacterId::Kind,
            stale,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::StalePreview
        ))
    ));
    assert!(target.memories().await.unwrap().records().is_empty());
    assert_eq!(target.user_variables().await.unwrap().values().len(), 1);
    target
        .import_variables(
            target.user_variables().await.unwrap(),
            BTreeMap::from([("trained".into(), json!(false))]),
        )
        .await
        .unwrap();
    assert!(matches!(
        PortableTransferApi::preview_import(CharacterId::Kind, &bytes).await,
        Err(PortableTransferApiError::Transfer(
            PortableTransferError::Variables(_)
        ))
    ));
    assert!(target.memories().await.unwrap().records().is_empty());
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn composite_survives_sqlite_reopen_without_replaying_duplicate_events() {
    let _serial = APPLICATION.lock().await;
    let root = std::env::temp_dir().join(format!("neuromita-portable-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let make_config = || {
        let mut cfg = config();
        cfg.timeline = TimelineConfig::Persistent {
            database: root.join("timeline.sqlite3"),
        };
        cfg
    };
    let bytes;
    {
        let app = ApplicationLifecycle::start(make_config()).await.unwrap();
        populate_source().await;
        bytes = PortableTransferApi::export(CharacterId::Crazy)
            .await
            .unwrap();
        let plan = PortableTransferApi::preview_import(CharacterId::Kind, &bytes)
            .await
            .unwrap();
        let result = PortableTransferApi::apply_confirmed(
            CharacterId::Kind,
            plan,
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!((result.applied_memories, result.applied_variables), (1, 2));
        app.shutdown().await.unwrap();
    }
    {
        let app = ApplicationLifecycle::start(make_config()).await.unwrap();
        let source = CharacterApi::get(CharacterId::Crazy).unwrap();
        let target = CharacterApi::get(CharacterId::Kind).unwrap();
        assert_eq!(
            source.memories().await.unwrap().records(),
            target.memories().await.unwrap().records()
        );
        assert_eq!(
            source.user_variables().await.unwrap().values(),
            target.user_variables().await.unwrap().values()
        );
        let replay = PortableTransferApi::preview_import(CharacterId::Kind, &bytes)
            .await
            .unwrap();
        assert_eq!((replay.new_memories(), replay.new_variables()), (0, 0));
        app.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(root).unwrap();
}
