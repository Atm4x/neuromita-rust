# Chat composer, attachments, screen capture and microphone controls

**Capability:** `CAP-UI-CHAT-001`, **Status:** ACTIVE/CONDITIONAL; source audit 2026-10-30 `CodeNow.zip`, UI/backend not launched. **Scope:** composer and submission admission, not complete message rendering (see separate UI gap). Critical distinction: composing, staging and request acceptance are not the same transaction as successful model response.

## Composition and states

`src/ui/widgets/chat_panel.py:54-113,353-550` contains conversation heading (character/runtime source), actions History, Refresh, Clear chat, multi-line composer, attach, screenshot, microphone, send/stop button, staged-image thumbnails and warning/settings deep-link. `src/ui/widgets/chat_panel_presentation.py` declares immutable `ChatPanelState`: character_id, `blocked`, warning/category, backend readiness, `can_send`, number of active generations, text/attachment presence, typed `ASRCaptureState`, dialogue snapshot. `ChatPanelActions` delegates send, cancel, refresh/clear, settings, image display to application shell; the widget does not implement LLM requests.

`src/controllers/gui/chat_panel_view_model.py:40-149` listens for character/preset/dialogue/capture/generation/settings state and coalesces composer refresh, updates at most every 800 ms for radio lease state. `_block_reason:174-205` distinguishes absent usable API preset and absent prompt sets for the active character. When blocked, `chat_panel.py:99-115` hides composer and displays a warning with deep-link to settings. `can_send` requires ready backend, no block, and at least one of text/attachments/automatic screen or camera capture (`chat_panel_view_model.py:158-173`); `can_submit` adds `not asr_busy` (`chat_panel_presentation.py`). This is UI admission only, not guarantee provider accepts or responds.

## User actions and exact contracts

| UI action | Source and effect |
|---|---|
| Enter | `chat_panel.py:246-288`: sends unless Shift is held; Shift+Enter uses normal multiline input; Ctrl/Meta+V or Shift+Insert tries to stage clipboard image first. |
| Send button | `:166-203`: when `active_generation_count > 0`, button changes to Stop and invokes `ChatPanelActions.cancel_active_generations()`; otherwise `send_message()` only if `can_submit`. Number of active generations comes from `GenerationActivityService` and `Events.Chat.GENERATION_ACTIVITY_CHANGED`. |
| Attach files | `:310-322` → `ChatPanelViewModel._stage_files:225-294`: accepted suffixes png/jpg/jpeg/webp/bmp/gif; max 8 files per dialog, max 32 MiB each, max 64 MiB aggregate **per staging operation**. File reads off Qt thread, invalid extension/size/read-race raises visible error. Bytes are not decoded/validated for image structure at this stage. |
| Paste clipboard image | `:323-334`: convert actual clipboard QImage to PNG bytes, dispatch stage intent; no per-file/batch limit validated in `_stage_images` path. |
| Remove thumbnail / clear attachments | `:205-233,336-342`: removal updates view snapshot/preview count; clear-all intent additionally emits `Events.Chat.CLEAR_STAGED_IMAGES`. Thumbnail click opens full image via host action. |
| Screenshot | `:455-472`, `ChatPanelViewModel._capture_screen:304-329`: optional `CaptureService.capture_screen(1)`, extracts image bytes, stages results; if no image, shows feature settings warning. Screenshot button itself is a one-time staging operation, not equivalent to `AUTO_ATTACH_IMAGES`. |
| Radio microphone | `chat_panel.py:117-164`, `ChatPanelViewModel._toggle_microphone:348-380`: radio mode toggles a session-scoped capture lease (`session_id`, monotonically incremented generation); any other mode opens microphone settings. ViewModel renews its active lease on capture refresh and releases it on close (`:388+`). Disabled/permitted/recognizing are separate states; send is blocked while ASR recognizes. |
| History/Refresh/Clear | `chat_panel.py:353-405`: separate actions; History opens character-filtered DB Viewer through VM (`:207-225`), Refresh and Clear call application shell callbacks. Clear-chat callback downstream requires separate audit. |

## What actually leaves composer

`src/controllers/gui/app_shell_controller.py:190-254` reads `staged_images_snapshot()` from current ChatPanel **before** asynchronous automatic capture, freezes `character_id` and current user input, optionally appends screen/camera frames, and delegates to `_finish_send_message:256-345`. That method concatenates `explicit_image_data + screen + staged + camera`, drops all images if `ENABLE_IMAGE_ANALYSIS` false, deduplicates payloads, refuses an entirely empty message, generates `req_id` and `ConversationMessageIds.incoming(req_id)`, renders outgoing bubble, then emits `Events.Chat.SEND_MESSAGE`. If images were staged it emits clear-staged event and empties the view.

