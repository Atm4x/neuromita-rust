# Issue 60: bounded drain after capture ends

Внешнее ревью отмечало: после EOF capture код ждал worker без ограничения, и
зависший ASR или chat мог держать shutdown вечно.

## Изменение

- `DRAIN_TIMEOUT = 30 s`. После завершения capture ожидание dispatch идёт через
  `tokio::select!` с дедлайном; по истечении задача отменяется через
  `AbortHandle`, а сессия возвращает `SpeechSessionError::Task` с текстом про
  drain deadline.
- Штатный путь завершения сохранён: ошибки capture, VAD и dispatch по-прежнему
  проходят через общий выход из `run()`.
- `tokio` добавлен в `dev-dependencies` с фичей `test-util` только для тестов с
  замороженным временем; runtime-набор фич не менялся.

## Проверка

`stalled_asr_cannot_hold_shutdown_open` — транскрайбер, который никогда не
завершается, сегмент уходит в очередь, capture заканчивается; тест с
`start_paused` подтверждает, что сессия выходит по дедлайну, а не висит.

`cargo test --lib audio::session` — 7 passed; `--lib audio::` — 28 passed;
clippy `--all-targets -D warnings` и fmt чистые.

## Границы

Дедлайн фиксированный (30 с) и не настраивается из настроек. Этого достаточно для
локального shutdown; отдельный конфиг появится, если понадобится другое поведение.
