# Rust core handoff

Read HANDOFF.md, then future/implementation-checklist.md and future/minimal-contracts.md.

Work from this repository root so Cargo loads .cargo/config.toml and uses vendor/. Run cargo test --locked --offline, cargo clippy --locked --offline --all-targets -- -D warnings, and cargo fmt --all -- --check.

This is a fresh Rust implementation. Python/Unity sources are read-only context. Follow the user's architecture: static *Api, explicit typed inputs, one core per process, owned CharacterHandler instances, subjective context from the timeline, effects committed before Finished, independent presenters and typed bindings. Do not add silent preset/config fallbacks or a service locator.

This standalone repository has a local main branch. The user will create the remote repository; do not create or configure a remote unless asked. The original Python audit is in docs/ and architecture contracts are in future/. References to audit-docs/rust or codex/rust-core-contracts in historical documents describe the source repository, not this working directory.

Vendor files are third-party sources; preserve their bytes/checksums and licenses. Update dependencies with Cargo and regenerate vendor intentionally. Do not commit target/, credentials, local settings or build archives. Provider tests use local fixtures; the ignored live test needs explicit environment variables and makes a real API call.
