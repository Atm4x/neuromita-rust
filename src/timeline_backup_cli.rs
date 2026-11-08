use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::timeline::storage::SqliteTimelineRepository;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Create {
        source: PathBuf,
        destination: PathBuf,
    },
    Help,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    match args.next().as_deref() {
        Some(value) if value == "--help" => return Ok(Action::Help),
        Some(value) if value == "create" => {}
        _ => return Err("expected 'create' or '--help'".into()),
    }
    let (mut source, mut destination) = (None, None);
    while let Some(argument) = args.next() {
        if argument == "--database" {
            if source.is_some() {
                return Err("duplicate --database".into());
            }
            source = Some(PathBuf::from(args.next().ok_or("--database needs a path")?));
        } else if argument == "--output" {
            if destination.is_some() {
                return Err("duplicate --output".into());
            }
            destination = Some(PathBuf::from(args.next().ok_or("--output needs a path")?));
        } else {
            return Err("unknown timeline-backup argument".into());
        }
    }
    let source = source.ok_or("explicit --database is required")?;
    let destination = destination.ok_or("explicit --output is required")?;
    if !source.is_absolute() || !source.is_file() {
        return Err("--database must name an existing absolute SQLite file".into());
    }
    if !destination.is_absolute() || destination.parent().is_none_or(|p| !p.is_dir()) {
        return Err("--output must name an absolute path in an existing directory".into());
    }
    Ok(Action::Create {
        source,
        destination,
    })
}

pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    match parse(args).map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))? {
        Action::Help => {
            println!(
                "neuromita timeline-backup create --database ABSOLUTE_SQLITE_PATH --output NEW_ABSOLUTE_SQLITE_PATH\n\nTrusted offline operator only. Stop other writers. Creates a validated, standalone SQLite backup including committed WAL frames. Never overwrites an existing file. Does not back up external SessionArchive, model assets, or other application files."
            );
        }
        Action::Create {
            source,
            destination,
        } => {
            let receipt = SqliteTimelineRepository::create_verified_backup(&source, &destination)?;
            println!(
                "verified SQLite Timeline backup created: {}",
                destination.display()
            );
            println!("timeline revision: {}", receipt.revision);
            println!("canonical events: {}", receipt.canonical_events);
            println!("source fingerprint: {}", receipt.source_fingerprint);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeline_backup_cli_rejects_missing_and_relative_arguments() {
        let args = |items: &[&str]| items.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse(args(&["--help"])), Ok(Action::Help));
        assert!(
            parse(args(&[
                "create",
                "--database",
                "./relative.db",
                "--output",
                "/tmp/out.db"
            ]))
            .is_err()
        );
        assert!(
            parse(args(&[
                "create",
                "--database",
                "/nonexistent.sqlite",
                "--output",
                "/tmp/out.db"
            ]))
            .is_err()
        );
        assert!(parse(args(&["create", "--output", "/tmp/out.db"])).is_err());
        assert!(
            parse(args(&[
                "create",
                "--output",
                "/tmp/out.db",
                "--output",
                "/tmp/out2.db"
            ]))
            .is_err()
        );
    }
}
