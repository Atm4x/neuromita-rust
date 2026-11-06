use std::{num::NonZeroUsize, sync::Arc, time::Duration};

use neuromita::{
    app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
    character::CharacterId,
    generation::{
        GenerationPreset, GenerationProvider, ProviderTemplate, ResponseGenerator, ToolSettings,
        gemini::{GeminiProvider, GeminiRequestOptions, GeminiTemplate, GenerationSettings},
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

async fn read_request(stream: &mut BufReader<TcpStream>) -> Value {
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(stream.read_line(&mut line).await.unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut data = vec![0; length.expect("Content-Length header")];
    stream.read_exact(&mut data).await.unwrap();
    serde_json::from_slice(&data).unwrap()
}

async fn respond(stream: &mut BufReader<TcpStream>, payload: &[Value], streaming: bool) {
    let data = if streaming {
        payload
            .iter()
            .map(|value| format!("data: {value}\r\n\r\n"))
            .collect::<String>()
    } else {
        payload[0].to_string()
    };
    let mime = if streaming {
        "text/event-stream"
    } else {
        "application/json"
    };
    stream.get_mut().write_all(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        data.len()
    ).as_bytes()).await.unwrap();
    for fragment in data.as_bytes().chunks(11) {
        stream.get_mut().write_all(fragment).await.unwrap();
    }
}

fn limits() -> RuntimeLimits {
    let limit = |n| NonZeroUsize::new(n).unwrap();
    RuntimeLimits {
        request_queue: limit(4),
        discovery_buffer: limit(16),
        character_commands: limit(16),
        presenter_updates: limit(16),
        retained_finished: limit(8),
    }
}

fn native_call(id: bool) -> Value {
    let mut call = json!({"name":"calculator","args":{"expression":"2+3"}});
    if id {
        call["id"] = json!("call_123");
    }
    json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[
        {"text":"internal thought","thought":true,"thoughtSignature":"thought_signature_1"},
        {"functionCall":call,"thoughtSignature":"tool_signature_2"}
    ]}}]})
}

fn completed() -> Value {
    json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[
        {"text":"{\"segments\":[{\"text\":\"5\"}]}"}
    ]}}]})
}

async fn scenario(streaming: bool, with_id: bool, split_thoughts: bool) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for first in [true, false] {
            let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut client = BufReader::new(socket);
            let request = read_request(&mut client).await;
            assert_eq!(
                request["tools"][0]["functionDeclarations"][0]["name"],
                "calculator"
            );
            assert_eq!(
                request["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"]["type"],
                "object"
            );
            assert!(
                request["tools"][0]["functionDeclarations"][0]
                    .get("parameters")
                    .is_none()
            );
            assert_eq!(
                request["toolConfig"]["functionCallingConfig"]["mode"],
                "AUTO"
            );
            assert!(
                request["generationConfig"]
                    .get("responseMimeType")
                    .is_none()
            );
            assert!(request["generationConfig"].get("responseSchema").is_none());
            if first {
                assert!(!request["contents"].as_array().unwrap().iter().any(|part| {
                    part["parts"].as_array().is_some_and(|parts| {
                        parts.iter().any(|p| p.get("functionResponse").is_some())
                    })
                }));
                if split_thoughts {
                    let response = native_call(with_id);
                    let parts = response["candidates"][0]["content"]["parts"]
                        .as_array()
                        .unwrap();
                    let thought = parts[0].clone();
                    let tool = parts[1].clone();
                    respond(&mut client, &[
                        json!({"candidates":[{"content":{"role":"model","parts":[thought]}}]}),
                        json!({"candidates":[{"finishReason":"STOP","content":{"role":"model","parts":[tool]}}]})
                    ], streaming).await;
                } else {
                    respond(&mut client, &[native_call(with_id)], streaming).await;
                }
            } else {
                let contents = request["contents"].as_array().unwrap();
                let model = contents
                    .iter()
                    .find(|c| {
                        c["parts"].as_array().is_some_and(|parts| {
                            parts.iter().any(|p| p.get("functionCall").is_some())
                        })
                    })
                    .expect("original functionCall replayed");
                assert_eq!(model["parts"][0]["thoughtSignature"], "thought_signature_1");
                assert_eq!(model["parts"][1]["thoughtSignature"], "tool_signature_2");
                assert_eq!(model["parts"][1]["functionCall"]["id"].is_string(), with_id);
                let response = contents
                    .iter()
                    .find(|c| {
                        c["parts"].as_array().is_some_and(|parts| {
                            parts.iter().any(|p| p.get("functionResponse").is_some())
                        })
                    })
                    .expect("functionResponse sent");
                let tool = &response["parts"][0]["functionResponse"];
                assert_eq!(tool["name"], "calculator");
                assert_eq!(tool["id"].is_string(), with_id);
                assert!(tool["response"]["result"].as_str().unwrap().contains('5'));
                respond(&mut client, &[completed()], streaming).await;
            }
        }
    });
    let options = GeminiRequestOptions {
        stream: streaming,
        max_retries: 0,
        ..GeminiRequestOptions::default()
    };
    let template = GeminiTemplate::new(
        &base,
        "mock",
        "test-key",
        GenerationSettings::new(0.2, 256, Duration::from_secs(4)).unwrap(),
    )
    .unwrap()
    .with_options(options)
    .unwrap();
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
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
        .reply(CharacterId::Crazy, "calculate 2+3".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(8), request.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(snapshot.state, RequestState::Finished(RequestOutcome::Success(ref r)) if r.text.as_ref() == "5"),
        "{snapshot:?}"
    );
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn json_tool_with_id_and_thought_signature() {
    scenario(false, true, false).await
}
#[tokio::test]
async fn sse_tool_with_id_and_thought_signature() {
    scenario(true, true, false).await
}
#[tokio::test]
async fn sse_tool_preserves_thought_signature_from_prior_event() {
    scenario(true, true, true).await;
}

