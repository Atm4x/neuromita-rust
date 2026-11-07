# C1 — real managed Python SDK and dynamic plugin acceptance

This contract extends C0's real uv-managed CPython acceptance without changing the installed model lifecycle or allowing GUI Remove.

The opt-in integration test `tests/ai_managed_plugin_sdk_live.rs` creates an isolated `AIData` and uses a pinned `uv 0.10.0` executable to install actual managed CPython 3.12.10. Unlike the ordinary mock-worker tests, its Rust `AiSupervisor` and isolated install-plan requests execute `python/neuromita_ai_sdk/host.py` with the downloaded interpreter.

The test stages a previously unknown, versioned, permissionless `AIData/plugins/community/community_voice` TTS plugin and the shipped dependency-free `mock_asr` builtin. It also stages a corrupt SDK API-version manifest and a permissioned community plugin with an import-time exception. The host must discover the healthy entries despite those failures, classify the corrupt plugin as invalid and the permissioned one as requiring approval, and must refuse loading either rejected entry. No Rust plugin registry update is needed.

For the community plugin, the isolated install-contract path must return a typed `PluginInstallPlan` and a validated settings schema and probe. The test checks that the live shared worker has no loaded models after handshake, rejects invalid settings and invalid plugin initialization, initializes selected community TTS and builtin ASR separately, performs both inference calls, observes `model_ready` only after successful initialization, and retires the worker on stop. This is mock-model inference with real CPython and real SDK IPC, **not** proof that production weights or the chosen backend have been installed. It does not grant an uninstall permission.

Run on a network-enabled test machine with a trusted external uv 0.10.0 executable:

`NEUROMITA_REAL_UV=/path/to/uv cargo test --locked --offline --test ai_managed_plugin_sdk_live -- --ignored --nocapture`

The CI uses `astral-sh/setup-uv` pinned to version 0.10.0 on Linux and Windows, then checks Rust 1.98.1 with fail-fast exit codes. Its archive SHA256 is computed from that runner-provided executable to test integrity mechanics; production trust still requires a separately authenticated release digest. Regular offline unit tests skip this network-dependent integration.

Acceptance remains distinct from: complete Python source-revision parity; production CUDA/DirectML package availability; sandbox guarantees beyond manifest approval; source-pinned community distribution; C3 destructive uninstall, and C4 UI readiness. These conditions cannot be inferred from a successful `hello`, plugin discovery or mock-model inference.
