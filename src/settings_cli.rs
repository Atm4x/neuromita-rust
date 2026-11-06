use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::settings::{JsonSettingsRepository, SettingsImportFormat};

pub const HELP: &str = "neuromita settings export --source STORE.json --destination BACKUP.json\n\
    neuromita settings import --source BACKUP.json --destination STORE.json [--format rust|python] [--dry-run]\n\n\
    Offline operations: no Core and no GEMINI_API_KEY. Stop the application first.\n\
    Rust imports require the current strict settings format; no dev migrations.\n\
    The default format is rust. Python import overlays supported keys, lists omissions,\n\
    and creates a backup of the existing destination before writing.\n\
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
    while let Some(option) = arguments.next() {
        if option == "--help" {
            print!("{HELP}");
            return Ok(());
        }
        match option.to_str() {
            Some("--source") => path_argument(&mut source, arguments.next())?,
            Some("--destination") => path_argument(&mut destination, arguments.next())?,
            Some("--dry-run") if import && !dry_run => dry_run = true,
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
    if dry_run && (!import || format != SettingsImportFormat::Python) {
        return Err(invalid("--dry-run is only supported for Python import").into());
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
        let repository = JsonSettingsRepository::new(destination);
        if dry_run {
            let preview = repository.preview_python(&bytes)?;
            print_python_report(&preview, true, None);
        } else {
            let applied = repository.import_python(&bytes)?;
            print_python_report(&applied.preview, false, applied.backup_path.as_deref());
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
