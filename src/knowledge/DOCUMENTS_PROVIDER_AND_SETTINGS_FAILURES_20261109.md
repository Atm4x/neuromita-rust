# Agent A Cycle63 — Documents generation faults and settings durability

## Ownership

The trusted, character-scoped Documents index is still a derived view of authoritative `.md`/`.txt` files. Active index selection is owned exclusively by `SettingsApi` and captured at request admission; `DocumentRevisionApi` only performs verified preparation and explicit CAS updates. The LLM provider owns only its request/response. Neither a provider failure nor cancellation authorizes reindex, fallback, selection mutation, or rewriting source files.

## Controlled-provider process evidence

`tests/rag_documents_restart.rs::document_provider_failure_and_explicit_cancellation_do_not_change_selection` uses the real `ApplicationLifecycle`, two independently selected character indexes, and a controlled provider. Dropping a pending provider response simulates a genuine transport disconnect: the request ends as `TextGenerationFailed(Transport)`, while another character still generates with its own provenance. A second in-flight call is explicitly cancelled; after terminal `Cancelled`, an attempted late provider reply is rejected. A subsequent fresh request for the original character succeeds using the same selected index. The stored settings revision is unchanged. This is **not** a claim that the arbitrary controlled provider enforces an automatic timeout; per-provider timeout policies remain separate.

## Persistence failure and retry

`failed_settings_save_needs_explicit_retry_before_restart_keeps_document_selection` injects a deterministic repository write failure *after* the initial settings state is durably saved. `DocumentRevisionApi::activate` succeeds as in-memory CAS, but `SettingsApi::flush` returns `SettingsError::Storage`; the previous on-disk revision remains authoritative for restart. The active in-memory source can still generate, and inspection reports `Ready`, but neither result implies durable selection. Clearing the failure and explicitly invoking `SettingsApi::retry_save` followed by a successful `flush` persists the selected source. A shutdown and `SettingsSource::Stored` restart reopens that same revision and successfully uses the verified document evidence.

These tests exercise real async cancellation, persistence acknowledgments, on-disk file ownership, provider-invocation boundaries, and restart. They do not simulate process kill or power loss, provide OS-level locks, implement automatic retry or timeout in the provider, add user-facing controls, or prove PDF/Office/semantic Documents parity. Test binaries containing an `ApplicationLifecycle` singleton must run with `--test-threads=1`.

**Formal R6/R7 remain OPEN; total 16 PASS / 4 PARTIAL / 6 OPEN.**
