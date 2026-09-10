#![cfg_attr(not(test), windows_subsystem = "windows")]

mod config;
mod engine;
mod platform;
mod ui;

fn main() {
    if let Err(error) = ui::run() {
        ui::show_error(&format!("CleanDL could not start: {error:#}"));
    }
}
