# Read-only native Rust application update check

The Home launcher action **Check updates** now checks the **Rust application**, not the
historical Python launcher or external Unity game. The source is the `latest` GitHub
release of `Atm4x/neuromita-rust`; the existing `GithubUpdateCatalog` validates HTTPS
metadata, publication state, bounded metadata length, unique asset names and publisher
SHA-256 digests. An HTTP 404 for `latest` means **no published native release**, not
"the installed version is up to date". Other failures are reported as errors.

The currently installed version is the Cargo application version. A remote release
is considered comparable only when the tag is an exact `vMAJOR.MINOR.PATCH` or
`MAJOR.MINOR.PATCH` with non-negative, non-overflowing decimal components and no
pre-release/build suffix. No arbitrary string comparison or fallback to Python/Unity
release IDs takes place. A potential update requires a publisher-digested, exact-match
**raw executable** asset for the current supported OS/architecture:

- Windows x86_64: `neuromita-gui-windows-x86_64.exe`
- Linux x86_64: `neuromita-gui-linux-x86_64`
- Linux aarch64: `neuromita-gui-linux-aarch64`

The naming scheme is an **explicit compatibility contract for future releases**;
this document does not claim any such binaries are already published. ZIP/7z/RAR,
PythonBuild, UnityBuild, unrecognized architectures, unverifiable tags, missing
publisher digests and mismatched filenames are not silently treated as installable.

A background, 18-second-bounded read-only check is independent of AI Core state.
Duplicate requests are rejected while in flight. The existing GUI notice reports
not-published, current, older, candidate asset, unsupported platform, unverifiable
or network failure distinctly. A candidate is **not automatically applied or staged**.
Download, file replacement, restart, rollback, actual installed binary identity,
archive support and UserData preservation remain separate acceptance work for #44.

## Explicit verified native download (no installation)

Home → More → **Download and verify update** performs a fresh lookup of the latest
`Atm4x/neuromita-rust` release, repeats exact-platform asset and strict version
selection, then calls the existing `UpdateStager` on its publisher-declared raw
executable. No previous "available" banner is trusted as an authorization to
download. Assets from the Python or Unity release feed are never eligible.

The result is stored only in the selected portable `UserData/updates/staged`
directory after SHA-256 and length checks. A previously cached file is
re-hashed before reporting success. A corrupt cache, truncated or oversized
download, publisher digest mismatch, cancellation and other I/O failures are
reported as failures, not success. While verification is active the GUI keeps
only one native update operation in flight and does not block the Slint event
loop. On GUI controller teardown the cancellation token aborts staging.

**Verified download is not installation.** No archive extraction, replacement of
a live executable, updater helper handoff, restart, rollback or mutation of
settings is attempted. The publisher must first ship a release with the exact
raw native artifact name and GitHub asset SHA-256 metadata.

## Explicit raw executable activation (D47)

Home → More → **Install Rust update and restart** is a separate, destructive user
operation. It does not trust a prior download confirmation: it obtains fresh
native-release metadata, refuses older/unknown versions and non-native assets,
and re-verifies cached bytes (or downloads and checks SHA-256 again) before
creating an installation transaction. Only the publisher's exact platform-
specific executable is mapped to the fixed local `neuromita-gui[.exe]` target.
There is no generic rename API for Python/Unity packages or archives.

The running application must actually be the installed `neuromita-gui[.exe]`
next to `neuromita-updater[.exe]`. Its persistent root must be the ordinary
installation-owned `UserData` directory, not a custom data directory or a
symlink. Missing helper, non-installed executable, or an existing update
transaction fails closed **before staging or replacing the running binary**.
Only the existing `SingleFileActivator` can prepare the journal and verified
candidate. Preparation never mutates the installed executable or UserData.

After successful preparation, the real GUI process schedules the fixed helper,
which checks the parent's *PID plus process-creation identity*, and then quits
the normal Slint event loop. The application runtime shuts down normally;
only after that process exits can the helper apply the journaled file move and
attempt to launch the new GUI executable. If scheduling the helper fails, the
prepared/unapplied candidate is discarded under the installation lock, leaving
both the old executable and user data unchanged. If the transaction has been
partially applied, cleanup refuses to discard unknown installation state.

## Post-restart readiness and rollback (D48)