#[tokio::test]
async fn json_tool_without_optional_id() {
    scenario(false, false, false).await
}
#[tokio::test]
async fn sse_tool_without_optional_id() {
    scenario(true, false, false).await
}

async fn rejected(streaming: bool, frames: Vec<Value>, tool_enabled: bool, expected_error: &str) {
    let _guard = SERIAL.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut client = BufReader::new(socket);
        let request = read_request(&mut client).await;
        assert_eq!(request["tools"].is_array(), tool_enabled);
        respond(&mut client, &frames, streaming).await;
    });
    let options = GeminiRequestOptions {
        stream: streaming,
        max_retries: 0,
        ..GeminiRequestOptions::default()
    };
    let template = GeminiTemplate::new(
        &base,
        "mock",
        "key",
        GenerationSettings::new(0.2, 128, Duration::from_secs(4)).unwrap(),
    )
    .unwrap()
    .with_options(options)
    .unwrap();
    let provider: Arc<dyn GenerationProvider> = Arc::new(GeminiProvider::new().unwrap());
    let app = ApplicationLifecycle::start(ApplicationConfig::minimal(
        GenerationPreset::new(ProviderTemplate::Gemini(template)),
        ResponseGenerator::new([provider]).unwrap(),
        limits(),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .await
    .unwrap();
    SettingsApi::edit::<ToolSettings>(|tools| {
        tools.enabled = tool_enabled;
        tools.calculator = tool_enabled;
        tools.memory_search = false;
        tools.max_depth = 2;
    })
    .unwrap();
    let request = app
        .presenter()
        .conversation
        .reply(CharacterId::Crazy, "calculate".into())
        .unwrap();
    let snapshot = timeout(Duration::from_secs(8), request.wait())
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
    timeout(Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn two_calls_rejected_as_whole_turn() {
    let c = json!({"name":"calculator","args":{"expression":"2+3"}});
    rejected(false, vec![json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":c},{"functionCall":c}]}}]})], true, "mixed").await;
}
#[tokio::test]
async fn native_and_spoken_text_cannot_mix() {
    rejected(false, vec![json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":"unaccepted"},{"functionCall":{"name":"calculator","args":{"expression":"2+3"}}}]}}]})], true, "mixed").await;
}
#[tokio::test]
async fn partial_arguments_rejected() {
    rejected(true, vec![json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":{"name":"calculator","partialArgs":[{"jsonPath":"$.expression","stringValue":"2+3"}],"willContinue":false}}]}}]})], true, "partial").await;
}
#[tokio::test]
async fn tool_disabled_fails_closed() {
    rejected(
        false,
        vec![native_call(true)],
        false,
        "disabled or unknown tool",
    )
    .await;
}
#[tokio::test]
async fn malformed_args_rejected() {
    rejected(false, vec![json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"functionCall":{"name":"calculator","args":"2+3"}}]}}]})], true, "invalid").await;
}
#[tokio::test]
async fn incomplete_stream_rejected() {
    rejected(true, vec![json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"calculator","args":{"expression":"2+3"}}}]}}]})], true, "terminated before completion").await;
}
