# Cycle66 — R7 loopback HTTP deadline acceptance evidence

Status: backend-only executable regression evidence, not complete R7 acceptance. The authoritative provider implementation remains `OpenAiProvider` and the deadline owner remains `ResponseGenerator`; no new HTTP provider, timer owner, retry policy or fallback was introduced.

## Verified protocol boundary

`tests/rag_http_deadline.rs` uses an actual loopback TCP listener and HTTP/1.1 with fully received JSON POST bodies. Each test starts a real `ApplicationLifecycle` using the production `OpenAiProvider` and a compatible `OpenAiTemplate`, without external keys or model endpoints. All fixture options explicitly disable HTTP retries, and all tests share one process-local serial guard because the lifecycle API has a global active instance.

1. **Stalled provider HTTP response**: the server accepts and reads the request but holds the reply beyond the admitted 420 ms preset deadline. The request terminates as `TextGenerationFailed(DeadlineExceeded)` with `ProviderTimeout` diagnostics. A fresh request receives a separate successful HTTP reply, while the delayed response to the previous socket is not accepted into the failed request.
2. **Native tool continuation**: the first HTTP response requests `calculator(2+3)` through the existing native tool protocol, the second HTTP POST contains the matching tool-call receipt and result `5`, and the server holds the second response. The request times out with `DeadlineExceeded` and `ProviderTimeout`, demonstrating the deadline is not reset when a provider resumes after the calculator tool.
3. **Transport disconnect**: an HTTP peer that reads the request and closes without a reply terminates as `TextGenerationFailed(Transport(_))` with diagnostic `ProviderTransport`, not `ProviderTimeout`.
4. **Explicit cancellation**: the request is cancelled while the actual HTTP server holds its response, resulting in `RequestState::Cancelled`, not provider timeout. A subsequent request succeeds, and the delayed old HTTP body cannot become an accepted response.

These are live local TCP/HTTP transactions, not injected future-only provider mocks. The fixture owns only test sockets; it never mutates user settings or documents. The current tests do **not** prove that the calculator execution itself is a slow or blocked future: they verify a slow **provider continuation after a completed native tool**. The Cycle64 production `timeout_at` already covers supported tool futures, but a deterministic blocked tool implementation is not tested here. No claim is made about live external HTTPS, DNS, uncooperative synchronous plugin threads, killing child processes or cross-process clocks.

## Remaining formal scope

The deadline is cooperative cancellation for async provider/tool futures; an OS worker performing a non-cooperative operation may continue after the caller drops the future. Document source attribution still relies on optimistic pre- and post-provider verification; no cross-process writer lock or authenticated provenance is present. The R6 Documents workflow lacks PDF/Office ingestion, semantic retrieval and UI parity. **R7 and R6 remain OPEN; formal 16 PASS / 4 PARTIAL / 6 OPEN.**
