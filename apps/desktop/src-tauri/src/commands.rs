//! The commands behind window.sk (app/shared/api.ts; the page side is app/lib/tauriHost.ts, which
//! names them). They do what Electron's main process did for the preload API
//! (apps/electron/src/main/index.ts registerIpc).

use std::sync::{Mutex, mpsc};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, UserAttentionType, WebviewWindow, Wry};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder, FilePath};
use tauri_plugin_opener::OpenerExt;

use crate::backend::{Backend, BackendState};
use crate::links::project_page;
use crate::text::{TextKey, mt, set_lang};
use crate::theme::ThemeKeeper;
use crate::window::TauriThemeHost;

/// The theme keeper, managed by Tauri.
pub type Themes = Mutex<ThemeKeeper<TauriThemeHost>>;

/// The platform as the page names it (SkApi.platform: Node's names).
pub fn page_platform(os: &str) -> &'static str {
    match os {
        "windows" => "win32",
        "macos" => "darwin",
        _ => "linux",
    }
}

#[derive(Serialize)]
pub struct Versions {
    tauri: String,
    webview: String,
}

/// What the page knows before anything else (lib/tauriHost.ts AppInfo).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    platform: &'static str,
    version: String,
    /// started by 「无缝更新」: the page says once that the update is done
    just_updated: bool,
    versions: Versions,
}

#[tauri::command]
pub fn app_info(app: AppHandle) -> AppInfo {
    AppInfo {
        platform: page_platform(std::env::consts::OS),
        version: app.package_info().version.to_string(),
        just_updated: std::env::args().any(|a| a == "--updated"),
        versions: Versions { tauri: tauri::VERSION.to_string(), webview: tauri::webview_version().unwrap_or_default() },
    }
}

#[tauri::command]
pub fn backend_state(backend: State<'_, Backend>) -> BackendState {
    backend.state()
}

// ------------------------------------------------------------------ window

#[tauri::command]
pub fn window_minimize(window: WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
pub fn window_toggle_maximize(window: WebviewWindow) {
    let _ = if window.is_maximized().unwrap_or(false) { window.unmaximize() } else { window.maximize() };
}

#[tauri::command]
pub fn window_close(window: WebviewWindow) {
    let _ = window.close();
}

#[tauri::command]
pub fn window_is_maximized(window: WebviewWindow) -> bool {
    window.is_maximized().unwrap_or(false)
}

/// Ask for attention (W7 常规 · 扫描结束提醒): taskbar flash / dock bounce / urgency hint, unless
/// the window has the focus; it stops when the window is focused (window.rs).
#[tauri::command]
pub fn window_flash(window: WebviewWindow) {
    if !window.is_focused().unwrap_or(true) {
        let _ = window.request_user_attention(Some(UserAttentionType::Informational));
    }
}

// ------------------------------------------------------------------ shell

#[tauri::command]
pub fn show_item_in_folder(path: String) {
    if let Err(err) = tauri_plugin_opener::reveal_item_in_dir(&path) {
        eprintln!("[shell] could not show {path}: {err}");
    }
}

/// Open a file or folder with its default application: "" when it opened, else why not.
#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> String {
    match app.opener().open_path(&path, None::<&str>) {
        Ok(()) => String::new(),
        Err(err) => err.to_string(),
    }
}

#[tauri::command]
pub fn open_terminal(dir: String) {
    crate::terminal::open_terminal(&dir);
}

#[tauri::command]
pub fn copy_text(app: AppHandle, text: String) {
    if let Err(err) = app.clipboard().write_text(text) {
        eprintln!("[shell] could not copy: {err}");
    }
}

/// macOS: System Settings › Privacy & Security › Full Disk Access; one fixed pane, not a URL from
/// the page (spaceknife.privacy.FULL_DISK_ACCESS_SETTINGS). Nothing elsewhere.
#[tauri::command]
pub fn open_full_disk_access(app: AppHandle) {
    if cfg!(target_os = "macos") {
        let _ = app.opener().open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles", None::<&str>);
    }
}

/// A page of the project (GitHub, the website) in the browser; anything else is refused.
#[tauri::command]
pub fn open_external(app: AppHandle, url: String) {
    match project_page(&url) {
        Some(page) => {
            if let Err(err) = app.opener().open_url(page, None::<&str>) {
                eprintln!("[shell] could not open {url}: {err}");
            }
        }
        None => eprintln!("[shell] not opening {url}: not a page of the project (GitHub, the website)"),
    }
}

