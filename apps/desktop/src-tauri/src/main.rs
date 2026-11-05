// No console window behind the app in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    spaceknife_desktop::prepare_environment();
    spaceknife_desktop::run();
}
