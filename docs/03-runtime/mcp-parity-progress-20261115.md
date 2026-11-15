# MCP ↔ GUI parity progress — 2026-11-15 (Cycle 22)

## Scope and counting

The denominator is the **68 `Command` variants** in `src/bin/neuromita-gui/runtime.rs`, originally enumerated in `mcp-gui-command-parity-audit.md`. A command is counted as fully covered when an MCP/LocalControl path provides the same functional Core/Presenter operation with equivalent validation or an equivalent read-only refresh. This is **not** a measure of all Slint callbacks, full-stack E2E, or transport security.

| State | Commands | Share |
|---|---:|---:|
| Fully covered | **66** | **97.1%** |
| Partially covered | **2** | **2.9%** |
| No coverage | **0** | **0%** |

History of source-based counts: Cycle 15: 50 full / 11 partial / 7 missing; Cycle 16: 59 full / 9 partial / 0 missing; Cycle 17: 62 full / 6 partial; Cycle 18: 63 full / 5 partial; Cycle 19: diagnostics safe subset (still partial); Cycle 20: prompt catalog refresh, 64 full / 4 partial; Cycle 21: GUI debug selection projection, 65 full / 3 partial; Cycle 22: live conversation refresh, 66 full / 2 partial.

## Cycle 20: RefreshPromptCatalog

A new read-only MCP tool `presenter_prompt_catalog(limit=1..200)` asks the *same* `PromptFolderPresenter.inventory()` that the desktop GUI uses. Both scan the live `PromptCatalog` filesystem; the application does not create a second catalog, duplicate cached index, or separate state owner. The desktop's existing `RefreshPromptCatalog` event still triggers the normal administration snapshot re-render. The inventory counts per-character directory-read errors without exposing their raw text. MCP returns bounded prompt-set metadata; no template source or absolute host filesystem path. The MCP endpoint requires `--allow-content` because names and metadata are user-authored. Existing LocalControl transport is unauthenticated and needs OS-user protection before deployment on untrusted hosts.

### Verification

- Rust 1.100.0, locked offline, `cargo check --features mcp --bin neuromita-agent-mcp --test local_control`: PASS.
- `cargo test --features mcp --test local_control -- --test-threads=1`: **55/55 PASS**, including an actual Core TCP check for a newly created and removed prompt set during the lifetime of the same Core, bound enforcement, redaction of source text/path, and stable inventory ordering.
- `cargo test --features mcp --bin neuromita-agent-mcp`: **8/8 PASS**; invalid MCP limit and unknown arguments rejected.
- `cargo fmt --package neuromita -- --check` and `git diff --check`: PASS.
- `cargo check --locked --offline --features "gui mcp" --bin neuromita-gui`: **PASS** (four unrelated compiler warnings). This checks compilation, not Slint rendering or Unity E2E.

## Cycle 21: DebugSelect debug projection

The desktop DebugSelect continues to own its per-window selected character, but its debug output now goes through the same stateless `project_character_debug` computation as MCP. The MCP `presenter_character_debug` tool can request exactly the same current character projection without impersonating the GUI window or mutating selection state. By default only the field count is returned; exposing labels and values requires both MCP `--allow-content` and Core-host `NEUROMITA_ENABLE_LOCAL_CHARACTER_DEBUG=1`, since the existing loopback LocalControl port is not OS-user authenticated. The projection reuses the active character settings, PromptCatalog and the existing credential-filtering logic.

Verified: **56/56 LocalControl** and **8/8 MCP** tests on Rust 1.100.0; GUI `cargo check --locked --offline --features "gui mcp" --bin neuromita-gui` **PASS** on memory-limited retry (the first attempt was OOM-killed, not a compiler diagnostic). Full GUI rendering remains untested.

## Cycle 22: Refresh conversation snapshot

The desktop `Refresh` handler re-enters its regular update loop and reads `ConversationPresenter::snapshot(character)` again, publishing the resulting `Update::Snapshot` for its own window. The MCP `presenter_refresh(surface=conversation, character=...)` now performs the exact same fresh Presenter read and returns a sanitized refresh receipt: Timeline branch/generation, visible message count, chat and settings revisions, selected preset and observed request count. This does not mutate the GUI selection and never claims a GUI window was repainted. There is no duplicated snapshot store.

Local acceptance checks that a real completed chat request changes the subsequent refresh revision; the receipt matches a direct GUI Presenter snapshot, never includes user input/output/provider credentials, and stays character-scoped.

## Two remaining partial commands

| GUI Command | Remaining acceptance condition |
|---|---|
| `Diagnostics` | GUI exports raw full diagnostic trace; MCP deliberately provides only bounded redacted state. A full export needs a distinct privileged and auditable access policy, not a silent content downgrade. |
| `Select` | GUI selection owns per-window character and ASR routing with a `selection_revision` fence. MCP can inspect but cannot impersonate the active GUI session or change its selection safely. Requires explicit session ownership. |

## Non-command release gates (not included in the percentage)

1. Authenticate **LocalControl by OS user or equivalent peer principal**. Loopback alone, MCP `--allow-write` and opt-in environment flags do not secure the Core listener.
2. Main agent to perform real Slint GUI, Unity, speech devices/provider and restart E2E; Core-only fixtures are not sufficient. Do not mark #442 E2E gates passed without evidence.
3. Inventory all public `GuiPresenter`, Slint callbacks, settings surfaces and launcher actions beyond the 68 runtime command variants; add newly found uncovered operations to the separate extended-surface backlog.
4. Enforce reviewed privilege boundaries for raw diagnostic trace and OS GUI session routing; neither should be exposed via generic Core debug commands by default.
