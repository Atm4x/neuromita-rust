# Rust LLM streaming and bounded retry (#17)

This document describes the implemented Rust provider contracts for OpenAI-compatible Chat Completions, OpenAI Responses and Gemini. The authoritative implementation is `src/generation/{http,openai}.rs`, `src/generation/gemini/*`, `src/request/{handler,record,model}.rs`.

## Preset settings

Both `OpenAiOptions` and `GeminiRequestOptions` expose:

- `stream: bool` — defaults to `false` for existing stored presets. Enabling it switches the wire protocol to SSE.
- `max_retries: u8` — defaults to `2`, valid range `0..=4`. A value of zero disables retries.

OpenAI Chat Completions and Responses use `stream: true` in the JSON payload. Gemini uses `:streamGenerateContent?alt=sse`. The existing provider-specific structured output settings remain in effect.

## Network and completion semantics

The SSE reader accepts arbitrarily fragmented UTF-8/CRLF frames, multiple `data:` lines per event and comment/keepalive frames. Responses are limited to 8 MiB, SSE events and unfinished lines to 1 MiB. Terminal markers:

- Chat Completions — a single choice with `finish_reason: stop` followed by `[DONE]`.
- Responses — `response.completed` containing status `completed`; explicit failure/incomplete events reject the stream.
- Gemini — one candidate with `finishReason: STOP`; thought parts are excluded from the accumulated answer.

Refusal and unsupported tool/function deltas are rejected. The complete text must parse as `StructuredResponse`. Application effects go through the existing validated `ProposedResponse` / Timeline acceptance path once, after the provider has completed. An invalid, interrupted or cancelled stream cannot accept a partial response.

The `GenerationObserver::stream_delta` callback exposes **unvalidated JSON** as it arrives. `RequestSnapshot.streamed_json` provides a bounded, in-process read-side projection (64 KiB) with increasing revisions while the request is generating text. The request also has a bounded, **read-only spoken preview** (`streamed_text`) extracted solely from the top-level `segments[*].text` JSON string values; the scanner handles truncated strings, escaped text and nested non-spoken fields. It does not expose `reasoning`, tool calls, commands, memory mutations, or game actions. Because the model's JSON is incomplete, this is **provisional text** rather than a validated/accepted reply. The preview is reset whenever the request leaves the generating-text stage. The committed message still comes exclusively from the existing validated `StructuredResponse` path.

External LocalControl and GameLink request status frames expose `streamed_bytes` and, when available, `preview_text` with `provisional: true`. They never publish raw streamed JSON. Sandbox renders `streamed_text` in the existing assistant bubble with a provisional accent and a `Печатает · черновик, ответ ещё не принят` status. It never displays private `reasoning` as message content. Future provider-specific reasoning channels remain separate work under #17.

## Retry, timeout and cancellation

Retry occurs only while acquiring HTTP response headers. HTTP 429, 500, 502, 503 and 504, as well as connection failures, are eligible. Other transport failures and mid-body errors terminate the attempt. Backoff is bounded (200/400/800/1600 ms); numeric `Retry-After` values are honored up to 2 seconds. Both streamed and ordinary JSON requests use this policy. The complete operation is bounded by the preset request timeout, covering retries and response transfer. Cancellation drops the in-flight request and backoff future via the request handler's cancellation selector.

The provider `response_received` observer records the completed or failed HTTP transfer and optional token usage. The application-level request lifecycle controls terminal success, errors and cancellation. No new persistent store or background worker is introduced.

## Checks

`tests/llm_streaming.rs` uses local HTTP servers to verify all three SSE dialects, fragmented SSE frames, streaming snapshots, 429/503 retry, truncated streams, cancellation and total deadline. `tests/openai_protocols.rs` verifies the existing non-streamed preset behavior.

Remaining acceptance work for closing #17: dedicated reasoning/tool channels where required by supported providers, configurable provider-specific retry classification, user-facing policy for provisional text on failure, and representative provider API E2E. A visual GUI smoke run on a real display is still required after merge. These checks should be performed after integrating the feature branch.
