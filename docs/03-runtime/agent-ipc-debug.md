# LocalControl agent IPC debug client — issue #442

`neuromita-agent` is a scriptable **second client of the existing Core LocalControl**, not a separate authoritative server, browser automation, GUI state simulator, event bus, or bypass for Timeline and request admission. It uses the same loopback-only bound TCP endpoint as the product, with JSONL commands and response `command_id` correlation.

## Boundary and safety

- Requires a **running Core with LocalControl enabled**. It never starts Core, Unity, TTS, or an AI worker by impersonation. A successful IPC assertion is classified `core_ipc_only`, **not** a full-stack E2E PASS for #442.
- All Core-mutating commands (`publish_message`, `reply`, `react`, `initiate`, `cancel`, `set_volume`, and any unknown future command) require `--allow-write`. Batch commands are validated **before** connecting, and the entire script fails closed if any mutation lacks this option.
- Only loopback TCP addresses are accepted by both client and Core. No network listener, shell/eval, arbitrary function dispatcher, filesystem or SQL manipulation, secret export, remote admin control, or branch rebinding is introduced.
- Unsolicited settings payloads are never printed. Command responses and unsolicited request events are redacted by default (`settings`, `text`, `content`, `audio`, `error`, etc.). `--include-private` permits local explicit review; keep such logs out of public CI/issues. Private diagnostics remain bound by existing `DiagnosticScope::owner(character, client_id)`.
- `debug_inspect` is **read-only**. `lifecycle` uses Core runtime state; `timeline` uses the canonical `SessionTimeline` anchor/selection; `character` obtains counts from the same observer-scoped `CharacterHandle::get_context` as the presenter (no hidden messages); `requests` lists only this TCP connection's tracked receipts; `capabilities` lists executable methods and their read/write boundary. None of these claims inference readiness, durable distributed ACK, or Unity visibility.
- A `command_id` is generated if omitted. Reusing a fixed UUID in a `batch` preserves existing same-connection replay/conflict semantics. A different connection **does not share receipt ownership**. `--wait-terminal` explicitly waits for a terminal request outcome, separate from `accepted`. It **fails closed** for cancelled/error outcomes by default; provide `--expect-terminal '{"state":"cancelled"}'` (or JSONL field `expect_terminal`) when deliberately testing rejection/cancellation. The total response-and-terminal deadline is shared across all incoming frames: unsolicited traffic cannot keep the CLI alive indefinitely.

## Commands

Use the actual LocalControl port from Launcher/Core configuration; the example uses `127.0.0.1:8765` as a placeholder, not a claimed default runtime port.

```bash
neuromita-agent --addr 127.0.0.1:8765 inspect capabilities
neuromita-agent --addr 127.0.0.1:8765 inspect lifecycle
neuromita-agent --addr 127.0.0.1:8765 inspect timeline --expect '{"snapshot":{"selection_matches_head":true}}'
neuromita-agent --addr 127.0.0.1:8765 inspect character Crazy
neuromita-agent --addr 127.0.0.1:8765 inspect requests
neuromita-agent --addr 127.0.0.1:8765 call '{"command":"ping"}' --expect '{"type":"pong"}'
neuromita-agent --addr 127.0.0.1:8765 batch tests/agent_ipc_smoke.jsonl
neuromita-agent --allow-write --timeout-ms 90000 --expect-terminal '{"state":"finished","outcome":"success"}' call '{"command":"initiate","character":"Crazy"}'
```

For a controlled real Core action, first validate fixture/disposable data roots, then use a script with explicit command IDs and `--allow-write`. Pair `publish_message` with `reply`/`react`/`initiate` and use `--wait-terminal --timeout-ms 90000` to distinguish receipt from actual generation completion. The client returns a nonzero exit on malformed commands, timeout, assertion failure or unexpected Core error. `--timeout-ms` bounds the entire command (send, acknowledgment and terminal result), not merely individual received frames. JSONL commands may carry `expect`, `wait_terminal`, or `expect_terminal` client-side assertion fields, which are removed before sending to Core. For a batch mixing read and write actions, set `wait_terminal` per generation command rather than globally. An expected non-success terminal state must be asserted explicitly, and even then **it is evidence of failure/cancellation, never success of a user action**.

Batch example (`jsonl`):

