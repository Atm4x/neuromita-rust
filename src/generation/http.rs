use super::{GenerationError, GenerationObserver, ProviderReply, ProviderUsage};
use std::{sync::Arc, time::Instant};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;
const MAX_RETRIES: u8 = 4;

#[derive(Default)]
pub(crate) struct ResponseMetadata {
    pub answer_text: Option<Arc<str>>,
    pub finish_reason: Option<Arc<str>>,
    pub usage: Option<ProviderUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeCall {
    pub id: String,
    pub name: String,
    pub args: String,
    pub metadata: Option<serde_json::Value>,
}

pub(crate) enum NativeEvent {
    Start {
        id: String,
        name: String,
        arguments: String,
        metadata: Option<serde_json::Value>,
    },
    Arguments(String),
    Complete(String),
}

pub(crate) struct StreamUpdate {
    pub delta: String,
    pub native: Option<NativeEvent>,
    pub finish_reason: Option<String>,
    pub usage: Option<ProviderUsage>,
    pub terminal: bool,
}

impl StreamUpdate {
    pub fn progress(delta: impl Into<String>) -> Self {
        Self {
            delta: delta.into(),
            native: None,
            finish_reason: None,
            usage: None,
            terminal: false,
        }
    }
}

pub(crate) struct StreamResult {
    pub text: String,
    pub native_call: Option<NativeCall>,
    pub finish_reason: Option<String>,
}

fn retry_delay(attempt: usize) -> u64 {
    (200_u64 << attempt.min(3)).min(1600)
}

fn provider_request_error(error: reqwest::Error) -> GenerationError {
    if error.is_timeout() {
        GenerationError::DeadlineExceeded
    } else {
        GenerationError::Transport(error.without_url().to_string())
    }
}

async fn send_with_retry(
    request: reqwest::RequestBuilder,
    retries: u8,
) -> Result<reqwest::Response, GenerationError> {
    if retries > MAX_RETRIES {
        return Err(GenerationError::Response("invalid retry count".into()));
    }
    let original = request;
    for attempt in 0..=usize::from(retries) {
        let next = original.try_clone().ok_or_else(|| {
            GenerationError::Transport("provider request cannot be replayed".into())
        })?;
        match next.send().await {
            Ok(response) => {
                let status = response.status();
                if !matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
                    || attempt == usize::from(retries)
                {
                    return Ok(response);
                }
                let delay = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|seconds| seconds.saturating_mul(1000).min(2000))
                    .unwrap_or(retry_delay(attempt));
                drop(response);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(error) => {
                if attempt == usize::from(retries) || !error.is_connect() {
                    return Err(provider_request_error(error));
                }
                tokio::time::sleep(std::time::Duration::from_millis(retry_delay(attempt))).await;
            }
        }
    }
    unreachable!("all retry attempts return")
}

pub(crate) async fn send_json(
    request: reqwest::RequestBuilder,
    retries: u8,
    observer: &mut dyn GenerationObserver,
    metadata: impl Fn(&[u8]) -> ResponseMetadata,
) -> Result<Vec<u8>, GenerationError> {
    let started = Instant::now();
    let mut response = match send_with_retry(request, retries).await {
        Ok(response) => response,
        Err(error) => {
            observer.failed(&error);
            return Err(error);
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(chunk) => chunk,
            Err(error) => {
                let error = provider_request_error(error);
                observer.response_received(ProviderReply {
                    status: Some(status.as_u16()),
                    raw_body: String::from_utf8_lossy(&bytes).into_owned().as_str().into(),
                    answer_text: None,
                    complete: false,
                    finish_reason: None,
                    usage: None,
                    elapsed: started.elapsed(),
                });
                observer.failed(&error);
                return Err(error);
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            let remaining = MAX_RESPONSE_BYTES.saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..remaining.min(chunk.len())]);
            observer.response_received(ProviderReply {
                status: Some(status.as_u16()),
                raw_body: String::from_utf8_lossy(&bytes).into_owned().as_str().into(),
                answer_text: None,
                complete: false,
                finish_reason: None,
                usage: None,
                elapsed: started.elapsed(),
            });
            let error = GenerationError::Response("response exceeds 8 MiB".into());
            observer.failed(&error);
            return Err(error);
        }
        bytes.extend_from_slice(&chunk);
    }
    let raw_body = String::from_utf8_lossy(&bytes).into_owned();
    let observation = metadata(&bytes);
    observer.response_received(ProviderReply {
        status: Some(status.as_u16()),
        raw_body: raw_body.as_str().into(),
        answer_text: observation.answer_text,
        complete: true,
        finish_reason: observation.finish_reason,
        usage: observation.usage,
        elapsed: started.elapsed(),
    });
    if !status.is_success() {
        let error = GenerationError::Http(status.as_u16());
        observer.failed(&error);
        return Err(error);
    }
    Ok(bytes)
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<String>,
}

