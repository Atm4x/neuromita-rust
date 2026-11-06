use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator, ToolSettings,
        openai::{OpenAiPresetDefinition, OpenAiProtocol, OpenAiProvider, OpenAiTemplate},
    },
    request::{RequestOutcome, RequestState},
    settings::SettingsApi,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    time::timeout,
};

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn receive(reader: &mut BufReader<TcpStream>) -> Value {
    let mut size = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            size = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut data = vec![0; size.expect("request has Content-Length")];
    reader.read_exact(&mut data).await.unwrap();
    serde_json::from_slice(&data).unwrap()
}

async fn respond(reader: &mut BufReader<TcpStream>, payload: Value, stream: bool) {
    let data = if stream {
        payload
            .as_array()
            .unwrap()
            .iter()
            .map(|frame| {
                if frame == "[DONE]" {
                    "data: [DONE]\r\n\r\n".to_string()
                } else {
                    format!("data: {frame}\r\n\r\n")
                }
            })
            .collect::<String>()
    } else {
        payload.to_string()
    };
    let kind = if stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    reader.get_mut().write_all(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        data.len()
    ).as_bytes()).await.unwrap();
    for chunk in data.as_bytes().chunks(13) {
        reader.get_mut().write_all(chunk).await.unwrap();
    }
}

fn fixture_response(protocol: OpenAiProtocol, stream: bool, first: bool) -> Value {
    let args = r#"{"expression":"2**3+1"}"#;
    let answer = r#"{"segments":[{"text":"9"}]}"#;
    if !stream {
        if protocol == OpenAiProtocol::Responses {
            if first {
                json!({"status":"completed","output":[{"type":"function_call","call_id":"call_123","name":"calculator","arguments":args}]})
            } else {
                json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":answer}]}]})
            }
        } else if first {
            json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_123","type":"function","function":{"name":"calculator","arguments":args}}]}}]})
        } else {
            json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":answer}}]})
        }
    } else if protocol == OpenAiProtocol::Responses {
        if first {
            json!([
                {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_123","name":"calculator"}},
                {"type":"response.function_call_arguments.delta","delta":"{\"expression\":\"2**3"},
                {"type":"response.function_call_arguments.delta","delta":"+1\"}"},
                {"type":"response.function_call_arguments.done","arguments":args},
                {"type":"response.completed","response":{"status":"completed"}}
            ])
        } else {
            json!([
                {"type":"response.output_text.delta","delta":answer},
                {"type":"response.completed","response":{"status":"completed"}}
            ])
        }
    } else if first {
        json!([
            {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_123","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\""}}]},"finish_reason":null}]},
            {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"2**3+1\"}"}}]},"finish_reason":null}]},
            {"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]},
            "[DONE]"
        ])
    } else {
        json!([
            {"choices":[{"index":0,"delta":{"content":answer},"finish_reason":null}]},
            {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]},
            "[DONE]"
        ])
    }
}

fn limits() -> RuntimeLimits {
    let n = |count| NonZeroUsize::new(count).unwrap();
    RuntimeLimits {
        request_queue: n(4),
        discovery_buffer: n(16),
        character_commands: n(16),
        presenter_updates: n(16),
        retained_finished: n(8),
    }
}

async fn scenario(protocol: OpenAiProtocol, stream: bool) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for first in [true, false] {
            let (socket, _) = timeout(Duration::from_secs(10), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut client = BufReader::new(socket);
            let request = receive(&mut client).await;
            let tools = request["tools"]
                .as_array()
                .expect("native tools advertised");
            assert_eq!(tools.len(), 1);
            if protocol == OpenAiProtocol::Responses {
                assert_eq!(tools[0]["name"], "calculator");
                if !first {
                    let input = request["input"].as_array().unwrap();
                    assert!(input.iter().any(
                        |item| item["type"] == "function_call" && item["call_id"] == "call_123"
                    ));
                    assert!(
                        input
                            .iter()
                            .any(|item| item["type"] == "function_call_output"
                                && item["call_id"] == "call_123"
                                && item["output"].as_str().unwrap().contains('9'))
                    );
                }
            } else {
                assert_eq!(tools[0]["function"]["name"], "calculator");
                if !first {
                    let input = request["messages"].as_array().unwrap();
                    assert!(input.iter().any(|item| item["role"] == "assistant"
                        && item["tool_calls"][0]["id"] == "call_123"));
                    assert!(input.iter().any(|item| item["role"] == "tool"
                        && item["tool_call_id"] == "call_123"
                        && item["content"].as_str().unwrap().contains('9')));
                }
            }
            respond(
                &mut client,
                fixture_response(protocol, stream, first),
                stream,
            )
            .await;
        }
    });
    let mut options = protocol.options();
    options.stream = stream;
    options.max_retries = 0;
    let definition = OpenAiPresetDefinition {
        protocol,
        endpoint,
        model: "mock".into(),
        credential: None,
        temperature: 0.2,
        max_output_tokens: 128,
        timeout_ms: 5000,
        options,
        transforms: Vec::new(),
    };
    let provider: Arc<dyn GenerationProvider> = Arc::new(OpenAiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::OpenAi(
            OpenAiTemplate::new(definition, None).unwrap(),
        )),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
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
        .reply(CharacterId::Crazy, "how much is 2**3+1?".into())
        .unwrap();
    let result = timeout(Duration::from_secs(10), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(result.state, RequestState::Finished(RequestOutcome::Success(ref reply)) if reply.text.as_ref() == "9"),
        "{result:?}"
    );
    timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn chat_json_native_tool_roundtrip() {
    scenario(OpenAiProtocol::Compatible, false).await;
}

