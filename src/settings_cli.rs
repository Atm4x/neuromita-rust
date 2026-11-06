use std::{
    error::Error,
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use neuromita::settings::{
    AppSettings, JsonSettingsRepository, PythonSettingsMigration, SettingsImportFormat,
    SettingsRepository, SettingsTransferError, StoredSettings,
};
use uuid::Uuid;

pub const HELP: &str = "neuromita settings export --source STORE.json --destination BACKUP.json\n\
    neuromita settings import --source BACKUP.json --destination STORE.json [--format rust|python]\n\
    neuromita settings import --format python --source PYTHON_SETTINGS.json --destination RUST_SETTINGS.json --preview\n\
    neuromita settings import --format python --source PYTHON_SETTINGS.json --destination RUST_SETTINGS.json [--allow-unmapped]\n\n\
    Offline operations: no Core and no GEMINI_API_KEY. Stop the application first.\n\
    Rust imports use the current strict dev format; no dev migration chain.\n\
    Python import maps only reviewed keys; unmapped keys require --allow-unmapped.\n\
    Preview has no side effects. Applying backs up source and existing destination next to the originals.\n\
    Backups may contain plaintext credentials; keep them private.\n\
    API preset bodies, credentials, Timeline and models are not imported.\n";

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
    let mut preview = false;
    let mut allow_unmapped = false;
    while let Some(option) = arguments.next() {
        if option == "--help" {
            print!("{HELP}");
            return Ok(());
        }
        match option.to_str() {
            Some("--source") => path_argument(&mut source, arguments.next())?,
            Some("--destination") => path_argument(&mut destination, arguments.next())?,
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
            Some("--preview") if import && !preview => preview = true,
            Some("--allow-unmapped") if import && !allow_unmapped => allow_unmapped = true,
            _ => return Err(invalid("unknown settings option; use --help").into()),
        }
    }
    let source = source.ok_or_else(|| invalid("--source is required"))?;
    let destination = destination.ok_or_else(|| invalid("--destination is required"))?;
    let format = format.unwrap_or(SettingsImportFormat::Rust);
    if preview || allow_unmapped {
        if !import || format != SettingsImportFormat::Python {
            return Err(invalid("--preview and --allow-unmapped require Python import").into());
        }
    }
    if import && format == SettingsImportFormat::Python {
        return import_python(&source, &destination, preview, allow_unmapped);
    }
    let transfer = if import {
        let bytes = fs::read(&source)?;
        JsonSettingsRepository::new(destination).import_from(format, &bytes)?
    } else {
        JsonSettingsRepository::new(source).export_to(&destination)?
    };
    println!(
        "{} full Rust AppSettings (revision {})",
        if import { "Imported" } else { "Exported" },
        transfer.stored().revision
    );
    Ok(())
}

fn import_python(
    source: &Path,
    destination: &Path,
    preview_only: bool,
    allow_unmapped: bool,
) -> Result<(), Box<dyn Error>> {
    let bytes = fs::read(source)?;
    let plan = PythonSettingsMigration::preview(&bytes)?;
    println!("Python mapped keys: {}", plan.mapped_keys.join(", "));
    println!(
        "Python unmapped keys (not imported): {}",
        if plan.unmapped_keys.is_empty() {
            "none".to_owned()
        } else {
            plan.unmapped_keys.join(", ")
        }
    );
    if preview_only {
        println!("Preview only: no settings or backups were written");
        return Ok(());
    }
    if plan.mapped_keys.is_empty() {
        return Err(SettingsTransferError::NotFullSettingsTransfer(
            "Python settings have no supported fields to import".into(),
        )
        .into());
    }
    if !allow_unmapped && !plan.unmapped_keys.is_empty() {
        return Err(SettingsTransferError::UnmappedPythonFields(plan.unmapped_keys).into());
    }
    let source_path = source.canonicalize()?;
    let existing_bytes = match fs::read(destination) {
        Ok(bytes) => {
            if source_path == destination.canonicalize()? {
                return Err(invalid("source and destination must be different files").into());
            }
            Some(bytes)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let repository = JsonSettingsRepository::new(destination.to_path_buf());
    let stored = match &existing_bytes {
        Some(_) => repository.load()?,
        None => StoredSettings {
            revision: 0,
            settings: AppSettings::initial_profile(),
        },
    };
    let imported = plan.apply_to(&stored)?;
    if existing_bytes.is_some() && imported == stored {
        println!(
            "Already imported: settings unchanged (revision {})",
            stored.revision
        );
        return Ok(());
    }
    let source_backup = backup_bytes(source, &bytes)?;
    println!("Python source backup: {}", source_backup.display());
    if let Some(existing) = &existing_bytes {
        let destination_backup = backup_bytes(destination, existing)?;
        println!("Rust destination backup: {}", destination_backup.display());
    }
    repository.save(&imported)?;
    println!(
        "Imported reviewed Python settings ({} keys, revision {})",
        plan.mapped_keys.len(),
        imported.revision
    );
    Ok(())
}

/// The backup contains a snapshot of exactly the bytes validated for this import.
fn backup_bytes(original: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let name = original
        .file_name()
        .ok_or_else(|| invalid("backup source must have a file name"))?;
    let mut backup_name = name.to_os_string();
    backup_name.push(format!(".pre-python-import-{}.bak", Uuid::new_v4()));
    let destination = original.with_file_name(backup_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut backup = options.open(&destination)?;
    let write = backup.write_all(bytes).and_then(|()| backup.sync_all());
    if let Err(error) = write {
        drop(backup);
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(destination)
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
