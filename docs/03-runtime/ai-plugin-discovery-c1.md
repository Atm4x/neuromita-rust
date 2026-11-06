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
per-plugin probe subprocesses, or typed `list_models` and `verify_install` SDK
operations. Consumer wiring in AI Hub (#37) is separate. No new data schema
version or storage migration is introduced.
