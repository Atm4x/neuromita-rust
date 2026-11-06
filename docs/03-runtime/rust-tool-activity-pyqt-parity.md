# Tool-call presentation — PyQt parity slice

## Source of truth

Reference: Python Git revision `5f3d061f523d9abefa6a264bb2953048a935fa65`.

- `src/ui/chat/structured_panel.py`: `StructuredOutputPanel._build_brief_view` renders a violet `ToolCallBlock` (`⚙️ tool_call: {name}` and JSON arguments) inside the assistant response's structured panel.
- `src/ui/chat/message_renderer.py`: `SHOW_STRUCTURED_IN_GUI` defaults to `Выкл` / Off; `Кратко` / Brief renders structured cards; `JSON` renders the raw structured response.
- `src/ui/settings/debug_settings.py`: the three choices are `Выкл`, `Кратко`, `JSON`, persisted in UI settings.
- `src/ui/widgets/mita_status_widget.py`: ongoing operations reuse the existing chat status/typing surface; there is no separate tool activity dashboard. A terminal error is not overwritten by a background status.
- `src/controllers/model_controller.py`: `_handle_tool_call` emits the intermediate message with structured data, then `ON_TOOL_EXECUTING`, then executes the tool and continues the LLM turn. Historical persistence differs between schema and native tools.

## Rust implementation

- Existing Sandbox message bubble and status line display the **request-local** tool activity during `LLM → Tool → LLM`; neither a new page nor a monitoring history is introduced.
- `GenerationObserver::tool_started/tool_finished` carries a validated intermediate call into the existing `RequestRecord` watch snapshot. The displayed name and arguments are bounded; `tool_call` JSON is a bounded valid JSON **subset**, not the complete raw provider response.
- The existing structured-display setting (previously an inert `Выкл` field) is now backed by `InterfaceSettings.structured_display` through the canonical `SettingsOwner` and its Presenter. It defaults to Off and accepts Off, Brief and JSON, including old stored JSON where the field is absent. Brief shows the PyQt-style violet tool-call card. JSON shows the request-local tool call as a JSON object, **not** the full provider response (the full PyQt structured JSON viewer is outside this slice).
- The compact status line shows `Выполняет инструмент: {name}` while execution is in progress. Spoken text from an intermediate **schema** tool response can appear provisionally; the synthetic native tool message is not shown as a spoken response.
- Completed tool calls retain their card while a subsequent LLM provider turn is in progress; state changes, cancellation, errors, and accepted text discard the transient card. No intermediate effects, tool results, or raw reasoning are committed to Timeline as chat messages by this presentation change.
- No extra local UI store for tool outcomes is introduced. Request updates use the existing `RequestSnapshot.revision` watch notifications and immutable request identity.

## Verification / remaining parity gaps

- Unit tests: pending requests never expose tool calls; executing → continuation; bounded Unicode arguments; valid JSON on oversize; cancellation clears activity. Legacy settings default to Off and saved Brief/JSON values round-trip.
- Integration: the existing `tool_continuation` mock confirms the visible activity during the second LLM turn, final-only acceptance and terminal cleanup.
- This is not full Python chat-history parity: the Python schema tool path can persist the first assistant turn, whereas Rust currently keeps tool-call presentation transient to preserve Timeline semantics. Full `StructuredOutputPanel` rendering for all fields and raw JSON should be handled as a separate reviewed UI/Timeline feature, not silently inferred from a tool event.
