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
    let n = |value| NonZeroUsize::new(value).unwrap();
    RuntimeLimits {
        request_queue: n(4),
        discovery_buffer: n(16),
        character_commands: n(16),
        presenter_updates: n(16),
        retained_finished: n(8),
    }
}

async fn start(endpoint: String, protocol: OpenAiProtocol, retries: u8) -> ApplicationLifecycle {
    let mut options = protocol.options();
    options.stream = true;
    options.max_retries = retries;
    let preset = OpenAiPresetDefinition {
        protocol,
        endpoint,
        model: "cycle67-stream".into(),
        credential: None,
        temperature: 0.2,
        max_output_tokens: 128,
        timeout_ms: 850,
        options,
        transforms: Vec::new(),
    };
    let provider: Arc<dyn GenerationProvider> = Arc::new(OpenAiProvider::new().unwrap());
    ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::OpenAi(
            OpenAiTemplate::new(preset, None).unwrap(),
        )),
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
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).await.unwrap();
    (reader, serde_json::from_slice(&body).unwrap())
}

async fn sse_headers(stream: &mut BufReader<TcpStream>) {
    stream
        .get_mut()
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
}

async fn sse_frame(stream: &mut BufReader<TcpStream>, event: Value) {
    let bytes = format!("data: {event}\r\n\r\n");
    for chunk in bytes.as_bytes().chunks(7) {
        if stream.get_mut().write_all(chunk).await.is_err() {
            return;
        }
    }
}

fn text_delta(protocol: OpenAiProtocol, delta: &str) -> Value {
    if protocol == OpenAiProtocol::Responses {
        json!({"type":"response.output_text.delta","delta":delta})
    } else {
        json!({"choices":[{"index":0,"delta":{"content":delta},"finish_reason":null}]})
    }
}

async fn completed_sse(stream: &mut BufReader<TcpStream>, protocol: OpenAiProtocol, text: &str) {
    sse_headers(stream).await;
    sse_frame(
        stream,
        text_delta(protocol, &json!({"segments":[{"text":text}]}).to_string()),
    )
    .await;
    let terminal = if protocol == OpenAiProtocol::Responses {
        json!({"type":"response.completed","response":{"status":"completed"}})
    } else {
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
    };
    sse_frame(stream, terminal).await;
    let _ = stream.get_mut().write_all(b"data: [DONE]\r\n\r\n").await;
}

fn assert_timeout(request: &neuromita::request::RequestHandle) {
    let snapshot = request.snapshot();
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(
                GenerationError::DeadlineExceeded
            ))
        ),
        "{snapshot:?}"
    );
    assert!(snapshot.streamed_json.is_none());
    assert!(snapshot.streamed_text.is_none());
    let public = neuromita::server::status(&snapshot);
    assert!(public.get("preview_text").is_none());
    assert!(matches!(
        ChatApi::diagnostics_for(DiagnosticScope::owner(
            CharacterId::Crazy,
            RequestSource::Presenter
        ))
        .unwrap()
        .request(request.id())
        .unwrap()
        .trace
        .view()
        .outcome,
        Some(DiagnosticOutcome::Failed {
            stage: FailureStage::ProviderTimeout,
            ..
        })
    ));
}