#[tokio::test]
async fn responses_json_native_tool_roundtrip() {
    scenario(OpenAiProtocol::Responses, false).await;
}

#[tokio::test]
async fn chat_sse_native_tool_roundtrip() {
    scenario(OpenAiProtocol::Compatible, true).await;
}

#[tokio::test]
async fn responses_sse_native_tool_roundtrip() {
    scenario(OpenAiProtocol::Responses, true).await;
}

#[tokio::test]
async fn multiple_native_calls_fail_closed() {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut client = BufReader::new(socket);
        let _ = receive(&mut client).await;
        let answer = json!({"choices":[{"finish_reason":"tool_calls","message":{
            "content":null,"tool_calls":[
                {"id":"call_a","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\"2+2\"}"}},
                {"id":"call_b","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\"3+3\"}"}}
            ]
        }}]});
        respond(&mut client, answer, false).await;
    });
    let definition = OpenAiPresetDefinition {
        protocol: OpenAiProtocol::Compatible,
        endpoint,
        model: "mock".into(),
        credential: None,
        temperature: 0.2,
        max_output_tokens: 128,
        timeout_ms: 3000,
        options: OpenAiProtocol::Compatible.options(),
        transforms: Vec::new(),
    };
    let provider: Arc<dyn GenerationProvider> = Arc::new(OpenAiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::OpenAi(
            OpenAiTemplate::new(definition, None).unwrap(),
        )),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = true;
        tools.calculator = true;
    })
    .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "compute".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
        ),
        "{snapshot:?}"
    );
    assert!(
        format!("{:?}", snapshot.state).contains("multiple")
            || format!("{:?}", snapshot.state).contains("mixed")
    );
    server.await.unwrap();
    app.shutdown().await.unwrap();
}

async fn rejected_case(
    protocol: OpenAiProtocol,
    stream: bool,
    invalid_response: Value,
    settings_enabled: bool,
    expected_error: &str,
) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/generate", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut client = BufReader::new(socket);
        let body = receive(&mut client).await;
        assert_eq!(body["tools"].is_array(), settings_enabled);
        respond(&mut client, invalid_response, stream).await;
    });
    let mut options = protocol.options();
    options.stream = stream;
    options.max_retries = 0;
    let definition = OpenAiPresetDefinition {
        protocol,
        endpoint,
        model: "mock".into(),
        credential: None,
        temperature: 0.2,
        max_output_tokens: 128,
        timeout_ms: 3000,
        options,
        transforms: Vec::new(),
    };
    let provider: Arc<dyn GenerationProvider> = Arc::new(OpenAiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::OpenAi(
            OpenAiTemplate::new(definition, None).unwrap(),
        )),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = settings_enabled;
        tools.calculator = settings_enabled;
        tools.memory_search = false;
    })
    .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "calculate".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(5), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            snapshot.state,
            RequestState::Finished(RequestOutcome::TextGenerationFailed(_))
        ),
        "{snapshot:?}"
    );
    assert!(
        format!("{:?}", snapshot.state).contains(expected_error),
        "{snapshot:?}"
    );
    server.await.unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_tool_arguments_must_be_an_object() {
    rejected_case(
        OpenAiProtocol::Compatible,
        false,
        json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"call_a","type":"function","function":{"name":"calculator","arguments":"[]"}}]}}]}),
        true,
        "JSON object",
    ).await;
}

#[tokio::test]
async fn native_reminder_is_not_accidentally_executed() {
    rejected_case(
        OpenAiProtocol::Compatible,
        false,
        json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":[{"id":"call_a","type":"function","function":{"name":"reminder","arguments":"{}"}}]}}]}),
        false,
        "native reminder tool is not registered",
    ).await;
}

#[tokio::test]
async fn responses_mixed_message_and_call_are_rejected() {
    rejected_case(
        OpenAiProtocol::Responses,
        false,
        json!({"status":"completed","output":[
            {"type":"function_call","call_id":"call_a","name":"calculator","arguments":"{\"expression\":\"2+2\"}"},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"unaccepted"}]}
        ]}),
        true,
        "mixed native tool",
    ).await;
}

#[tokio::test]
async fn incomplete_chat_sse_tool_call_fails_without_continuation() {
    rejected_case(
        OpenAiProtocol::Compatible,
        true,
        json!([
            {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"calculator","arguments":"{\"expression\":\""}}]},"finish_reason":null}]},
            {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"2+2"}}]},"finish_reason":null}]}
        ]),
        true,
        "terminated before completion",
    ).await;
}
