# Portable diagnostics bootstrap (Agent D, D4)

CLI and Slint GUI call the same `neuromita::diagnostics::init()` bootstrap. At the composition boundary, attach the chosen `PortableDataRoot`; persistent session logs live in `UserData/logs` next to the executable or explicit absolute test data directory. There is no AppData/XDG/current-working-directory fallback. Unwritable data roots are startup errors.

The stderr tracing layer obeys `RUST_LOG` (default `warn,neuromita=info`). The persistent log has an independent narrow event filter: only explicitly emitted `neuromita::diagnostics` lifecycle boundary records at INFO are accepted; arbitrary prompt/document/tool/audio content is not copied into it. Only start/finish/status labels are recorded, not exception contents. Files use exclusive creation, bounded 256 KiB per session, and at most five recently named `session-*.log` files; other user files are never rotated. Unix logs are created mode 0600, directory symlink roots are rejected.

No promise of crash-time fsync, secure log export or Windows E2E certification. GUI diagnostics attach before onboarding; launcher and settings subcommands report start/failure/finish through the same root. Tests use injected temporary roots. Application Timeline events are separate from these technical boundary logs.
