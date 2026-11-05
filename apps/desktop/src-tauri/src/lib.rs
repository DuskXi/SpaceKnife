//! SpaceKnife's desktop app: a Tauri window around the interface (apps/desktop/app, a Nuxt SPA), the
//! host API it calls as window.sk (commands.rs) and, soon, the backend in this process (backend.rs).

pub mod backend;
pub mod commands;
pub mod events;
pub mod files;
pub mod links;
pub mod portable;
pub mod terminal;
pub mod text;
pub mod theme;
pub mod update;
pub mod window;

/// A portable copy keeps its data next to the executable (on macOS next to the app bundle, also
/// when macOS runs a translocated copy of it): SPACEKNIFE_HOME points there for the whole process
/// (the backend included). Call first thing in main, before any thread starts.
pub fn prepare_environment() {
    let Ok(exe) = std::env::current_exe() else { return };
    let home = std::env::var_os("SPACEKNIFE_HOME");
    if let Some(dir) = portable::portable_home(&exe, std::env::consts::OS, !tauri::is_dev(), home.as_deref(), portable::original_path) {
        // SAFETY: called from main before the runtime or any other thread exists
        unsafe { std::env::set_var("SPACEKNIFE_HOME", dir) };
    }
}

pub fn run() {
    tauri::Builder::default()
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
            window::create(app.handle())?;
            backend::start(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("SpaceKnife could not start its window");
}
