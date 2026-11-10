# Agent A Cycle68 — SSE terminal completion independent of HTTP EOF

## Defect and production boundary

The existing `generation::http::send_sse` read the HTTP response body to network EOF even after a complete protocol terminal event (`finish_reason=stop`/`tool_calls` followed by `[DONE]` for Chat Completions, or `response.completed` for Responses). An HTTP/1.1 chunked, keep-alive peer may correctly leave the connection open after emitting a final SSE event. Waiting for EOF consumes the admitted provider deadline and turns a complete response into `DeadlineExceeded`; for native tools, the second turn never begins.

`send_sse` should end its read loop when the **whole current decoded chunk has been validated**, the SSE event decoder has no incomplete frame, and the relevant terminal evidence is present: Chat Completions requires both protocol finish and `[DONE]`; Responses requires `response.completed` with status `completed`. It must not early-finish on an incomplete frame, a bare `[DONE]` without terminal, a Chat terminal without `[DONE]`, or content following completion **within the same received chunk**. Connection errors, content limits and bad JSON remain failures. A `reqwest` timeout from request dispatch or body decoding is normalized to `GenerationError::DeadlineExceeded`, not misreported as `Transport`; actual non-timeout network failures remain `Transport`. This is not a generic permission to trust arbitrary partial model output.

## Executable acceptance

`tests/rag_sse_deadline.rs` uses production OpenAI SSE generation in `ApplicationLifecycle`, with actual local TCP/HTTP/1.1 chunked transfer encoding and a socket deliberately left open after the SSE terminal event:

1. Complete Chat Completions answer succeeds without waiting for HTTP EOF.
2. Complete Responses answer succeeds without waiting for HTTP EOF.
3. Chat native calculator response advances to its second HTTP POST while the **first** connection remains open, and the final answer succeeds while the **second** connection remains open.
4. Responses native calculator likewise advances and succeeds without either HTTP connection closing.
5. Chat terminal without `[DONE]` does **not** bypass the deadline; an open peer still expires with `DeadlineExceeded`, clearing provisional output, including when the lower-level reqwest request timeout fires first.
6. A peer emitting a valid terminal and `[DONE]` but then an additional data event in the **same decoded HTTP chunk** is rejected as an invalid stream.
7. Existing partial/stalled streams still fail by the admitted deadline and clear preview.

## Non-claims

These are real loopback HTTP tests, not live external HTTPS-provider proofs or an intentionally stalled async web-tool future. The calculator completes promptly. A Tokio deadline cancels cooperative async futures, not uncooperative synchronous native tasks or processes. Neither R6 nor R7 is yet fully accepted; formal acceptance remains 16 PASS / 4 PARTIAL / 6 OPEN.