async fn partial_then_deadline(protocol: OpenAiProtocol) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (sent, observed) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut first, json) = receive(socket).await;
        assert_eq!(json["stream"], true);
        sse_headers(&mut first).await;
        sse_frame(
            &mut first,
            text_delta(protocol, "{\"segments\":[{\"text\":\"old "),
        )
        .await;
        sse_frame(&mut first, text_delta(protocol, "partial\"}]}")).await;
        sent.send(()).unwrap();
        let delayed = tokio::spawn(async move {
            let _ = released.await;
            let _ = sse_frame(
                &mut first,
                if protocol == OpenAiProtocol::Responses {
                    json!({"type":"response.completed","response":{"status":"completed"}})
                } else {
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
                },
            )
            .await;
        });
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut second, _) = receive(socket).await;
        completed_sse(&mut second, protocol, "fresh complete").await;
        drop(second);
        delayed.await.unwrap();
    });
    let app = start(endpoint, protocol, 0).await;
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "first partial".into())
        .unwrap();
    let mut updates = request.observe();
    timeout(Duration::from_secs(8), observed)
        .await
        .unwrap()
        .unwrap();
    let saw_partial = timeout(Duration::from_secs(8), async {
        while let Some(snapshot) = updates.next().await.unwrap() {
            if snapshot.streamed_json.is_some() {
                return true;
            }
            if snapshot.state.is_terminal() {
                return false;
            }
        }
        false
    })
    .await
    .unwrap();
    assert!(
        saw_partial,
        "expected a partial provisional streamed JSON update"
    );
    timeout(Duration::from_secs(8), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert_timeout(&request);

    let fresh = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "fresh".into())
        .unwrap();
    let state = timeout(Duration::from_secs(8), fresh.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(state.state, RequestState::Finished(RequestOutcome::Success(ref reply)) if reply.text.as_ref() == "fresh complete"),
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
async fn partial_chat_sse_timeout_clears_provisional_and_retries_freshly() {
    partial_then_deadline(OpenAiProtocol::Compatible).await;
}

#[tokio::test]
async fn partial_responses_sse_timeout_clears_provisional_and_retries_freshly() {
    partial_then_deadline(OpenAiProtocol::Responses).await;
}

async fn streaming_tool_continuation(protocol: OpenAiProtocol) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (sent, observed) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut first, request) = receive(socket).await;
        assert_eq!(request["stream"], true);
        sse_headers(&mut first).await;
        if protocol == OpenAiProtocol::Responses {
            sse_frame(&mut first, json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"cycle67_calc","name":"calculator"}})).await;
            sse_frame(&mut first, json!({"type":"response.function_call_arguments.delta","delta":"{\"expression\":\"2+3\"}"})).await;
            sse_frame(&mut first, json!({"type":"response.function_call_arguments.done","arguments":"{\"expression\":\"2+3\"}"})).await;
            sse_frame(
                &mut first,
                json!({"type":"response.completed","response":{"status":"completed"}}),
            )
            .await;
        } else {
            sse_frame(&mut first, json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"cycle67_calc","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\"2+3\"}"}}]},"finish_reason":null}]})).await;
            sse_frame(
                &mut first,
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
            )
            .await;
        }
        let _ = first.get_mut().write_all(b"data: [DONE]\r\n\r\n").await;
        drop(first);

        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut second, request) = receive(socket).await;
        let messages = request[if protocol == OpenAiProtocol::Responses {
            "input"
        } else {
            "messages"
        }]
        .as_array()
        .unwrap();
        if protocol == OpenAiProtocol::Responses {
            assert!(messages.iter().any(|m| m["type"] == "function_call_output"
                && m["call_id"] == "cycle67_calc"
                && m["output"].as_str().unwrap().contains('5')));
        } else {
            assert!(messages.iter().any(|m| m["role"] == "tool"
                && m["tool_call_id"] == "cycle67_calc"
                && m["content"].as_str().unwrap().contains('5')));
        }
        sse_headers(&mut second).await;
        sse_frame(
            &mut second,
            text_delta(
                protocol,
                "{\"segments\":[{\"text\":\"unaccepted continuation\"}]}",
            ),
        )
        .await;
        sent.send(()).unwrap();
        released.await.unwrap();
        let _ = sse_frame(
            &mut second,
            if protocol == OpenAiProtocol::Responses {
                json!({"type":"response.completed","response":{"status":"completed"}})
            } else {
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
            },
        )
        .await;
    });
    let app = start(endpoint, protocol, 0).await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
        tools.memory_search = false;
        tools.max_depth = 2;
    })
    .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "calculate 2+3".into())
        .unwrap();
    timeout(Duration::from_secs(8), observed)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(8), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert_timeout(&request);
    release.send(()).unwrap();
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn chat_sse_native_tool_continuation_uses_original_deadline() {
    streaming_tool_continuation(OpenAiProtocol::Compatible).await;
}

