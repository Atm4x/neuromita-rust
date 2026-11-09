use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    document_revision::{
        DocumentRevisionApi, DocumentRevisionInspection, DocumentRevisionIssue,
        DocumentRevisionStatus,
    },
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    request::{InitiateRequest, RequestOutcome, RequestSource, RequestState},
    settings::{
        AppSettings, DocumentGenerationSettings, JsonSettingsRepository, SettingsApi,
        SettingsConfig, SettingsError, SettingsRepository, SettingsSource,
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};

struct ProviderCall {
    input: GenerationInput,
    reply: oneshot::Sender<String>,
}

struct ControlledProvider(mpsc::UnboundedSender<ProviderCall>);

impl GenerationProvider for ControlledProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Gemini
    }

    fn generate(&self, input: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
        Box::pin(async move {
            let (reply, receive) = oneshot::channel();
            self.0
                .send(ProviderCall { input, reply })
                .map_err(|_| GenerationError::Transport("provider closed".into()))?;
            let response = receive
                .await
                .map_err(|_| GenerationError::Transport("provider cancelled".into()))?;
            Ok(StructuredResponse::spoken(response))
        })
    }
}

fn config(
    sender: mpsc::UnboundedSender<ProviderCall>,
    repository: Arc<JsonSettingsRepository>,
    source: SettingsSource,
) -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "test",
            "unused",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let size = |n| NonZeroUsize::new(n).unwrap();
    let mut config = ApplicationConfig::minimal(
        preset,
        ResponseGenerator::new([
            Arc::new(ControlledProvider(sender)) as Arc<dyn GenerationProvider>
        ])
        .unwrap(),
        RuntimeLimits {
            request_queue: size(4),
            discovery_buffer: size(8),
            character_commands: size(16),
            presenter_updates: size(8),
            retained_finished: size(8),
        },
        "127.0.0.1:0".parse().unwrap(),
    );
    config.settings = SettingsConfig { source, repository };
    config
}

async fn generated(
    calls: &mut mpsc::UnboundedReceiver<ProviderCall>,
    character: CharacterId,
    expected: &str,
) {
    let handle = ChatApi::initiate(InitiateRequest::new(
        character,
        "nebula orchid",
        RequestSource::Presenter,
    ))
    .unwrap();
    let call = timeout(Duration::from_secs(8), calls.recv())
        .await
        .unwrap()
        .unwrap();
    let text = call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(text.contains(expected), "missing {expected} in {text}");
    assert!(text.contains("source_sha256") && text.contains("chunk_index"));
    call.reply.send("verified".into()).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(8), handle.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
}

async fn rejected_before_provider(
    calls: &mut mpsc::UnboundedReceiver<ProviderCall>,
    character: CharacterId,
) {
    let handle = ChatApi::initiate(InitiateRequest::new(
        character,
        "nebula orchid",
        RequestSource::Presenter,
    ))
    .unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(8), handle.wait())
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert!(
        calls.try_recv().is_err(),
        "provider received rejected evidence"
    );
}

