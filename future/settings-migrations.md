# Миграции и перенос настроек

Объём от 2026-11-02: минимальный общий контракт миграции, единая подготовка Rust-настроек при старте и импорте, применение через существующего владельца настроек. Python-перенос не реализуется.

- [x] Общий Migration trait без глобального реестра и скрытого поиска модулей.
- [x] Одна строгая цепочка Rust schema 1 → 2 для startup и import; текущая schema проверяется без defaults.
- [x] Проверенный SettingsTransfer и явный Python stub с ошибкой вместо фиктивного успеха.
- [x] SettingsOwner/SettingsApi/SettingsPresenter: экспорт и атомарное применение подготовленного импорта с CAS и наблюдением.
- [x] Offline CLI и атомарная файловая запись без запуска Core/LLM.
- [x] Проверки миграции, импорта, ошибок, сохранения и рестарта; Cargo test/Clippy/fmt.

Migration преобразует вход в результат или ошибку. Форматы и версии знает конкретный модуль. Порядок startup задаётся композицией: SettingsLifecycle загружает и проверяет настройки до публикации API. Trait не является service locator, не запускает произвольные callbacks и не обещает транзакцию между всеми хранилищами Core. Существующие миграции SQLite не переводятся на новый механизм в рамках этой задачи.

SettingsTransfer — полный проверенный Rust envelope, одновременно подготовленный пакет переноса. Подготовка из байтов не изменяет состояние или файлы. Rust использует ту же миграцию, что startup. Python имеет отдельный явный вход, который пока возвращает NotImplemented.

Живой import заменяет AppSettings целиком по expected_revision. Revision из файла не заменяет revision работающего владельца: реальные изменения увеличивают локальную revision на один, повтор тех же значений ничего не меняет. Подписчики и bindings получают обычное обновление SettingsSnapshot. Сохранение остаётся фоновым с отдельным состоянием ошибки и flush.

Offline import заменяет весь файл, сохраняет revision из envelope и предназначен для остановленного приложения. Export охватывает только AppSettings: каталог тел пресетов, credentials, Timeline и RequestTrace не входят в перенос. Привязки к PresetId сохраняются, но сам каталог нужно переносить отдельно.

## Контракты и связи

`Migration<Input>` в `src/migration.rs` содержит associated Output/Error и единственный метод migrate. Реализации RustSettingsMigration и PythonSettingsMigration принадлежат settings. Следующий модуль Core может реализовать тот же trait для своего входа; порядок и границу записи определяет его владелец при prepare/startup. Универсального реестра startup hooks нет.

Startup: ApplicationLifecycle::start → SettingsLifecycle::prepare → JsonSettingsRepository::load → SettingsTransfer::decode → RustSettingsMigration. Ошибка прерывает старт до публикации API, исходный файл не заменяется. Миграция выполняется в памяти; сохранение schema 2 идёт существующим SettingsLifecycle, с обычной наблюдаемой Pending/Saved/Failed и flush.

Импорт: SettingsApi::prepare_import(format, bytes) → SettingsTransfer → SettingsApi::import(expected_revision, transfer) → SettingsOwner::change → SettingsSnapshot/watch и фоновое сохранение. Подготовка доступна до старта Core, применение требует работающего владельца. Prepared transfer не хранит ссылку на Core. SettingsPresenter::import/export обращаются к своему owner, поэтому старый Presenter не переключится на новый Core после рестарта.

Экспорт: SettingsApi::export или SettingsPresenter::export → SettingsTransfer → encode. Экспортируется согласованное текущее состояние в памяти, включая ещё не записанные изменения. Выбор файла и IO принадлежат вызывающему адаптеру. CLI использует JsonSettingsRepository::export_to/import_from вне работающего Core.

## Форматы и расширение

Текущая схема — 2, все её разделы обязательны. Schema 1 поддерживает два исторических варианта: исходные voice/capture/interface без presets и расширенный presets с character assignments. Недостающие presets/assignments создаются только явным правилом версии 1. Существующие значения и revision сохраняются; assignments перемещаются в characters. Эти правила не применяются к повреждённой schema 2. Неизвестная версия, повторяющиеся/неизвестные поля и неверные значения дают ошибку.

Следующая версия добавляется в RustSettingsMigration вместе с типом старого документа, явным преобразованием и проверками сохранения значений. Для Python реализуется PythonSettingsMigration::migrate; пока результат всегда SettingsTransferError::NotImplemented. Функция не читает реальные Python settings и не создаёт пустую успешную конфигурацию.

## CLI

```text
neuromita settings export --source STORE.json --destination BACKUP.json
neuromita settings import --source BACKUP.json --destination STORE.json
neuromita settings import --source PYTHON.json --destination STORE.json --format python
```

Последняя команда пока завершается явной ошибкой без изменения назначения. Имя формата по умолчанию — rust, автодетекта или переключения после ошибки нет. При ошибке валидации/миграции файловая запись не начинается. Замена использует уникальный соседний temp с create_new, sync, закрытием и rename; неудачная замена сохраняет предыдущий файл. Межпроцессного lock и гарантий power-loss durability нет.

Проверки в tests/settings_migrations.rs: одинаковая миграция при load/prepare/import, строгая текущая схема, ошибочный startup без публикации API, CAS и binding, экспорт до flush, повторный импорт, сохранение и restart, отказ rename/share lock, offline CLI и Python stub. Внешние модели и реальные Python настройки не вызываются.

Проверено на Windows: cargo test --locked --offline — 186 passed, 1 live ignored; с --no-default-features — 135 passed, 1 live ignored. Clippy --all-targets с -D warnings в обоих режимах и rustfmt прошли. Новых dependencies и изменений vendor нет.
