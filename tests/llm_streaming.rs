use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator,
        gemini::{GeminiProvider, GeminiRequestOptions, GeminiTemplate, GenerationSettings},
        openai::{OpenAiPresetDefinition, OpenAiProtocol, OpenAiProvider, OpenAiTemplate},
    },
    request::{RequestOutcome, RequestState},
};
use serde_json::{Value, json};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    time::timeout,
};

static APPLICATION: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const ANSWER: &str = r#"{"segments":[{"text":"stream response"}]}"#;

fn event(value: Value) -> String {
    format!("data: {value}\r\n\r\n")
}

fn response_frames(protocol: Option<OpenAiProtocol>) -> Vec<String> {
    let left = &ANSWER[..16];
    let right = &ANSWER[16..];
    match protocol {
        Some(OpenAiProtocol::Responses) => vec![
            event(json!({"type":"response.output_text.delta","delta":left})),
            event(json!({"type":"response.output_text.delta","delta":right})),
            event(
                json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":7,"total_tokens":12}}}),
            ),
            "data: [DONE]\r\n\r\n".into(),
        ],
        Some(_) => vec![
            event(json!({"choices":[{"index":0,"delta":{"content":left},"finish_reason":null}]})),
            event(json!({"choices":[{"index":0,"delta":{"content":right},"finish_reason":null}]})),
            event(json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})),
            event(
                json!({"choices":[],"usage":{"prompt_tokens":5,"completion_tokens":7,"total_tokens":12}}),
            ),
            "data: [DONE]\r\n\r\n".into(),
        ],
        None => vec![
            event(json!({"candidates":[{"content":{"parts":[{"text":left}]}}]})),
            event(json!({"candidates":[{"content":{"parts":[{"text":right}]}}]})),
            event(
                json!({"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":8,"totalTokenCount":12}}),
            ),
        ],
    }
}

async fn read_request(stream: &mut BufReader<TcpStream>) -> Value {
    let mut len = 0;
    loop {
        let mut line = String::new();
        assert!(stream.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = value.trim().parse::<usize>().unwrap();
        }
    }
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn stream_reply(stream: &mut BufReader<TcpStream>, frames: Vec<String>, stall: bool) {
    let size = frames.iter().map(String::len).sum::<usize>();
    stream.get_mut().write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    for (i, frame) in frames.into_iter().enumerate() {
        for part in frame.as_bytes().chunks(7) {
            if stream.get_mut().write_all(part).await.is_err() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(if stall && i == 0 {
            600
        } else {
            45
        }))
        .await;
    }
}

fn limits() -> RuntimeLimits {
    let limit = |n| NonZeroUsize::new(n).unwrap();
    RuntimeLimits {
        request_queue: limit(4),
        discovery_buffer: limit(16),
        character_commands: limit(16),
        presenter_updates: limit(16),
        retained_finished: limit(16),
    }
}

async fn run_stream(
    protocol: Option<OpenAiProtocol>,
    truncate: bool,
    retry: bool,
    cancel: bool,
    stall: bool,
) {
    let _guard = APPLICATION.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let selected = protocol;
    let server = tokio::spawn(async move {
        for i in 0..(if retry { 2 } else { 1 }) {
            let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut stream = BufReader::new(socket);
            let request = read_request(&mut stream).await;
            if selected.is_none() {
                assert_eq!(request["generationConfig"]["candidateCount"], 1);
            } else {
                assert_eq!(request["stream"], true);
            }
            if retry && i == 0 {
                let status = if selected == Some(OpenAiProtocol::Responses) {
                    "429 Too Many Requests"
                } else {
                    "503 Service Unavailable"
                };
                stream.get_mut().write_all(format!("HTTP/1.1 {status}\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                continue;
            }
            let mut frames = response_frames(selected);
            if truncate {
                frames.truncate(1);
            }
            stream_reply(&mut stream, frames, stall).await;
        }
    });
    let (template, provider): (ProviderTemplate, Arc<dyn GenerationProvider>) =
        if let Some(protocol) = protocol {
            let mut options = protocol.options();
            options.stream = true;
            let config = OpenAiPresetDefinition {
                protocol,
                endpoint: format!("{endpoint}chat"),
                model: "stream-fixture".into(),
                credential: None,
                temperature: 0.2,
                max_output_tokens: 256,
                timeout_ms: if stall { 250 } else { 5000 },
                options,
                transforms: protocol.transforms(),
            };
            (
                ProviderTemplate::OpenAi(OpenAiTemplate::new(config, None).unwrap()),
                Arc::new(OpenAiProvider::new().unwrap()),
            )
        } else {
            let options = GeminiRequestOptions {
                stream: true,
                ..Default::default()
            };
            let template = GeminiTemplate::new(
                &endpoint,
                "stream-fixture",
                "fixture-key",
                GenerationSettings::new(
                    0.2,
                    256,
                    Duration::from_millis(if stall { 250 } else { 5000 }),
                )
                .unwrap(),
            )
            .unwrap()
            .with_options(options)
            .unwrap();
            (
                ProviderTemplate::Gemini(template),
                Arc::new(GeminiProvider::new().unwrap()),
            )
        };
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(template),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "hello".into())
        .unwrap();
    let mut updates = request.observe();
    let mut observed_partial = false;
    let mut cancelled = false;
    let final_state = timeout(Duration::from_secs(8), async {
        while let Some(snapshot) = updates.next().await.unwrap() {
            if snapshot.streamed_json.is_some() {
                observed_partial = true;
                if cancel && !cancelled {
                    cancelled = true;
                    request.cancel();
                }
            }
            if snapshot.state.is_terminal() {
                return snapshot.state.clone();
            }
        }
        panic!("request closed without terminal update");
    })
    .await
    .unwrap();
    if !truncate || cancel {
        assert!(
            observed_partial,
            "streaming must produce provisional request updates"
        );
    }
    match final_state {
        RequestState::Cancelled if cancel => {}
        RequestState::Finished(RequestOutcome::Success(response)) if !truncate && !stall => {
            assert_eq!(&*response.text, "stream response");
        }
        RequestState::Finished(RequestOutcome::TextGenerationFailed(_)) if truncate || stall => {}
        other => panic!("unexpected stream result: {other:?}"),
    }
    app.shutdown().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn chat_completions_responses_and_gemini_stream_to_request_snapshots() {
    for protocol in [
        Some(OpenAiProtocol::Compatible),
        Some(OpenAiProtocol::Responses),
        None,
    ] {
        run_stream(protocol, false, false, false, false).await;
    }
}

#[tokio::test]
async fn truncated_stream_rejects_response_without_acceptance() {
    run_stream(Some(OpenAiProtocol::Compatible), true, false, false, false).await;
}

#[tokio::test]
async fn rate_limited_stream_retries_before_reading_body() {
    run_stream(Some(OpenAiProtocol::Responses), false, true, false, false).await;
    run_stream(Some(OpenAiProtocol::Compatible), false, true, false, false).await;
}

#[tokio::test]
async fn cancelling_stream_discards_partial_and_no_response_is_accepted() {
    run_stream(Some(OpenAiProtocol::Compatible), false, false, true, false).await;
}

#[tokio::test]
async fn total_request_deadline_stops_stalled_stream() {
    run_stream(Some(OpenAiProtocol::Compatible), false, false, false, true).await;
}
