use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::recovery_inventory;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Inspect { root: PathBuf },
    Verify { root: PathBuf, manifest: PathBuf },
    Help,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let action = match args.next().as_deref() {
        Some(value) if value == "--help" => return Ok(Action::Help),
        Some(value) if value == "inspect" => "inspect",
        Some(value) if value == "verify" => "verify",
        _ => return Err("expected 'inspect', 'verify' or '--help'".into()),
    };
    let mut root = None;
    let mut manifest = None;
    while let Some(argument) = args.next() {
        if argument == "--root" && root.is_none() {
            root = Some(PathBuf::from(args.next().ok_or("--root requires a path")?));
        } else if argument == "--manifest" && manifest.is_none() && action == "verify" {
            manifest = Some(PathBuf::from(
                args.next().ok_or("--manifest requires a path")?,
            ));
        } else {
            return Err("unknown or duplicate recovery-inventory argument".into());
        }
    }
    let root = root.ok_or("--root is required")?;
    if !root.is_absolute() {
        return Err("--root must be absolute".into());
    }
    if action == "inspect" {
        return Ok(Action::Inspect { root });
    }
    let manifest = manifest.ok_or("--manifest is required")?;
    if !manifest.is_absolute() {
        return Err("--manifest must be absolute".into());
    }
    Ok(Action::Verify { root, manifest })
}

pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    let action =
        parse(args).map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    match action {
        Action::Help => println!(
            "neuromita recovery-inventory inspect --root ABSOLUTE_DATA_DIRECTORY\nneuromita recovery-inventory verify --root ABSOLUTE_DATA_DIRECTORY --manifest ABSOLUTE_JSON_PATH\n\nRead-only inventory of Timeline SQLite plus four known optional configuration JSON files. Output is not an application backup; missing owner APIs, SessionArchive and external assets remain unverified. Save inspect JSON separately and verify it against a stopped/offline installation. No restores or writes are performed."
        ),
        Action::Inspect { root } => {
            println!(
                "{}",
                recovery_inventory::json(&recovery_inventory::inspect(root)?)?
            );
        }
        Action::Verify { root, manifest } => {
            let expected = recovery_inventory::read_manifest(manifest)?;
            recovery_inventory::verify(root, &expected)?;
            println!(
                "recovery inventory verified: known files match; complete dataset recovery: false"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_arguments() {
        let args = |parts: &[&str]| parts.iter().map(OsString::from).collect::<Vec<OsString>>();
        assert_eq!(parse(args(&["--help"])), Ok(Action::Help));
        assert!(parse(args(&["inspect", "--root", "relative"])).is_err());
        assert!(parse(args(&["inspect", "--root", "/tmp", "--manifest", "/tmp/x"])).is_err());
        assert!(parse(args(&["verify", "--root", "/tmp"])).is_err());
        assert!(
            parse(args(&[
                "verify",
                "--root",
                "/tmp",
                "--manifest",
                "relative"
            ]))
            .is_err()
        );
        assert!(parse(args(&["inspect", "--root", "/tmp", "--root", "/tmp"])).is_err());
    }
}
