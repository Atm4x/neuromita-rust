# B74 — native SQLite Regenerate recovery benchmark

The ignored Rust test `timeline::regenerate_enumeration_tests::b74_native_recovery_benchmark` constructs a **real** SQLite `SessionTimeline` from the accepted source and regenerates historical branches through the owner API. It writes durable admission markers, dispatch / abandonment facts, actual accepted replies with `CharacterVariablesChanged` effects, and unrelated player events *before* any recovery timing starts. It then re-reads identical typed `RegenerateRecoverySnapshot` rows via the legacy unbounded branch query and the B72/B73 keyset collector. It does not call the provider and it does not authorize replay.

Run from the repository with Rust 1.98.1 and vendored dependencies:

```
B74_ADMISSIONS=132 B74_EVENTS=512 B74_ITERATIONS=12 \
  cargo test --locked --offline --lib b74_native_recovery_benchmark -- \
  --ignored --nocapture --test-threads=1
```

With its default 132 admissions, the benchmark crosses a 128-row page boundary; admission count, unrelated-event count and iterations are configurable. It prints the counts of each durable lifecycle outcome. The printed `setup_ms` is reported **separately** and excluded from recovery latencies. The two read paths return equal cardinality and identical typed content and are alternated to limit cache-order bias. `p50` and `p95` are measured wall-clock time per complete read, not per SQL statement. Do not compare these results to B73's Python-only SQL microbenchmark as if the latter were an end-to-end Rust timing.

The legacy comparison is benchmark-only: it lacks the B72 cross-page revision/selection fence and is **not** an alternative production implementation. In particular, equivalence is checked in a fixed database without concurrent writers. The existing B72 tests separately verify stale-cursor rejection with another writer and A→B→A.

On Linux the runner samples `/proc/self/status` `VmRSS` at 2-ms intervals and records the greatest observed **process-wide** RSS. It is not an allocation profiler, is not an isolated peak per read strategy, and may under-sample short peaks. The process also includes the test runtime and loaded SQLite state. Any assertion about per-call peak allocations or actual deployed application RSS requires separate instrumentation or a dedicated process-level harness. Cold OS page-cache measurements are **not** asserted by this test; both methods run against warm SQLite caches in one process. `setup_ms` is also affected by the current store's whole-state load on mutations.

No cursor is serialized or exposed externally. Public endpoints would need a separate authenticated/versioned cursor.
