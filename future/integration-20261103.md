# Объединение разработанных веток, 2026-11-03

По запросу пользователя все origin-ветки после fetch объединены в `codex/slint-gui-20261103` для единого PR в main. Использованы обычные merge commits с сохранением истории. Перед интеграцией рабочая папка была чистой, GUI/localization находились на `99ad9c83`, main — на `783928b5`.

## Основные линии

| Ветка / ревизия после fetch | Результат |
|---|---|
| `codex/slint-gui-20261103` / `99ad9c83` | Slint shell/Home, макетные страницы и меню, JSON-локализация и живые bindings |
| `codex/ai-python-plugin-architecture-20261103` / `6083e5be` | Python SDK, Rust supervisor и IPC, bootstrap uv/CPython, package layers/overlays, resolver и проверка install contract, staged activation |
| `codex/character-memory-presenter-20261103` / `4a0cec47` | Вся последовательная цепочка semantic retrieval → provenance/benchmark → fusion → knowledge boundary → lifecycle/islands → protection/maintenance → recall/expiry → safe purge → Character memory/presenter |
| `codex/request-provisional-contract-20261102` / `e94b4535` | Предварительный текст и стадии acceptance в наблюдении запросов и сетевых ответах |
| `codex/preset-resolve-at-execution-20261102` / `73a118de` | Контракт однократного выбора пресета при исполнении; сохранена более новая редакция с prepared knowledge и отдельным каталогом |
| `codex/unified-reply-unity-directed-20261101` / `c366cae0` | Инвариант непосредственного наблюдения; сохранена редакция, уже описывающая его реализацию |

Промежуточные origin-ветки памяти и semantic retrieval включены через последнюю вершину этой цепочки. Остальные старые функциональные ветки уже являлись предками объединяемой истории.

## Служебные ветки

Включены предки `bridge/request-semantic-publish-20261102` (`1c530f56`), `bridge/sometests-bundle-20261102` (`24ab511e`) и `bridge/timeline-audit-source-20261101` (`41fc6ec7`). Их изменения проверены: это транспорт Git-патчей/архивов, а не функциональность приложения.

В итоговом дереве отсутствуют временные `bridge-verified-patches-20261102.yml`, `temporary-sometests-bundle.yml`, `timeline-source-export.yml` и `.github/bridge/patches.b64`. Они сохраняются в истории исходных веток. Рабочий main не получает команды повторной публикации старых патчей и экспорта bundle.

## Разрешённые конфликты и исправления интеграции

- `src/lib.rs`: объединены экспорты AI runtime, adapters, knowledge, localization и maintenance. Сохранён перенос домена памяти в Character с совместимым публичным alias.
- `GOAL.md`: сохранены обе линии планов — AI runtime и semantic retrieval.
- Документация пресетов и наблюдений: новые действующие контракты сохранены поверх ранних редакций, описывавших ещё не реализованные изменения.
- Cargo: удалён дублирующий default-run, автоматически добавленный из двух веток; сохранены GUI features и новые AI dependencies.
- Windows fixtures: поиск заменяемой Python-строки не зависит от CRLF/LF; test interpreter учитывает Windows. Производственный выбор managed Python не изменён.
- Unix-only helpers ограничены соответствующим cfg, устранены предупреждения Windows; добавлены правила для LF Python и игнорирования __pycache__.
- Новые Rust-файлы приведены к rustfmt без изменения поведения.

## Проверка и границы

На Windows прошёл общий набор с GUI: 321 passed, 2 ignored. Python SDK: 5 passed. Проверки default/no-default, Clippy и итоговая сборка отражаются в актуальном HANDOFF после завершения прогона.

Живые Gemini/embedding benchmarks, загрузка настоящих GPU-пакетов/моделей и production inference этим объединением не проверяются. GUI остаётся самостоятельным preview, не запускает AI Supervisor автоматически. Часть bootstrap/archive fixtures исходных веток работает только на Unix и не считается проверенной этим Windows-прогоном.

В итоговом дереве нет постоянного GitHub Actions workflow: временные transport jobs не являются CI приложения. Локальные результаты не выдаются за проверку Linux или облачный CI.
