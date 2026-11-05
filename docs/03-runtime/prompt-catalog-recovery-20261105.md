# Prompt Catalog recovery / 2026-11-05

## Recovery inputs

- Rust branch base `51d87276e5441ac7ed220c3999e352790a5e8f92` (`settings-unified-asr-runtime-repair-20261104`).
- Original unpushed PromptCatalog patch has source commit label `3071c36d4` (reapplied to the pinned base, local replay commit `56ead251`).
- Python reference bundle `session-timeline.bundle` at `5f3d061f` (reference only, no edits).
- The former scratch directory was lost at the conversation break. The previous command history was read and the changes reconstructed in their dependency order: memory domain, character snapshots, knowledge retrieval, DSL/request lifecycle, tests.

## Verified behavior

- Atomic idempotent prompt seeding through the Character owner and Timeline.
- Different `MemoryKind::RagOnly`, entity annotations and typed entity relations.
- Character-scoped projection; no additional authoritative memory store.
- Declared seeds persist across SQLite restart; invalid later commands never partially commit.
- `SEED_*` flags from `config.json` are preserved.
- Historical anchors reject initialization.
- Build: `cargo check --offline --all-targets` with Rust 1.98.1: PASS.
- `cargo test --offline --lib`: 299 PASS / 1 FAIL (AI installer fixture requires Python 3.12 in this environment).

## Remaining parity and safety limitations

- This is **not** a complete 1:1 Python DSL replacement; PostDSL and unrestricted expression semantics remain unsupported.
- Live GUI and real provider inference were not exercised.
- Semantic relation ranking is deterministic lexical matching, not a general-purpose graph reasoner.
- Prompt initialization and generation are separate commits; cancellation after initialization does not roll back persisted seed memory.
- The local code is not automatically published to the GitHub branch. Preserve the patch and bundle artifacts.
