//! Isothrum desktop binary: no console window on Windows in release, and start-up failures are
//! returned so Cargo/the OS report them instead of a panic.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    isothrum_desktop_lib::run()
}