#[tokio::test]
async fn persistent_document_revisions_survive_restart_and_fail_closed_until_explicit_recovery() {
    let folder = std::env::temp_dir().join(format!("nm-r6-restart-{}", uuid::Uuid::new_v4()));
    let crazy_root = folder.join("crazy");
    let kind_root = folder.join("kind");
    let revisions = folder.join("revisions");
    std::fs::create_dir_all(&crazy_root).unwrap();
    std::fs::create_dir_all(&kind_root).unwrap();
    std::fs::create_dir(&revisions).unwrap();
    std::fs::write(crazy_root.join("notes.md"), "Nebula orchid: sapphire blue.").unwrap();
    std::fs::write(kind_root.join("notes.txt"), "Nebula orchid: violet sky.").unwrap();
    let repo = Arc::new(JsonSettingsRepository::new(folder.join("settings.json")));
    let v1 = DocumentRevisionApi::prepare(&crazy_root, CharacterId::Crazy, &revisions).unwrap();
    let kind = DocumentRevisionApi::prepare(&kind_root, CharacterId::Kind, &revisions).unwrap();
    let (sender, mut calls) = mpsc::unbounded_channel();

    let app = ApplicationLifecycle::start(config(
        sender.clone(),
        Arc::clone(&repo),
        SettingsSource::initial(AppSettings::initial_profile()),
    ))
    .await
    .unwrap();
    assert!(matches!(
        DocumentRevisionApi::inspect(CharacterId::Crazy).unwrap(),
        DocumentRevisionInspection::Disabled { .. }
    ));
    let first_revision = SettingsApi::snapshot::<DocumentGenerationSettings>()
        .unwrap()
        .revision;
    let first = DocumentRevisionApi::activate(&v1, first_revision).unwrap();
    let both = DocumentRevisionApi::activate(&kind, first.revision).unwrap();
    SettingsApi::flush().await.unwrap();
    let saved = repo.load().unwrap();
    assert_eq!(saved.revision, both.revision);
    assert_eq!(
        saved.settings.document_generation.sources[&CharacterId::Crazy].index,
        v1.index
    );
    assert_eq!(
        saved.settings.document_generation.sources[&CharacterId::Kind].index,
        kind.index
    );
    app.shutdown().await.unwrap();

    let app = ApplicationLifecycle::start(config(
        sender.clone(),
        Arc::clone(&repo),
        SettingsSource::Stored,
    ))
    .await
    .unwrap();
    assert!(matches!(
        DocumentRevisionApi::status(CharacterId::Crazy).unwrap(),
        DocumentRevisionStatus::Ready { settings_revision, revision } if settings_revision == both.revision && revision == v1
    ));
    assert!(matches!(
        DocumentRevisionApi::inspect(CharacterId::Kind).unwrap(),
        DocumentRevisionInspection::Ready { revision, .. } if revision == kind
    ));
    generated(&mut calls, CharacterId::Crazy, "sapphire blue").await;
    generated(&mut calls, CharacterId::Kind, "violet sky").await;

    let displaced = folder.join("displaced-index.json");
    std::fs::rename(&v1.index, &displaced).unwrap();
    assert!(matches!(
        DocumentRevisionApi::inspect(CharacterId::Crazy).unwrap(),
        DocumentRevisionInspection::Unavailable {
            settings_revision,
            issue: DocumentRevisionIssue::MissingIndex
        } if settings_revision == both.revision
    ));
    assert!(DocumentRevisionApi::status(CharacterId::Crazy).is_err());
    assert!(DocumentRevisionApi::activate(&v1, both.revision).is_err());
    rejected_before_provider(&mut calls, CharacterId::Crazy).await;
    generated(&mut calls, CharacterId::Kind, "violet sky").await;
    assert_eq!(
        SettingsApi::snapshot::<DocumentGenerationSettings>()
            .unwrap()
            .revision,
        both.revision
    );
    assert_eq!(repo.load().unwrap().revision, both.revision);

    let v2 = DocumentRevisionApi::prepare(&crazy_root, CharacterId::Crazy, &revisions).unwrap();
    assert_ne!(v2.index, v1.index);
    assert!(matches!(
        DocumentRevisionApi::activate(&v2, first.revision),
        Err(
            neuromita::document_revision::DocumentRevisionError::Settings(
                SettingsError::Conflict { .. }
            )
        )
    ));
    let repaired = DocumentRevisionApi::activate(&v2, both.revision).unwrap();
    assert_eq!(repaired.value.sources[&CharacterId::Kind].index, kind.index);
    SettingsApi::flush().await.unwrap();
    generated(&mut calls, CharacterId::Crazy, "sapphire blue").await;
    app.shutdown().await.unwrap();

    let app =
        ApplicationLifecycle::start(config(sender, Arc::clone(&repo), SettingsSource::Stored))
            .await
            .unwrap();
    assert!(matches!(
        DocumentRevisionApi::status(CharacterId::Crazy).unwrap(),
        DocumentRevisionStatus::Ready { settings_revision, revision } if settings_revision == repaired.revision && revision == v2
    ));
    assert_eq!(repo.load().unwrap().revision, repaired.revision);
    std::fs::write(&v2.index, b"not-json").unwrap();
    assert!(matches!(
        DocumentRevisionApi::inspect(CharacterId::Crazy).unwrap(),
        DocumentRevisionInspection::Unavailable {
            settings_revision,
            issue: DocumentRevisionIssue::InvalidIndex,
        } if settings_revision == repaired.revision
    ));
    rejected_before_provider(&mut calls, CharacterId::Crazy).await;
    generated(&mut calls, CharacterId::Kind, "violet sky").await;
    assert_eq!(repo.load().unwrap().revision, repaired.revision);

    std::fs::write(
        kind_root.join("notes.txt"),
        "Nebula orchid: changed violet sky.",
    )
    .unwrap();
    assert!(matches!(
        DocumentRevisionApi::inspect(CharacterId::Kind).unwrap(),
        DocumentRevisionInspection::Unavailable {
            settings_revision,
            issue: DocumentRevisionIssue::StaleSource,
        } if settings_revision == repaired.revision
    ));
    rejected_before_provider(&mut calls, CharacterId::Kind).await;
    assert_eq!(repo.load().unwrap().revision, repaired.revision);
    std::fs::remove_file(kind_root.join("notes.txt")).unwrap();
    std::fs::remove_dir(&kind_root).unwrap();
    assert!(matches!(
        DocumentRevisionApi::inspect(CharacterId::Kind).unwrap(),
        DocumentRevisionInspection::Unavailable {
            settings_revision,
            issue: DocumentRevisionIssue::MissingRoot,
        } if settings_revision == repaired.revision
    ));
    assert_eq!(repo.load().unwrap().revision, repaired.revision);
    app.shutdown().await.unwrap();
    std::fs::remove_dir_all(folder).unwrap();
}