```jsonl
{"command":"ping","expect":{"type":"pong"}}
{"command":"server_status","expect":{"type":"server_status","running":true}}
{"command":"debug_inspect","scope":"timeline","expect":{"type":"debug_inspect","snapshot":{"selection_matches_head":true}}}
{"command":"debug_inspect","scope":"requests","expect":{"type":"debug_inspect","snapshot":{"ownership":"connection_local_receipts_only"}}}
```

## Mapping to acceptance issue #442

| Gate | What agent IPC can prove | What it **cannot** prove by itself |
|---|---|---|
| E2E-01, 09 | Loopback handshake, runtime status, correlated errors, reconnect by separate client invocation | Unity/Launcher/GUI user-visible readiness and real network failures |
| E2E-02, 03, 05 | Canonical branch/head metadata, per-character *visible counts*, intentional Core-ingress events with `--allow-write` | Actual Unity world/witness calculations, seen-by correctness or private knowledge end-to-end |
| E2E-04, 07, 08 | Authoritative reply/react/initiate/cancel, same-connection UUID replay and conflict, request terminal frame and owner-scoped diagnostics | Cross-process durable idempotency, real AI cancellation and in-game outcome |
| E2E-06, 10, 11 | Request status + Timeline selection before/after restart, stable JSONL traces | GUI history rendering, physical restart persistence without a restart step |
| E2E-12…15 | Precise correlation/errors of supported LocalControl commands | Device/model inference readiness, asset validation, GUI snapshots, update rollback |

For true #442 PASS, add **real Unity and GUI evidence and exact SHAs**. Do not close #442 or #95 based on an IPC smoke pass.


## Presenter IPC: read-only phase 2 (`presenter_inspect`)

This is a **typed projection of the already-running presenters**. `ApplicationLifecycle`
passes a clone of the existing `ChatPresenter` watch state and `RuntimeStatePresenter`
handles into LocalControl. It does not construct a second presenter, own a new
Timeline, or replay messages. The server reports `presenter_unavailable` rather
than fabricating a snapshot if no application presenter is attached.

```bash
neuromita-agent --addr 127.0.0.1:8765 presenter chat Crazy
neuromita-agent --addr 127.0.0.1:8765 presenter ai_runtime
neuromita-agent --addr 127.0.0.1:8765 presenter connections
neuromita-agent --addr 127.0.0.1:8765 call '{"command":"presenter_inspect","surface":"chat","character":"Crazy"}'
```

The server uses a **closed `PresenterSurface` enum** (`chat`, `ai_runtime`,
`connections`). `chat` requires an explicit character, and reports only
presenter revision, pending/processing/terminal request *counts*, closed flag
and presence of discovery errors. `ai_runtime` returns the five known service
phases, generation correlation and readiness/error **booleans**, not plugin IDs,
model paths, credentials or provider text. `connections` projects the existing
`ServerHandle` consumed by `ConnectionsPresenter`, omitting client addresses
and last error contents. Each reply has a `command_id`, `schema_version: 1`
and `evidence_level: core_ipc_only`.

These views do **not** prove GUI rendering or live Unity witnesses. The `revision`
is an observable presentation revision, not a durable Timeline receipt.
No privileged write-methods or server debug token are introduced: any
future deep-debug mutation MUST have explicit server-side authorization,
separate from client `--allow-write`. Loopback is not authentication; do not
expose this LocalControl port to untrusted local processes.

## Optional loopback HTTP gateway

