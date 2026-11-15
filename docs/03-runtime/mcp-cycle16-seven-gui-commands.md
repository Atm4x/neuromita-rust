# Cycle 16 — MCP equivalence for the seven missing GUI commands

The MCP server uses the application-owned Presenter handles. It does not start Unity, replace the GUI, or bypass canonical Timeline / Settings / Knowledge owners.

## New tools

- `presenter_memory_recall(character, memory_id, query_text)`: explicit historical recall of one forgotten record. Requires MCP `--allow-content`; returns the original API result without changing memory or injecting into generation. Caller must separately verify `Forgotten`/`Fact` selection in `presenter_memories`.
- `presenter_graph(action=preview, character, memory_id, mode=clear|reindex)`: reads canonical character memory and returns an opaque `request_id` and relation count. The underlying immutable plan remains private to one shared `GraphMaintenancePresenter` used by GUI and MCP. `reindex` requires an injected extractor; no silent fallback. Repeated preview invalidates the previous one.
- `presenter_graph(action=apply, character, request_id)`: consumes the exact previous plan once and calls the canonical atomic GraphMaintenance API. Requires MCP `--allow-write` **and** Core host `NEUROMITA_ENABLE_LOCAL_GRAPH_MAINTENANCE=1` because existing TCP listener does not authenticate local OS users. A missing/foreign/stale plan fails closed. `abort` discards the plan, never a committed operation.
- `presenter_attach_file(character, version, text, source)`: imports an absolute local file (max 8 MiB in configured TimelineAssets) and publishes one verified attachment-bearing message through the same `ConversationPresenter` as GUI. `version` is the branch/head/generation fence returned by `presenter_history`; any advancement rejects. Requires MCP `--allow-write --allow-content` and Core `NEUROMITA_ENABLE_LOCAL_FILE_ATTACH=1` on a trusted machine. Source path is not written into canonical message content. No inference worker is started for a typed attachment.
- `presenter_prompt_folder(character, action=locate|open)`: resolves actual selected prompt folder under the canonical PromptCatalog and opens host folder via OS file browser on `open`; requires MCP `--allow-content` to expose the local path, and `--allow-write` plus Core `NEUROMITA_ENABLE_LOCAL_HOST_ACTIONS=1` for opening.
- `presenter_guide_retry()`: retries the **same failed GUI Guide change**, via the shared `GuidePersistencePresenter` and Settings repository, rather than inventing another retry queue. Requires `--allow-write`. If no failed change exists, returns an error.

## Main E2E agent protocol

1. Start one real NeuroMita application with GUI/Unity as needed; MCP attaches to the same Core LocalControl port.
2. Inspect current character and memory snapshot before recall or graph operation. For graph, preview, review `relation_count`, then apply the exact `request_id` without switching character; inspect memory state afterward.
3. For attachment, read `presenter_history` and use the returned exact `version` for `presenter_attach_file`; verify its resulting EventId and content-part inventory via normal GUI/history read. An error may leave an orphan asset, but no canonical receipt for missing bytes.
4. For Guide, first exercise a genuine save failure through GUI or MCP, then call retry. Validate persisted revision after I/O and a physical restart.
5. For host actions and secret-sensitive permissions, use only an isolated machine/profile. The opt-in environment variables **do not authenticate** a LocalControl peer; secure the transport separately before deployment.

## Boundaries

The 68 `runtime::Command` variants are the parity denominator, not all public Presenter methods. These operations are exposed so that the main agent **can perform** full-stack acceptance; local IPC fixture tests are not full-stack Unity/Slint/device acceptance. Do not mark issue #442 E2E gates PASS solely on the basis of this tool surface.
