# ABCD: service selection and ownership follow-up

Base: integration `7e055da3`, including merged PR #416 through `f0f2199e`. This document supersedes the three open P1 sections of [the earlier review](abcd-pr416-review-20261111.md) only to the extent verified below. The separate [module installation contract](../../future/model-module-installation-contract.md), tracked by [issue #417](https://github.com/Atm4x/neuromita-rust/issues/417), remains planned work.

## Explicit model selection

Functional TTS/ASR settings presenters expose a typed `model` binding carrying `SpeechModelReference`. One checked SettingsOwner edit changes the reference and its preference ID together. Invalid input or a contradictory preference edit cannot publish a new revision. Existing Python-specific preference forms retain their explicit Python adapter; they do not change a native reference simply because names coincide.

AI Hub sends the full selected identity through its functional settings binding. Python/Rust is a Slint enum, with an explicit adapter to Core's `WorkerImplementation`. The display string is named `implementation_label`; it is no longer stored in a misleading GUI `backend` field. Selection is distinct from enabling TTS or choosing the signed native revision's compute backend. The latter remains an explicit prerequisite; no backend is guessed.

The speech owner observes model identity as part of its configuration key, including implementation and revision. A native selection follows the normal speech lifecycle: validate the native selection, retire the old same-service Python/native worker, activate the exact revision and expose its actual state. A changed selection while startup is pending is discarded. An existing native worker may be used only when its exact selected identity matches. Typed native TTS/ASR operations continue through the common service invocation contract.

Python asset revision pins are respected even when multiple revisions exist. A missing pin fails instead of choosing another revision. Native SHA-256 references use a canonical lowercase representation, avoiding an identity that parses to the same digest but can never match the active claim.

This closes identity delivery and lifecycle selection. It does not turn the existing Python preference forms or ID-keyed options maps into a universal implementation/revision-scoped parameter schema. That module-owned configuration contract remains part of #417; native worker support here is the existing signed-manifest service protocol.

## Independent service ownership

LiveSpeechRuntime owns two independent SpeechServiceRuntime objects. Each has its own current application session, transition mutex, claim lifetime and applied settings. Unchanged configurations return before taking the inference/transition gate. ASR preparation/replacement does not wait for an unchanged TTS invocation. Only ASR replacement takes the microphone controller lock.

Initial split workers are transferred by service from RunningAiApplication. The outer application retains Embedding/Reranker/Beats and their prepared recovery plans. Subsequent speech changes therefore cannot shut down semantic workers while leaving their outer claims alive. Running application snapshots are scoped to the services they own, including all members of a lower-level composed worker; the product ApplicationLifecycle still requires split topology.

Speech and semantic status streams are combined into one owned presenter stream. This aggregation only projects status; it neither starts models nor owns a second set of workers. Shutdown stops both speech sessions even if stopping one returns an error.

## Canonical dispatch boundary

The final two PR #416 commits add an RAII SQLite write reservation and an exact canonical source check for memory/history dispatch, plus generation-bound service invocations. The reservation remains alive through the provider operation and is dropped before post-validation. Independent SQLite writers cannot invalidate the admitted source while that reservation is held.

Admission now runs asynchronously on a blocking task. Persistent admission captures the repository authority before waiting; it does not hold SessionTimeline's mutex while acquiring a SQLite reservation. Waiting for a competing provider admission therefore does not itself block a Tokio executor thread.

Asynchronous admission exposed a cache reconciliation race. A request trigger is not part of the canonical context prefix identity. Refreshing the same selection/head/context epoch must not invalidate an in-flight joint memory/history operation merely because the request has a trigger and the background reconciler does not. Memory projection refresh also preserves eligible history cache entries. A history-only read tracks its prefix without replacing canonical memory membership; when the memory projection for that same prefix becomes known, it does not invalidate the history read. Reset/selection/epoch changes still invalidate the old barrier.

This is a correctness-first reservation, not a latency optimization. It holds SQLite's writer reservation through the external call, so canonical writes may wait or reach the database busy deadline. Cancellation releases the operation's lease. File replacement and external Documents corpus mutation remain different authority boundaries. Native semantic inference protocols and production-model acceptance remain outside this fix.

## Verification

The complete default locked/offline suite finished with **1,704 passed, zero failed, and 11 ignored**. This count excludes nested child-process summaries with filtered tests. All **35 GUI binding tests** passed on the final code. The run logs are under `target/abcd-validation/closure-delivery-*`.

- `cargo check --locked --offline --no-default-features`: passed, including the transient/non-SQLite authority path.
- `cargo build --locked --offline --features gui --bin neuromita-gui`: passed on Windows with two GUI binary warnings.
- Strict default `cargo clippy --locked --offline --all-targets -- -D warnings` failed with **185 diagnostics** for the library-test target. These include large error variants, complex signatures, platform-conditional unused imports, deprecated API use and a test's unchecked read count. The earlier strict GUI Clippy check also failed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

The regression set includes typed settings atomicity and rejected divergent edits, explicit Python revision pins, native typed TTS routing without same-name substitution, unchanged TTS admission during ASR reconfiguration, SQLite foreign-writer/source checks, common service-session ABA/cancellation checks, and real Slint model identity/selection callback tests.

The tests use native protocol fixtures. Acceptance with production models, paid providers and a live Unity client remains open.
