# MCP / GUI Remote Voice administration (Cycle 15)

## Runtime owner

`RemoteVoiceAdmin` owns one validated Fish Audio catalog, atomic persisted writes, a monotonically increasing revision, a cancellable preview and a `tokio::watch` status channel. The desktop GUI, its existing `RemoteVoiceAction` callbacks, the MCP/LocalControl server and production `RemoteVoiceRouter` share this instance created by `ApplicationLifecycle`. Production TTS resolves the active voice against this same snapshot, rather than separately reading the JSON file. `RemoteVoiceStore` retains its validated, owner-only file storage and atomic rename. No second HTTP client is made into a worker owner.

## MCP operations

`inspect_presenter({"surface":"remote_voice"})` returns `state.revision`, `state.active_id`, sanitized catalog presets (name/model/speed/voice IDs and `credential_present`), `state.phase` and `state.busy`; it never returns credentials, input text, synthesized PCM, playback paths or raw service errors.

`presenter_remote_voice` supports:

- `add`: `{"action":"add","expected_revision":0}`
- `select`: `{"action":"select","id":"<saved ID>","expected_revision":1}`
- `select_with_save`: `{"action":"select_with_save","id":"<target ID>","expected_revision":2,"preset":{"id":"<target ID>","name":"Mita"}}` (atomic draft save + selection, same GUI contract)
- `save`: `{"action":"save","expected_revision":2,"preset":{"id":"<saved ID>","name":"Mita","model":"s2.1-pro-free","speed_percent":120,"default_voice_id":"<32 hex characters>"}}`
- `delete`: `{"action":"delete","expected_revision":3}` (deletes the active preset except the last)
- `preview`: `{"action":"preview","expected_revision":4,"preset":{"id":"<active ID>"},"text":"Test","character":"Crazy"}`
- `cancel`: `{"action":"cancel"}`; cancellation is idempotent when no preview is active.

The preset patch is partial and preserves unspecified credentials. `api_key` is intentionally omitted from normal read responses; `api_key` can only be changed when MCP is started with `--allow-content --allow-write` and the **Core** host explicitly sets `NEUROMITA_ENABLE_LOCAL_REMOTE_VOICE_SECRETS=1`. This opt-in does not authenticate the underlying loopback TCP transport. Use only on a trusted, isolated machine. Other mutations require `--allow-write`; preview additionally requires `--allow-content`, because its text is sent to Fish Audio. A pending preview requires a validated active preset and an absolute local FFplay executable from the regular desktop/host config (`NEUROMITA_FFPLAY` or `AIData/bin/ffplay`).

`presenter_wait({"target":"remote_voice","after_revision":4,"timeout_ms":10000})` observes the **same** status watcher used by GUI. Each edit, preview start, completion or cancellation changes the revision. An editing action must include the current revision and fails closed on stale data, preventing concurrent GUI/MCP overwrites. Preview never replays a previous preset when the incoming draft is stale or invalid.

## Validation

Check separately:

1. `cargo check --locked --offline --features mcp --lib --bin neuromita-agent-mcp --test local_control`
2. `cargo check --locked --offline --features 'gui mcp' --bin neuromita-gui`
3. MCP tool mapping: typed actions and invalid parameter rejection
4. `RemoteVoiceAdmin` unit tests: version fence, atomic catalog persistence, no secret in state/Debug, cancellation
5. TCP fixture: owner catalog changes, GUI/MCP shared revision, stale update rejection and watch
6. Real Fish Audio preview needs valid user credentials, networking, actual audio playback and GUI observation; **not proven by fixture tests**.

## GUI parity scope

This adds one previously missing GUI `Command::RemoteVoice` surface. For the 68-command baseline it provisionally changes 49 complete / 11 partial / 8 missing to **50 complete / 11 partial / 7 missing**, subject to compile and tests. This metric does not cover every public Presenter method. Remaining scopes include Knowledge Graph maintenance, selection and prompt catalog UX parity, chat attachment and other incomplete operations.