/// One of the update card's two buttons (shared/install.ts installStep): "" when under way, else
/// why not.
#[tauri::command]
pub fn install_update(path: String, how: String) -> String {
    // TODO(update): port apps/electron/src/main/update.ts (installKind, installRefusal,
    // nsisUpdateArgs, swapPlanJson) and index.ts installUpdate for the Tauri bundles (NSIS/MSI,
    // AppImage, deb/rpm, dmg, portable). Until then no install kind has a step.
    eprintln!("[update] not installing {path} ({how}): no install step yet");
    mt(TextKey::UpdateNoInstall, &[])
}

#[tauri::command]
pub fn set_language(lang: String) {
    set_lang(&lang);
}

/// The page applied the theme setting to itself; here the window, the native UI and theme.json.
#[tauri::command]
pub fn set_theme(themes: State<'_, Themes>, theme: String) {
    themes.lock().unwrap_or_else(|e| e.into_inner()).set(&theme);
}

// ------------------------------------------------------------------ dialogs

#[derive(Deserialize)]
pub struct Filter {
    name: String,
    extensions: Vec<String>,
}

/// shared/api.ts FileDialogOptions (and defaultName for saving).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDialogOptions {
    title: String,
    filters: Vec<Filter>,
    #[serde(default)]
    default_name: Option<String>,
}

fn file_dialog(window: &WebviewWindow, title: &str, filters: &[Filter]) -> FileDialogBuilder<Wry> {
    let mut builder = window.app_handle().dialog().file().set_title(title).set_parent(window);
    for f in filters {
        let extensions: Vec<&str> = f.extensions.iter().map(String::as_str).collect();
        builder = builder.add_filter(&f.name, &extensions);
    }
    builder
}

/// Wait for a dialog's callback without holding up the async runtime.
async fn answer<T: Send + 'static>(start: impl FnOnce(mpsc::Sender<T>)) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    start(tx);
    tauri::async_runtime::spawn_blocking(move || rx.recv().ok()).await.ok().flatten()
}

fn path_text(path: FilePath) -> Option<String> {
    path.into_path().ok().map(|p| p.to_string_lossy().into_owned())
}

/// Pick a snapshot file (UI-W1-45, W4 「打开快照文件…」); null when cancelled.
#[tauri::command]
pub async fn dialog_open_snapshot(window: WebviewWindow) -> Option<String> {
    let filters = [Filter { name: mt(TextKey::SnapshotFilter, &[]), extensions: vec!["sks".into()] }];
    let dialog = file_dialog(&window, &mt(TextKey::OpenSnapshot, &[]), &filters);
    answer(|tx| dialog.pick_file(move |p| drop(tx.send(p)))).await.flatten().and_then(path_text)
}

/// Pick one or more folders (UI-W1-26 「浏览…」); [] when cancelled.
#[tauri::command]
pub async fn dialog_open_folders(window: WebviewWindow) -> Vec<String> {
    let dialog = file_dialog(&window, &mt(TextKey::PickFolders, &[]), &[]);
    let picked = answer(|tx| dialog.pick_folders(move |p| drop(tx.send(p)))).await.flatten();
    picked.unwrap_or_default().into_iter().filter_map(path_text).collect()
}

/// Pick one file to read (settings import, SpaceSniffer configuration); null when cancelled.
#[tauri::command]
pub async fn dialog_open_file(window: WebviewWindow, options: FileDialogOptions) -> Option<String> {
    let dialog = file_dialog(&window, &options.title, &options.filters);
    answer(|tx| dialog.pick_file(move |p| drop(tx.send(p)))).await.flatten().and_then(path_text)
}

/// Choose where to write a file (export, settings export); null when cancelled.
#[tauri::command]
pub async fn dialog_save_file(window: WebviewWindow, options: FileDialogOptions) -> Option<String> {
    let mut dialog = file_dialog(&window, &options.title, &options.filters);
    if let Some(name) = options.default_name.as_deref().filter(|n| !n.is_empty()) {
        dialog = dialog.set_file_name(name);
    }
    answer(|tx| dialog.save_file(move |p| drop(tx.send(p)))).await.flatten().and_then(path_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_platforms_as_the_page_does() {
        assert_eq!(page_platform("windows"), "win32");
        assert_eq!(page_platform("macos"), "darwin");
        assert_eq!(page_platform("linux"), "linux");
        assert_eq!(page_platform("freebsd"), "linux");
    }

    #[test]
    fn reads_the_pages_dialog_options() {
        let o: FileDialogOptions = serde_json::from_str(r#"{"title":"Export","filters":[{"name":"CSV","extensions":["csv"]}],"defaultName":"a.csv"}"#).unwrap();
        assert_eq!((o.title.as_str(), o.filters[0].extensions[0].as_str(), o.default_name.as_deref()), ("Export", "csv", Some("a.csv")));
        let o: FileDialogOptions = serde_json::from_str(r#"{"title":"Import","filters":[]}"#).unwrap();
        assert!(o.default_name.is_none());
    }
}
