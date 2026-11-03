# Dataset для fine-tuning в Rust Core

Это пользовательский сбор пар фактического запроса и ответа провайдера, их оценка, хранение и экспорт. Обучение модели и загрузка датасета в сеть не выполняются.

## Включение и путь данных

Конфигурация хранится в явно указанном JSON-файле. Обычный persistent bootstrap читает `finetune.json` из `--data-dir`, если файл существует; `--finetune-config PATH` выбирает другой файл. Ошибка чтения или проверки останавливает запуск, молчаливого отключения сбора нет.

```powershell
neuromita dataset init --config C:\Data\finetune.json --dir C:\Data\FineTuneData --enabled on --max 50
neuromita dataset config --config C:\Data\finetune.json enable off
neuromita dataset config --config C:\Data\finetune.json max unlimited
neuromita dataset config --config C:\Data\finetune.json dir C:\Other\FineTuneData
```

`init`, `config`, `stats`, `list`, `rate`, `export` и `clear` работают без `GEMINI_API_KEY`, Core и сетевого доступа. `config max` сразу применяет новый лимит к существующему каталогу. Смена каталога не переносит старые файлы автоматически.

Работающий Core использует конфигурацию, загруженную при запуске. Изменения enable/disable, каталога и лимита сбора вступают в силу для Core после его перезапуска; CLI сообщает об этом явно.

## Что сохраняется

Один sample соответствует одному запросу с одним устойчивым ID, равным `RequestId`. Это позволяет сопоставить реплику, запрос и оценку без отдельного изменяемого pending-slot.

Observer провайдера получает фактический подготовленный prompt и точное тело JSON, отправленное провайдеру. `response_received` вызывается сразу после полного тела HTTP-ответа, до декодирования provider DTO и проверки структурированного ответа. В sample сохраняются:

- `request_payload` — точное JSON-тело запроса;
- `provider_response` — точное тело успешного HTTP-ответа;
- `messages` — сообщения, построенные из того же фактического provider-neutral prompt, без повторной сборки контекста;
- `response`/`raw_response` — текст ответа, извлечённый из provider body до application validation;
- модель, провайдер, protocol/dialect, temperature/max tokens и фактический usage, если он присутствует в ответе;
- UTC timestamp, персонаж, `RequestId`, intent и source.

HTTP-ответ с ошибкой, оборванный transport body или тело сверх 8 MiB не создаёт sample. Для частичного ответа observer всё равно получает ограниченный prefix с `complete=false`; это диагностическое событие, но не успешная пара для обучения.

Персистентность выполняется в `spawn_blocking` после получения наблюдения и до принятия сообщения персонажем. Ошибка записи не заменяет и не отменяет provider result. Успешное принятие, ошибка принятия и отмена до принятия записываются отдельно в `outcomes_YYYYMM.jsonl`; отсутствие outcome означает, что финальный исход не был зафиксирован.

## Хранение и конкуренция

Samples лежат в `samples_YYYYMM.jsonl`, outcomes — в `outcomes_YYYYMM.jsonl`. Записи append-only при обычном сборе. Лимит удаляет самые старые валидные записи по месячным файлам, не удаляя нечитаемые или неполные строки. Пустые файлы после полностью удалённого содержимого удаляются.

Совместный доступ Core и отдельного CLI сериализуется блокировкой `.neuromita-finetune.lock` через `File::lock`; lock-файл может оставаться на диске и не является признаком busy. Локальный mutex дополнительно сериализует операции одного процесса.

Если последняя строка файла содержит separator-валидный JSON без завершающего перевода строки, append дописывает разделитель и новую запись успешно. Если хвост не парсится как JSON (оборванная или неполная последняя запись), append возвращает ошибку и не изменяет ни одного байта. Операции рейтинга и retention сохраняют такие байты без потерь. Сканирование ограничено 32 MiB на запись, поэтому файл без newline не приводит к неограниченному выделению памяти. Слишком большая новая запись отклоняется до открытия файла.

## Просмотр и фильтры

```powershell
neuromita dataset stats --config C:\Data\finetune.json
neuromita dataset list --config C:\Data\finetune.json --from 2026-01-01 --to 2026-01-31 --character mita --model gemini-3.6-flash --rating no-negative --outcome accepted
neuromita dataset rate --config C:\Data\finetune.json --sample UUID --value 1
neuromita dataset rate --config C:\Data\finetune.json --sample UUID --value -1
```

Дата из десяти символов означает полный UTC-день; RFC 3339 со смещением нормализуется в UTC. Границы включительные. Фильтры персонажа, модели, рейтинга и accepted outcome применяются до вывода или экспорта. `stats` показывает общее число, рейтинги, исходы и распределение по персонажам и моделям.

## Экспорт

```powershell
neuromita dataset export --config C:\Data\finetune.json --format raw --out C:\Export\raw.jsonl --preview
neuromita dataset export --config C:\Data\finetune.json --format raw --out C:\Export\raw.jsonl
neuromita dataset export --config C:\Data\finetune.json --format sharegpt --out C:\Export\sharegpt.jsonl --rating positive
```

Raw JSONL сохраняет фактические provider request/response, мультимодальные части и outcome. ShareGPT и ChatML сохраняют только текстовые роли `system/user/assistant`; image и другие non-text parts, а также tool/unknown messages теряются. Предварительный `--preview` показывает число подходящих записей и точное число потерянных частей/сообщений, не создавая файл.

Файл сначала полностью пишется во временный файл рядом с выбранным путём, синхронизируется и публикуется без перезаписи существующего destination. Ошибка или пустая выборка не оставляет неполный итоговый файл. Экспорт запрещён в managed store root и в имена `samples_*.jsonl`/`outcomes_*.jsonl`.

`clear --yes` удаляет только managed sample/outcome-файлы и не затрагивает настройки, SQLite Timeline или другие файлы. Без `--yes` команда отказывает.

## Границы

В этом срезе нет автоматической отправки датасета, Python-import, GUI picker и отдельного user-facing rating widget. `character_name` в standalone Core совпадает с ID, `game_connected=false`, потому что Unity-состояние ядру здесь неизвестно. Реальный provider call проверяется локальным HTTP fixture; production credentials и внешняя сеть в тестах не используются.
