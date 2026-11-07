//! Standalone updater helper. Install alongside the GUI executable.
//! It never accepts an arbitrary target path or URL.

use std::process::ExitCode;

fn main() -> ExitCode {
    match neuromita::updater::handoff::run_helper(std::env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("NeuroMita update failed: {error}");
            ExitCode::FAILURE
        }
    }
}
