# Миграции и перенос настроек

Объём от 2026-11-02: настройки подключены к [общей системе миграций](migration-system.md), единая подготовка Rust-настроек при старте и импорте, применение через существующего владельца настроек. Python-перенос не реализуется.

- [x] Общий Migration trait без глобального реестра и скрытого поиска модулей.
- [x] Одна строгая цепочка Rust schema 1 → 2 для startup и import; текущая schema проверяется без defaults.
- [x] Проверенный SettingsTransfer и явный Python stub с ошибкой вместо фиктивного успеха.
- [x] SettingsOwner/SettingsApi/SettingsPresenter: экспорт и атомарное применение подготовленного импорта с CAS и наблюдением.
- [x] Offline CLI и атомарная файловая запись без запуска Core/LLM.
- [x] Проверки миграции, импорта, ошибок, сохранения и рестарта; Cargo test/Clippy/fmt.

Migration преобразует внешний формат в результат или ошибку. Версии, шаги и storage adapter принадлежат модулю; цепочку выполняет общий MigrationService. StartupMigrations запускает явно подключённые модули до публикации API. Между всеми хранилищами Core нет общей транзакции. Существующие миграции SQLite не переводятся на новый механизм в рамках этой задачи.

SettingsTransfer — полный проверенный Rust envelope, одновременно подготовленный пакет переноса. Подготовка из байтов не изменяет состояние или файлы. Rust использует ту же миграцию, что startup. Python имеет отдельный явный вход, который пока возвращает NotImplemented.

Живой import заменяет AppSettings целиком по expected_revision. Revision из файла не заменяет revision работающего владельца: реальные изменения увеличивают локальную revision на один, повтор тех же значений ничего не меняет. Подписчики и bindings получают обычное обновление SettingsSnapshot. Сохранение остаётся фоновым с отдельным состоянием ошибки и flush.

Offline import заменяет весь файл, сохраняет revision из envelope и предназначен для остановленного приложения. Export охватывает только AppSettings: каталог тел пресетов, credentials, Timeline и RequestTrace не входят в перенос. Привязки к PresetId сохраняются, но сам каталог нужно переносить отдельно.

## Контракты и связи

`Migration<Input>` задаёт преобразование внешнего формата. Полный контракт версионированной миграции состоит из MigrationSchema, MigrationStep, MigrationStore и MigrationService. RustSettingsMigration::service создаёт сервис со схемой настроек и шагом SettingsV1ToV2. Другому модулю достаточно реализовать свои схему/шаги/адаптер и зарегистрировать сервис в ApplicationConfig.migrations.

Startup: ApplicationLifecycle::start → StartupMigrations → settings job → SettingsRepository::migrate → MigrationService::run → проверка/шаг/атомарная запись schema 2 → SettingsLifecycle::prepare. Ошибка миграции или её записи прерывает старт до публикации API; последний завершённый шаг остаётся зафиксированным. Обычный load и подготовка импорта не изменяют исходный файл. Последующие пользовательские изменения сохраняются существующим SettingsLifecycle с Pending/Saved/Failed и flush.

Импорт: SettingsApi::prepare_import(format, bytes) → SettingsTransfer → SettingsApi::import(expected_revision, transfer) → SettingsOwner::change → SettingsSnapshot/watch и фоновое сохранение. Подготовка доступна до старта Core, применение требует работающего владельца. Prepared transfer не хранит ссылку на Core. SettingsPresenter::import/export обращаются к своему owner, поэтому старый Presenter не переключится на новый Core после рестарта.

Экспорт: SettingsApi::export или SettingsPresenter::export → SettingsTransfer → encode. Экспортируется согласованное текущее состояние в памяти, включая ещё не записанные изменения. Выбор файла и IO принадлежат вызывающему адаптеру. CLI использует JsonSettingsRepository::export_to/import_from вне работающего Core.

## Форматы и расширение

Текущая схема — 2, все её разделы обязательны. Schema 1 поддерживает два исторических варианта: исходные voice/capture/interface без presets и расширенный presets с character assignments. Недостающие presets/assignments создаются только явным правилом версии 1. Существующие значения и revision сохраняются; assignments перемещаются в characters. Эти правила не применяются к повреждённой schema 2. Неизвестная версия, повторяющиеся/неизвестные поля и неверные значения дают ошибку.

Следующая версия добавляется новым MigrationStep, типом старого документа и обновлением SettingsSchema. Исполнитель и startup dispatcher при этом не меняются. Для Python реализуется PythonSettingsMigration::migrate; пока результат всегда SettingsTransferError::NotImplemented. Функция не читает реальные Python settings и не создаёт пустую успешную конфигурацию.

## CLI

```text
neuromita settings export --source STORE.json --destination BACKUP.json
neuromita settings import --source BACKUP.json --destination STORE.json
neuromita settings import --source PYTHON.json --destination STORE.json --format python
```

Последняя команда пока завершается явной ошибкой без изменения назначения. Имя формата по умолчанию — rust, автодетекта или переключения после ошибки нет. При ошибке валидации/миграции файловая запись не начинается. Замена использует общий sidecar lock, уникальный соседний temp с create_new, sync, закрытием и rename; неудачная замена сохраняет предыдущий файл. Гарантий power-loss durability нет. Offline import по-прежнему предназначен для остановленного приложения.

Проверки в tests/settings_migrations.rs: одинаковая миграция при load/prepare/import, строгая текущая схема, ошибочный startup без публикации API, CAS и binding, экспорт до flush, повторный импорт, сохранение и restart, отказ rename/share lock, offline CLI и Python stub. Внешние модели и реальные Python настройки не вызываются.

Первый срез импорта/экспорта проходил 186 тестов с SQLite и 135 без него. Актуальная проверка расширенной системы описана в [migration-system](migration-system.md). Новых dependencies и изменений vendor нет.
