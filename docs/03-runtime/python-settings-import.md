# Python → Rust settings import (preview-only discovery first)

This is an intentional **selective import**, not a complete Python application
backup, historical Rust migration chain, or lossless conversion. It targets the
Python `Settings/settings.json` flat object on source
`Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65` and the current
Rust `StoredSettings` envelope. Both applications should be stopped during
an actual import.

## Commands

```text
neuromita settings import --format python --source /absolute/Python/Settings/settings.json --destination /absolute/Rust/UserData/settings.json --preview
neuromita settings import --format python --source /absolute/Python/Settings/settings.json --destination /absolute/Rust/UserData/settings.json --allow-unmapped
```

Run the preview before applying. It reports **only field names**, not values.
Import refuses unexpected fields by default, listing the skipped key names;
`--allow-unmapped` explicitly authorizes skipping them. Unsupported or malformed
values of a *supported* field are **always rejected**, even with that flag.
No fields are silently replaced with Rust defaults on a failed import.

| Python key | Rust destination | Validation |
| --- | --- | --- |
| `LANGUAGE` | `interface.language` | `RU` or `EN` |
| `HIDE_PRIVATE` | `interface.hide_private` | Python-style bool or 0/1 |
| `REMINDERS_ENABLED` | `reminders.enabled` | Python-style bool or 0/1 |
| `VOICE_LANGUAGE` | `speech.tts.local_language` | `ru` or `en` |
| `LOCAL_VOICE_LOAD_LAST` | `speech.tts.local_auto_load` | Python-style bool or 0/1 |
| `LOCAL_VOICE_INIT_ON_REQUEST` | `speech.tts.local_lazy_init` | Python-style bool or 0/1 |

Other Python settings, including API credentials, voice volume (different
scales), speech models, presets, prompt bodies and plugin settings, remain
unmapped. No Timeline/history, database, Character, neural model binaries,
`Histories`, `Models`, `Checkpoints` or plugin files are imported.

## Safety and partial failure

- Preview reads Python input, validates known fields and enumerates unmapped
  fields without writing files or touching the running Core.
- Applying merges only mapped fields into an **already validated** Rust
  `StoredSettings` snapshot. Other Rust values remain intact. The revision
  advances only if mapped values changed; repeating an unchanged import is a
  no-op without additional backups.
- Before changing Rust settings, an exact byte-for-byte copy of the Python
  input and, if it exists, the original Rust destination is written beside the
  respective file with a `.pre-python-import-<uuid>.bak` suffix. On Unix these
  backups are mode 0600. **Backups may include plaintext secrets.**
- The Rust settings file is published by the existing atomic-replace storage
  implementation. Errors before publication leave the original settings
  unchanged, though any successfully created backups remain.
- `SettingsTransfer::prepare`/`JsonSettingsRepository::import_from` explicitly
  **reject** Python payloads because generic full replacement cannot preserve
  existing settings; Python import must use the reviewed merge workflow.
- The CLI is offline-only; if another process edits settings concurrently,
  there is no cross-process compare-and-swap spanning backup and merge.
  Stop the application. Directory/fsync and power-loss durability are not
  promised by the current Rust settings repository.

This stage does not satisfy the rest of issue #45: additional supported
formats, id collisions between user presets, interrupted-save end-to-end
simulation and full transfer/export parity require separate acceptance.
