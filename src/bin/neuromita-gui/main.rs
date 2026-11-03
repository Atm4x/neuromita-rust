#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod binding;
mod presenter;

mod view {
    slint::include_modules!();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    binding::run()
}
