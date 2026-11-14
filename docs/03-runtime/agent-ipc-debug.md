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