#[tokio::test]
async fn responses_sse_native_tool_continuation_uses_original_deadline() {
    streaming_tool_continuation(OpenAiProtocol::Responses).await;
}

#[tokio::test]
async fn retryable_http_status_then_partial_sse_still_uses_one_deadline() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut retry, _) = receive(socket).await;
        retry.get_mut().write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
        drop(retry);
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut stream, _) = receive(socket).await;
        sse_headers(&mut stream).await;
        sse_frame(
            &mut stream,
            text_delta(
                OpenAiProtocol::Compatible,
                "{\"segments\":[{\"text\":\"retry partial\"}]}",
            ),
        )
        .await;
        released.await.unwrap();
        let _ = sse_frame(
            &mut stream,
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        )
        .await;
    });
    let app = start(endpoint, OpenAiProtocol::Compatible, 1).await;
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "retry".into())
        .unwrap();
    timeout(Duration::from_secs(8), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert_timeout(&request);
    release.send(()).unwrap();
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

async fn chunked_headers(stream: &mut BufReader<TcpStream>) {
    stream
        .get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n")
        .await
        .unwrap();
}

async fn chunked_frame(stream: &mut BufReader<TcpStream>, data: &str) {
    let frame = format!("data: {data}\r\n\r\n");
    let chunk = format!("{:x}\r\n{frame}\r\n", frame.len());
    stream.get_mut().write_all(chunk.as_bytes()).await.unwrap();
}

async fn chunked_terminal(stream: &mut BufReader<TcpStream>, protocol: OpenAiProtocol, tool: bool) {
    let terminal = if protocol == OpenAiProtocol::Responses {
        json!({"type":"response.completed","response":{"status":"completed"}})
    } else {
        json!({"choices":[{"index":0,"delta":{},"finish_reason":if tool { "tool_calls" } else { "stop" }}]})
    };
    chunked_frame(stream, &terminal.to_string()).await;
    if protocol != OpenAiProtocol::Responses {
        chunked_frame(stream, "[DONE]").await;
    }
}

async fn terminal_without_connection_close(protocol: OpenAiProtocol) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (release, held) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut connection, request) = receive(socket).await;
        assert_eq!(request["stream"], true);
        chunked_headers(&mut connection).await;
        chunked_frame(
            &mut connection,
            &text_delta(
                protocol,
                &json!({"segments":[{"text":"completed on keepalive"}]}).to_string(),
            )
            .to_string(),
        )
        .await;
        chunked_terminal(&mut connection, protocol, false).await;
        let _ = held.await;
    });
    let app = start(endpoint, protocol, 0).await;
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "keepalive".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(3), request.wait())
        .await
        .unwrap()
        .unwrap();
    let _ = release.send(());
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::Success(ref response))
            if response.text.as_ref() == "completed on keepalive"
        ),
        "{snapshot:?}"
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn completed_chat_sse_does_not_wait_for_http_connection_close() {
    terminal_without_connection_close(OpenAiProtocol::Compatible).await;
}

#[tokio::test]
async fn completed_responses_sse_does_not_wait_for_http_connection_close() {
    terminal_without_connection_close(OpenAiProtocol::Responses).await;
}

