# ABCD integration: review of PR #416

Reviewed head: `a6612788dcb4a087e0f98957fa66c98585ae22f8`, seven commits after integration `f9c432b1`. The attached iteration-7 report was treated as a set of claims to verify. The destination is `codex/abcd-integration-20261110`, not `main`.

## Accepted improvements

| Boundary | Evidence in implementation | Limits |
| --- | --- | --- |
| GameLink command ownership | A per-session asynchronous gate covers owner validation, replay lookup, execution and receipt storage. Resume takes the same gate. | The race regression exercises the ownership gate; this is not a live Unity acceptance test. |
| Semantic retrieval freshness | The Core binds Timeline authority to its semantic retriever. Query/document embedding and generation reranking validate the pinned turn before and after external work. Archived-history generation now propagates that turn. | Validation and dispatch are separate operations. No transaction-level exclusion of foreign SQLite writers is provided. |
| Python installation ownership | `AiHubInstallCoordinator` owns approval, preview and active installation task handles and joins/aborts them on shutdown. GUI consumes operation events. | Actual installer/subprocess cancellation needs end-to-end verification; the new shutdown test only proves cancellation of an injected pending future. |
| Native ownership | `AiHubNativeService` is created and shut down by ApplicationLifecycle. Claims are shared with Python at the service registry. | It is not yet a complete cross-implementation selection/reconfiguration owner. |
| Native speech | Signed manifest capabilities and worker attestation precede typed, correlated TTS/ASR IPC. Each speech service has its own request mutex and process session. | Fixtures implement the protocol, not a real neural model. |
| AI Hub discovery | One read-only catalog distinguishes service, model ID, implementation and native digest; exact native start/stop commands verify the revision. | Python entries still expose a plugin-level identity with no revision. This does not yet provide uniform revision selection for both implementations. |

## Corrections made while integrating

- Removed `LegacySpeechEngine`, its read-only conversion and the custom settings wire deserializer. Old `tts_engine/asr_engine` input is rejected as an unknown field. No existing local settings were converted or overwritten. This supersedes the compatibility paragraph in `abcd-service-worker-ownership-20261111.md`.
- Duplicate unified-catalog identities now reject **all** ambiguous rows. Previously `retain(seen.insert(...))` kept whichever conflicting row was first, despite the stated fail-closed contract. A regression covers both discovery orders.
- Fixed the stale AI Hub filter: unified rows advertise implementation metadata, but the menu still compared it against NVIDIA/ONNX/CPU, hiding every row. It now filters the explicit Python/Rust metadata. This is a list filter, not an execution-engine setting; actual acceleration filtering needs authoritative capability metadata first.
- Added 25 missing English translations found by the actual declaration-completeness test. The one graph key fixed in the PR was insufficient to make this gate pass.
- Fixed a GUI test's assumption that the selected memory remains the first row after another memory is added. It must preserve the selected MemoryId independently of UUID-based ordering.
- Added an actual Slint adapter regression for same-named Python/native entries, two native revisions, inventory arrival in either order, stale revision lookup and the fully qualified start/stop command payloads.

## Remaining architectural work

These are integration follow-ups, not evidence that all PR boundaries are complete.

### P1 — model selection is not connected end to end

`SpeechSettings` now has `tts_model/asr_model`, while existing functional settings presenters expose preferences containing `local_model_id/recognizer_id` and do not expose a typed model-reference selection command. `model_for()` prioritizes the new reference; the Python planner and preference editors still use the old ID fields. Contradictory values are not rejected by validation.

The GUI's `ActivateNativeModel` command starts a native worker, but does not select it in speech settings. Conversely, persisting a native reference does not cause the current Python speech settings watcher to activate that native worker. Starting a native model in AI Hub must not be advertised as selecting it for normal speech.

Required next contract: one explicit service/model selection owned by Core, an atomic functional presenter command, and runtime resolution of that exact identity. Model options and language also need implementation/revision-aware scoping if the same plugin ID can represent different implementations. Do not add another engine toggle or infer a replacement after errors.

Relevant code: `src/settings/speech.rs::SpeechSettings`, `src/presentation/settings.rs::TtsSettingsPresenter/AsrSettingsPresenter`, `src/bin/neuromita-gui/runtime.rs::ActivateNativeModel`, `src/ai_runtime/live_speech.rs::SelectionKey`.

### P1 — Python speech replacement still affects sibling services

`LiveSpeechRuntime::reconcile` acquires both TTS and ASR transition gates, takes the entire current RunningAiApplication and shuts it down before starting the new plan. A change to one service therefore retires the other Python speech worker too. Separate OS processes alone do not establish independent service lifecycles.

There is an additional composition hazard when a prepared application containing semantic workers is transferred into LiveSpeechRuntime: later speech reconciliation shuts down that whole application, while semantic claims remain held by the outer owner. This mixed configuration needs a regression and separate ownership; the desktop's ordinary speech-only path does not prove it safe.

Required next contract: per-service replacement retains untouched worker generations and claims. Failure to initialize ASR must not stop an unchanged TTS/Embedding/Reranker owner. Do not solve this with retries or another shared coordinator that hides the coupling.

Relevant code: `src/ai_runtime/live_speech.rs::reconcile`, `src/app.rs` transferring `ai_runtime.take()` into LiveSpeechRuntime and retaining `prepared_claims`.

### P1 — authoritative validation is not an atomic dispatch fence

The additional Timeline reads substantially improve stale-result detection. However, an external writer can commit after validation and before provider IO. Awaiting a service permit or a provider queue can widen that interval. Revalidating after return rejects stale output but cannot undo sending private text externally.

Define the writer/admission linearization contract first, including foreign SQLite writers and provider queues. Test deletion/reset during a blocked provider admission. Until then, claim best-effort pre/post validation, not a guarantee that no forgotten content can reach an external provider.

### P2 — execution coverage and strict static checks

No live Unity client, signed production native model or paid external inference was used. Native Embedding/Reranker protocols remain absent. Full strict GUI Clippy fails and is not waived by the passing test suite. The integration branch remains the place to close these gates before main acceptance.

## Verification record

Windows, vendored dependencies, locked/offline Cargo, at most two compilation jobs. Logs are local ignored files under `target/abcd-validation/`.

- Full default suite: 1,690 passed, one failed, 11 ignored. The sole failure was English declaration completeness. Its corrected target is rerun separately below; nested child-process summaries with filtered tests are excluded from this count.
- Initial GUI suite: 33 passed, one failed; the failure was the UUID-order assumption described above.
- Corrected GUI/localization rerun: 35 GUI tests and all nine localization tests passed. The final GUI rebuild/rerun after the list-filter correction also passed all 35 tests (`pr416-final-gui-filter.log`).
- Final default accounting: 1,691 passing tests and 11 ignored, combining the full default run with its corrected localization target. The entire default suite was not repeated after the translation-only fix.
- `cargo fmt --all -- --check` and `git diff --check`: passed, including the final GUI test addition.
- Strict all-target GUI Clippy: failed (185 diagnostics for the library test target, with overlapping library diagnostics). Both inherited and new issues are present; no blanket lint suppression was added. Log: `pr416-clippy.log`.

Passing tests establish the exercised contracts; they do not close the architectural follow-ups listed above.
