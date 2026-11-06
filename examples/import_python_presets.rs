use neuromita::settings::PythonPresetStage;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let dry_run = arguments
        .last()
        .is_some_and(|argument| argument == "--dry-run");
    if dry_run {
        arguments.pop();
    }
    if arguments.len() != 3 {
        return Err("usage: import_python_presets <api_presets.json> <api_templates.json> <new-output-directory> [--dry-run]".into());
    }
    let plan = PythonPresetStage::prepare(
        &std::fs::read(&arguments[0])?,
        &std::fs::read(&arguments[1])?,
    )?;
    println!(
        "Convertible presets: {}; blocked presets: {}; template descriptors: {}",
        plan.preview.converted_ids.len(),
        plan.preview.blocked_count(),
        plan.preview.template_count
    );
    println!("Convertible preset IDs: {:?}", plan.preview.converted_ids);
    println!(
        "Blocked numeric IDs: {:?}; invalid IDs: {}",
        plan.preview.blocked_ids, plan.preview.blocked_without_id
    );
    if plan.preview.python_order_not_imported {
        println!(
            "Note: the Python custom display order is not represented in the Rust preset catalog; the exact source is preserved."
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
