# B73 — paged read-only Regenerate recovery diagnostics

`SessionTimeline::enumerate_regenerate_admissions(character)` now traverses the
B72 typed keyset pager (`limit = 128`) rather than asking the repository for an
unbounded list of child markers. This is a compatibility method returning a
complete `Vec<RegenerateRecoverySnapshot>`: **its output memory still grows
with the number of admissions**. Consumers requiring bounded output should
call `enumerate_regenerate_admissions_page` and process one page at a time.

Both methods share the same canonical validation, session/recipient scoping,
request-id uniqueness check across other pages/characters, request-local
lifecycle classification and read-only semantics. They never allocate a queue
permit, update Timeline or authorize replay of a possibly dispatched provider.
A cursor binds the session, character, last child UUID, fact revision and
persisted active-selection generation. Any detected concurrent append or
A→B→A transition invalidates the continuation, so the compatibility method
returns an error rather than combining observed pages from different epochs.
This is a conservative fence, not a multi-statement SQLite snapshot held open
across the entire scan. A writer can act after the final page completes.

The regression suite includes a 130-admission transient case and a native SQLite
130-admission case (including reopen), as well as B72 privacy, duplicate,
archive, cursor, and cross-owner races. The transient source still filters
in-memory branch metadata and is not O(page-size) in scan cost.

## Reproducible discovery-only benchmark

Run `python3 tools/benchmarks/regenerate_recovery_sql.py`. It creates a
synthetic SQLite dataset with 6,000 branch markers and 40,000 unrelated events,
then compares the previous unbounded marker query with complete 128-record
keyset traversal. Both strategies return and validate exactly the same 3,000
matching branch ids. It reports 60 paired p50/p95 timings, SQLite query plans
and Python `tracemalloc` peak allocations for each strategy. The indexed query
uses `timeline_regenerate_markers_by_character`; the old one scans all session
child-branch markers. Unlike B72's first-page comparison, this compares equal
output cardinality.

This is **not** an end-to-end Rust `SessionTimeline` benchmark: it omits receipt
validation, request-local indexed reads, WAL contention, cold cache, native
SQLite allocations and Rust RSS. Python `tracemalloc` is not an OS memory peak.
It cannot justify an end-to-end speedup claim or a memory-usage SLA. Realistic
full-session Rust profiling and externally authenticated cursor contracts remain
separate work.
