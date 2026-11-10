use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::timeline::storage::SqliteTimelineRepository;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Create {
        source: PathBuf,
        destination: PathBuf,
    },
    Inspect {
        source: PathBuf,
        expected: Option<String>,
    },
    Help,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let action = match args.next().as_deref() {
        Some(value) if value == "--help" => return Ok(Action::Help),
        Some(value) if value == "create" => "create",
        Some(value) if value == "inspect" => "inspect",
        _ => return Err("expected 'create', 'inspect' or '--help'".into()),
    };
    let (mut source, mut destination, mut expected) = (None, None, None);
    while let Some(argument) = args.next() {
        if argument == "--database" {
            if source.is_some() {
                return Err("duplicate --database".into());
            }
            source = Some(PathBuf::from(args.next().ok_or("--database needs a path")?));
        } else if argument == "--output" && action == "create" {
            if destination.is_some() {
                return Err("duplicate --output".into());
            }
            destination = Some(PathBuf::from(args.next().ok_or("--output needs a path")?));
        } else if argument == "--expect" && action == "inspect" {
            if expected.is_some() {
                return Err("duplicate --expect".into());
            }
            let fingerprint = args
                .next()
                .ok_or("--expect needs a fingerprint")?
                .into_string()
                .map_err(|_| "--expect requires a UTF-8 fingerprint")?;
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("--expect requires a 64-character hexadecimal fingerprint".into());
            }
            expected = Some(fingerprint.to_ascii_lowercase());
        } else {
            return Err("unknown timeline-backup argument".into());
        }
    }
    let source = source.ok_or("explicit --database is required")?;
    if !source.is_absolute() || !source.is_file() {
        return Err("--database must name an existing absolute SQLite file".into());
    }
    if action == "inspect" {
        return Ok(Action::Inspect { source, expected });
    }
    let destination = destination.ok_or("explicit --output is required")?;
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
                "neuromita timeline-backup create --database ABSOLUTE_SQLITE_PATH --output NEW_ABSOLUTE_SQLITE_PATH\nneuromita timeline-backup inspect --database ABSOLUTE_SQLITE_PATH [--expect 64_HEX_FINGERPRINT]\n\nTrusted offline operator only. Stop other writers. Create: validated standalone SQLite backup including committed WAL frames; never overwrites. Inspect: read-only integrity, canonical/derived and optional fingerprint validation of an existing SQLite Timeline file. Does not authenticate its origin or cover SessionArchive or other application assets."
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
        Action::Inspect { source, expected } => {
            let receipt = SqliteTimelineRepository::inspect_verified_snapshot(&source)?;
            if expected.is_some_and(|fingerprint| fingerprint != receipt.source_fingerprint) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "snapshot fingerprint mismatch",
                )
                .into());
            }
            println!(
                "verified existing SQLite Timeline snapshot: {}",
                source.display()
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
        assert!(parse(args(&["inspect", "--database", "./relative.sqlite"])).is_err());
        assert!(
            parse(args(&[
                "inspect",
                "--database",
                "/tmp/missing.sqlite",
                "--expect",
                "deadbeef"
            ]))
            .is_err()
        );
        assert!(parse(args(&["inspect", "--output", "/tmp/x"])).is_err());
        assert!(parse(args(&["create", "--expect", "something"])).is_err());
    }
}
