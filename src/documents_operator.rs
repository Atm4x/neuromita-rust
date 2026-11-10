use std::{
    io::Read,
    path::{Component, PathBuf},
};

use neuromita::{
    character::CharacterId,
    document_revision::{
        DocumentRevisionApi, DocumentRevisionInspection, DocumentRevisionReceipt,
        DocumentRevisionStatus,
    },
    settings::{DocumentGenerationSettings, SettingsApi},
};

const USAGE: &str = "Используй /documents status CHARACTER | inspect CHARACTER | activate CHARACTER EXPECTED_SETTINGS_REVISION ABSOLUTE_RECEIPT_JSON | deactivate CHARACTER EXPECTED_SETTINGS_REVISION | retry-save EXPECTED_SETTINGS_REVISION";
const MAX_RECEIPT_BYTES: usize = 8192;

#[derive(Debug, PartialEq, Eq)]
enum DocumentsCommand {
    Status(CharacterId),
    Inspect(CharacterId),
    Activate {
        character: CharacterId,
        expected_revision: u64,
        receipt_path: PathBuf,
    },
    Deactivate {
        character: CharacterId,
        expected_revision: u64,
    },
    RetrySave {
        expected_revision: u64,
    },
}

fn parse(line: &str) -> Result<DocumentsCommand, &'static str> {
    let args = line.trim().strip_prefix("/documents").ok_or(USAGE)?.trim();
    let mut args = args.splitn(2, char::is_whitespace);
    let action = args.next().ok_or(USAGE)?;
    let arguments = args.next().unwrap_or("").trim();
    match action {
        "status" => {
            let character = exact_character(arguments)?;
            Ok(DocumentsCommand::Status(character))
        }
        "inspect" => {
            let character = exact_character(arguments)?;
            Ok(DocumentsCommand::Inspect(character))
        }
        "deactivate" => {
            let mut parts = arguments.split_whitespace();
            let character = character(parts.next().ok_or(USAGE)?)?;
            let expected_revision = revision(parts.next().ok_or(USAGE)?)?;
            if parts.next().is_some() {
                return Err(USAGE);
            }
            Ok(DocumentsCommand::Deactivate {
                character,
                expected_revision,
            })
        }
        "activate" => {
            let mut parts = arguments.splitn(3, char::is_whitespace);
            let character = character(parts.next().ok_or(USAGE)?)?;
            let expected_revision = revision(parts.next().ok_or(USAGE)?)?;
            let path = parts.next().ok_or(USAGE)?.trim();
            let path = if path.starts_with('"') && path.ends_with('"') && path.len() > 1 {
                &path[1..path.len() - 1]
            } else {
                if path.chars().any(char::is_whitespace) || path.contains('"') {
                    return Err(USAGE);
                }
                path
            };
            let receipt_path = PathBuf::from(path);
            if !receipt_path.is_absolute()
                || receipt_path
                    .components()
                    .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
                || path.is_empty()
            {
                return Err(USAGE);
            }
            Ok(DocumentsCommand::Activate {
                character,
                expected_revision,
                receipt_path,
            })
        }
        "retry-save" => Ok(DocumentsCommand::RetrySave {
            expected_revision: revision(arguments)?,
        }),
        _ => Err(USAGE),
    }
}

fn character(value: &str) -> Result<CharacterId, &'static str> {
    CharacterId::parse(value).ok_or(USAGE)
}

fn exact_character(value: &str) -> Result<CharacterId, &'static str> {
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err(USAGE);
    }
    character(value)
}

fn revision(value: &str) -> Result<u64, &'static str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(USAGE);
    }
    value.parse::<u64>().map_err(|_| USAGE)
}

