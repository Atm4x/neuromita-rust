# GameLink protocol — runtime-origin command and push channel

Unity-клиент: [полная спецификация JSON-команд, push-событий и последовательностей интеграции](unity-game-link-integration.md).

GameLink is a newline-delimited JSON TCP protocol enabled by `ApplicationConfig::game_link`. Each request frame has `command_id` (UUID) and `command` (tag). `ServerLifecycle` remains an independent local administrative endpoint; both use one Application Core, Chat API, Timeline, SettingsOwner and generation backend.

GameLink binds to loopback. The default limits are 32 concurrent connections, 16 retained game sessions, 64 KiB input frames, 64 semantic events per batch, 4096 receipts per session, 64 queued outbound frames and a ten-second handshake deadline. A disconnected session remains resumable for 30 minutes by default. A session token acts as a bearer credential within this local transport.

## Session and delivery

- `hello` creates a session and returns `hello_ack` with `session_id` and `session_token`.
- `resume` submits both credentials. The new connection owns the session and the old socket closes. The server pushes `session_resumed`, a `session_state` containing current GameContext revision, PTT, playback and result acknowledgments, then current request snapshots.
- `ping` returns `pong`.
- The game connection receives `request_accepted`, `request_updated`, and terminal `request_completed`, `request_failed`, or `request_cancelled` automatically. Request snapshots carry `request_id`, a monotonically increasing `revision`, and the actual state. The client reconciles snapshots by identity and revision. No polling or subscription command is needed.
- `acknowledge_result` takes `request_id`, accepted `event_id`, `applied` and an optional `error` when `applied=false`. It records delivery/application status separately from successful generation. Its receipt is idempotent by command ID. On resume the `session_state.deliveries` list restores these acknowledgments.

The client applies structured result segments according to the game action contract. A successful result contains `text`, `event_id`, and validated `segments`. A voice synthesis failure preserves the already accepted text and event ID.

## Canonical event ingress

- `publish_message` takes `event_id`, `occurred_at_unix_ms`, `character` and `content` and publishes a player message addressed to that character.
- `publish_events` takes an ordered array with `event_id`, `kind`, `occurred_at_unix_ms`, `actor?`, `observers` and `payload`. Allowed kinds: `unity_event`, `game_started`, `game_action`, `game_ended`, `character_joined`, `character_left`, `player_inactive`, `proactive_opportunity`. The batch enters Timeline atomically through `SemanticBatch`.
- `reply(character,message_id)`, `react(character,event_id,instruction?)` and `initiate(character,instruction?)` start generation through ChatApi. They use `RequestSource::Game(session_id)` and choose `VoiceSettings.in_game`. `cancel(request_id)` is restricted to the current game session.

The producer supplies canonical event identities and observers. Core validates event semantics at Timeline ingress; the Unity-to-actor observer authorization policy remains a separate integration boundary.

## Transient runtime context

`update_context` accepts a complete `GameRuntimeContext` object with `revision`, `scene?`, `world` (JSON object) and `capabilities` (string list). Revisions must increase, and all fields have size limits. `context_applied` acknowledges acceptance. Retrying an identical command ID replays its receipt. Replaying an older revision with a different command ID is rejected.

GameLink copies the most recent immutable snapshot at **request admission**. It is carried in the request to generation as `CharacterContext.runtime_context`; the provider receives it as a separate `[LIVE GAME SNAPSHOT]` message containing data from this specific game session. Timeline knowledge and CharacterState are still captured through the canonical ContextCapture. Launcher requests have no GameRuntimeContext. The session's current context survives reconnect and is replaced by the next accepted revision.

## Voice, playback and ASR feedback

`get_game_settings` returns a scoped voice projection (`game_settings`) with revision and game voice enablement, volume and profile. Every SettingsOwner change pushes `game_settings_updated` to connected game sessions.

For a successful TTS response, `request_completed.audio_asset` contains the generating `request_id`, `format: wave`, `size_bytes` and a SHA-256 checksum. `read_audio_chunk(request_id,offset,length)` returns an `audio_chunk` with Base64 audio data, a byte offset and `next_offset`. Length is bounded to 16 KiB. The audio remains owned by the accepted request; bytes are read from the existing response, with no second asset store. A session can access only its own request's audio. Assets remain available while the request is retained by the game session.

- `report_playback(request_id,playback_id,phase)` accepts `started`, `completed` or `failed` for an owned completed audio response. It validates state transitions and returns `playback_accepted`.
- `report_ptt(capture_id,generation,active,cancelled?)` tracks Unity's microphone capture state and rejects stale generation and inconsistent cancellation. It returns `ptt_accepted`.
- Both feedback states are retained in the session and included in `session_state` after resume. Unity performs game-side microphone capture and suppresses local playback echo. The Rust desktop ASR pipeline has its own independent audio controller for launcher-origin speech.

## Capacity, retries and safety

Frames are bounded while reading, before the complete line is accumulated. A slow consumer whose outbound channel fills is disconnected and can resume. At receipt capacity, new side-effecting commands are rejected before admission. `command_id` collisions with different payloads are rejected. Read-only commands can be repeated. Request and audio lookup are session-scoped; a new TCP socket without the session credential cannot obtain the previous session's results.

The in-process session registry is intentionally transient. Restoring active network state following a full Core restart belongs to the deferred durable recovery work (#54). Transport-level `resume` restores state during the same application process lifetime.

World-event receipts have a separate lifetime. The selected Timeline branch identifies a `publish_events` batch by its ordered list of event IDs. After opening a new GameLink session or restarting Core with the same branch, a client can resend the same batch and get `events_published` with `replayed: true`. An event's observers form a set. Ordering and repeated observers do not affect identity. Reusing the event identities with different semantic content is rejected. This receipt covers canonical world facts; the GameLink request registry and acknowledgments of presented replies still follow the session lifetime above.

### Dialogue audience

`publish_message`, `reply`, `react` and `initiate` can carry an explicit `audience` with `targets`, `participants` and `seen_by`. The first two are sets of speaker identities (`"player"` or `{"character":"Kind"}`); `seen_by` is the set of character IDs that heard the utterance. Only `seen_by` grants knowledge. For `publish_message`, the selected respondent must be among its witnesses. A client omitting `audience` uses the single-character command semantics. Set order and repeated members do not change the command fingerprint.

Unity captures player witnesses at submission, using local hearing for group input and an explicit private audience for a named recipient. The response has its own witnesses around the actual speaker at dispatch. An explicit `@все` asks Unity to continue the group dialogue through its existing speaker order and turn limits. Each next speaker must have heard the last reply. Unity sends its canonical `event_id` as `message_id` in `reply`; Core does not append a second player message. A model segment can name a known `target`; this addressing does not extend who witnessed the response.

## Integration boundary

The Rust contract tests cover framing, reconnect, ownership, event publication, retry, admission-time GameContext, voice origin, playback/PTT feedback and chunked audio transfer. The Unity repository still needs its client implementation for this new protocol and real engine-to-engine acceptance tests. Physical gameplay execution remains Unity's responsibility; Unity applies the typed result segments and acknowledges acceptance.
