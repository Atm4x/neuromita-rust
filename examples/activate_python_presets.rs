//! Explicit offline activation and recovery of reviewed Python preset data.
use neuromita::settings::{PresetActivation, PresetRecovery, PythonUnmappedPolicy};
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    if let Err(error) = run() {
        eprintln!("Preset activation failed: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let confirmed = args.iter().any(|a| a == "--confirm-stopped");
    args.retain(|a| a != "--confirm-stopped");
    let allow_unmapped = args.iter().any(|a| a == "--allow-unmapped");
    args.retain(|a| a != "--allow-unmapped");
    let mut approved_keys = Vec::new();
    let mut remaining = Vec::new();
    let mut values = args.into_iter();
    while let Some(value) = values.next() {
        if value == "--accept-skipped-key" {
            let key = values
                .next()
                .ok_or("--accept-skipped-key requires a name")?;
            let key = key
                .into_string()
                .map_err(|_| "skipped key names must be UTF-8")?;
            if key.is_empty() || approved_keys.contains(&key) {
                return Err("skipped key approval must be nonempty and unique".into());
            }
            approved_keys.push(key);
        } else {
            remaining.push(value);
        }
    }
    let args = remaining;
    if allow_unmapped
        && !matches!(args.as_slice(), [action, _, _] if action == "--apply-with-imported-settings")
    {
        return Err("--allow-unmapped only applies to --apply-with-imported-settings".into());
    }
    if !allow_unmapped && !approved_keys.is_empty() {
        return Err("--accept-skipped-key requires --allow-unmapped".into());
    }
    if !confirmed {
        return Err(
            "explicit --confirm-stopped required: close Python and Rust applications first".into(),
        );
    }
    match args.as_slice() {
        [action, review, active] if action == "--apply" => {
            PresetActivation::apply(&PathBuf::from(review), &PathBuf::from(active))?;
            println!("Both preset files activated; no live process was modified");
        }
        [action, review, active] if action == "--apply-with-preferences" => {
            PresetActivation::apply_with_preferences(&PathBuf::from(review), &PathBuf::from(active))?;
            println!("Presets, credentials and active user selection installed; no live process was modified");
        }
        [action, review, active] if action == "--apply-with-imported-settings" => {
            PresetActivation::apply_with_imported_settings(
                &PathBuf::from(review),
                &PathBuf::from(active),
                if allow_unmapped {
                    PythonUnmappedPolicy::SkipApprovedKeys(approved_keys)
                } else {
                    PythonUnmappedPolicy::Reject
                },
            )?;
            println!("Presets, credentials and reviewed Python settings merged in one recoverable transaction");
        }
        [action, active] if action == "--recover" => {
            let state = PresetActivation::recover(&PathBuf::from(active))?;
            match state {
                PresetRecovery::NoJournal => println!("No preset activation journal found"),
                PresetRecovery::RolledBack => println!("Restored previous preset files"),
                PresetRecovery::Committed => println!("Verified completed activation; kept installed presets"),
                PresetRecovery::DiscardedUnprepared => println!("Removed incomplete pre-activation journal; existing presets unchanged"),
            }
        }
        _ => return Err("usage: activate_python_presets --apply REVIEW_DIR ACTIVE_USERDATA_DIR --confirm-stopped | --apply-with-preferences REVIEW_DIR ACTIVE_USERDATA_DIR --confirm-stopped | --apply-with-imported-settings REVIEW_DIR ACTIVE_USERDATA_DIR --confirm-stopped [--allow-unmapped --accept-skipped-key NAME ...] | --recover ACTIVE_USERDATA_DIR --confirm-stopped".into()),
    }
    Ok(())
}
