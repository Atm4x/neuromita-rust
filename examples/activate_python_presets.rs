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
    if allow_unmapped
        && !matches!(args.as_slice(), [action, _, _] if action == "--apply-with-imported-settings")
    {
        return Err("--allow-unmapped only applies to --apply-with-imported-settings".into());
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
                    PythonUnmappedPolicy::SkipAfterReview
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
        _ => return Err("usage: activate_python_presets --apply REVIEW_DIR ACTIVE_USERDATA_DIR --confirm-stopped | --apply-with-preferences REVIEW_DIR ACTIVE_USERDATA_DIR --confirm-stopped | --apply-with-imported-settings REVIEW_DIR ACTIVE_USERDATA_DIR --confirm-stopped [--allow-unmapped] | --recover ACTIVE_USERDATA_DIR --confirm-stopped".into()),
    }
    Ok(())
}
