# Python settings import: consolidation of PR #109 into #124

Current canonical stack: #103 (portable root) → #124 (Python settings) → #128 (onboarding) → #129 (verified updater) → this PR. The alternate #109 is not merged directly: the stacks diverged from #103, and cherry-picking the whole patch would overwrite an independent implementation.

This change ports the independent, verified behavior from #109 into the existing #124 implementation. It does **not** establish a second settings parser or migration format. Source of Python compatibility: `Atm4x/NeuroMita@5f3d061f523d9abefa6a264bb2953048a935fa65`.

## Offline workflow

```text
neuromita settings import --format python --source PYTHON/Settings/settings.json --destination RUST/UserData/settings.json --preview
neuromita settings import --format python --source PYTHON/Settings/settings.json --destination RUST/UserData/settings.json --expect-source-sha256 <SOURCE_SHA_FROM_PREVIEW> --expect-target-sha256 <TARGET_SHA_FROM_PREVIEW_OR_absent> --accept-skipped-key KEY_NAME
```

`--dry-run` remains an alias for `--preview` (it does not write settings or backup files; a stable writer lock sidecar may be created). The preview prints the SHA-256 of the exact Python source and existing Rust target, or `absent` for a new target, plus imported and skipped key **names only**. To approve omissions, provide both fingerprints and exactly one `--accept-skipped-key NAME` for **each** skipped key in preview order (repeat the flag as needed). Old `--allow-unmapped` is rejected: a one-bit allowance cannot attest which values were omitted. Under the existing writer lock, the repository recomputes the hashes and complete key lists before backup/publish. Missing, duplicate or reordered keys, value-only edits, foreign target edits and newly appearing keys fail without changing the target. If there are no skipped keys, an ordinary import without approvals remains supported. Invalid mapped values fail even with approval. A direct generic `SettingsRepository::import_from(Python)` is rejected.

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

When an import changes settings, the CLI stores an exact Python-source backup named `python-source-<basename>.pre-python-import-<uuid>.bak` beside the Rust target, then the repository stores a backup of the previous Rust settings (if one existed) before atomic replace. A failed receipt check removes any newly created Python-source backup; no target backup or replacement occurs. The legacy Python source is never modified. On Unix, source backups, destination backups and newly published settings use mode `0600`. Operators must protect these files: backups may contain API secrets. Repeated imports that produce the same Rust profile do not write new backups. Rejected imports do not alter the settings target; stable lock files may remain.

## Non-goals and follow-up

The merge and atomic replacement assume the application is stopped. Cross-process handling is limited to cooperating settings writers; it does not promise durable directory fsync/power-loss transactions or recovery from concurrent external edits. Windows UI/installer import, full presets and complete historical Python format coverage remain open under #45. Neither PR #109 nor this follow-up fully closes #45.
