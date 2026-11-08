# C4 AI Hub — Core-owned installation operation visibility

## Existing behavior and bug

PR #300 introduced a real, admission-gated installation path for a builtin,
asset-free `remote-api` plugin overlay. Its UI showed progress and completion
inside `HubInstallPreview`, which is tied to one selected model and review
revision. The `SpeechInventory` refresh and model navigation deliberately
invalidate that preview, even while the Core installation continues. This
caused the visible operation status to disappear.

## Implemented boundary

- The existing Core task and `active_install` UUID remain the only owner of
  execution and completion. No second installer, queue, storage, or worker is
  introduced.
- `Update::AiHubInstallOperation` is emitted only when a real Core task starts
  and when its result arrives, with the same UUID, model category and ID.
  An invalid/replayed confirmation does not create a running operation.
- Slint receives a separate `HubInstallOperation` value on `AppWindow`.
  It is **not** cleared when `SpeechInventory` replaces the catalog or
  `HubInstallPreview` is invalidated. The one-line, non-modal notification
  appears only while an operation has been reported; finished operations can
  be dismissed by the user. The dismissal key is held by `AppWindow`,
  preserving that choice when the AI Hub page is unmounted and reopened.
- An `Update::Stopped` during an observed active operation changes the visible
  status to **unknown**, not successful or failed. The last Core result is
  not persisted: this is session-only UI visibility, **not** an install journal,
  restart recovery, a durable queue, or a guarantee that a subprocess can
  finish after application shutdown.
- The term **overlay installed** never means a model was loaded, asset weights
  were installed, or an inference worker is ready.

## Acceptance and limits

The independent visual QA uses the actual Slint AI Hub and Sidebar components
with synthetic catalog entries and operation events to check layout at
1560×918 and 1180×680; it must not be presented as production install E2E.
The executable Core tests continue to verify admission and real overlay
publication. Production GUI typechecks verify the event bindings; **actual
Core → installed UserData → native window click E2E remains unverified**.

Next: a Core-owned durable installation receipt/status query for restart and
recovery, followed by a selected-backend/asset admission contract and genuine
TTS/ASR package/weights installation. Destructive removal remains disabled.
