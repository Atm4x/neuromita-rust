# AI plugin discovery: initial C1 boundary

The Python host supports the previously installed flat directory layout and a
shared root with `builtin/<plugin-id>` and `community/<plugin-id>` folders.
Discovery reads bounded JSON manifests only; it does **not** import plugin code.
Installation hooks use the same unique plugin resolver as inference. Duplicate
IDs across flat, builtin and community sources are rejected, never shadowed.

For community plugins, `plugin.json` must contain `id`, `name`, `kind`,
`entrypoint: "plugin.py"`, `api_version: 1`, a semantic `version`, a stable
`publisher` and an explicit `permissions` array. The host validates the existing
API/version, kind, filename containment, a 64 KiB manifest limit, safe ID and
publisher syntax, and supported permission names. Legacy flat/bundled manifests
remain compatible when publisher and permissions are absent. Each invalid
plugin is reported separately as `discovery_status: invalid`.

Permissions currently recognized: `network`, `filesystem_read`,
`filesystem_write`, `subprocess`. A non-empty permission declaration produces
`discovery_status: approval_required` and **all** dynamic Python operations
reject it. There is deliberately no approval bypass yet: approval storage,
policy propagation and OS/process isolation are future work under #59.
A declaration is not an OS sandbox, and `available` means only that the static
manifest is valid, **not** that the model can be initialized or inference works.

This is a partial #59 change. It does not yet implement versioned side-by-side
community packages, runtime approval/grants, trusted publisher verification,
per-plugin probe subprocesses or Rust-side typed consumers of the SDK
model DTOs. In the follow-on stage, `list_models` and `verify_install` are
optional, explicitly typed SDK operations; absent methods fail instead of
returning synthetic catalog entries/readiness. Consumers must not interpret
`verified: true` as inference readiness. Consumer wiring in AI Hub (#37) is
separate. No new data schema
version or storage migration is introduced.

Rust `SpeechModelCatalog` also scans one-level `builtin/` and `community/`
subdirectories in supplied plugin roots, without importing Python. Manifests
must declare a supported API and a regular entrypoint; community publisher,
version, and permissions are inspected statically. Duplicate IDs within a
single configured root, broken manifests, missing entrypoints and unapproved
permissions are reported in `SpeechInventory.errors` and excluded from dynamic
speech choices. The read-only inventory never equates a discovered plugin
with an installed environment or verified model revision.

This is catalog parity, **not activation parity**: the current
`InstalledSpeechEnvironment` still selects a single configured executable
plugin root (`AIData/Scripts` or bundled plugins), so community plugins found
only in `AIData/plugins` are not launchable until the root-selection boundary
is unified. That requires a subsequent coordinated integration change rather
than a second implicit plugin path resolver or a fake "ready" state.
