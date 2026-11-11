use std::{
    error::Error,
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

use neuromita::settings::{JsonSettingsRepository, PythonSettingsFileReview, SettingsImportFormat};

pub const HELP: &str = "neuromita settings export --source STORE.json --destination BACKUP.json\n\
    neuromita settings import --source BACKUP.json --destination STORE.json [--format rust|python] [--dry-run|--preview]\n\
    For Python settings with skipped keys: --expect-source-sha256 HEX --expect-target-sha256 HEX|absent --accept-skipped-key NAME (repeat for every skipped key)\n\n\
    Offline operations: no Core and no GEMINI_API_KEY. Stop the application first.\n\
    Rust imports require the current strict settings format; no dev migrations.\n\
    The default format is rust. Python import overlays supported keys, lists omissions,\n\
    and requires exact source/target fingerprints and skipped-key approvals to omit fields.\n\
    Backups of both source and destination are kept by the Rust target.\n\
    Preset bodies, credentials and Timeline are not included.\n";

pub fn run(arguments: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    let mut arguments = arguments.into_iter();
    let command = arguments
        .next()
        .ok_or_else(|| invalid("expected export or import; use --help"))?;
    if command == "--help" {
        print!("{HELP}");
        return Ok(());
    }
    let import = match command.to_str() {
        Some("export") => false,
        Some("import") => true,
        _ => return Err(invalid("expected export or import; use --help").into()),
    };
    let mut source = None;
    let mut destination = None;
    let mut format = None;
    let mut dry_run = false;
    let mut source_sha256 = None;
    let mut target_sha256 = None;
    let mut accepted_skipped_keys = Vec::new();
    while let Some(option) = arguments.next() {
        if option == "--help" {
            print!("{HELP}");
            return Ok(());
        }
        match option.to_str() {
            Some("--source") => path_argument(&mut source, arguments.next())?,
            Some("--destination") => path_argument(&mut destination, arguments.next())?,
            Some("--dry-run" | "--preview") if import && !dry_run => dry_run = true,
            Some("--expect-source-sha256") if import => {
                string_argument(&mut source_sha256, arguments.next())?;
            }
            Some("--expect-target-sha256") if import => {
                string_argument(&mut target_sha256, arguments.next())?;
            }
            Some("--accept-skipped-key") if import => {
                let key = arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("--accept-skipped-key requires a nonempty key"))?;
                accepted_skipped_keys.push(key);
            }
            Some("--allow-unmapped") => {
                return Err(invalid("--allow-unmapped is unsafe; use exact SHA-256 and --accept-skipped-key confirmations").into());
            }
            Some("--format") if import => {
                let value = match arguments.next().as_deref().and_then(|value| value.to_str()) {
                    Some("rust") => SettingsImportFormat::Rust,
                    Some("python") => SettingsImportFormat::Python,
                    _ => return Err(invalid("--format requires rust or python").into()),
                };
                if format.replace(value).is_some() {
                    return Err(invalid("duplicate --format").into());
                }
            }
            _ => return Err(invalid("unknown settings option; use --help").into()),
        }
    }
    let source = source.ok_or_else(|| invalid("--source is required"))?;
    let destination = destination.ok_or_else(|| invalid("--destination is required"))?;
    let format = format.unwrap_or(SettingsImportFormat::Rust);
    let has_approval =
        source_sha256.is_some() || target_sha256.is_some() || !accepted_skipped_keys.is_empty();
    if (dry_run || has_approval) && (!import || format != SettingsImportFormat::Python) {
        return Err(invalid("--preview and review approval require Python import").into());
    }
    if dry_run && has_approval {
        return Err(invalid("preview does not accept approval flags").into());
    }
    if import && format == SettingsImportFormat::Python {
        if source.exists()
            && destination.exists()
            && std::fs::canonicalize(&source)? == std::fs::canonicalize(&destination)?
        {
            return Err(
                invalid("Python source and Rust destination must be different files").into(),
            );
        }
        let bytes = std::fs::read(&source)?;
        let repository = JsonSettingsRepository::new(destination.clone());
        let (preview, review) = repository.review_python(&bytes)?;
        if dry_run {
            print_python_report(&preview, true, None);
            print_python_review(&review);
        } else {
            let approval = match (source_sha256, target_sha256) {
                (Some(source), Some(target)) => Some(PythonSettingsFileReview {
                    source_sha256: source,
                    target_settings_sha256: if target == "absent" {
                        None
                    } else {
                        Some(target)
                    },
                    imported_keys: preview.imported_keys.clone(),
                    skipped_keys: accepted_skipped_keys,
                }),
                (None, None) if !has_approval => None,
                _ => {
                    return Err(invalid(
                        "approval requires both SHA-256 values and the exact skipped-key list",
                    )
                    .into());
                }
            };
            if approval.is_none() && !preview.skipped_keys.is_empty() {
                return Err(invalid("unmapped Python settings require exact SHA-256 and --accept-skipped-key approval after preview").into());
            }
            if let Some(approved) = &approval {
                if *approved != review {
                    return Err(invalid(
                        "Python import approval does not match source, target or skipped keys",
                    )
                    .into());
                }
            }
            let source_backup = if preview.changed {
                Some(backup_python_source(&source, &destination, &bytes)?)
            } else {
                None
            };
            let result = match &approval {
                Some(approved) => repository.import_python_reviewed(&bytes, approved),
                None => repository.import_python(&bytes),
            };
            let applied = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    if let Some(path) = source_backup {
                        let _ = fs::remove_file(path);
                    }
                    return Err(error.into());
                }
            };
            print_python_report(&applied.preview, false, applied.backup_path.as_deref());
            if let Some(source_backup) = source_backup {
                println!("Python source backup: {}", source_backup.display());
            }
        }
    } else {
        let transfer = if import {
            let bytes = std::fs::read(&source)?;
            JsonSettingsRepository::new(destination).import_from(format, &bytes)?
        } else {
            JsonSettingsRepository::new(source).export_to(&destination)?
        };
        println!(
            "{} full Rust AppSettings (revision {})",
            if import { "Imported" } else { "Exported" },
            transfer.stored().revision
        );
    }
    Ok(())
}

