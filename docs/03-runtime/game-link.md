# GameLink TCP contract (2026-11-05)

GameLink is enabled by `ApplicationConfig::game_link` and uses newline-delimited JSON. Every incoming frame has a `command_id` UUID and a `command` discriminator. It shares the canonical Core/Timeline and request system with the local launcher. Its requests are marked `RequestSource::Game(session_id)` and use the `in_game` voice policy; LocalControl requests use launcher voice policy.

## Session lifecycle

- `hello` creates a game session and returns `hello_ack` with `session_id` and `session_token`.
- `resume` requires both identifiers. The new connection takes ownership of the session, closes the previous connection and returns `session_resumed`, followed by current request snapshots.
- `ping` receives `pong`.
- Session credentials last for the application process lifetime, subject to `disconnected_session_ttl` (default: 30 minutes) and bounded session capacity (default: 16). The token is a bearer credential; production network exposure requires an explicit network security decision.

## Timeline ingress

- `publish_message`: `event_id`, `occurred_at_unix_ms`, `character`, `content`. Publishes the player's message for one character and returns `published` with the canonical event ID.
- `publish_events`: an ordered array of semantic world events. Each event requires `event_id`, `kind`, `occurred_at_unix_ms`, `observers`, and `payload`; `actor` is optional. Supported kinds: `unity_event`, `game_started`, `game_action`, `game_ended`, `character_joined`, `character_left`, `player_inactive`, `proactive_opportunity`. A successful atomic append returns `events_published` and the IDs of all facts.
- Every canonical event has a stable client-provided ID. Retrying an identical `command_id` in a session returns the saved receipt; reusing an ID with modified contents is rejected. Timeline validates duplicate event identity and atomicity.

The game client is responsible for assigning character witnesses according to current scene and dialogue state. `observers` are accepted for the game session at this stage; authorization and scene-witness enforcement are a remaining GameLink integration requirement.

## Generation

- `reply`: `character`, `message_id` (known message event).
- `react`: `character`, `event_id` (known event), optional `instruction`.
- `initiate`: `character`, optional `instruction`.
- `cancel`: `request_id`, restricted to requests owned by the game session.

After successful admission, the server sends `request_accepted`, followed by push `request_updated` and a terminal `request_completed`, `request_failed`, or `request_cancelled`. Every snapshot contains `request_id` and `revision`. No polling or explicit subscribe command is required. A reconnect resynchronizes current snapshots; session ownership survives socket replacement.

## Bounds and delivery

The transport enforces `max_command_bytes` while receiving a line, before allocating the complete frame. `outbound_capacity` is bounded; a client which cannot consume push updates is disconnected and may resume. Session receipts are retained for the session lifetime up to `max_requests_per_session` (default: 4096); at capacity new side-effecting operations are rejected before admission, preserving command deduplication.

## Remaining integration work

- Runtime world snapshots and capabilities, bound into `ContextCapture` per game request.
- Game speech/PTT/playback reports and feedback into ASR arbitration.
- Typed game action delivery and audio asset transfer/acknowledgment with Unity E2E.
- Session authorization for environments outside a trusted local process, and broader fault-injection coverage.
