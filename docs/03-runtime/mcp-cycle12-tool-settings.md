# Tool settings GUI parity over MCP — Cycle 12B

The main agent can now use `presenter_tool_settings` (`set_option` and `set_depth`) through the **same SettingsPresenter.tools.state** as the GUI commands `SetToolOption` and `SetToolDepth`.

- `set_option`: `option = enabled | calculator | memory_search | web_search | web_reader`, `enabled = bool`. The Presenter recomputes `web.enabled` from the two related toggles and runs the canonical `ToolSettings::validate` before updating.
- `set_depth`: integer 1..5 (exact GUI admissible range). Invalid depth is rejected without mutation.
- Requires MCP `--allow-write`; this remains a local administration flag, not authentication of raw Core TCP.
- `presenter_settings_read(area=tools)` now exposes only nonsecret enablement flags and depth; web endpoints, credentials, host allowlists and arbitrary plugin payloads remain omitted.
- Persistence must be confirmed through `presenter_settings_read(area=persistence)` or `presenter_wait(target=settings)`; a successful setter is not disk durability evidence.

**GUI Command parity** after this change: 42/68 completely covered (61.8%), 12/68 partial (17.6%), 14/68 missing (20.6%). `Settings / Launcher`: 8/11 complete, 2 partial, 1 missing. This count is for the enum `runtime::Command` only, not all Slint actions or Presenter methods. Total MCP tool catalog: **37**.

Regressions check actual shared SettingsPresenter state, depth rejection without mutation, credential-free read projection, and typed MCP command validation. Main agent still performs the real Unity/GUI E2E acceptance separately.
