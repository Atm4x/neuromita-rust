# ABCD Core — semantic service worker ownership (iteration 6)

Base: `f0fe30ffe50919b7a3487e6e86cfbbdf4ed3ec0a`.

## Model identity and process ownership

`ServiceWorkerRegistry` is the application-wide owner of one model per `AiService` (TTS, ASR, Embedding, Reranker, Beats). The immutable identity contains the implementation (`Python` or `Rust`), the model ID, and the pinned revision. It is not inferred from a user-visible engine switch. An equally named model built using a different implementation has a different identity.

Product application startup requires a **split** Python process topology. The historical lower-level shared topology remains available for its existing diagnostics/compatibility tests but is rejected by `ApplicationLifecycle`; it is not the product execution model.

`ApplicationLifecycle` claims Embedding, Reranker and Beats slots from the *prepared Python worker plan before spawning* the processes. After activation it compares the actual loaded model/revision with each claim and rejects mismatches. On failed optional automatic activation, provisional claims are released. The existing TTS/ASR ownership remains in the speech owner. All prepared claims stay owned through application shutdown.

## Semantic operation ports

`ServiceEmbeddingProvider` implements the existing `EmbeddingProvider` contract and `ServiceRerankProvider` implements `RerankProvider`. Both are constructed with an immutable `ServiceModel`, the shared registry, and a provider implementation. No Python/Rust conditional is present in these ports. The currently installed implementation is the existing Python provider; supplying a genuine Rust implementation requires its own verified manifest and typed IPC contract, not an inferred model name.

An Embedding query or **entire embedding-document batch** obtains one service read permit. A reranker request obtains an independent Reranker read permit. The port checks active ownership **before invoking any provider method** and after successful completion, returning an explicit error if the model has changed. Transition locks exclude in-flight service operations but do not block independent services.

The Core's explicit recovery entry point obtains the corresponding exclusive transition permit before restarting a failed prepared worker. There is no automatic fallback to a different model or implementation.

## Verification and exclusions

The new port tests check service isolation, stale/mismatched model rejection before any external call, batch-level permit lifetime, and deferral of even an eager provider method until the transition gate releases. The whole Rust library suite and existing native process/registry/revision/speech IPC suites should be rerun after this change.

**Not yet guaranteed:** Cross-process SQLite writes against Timeline cannot be serialized merely by checking the in-process service worker registry. `TimelineTurn` freshness validation does not atomically constrain an arbitrary external writer committing between validation and network provider IO. Full correctness needs a database-level writer/admission contract and concurrent-process tests; no such guarantee is claimed here. Likewise, no native Embedding/Reranker binary protocol, unified Slint model catalog, real model/Windows acceptance or complete GUI Clippy acceptance is implied. The historical shared supervisor remains implemented below the product boundary and may be migrated or removed separately after its consumers are audited.
