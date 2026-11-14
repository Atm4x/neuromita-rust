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


## Presenter-owned projections (phase 2)

The existing application lifecycle registers a single read-only \`PresenterProbe\`
before publishing the Core runtime. It reuses the actual ChatPresenter, AI runtime
presenter and ConnectionsPresenter watch-backed state. It does not create a second
timeline, an alternate GUI lifecycle or an event bus.

\`\`\`sh
neuromita-agent --addr 127.0.0.1:PORT inspect presenter chat
neuromita-agent --addr 127.0.0.1:PORT inspect presenter ai_runtime
neuromita-agent --addr 127.0.0.1:PORT inspect presenter connections
\`\`\`

All surfaces are typed, use schema_version 1 and expose structure only: chat
request IDs/revisions/phases, worker generations and presence flags, connection
counts. No prompts, model names, personal message text, raw errors or audio.
An unknown surface or misplaced parameter fails closed. Snapshots are read at
the time of the Core request, not a claim that a GUI widget rendered anything.
For issue #442 these are still \`core_ipc_only\` evidence, not Unity/GUI E2E.