fn load_receipt(path: &PathBuf) -> Result<DocumentRevisionReceipt, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_RECEIPT_BYTES as u64
    {
        return Err("receipt must be a bounded regular file, not a symlink".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take((MAX_RECEIPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err("receipt exceeds byte limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "invalid Documents revision receipt JSON".into())
}

fn check_durable_revision(expected_revision: u64) -> Result<(), String> {
    let saved = SettingsApi::snapshot_all().map_err(|error| error.to_string())?;
    if saved.revision != expected_revision || saved.persisted_revision != Some(expected_revision) {
        return Err(format!(
            "настройки изменились конкурентно или не подтверждены на диске: ожидалась ревизия {expected_revision}; сейчас {}, сохранена {:?}; перечитайте /documents inspect",
            saved.revision, saved.persisted_revision
        ));
    }
    Ok(())
}

pub async fn run(line: &str) -> Result<String, String> {
    match parse(line).map_err(str::to_owned)? {
        DocumentsCommand::Status(character) => {
            let status =
                tokio::task::spawn_blocking(move || DocumentRevisionApi::status(character))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
            let (revision, description) = match status {
                DocumentRevisionStatus::Disabled { settings_revision } => {
                    (settings_revision, "отключено".to_owned())
                }
                DocumentRevisionStatus::Ready {
                    settings_revision,
                    revision,
                } => (
                    settings_revision,
                    format!(
                        "готово; fingerprint={}, документов={}",
                        revision.fingerprint, revision.documents
                    ),
                ),
            };
            let snapshot = SettingsApi::snapshot_all().map_err(|error| error.to_string())?;
            let durability =
                if snapshot.revision == revision && snapshot.persisted_revision == Some(revision) {
                    "сохранено"
                } else {
                    "сохранение НЕ подтверждено"
                };
            Ok(format!(
                "Documents {character}: {description}; settings revision {revision}; {durability}"
            ))
        }
        DocumentsCommand::Inspect(character) => {
            let status =
                tokio::task::spawn_blocking(move || DocumentRevisionApi::inspect(character))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
            let (revision, description) = match status {
                DocumentRevisionInspection::Disabled { settings_revision } => {
                    (settings_revision, "отключено".to_owned())
                }
                DocumentRevisionInspection::Ready {
                    settings_revision,
                    revision,
                } => (
                    settings_revision,
                    format!(
                        "готово; fingerprint={}, документов={}",
                        revision.fingerprint, revision.documents
                    ),
                ),
                DocumentRevisionInspection::Unavailable {
                    settings_revision,
                    issue,
                } => (
                    settings_revision,
                    format!("источник недоступен: {issue:?}; автоматического переключения нет"),
                ),
            };
            let snapshot = SettingsApi::snapshot_all().map_err(|error| error.to_string())?;
            let durability =
                if snapshot.revision == revision && snapshot.persisted_revision == Some(revision) {
                    "сохранено"
                } else {
                    "сохранение НЕ подтверждено"
                };
            Ok(format!(
                "Documents {character}: {description}; settings revision {revision}; {durability}"
            ))
        }
        DocumentsCommand::Activate {
            character,
            expected_revision,
            receipt_path,
        } => {
            let receipt = tokio::task::spawn_blocking(move || load_receipt(&receipt_path))
                .await
                .map_err(|error| error.to_string())??;
            if receipt.character != character {
                return Err("receipt относится к другому персонажу; настройки не изменены".into());
            }
            let check = receipt.clone();
            let activated = tokio::task::spawn_blocking(move || {
                DocumentRevisionApi::activate(&receipt, expected_revision)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
            if let Err(error) = SettingsApi::flush().await {
                return Err(format!(
                    "активация выполнена в памяти, но запись на диск НЕ подтверждена: {error}; используйте /documents retry-save {}",
                    activated.revision
                ));
            }
            check_durable_revision(activated.revision)?;
            let actual =
                tokio::task::spawn_blocking(move || DocumentRevisionApi::status(character))
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?;
            if !matches!(actual, DocumentRevisionStatus::Ready { settings_revision, revision } if settings_revision == activated.revision && revision == check)
            {
                return Err("ревизия сохранена, но источник уже изменился; выполните inspect и повторную переиндексацию".into());
            }
            Ok(format!(
                "Documents {character}: ревизия {} активирована и подтверждённо сохранена",
                activated.revision
            ))
        }
        DocumentsCommand::Deactivate {
            character,
            expected_revision,
        } => {
            let deactivated = DocumentRevisionApi::deactivate(character, expected_revision)
                .map_err(|error| error.to_string())?;
            if let Err(error) = SettingsApi::flush().await {
                return Err(format!(
                    "отключено в памяти, но запись на диск НЕ подтверждена: {error}; используйте /documents retry-save {}",
                    deactivated.revision
                ));
            }
            check_durable_revision(deactivated.revision)?;
            let snapshot = SettingsApi::snapshot::<DocumentGenerationSettings>()
                .map_err(|error| error.to_string())?;
            if snapshot.value.sources.contains_key(&character)
                || snapshot.revision != deactivated.revision
            {
                return Err(
                    "настройки конкурентно изменились после отключения; выполните inspect".into(),
                );
            }
            Ok(format!(
                "Documents {character}: отключено и подтверждённо сохранено (revision {})",
                deactivated.revision
            ))
        }
        DocumentsCommand::RetrySave { expected_revision } => {
            let before = SettingsApi::snapshot_all().map_err(|error| error.to_string())?;
            if before.revision != expected_revision {
                return Err(format!(
                    "settings CAS conflict: expected {expected_revision}, actual {}",
                    before.revision
                ));
            }
            SettingsApi::retry_save().map_err(|error| error.to_string())?;
            SettingsApi::flush()
                .await
                .map_err(|error| format!("повторная запись не подтверждена: {error}"))?;
            check_durable_revision(expected_revision)?;
            Ok(format!(
                "Settings revision {expected_revision}: повторная запись подтверждена; все секции настроек сохранены"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_parser_rejects_ambiguous_or_mutating_variants() {
        assert_eq!(
            parse("/documents status Crazy"),
            Ok(DocumentsCommand::Status(CharacterId::Crazy))
        );
        assert_eq!(
            parse("/documents inspect Crazy"),
            Ok(DocumentsCommand::Inspect(CharacterId::Crazy))
        );
        assert_eq!(
            parse("/documents deactivate Kind 17"),
            Ok(DocumentsCommand::Deactivate {
                character: CharacterId::Kind,
                expected_revision: 17
            })
        );
        assert_eq!(
            parse("/documents retry-save 17"),
            Ok(DocumentsCommand::RetrySave {
                expected_revision: 17
            })
        );
        let receipt = std::env::temp_dir().join("receipt with spaces.json");
        assert_eq!(
            parse(&format!(
                "/documents activate Kind 9 \"{}\"",
                receipt.display()
            )),
            Ok(DocumentsCommand::Activate {
                character: CharacterId::Kind,
                expected_revision: 9,
                receipt_path: receipt
            })
        );
        for invalid in [
            "/documents",
            "/documents inspect",
            "/documents inspect Kind extra",
            "/documents activate Kind 4",
            "/documents activate Kind -1 /tmp/x",
            "/documents activate Kind 0 relative.json",
            "/documents activate Kind 0 /tmp/../x",
            "/documents activate Kind 0 /tmp/receipt with spaces",
            "/documents deactivate Crazy",
            "/documents retry-save",
            "/documents retry-save -1",
            "/documents retry-save 4 extra",
            "/documents reindex Kind",
        ] {
            assert!(parse(invalid).is_err(), "{invalid}");
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use neuromita::{
        app::{ApplicationConfig, ApplicationLifecycle, RuntimeLimits},
        generation::{
            GenerationError, GenerationInput, GenerationPreset, GenerationProvider, ProviderFuture,
            ProviderKind, ProviderTemplate, ResponseGenerator,
            gemini::{GeminiTemplate, GenerationSettings},
        },
        settings::{
            AppSettings, JsonSettingsRepository, SettingsConfig, SettingsError, SettingsRepository,
            SettingsSource, StoredSettings,
        },
    };
    use std::{
        num::NonZeroUsize,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    struct UnusedProvider;

    impl GenerationProvider for UnusedProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Gemini
        }

        fn generate(&self, _: GenerationInput, _: ProviderTemplate) -> ProviderFuture<'_> {
            Box::pin(async {
                Err(GenerationError::Transport(
                    "unused in operator tests".into(),
                ))
            })
        }
    }

    fn config<R: SettingsRepository>(
        repository: Arc<R>,
        source: SettingsSource,
    ) -> ApplicationConfig {
        let preset = GenerationPreset::new(ProviderTemplate::Gemini(
            GeminiTemplate::google(
                "test",
                "unused",
                GenerationSettings::new(0.0, 128, Duration::from_secs(5)).unwrap(),
            )
            .unwrap(),
        ));
        let size = |value| NonZeroUsize::new(value).unwrap();
        let mut config = ApplicationConfig::minimal(
            preset,
            ResponseGenerator::new([Arc::new(UnusedProvider) as Arc<dyn GenerationProvider>])
                .unwrap(),
            RuntimeLimits {
                request_queue: size(4),
                discovery_buffer: size(8),
                character_commands: size(16),
                presenter_updates: size(8),
                retained_finished: size(8),
            },
            "127.0.0.1:0".parse().unwrap(),
        );
        config.settings = SettingsConfig { source, repository };
        config
    }

    struct FailableRepository {
        inner: JsonSettingsRepository,
        reject_saves: AtomicBool,
    }

    impl SettingsRepository for FailableRepository {
        fn migrate(&self) -> Result<(), SettingsError> {
            self.inner.migrate()
        }
        fn load(&self) -> Result<StoredSettings, SettingsError> {
            self.inner.load()
        }
        fn save(&self, value: &StoredSettings) -> Result<(), SettingsError> {
            if self.reject_saves.load(Ordering::SeqCst) {
                Err(SettingsError::Storage(
                    "injected settings disk failure".into(),
                ))
            } else {
                self.inner.save(value)
            }
        }
    }

    static CORE_TEST_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn real_terminal_operator_activates_persists_inspects_and_deactivates_after_restart() {
        let _serial = CORE_TEST_MUTEX.lock().await;
        let folder = std::env::temp_dir().join(format!("nm-doc-operator-{}", uuid::Uuid::new_v4()));
        let crazy = folder.join("crazy");
        let kind = folder.join("kind");
        let revisions = folder.join("revisions");
        std::fs::create_dir_all(&crazy).unwrap();
        std::fs::create_dir_all(&kind).unwrap();
        std::fs::create_dir(&revisions).unwrap();
        std::fs::write(crazy.join("note.md"), "Orchid blue notes").unwrap();
        std::fs::write(kind.join("note.txt"), "Violet document").unwrap();
        let receipt = DocumentRevisionApi::prepare(&crazy, CharacterId::Crazy, &revisions).unwrap();
        let other = DocumentRevisionApi::prepare(&kind, CharacterId::Kind, &revisions).unwrap();
        let receipt_path = folder.join("crazy-receipt.json");
        let other_path = folder.join("kind-receipt.json");
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        std::fs::write(&other_path, serde_json::to_vec(&other).unwrap()).unwrap();
        let repository = Arc::new(JsonSettingsRepository::new(folder.join("settings.json")));

        let app = ApplicationLifecycle::start(config(
            Arc::clone(&repository),
            SettingsSource::initial(AppSettings::initial_profile()),
        ))
        .await
        .unwrap();
        SettingsApi::flush().await.unwrap();
        let base = SettingsApi::snapshot_all().unwrap().revision;
        assert!(
            run("/documents inspect Crazy")
                .await
                .unwrap()
                .contains("отключено")
        );
        let malformed = folder.join("malformed.json");
        std::fs::write(&malformed, b"{not-json}").unwrap();
        assert!(
            run(&format!(
                "/documents activate Crazy {base} {}",
                malformed.display()
            ))
            .await
            .is_err()
        );
        assert_eq!(SettingsApi::snapshot_all().unwrap().revision, base);
        assert!(
            run(&format!(
                "/documents activate Kind {base} {}",
                receipt_path.display()
            ))
            .await
            .is_err()
        );
        let mut forged = receipt.clone();
        forged.fingerprint.push('0');
        let fake_path = folder.join("forged.json");
        std::fs::write(&fake_path, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(
            run(&format!(
                "/documents activate Crazy {base} {}",
                fake_path.display()
            ))
            .await
            .is_err()
        );
        assert_eq!(SettingsApi::snapshot_all().unwrap().revision, base);
        let one = run(&format!(
            "/documents activate Crazy {base} {}",
            receipt_path.display()
        ))
        .await
        .unwrap();
        assert!(one.contains("подтверждённо сохранена"));
        assert!(
            run(&format!(
                "/documents activate Kind {base} {}",
                other_path.display()
            ))
            .await
            .is_err()
        );
        let first = repository.load().unwrap();
        assert_eq!(first.revision, base + 1);
        assert_eq!(
            first.settings.document_generation.sources[&CharacterId::Crazy].index,
            receipt.index
        );
        let both = run(&format!(
            "/documents activate Kind {} {}",
            first.revision,
            other_path.display()
        ))
        .await
        .unwrap();
        assert!(both.contains("подтверждённо сохранена"));
        assert_eq!(repository.load().unwrap().revision, base + 2);
        app.shutdown().await.unwrap();

        let app =
            ApplicationLifecycle::start(config(Arc::clone(&repository), SettingsSource::Stored))
                .await
                .unwrap();
        assert!(
            run("/documents inspect Crazy")
                .await
                .unwrap()
                .contains(&receipt.fingerprint)
        );
        assert!(
            run("/documents status Crazy")
                .await
                .unwrap()
                .contains(&receipt.fingerprint)
        );
        assert!(
            run("/documents inspect Kind")
                .await
                .unwrap()
                .contains(&other.fingerprint)
        );
        assert!(
            run(&format!("/documents deactivate Crazy {}", base + 1))
                .await
                .is_err()
        );
        assert!(
            run(&format!("/documents deactivate Crazy {}", base + 2))
                .await
                .unwrap()
                .contains("подтверждённо сохранено")
        );
        assert!(
            !repository
                .load()
                .unwrap()
                .settings
                .document_generation
                .sources
                .contains_key(&CharacterId::Crazy)
        );
        assert!(
            repository
                .load()
                .unwrap()
                .settings
                .document_generation
                .sources
                .contains_key(&CharacterId::Kind)
        );
        std::fs::write(
            crazy.join("note.md"),
            "Source changed after index preparation",
        )
        .unwrap();
        assert!(
            run(&format!(
                "/documents activate Crazy {} {}",
                base + 3,
                receipt_path.display()
            ))
            .await
            .is_err()
        );
        assert_eq!(repository.load().unwrap().revision, base + 3);
        #[cfg(unix)]
        {
            let symlink = folder.join("receipt-symlink.json");
            std::os::unix::fs::symlink(&receipt_path, &symlink).unwrap();
            assert!(
                run(&format!(
                    "/documents activate Crazy {} {}",
                    base + 3,
                    symlink.display()
                ))
                .await
                .is_err()
            );
        }
        app.shutdown().await.unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[tokio::test]
    async fn failed_durable_write_requires_explicit_cas_guarded_retry() {
        let _serial = CORE_TEST_MUTEX.lock().await;
        let folder =
            std::env::temp_dir().join(format!("nm-doc-operator-fault-{}", uuid::Uuid::new_v4()));
        let source = folder.join("documents");
        let revisions = folder.join("revisions");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir(&revisions).unwrap();
        std::fs::write(source.join("note.md"), "Blue orchid").unwrap();
        let receipt =
            DocumentRevisionApi::prepare(&source, CharacterId::Crazy, &revisions).unwrap();
        let receipt_path = folder.join("receipt.json");
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let repo = Arc::new(FailableRepository {
            inner: JsonSettingsRepository::new(folder.join("settings.json")),
            reject_saves: AtomicBool::new(false),
        });
        let app = ApplicationLifecycle::start(config(
            Arc::clone(&repo),
            SettingsSource::initial(AppSettings::initial_profile()),
        ))
        .await
        .unwrap();
        SettingsApi::flush().await.unwrap();
        let before = repo.load().unwrap().revision;
        repo.reject_saves.store(true, Ordering::SeqCst);
        let failure = run(&format!(
            "/documents activate Crazy {before} {}",
            receipt_path.display()
        ))
        .await
        .unwrap_err();
        assert!(failure.contains("НЕ подтверждена"), "{failure}");
        let changed = SettingsApi::snapshot_all().unwrap();
        assert_eq!(changed.revision, before + 1);
        assert_eq!(repo.load().unwrap().revision, before);
        assert!(
            run("/documents inspect Crazy")
                .await
                .unwrap()
                .contains("НЕ подтверждено")
        );
        assert!(
            run(&format!("/documents retry-save {before}"))
                .await
                .is_err()
        );
        assert!(
            run(&format!("/documents retry-save {}", changed.revision))
                .await
                .is_err()
        );
        assert_eq!(repo.load().unwrap().revision, before);
        repo.reject_saves.store(false, Ordering::SeqCst);
        assert!(
            run(&format!("/documents retry-save {}", changed.revision))
                .await
                .unwrap()
                .contains("подтверждена")
        );
        assert_eq!(repo.load().unwrap().revision, changed.revision);
        app.shutdown().await.unwrap();
        let app = ApplicationLifecycle::start(config(Arc::clone(&repo), SettingsSource::Stored))
            .await
            .unwrap();
        assert!(
            run("/documents inspect Crazy")
                .await
                .unwrap()
                .contains(&receipt.fingerprint)
        );
        app.shutdown().await.unwrap();
        std::fs::remove_dir_all(folder).unwrap();
    }
}
