use std::{error::Error, ffi::OsString, io, path::PathBuf};

use neuromita::recovery_snapshot;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Create {
        root: PathBuf,
        destination: PathBuf,
    },
    Verify {
        snapshot: PathBuf,
    },
    Restore {
        snapshot: PathBuf,
        destination: PathBuf,
    },
    Help,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Action, String> {
    let mut args = args.into_iter();
    let verb = match args.next().as_deref() {
        Some(v) if v == "--help" => return Ok(Action::Help),
        Some(v) if v == "create" => "create",
        Some(v) if v == "verify" => "verify",
        Some(v) if v == "restore" => "restore",
        _ => return Err("expected 'create', 'verify', 'restore' or '--help'".into()),
    };
    let mut root = None;
    let mut output = None;
    let mut snapshot = None;
    while let Some(flag) = args.next() {
        let value = PathBuf::from(args.next().ok_or("missing path after option")?);
        if !value.is_absolute() {
            return Err("all paths must be absolute".into());
        }
        match (verb, flag.to_str()) {
            ("create", Some("--root")) if root.is_none() => root = Some(value),
            ("create", Some("--output")) if output.is_none() => output = Some(value),
            ("verify", Some("--snapshot")) | ("restore", Some("--snapshot"))
                if snapshot.is_none() =>
            {
                snapshot = Some(value)
            }
            ("restore", Some("--output")) if output.is_none() => output = Some(value),
            _ => return Err("unknown or duplicate recovery-snapshot option".into()),
        }
    }
    if verb == "create" {
        Ok(Action::Create {
            root: root.ok_or("--root is required")?,
            destination: output.ok_or("--output is required")?,
        })
    } else if verb == "verify" {
        Ok(Action::Verify {
            snapshot: snapshot.ok_or("--snapshot is required")?,
        })
    } else {
        Ok(Action::Restore {
            snapshot: snapshot.ok_or("--snapshot is required")?,
            destination: output.ok_or("--output is required")?,
        })
    }
}

pub fn run(args: Vec<OsString>) -> Result<(), Box<dyn Error>> {
    match parse(args).map_err(|reason| io::Error::new(io::ErrorKind::InvalidInput, reason))? {
        Action::Help => println!(
            "neuromita recovery-snapshot create --root ABSOLUTE_DATA_DIRECTORY --output NEW_ABSOLUTE_DIRECTORY\nneuromita recovery-snapshot verify --snapshot ABSOLUTE_DIRECTORY\nneuromita recovery-snapshot restore --snapshot VERIFIED_ABSOLUTE_DIRECTORY --output NEW_ABSOLUTE_DATA_DIRECTORY\n\nOffline operator only. Stop all writers. Captures a verified SQLite Timeline plus four optional configuration JSONs, preserving their contents. This is an incomplete known-file snapshot: NO SessionArchive, models, external assets, cross-owner lock or live restore. Output cannot overwrite an existing directory."
        ),
        Action::Create { root, destination } => {
            let manifest = recovery_snapshot::create(&root, &destination)?;
            println!("published known-files snapshot: {}", destination.display());
            println!("timeline fingerprint: {}", manifest.timeline.fingerprint);
            println!("complete dataset recovery: false");
        }
        Action::Restore {
            snapshot,
            destination,
        } => {
            let manifest = recovery_snapshot::restore_into_new(&snapshot, &destination)?;
            println!(
                "restored known files into new data directory: {}",
                destination.display()
            );
            println!("timeline fingerprint: {}", manifest.timeline.fingerprint);
            println!("complete dataset recovery: false");
        }
        Action::Verify { snapshot } => {
            let manifest = recovery_snapshot::verify(&snapshot)?;
            println!("known-files snapshot verified: {}", snapshot.display());
            println!("timeline fingerprint: {}", manifest.timeline.fingerprint);
            println!("complete dataset recovery: false");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_create_and_verify_options() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(parse(args(&["--help"])), Ok(Action::Help));
        for bad in [
            vec!["create", "--root", "/tmp/data"],
            vec!["create", "--root", "relative", "--output", "/tmp/new"],
            vec![
                "create", "--root", "/tmp", "--root", "/tmp", "--output", "/tmp/out",
            ],
            vec!["verify", "--snapshot", "/tmp", "--root", "/tmp"],
            vec!["verify", "--snapshot", "relative"],
            vec!["restore", "--snapshot", "/tmp/source"],
            vec![
                "restore",
                "--snapshot",
                "/tmp/source",
                "--output",
                "relative",
            ],
            vec![
                "restore",
                "--snapshot",
                "/tmp/source",
                "--output",
                "/tmp/out",
                "--root",
                "/tmp",
            ],
        ] {
            assert!(parse(args(&bad)).is_err(), "{bad:?}");
        }
    }
}