Start \`neuromita-agent-http\` explicitly; it is **not** launched by the
GUI or Core automatically. Set \`NEUROMITA_AGENT_HTTP_TOKEN\` in the
gateway process environment to a random ASCII secret of at least 32 bytes.
It never accepts non-loopback bindings or Core addresses, rejects foreign
Host headers, does not enable CORS, bounds headers/bodies/frames/timeouts,
and requires \`Authorization: Bearer <secret>\` on **every** request.

\`\`\`sh
NEUROMITA_AGENT_HTTP_TOKEN="$TOKEN" neuromita-agent-http \
  --listen 127.0.0.1:8766 --core 127.0.0.1:8765
curl --fail-with-body -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8766/v1/presenter/chat/Crazy
curl --fail-with-body -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:8766/v1/timeline
\`\`\`

Read-only GET endpoints:
\`/v1/ping\`, \`/v1/methods\`, \`/v1/server\`,
\`/v1/capabilities\`, \`/v1/lifecycle\`, \`/v1/timeline\`,
\`/v1/characters\`, \`/v1/character/{id}\`, \`/v1/requests\`,
\`/v1/presenter/{chat|ai_runtime|connections}\`.

POST \`/v1/commands\` accepts the canonical Core JSON command schema.
Only \`ping\`, \`methods\`, \`server_status\` and \`debug_inspect\` are
admitted by default. Launching the **gateway** with \`--allow-write\`
additionally admits \`publish_message\`, \`reply\`, \`react\`,
\`initiate\` and \`set_volume\`. Each POST is an independent Core
connection; this HTTP phase supports command ACK, not sessionful
\`cancel\` or terminal result subscriptions. For those, use the
persistent JSONL \`neuromita-agent\` batch client.

The gateway authenticates **its own** requests, but the underlying Core
LocalControl loopback socket does not yet have server-side authentication.
Do not bind it on a remote interface or claim this is a secure local-user
boundary. Use an isolated disposable data root for mutation acceptance.

## MCP over stdio

Build the optional MCP executable with `cargo build --locked --offline --features mcp --bin neuromita-agent-mcp`.
Configure your MCP client to start it over stdio. Supply the actual LocalControl
loopback address of the running NeuroMita instance in `--core`.

```json
{
  "mcpServers": {
    "neuromita": {
      "command": "C:/path/to/neuromita-agent-mcp.exe",
      "args": ["--core", "127.0.0.1:8765"]
    }
  }
}
```

`inspect_core` calls `debug_inspect`. `inspect_presenter` calls
`presenter_inspect`; the `chat` surface requires a canonical `character` ID.
The inspection tools are read-only and return the correlated LocalControl frame as
structured MCP output. The executable uses the Rust MCP SDK and shares the TCP
client with the CLI and HTTP gateway.

### Targeted issue #442 probes (Core-only evidence)

`GET /v1/request/{request_uuid}` returns a privacy-safe bounded ChatPresenter
state for that request ID, or `observed=false` if absent from the presenter
retention window. `GET /v1/character/{character}/event/{event_uuid}` checks
one observer's projection at the returned branch/head sequence, without
exposing any payload or proving that a hidden event does not exist.

`GET /v1/presenter/speech` and `/v1/presenter/releases` expose only phase,
error-presence and structural counters from lifecycle-owned presenters. All
these probes are read-only and remain `core_ipc_only`, not Unity E2E PASS.

## MCP acceptance orchestration: one connection + active preset

MCP is **read-only by default**. Start `neuromita-agent-mcp --core
127.0.0.1:8765 --allow-write --timeout-ms 90000` only with a trusted local
agent and a disposable Core/data root. The opt-in flag gates all mutations,
including `select_active_preset` and mutating steps inside `run_scenario`.
This is an *MCP-process permission*, not an authentication mechanism for
LocalControl, whose loopback endpoint is still available to other local apps.

Additional typed tools:

- `inspect_onboarding` returns Core settings revision, persisted revision and
  Guide level/completion/page flags. Reading after a **real restart** can help
  verify persistence, but cannot prove the GUI displayed the intended screen.

- `inspect_presets` returns the live active preset ID, settings revision and
  persisted preset **names and IDs only**. Runtime preset ID 0 is indicated
  separately; no credentials, endpoints, models or provider settings are
  exported. `select_active_preset` accepts `preset_id` and optional
  `expected_revision` to reject concurrent settings changes; the ID must exist
  in the real catalog without materializing credentials. Selection changes
  global Core settings, not one request's ephemeral override. A queued
  request resolves its selected preset when generation begins.
- `run_scenario` accepts 1–32 JSONL-equivalent `steps`, using **one** LocalControl
  TCP connection throughout so `cancel` can act on that connection's request
  receipts. Each step can set `command_id`, `wait_terminal`, `expect` (subset),
  `expect_terminal` (subset). `$last_request_id` is supported in `request_id`,
  and `$last_event_id` in `message_id` or `event_id`; a missing preceding source
  fails explicitly. Results are sanitized and contain each command's ACK,
  optional actual terminal status and a failing step index when an assertion or
  Core command error occurs. A terminal success is never inferred from ACK.
- The typed `inspect_event_visibility` and `inspect_request` probes continue to
  use the real Timeline and ChatPresenter by ID. `inspect_presenter` covers
  speech/AI-runtime/release lifecycle status without microphone data or logs.

Example MCP `run_scenario` arguments for a **single actual generation**:

```json
{
  "steps": [
    {"command": "debug_inspect", "scope": "presets"},
    {"command": "set_active_preset", "preset_id": 0},
    {"command": "initiate", "character": "Crazy", "wait_terminal": true,
      "expect_terminal": {"state": "finished", "outcome": "success"}},
    {"command": "debug_inspect", "scope": "character", "character": "Crazy",
      "event_id": "$last_event_id", "expect": {"snapshot": {"event_visible": true}}}
  ]
}
```

A duplicate/cancel probe can use an explicit identical `command_id` for both
submission attempts and `{"command":"cancel","request_id":"$last_request_id"}`
between them. The second receipt must be `replayed=true` and retain its
original request ID. For a cancellation test, do not enable terminal success
expectations on the first submission. The `cancel` reply establishes only Core
receipt ownership/cancellation; it is **not** an AI-provider or Unity delivery
ACK. Error and text fields are redacted even with an opt-in writer.

The MCP acceptance boundary remains limited to Core/Presenter. Real Unity
GameLink event delivery, mic/device capture, model installation, audible TTS,
Slint/Guide navigation, persistence **after a physical restart** and automatic
reconnect require independent actors and explicit evidence. A local inspector
must not mark #442 full-stack gates PASS for these operations.

## GameLink delivery and reconnect probes (`inspect_gamelink`)

The optional ninth MCP tool, `inspect_gamelink`, reads the **existing GameLink
protocol session owner**, not the LocalControl connection summary or a second
cache. The equivalent read-only LocalControl command and authenticated HTTP
routes are:

```json
{"command":"game_link_inspect"}
{"command":"game_link_inspect","session_id":"<session UUID>"}
{"command":"game_link_inspect","session_id":"<session UUID>","request_id":"<request UUID>"}
```

```text
GET /v1/gamelink
GET /v1/gamelink/<session UUID>/request/<request UUID>
```

The MCP tool takes optional `session_id` and `request_id` UUID strings.
`request_id` without `session_id` is rejected. With neither ID, only counts
of existing/connected sessions are returned; individual session identities are
**not** enumerated. With a known session ID, it reports connected state,
context revision, number of recorded command receipts, delivery ACK count,
playback report count, PTT active flag, and **resume_count**. With a known
request ID, the authoritative request state, terminal flag, matching event ID,
GameLink accepted `acknowledge_result` (`applied` and `error_present` only), and
playback phase become visible. No session credentials, private text, audio,
raw Unity error messages, or other sessions' request IDs are returned.

A GameLink ACK proves only that the GameLink protocol owner accepted a matching
ACK sent by its connected client. An `applied: true` claim is supplied by the
client and **does not independently prove Unity scene mutation, rendered speech,
or actual playback**. `resume_count` increments on successful ownership transfer
and does not reset on reconnect; a stable delivery ACK count and replayed request
identity can then support a duplicate-prevention acceptance trace. The data is
explicitly classified `core_gamelink_tcp_only`, not full-stack E2E.

Integration verification uses the real TCP GameLink lifecycle: initiate a
request, finish generation, retrieve WAV bytes, send playback start/completed,
acknowledge its event, inspect by session/request ID through LocalControl,
reconnect with the real session credential, then inspect again. Unknown sessions
and request-without-session cases fail closed without disclosing payloads.
The Unity-side fixture/agent remains responsible for proving who sent that ACK.

## Verifying that a new Core instance actually started

`inspect_core(scope="lifecycle")` now includes `core_instance_id`: a new random
identity assigned by the authoritative `RuntimeLease` on each successful Core
start. When testing memory or Guide persistence, capture this identity **before**
shutdown and **after** the new Core starts. Fail the acceptance sequence if the
IDs are equal; then independently requery the same EventId through
`inspect_event_visibility`, or the Guide values and `persisted_revision` through
`inspect_onboarding`. This avoids incorrectly treating two polls of one still-
running Core as restart evidence. It is an **application-instance identity**,
not the OS process PID, nor proof that Unity or the GUI restarted. Neither
memory durability nor settings durability is inferred solely from changed IDs.

## MCP Presenter Control and separate Core Debug boundary

The MCP adapter is for **the main acceptance agent to operate the running application**. It does not execute Unity, prove rendering, or certify an E2E scenario on its own. The primary path for ordinary acceptance is the *same* `ChatPresenter`, `ConversationPresenter`, `AudioInputPresenter`, `VoiceSettingsPresenter`, `OnboardingSettingsPresenter`, `ReleasesPresenter`, and `RuntimeStatePresenter` used by the desktop host. A cloned presenter handle shares its authoritative owner and watch state; no replacement presenter or event store is constructed.

Build with Rust 1.100.0 and feature `mcp`. Connect the main agent to:

```text
neuromita-agent-mcp --core 127.0.0.1:<actual-port> --allow-write --allow-content --timeout-ms 90000
```

`--allow-write` authorizes application actions through this **MCP process**; leave it off for a read-only agent. `--allow-content` separately permits request text in `presenter_conversation`. Low-level Core access is off by default; use `--core-debug` for `core_debug_command`, `inspect_core`, `inspect_event_visibility`, `inspect_request`, `inspect_gamelink`, and Core JSONL steps in `run_scenario`. Mutating Core-debug calls also need `--allow-write`. These are *adapter-level* permissions, **not TCP server authentication**: the underlying LocalControl loopback trust boundary remains a separate security task. Use a disposable test profile when enabling writes or content export.

Presenter tools added to the existing catalog:

| MCP tool | Actual owner | Supported operations |
|---|---|---|
| `presenter_chat` | `ChatPresenter` | `reply(character,text)`, `react(character,event_id,instruction)`, `initiate(character,instruction)`, `cancel(request_id)` |
| `presenter_conversation` | `ConversationPresenter` | Read a branch/selection-consistent visible-message page (last 1–100), `EventId`, `RequestId`, role, sequence, active preset; optional content (max 4,000 chars/message) |
| `presenter_audio_input_devices` | `AudioInputPresenter` | List backend and device identity for a real microphone test |
| `presenter_audio_input` | `AudioInputPresenter` | `start_test(backend,identity)`, `stop_test()`; levels/error flag via `inspect_presenter(surface=audio_input)` |
| `presenter_guide` | `OnboardingSettingsPresenter` | `level`, `page`, `completed` via existing validated BoundProperties |
| `presenter_voice` | `VoiceSettingsPresenter` | Set volume 0–100 |
| `presenter_presets` | `PresetsPresenter` and active preset BoundProperty | Create unconfigured, select, move (-1/+1), delete; read names/IDs via `inspect_presenter(surface=presets)` |
| `presenter_refresh` | `ReleasesPresenter` / `RuntimeStatePresenter` | Queue release refresh or refresh actual AI runtime status |
| `core_debug_command` | Typed Core LocalControl | Explicit allowlisted raw Core commands; `--core-debug` required |

Extended `inspect_presenter` with surfaces `audio_input`, `voice`, and `guide`. `presenter_conversation` returns structural metadata by default, and only exports content under `--allow-content`. Use a tool's returned `request_id` and `EventId` to correlate with the real UI, GameLink and Timeline. The `run_scenario` tool supports `presenter_action` steps (including `wait_terminal`), same-connection deduplication and nested `$last_request_id` / `$last_event_id` references; low-level Core steps require `--core-debug`.

**Examples for the main agent:**

```json
{"tool":"presenter_chat","arguments":{"action":"reply","character":"Crazy","text":"Hello"}}
{"tool":"presenter_conversation","arguments":{"character":"Crazy","limit":20,"include_content":true}}
{"tool":"presenter_guide","arguments":{"field":"level","value":"advanced"}}
{"tool":"presenter_audio_input_devices","arguments":{}}
{"tool":"presenter_audio_input","arguments":{"action":"start_test","backend":"<returned-backend>","identity":"<returned-id>"}}
{"tool":"inspect_presenter","arguments":{"surface":"audio_input"}}
{"tool":"core_debug_command","arguments":{"method":"debug_inspect","params":{"scope":"timeline"}}}
```

Do not mark a full-stack gate PASS solely because a Presenter action returned success: verify the actual displayed UI/Unity event/device result using the main agent's independent observation. Direct Core commands intentionally **do not** simulate UI clicks. Pending wider coverage: configured Preset connection editing, detailed Character/Memory Presenter operations, model installation, real ASR capture/transcription, and UI-specific navigation/actions not yet exposed by their current presenter APIs.
