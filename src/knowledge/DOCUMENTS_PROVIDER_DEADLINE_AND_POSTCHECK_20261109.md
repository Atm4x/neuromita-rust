# Cycle64 — Provider deadline and post-provider Documents source verification

Status: backend-only incremental R7 evidence, not complete user-facing R7 acceptance.

## Owner boundaries

`ResponseGenerator` owns the provider-neutral generation deadline. The timeout comes from the **admitted immutable preset** (`GeminiTemplate.settings().timeout()` / `OpenAiTemplate.timeout()`), not from a mutable global timer. A single absolute `tokio::time::Instant` deadline applies to all provider turns **and supported tool execution** of a request; custom trusted `GenerationProvider` implementations cannot ignore the configured deadline merely by omitting their own timer. The built-in provider HTTP timers continue to bound transport separately. Deadline expiry yields `GenerationError::DeadlineExceeded` and diagnostic `FailureStage::ProviderTimeout`, distinct from `GenerationError::Transport` and an explicitly cancelled `RequestState::Cancelled`. The unfinished provider future is dropped; acceptance of any eventual provider reply is prevented. This is cooperative future cancellation, **not** a guarantee that an uncooperative worker process or synchronous operation is forcibly terminated.

The `MessageHandler` owns cross-boundary admission and final acceptance. With opt-in Documents enabled, the already admitted character-scoped `root/index` is verified by `document_index::search_verified` before the LLM invocation. Once a proposed LLM reply returns, `document_index::load_verified` independently revalidates the same pinned root/index and authoritative source bytes **before** `record.text_ready`, voice generation, effects or Timeline acceptance. Source mutation, deletion, index corruption or foreign index at this boundary causes terminal `TextGenerationFailed(GenerationError::Response(_))`; the prompt cannot silently use a stale excerpt. The check is performed on a blocking thread, with the existing request cancellation and lifecycle stop selection remaining active. It does not create a second index, change `SettingsApi`, reindex automatically or use a fallback source.

## Deterministic executable evidence

`tests/rag_documents_restart.rs`:

- `documents_generation_automatic_deadline_rejects_late_provider_result`: an intentionally pending controlled provider with a short admitted preset deadline is terminated by the **generation engine's own timer**, not by a `RequestHandle::cancel` call. A late reply is rejected, the persisted revision is unchanged, and a subsequent request succeeds with the same verified Documents source.
- `documents_changed_during_provider_reply_are_rejected_before_acceptance`: the provider receives source-attributed verified evidence, the authoritative file changes while the provider is waiting, and the provider returns a plausible answer. The post-provider check rejects it without acceptance; the settings revision remains selected and a new explicitly built/CAS-activated revision restores generation with the updated content.

Existing Cycle63 tests independently cover explicit cancellation, transport failure and settings-write failure with explicit retry. The new tests do **not** claim a Python subprocess hard kill, a user-interface timer, real model endpoint behavior or an all-provider/tool-process crash guarantee.

## Remaining scope

The two source checks are optimistic; a concurrent process can alter source bytes after the last successful verification or perform an ABA rewrite. No application-wide writer lock or signed provenance is established. The generation deadline bounds provider turns and built-in tool futures, but not the entire prompt preparation, non-provider retrieval or external subprocess lifetime. Custom `GenerationProvider` futures must yield for Tokio to enforce the timer. Source-attributed `.md/.txt` lexical Documents remains the only supported opt-in source; PDF/Office, semantic Documents, GUI indexing and complete R7 failure acceptance are still absent.

**R6/R7 remain OPEN. Formal acceptance remains 16 PASS / 4 PARTIAL / 6 OPEN.**
