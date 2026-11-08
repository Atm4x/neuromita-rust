//! Explicit offline activation and recovery of reviewed Python preset data.
use neuromita::settings::{
    PresetActivation, PresetRecovery, PythonSchemaDirectoryReview, PythonUnmappedPolicy,
};
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
    if args.iter().any(|a| a == "--allow-unmapped") {
        return Err("--allow-unmapped is unsafe for combined activation; use --review-imported-settings and exact --expect-source-sha256 / --accept-skipped-key confirmations".into());
    }
    let schema_source = take_option(&mut args, "--python-schemas-dir")?;
    let schemas = match schema_source {
        Some(source) => Some(PythonSchemaDirectoryReview::inspect(&PathBuf::from(
            source,
        ))?),
        None => None,
    };
    let expected_source = take_option(&mut args, "--expect-source-sha256")?;
    let expected_target = take_option(&mut args, "--expect-target-sha256")?;
    let mut accepted_skipped = Vec::new();
    while let Some(name) = take_option(&mut args, "--accept-skipped-key")? {
        accepted_skipped.push(name);
    }
    accepted_skipped.sort();
    if (expected_source.is_some() || expected_target.is_some() || !accepted_skipped.is_empty())
        && !matches!(args.as_slice(), [action, _, _] if action == "--apply-with-imported-settings")
    {
        return Err(
            "source and skipped-key confirmations only apply to --apply-with-imported-settings"
                .into(),
        );
    }
    if expected_source.is_some() != expected_target.is_some() {
        return Err(
            "both --expect-source-sha256 and --expect-target-sha256 are required for exact review"
                .into(),
        );
    }
    if !accepted_skipped.is_empty() && expected_source.is_none() {
        return Err(
            "--accept-skipped-key requires exact source and target digest confirmations".into(),
        );
    }
    if !confirmed {
        return Err(
            "explicit --confirm-stopped required: close Python and Rust applications first".into(),
        );
    }
    if !matches!(args.as_slice(), [action, _] if action == "--recover") && schemas.is_none() {
        return Err("--python-schemas-dir must explicitly identify the original Python APIModelSchemas directory, including when absent".into());
    }
    if matches!(args.as_slice(), [action, _] if action == "--recover") && schemas.is_some() {
        return Err(
            "recovery reads only the Rust transaction journal; omit --python-schemas-dir".into(),
        );
    }
    match args.as_slice() {
        [action, review, active] if action == "--apply" => {
            PresetActivation::apply_with_schemas(&PathBuf::from(review), &PathBuf::from(active), schemas.as_ref().unwrap())?;
            println!("Both preset files activated; no live process was modified");
        }
        [action, review, active] if action == "--apply-with-preferences" => {
            PresetActivation::apply_with_preferences_and_schemas(&PathBuf::from(review), &PathBuf::from(active), schemas.as_ref().unwrap())?;
            println!("Presets, credentials and active user selection installed; no live process was modified");
        }
        [action, review, active] if action == "--review-imported-settings" => {
            let inspected = PresetActivation::review_imported_settings_with_schemas(
                &PathBuf::from(review),
                &PathBuf::from(active),
                schemas.as_ref().unwrap(),
            )?;
            println!("Python settings source SHA-256: {}", inspected.source_sha256);
            println!("Rust target settings SHA-256: {}", inspected.target_settings_sha256);
            println!("Mapped keys ({}): {:?}", inspected.imported_keys.len(), inspected.imported_keys);
            println!("Skipped keys ({}): {:?}", inspected.skipped_keys.len(), inspected.skipped_keys);
            println!("Review only: no three-file activation performed");
        }
        [action, review, active] if action == "--apply-with-imported-settings" => {
            let review_path = PathBuf::from(review);
            let active_path = PathBuf::from(active);
            if let (Some(expected_source), Some(expected_target)) = (expected_source, expected_target) {
                let inspected = PresetActivation::review_imported_settings_with_schemas(&review_path, &active_path, schemas.as_ref().unwrap())?;
                if expected_source != inspected.source_sha256
                    || expected_target != inspected.target_settings_sha256
                    || accepted_skipped != inspected.skipped_keys
                {
                    return Err("review no longer matches the exact Python source digest and skipped key names; run --review-imported-settings again".into());
                }
                PresetActivation::apply_with_reviewed_settings_and_schemas(&review_path, &active_path, &inspected, schemas.as_ref().unwrap())?;
            } else {
                PresetActivation::apply_with_imported_settings_and_schemas(
                    &review_path,
                    &active_path,
                    PythonUnmappedPolicy::Reject,
                    schemas.as_ref().unwrap(),
                )?;
            }
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
        _ => return Err("usage: activate_python_presets --apply REVIEW ACTIVE --confirm-stopped --python-schemas-dir PYTHON_DIR | --apply-with-preferences REVIEW ACTIVE --confirm-stopped --python-schemas-dir PYTHON_DIR | --review-imported-settings REVIEW ACTIVE --confirm-stopped --python-schemas-dir PYTHON_DIR | --apply-with-imported-settings REVIEW ACTIVE --confirm-stopped --python-schemas-dir PYTHON_DIR [--expect-source-sha256 HEX --expect-target-sha256 HEX --accept-skipped-key NAME ...] | --recover ACTIVE --confirm-stopped".into()),
    }
    Ok(())
}

fn take_option(
    args: &mut Vec<std::ffi::OsString>,
    flag: &str,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(position) = args.iter().position(|value| value == flag) else {
        return Ok(None);
    };
    if position + 1 == args.len() {
        return Err(format!("{flag} requires one value").into());
    }
    let value = args
        .remove(position + 1)
        .into_string()
        .map_err(|_| format!("{flag} requires valid UTF-8"))?;
    args.remove(position);
    Ok(Some(value))
}
