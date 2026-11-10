# Rust Core: request-scoped LLM tools and continuation

**Status:** implementation slice for #30 (and provider streaming #17), based on `codex/llm-streaming-retry-20261106`. This document covers the schema tool path. Native OpenAI/Gemini `tool_calls` remain disabled until their full tool call/result transaction is implemented.

## Configuration and admission

`SettingsApi::edit::<ToolSettings>(...)` edits `AppSettings.tools`. The field uses a serde default for settings created before the tool schema was introduced. Defaults are `enabled=false`, `calculator=false`, `memory_search=true`, `max_depth=2`. Enabling the global switch and the specific tool is mandatory. Core accepts depth 0–5 (zero disables read-only tools); the PyQt-parity Slint control offers 1–5; The admitted settings snapshot is immutable for each request, so changes made while a request is running do not change its tool permissions.

`reminder` is an existing durable structured response effect with its own settings/acceptance contract and is not routed through the read-only continuation mechanism. Its presence in the structured schema does not grant permission to execute unrelated tools.

The provider prompt and request schema advertise only the read-only tool names enabled for that request, plus the existing `reminder` name. An enabled tool is requested through the structured response v3 root `tool_call`, with a normal nonempty `segments` array:

```json
{"segments":[{"text":"Проверю вычисление"}],"tool_call":{"name":"calculator","args":{"expression":"2 * (4 + 1)"}}}
```

`calculator` evaluates only numbers, unary signs, parentheses, `+`, `-`, `*`, `/`, `//`, `%`, `**` using a bounded Rust expression evaluator. It prohibits identifiers, calls and other Python syntax. Length ≤256 bytes, complexity/depth and finite numerical magnitude are bounded. No new dependencies or Python subprocess are required.

`memory_search` takes `{"query":"text","limit":5}` with query 1–256 bytes and limit 1–10. It lexically searches **only active facts in the same captured character context**, returns stable memory IDs and bounded data. It does not search other characters, refresh the underlying Timeline, or silently substitute vector retrieval. The output is read-only historical data, not an instruction.

## Execution and acceptance

1. `RequestHandler` captures settings, character view and preset at request admission, including the tool permissions.
2. `ResponseGenerator` performs the normal provider call. If the validated structured response requests a read-only tool, it checks permissions, arguments, bounds and remaining depth. Unknown or disabled names fail closed.
3. Intermediate responses may contain spoken segments and the tool call, but **cannot** contain memory, reminders, game commands, working-state or character-state effects. This prevents losing a side effect between LLM turns.
4. The tool computes a bounded result synchronously against the captured request context. Its call and labeled result are appended to an ephemeral provider transcript. The provider receives a subsequent request with the same captured character snapshot and preset. Streamed partial text from the previous provider turn is cleared before the continuation.
5. Only the final validated `StructuredResponse` is passed to PostDSL and proposed for Timeline acceptance. Failure, cancellation, depth exhaustion and malformed arguments yield no accepted intermediate message or effects.

The user-visible/request status may display an unaccepted spoken preview while a provider turn streams. It is reset before tool continuation. Tool results are not independently committed to Timeline; there is no durable tool-result cache across process restarts. These tools are read-only, so replay on a completely new generation does not duplicate external side effects.

## Tests and remaining work

`tests/tool_continuation.rs` checks calculator continuation, disabled tool, rejected intermediate effects, depth exhaustion, cancellation and character-scoped memory search. Unit tests check arithmetic limits and the dynamically advertised schema. `tests/llm_streaming.rs` verifies the existing SSE/preview lifecycle. Full real-provider/API E2E remains outstanding.

**Not yet implemented:** native Gemini function-call/result round trips, `web_search`, `google_search`, `web_reader`, typed tool UI status, web permission/URL policy, per-tool network deadlines, and explicit user approval for externally mutating actions. Incoming native Gemini function-call/executable-code parts fail closed. Native OpenAI Chat/Responses support was added in a subsequent iteration (below). These remaining scenarios belong to #30 and #17.

## Native OpenAI tool calls

The existing `calculator` and `memory_search` executor is now available through the native OpenAI Chat Completions / Responses tool-call protocols (JSON and SSE), with stable call-ID pairing and result continuation. See [rust-native-openai-tools.md](rust-native-openai-tools.md). Network tools and Gemini-native calls remain outside this slice.
