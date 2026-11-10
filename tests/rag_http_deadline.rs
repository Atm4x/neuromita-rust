use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    api::ChatApi,
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationError, GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        ToolSettings,
        openai::{OpenAiPresetDefinition, OpenAiProtocol, OpenAiProvider, OpenAiTemplate},
    },
    request::{
        DiagnosticOutcome, DiagnosticScope, FailureStage, RequestOutcome, RequestSource,
        RequestState,
    },
    settings::SettingsApi,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::timeout,
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn limits() -> RuntimeLimits {
    let nonzero = |value| NonZeroUsize::new(value).unwrap();
    RuntimeLimits {
        request_queue: nonzero(4),
        discovery_buffer: nonzero(16),
        character_commands: nonzero(16),
        presenter_updates: nonzero(16),
        retained_finished: nonzero(8),
    }
}

async fn start(endpoint: String, timeout_ms: u64) -> ApplicationLifecycle {
    let mut options = OpenAiProtocol::Compatible.options();
    options.max_retries = 0;
    options.stream = false;
    let template = OpenAiTemplate::new(
        OpenAiPresetDefinition {
            protocol: OpenAiProtocol::Compatible,
            endpoint,
            model: "cycle66-loopback".into(),
            credential: None,
            temperature: 0.2,
            max_output_tokens: 128,
            timeout_ms,
            options,
            transforms: Vec::new(),
        },
        None,
    )
    .unwrap();
    let provider: Arc<dyn GenerationProvider> = Arc::new(OpenAiProvider::new().unwrap());
    ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::OpenAi(template)),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap()
}

async fn receive(stream: TcpStream) -> (BufReader<TcpStream>, Value) {
    let mut reader = BufReader::new(stream);
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut data = vec![0; length.unwrap()];
    reader.read_exact(&mut data).await.unwrap();
    (reader, serde_json::from_slice(&data).unwrap())
}

async fn respond(reader: &mut BufReader<TcpStream>, payload: Value) -> std::io::Result<()> {
    let body = payload.to_string();
    reader
        .get_mut()
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
}

fn answer(text: &str) -> Value {
    let structured = json!({"segments":[{"text":text}]}).to_string();
    json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":structured}}]})
}

fn calculator_call() -> Value {
    json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"cycle66_calc","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\"2+3\"}"}}]}}]})
}

#[tokio::test]
async fn real_http_stall_expires_and_next_request_succeeds_without_late_acceptance() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (received, entered) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut first, body) = receive(first_socket).await;
        assert_eq!(body["model"], "cycle66-loopback");
        received.send(()).unwrap();
        let first_task = tokio::spawn(async move {
            let _ = released.await;
            let _ = respond(&mut first, answer("late stale reply")).await;
        });
        let (second_socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut second, body) = receive(second_socket).await;
        assert_eq!(body["model"], "cycle66-loopback");
        respond(&mut second, answer("fresh HTTP reply"))
            .await
            .unwrap();
        first_task.await.unwrap();
    });
    let app = start(endpoint, 420).await;
    let expired = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "first".into())
        .unwrap();
    timeout(Duration::from_secs(8), entered)
        .await
        .unwrap()
        .unwrap();
    let state = timeout(Duration::from_secs(8), expired.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            state.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(
                GenerationError::DeadlineExceeded
            ))
        ),
        "{state:?}"
    );

    assert!(matches!(
        ChatApi::diagnostics_for(DiagnosticScope::owner(
            CharacterId::Crazy,
            RequestSource::Presenter
        ))
        .unwrap()
        .request(expired.id())
        .unwrap()
        .trace
        .view()
        .outcome,
        Some(DiagnosticOutcome::Failed {
            stage: FailureStage::ProviderTimeout,
            ..
        })
    ));

    let fresh = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "second".into())
        .unwrap();
    let state = timeout(Duration::from_secs(8), fresh.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(state.state, RequestState::Finished(RequestOutcome::Success(ref response)) if response.text.as_ref() == "fresh HTTP reply"),
        "{state:?}"
    );

    release.send(()).unwrap();
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_tool_continuation_stalled_http_uses_original_absolute_deadline() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (continued, entered) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut first, body) = receive(first_socket).await;
        assert_eq!(body["tools"][0]["function"]["name"], "calculator");
        respond(&mut first, calculator_call()).await.unwrap();
        let (second_socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut second, body) = receive(second_socket).await;
        let messages = body["messages"].as_array().unwrap();
        assert!(messages.iter().any(|m| m["role"] == "tool"
            && m["tool_call_id"] == "cycle66_calc"
            && m["content"].as_str().unwrap().contains('5')));
        continued.send(()).unwrap();
        released.await.unwrap();
        let _ = respond(&mut second, answer("late continuation")).await;
    });
    let app = start(endpoint, 550).await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
        tools.memory_search = false;
        tools.max_depth = 2;
    })
    .unwrap();
    let pending = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "calculate 2+3".into())
        .unwrap();
    timeout(Duration::from_secs(8), entered)
        .await
        .unwrap()
        .unwrap();
    let state = timeout(Duration::from_secs(8), pending.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            state.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(
                GenerationError::DeadlineExceeded
            ))
        ),
        "{state:?}"
    );
    assert!(matches!(
        ChatApi::diagnostics_for(DiagnosticScope::owner(
            CharacterId::Crazy,
            RequestSource::Presenter
        ))
        .unwrap()
        .request(pending.id())
        .unwrap()
        .trace
        .view()
        .outcome,
        Some(DiagnosticOutcome::Failed {
            stage: FailureStage::ProviderTimeout,
            ..
        })
    ));
    release.send(()).unwrap();
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_http_disconnect_is_transport_not_timeout() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (_reader, _) = receive(socket).await;
    });
    let app = start(endpoint, 2000).await;
    let failed = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "disconnect".into())
        .unwrap();
    let state = timeout(Duration::from_secs(8), failed.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            state.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(
                GenerationError::Transport(_)
            ))
        ),
        "{state:?}"
    );
    assert!(matches!(
        ChatApi::diagnostics_for(DiagnosticScope::owner(
            CharacterId::Crazy,
            RequestSource::Presenter
        ))
        .unwrap()
        .request(failed.id())
        .unwrap()
        .trace
        .view()
        .outcome,
        Some(DiagnosticOutcome::Failed {
            stage: FailureStage::ProviderTransport,
            ..
        })
    ));
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_cancel_during_real_http_wait_stays_cancelled_not_provider_timeout() {
    use neuromita::request::CancelResult;

    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (received, entered) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (first_socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut first, _) = receive(first_socket).await;
        received.send(()).unwrap();
        let blocked = tokio::spawn(async move {
            let _ = released.await;
            let _ = respond(&mut first, answer("late cancelled reply")).await;
        });
        let (second_socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut second, _) = receive(second_socket).await;
        respond(&mut second, answer("after cancellation"))
            .await
            .unwrap();
        blocked.await.unwrap();
    });
    let app = start(endpoint, 3000).await;
    let cancelled = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "cancel me".into())
        .unwrap();
    timeout(Duration::from_secs(8), entered)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        cancelled.cancel(),
        CancelResult::Requested | CancelResult::Cancelled
    ));
    let state = timeout(Duration::from_secs(8), cancelled.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(state.state, RequestState::Cancelled), "{state:?}");
    let next = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "next".into())
        .unwrap();
    let state = timeout(Duration::from_secs(8), next.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(state.state, RequestState::Finished(RequestOutcome::Success(ref response)) if response.text.as_ref() == "after cancellation"),
        "{state:?}"
    );
    release.send(()).unwrap();
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}
