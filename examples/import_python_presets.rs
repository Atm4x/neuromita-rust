use neuromita::settings::PythonPresetStage;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let dry_run = arguments.iter().any(|argument| argument == "--dry-run");
    arguments.retain(|argument| argument != "--dry-run");
    let preferences =
        if let Some(index) = arguments.iter().position(|arg| arg == "--python-settings") {
            if arguments.len() <= index + 1 {
                return Err("--python-settings requires a Python settings.json path".into());
            }
            let path = PathBuf::from(arguments.remove(index + 1));
            arguments.remove(index);
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("Python settings source must be a regular file".into());
            }
            Some(std::fs::read(path)?)
        } else {
            None
        };
    let existing = if let Some(index) = arguments.iter().position(|arg| arg == "--existing-dir") {
        if arguments.len() <= index + 1 {
            return Err("--existing-dir requires a directory".into());
        }
        let directory = PathBuf::from(arguments.remove(index + 1));
        arguments.remove(index);
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("--existing-dir must be a real directory".into());
        }
        Some(directory)
    } else {
        None
    };
    if arguments.len() != 3 {
        return Err("usage: import_python_presets <api_presets.json> <api_templates.json> <new-output-directory> [--dry-run] [--existing-dir DIR] [--python-settings PYTHON_SETTINGS.json]".into());
    }
    let plan = PythonPresetStage::prepare(
        &std::fs::read(&arguments[0])?,
        &std::fs::read(&arguments[1])?,
    )?;
    let plan = if let Some(directory) = existing {
        let existing_catalog = read_existing(&directory.join("presets.json"))?;
        let existing_credentials = read_existing(&directory.join("preset-credentials.json"))?;
        if existing_catalog.is_none() && existing_credentials.is_none() {
            return Err("existing directory has no preset or credential store".into());
        }
        plan.merge_with_existing(existing_catalog.as_deref(), existing_credentials.as_deref())?
    } else {
        plan
    };
    let plan = if let Some(ref bytes) = preferences {
        let (plan, review) = plan.with_preferences(bytes)?;
        println!(
            "Python selected user preset: {:?}; display order: {:?}; applied: false",
            review.selected_user_id, review.user_order
        );
        plan
    } else {
        plan
    };
    println!(
        "Convertible presets: {}; blocked presets: {}; template descriptors: {}",
        plan.preview.converted_ids.len(),
        plan.preview.blocked_count(),
        plan.preview.template_count
    );
    println!("Convertible preset IDs: {:?}", plan.preview.converted_ids);
    if !plan.preview.preserved_ids.is_empty() {
        println!("Preserved existing IDs: {:?}", plan.preview.preserved_ids);
    }
    println!(
        "Blocked numeric IDs: {:?}; invalid IDs: {}",
        plan.preview.blocked_ids, plan.preview.blocked_without_id
    );
    if plan.preview.python_order_imported {
        println!(
            "Python custom user order is stored in the Rust preset catalog; the exact Python source is also preserved."
        );
    }
    if dry_run {
        println!("Dry-run: no files written");
        return Ok(());
    }
    plan.stage_new_directory(&PathBuf::from(&arguments[2]))?;
    println!("Published verified import into a new directory; Python sources unchanged");
    Ok(())
}

fn read_existing(path: &std::path::Path) -> Result<Option<Vec<u8>>, std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "existing store must be a regular file, not a symlink",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
