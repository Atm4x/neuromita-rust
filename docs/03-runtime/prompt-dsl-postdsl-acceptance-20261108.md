# Prompt DSL / PostDSL acceptance delta (2026-11-08)

This document describes **verified source-level facts and test coverage**, not a
claim that issue [#18](https://github.com/Atm4x/neuromita-rust/issues/18)
is complete. Changes are based on the open
[PR #93](https://github.com/Atm4x/neuromita-rust/pull/93)
head `e38ee3975cee12d2a3b06535df1f2537fdef05ad`, not upstream `main`.
The new PR targets that feature branch so the entire #93 history remains a
dependency rather than being re-imported.

## Python sources and fixture provenance

- Active public Python repository inspected:
  `NeuroMita/NeuroMitaPython`, `main` commit
  `97059b0a40646a5fdd784aeda6c26753c0bab470`.
  Source files: `src/DSL/dsl_engine.py` (blob
  `edd961344a512722218c3d3b1c91a0e0fc1184fa`) and
  `src/DSL/post_dsl_engine.py` (blob
  `ae9bda9ad980af87d477582c7ba08d2c43e59376`).
- Also inspected `Atm4x/NeuroMita`, `main` commit
  `97310d6aaf6bf7eeb15ae84cf606adc0f024e135`;
  its `src/DSL/dsl_engine.py` blob is
  `fface9735d5c767a9711805249766bf2e1b46a4e`.
- Existing Python golden output was generated for the *older*
  `Atm4x/NeuroMita` reference `5f3d061f523d9abefa6a264bb2953048a935fa65`,
  described in [prompt-real-corpus-parity.md](prompt-real-corpus-parity.md).
  **It has not been regenerated against either current Python head.**
  These fixtures are retained unchanged; Rust output is not used as an
  alternate Python oracle.

## Acceptance matrix

Legend: **existing** = present in the #93 branch, **new** = changed/test added
here, **unverified** = cannot claim current Python parity or completed run.

| Capability | Python/reference evidence | Rust #93 baseline / this PR | Acceptance |
| --- | --- | --- | --- |
| Prompt sets, Default/fallback, character selection | Runtime Python resolver needs full request parity check | Existing catalog/settings and admission capture; `src/prompts/mod.rs` tests | Existing; GUI empty-set fallback corrected here |
| RU/EN, missing/corrupt files | Source/file corpus examined in #93 | Existing corpus, missing-set/config rejection, bounded snapshot reads | Existing tests; live RU/EN UI **unverified** |
| Snapshot during pending request | Request-scoped contract | `ChatApiState::send` captures settings/preset/pack at admission; source changes cannot affect captured files | Existing unit tests; concurrent live GUI run **unverified** |
| Variables, includes, nested scripts, conditions | Python `DslInterpreter` at stated SHA; real corpus baseline at old SHA | Existing interpreter and 31-set corpus; this PR fixes case-insensitive `SET local` / `then` and invalid `ELSE` arguments | New deterministic regression (not executed here) |
| Safe expressions and erroneous inputs | Python `safe_eval_expression` with a bounded allow-list | Rust expression parser deliberately supports a subset and fails on unsupported inputs | 21/31 exact old-source corpus outcomes; **current parity unverified** |
| `SET LOCAL` isolation | Python script invocation clears local map; redeclaration is first-write-only | Rust interpreter creates per-render state, clears locals per script; PostDSL locals are invocation-only | Existing + new tests; broader concurrency/error matrix **unverified** |
| Seed initialization / Timeline | Python side effects not represented by corpus oracle | Existing typed declarations, single CharacterHandle/Timeline command, atomic idempotent batch | Existing unit + SQLite tests; Python seed-effects parity **unverified** |
| PostDSL TEXT/REGEX/FIELD, custom params | 9/9 sampled PostDSL cases in older Python reference | Existing pure evaluator; no mutation before request acceptance | Existing parity fixtures + new preview/replay/error test (not run here) |
| Invalid/cancelled/stale/duplicate acceptance | #19 behavioral contract; differs deliberately from imperative Python | Existing `TurnProposal` + `CharacterHandle::accept` / Timeline CAS and atomic append | Existing character-owner tests; full mock-provider suite **unverified** |
| Streamed and tool intermediates | Python current behavior not replayed | Providers decode final structured response, generator post-processes it once | No new streaming/tool integration test; **unverified** |
| `DEBUG_DISPLAY` | Python `DEBUG_DISPLAY` parser and debug variables | Existing GUI projection; now redacts credential-like source names/value patterns, defaults without packs | New unit test (not executed here); live GUI **unverified** |
| Restart/persistence | Python state persistence differs | Existing SQLite seed/variable replay tests | Existing tests; full end-to-end restart **unverified** |

The prior corpus reports 31/31 templates render, 21/31 exact stable/dynamic
block comparisons, 10/31 annotated differences (Python unknown-local shadowing),
and 9/9 sampled PostDSL cases. These are **previous reported results**, not
results of this PR or of the current Python revision.

## Confirmed defects addressed

1. `SET LOCAL` recognized only uppercase `LOCAL`; Python uses
   `parts_after_set[0].upper() == "LOCAL"`. Mixed-case local variables could
   accidentally become ordinary variables or raise expression errors.
2. `IF ... then` / `ELSEIF ... then` did not recognize case-insensitive
   `THEN`; Python checks `condition_without_comment.upper().endswith(" THEN")`.
3. Rust silently accepted unexpected text after `ELSE`, while Python rejects
   `ELSE` with arguments.
4. The GUI could project credential-named `DEBUG_DISPLAY` values (or common
   token/key formats) verbatim. This is a presentation-only redaction; it does
   **not** alter the authoritative variables or change the DSL/Timeline.
   Redaction is best-effort, not a proof that arbitrary custom secret formats
   cannot appear in a user-supplied debug configuration.
5. With a configured but empty catalog, request admission supports fallback
   to the built-in prompt, while the GUI debug preview returned `MissingSet`
   instead of the built-in debug fields. An explicitly selected missing set
   still produces an error.

No second parser, state store, or side-effect executor was introduced.

**Not changed:** `//` inside PostDSL literals. Both the current Python
parser and the existing Rust parser split inline `//` as comments. Treating
URLs differently would change behavior without Python parity evidence.

## Test and quality gate ledger

The following new source-level tests were authored:

- `prompts::tests::python_keyword_casing_and_local_updates_match_active_dsl`;
- `prompts::tests::local_variables_are_script_scoped_and_reset_on_each_render`;
- `prompts::tests::a_b_a_character_switch_keeps_snapshots_and_defaults_isolated`;
- `prompts::post_dsl::tests::preview_replay_and_invalid_custom_fields_never_mutate_input_state`;
- `presentation::characters::debug_display_tests::debug_display_redacts_credentials_but_not_character_secrets`.

Existing acceptance-oriented coverage includes `tests/prompt_corpus.rs`,
`src/request/prompt_preparation.rs`, `src/request/prompt_initialization.rs`,
and `src/character/handler.rs` (atomic acceptance, stale branch, duplicate
state, SQLite persistence).

| Required command | Actual result in this environment |
| --- | --- |
| `cargo fmt --all -- --check` | **NOT RUN** — Rust toolchain unavailable |
| `cargo check --locked --offline --all-targets` | **NOT RUN** — no Cargo |
| `cargo clippy --locked --offline --all-targets -- -D warnings` | **NOT RUN** — no Cargo |
| `cargo test --locked --offline` | **NOT RUN** — no Cargo |
| `cargo check --locked --offline --features gui --bin neuromita-gui` | **NOT RUN** — no Cargo |
| `git diff --check` | **NOT RUN** — no local checkout; git remote unavailable |

The execution host has no `cargo`/`rustc`, and direct Git access cannot
resolve `github.com`. Code was updated through the authenticated GitHub
connector, not a local test runner. **Do not promote these tests to PASS until
they actually execute** on the target branch. The earlier #93 author's
validation is recorded in that PR but is not a substitute for this new check.

## Acceptance blockers / dependencies

- Run the required quality gates on this branch, resolve any findings and
  attach their actual results.
- Regenerate and compare Python golden outputs at the pinned **current**
  Python source commit. The existing 10 semantic divergences must be explicitly
  accepted or resolved; they are not green Python parity.
- Execute full mock LLM response → PostDSL → accepted effects → SQLite reopen
  scenarios, including cancellation, duplicate answer, A→B→A, changing settings
  in flight, late responses, and preview with no effects.
- Run GUI/debug selection and refresh checks with both characters and a
  configured/empty catalog; verify all non-key secret formats in context.
- Issue #19 remains the owner of structured response/action acceptance
  semantics, especially tool-intermediate behavior. No #19 closure is implied.

**Do not close issue #18 based on this PR alone.**