The GUI update helper now retains its verified old executable backup after the
new executable's SHA-256 matches. The journal records a one-time UUID nonce,
which the helper passes only to the exact new GUI child. At least 800 ms after
the Slint event loop starts, the displayed window publishes a durable readiness
receipt containing the nonce, its PID, and OS process-creation identity. The
helper checks the exact child's live identity and journal state before deleting
the old backup. Merely spawning a process or verifying the EXE hash is **not**
a successful update. This signal confirms initialized, visible GUI event-loop
progress, **not** readiness of every AI engine or attached Unity game.

The helper waits up to 30 seconds for this explicit receipt. On early exit,
invalid receipt or timeout it terminates/reaps only its own new child, then
restores the prior EXE from the hashed backup under the installation lock. The
rollback-intent marker makes interruption between its rename operations
recoverable. It retries Windows sharing-violation / permission-denied renames
for up to four seconds, preserving the journal and backup if the old state
cannot be safely restored; an unrecognized replacement is never overwritten.
UserData is not moved or erased. The helper reports success only after readiness,
and records failure/rollback status in `UserData/updates/last-activation.json`.

**Outstanding acceptance:** No actual published Rust native release or installed
Windows GUI click-through has been exercised yet. Abrupt termination of the
helper itself after replacing the EXE leaves a durable pending transaction, but
an automatic watchdog relaunch across power loss has not been integrated.
Multi-file/archive activation and deeper Core/AI startup acknowledgement remain
separate work; this is a verified *single raw EXE* rollback boundary.

## Explicit offline recovery after helper termination (D49)

If the updater helper itself terminates or the machine loses power **after** the
new EXE is installed but **before** the verified GUI readiness receipt is
accepted, the previous version remains in the journaled backup. Never assume
that restarting the GUI alone completes that transaction. Stop **all**
`neuromita-gui[.exe]` instances and other `neuromita-updater[.exe]` processes,
then run the actual installed helper beside the GUI:

- Windows: `neuromita-updater.exe --recover-interrupted-update`
- Linux: `./neuromita-updater --recover-interrupted-update`

This is an explicit offline repair command, **not an automatic retry or update
installer**. The helper accepts no user-supplied installation path, package
path, nonce or release URL. Its own executable location determines the target.
It verifies the default, non-symlink `UserData` root, the health-armed journal,
and the exact hashes of both the old and new bytes. A still-running installed
GUI or another helper blocks recovery. On Linux, even a running executable
renamed into the transaction workspace is detected by device/inode scanning.
Unknown target bytes, a damaged backup, an unarmed/legacy transaction, or
inaccessible relevant processes cause an explicit refusal without overwriting
evidence. A successfully recovered transaction restores the old binary,
preserves `UserData`, records `interrupted-update-restored` in
`UserData/updates/last-activation.json`, and cannot be replayed.

The operator must start the restored GUI separately after recovery. An
unattended boot-time watchdog or automatic safe helper restart is **not yet
implemented**. Production Windows acceptance still requires testing the real
installed executable, Windows file-sharing locks, and GUI interaction.

### Windows recovery after a forced shutdown (D50)

