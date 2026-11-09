use std::{num::NonZeroUsize, path::PathBuf, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
        ProviderKind, ProviderTemplate, ResponseGenerator, StructuredResponse,
        gemini::{GeminiTemplate, GenerationSettings},
    },
    knowledge::document_index,
    request::{InitiateRequest, RequestOutcome, RequestSource, RequestState},
    settings::{DocumentGenerationSettings, DocumentGenerationSource, SettingsApi},
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

fn config(sender: mpsc::UnboundedSender<ProviderCall>) -> ApplicationConfig {
    let preset = GenerationPreset::new(ProviderTemplate::Gemini(
        GeminiTemplate::google(
            "test",
            "unused",
            GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
        )
        .unwrap(),
    ));
    let size = |n| NonZeroUsize::new(n).unwrap();
    ApplicationConfig::minimal(
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
    )
}

fn request(character: CharacterId) -> neuromita::request::RequestHandle {
    ChatApi::initiate(InitiateRequest::new(
        character,
        "nebula orchid",
        RequestSource::Presenter,
    ))
    .unwrap()
}

async fn next_call(receiver: &mut mpsc::UnboundedReceiver<ProviderCall>) -> ProviderCall {
    timeout(Duration::from_secs(8), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn completed(handle: neuromita::request::RequestHandle) -> RequestState {
    timeout(Duration::from_secs(8), handle.wait())
        .await
        .unwrap()
        .unwrap()
        .state
        .clone()
}

fn enable(root: PathBuf, index: PathBuf) {
    let current = SettingsApi::snapshot::<DocumentGenerationSettings>().unwrap();
    let mut value = current.value;
    value
        .sources
        .insert(CharacterId::Crazy, DocumentGenerationSource { root, index });
    SettingsApi::replace(current.revision, value).unwrap();
}

fn disable() {
    let current = SettingsApi::snapshot::<DocumentGenerationSettings>().unwrap();
    let mut value = current.value;
    value.sources.clear();
    SettingsApi::replace(current.revision, value).unwrap();
}

#[tokio::test]
async fn documents_are_explicit_character_scoped_and_fail_closed_on_stale_source() {
    let folder = std::env::temp_dir().join(format!("nm-doc-gen-{}", uuid::Uuid::new_v4()));
    let root = folder.join("documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("note.md"),
        "Nebula orchid grows beside a blue stream.",
    )
    .unwrap();
    let index = folder.join("documents-index.json");
    document_index::build_new(&root, CharacterId::Crazy, &index).unwrap();
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();

    assert!(
        SettingsApi::snapshot::<DocumentGenerationSettings>()
            .unwrap()
            .value
            .sources
            .is_empty()
    );
    let off = request(CharacterId::Crazy);
    let no_documents = next_call(&mut calls).await;
    assert!(
        !no_documents
            .input
            .prepared_knowledge
            .as_deref()
            .unwrap_or("")
            .contains("[RETRIEVED DOCUMENT EVIDENCE]")
    );
    no_documents.reply.send("off".into()).unwrap();
    assert!(matches!(
        completed(off).await,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    enable(root.clone(), index.clone());
    let on = request(CharacterId::Crazy);
    let doc_call = next_call(&mut calls).await;
    let evidence = doc_call.input.prepared_knowledge.as_deref().unwrap_or("");
    assert!(
        evidence.contains("[RETRIEVED DOCUMENT EVIDENCE]"),
        "{evidence}"
    );
    assert!(
        evidence.contains("blue stream")
            && evidence.contains("source_sha256")
            && evidence.contains("note.md")
            && evidence.contains("chunk_index"),
        "{evidence}"
    );
    doc_call.reply.send("on".into()).unwrap();
    assert!(matches!(
        completed(on).await,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    let foreign = request(CharacterId::Kind);
    let other_call = next_call(&mut calls).await;
    assert!(
        !other_call
            .input
            .prepared_knowledge
            .as_deref()
            .unwrap_or("")
            .contains("[RETRIEVED DOCUMENT EVIDENCE]")
    );
    other_call.reply.send("kind".into()).unwrap();
    assert!(matches!(
        completed(foreign).await,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    std::fs::write(root.join("note.md"), "Nebula orchid was relocated.").unwrap();
    let stale = request(CharacterId::Crazy);
    assert!(matches!(
        completed(stale).await,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert!(
        calls.try_recv().is_err(),
        "provider invoked after stale document source"
    );
    std::fs::write(
        root.join("note.md"),
        "Nebula orchid grows beside a blue stream.",
    )
    .unwrap();
    std::fs::write(&index, b"{not valid JSON").unwrap();
    let corrupt = request(CharacterId::Crazy);
    assert!(matches!(
        completed(corrupt).await,
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
    ));
    assert!(
        calls.try_recv().is_err(),
        "provider invoked after invalid document index"
    );
    disable();
    app.shutdown().await.unwrap();
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn document_selection_uses_admitted_settings_not_mutable_queue_state() {
    let folder = std::env::temp_dir().join(format!("nm-doc-admit-{}", uuid::Uuid::new_v4()));
    let root = folder.join("documents");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("note.txt"), "Nebula orchid is blue.").unwrap();
    let index = folder.join("documents-index.json");
    document_index::build_new(&root, CharacterId::Crazy, &index).unwrap();
    let (tx, mut calls) = mpsc::unbounded_channel();
    let app = ApplicationLifecycle::start(config(tx)).await.unwrap();

    let blocked = request(CharacterId::Crazy);
    let first = next_call(&mut calls).await;
    enable(root.clone(), index.clone());
    let admitted = request(CharacterId::Crazy);
    disable();
    first.reply.send("ready".into()).unwrap();
    assert!(matches!(
        completed(blocked).await,
        RequestState::Finished(RequestOutcome::Success(_))
    ));
    let selected = next_call(&mut calls).await;
    assert!(
        selected
            .input
            .prepared_knowledge
            .as_deref()
            .unwrap_or("")
            .contains("[RETRIEVED DOCUMENT EVIDENCE]")
    );
    selected.reply.send("selected".into()).unwrap();
    assert!(matches!(
        completed(admitted).await,
        RequestState::Finished(RequestOutcome::Success(_))
    ));

    app.shutdown().await.unwrap();
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn document_settings_are_disabled_by_default_backward_compatible_and_validated() {
    let settings = neuromita::settings::AppSettings::initial_profile();
    assert!(settings.document_generation.sources.is_empty());
    let mut old = serde_json::to_value(&settings).unwrap();
    old.as_object_mut().unwrap().remove("document_generation");
    let parsed: neuromita::settings::AppSettings = serde_json::from_value(old).unwrap();
    assert!(parsed.document_generation.sources.is_empty());
    let mut value = DocumentGenerationSettings::default();
    value.sources.insert(
        CharacterId::Crazy,
        DocumentGenerationSource {
            root: PathBuf::from("relative"),
            index: PathBuf::from("/tmp/index.json"),
        },
    );
    assert!(value.validate().is_err());
    value.sources.get_mut(&CharacterId::Crazy).unwrap().root = PathBuf::from("/tmp/root");
    assert!(value.validate().is_ok());
    value.max_results = 0;
    assert!(value.validate().is_err());
}