async fn native_tool_without_connection_close(protocol: OpenAiProtocol) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (release, held) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut first, request) = receive(socket).await;
        assert_eq!(request["stream"], true);
        chunked_headers(&mut first).await;
        if protocol == OpenAiProtocol::Responses {
            chunked_frame(&mut first, &json!({"type":"response.output_item.added","item":{"type":"function_call","call_id":"cycle68_calc","name":"calculator"}}).to_string()).await;
            chunked_frame(&mut first, &json!({"type":"response.function_call_arguments.delta","delta":"{\"expression\":\"2+3\"}"}).to_string()).await;
            chunked_frame(&mut first, &json!({"type":"response.function_call_arguments.done","arguments":"{\"expression\":\"2+3\"}"}).to_string()).await;
        } else {
            chunked_frame(&mut first, &json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"cycle68_calc","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\"2+3\"}"}}]},"finish_reason":null}]}).to_string()).await;
        }
        chunked_terminal(&mut first, protocol, true).await;
        let (socket, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut second, request) = receive(socket).await;
        let events = request[if protocol == OpenAiProtocol::Responses {
            "input"
        } else {
            "messages"
        }]
        .as_array()
        .unwrap();
        assert!(events.iter().any(|event| {
            if protocol == OpenAiProtocol::Responses {
                event["type"] == "function_call_output"
                    && event["call_id"] == "cycle68_calc"
                    && event["output"]
                        .as_str()
                        .is_some_and(|value| value.contains('5'))
            } else {
                event["role"] == "tool"
                    && event["tool_call_id"] == "cycle68_calc"
                    && event["content"]
                        .as_str()
                        .is_some_and(|value| value.contains('5'))
            }
        }));
        chunked_headers(&mut second).await;
        chunked_frame(
            &mut second,
            &text_delta(
                protocol,
                &json!({"segments":[{"text":"tool continuation complete"}]}).to_string(),
            )
            .to_string(),
        )
        .await;
        chunked_terminal(&mut second, protocol, false).await;
        let _ = held.await;
    });
    let app = start(endpoint, protocol, 0).await;
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
        tools.memory_search = false;
        tools.max_depth = 2;
    })
    .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "calculate".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(3), request.wait())
        .await
        .unwrap()
        .unwrap();
    let _ = release.send(());
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::Success(ref response))
            if response.text.as_ref() == "tool continuation complete"
        ),
        "{snapshot:?}"
    );
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn chat_native_tool_continues_before_previous_http_connection_closes() {
    native_tool_without_connection_close(OpenAiProtocol::Compatible).await;
}

#[tokio::test]
async fn responses_native_tool_continues_before_previous_http_connection_closes() {
    native_tool_without_connection_close(OpenAiProtocol::Responses).await;
}

#[tokio::test]
async fn chat_terminal_without_done_does_not_bypass_deadline() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let (release, held) = oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut peer, _) = receive(socket).await;
        chunked_headers(&mut peer).await;
        chunked_frame(
            &mut peer,
            &text_delta(
                OpenAiProtocol::Compatible,
                &json!({"segments":[{"text":"not finished"}]}).to_string(),
            )
            .to_string(),
        )
        .await;
        chunked_frame(
            &mut peer,
            &json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string(),
        )
        .await;
        let _ = held.await;
    });
    let app = start(endpoint, OpenAiProtocol::Compatible, 0).await;
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "missing done".into())
        .unwrap();
    timeout(Duration::from_secs(3), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert_timeout(&request);
    let _ = release.send(());
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn content_after_done_in_one_http_chunk_is_rejected() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(8), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut peer, _) = receive(socket).await;
        chunked_headers(&mut peer).await;
        let text = text_delta(
            OpenAiProtocol::Compatible,
            &json!({"segments":[{"text":"complete"}]}).to_string(),
        );
        let stop = json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]});
        let extra = text_delta(OpenAiProtocol::Compatible, "late injected content");
        let all_events = format!(
            "data: {text}\r\n\r\ndata: {stop}\r\n\r\ndata: [DONE]\r\n\r\ndata: {extra}\r\n\r\n"
        );
        peer.get_mut()
            .write_all(format!("{:x}\r\n{all_events}\r\n", all_events.len()).as_bytes())
            .await
            .unwrap();
    });
    let app = start(endpoint, OpenAiProtocol::Compatible, 0).await;
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "extra event".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(3), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(
                GenerationError::Response(_)
            ))
        ),
        "{snapshot:?}"
    );
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}
