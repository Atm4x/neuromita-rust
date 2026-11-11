# ABCD integration: post-merge runtime boundaries (2026-11-10)

Baseline: `f9c432b179d0aea43a0e321a63a0ff05bb6c589c`. This is an incremental architecture correction, not final sign-off for `main`.

## Addressed

- GameLink session ownership and command execution are serialized by a per-session asynchronous gate, covering owner check, receipt lookup, command dispatch, and receipt publication. `resume` takes the same gate before replacing its socket. The global sessions mutex remains short-lived and is never held across an `await`.
- The desktop Core composition binds GameLink at `127.0.0.1:12345`, matching the documented existing Unity loopback port. The independent administrative server remains on its own address.
- Semantic retrieval binds a Core-owned Timeline authority once. Every query/document embedding dispatch revalidates the Timeline turn, and the result is rechecked after the provider returns. Generation rerank dispatches are guarded. Historical semantic generation carries its captured turn into the scorer instead of using an unversioned cache snapshot.
- Added the first known missing English catalog entry (`memory.graph.confirm`). The whole English localization catalog still requires an independent completeness pass.

## Architecture limits / not ready for main

- AI Hub still holds native model workflow and Python installation approval/task ownership in desktop runtime. Native TTS/ASR routing is not wired through Core. This requires one Core-owned service exposing typed native operations, explicit fallback policy, supervised installation cancellation/join and GUI commands as messages, not another GUI-side adapter. **Not fixed in this commit.**
- A foreign SQLite writer can commit immediately *after* a valid external-dispatch check; absolute prevention requires a shared persistent read/commit lease or a stronger per-request authorization contract carried through all providers. The new fence closes the documented unguarded window between query embedding and document embedding, but does not claim a distributed transaction with a remote provider.
- The desktop game listener uses the conventional fixed port and can fail to bind if another process owns it. A user-configurable endpoint and Unity integration handshake remain to be specified.
- Linux test runs do not substitute for the Windows full-GUI/Unity/native-model acceptance suite.

## Validation

- The restored source was verified by artifact SHA-256 and `git fsck`.
- Rust compiler 1.98.1 from the provided toolchain archive; vendored Cargo dependencies.
- `cargo test --lib --offline`: 861 passed, 0 failed, 2 ignored (Linux).
- GameLink handover gate regression passes.
- Additional Windows GUI/Clippy and native-model integration tests: not claimed.
