# Python settings import: consolidation of PR #109 into #124

Current canonical stack: #103 (portable root) → #124 (Python settings) → #128 (onboarding) → #129 (verified updater) → this PR. The alternate #109 is not merged directly: the stacks diverged from #103, and cherry-picking the whole patch would overwrite an independent implementation.

This change ports the independent, verified behavior from #109 into the existing #124 implementation. It does **not** establish a second settings parser or migration format. Source of Python compatibility: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`.

## Offline workflow

```text
neuromita settings import --format python --source PYTHON/Settings/settings.json --destination RUST/UserData/settings.json --preview
neuromita settings import --format python --source PYTHON/Settings/settings.json --destination RUST/UserData/settings.json --allow-unmapped
```

`--dry-run` remains an alias for `--preview` (read-only). `--allow-unmapped` explicitly accepts skipping unknown names. Without it, the import is rejected if *any* unmapped keys exist, regardless of whether the known keys would have changed. Unknown **values are never printed**. Invalid mapped values always fail, even with approval. A direct generic `SettingsRepository::import_from(Python)` is rejected; the reviewed merge API is required.

### Known mappings

| Python key | Rust field |
| --- | --- |
| `LANGUAGE` | `interface.language` |
| `HIDE_PRIVATE` | `interface.hide_private` |
| `EULA_ACCEPTED` | `onboarding.eula_accepted` (strict boolean only) |
| `CHAT_FONT_SIZE` | `interface.font_size` |
| `VOICEOVER_LOCAL_VOLUME` | `voice.volume` |
| `PROMPT_SET_<character>` | `characters.<character>.prompt_set`, recognized IDs only |
| `REMINDERS_ENABLED` | `reminders.enabled` |
| `VOICE_LANGUAGE` | `speech.tts.local_language` (`ru` or `en`) |
| `LOCAL_VOICE_LOAD_LAST` | `speech.tts.local_auto_load` |
| `LOCAL_VOICE_INIT_ON_REQUEST` | `speech.tts.local_lazy_init` |

Settings that do not have a confirmed destination are *not* inferred, including credentials, complete preset bodies, neural model binaries and Python-specific plugin state. This is a selective settings migration, not a full backup. The existing Rust profile must validate; it is not silently replaced by defaults. Initial settings are used only for an absent Rust target.

When an import changes settings, the CLI stores an exact Python-source backup named `python-source-<basename>.pre-python-import-<uuid>.bak` beside the Rust target, then the repository stores a backup of the previous Rust settings (if one existed) before atomic replace. The legacy Python source is never modified. On Unix, source backups, destination backups and newly published settings use mode `0600`. Operators must protect these files: backups may contain API secrets. Repeated imports that produce the same Rust profile do not write new backups. Unchanged previews and rejected imports have no write side effects.

## Non-goals and follow-up

The merge and atomic replacement assume the application is stopped. Cross-process handling is limited to cooperating settings writers; it does not promise durable directory fsync/power-loss transactions or recovery from concurrent external edits. Windows UI/installer import, full presets and complete historical Python format coverage remain open under #45. Neither PR #109 nor this follow-up fully closes #45.