When the user explicitly chooses **Install Rust update and restart**, the GUI
registers an installation-specific, per-user `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
entry for the installed `neuromita-updater.exe` before spawning the helper.
The entry invokes only `--auto-recover-interrupted-update`, never a new
installation, download, package selection or arbitrary target. Failure to
register the guard blocks an otherwise prepared update. The helper refuses to
apply when that guard is missing or belongs to a different command.

A successful verified restart, or a completed rollback, removes the entry.
If Windows restarts during activation and the helper disappears, the next
interactive sign-in launches that same installed helper to **roll back**, under
the installation lock, provided the GUI and every other updater process are
stopped and the health-armed journal and both executable hashes match.
Unexpected, corrupted or externally modified files remain untouched. The
helper does **not** start the restored GUI automatically after a login-time
rollback, and never retries the failed release.

This is a **per-user sign-in recovery** rather than a system-boot service: it
requires a normal Windows login, a writable HKCU startup key, and successful
startup processing. If a different application auto-launches the GUI before
recovery, the guard fails closed, remains registered for a later sign-in,
and the operator may still use `--recover-interrupted-update` after stopping
all installation processes. Linux has no automatic login recovery in this
release; its explicit offline command remains unchanged. The Run entry is
scoped to each installation path, not shared globally between installations.

## Disposable installed-helper recovery acceptance (D52)

The integration test `tests/updater_offline_recovery.rs` runs the **real native
`neuromita-updater[.exe]` binary copied beside a disposable
`neuromita-gui[.exe]` fixture**. On Windows that GUI fixture is an actual PE
executable running a deliberately isolated subprocess test, not merely a file
with a GUI-looking name. It verifies that explicit offline recovery fails while
the GUI is executing, while the installed EXE has an exclusive Windows sharing
lock, and while its old-version backup has such a lock. After those processes
and handles are released, the same installed helper must restore the exact
previous bytes, preserve `UserData/settings.json`, write a recovery receipt,
clean up its workspace and refuse replay.

The test never publishes a release, downloads an executable, registers a value
under the real `HKCU\\...\\Run` key, or simulates a Windows reboot. The separate
registry-unit tests exercise the recovery lease in an isolated HKCU test key.
This acceptance therefore validates **offline native Windows process and file
sharing semantics**, not the full interactive GUI → latest-release → helper
handoff → authenticated readiness path, Windows sign-in startup ordering, or
publisher release availability. Those remain open acceptance criteria for #44.

## Verified native activation and restart probes (D53)

`tests/updater_native_apply_e2e.rs` has a standalone test entrypoint so a copy
of its compiled executable can run as a disposable installed GUI rather than
recursively invoking a test harness. The executable deliberately appends distinct
ELF/PE overlays to represent the previous and candidate versions; they are
independently SHA-256 hashed. The test creates no published release and never
fetches a remote URL. An exact cache entry is checked through `UpdateStager`
against a publisher-formatted digest, then `SingleFileActivator` prepares the
platform-native asset-name mapping.

On **Linux**, the test runs the actual `schedule_after_shutdown` path from a
separate installed GUI process, launches a real installed `neuromita-updater`,
and waits for its PID-bound helper handoff to replace the executable. Two cases
must pass: a restarted probe acknowledging the real nonce through
`acknowledge_gui_ready`, and a probe exiting before readiness so the helper
rolls back, relaunches the old executable and records the failure. In both
cases persistent UserData is unchanged and the transaction workspace is gone.
The helper's native recovery guard is a no-op on Linux, as in production.

On **Windows**, the same test exercises disposable native PE files, exact
SHA-256 reuse, `arm_health_check` and `apply_or_resume`, the real
`acknowledge_gui_ready` receipt, `complete_health_check`, and
`rollback_health_check` after a failed probe. **It does not start the
production helper or register a startup Run entry**, because a test must not
change the normal user's HKCU recovery guard. It therefore does not prove
Windows helper-parent handoff, interactive Slint readiness, a real Windows
reboot/sign-in, or application AI/Core health. The Windows PE probes emit a
real nonce/PID receipt but do not start the Slint GUI event loop.

## Failed helper scheduling and workspace preservation (D54)

A verified preparation is not permission to abandon a partial handoff. If
writing the parent-intent record, registering the Windows sign-in recovery
lease, arming the one-time health nonce, or spawning the installed helper
fails, handoff cancellation first removes the exact local intent, then
attempts to discard the verified, unapplied transaction under the exclusive
installation lock. Only after the discard succeeds may it deregister the
recovery lease created by that attempt. An already registered identical
lease is not claimed as owned by the failed attempt. If cancellation fails,
its error is reported alongside the original handoff failure rather than
silently treated as a successful rollback.

Before deleting **any** transaction file, activation cleanup validates the
complete workspace inventory: the journal must be present and all entries
must be known regular files without symlinks. Unexpected entries, including
an obstructing `handoff.json` directory, block cleanup *before* candidate or
journal deletion. The original executable and portable `UserData` are not
modified. After an operator removes the conflicting entry, the same
journal can be independently revalidated and discarded. This avoids an
unrecoverable partial cleanup that would otherwise delete the journal while
leaving an unexpected file in `.neuromita-update`.

Linux installed-process E2E additionally exercises successful activation,
readiness-based completion, early-startup rollback, helper launch refusal,
missing-helper refusal and a conflicting handoff record. Windows validation
uses native CI; it does not imply a real published release or a completed
interactive click-through with a running Slint GUI.
