# C0 — genuine uv / managed CPython acceptance

This is the *real executable* counterpart to the scripted bootstrap tests. The default fast suites use synthetic uv and Python wrappers to check failure paths deterministically; those tests alone cannot establish that the current uv CLI can install an isolated CPython 3.12 distribution.

Run the opt-in integration test on Windows and Linux with a verified real uv executable: `NEUROMITA_REAL_UV=/absolute/path/to/uv cargo test --locked --test ai_managed_bootstrap_live -- --ignored --nocapture`. The executable must be a supported uv release, not a shell stub. CI uses pinned `uv 0.10.0` via `astral-sh/setup-uv`.

The test stages those exact real uv bytes in a platform-specific local archive, computes the SHA-256 *for that test fixture*, and passes the archive through the same `UvInstaller` verification/extraction/receipt path used by production. `ManagedRuntimeBootstrap` then downloads and installs real managed CPython `3.12.10` under a fresh `AIData/bootstrap/python` directory, verifies the exact interpreter version, and launches isolated CPython to import `ssl` and report its implementation and version. It repeats `prepare` to prove deterministic reuse, asserts that no `.venv` exists, corrupts the installed uv executable and requires a fail-closed `Corrupted` probe.

This is not a signature or supply-chain proof for any deployed uv distribution: the CI fixture hash is computed from the already downloaded executable. Production must receive an independently trusted uv release digest, and real-model/GPU inference is outside C0. A CI skip does not constitute passing C0; the job must explicitly use `--ignored` with `NEUROMITA_REAL_UV` set. Real CPython download is network-dependent and therefore deliberately not part of the standard offline unit suite.

C0 acceptance consists of existing scripted negative-path tests plus successful genuine uv and Python 3.12.10 installation on *both* Windows and Linux. C1 dynamic plugin SDK discovery remains a separate gate and cannot be inferred from this test.

## Real uv 0.10.0 compatibility defect found and repaired

The initial live runs on both Linux and Windows caught a production bug: `PythonBootstrap::uv_command` set `UV_PYTHON_PREFERENCE=only-managed`, while `locate` additionally passed `--managed-python`. uv 0.10.0 rejects those mutually exclusive preference selectors and exited 2 even though CPython 3.12.10 had been installed. The compatibility fix retains the single environment-based preference and removes the redundant CLI option. The live integration test ensures the genuine uv executable now returns the installed managed interpreter rather than treating a CLI argument error as `PythonMissing`.
