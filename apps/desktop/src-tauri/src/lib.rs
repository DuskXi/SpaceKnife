//! SpaceKnife's desktop app: a Tauri window around the interface (apps/desktop/app, a Nuxt SPA), the
//! host API it calls as window.sk (commands.rs) and the backend in this process (backend.rs).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub mod backend;
pub mod cli;
pub mod commands;
pub mod console;
pub mod dialog;
pub mod events;
pub mod files;
pub mod install;
pub mod links;
pub mod navigation;
pub mod portable;
pub mod startup_check;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod update;
pub mod window;

/// The portable copy's data folder, when this is one (prepare_environment).
static PORTABLE: OnceLock<PathBuf> = OnceLock::new();

/// The data folder next to a portable copy's program; None for an installed copy.
pub fn portable_dir() -> Option<&'static Path> {
    PORTABLE.get().map(PathBuf::as_path)
}

/// A portable copy keeps its data next to the executable (on macOS next to the app bundle, also
/// when macOS runs a translocated copy of it): everything the process writes goes there, the
/// backend included (`sk_core::paths::set_home`, as SPACEKNIFE_HOME would; the environment is left
/// alone). The backend in this process needs no SPACEKNIFE_SHELL: unset is this shell, the Tauri
/// app (`sk_core::update::shell`; only the Electron shell passes `electron` to its backend).
/// Call first thing in main.
pub fn prepare_environment() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let home = std::env::var_os("SPACEKNIFE_HOME");
    if let Some(dir) = portable::portable_home(
        &exe,
        std::env::consts::OS,
        !tauri::is_dev(),
        home.as_deref(),
        portable::original_path,
    ) {
        sk_core::paths::set_home(dir.clone());
        let _ = PORTABLE.set(dir);
    }
}

/// The window could not be made (an old or broken web view runtime, most likely): the error in the
/// log, the user told why (startup_check.rs window_failed, dialog.rs), and the end, with 1. Tauri
/// panicked instead, without a word; nothing of the backend has started yet.
fn window_failed(error: &str) -> ! {
    tracing::error!("the window could not be made: {error}");
    dialog::tell(&startup_check::window_failed(error), dialog::Context::Start);
    std::process::exit(1)
}

pub fn run() {
    // Windows: grouped with the installer's (pinned) shortcuts, before any window exists
    window::set_app_user_model_id();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(backend::Backend::default())
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::backend_state,
            commands::window_minimize,
            commands::window_toggle_maximize,
            commands::window_close,
            commands::window_is_maximized,
            commands::window_flash,
            commands::show_item_in_folder,
            commands::open_path,
            commands::open_terminal,
            commands::copy_text,
            commands::open_full_disk_access,
            commands::open_external,
            commands::install_update,
            commands::set_language,
            commands::set_theme,
            commands::dialog_open_snapshot,
            commands::dialog_open_folders,
            commands::dialog_open_file,
            commands::dialog_save_file,
        ])
        .setup(|app| {
            console::set_app(app.handle());
            // an error here would be Tauri's panic (setup runs in the event loop)
            if let Err(err) = window::create(app.handle()) {
                window_failed(&err.to_string());
            }
            #[cfg(windows)]
            window::keep_event_target();
            backend::start(app.handle());
            Ok(())
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(err) => window_failed(&err.to_string()),
    };
    app.run(|app, event| {
        if let tauri::RunEvent::Exit = event {
            // sessions closed, scans stopped, the snapshot cache written
            backend::stop(app, Duration::from_secs(5));
        }
    });
}
