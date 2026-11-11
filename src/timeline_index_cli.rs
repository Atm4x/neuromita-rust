use std::{error::Error, ffi::OsString, path::PathBuf};

use neuromita::timeline::storage::{DerivedIndexPreview, SqliteTimelineRepository};

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Inspect(PathBuf),
    Repair(PathBuf, String),
    Help,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let operation = args.next().ok_or("expected 'inspect' or 'repair'")?;
    if operation == "--help" {
        return Ok(Action::Help);
    }
    let operation = operation.to_str().ok_or("operation must be UTF-8")?;
    if !matches!(operation, "inspect" | "repair") {
        return Err("expected 'inspect' or 'repair'".into());
    }
    let mut database = None;
    let mut confirmation = None;
    while let Some(argument) = args.next() {
        if argument == "--database" {
            if database.is_some() {
                return Err("duplicate --database".into());
            }
            database = Some(PathBuf::from(args.next().ok_or("--database needs a path")?));
        } else if argument == "--confirm" {
            if confirmation.is_some() {
                return Err("duplicate --confirm".into());
            }
            confirmation = Some(
                args.next()
                    .ok_or("--confirm needs a fingerprint")?
                    .into_string()
                    .map_err(|_| "confirmation must be UTF-8")?,
            );
        } else {
            return Err("unknown timeline-index argument".into());
        }
    }
    let path = database.ok_or("explicit --database PATH is required")?;
    if !path.is_absolute() || !path.is_file() {
        return Err("--database must name an existing absolute SQLite file path".into());
    }
    match (operation, confirmation) {
        ("inspect", None) => Ok(Action::Inspect(path)),
        ("inspect", Some(_)) => Err("inspect cannot accept --confirm".into()),
        ("repair", Some(token))
            if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            Ok(Action::Repair(path, token))
        }
        ("repair", _) => {
            Err("repair requires a 64-character --confirm FINGERPRINT from inspect".into())
        }
        _ => unreachable!(),
    }
}

fn display(preview: &DerivedIndexPreview) {
    println!("timeline revision: {}", preview.revision);
    println!("canonical events: {}", preview.canonical_events);
    for drift in &preview.drift {
        println!(
            "{}: exists={} expected={} actual={} missing={} unexpected={}",
            drift.table,
            drift.exists,
            drift.expected_rows,
            drift.actual_rows,
            drift.missing_rows,
            drift.unexpected_rows
        );
    }
    println!("repair needed: {}", preview.requires_repair());
    println!("confirmation fingerprint: {}", preview.confirmation);
}

pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    match parse(args)
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?
    {
        Action::Help => {
            println!(
                "neuromita timeline-index inspect --database ABSOLUTE_SQLITE_PATH\nneuromita timeline-index repair --database ABSOLUTE_SQLITE_PATH --confirm FINGERPRINT\n\nTrusted local operator only. Stop the application first. Inspect is read-only; repair rebuilds only derived Timeline lookup indexes from canonical events. The fingerprint is an optimistic concurrency guard, not authentication. No automatic repair or canonical event recovery."
            );
        }
        Action::Inspect(path) => {
            let preview = SqliteTimelineRepository::inspect_derived_indexes(path)?;
            display(&preview);
        }
        Action::Repair(path, fingerprint) => {
            let after =
                SqliteTimelineRepository::rebuild_derived_indexes_confirmed(path, &fingerprint)?;
            println!("confirmed derived-index repair completed");
            display(&after);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn maintenance_cli_requires_existing_absolute_file_and_explicit_confirmation() {
        assert!(parse(args(&["inspect", "--database", "/does-not-exist.db"])).is_err());
        assert!(parse(args(&["repair", "--database", "./relative.db"])).is_err());
        assert!(parse(args(&["inspect", "--database", "x", "--confirm", "123"])).is_err());
        assert!(parse(args(&["repair", "--database", "x", "--confirm", "123"])).is_err());
        assert!(parse(args(&["inspect", "--database", "x", "--database", "y"])).is_err());
        assert!(parse(args(&["--help"])).is_ok());
    }
}
