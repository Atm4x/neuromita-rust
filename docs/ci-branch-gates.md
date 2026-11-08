# Fork-only, deliberate CI gates

The only automatic trigger is **creation of a named CI gate branch** in
`ViNonerX/neuromita-rust`. Ordinary development branches and upstream PRs
do not run checks automatically. Manual `workflow_dispatch` remains available.

## Branch contract

| New remote branch prefix | Scope | Maximum job duration |
| --- | --- | --- |
| `ci/format/<issue-or-task>-<attempt>` | `cargo fmt --all -- --check` | 8 min |
| `ci/core/<issue-or-task>-<attempt>` | fmt, check, clippy, library tests | 25 min |
| `ci/full/<issue-or-task>-<attempt>` | fmt, all-features check/clippy/tests, GUI | 40 min |

Examples: `ci/format/pr307-01`, `ci/core/pr307-02`,
`ci/full/pr307-03`. Match the **exact prefix**, including the slash.
Names such as `fix/ci-core`, `ci/core`, and `ci/anything/` do not start
a test job. Branch creations without a valid prefix appear as skipped
workflow events, with no runner allocated.

## Agent policy: only promote a prepared revision

1. Work normally in `fix/...` or `feat/...` and commit the changes.
2. Decide deliberately that the revision is sufficiently complete for a
   specific level of test; choose the cheapest informative scope.
3. Record the **exact ready commit SHA** and use a unique `ci/<scope>/...`
   branch name for that attempt. Do not use `ci/...` as a development branch.
4. The CI YAML **must already be present in the branch's first remote
   commit**. Copy the approved workflow from the fork's `main` branch
   into the new CI-only branch, without modifying product files.
5. Publish the new CI branch **once**. Its creation (not later pushes)
   triggers the selected check. Do not push further development commits
   to a CI branch: results intentionally refer to its immutable creation SHA.
6. Inspect the Actions result. On failure, fix the normal development branch,
   then prepare a new uniquely named CI branch. Never claim a check passed
   until the run completed successfully.
7. Keep CI-only YAML commits and CI branch names **out of the upstream PR**.
   The working PR stays based on the original development branch.

### Git CLI example (Git Bash on Windows, bash on Linux/macOS)

Run in a clean worktree after you have committed your ready code.

```bash
git fetch origin main
git fetch origin fix/prompt-dsl-postdsl-acceptance-20261108
READY_SHA="$(git rev-parse origin/fix/prompt-dsl-postdsl-acceptance-20261108)"
git switch --detach "$READY_SHA"
git switch -c ci/core/pr307-01
mkdir -p .github/workflows
git show origin/main:.github/workflows/rust-manual-budgeted.yml > .github/workflows/rust-manual-budgeted.yml
git add .github/workflows/rust-manual-budgeted.yml
git commit -m "ci: run core gate for $READY_SHA"
git diff --name-only "$READY_SHA" HEAD  # expected: exactly the workflow file
git push -u origin HEAD                 # remote branch creation triggers once
```

Use a **new** branch name for every further intentional attempt; never
force-push/reuse an already published CI gate branch.

### GitHub connector / Git objects equivalent

For an agent that uses the GitHub API rather than a local Git client:

1. Read the ready commit and its tree SHA.
2. Fetch the authoritative YAML from
   `ViNonerX/neuromita-rust@main:.github/workflows/rust-manual-budgeted.yml`.
3. Create a new tree with that ready tree as `base_tree_sha` and
   **only** `.github/workflows/rust-manual-budgeted.yml` added or replaced.
4. Create a commit with the ready SHA as its **sole parent** and that new
   tree, then create the unique `ci/<scope>/...` ref **pointing at the CI
   commit** (not the original ready commit).
5. This creation event will see the YAML already in the ref and can start
   the gate. Check the resulting commit diff is CI-only.

**Do not** create the `ci/...` remote ref first and add the YAML later.
A `create` event checks the workflow at branch creation; adding it on a
later push won't retroactively trigger the gate.

## Quota and security controls

- `on: create` and `workflow_dispatch` only: no `on: push`,
  `pull_request` or schedules.
- Each accepted run is one Ubuntu job, subject to scope-specific ceilings
  (8/25/40 minutes). Concurrent accepted gate jobs cancel older ones.
- Ordinary branch creation is skipped **before** job concurrency and runner
  allocation, so it cannot cancel a running gate or use job minutes.
- The build steps use read-only checkout without persisted credentials.
  A final reporting step alone receives a short-lived `GITHUB_TOKEN` with
  `statuses: write` and posts a scoped GitHub Commit Status to both the CI
  gate commit and its original source parent; no repository code write
  permissions are granted.
- Code is checked with `--locked --offline` using the vendored Cargo
  dependencies (no remote Cargo crate fetch). Format check runs first.
- No compiler caches, uploaded artifacts, or secrets are required.
- A failed job or timeout consumes run time before stopping; quotas are
  *ceilings*, not a promise that checks will be fast.
- If the fork is made private, GitHub Actions billing rules can change.
  The user should review the repository Actions settings and plan limits.

## Manual escape hatch

Open the fork's **Actions → Rust checks (branch gates / manual) →
Run workflow**. Use the workflow in `main`, set `target_ref` to a fork
branch/SHA, choose `format`, `core` or `full`. This is a deliberate
manual run and observes the same runtime ceilings and concurrency limits.

The GitHub connector may be able to **create a CI gate branch** even when
it cannot dispatch workflows. Workflow execution still depends on Actions
being enabled in the fork; inspecting the run result is a separate task.

## Reading the result from an agent / GitHub connector

The installed GitHub connector can retrieve **commit statuses** and can read
workflow jobs/logs when given the numeric workflow-run ID, but it cannot
directly list `create`-event runs. Every gate therefore posts a
`ci/gate/format`, `ci/gate/core`, or `ci/gate/full` commit status when the
job finishes. That status contains:

- `state` (`success` or `failure`);
- `description`, with the first failing step and scope;
- `target_url`, whose suffix is the numeric GitHub Actions run ID.

Agent workflow after the gate finishes:

1. Get the source commit SHA (the working PR head) or the gate commit SHA.
2. Call GitHub `get_commit_combined_status` for
   `ViNonerX/neuromita-rust` and that SHA; select the
   `ci/gate/<scope>` context.
3. Take the final number in the returned `target_url` and call
   `fetch_workflow_run_jobs` with that run ID.
4. For each failed job, call `fetch_workflow_job_steps` and
   `fetch_workflow_job_logs` with the returned job ID. Inspect the actual
   compiler/test error before editing code.
5. Do not turn the Draft PR into a Ready PR merely because format passed.

The reporting step runs on the same runner and consumes no extra job, cache,
or artifact budget. The fork's GitHub Actions page still holds the original
full logs. If the workflow fails before a runner/step can start (for example,
a YAML validation problem), no commit status can be published; in that case
the GitHub Actions run URL needs to be obtained once from the website.

GitHub uses the run's creation SHA as `github.sha` for `create` events.
The automatic gate deliberately checks out that exact SHA and enforces that
its only extra file is the CI workflow. No additional source code can be
smuggled into the gate revision.
