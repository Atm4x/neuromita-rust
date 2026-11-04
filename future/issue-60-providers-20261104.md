# Issue 60: optional wavekat Earshot and Silero providers

Инкремент после `dc888e4a`. Подключает реальные крейты за фичами и не меняет
default-сборку.

## Что добавлено

- `vad-wavekat` активирует `WavekatEarshotScores` (окно 256 сэмплов).
- `vad-silero` активирует `SileroScores` (окно 512 сэмплов) и
  `init_onnx_runtime(path)` — инициализацию ONNX Runtime из одного явного
  абсолютного пути, без поиска библиотеки и без загрузок.
- `SileroScores::from_verified` принимает только `VerifiedSileroAsset`, то есть
  модель уже проверена по размеру и SHA-256.
- `Cargo.toml`/`Cargo.lock` и vendor обновлены (`wavekat-vad 0.1.17`,
  `ort 2.0.0-rc.12`, 30 новых пакетов).

## Проверки

- `cargo test --features vad-silero --lib audio::score` — 9 passed, 1 ignored.
  `wavekat_earshot_scores_same_frames_as_direct_detector` подтверждает, что
  волкат-обёртка даёт те же вероятности, что прямой Earshot на тестовых кадрах.
- `onnx_runtime_init_requires_absolute_existing_library` — относительный путь и
  отсутствующий файл отклоняются до создания сессии.
- Живой тест `live_silero_scores_silence_and_tone` (по умолчанию ignored, нужны
  `NEUROMITA_SILERO_MODEL` и `NEUROMITA_ORT_DLL`): запущен вручную, 1 passed,
  тишина ниже 0.2, тон уверенно выше тишины, 0.25 с.
- `cargo clippy --features vad-silero --all-targets -- -D warnings` — PASS,
  `cargo fmt --all -- --check` — PASS.

## Границы

- Ни модель, ни ONNX Runtime DLL в репозиторий не входят: обе задаются путями.
- Качество на настоящей речи не измерено — это следующий шаг с корпусом.
- `pem-rfc7468` в vendor содержит публичную тестовую фикстуру
  `tests/examples/ssh-id_ed25519.pem`, на которую срабатывает GitHub push
  protection. Push разблокирован владельцем репозитория; при регенерации vendor
  блокировка появится снова.