**Important:** `send_message()` returns `True` for scheduling async capture as well as direct-call finish, not necessarily for successful emission/provider processing. `ConversationMessageIds.incoming` correlates the displayed outgoing bubble to the request, but a failed provider may still leave a sent-looking bubble. `app_shell_controller.py:290-296` currently labels image data URLs as `image/jpeg`; actual staged bytes may originate from PNG/webp/GIF; content-type attribution requires a validation fixture.

## Ownership and risks

`ChatPanelViewModel._stage_images:294-302` emits `Events.Chat.STAGE_IMAGE` **and** `ChatImagesStaged` for Qt preview. `src/controllers/chat_controller.py:1266-1297` independently creates `nm_clip_*.png` temp files and owns `_owned_staged_images`; the send path above uses the **view's bytes snapshot**, not those temp paths. Removing one thumbnail (`chat_panel.py:336-342`) changes the Qt snapshot without publishing a per-image-remove event for ChatController temp staging. Full clear cleans controller temp files. Closing ChatPanel releases radio capture but its close path does not explicitly clear temp staged files. Thus there are **two staging representations and cleanup lifecycles**, with a static potential orphan temp-file risk; no runtime reproduction or retention measurement performed.

A `run_exclusive("chat-stage-files")` prevents overlapping file-stage workers but does not enforce 8/64 MiB on cumulative images already staged from prior operations or clipboard paths. Source confirms per-operation limits, not a global complete-chat attachment cap. On `StageImage` event delivery, event dispatcher timing may differ from view preview timing. Need explicit accepted vs durable resource cleanup guarantee.

## Requirements/validation (for future clean-room implementation)

`REQ-UI-CHAT-001`: composer admission and blocked reasons must preserve backend-ready, selected API/prompt config, voice recognizing, text and multimodal-only turns, plus send/stop dual mode; `REQ-UI-CHAT-002`: one authoritative attachment ownership with bounded count/bytes across every ingestion path, typed MIME/validated bytes, preview/remove/clear/submit and no temp leaks; `REQ-UI-CHAT-003`: screenshot/manual/auto capture semantics and source labels, cancellation and error feedback distinguish accepted vs actual submitted; `REQ-UI-CHAT-004`: Radio ASR must preserve session lease, ownership, generation and teardown; `REQ-UI-CHAT-005`: outgoing message correlation must survive async auto capture and suppression of empty post-filtered messages.

Tests to implement later: Enter/Shift+Enter/clipboard image; no API preset/missing prompts and late readiness; empty text with auto image capture that returns no frames; MIME mismatch; multiple-stage cumulative limit; remove one image before send; cancel generation after another starts; stop while ASR recognizing; clipboard paste + app close + temp directory; `ENABLE_IMAGE_ANALYSIS` false with images only; stale character during async capture; file swap/read failure; radio ownership handoff.

**Remaining GUI scope:** actual `ChatWidget`, `MessageRenderer`, `message actions` and `presentation_coordinator` (streaming, delete/edit/regenerate, structured outputs). Эти ветви помечены как следующие для полного GUI audit. Связи: [Sandbox](sandbox-page-functional-contract.md), [Image capture](../01-functional/images-and-capture.md), [dialogue lifecycle](../01-functional/llm-and-generation.md), [runtime events](../03-runtime/typed-services-and-settings-observers.md).

## Rust Slint desktop (Agent B47, 2026-11-07): late completion ownership

The active Rust GUI implementation is narrower than the PyQt contracts documented above: it supports a local-file-to-Timeline event, text submission, and explicit request cancellation, but not PyQt-style staged thumbnails/camera capture. A local file append does **not** automatically start a model request. None of these are implicit parity claims.

`neuromita-gui::sandbox` now stamps `Select`, `Send`, and `AttachFile` with a `(CharacterId, selection_revision)` origin. A character selection increments the view-only revision immediately; runtime snapshots, submit acknowledgments, attachment confirmations, and corresponding errors carry their captured origin. The Slint adapter discards any late update not matching both the current character and revision. In particular, A→B→A is **not** the same selection as the original A. The runtime also rejects stale queued send/attach commands before starting their operation. This revision only fences UI feedback; `ApplicationLifecycle` and Timeline remain authoritative for character state, branch selection, immutable history, and attachment commit validation.

The lightweight `chat_fence.rs` policy has an independently executable Rust test target. `binding_tests.rs` exercises the real Slint callbacks and stale update application via a headless adapter. Compilation/type-check and executable Slint results must be reported separately: a successful `cargo check` is not a successful runtime GUI test. Full #36 acceptance still requires user-visible streaming, refresh+live dedup, stale history page fencing, branch-switch acceptance, message actions, and parity review against the active Python implementation.

