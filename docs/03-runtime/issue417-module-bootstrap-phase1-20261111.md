# Issue #417 — module declaration and physical Hardware foundations

Base: `7e055da3eadca750dbe835d98fbceb20433c7d57` on `codex/abcd-integration-20261110`.

This is the **first implemented slice**, not the complete installation/worker architecture requested by #417. Source of truth: `future/model-module-installation-contract.md`.

## Implemented

- `ai_runtime::module_contract` provides typed `ModuleId`, `ModelId`, `HardwareId` and `ExecutionVariantId`. Invalid IDs are rejected on construction/deserialization.
- `ModelModuleDeclaration` is derived from the module's existing `plugin.json`, not a duplicated GUI catalog. It carries a SHA-256 fingerprint of the actual manifest, a service type, declared models and bootstrap requirements. No Python, SDK import, inference executable, model weights or GPU runtime is executed during discovery.
- Python plugin bootstrap is limited to the managed Python 3.12 + pinned SDK contract. It is **not** a package/weights plan. Existing plugin manifests without an explicit bootstrap retain this legacy host requirement; no model is fabricated for modules which have not declared one.
- The built-in mock TTS/ASR plugins now explicitly declare the model IDs/revisions they expose and their host bootstrap requirement.
- `SpeechModelCatalog::module_declarations` uses the same manifests and plugin source validation as the pre-existing speech catalog. `snapshot()` derives legacy speech entries from those declarations; there is no separate source list. The catalog also reports declarative modules for other known service kinds, without asserting install readiness.
- An invalid installed module cannot silently fall through to an identically named embedded module. Duplicate IDs and unsafe/unapproved executable plugin sources remain rejected.
- `HardwareInventory` binds a physical GPU by the OS DXGI LUID instead of CUDA/DirectML ordinal, using a typed vendor value; CPU is represented separately. A CUDA ordinal change does not change hardware identity. This inventory does not decide which model/runtime is compatible.
- `ExecutionConfiguration` and `ExecutionVariant` are typed data contracts. Module-dependent rules remain outside the generic inventory/registry.

## Boundaries not implemented yet

- The embedded declaration source is currently Python plugin manifests; pre-install native modules still require a **signed declarative module contract** that is separately authenticated from their inference binaries. A signed release digest alone is not a model's capability/installation plan.
- Most existing Python modules lack an explicit list of individual models in their manifests. These modules are discoverable but the new read model does **not invent** their model IDs. Migration must be done at the owning module source, not by a universal resolver.
- `ModuleBootstrap` and `ExecutionConfiguration` describe data but do not yet drive a single cross-implementation `InstallationOwner`, do not bootstrap host components and do not start workers. The existing Python and native installer/lifecycle owners still need consolidation.
- No full module-owned compatibility checker or detailed install plan is attached to a selected `HardwareId` and variant yet. No fallback is attempted.
- AI Hub presenters still need typed model+hardware+variant selection and a single installation progress/cancellation projection.
- Per-service reconfiguration and physical subprocess-generation identity still require separate runtime integration. Readiness must be proven by a live session and never by file presence.

## Validation

- The new unit tests cover independent same-named models, bad bootstrap/duplicate model rejection, stable physical GPU identity across CUDA ordinal changes, unknown hardware, clean-profile model declaration and fail-closed installed module validation.
- CI/main acceptance is not claimed by this phase. In particular, Windows physical inventory, real signed native modules, full GUI and end-to-end host/installation cancellation remain unverified.

## Next vertical slice

1. Introduce a signed native declaration contract with immutable module/model IDs and a lightweight independently executable planning endpoint (or direct in-process implementation for embedded modules).
2. Make a single Core `InstallationOwner` orchestrate host bootstrap → module-owned plan → component execution → receipt validation with cancellation and observable transitions. Retain specialized executors, not competing installation coordinators.
3. Pin module manifest digest, model revision, `HardwareId`, chosen execution variant, settings and component plan revision at admission; reject changed selections, unsupported hardware and stale plans without fallback.
4. Bind the selected Hardware to the actual `ModelSession`; only verified readiness enables service publication. Reconfigure one service without changing sibling generations.
