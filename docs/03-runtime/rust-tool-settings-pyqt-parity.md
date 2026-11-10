# Tool settings — PyQt parity (Rust + Slint)

Reference: Python source revision `5f3d061f523d9abefa6a264bb2953048a935fa65`, `src/ui/settings/model_interaction_settings.py` → **Поведение запросов / Инструменты (Tools)**. The PyQt `StructuredOutputPanel` also contains a compact `tool_call` block (`src/ui/chat/structured_panel.py`); it does **not** contain a separate tool activity dashboard.

## Included

The existing Slint `Settings → Модели → Поведение запросов → Инструменты` subsection now reflects and edits the persisted Rust `AppSettings.tools` through `SettingsPresenter.tools`, the desktop command path and SettingsUi signals. The controls correspond to the PyQt master tools switch, Calculator, Web Search, Web Reader, Memory Search, and the maximum tool chain depth (1–5). Values render from the authoritative SettingsSnapshot, not component-local defaults. The operation is validated and persisted by SettingsOwner; a rejected command reports the settings error and does not modify the state. In-flight LLM requests continue to use their admission-time settings snapshot.

Web Search and Web Reader are disabled in the UI until their existing Rust network policy is configured (HTTPS search endpoint / explicit reader host allowlist). This avoids presenting a toggle that would falsely imply unrestricted network access. No new network configuration UI is added: the PyQt reference does not have an equivalent SearXNG URL/allowlist editor.

Google Search is displayed in the original position but disabled because Rust has no Google CSE backend. Reminder is displayed but not wired as a tool toggle: Rust currently treats reminder as a structured effect, not a registered native tool. The PyQt-specific **Tool result message mode** (`both/system/user`) and Google API key/CSE ID are not represented as editable controls because their Rust behavior does not exist. Do not create standalone tool-monitor pages or fabricate executed-tool history.

The PyQt compact `tool_call` block within structured output is a separate message-renderer parity item, not an activity page; no such UI is implemented in this slice.

## Verification

- `ToolSettings` validates depth up to five; default remains two, without changing the global disabled-by-default security policy.
- Desktop settings command tests exercise enable, calculator, maximum depth, invalid depth and invalid web configuration.
- Slint SettingsUi binding tests assert command dispatch and local invalid-value rejection.
