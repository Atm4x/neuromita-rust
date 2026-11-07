# Windows uv staging recovery: named kernel Job witness (C3)

## Scope

The existing Windows installer binds `uv` to a private kill-on-close Job Object and checks job termination on cancellation. The earlier Windows orphan scanner was deliberately disabled because a cloned file lock inherited through stdin did **not** prove child liveness after parent death on Windows (GitHub Actions run 42343170731). This change adds a conservative, independently attestable restart recovery path.

## Protocol

1. The installer creates a unique `.staging-<uuid>.lock` and stage under the managed layer parent, with an exclusive lock held by the owner.
2. After `uv` has spawned, Windows creates `Local\\NeuroMita-AI-uv-stage-<uuid>`, enables `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and successfully assigns the child. A name collision is an error; no existing job is adopted.
3. **Only after successful assignment**, an adjacent `.staging-<uuid>.job` witness is created with the current Windows session ID and flushed to disk. Failure to create or flush the witness prevents further installation.
4. On the next installation, the Windows scanner considers only strictly named staging directories with a regular, bounded witness and a lock it can acquire. It compares the witness session ID to the current session, and opens the expected kernel Job by derived UUID name. If the job can still be opened or the query fails, the directory is preserved. Only a positive kernel `ERROR_FILE_NOT_FOUND` for the expected job permits removal. Missing/legacy/invalid witnesses are retained.
5. When publication succeeds or cancellation cleanup proves job termination, the staging and both markers are removed. Before committing a successfully completed direct-uv operation, the installer terminates/waits for remaining job descendants so no child may still write the candidate revision.

The logic introduces no global installation index, PID polling, wall-clock expiry or fallback deletion. The OS kernel object is the authoritative liveness witness. The marker is evidence that an *identical UUID-named job was successfully configured and assigned*, not a second model state store.

## Safety boundaries

- The filesystem containing the staging root and witness is assumed to be app-owned; an untrusted same-user process could tamper with evidence. Hostile filesystem replacement and remote filesystem lock semantics remain out of scope.
- Windows job names use the local session namespace. Cross-session recovery is **disabled**, not guessed. Stages from sessions that cannot be verified are preserved. If the job cannot be opened for a reason other than `ERROR_FILE_NOT_FOUND`, recovery is skipped.
- A crash between spawning the child and successful job assignment has no witness and is left alone. A crash between assignment and witness creation also leaves the stage alone. These are deliberate fail-closed outcomes, not guaranteed disk reclamation.
- No recovery on application startup is added: the scanner runs before a subsequent layer installation that needs staging.
- The complete user-facing install/remove API, persisted shared-dependency ownership, real uv-managed CPython downloads, and arbitrary model-asset integrity remain separate acceptance items of issue #42. Legacy stages without attested job ownership are not swept.

## Verification

The Windows-gated regression `windows_reclaims_only_closed_named_jobs_with_verified_witness` checks absent, wrong-session, live and gone-job states with a real PowerShell child. The Linux library regression checks for behavior changes under the existing non-Windows path. Prior independent Windows CI proves `KILL_ON_JOB_CLOSE` terminates a child after its parent is forcibly killed (run 42347035310). The new Windows scanner itself requires a fresh Windows CI run before certification.