## Rust Slint desktop (Agent B48, 2026-11-07): delayed ASR and cancellation feedback

In the active Rust desktop, a recognized transcript is stamped with the selected `(CharacterId, selection_revision)` **when the desktop runtime receives it**. The UI only merges it into the draft if that selection is still active. Delayed automatic submission captures the same origin and checks it again immediately before invoking the send callback, so revisiting A after A→B→A cannot trigger A's original delayed auto-send. A later recording delivered to the runtime **after** navigation is stamped with the newly selected character; capture-start/utterance-start provenance is not provided by the current speech sink and is **not** certified by this change.

The Slint cancel callback similarly stamps the selected request command. Runtime refuses cancel commands queued for a different selection, and cancel failures are published as origin-scoped `ChatError` rather than an unscoped global chat error. Request cancellation remains owned by `GuiPresenter`; the UI revision only decides whether a completion belongs to the visible composer. The selection token does not alter Timeline or change request acceptance semantics.

The previous combined Slint localization/layout test can fail at an unrelated localization `SourceMismatch("sandbox.attach")` before exercising chat callbacks. The selection/attachment/ASR checks therefore have a **separate headless Slint test**. A successful `cargo check` does not certify the executable GUI test; report its actual runtime result separately. This increment does not implement PyQt-style image staging or history paging and does not claim full #36 closure.

## Rust Slint desktop (Agent B49, 2026-11-07): confirmation and diagnostics selection fencing

The native `ConversationUi` commands `Preview`, `Confirm`, `Abort` and `Diagnostics` are separate from sending a message. They now carry the same ephemeral `ChatOrigin(CharacterId, selection_revision)` used by attachment/send/cancel UI callbacks. The runtime rejects preview and diagnostics commands whose origin does not match the selected character and selection revision. Confirmation is permitted only when **both** the current selection and the stored preview owner match; a new valid preview invalidates a previous preview before computation, and a preview failure clears the confirmation. The actual irreversible Timeline change is still guarded by `ConversationPresenter::apply_change` against the captured `ContextAnchor` and `TimelineSelection`.

Confirmation, diagnostics and errors returned to Slint are tagged with the captured origin and ignored after character reselection. Selecting A→B or A→B→A also closes the visible confirmation, history dialog and diagnostics immediately; returning to A does not reinstate the obsolete A overlay. This is **UI owner fencing**, not an additional Timeline authority or a new persistence store. An isolated headless Slint callback test verifies stale and current updates separately.

The current Rust GUI `Refresh` command does not load a separately paginated historical page. Each runtime observation reconstructs up to 200 projected messages plus live request rows from an authoritative `ConversationSnapshot`; the `VecModel` is replaced rather than append-only. Do not claim that PyQt's `HistoryLoadTicket`/late older-page reconciliation, per-message edit/regenerate menu, or staged-image thumbnails are implemented in Rust merely because the Python parity documentation describes them. Those are outstanding #36 product scope, not regressions to patch with a second history cache.

## B55 — отправка, отмена и голосовой ввод

Основной контрол компоновщика переключается с Send на Stop на основе текущего
`ConversationSnapshot`: выбранный персонаж имеет незавершённый запрос. Stop
передаёт исходный `RequestId` через `ChatOrigin` и не создаёт новый запрос; при
неканцелируемой стадии принятия ответов Stop остаётся неактивным. Под
компоновщиком показывается «Мита думает…», пока запрос находится в работе.
Кнопка микрофона отображает фактическое состояние `SpeechSessionPhase` и
включает существующую ASR-сессию. Для `radio`/`push_to_talk` она также
включает PTT; при выключении отпускает PTT и останавливает запись.
Анимированные полосы рядом с микрофоном обозначают активную запись, но не
претендуют на показ измеренной амплитуды (такого поля нет в `SpeechSessionSnapshot`).

Каждый `DesktopTranscriptSink` получает неизменяемый `ChatOrigin` при старте
конкретной speech-сессии. Transcript из старой сессии не приписывается новому
персонажу: очередь несёт исходный токен и отклоняется на границе runtime. При
смене выбора персонажа текущая session-bound запись останавливается, токен
нового выбора не наследует старую ASR-сессию. Дополнительное требование к
проверке — реальный аудио-девайс и полный Python/PyQt visual parity; без него
не утверждается аппаратная проверка записи.

Retry и Regenerate не подменяются новым `reply(text)` — такое действие создало
бы второй пользовательский Timeline event и нарушило бы смысл повторной
генерации. Для них необходим отдельный контракт на исходный `EventId`,
`RequestId`, observer scope и атомарную проверку Timeline head.
