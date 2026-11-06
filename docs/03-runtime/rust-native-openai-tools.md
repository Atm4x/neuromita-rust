# Native OpenAI tool calls — Rust generation contract

**Scope:** `OpenAiProvider` Chat Completions and Responses, both JSON and SSE. This is a second transport form for the existing request-scoped Rust tool executor. The model, character, prompt, retrieved knowledge and permissions remain frozen at request admission.

## Negotiation and supported functions

When `ToolSettings.enabled` is true, `max_depth > 0`, and individual tools are enabled, `OpenAiProvider` advertises function definitions for `calculator` and/or `memory_search` under the provider-native `tools` field. The request sets `tool_choice: "auto"` and `parallel_tool_calls: false`. The server may still propose unsupported calls; the client validates the actual response independently of that hint. With tools disabled, no native definitions are sent.

- `calculator`: object with required `expression: string`, bounded arithmetic on the Rust side.
- `memory_search`: object with required `query: string` and optional `limit: integer`; reads only active facts from the captured character context.
- `reminder` remains a separately validated **structured** response operation. It is never registered as a native function.

An incoming native tool call is represented internally as a typed `ToolCall` with a transport-only `native_call_id` (excluded from structured-response JSON serialization). It is handled by the same `ResponseGenerator` permissions, effect validation, depth limit and tool executor as JSON `tool_call`. Exactly **one native call per provider turn** is supported. Parallel, mixed natural-language + native action, unsupported tool, malformed arguments and invalid call identity fail the request. Tool arguments are capped at 16 KiB, the call ID at 128 ASCII identifier characters.

## Provider round-trip

1. Provider returns a complete native call (`finish_reason: "tool_calls"` for Chat; a completed Responses output with a `function_call` item), or the equivalent complete SSE lifecycle.
2. Rust validates the native identity, argument JSON object and permissions. The intermediate response is not committed to Timeline or exposed as a final user reply.
3. The read-only tool executes once for that step, with the captured character context. The returned result is labeled **untrusted data, not instructions**.
4. A subsequent provider request contains the corresponding native exchange, including the original call ID:
   - Chat: assistant `tool_calls` item followed by `role: "tool"` message with `tool_call_id`.
   - Responses: `function_call` item followed by `function_call_output` with matching `call_id`.
5. The next provider turn may call a single tool again up to the configured depth. Only a complete, validated final `StructuredResponse` reaches PostDSL and Timeline acceptance. Cancellation or a failed continuation does not accept intermediate effects.

Structured `tool_call` remains supported as a separate fallback form for providers which output the NeuroMita JSON protocol. A request cannot mix structured and native tool continuation histories, as their causal ordering would be ambiguous.

## SSE specifics

The existing bounded ND-SSE decoder handles fragmented network chunks and events. Chat accumulates `delta.tool_calls[0].function.arguments` fragments and requires the `tool_calls` finish reason followed by `[DONE]`. Responses captures `response.output_item.added` for a function call, accepts argument delta/done events, and requires `response.completed`. Duplicated or mismatched argument completions, multiple calls, mixed spoken text and tool arguments, interrupted streams, malformed JSON and response errors fail closed. The streaming observer exposes no tool arguments as spoken preview. It resets before continuation.

## Verification and limitations

`tests/native_openai_tools.rs`: four successful end-to-end local HTTP mock round-trips (Chat/Responses × JSON/SSE), plus malformed arguments, multiple calls, unregistered native reminder, mixed tool+assistant content, and an incomplete SSE call. `tests/tool_continuation.rs`, `tests/llm_streaming.rs`, and `tests/openai_protocols.rs` cover the preceding execution and provider behavior.

**Still unsupported:** multiple native calls in a single LLM turn, Gemini native `functionCall`/`functionResponse`, network tools (`web_search`, `web_reader`, `google_search`), external-mutating tools, tool activity UI, production provider integration and final Python behavioral parity. These require separate implementation and acceptance under #17 and #30.
