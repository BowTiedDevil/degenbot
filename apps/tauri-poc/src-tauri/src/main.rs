#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Err(error) = degenbot_tauri_poc::run() {
        eprintln!("failed to start degenbot feed PoC: {error}");
        std::process::exit(1);
    }
}
