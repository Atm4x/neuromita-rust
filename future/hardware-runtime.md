# #51: выбор устройства и состояние AI backend

**Статус: WIP, не проверено, не принято.** Worktree `.worktrees/issue-51-current`, ветка
`codex/issue-51-hardware-current`, база `upstream/main` `6c7e88ca`. Код не коммитился и не
публиковался. Ни один Cargo/Python-тест, Clippy или rustfmt по этому worktree не запускались:
сборка и исправления не выполнялись.

Референс поведения — read-only `Atm4x_NeuroMita`: `core/voice_device_selection.py`,
`core/cuda_precision_policy.py`, `utils/gpu_utils.py`, `services/hardware_inventory_service.py`,
`handlers/ai_engine/worker_process.py` (`_probe_runtime_capabilities`),
`core/backends/service.py`.

## Архитектура

- Rust владеет пользовательским выбором, конфигом и lifecycle worker'ов. Python остаётся
  платформой introspection/inference через существующий SDK/host; отдельного service locator
  или скрытых fallback-пресетов нет.
- `src/ai_runtime/hardware.rs`:
  - `HardwareSelection { backend, backend_revision, device, precision, service, plugin_id, settings }`;
  - `HardwareConfig { schema_version, revision, selection }` в атомарном JSON-store;
  - проверка комбинаций до запуска: `cpu` только для `*_cpu`, `cuda:N` для `*_cuda`,
    `dml:N` для `onnx_directml`, канонические идентификаторы без алиасов;
  - `HardwareRuntime::inspect` — одноразовый worker на установленном backend-слое;
  - `HardwareRuntime::apply` — загрузка установленного слоя
    `<AIData>/environments/bases/<backend>/<revision>/.neuromita-backend.json`, composition,
    `hardware` + `execute` + `initialize`, публикация только при неизменной revision конфига.
- Worker protocol: `hardware` (инвентаризация), `execute` (реальная операция на устройстве),
  `status` (загруженная модель + runtime-эхо). `initialize` получает `runtime`-профиль; host
  проверяет его по реальной интроспекции и подставляет `device` только если схема плагина его
  объявляет; иное значение устройства — ошибка.
- Readiness = успешный `model_ready` от загруженной модели плюс подтверждённый runtime-профиль.
  Наличие CUDA/драйвера/файлов или `discovery_status=available` готовностью не считается.
- Смена выбора готовит кандидата; при ошибке или изменившейся revision (stale switch) рабочий
  worker сохраняется. `call` после смены поколения возвращает `StaleGeneration`.
- Python SDK: `python/neuromita_ai_sdk/hardware.py` — DXGI-адаптеры, CUDA driver API (ordinals,
  compute capability, driver), torch, onnxruntime providers, `torch_directml`-порядок DML,
  порт tensor-core FP16-политики, execution-probe.
- CLI: `neuromita hardware inspect|select|apply` (терминал как пользовательский путь).

## Изменённые пути (WIP)

- `src/ai_runtime/hardware.rs` — новый модуль selection/config/inspect/apply/CLI.
- `src/ai_runtime/mod.rs` — `PythonHost` (module/script), `AiService` serde, типизированные
  ошибки устройства/модели/stale, `activate_hardware`, `inspect_layers`, `hardware_probe`,
  `readiness`, проверка профиля и gate публикации.
- `src/ai_runtime/backend.rs` — `BackendKind::as_str/parse`, публичный `validate`,
  `load_installed_backend` (read-only чтение маркера установленного слоя).
- `src/ai_runtime/layer.rs` — `LayerError::NotInstalled`.
- `src/ai_runtime/composition.rs` — `RuntimeComposition::for_single_service`.
- `src/main.rs` — ветка `hardware` до инициализации приложения.
- `python/neuromita_ai_sdk/hardware.py` — новая интроспекция/probe.
- `python/neuromita_ai_sdk/host.py` — операции `hardware`/`execute`/`status`, runtime-профиль
  в `initialize`, проверка конфликта `device`.
- `tests/fixtures/ai_hardware_fixture_host.py` — управляемый локальный worker для будущих
  тестов; production путь его не использует.

## Осталось до приёмки

- Rust integration-тесты на fixture-worker: hardware absent, invalid device, execution probe
  failure, worker failed load, stale switch/активация, in-flight `StaleGeneration`, persistence.
- Python unit-тесты политики FP16 и формы probe; отдельная проверка на машине без GPU.
- Компиляция и исправление ошибок; `cargo test --locked --offline`,
  `cargo clippy --locked --offline --all-targets -- -D warnings`, `cargo fmt --all -- --check`,
  `git diff --check`; Python unittest.
- Документация пользовательского пути и точных границ (установка backend-слоя вне #51;
  одноразовый `apply`; ONNX execution-probe требует пакет `onnx`, иначе fail closed).
- Независимое ревью, исправления и перевод draft PR в готовый к review. До этого issue не считается выполненным.
