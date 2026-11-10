use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::timeline::storage::SqliteTimelineRepository;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    IntoNew {
        backup: PathBuf,
        destination: PathBuf,
        fingerprint: String,
    },
    Help,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some(value) if value == "--help" => return Ok(Action::Help),
        Some(value) if value == "into-new" => {}
        _ => return Err("expected 'into-new' or '--help'".into()),
    }
    let (mut backup, mut destination, mut fingerprint) = (None, None, None);
    while let Some(argument) = args.next() {
        if argument == "--backup" {
            if backup.is_some() {
                return Err("duplicate --backup".into());
            }
            backup = Some(PathBuf::from(args.next().ok_or("--backup needs a path")?));
        } else if argument == "--output" {
            if destination.is_some() {
                return Err("duplicate --output".into());
            }
            destination = Some(PathBuf::from(args.next().ok_or("--output needs a path")?));
        } else if argument == "--confirm" {
            if fingerprint.is_some() {
                return Err("duplicate --confirm".into());
            }
            fingerprint = Some(
                args.next()
                    .ok_or("--confirm needs the backup fingerprint")?
                    .into_string()
                    .map_err(|_| "--confirm requires a UTF-8 fingerprint")?,
            );
        } else {
            return Err("unknown timeline-restore argument".into());
        }
    }
    let backup = backup.ok_or("explicit --backup is required")?;
    let destination = destination.ok_or("explicit --output is required")?;
    let fingerprint = fingerprint.ok_or("explicit --confirm is required")?;
    if !backup.is_absolute() || !backup.is_file() {
        return Err("--backup must name an existing absolute SQLite file".into());
    }
    if !destination.is_absolute() || destination.parent().is_none_or(|p| !p.is_dir()) {
        return Err("--output must name an absolute path in an existing directory".into());
    }
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("--confirm requires a 64-character hexadecimal fingerprint".into());
    }
    Ok(Action::IntoNew {
        backup,
        destination,
        fingerprint,
    })
}

pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    match parse(args).map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))? {
        Action::Help => {
            println!(
                "neuromita timeline-restore into-new --backup ABSOLUTE_BACKUP_SQLITE_PATH --output NEW_ABSOLUTE_SQLITE_PATH --confirm BACKUP_FINGERPRINT\n\nTrusted offline operator only. Obtain the fingerprint from 'timeline-backup create'. Requires a valid, consistent Timeline backup. Creates a new standalone SQLite file without overwriting any existing file. Does not switch the application database or restore external files."
            );
        }
        Action::IntoNew {
            backup,
            destination,
            fingerprint,
        } => {
            let receipt = SqliteTimelineRepository::restore_verified_backup_into_new_database(
                &backup,
                &destination,
                &fingerprint,
            )?;
            println!(
                "verified Timeline restored into new SQLite file: {}",
                destination.display()
            );
            println!("timeline revision: {}", receipt.revision);
            println!("canonical events: {}", receipt.canonical_events);
            println!("backup fingerprint: {}", receipt.backup_fingerprint);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_cli_requires_new_destination_and_pinned_backup_fingerprint() {
        let args = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse(args(&["--help"])), Ok(Action::Help));
        assert!(parse(args(&["into-new", "--backup", "relative.sqlite"])).is_err());
        assert!(parse(args(&["into-new", "--confirm", "abc"])).is_err());
        assert!(parse(args(&["into-new", "--output", "/tmp/output.sqlite"])).is_err());
        assert!(
            parse(args(&[
                "into-new",
                "--backup",
                "/tmp/missing",
                "--confirm",
                "0"
            ]))
            .is_err()
        );
        assert!(parse(args(&["create", "--backup", "/tmp/input.sqlite"])).is_err());
    }
}