impl SseDecoder {
    fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, GenerationError> {
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_SSE_EVENT_BYTES {
            return Err(GenerationError::Response("SSE line exceeds 1 MiB".into()));
        }
        let mut events = Vec::new();
        while let Some(newline) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data.is_empty() {
                    events.push(self.data.join("\n"));
                    self.data.clear();
                }
                continue;
            }
            if let Some(data) = line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                self.data.push(
                    String::from_utf8(data.to_vec())
                        .map_err(|_| GenerationError::Response("invalid SSE UTF-8".into()))?,
                );
                if self.data.iter().map(String::len).sum::<usize>() > MAX_SSE_EVENT_BYTES {
                    return Err(GenerationError::Response("SSE event exceeds 1 MiB".into()));
                }
            }
        }
        Ok(events)
    }

    fn complete(&self) -> bool {
        self.pending.is_empty() && self.data.is_empty()
    }
}

pub(crate) async fn send_sse(
    request: reqwest::RequestBuilder,
    retries: u8,
    observer: &mut dyn GenerationObserver,
    mut parse: impl FnMut(&str) -> Result<StreamUpdate, GenerationError>,
    requires_done: bool,
) -> Result<StreamResult, GenerationError> {
    let started = Instant::now();
    let mut response = match send_with_retry(request, retries).await {
        Ok(response) => response,
        Err(error) => {
            observer.failed(&error);
            return Err(error);
        }
    };
    let status = response.status();
    let mut raw = Vec::new();
    let mut decoder = SseDecoder::default();
    let mut text = String::new();
    let mut native_call: Option<NativeCall> = None;
    let mut finish_reason = None;
    let mut usage = None;
    let mut terminal = false;
    let mut done_marker = false;
    let mut failure = None;

    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                failure = Some(provider_request_error(error));
                break;
            }
        };
        if raw.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            failure = Some(GenerationError::Response(
                "SSE response exceeds 8 MiB".into(),
            ));
            break;
        }
        raw.extend_from_slice(&chunk);
        if !status.is_success() {
            continue;
        }
        match decoder.feed(&chunk) {
            Ok(events) => {
                for event in events {
                    if event == "[DONE]" {
                        done_marker = true;
                        continue;
                    }
                    if done_marker {
                        failure = Some(GenerationError::Response(
                            "SSE events received after completion".into(),
                        ));
                        break;
                    }
                    match parse(&event) {
                        Ok(update) => {
                            if terminal
                                && (!update.delta.is_empty()
                                    || update.native.is_some()
                                    || update.terminal
                                    || update.finish_reason.is_some())
                            {
                                failure = Some(GenerationError::Response(
                                    "SSE emitted content after completion".into(),
                                ));
                                break;
                            }
                            if let Some(native) = update.native {
                                let next = match native {
                                    NativeEvent::Start {
                                        id,
                                        name,
                                        arguments,
                                        metadata,
                                    } => {
                                        if native_call.is_some()
                                            || !text.is_empty()
                                            || arguments.len() > 16_384
                                        {
                                            Err(GenerationError::Response(
                                                "multiple or mixed native tool calls".into(),
                                            ))
                                        } else {
                                            native_call = Some(NativeCall {
                                                id,
                                                name,
                                                args: arguments,
                                                metadata,
                                            });
                                            Ok(())
                                        }
                                    }
                                    NativeEvent::Arguments(part) => match native_call.as_mut() {
                                        Some(call)
                                            if call.args.len().saturating_add(part.len())
                                                <= 16_384 =>
                                        {
                                            call.args.push_str(&part);
                                            Ok(())
                                        }
                                        _ => Err(GenerationError::Response(
                                            "tool arguments without a bounded call".into(),
                                        )),
                                    },
                                    NativeEvent::Complete(arguments) => {
                                        match native_call.as_mut() {
                                            Some(call)
                                                if arguments.len() <= 16_384
                                                    && (call.args.is_empty()
                                                        || call.args == arguments) =>
                                            {
                                                call.args = arguments;
                                                Ok(())
                                            }
                                            _ => Err(GenerationError::Response(
                                                "tool call arguments mismatch".into(),
                                            )),
                                        }
                                    }
                                };
                                if let Err(error) = next {
                                    failure = Some(error);
                                    break;
                                }
                            }
                            if !update.delta.is_empty() {
                                if native_call.is_some() {
                                    failure = Some(GenerationError::Response(
                                        "native tool call mixed with assistant text".into(),
                                    ));
                                    break;
                                }
                                if text.len().saturating_add(update.delta.len())
                                    > MAX_RESPONSE_BYTES
                                {
                                    failure = Some(GenerationError::Response(
                                        "streamed answer exceeds 8 MiB".into(),
                                    ));
                                    break;
                                }
                                text.push_str(&update.delta);
                                observer.stream_delta(&update.delta);
                            }
                            if let Some(reason) = update.finish_reason {
                                finish_reason = Some(reason);
                            }
                            if let Some(counts) = update.usage {
                                usage = Some(counts);
                            }
                            terminal |= update.terminal;
                        }
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
                if failure.is_some() {
                    break;
                }
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
        if failure.is_none() && terminal && (!requires_done || done_marker) && decoder.complete() {
            break;
        }
    }

    if failure.is_none() && !status.is_success() {
        failure = Some(GenerationError::Http(status.as_u16()));
    }
    if failure.is_none() && (!terminal || (requires_done && !done_marker) || !decoder.complete()) {
        failure = Some(GenerationError::Response(
            "SSE stream terminated before completion".into(),
        ));
    }
    let complete = failure.is_none();
    observer.response_received(ProviderReply {
        status: Some(status.as_u16()),
        raw_body: String::from_utf8_lossy(&raw).into_owned().into(),
        answer_text: (!text.is_empty()).then(|| Arc::from(text.as_str())),
        complete,
        finish_reason: finish_reason.as_deref().map(Arc::from),
        usage,
        elapsed: started.elapsed(),
    });
    if let Some(error) = failure {
        observer.failed(&error);
        return Err(error);
    }
    Ok(StreamResult {
        text,
        native_call,
        finish_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_handles_split_frames_multiline_payload_and_crlf() {
        let mut parser = SseDecoder::default();
        assert!(
            parser
                .feed(b": keepalive\r\ndata: {\"x\":")
                .unwrap()
                .is_empty()
        );
        assert_eq!(parser.feed(b"1}\r\n\r\n").unwrap(), ["{\"x\":1}"]);
        assert!(
            parser
                .feed(b"event: update\ndata: hello\n")
                .unwrap()
                .is_empty()
        );
        assert_eq!(parser.feed(b"data: world\n\n").unwrap(), ["hello\nworld"]);
        assert!(parser.complete());
    }

    #[test]
    fn sse_rejects_oversized_unterminated_frame() {
        assert!(
            SseDecoder::default()
                .feed(&vec![b'a'; MAX_SSE_EVENT_BYTES + 1])
                .is_err()
        );
    }
}
