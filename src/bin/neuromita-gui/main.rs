#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai_engine;
mod ai_hub;
mod asr;
mod binding;
mod capture;
mod chess;
mod launcher_ui;
mod memory;
mod presenter;
mod releases;
mod runtime;
mod sandbox;
mod settings;
mod speech_status;
mod update_check;
mod voice;
mod wiki;

mod view {
    slint::include_modules!();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--version")
    {
        println!("NeuroMita {}", neuromita::version::VERSION);
        return Ok(());
    }
    binding::run()
}
