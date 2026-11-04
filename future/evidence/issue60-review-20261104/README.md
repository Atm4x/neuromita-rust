# #60: повторный A/B после исправления оценщика

Дата: 2026-11-04. Обновление upstream/main: `095a4e71`.
Эти JSON получены новым `examples/vad_ab.rs` из этой ветки. Порог всех провайдеров
зафиксирован на 0.5; calibration и heldout сохранены, подбора порога не было.

- [direct.json](direct.json): прямой Earshot.
- [wavekat.json](wavekat.json): Earshot через wavekat-vad; совпадает с direct.
- [silero.json](silero.json): настоящий inference Silero на ONNX Runtime 1.24.2.

SHA-256 входного `assets/eval/manifest.json`:
`a75817404ff8eb2a1b37ddc982c4c8b785a9cf626590ecb8b3fdefb1b2a7d3f9`.
После ручной проверки этот вход сохранён как `assets/eval/manifest-heuristic-v2.json`;
для воспроизведения данного исторического прогона передавайте архивный путь вместо
обновлённого `manifest.json`. Итоговый ручной прогон — в соседнем
`issue60-manual-20261104/`.
Все 18 WAV сохранены без изменений. Происхождение, лицензии и SHA-256 каждого WAV
указаны в `assets/eval/sources.json`. Модель и runtime, их размер и SHA-256 описаны
в `future/issue-60-review-fixes-20261104.md`.

В каждом отчёте 12 клипов со статусом heuristic и 6 unknown. На unknown качество
не вычисляется: metrics, boundary и endpoint равны null; emitted_segments остаются
доступными. Проверены количество клипов, статусы, покрытие сэмплов и совпадение
direct/wavekat. Сырые сегменты на unknown: Earshot 9 / 10.128 с, Silero 1 / 0.768 с.
Это не подсчёт доказанных ложных реплик: клипы ещё не прослушаны вручную.

Границы PCM, решение onset и выдача после тишины теперь различаются. Timing means
исключают неоднозначные merge/split, EOF выделен отдельно; null не означает нулевую
ошибку. Это время потока сэмплов, а не wall-clock стоимость inference или ASR.
Старые метрики в `future/issue-60-ab-report-20261104.md` имеют другую семантику и
сохраняются как исторический эксперимент; напрямую сравнивать их нельзя.

## Воспроизведение

Из корня worktree, с явными абсолютными путями к проверенным локальным модели и DLL:

```powershell
$env:SILERO_MODEL_PATH = 'C:/absolute/path/silero_vad.onnx'
$env:NEUROMITA_SILERO_MODEL = $env:SILERO_MODEL_PATH
$env:NEUROMITA_ORT_DLL = 'C:/absolute/path/onnxruntime.dll'
cargo build --locked --offline --features vad-silero --example vad_ab
cargo run --locked --offline --features vad-silero --example vad_ab -- assets/eval/manifest.json direct 0.5
cargo run --locked --offline --features vad-silero --example vad_ab -- assets/eval/manifest.json wavekat 0.5
cargo run --locked --offline --features vad-silero --example vad_ab -- assets/eval/manifest.json silero 0.5
```

В этом прогоне все три запуска завершились с кодом 0. Запуск direct с дополнительным
`--validated-only` завершился ожидаемым кодом 1 и перечислил все 18 непроверенных
клипов. Итоговый прогон после ручной разметки должен использовать строгий режим.
Порядок разметки — в `future/issue-60-manual-annotation.md`.

Эти результаты не закрывают #60 и не устанавливают победителя backend. Остаются
ручная проверка корпуса, повторный A/B на подтверждённых метках, lifecycle/интеграция
и последняя микрофонная приёмка.
