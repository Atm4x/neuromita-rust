use std::{collections::BTreeMap, num::NonZeroUsize, sync::Arc, time::Duration};

#[cfg(feature = "sqlite-system")]
use neuromita::timeline::TimelineConfig;
use neuromita::{
    api::{CharacterApi, VariableTransferApi, VariableTransferApiError},
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::variable_transfer::{MAX_VARIABLE_ARCHIVE_BYTES, VariableTransferError},
};
use serde_json::{Value, json};
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

fn entries() -> BTreeMap<String, Value> {
    BTreeMap::from([
        ("favorite_food".into(), json!("green tea")),
        ("trained".into(), json!(true)),
        ("coordinates".into(), json!({"x": 2, "y": [1, 3]})),
    ])
}

#[tokio::test]
async fn canonical_variables_transfer_scoped_confirmed_and_repeat_is_noop() {
    let _serial = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let kind = CharacterApi::get(CharacterId::Kind).unwrap();
    crazy
        .import_variables(crazy.user_variables().await.unwrap(), entries())
        .await
        .unwrap();
    let bytes = VariableTransferApi::export(CharacterId::Crazy)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["scope"], "user_variables_only");
    assert_eq!(json["variables"], json!(entries()));
    for derived in ["attitude", "boredom", "stress", "ACCEPTED_REPLIES"] {
        assert!(json["variables"].get(derived).is_none());
        assert!(crazy.debug_variables().await.unwrap().contains_key(derived));
    }
    let preview = VariableTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    assert_eq!(preview.source_character(), CharacterId::Crazy);
    assert_eq!(preview.target_character(), CharacterId::Kind);
    assert_eq!(preview.incoming_count(), 3);
    assert_eq!(preview.added_count(), 3);
    assert!(matches!(
        VariableTransferApi::apply_confirmed(
            CharacterId::Kind,
            preview.clone(),
            false,
            &CancellationToken::new()
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::ConfirmationRequired
        )),
    ));
    assert!(matches!(
        VariableTransferApi::apply_confirmed(
            CharacterId::Crazy,
            preview.clone(),
            true,
            &CancellationToken::new()
        )
        .await,
        Err(VariableTransferApiError::ForeignPreview),
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        VariableTransferApi::apply_confirmed(CharacterId::Kind, preview.clone(), true, &cancel)
            .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::Cancelled
        )),
    ));
    assert!(kind.user_variables().await.unwrap().values().is_empty());
    let committed = VariableTransferApi::apply_confirmed(
        CharacterId::Kind,
        preview,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(committed.values(), &entries());
    assert_eq!(crazy.user_variables().await.unwrap().values(), &entries());
    let repeated = VariableTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    assert_eq!(repeated.added_count(), 0);
    assert_eq!(repeated.identical_count(), 3);
    let anchor = repeated.target_anchor();
    let noop = VariableTransferApi::apply_confirmed(
        CharacterId::Kind,
        repeated,
        true,
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(noop.anchor(), anchor);
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn corrupted_conflicting_and_stale_variable_archives_fail_without_partial_writes() {
    let _serial = APPLICATION.lock().await;
    let app = ApplicationLifecycle::start(config()).await.unwrap();
    let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
    let kind = CharacterApi::get(CharacterId::Kind).unwrap();
    crazy
        .import_variables(crazy.user_variables().await.unwrap(), entries())
        .await
        .unwrap();
    let bytes = VariableTransferApi::export(CharacterId::Crazy)
        .await
        .unwrap();
    let mut parsed: Value = serde_json::from_slice(&bytes).unwrap();
    parsed["variables"]["trained"] = json!(false);
    assert!(matches!(
        VariableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&parsed).unwrap()
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::IntegrityMismatch
        )),
    ));
    parsed["variables"]["CHARACTER_ID"] = json!("evil");
    assert!(matches!(
        VariableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&parsed).unwrap()
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::InvalidVariable(_)
        )),
    ));
    let mut parsed: Value = serde_json::from_slice(&bytes).unwrap();
    parsed["version"] = json!(42);
    assert!(matches!(
        VariableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&parsed).unwrap()
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::UnsupportedFormat
        )),
    ));
    let mut parsed: Value = serde_json::from_slice(&bytes).unwrap();
    parsed["extra"] = json!(true);
    assert!(matches!(
        VariableTransferApi::preview_import(
            CharacterId::Kind,
            &serde_json::to_vec(&parsed).unwrap()
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::Malformed(_)
        )),
    ));
    assert!(matches!(
        VariableTransferApi::preview_import(
            CharacterId::Kind,
            &vec![b' '; MAX_VARIABLE_ARCHIVE_BYTES + 1]
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::LimitExceeded
        )),
    ));
    let text = String::from_utf8(bytes.clone()).unwrap();
    let duplicate = text.replace(
        "\"trained\": true\n",
        "\"trained\": true, \"trained\": true\n",
    );
    assert_ne!(duplicate, text);
    assert!(matches!(
        VariableTransferApi::preview_import(CharacterId::Kind, duplicate.as_bytes()).await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::Malformed(_)
        )),
    ));
    let preview = VariableTransferApi::preview_import(CharacterId::Kind, &bytes)
        .await
        .unwrap();
    kind.import_variables(
        kind.user_variables().await.unwrap(),
        BTreeMap::from([("different".into(), json!(7))]),
    )
    .await
    .unwrap();
    assert!(matches!(
        VariableTransferApi::apply_confirmed(
            CharacterId::Kind,
            preview,
            true,
            &CancellationToken::new()
        )
        .await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::StalePreview
        )),
    ));
    kind.import_variables(
        kind.user_variables().await.unwrap(),
        BTreeMap::from([("trained".into(), json!(false))]),
    )
    .await
    .unwrap();
    assert!(matches!(
        VariableTransferApi::preview_import(CharacterId::Kind, &bytes).await,
        Err(VariableTransferApiError::Transfer(
            VariableTransferError::Conflict
        )),
    ));
    assert_eq!(kind.user_variables().await.unwrap().values().len(), 2);
    app.shutdown().await.unwrap();
}

#[cfg(feature = "sqlite-system")]
#[tokio::test]
async fn sqlite_reopen_preserves_variable_transfer_as_single_canonical_event() {
    let _serial = APPLICATION.lock().await;
    let dir = std::env::temp_dir().join(format!(
        "neuromita-variable-transfer-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let make_config = || {
        let mut config = config();
        config.timeline = TimelineConfig::Persistent {
            database: dir.join("timeline.sqlite3"),
        };
        config
    };
    let bytes;
    {
        let app = ApplicationLifecycle::start(make_config()).await.unwrap();
        let crazy = CharacterApi::get(CharacterId::Crazy).unwrap();
        crazy
            .import_variables(crazy.user_variables().await.unwrap(), entries())
            .await
            .unwrap();
        bytes = VariableTransferApi::export(CharacterId::Crazy)
            .await
            .unwrap();
        let plan = VariableTransferApi::preview_import(CharacterId::Kind, &bytes)
            .await
            .unwrap();
        VariableTransferApi::apply_confirmed(
            CharacterId::Kind,
            plan,
            true,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        app.shutdown().await.unwrap();
    }
    {
        let app = ApplicationLifecycle::start(make_config()).await.unwrap();
        assert_eq!(
            CharacterApi::get(CharacterId::Kind)
                .unwrap()
                .user_variables()
                .await
                .unwrap()
                .values(),
            &entries()
        );
        assert_eq!(
            VariableTransferApi::preview_import(CharacterId::Kind, &bytes)
                .await
                .unwrap()
                .added_count(),
            0
        );
        app.shutdown().await.unwrap();
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