fn string_argument(slot: &mut Option<String>, value: Option<OsString>) -> io::Result<()> {
    let value = value
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("option requires a value"))?;
    if slot.replace(value).is_some() {
        return Err(invalid("duplicate approval option"));
    }
    Ok(())
}

fn print_python_review(review: &PythonSettingsFileReview) {
    println!("Source SHA-256: {}", review.source_sha256);
    println!(
        "Target SHA-256: {}",
        review.target_settings_sha256.as_deref().unwrap_or("absent")
    );
    println!("Approve the exact skipped-key list before applying; no secret values are printed");
}

fn path_argument(slot: &mut Option<PathBuf>, value: Option<OsString>) -> io::Result<()> {
    let value = value
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("option requires a path"))?;
    if slot.replace(PathBuf::from(value)).is_some() {
        return Err(invalid("duplicate path option"));
    }
    Ok(())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn print_python_report(
    preview: &neuromita::settings::PythonSettingsPreview,
    dry_run: bool,
    backup: Option<&std::path::Path>,
) {
    println!(
        "Python settings {}: {} supported key(s), {} skipped key(s), changed={}, revision={}",
        if dry_run { "preview" } else { "import" },
        preview.imported_keys.len(),
        preview.skipped_keys.len(),
        preview.changed,
        preview.transfer.stored().revision
    );
    println!("Imported keys: {}", preview.imported_keys.join(", "));
    println!(
        "Skipped keys (not imported): {}",
        preview.skipped_keys.join(", ")
    );
    if let Some(backup) = backup {
        println!("Previous Rust settings backup: {}", backup.display());
    }
    if dry_run {
        println!("Dry-run: no destination or backup files were written");
    }
}

fn backup_python_source(source: &Path, target: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| invalid("Python source needs a filename"))?;
    let mut backup_name = OsString::from("python-source-");
    backup_name.push(name);
    backup_name.push(format!(".pre-python-import-{}.bak", uuid::Uuid::new_v4()));
    let directory = target
        .parent()
        .ok_or_else(|| invalid("Rust target needs a parent"))?;
    let backup_path = directory.join(backup_name);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut backup = options.open(&backup_path)?;
    if let Err(error) = backup.write_all(bytes).and_then(|_| backup.sync_all()) {
        drop(backup);
        let _ = fs::remove_file(&backup_path);
        return Err(error);
    }
    Ok(backup_path)
}
