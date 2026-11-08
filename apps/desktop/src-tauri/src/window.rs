//! The one window: frameless (the page draws its own title bar, components/shell/TitleBar.vue), in
//! the theme of the last start from its first frame, telling the page when it is maximized.
//! (Was createWindow and restoreTheme in apps/electron/src/main/index.ts.)

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::webview::NewWindowResponse;
use tauri::window::Color;
use tauri::{
    AppHandle, Emitter, Manager, Theme, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use tauri_plugin_opener::OpenerExt;

use crate::commands::Themes;
use crate::events::MAXIMIZED;
use crate::theme::{ThemeHost, ThemeKeeper, ThemeSetting};

pub const MAIN: &str = "main";

/// Where the shell keeps its own files (theme.json, the webview's profile): `<SPACEKNIFE_HOME>/desktop`
/// for an isolated home (demos, tests, portable copies), else the app's local data folder; apart
/// from the backend's files.
pub fn ui_dir(app: &AppHandle) -> PathBuf {
    match std::env::var_os("SPACEKNIFE_HOME").filter(|h| !h.is_empty()) {
        Some(home) => PathBuf::from(home).join("desktop"),
        None => app
            .path()
            .app_local_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("SpaceKnife")),
    }
}

/// "#RRGGBB" as a colour; None for anything else.
pub fn parse_hex(hex: &str) -> Option<Color> {
    let h = hex.strip_prefix('#').filter(|h| h.len() == 6)?;
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Color(byte(0)?, byte(2)?, byte(4)?, 255))
}

const fn tauri_theme(theme: ThemeSetting) -> Option<Theme> {
    match theme {
        ThemeSetting::System => None,
        ThemeSetting::Light => Some(Theme::Light),
        ThemeSetting::Dark => Some(Theme::Dark),
    }
}

/// The keeper's view of the window and of theme.json.
pub struct TauriThemeHost {
    app: AppHandle,
    file: PathBuf,
}

impl TauriThemeHost {
    fn window(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window(MAIN)
    }
}

impl ThemeHost for TauriThemeHost {
    fn set_source(&mut self, theme: ThemeSetting) {
        // before the window exists the builder takes it (create)
        if let Some(w) = self.window() {
            let _ = w.set_theme(tauri_theme(theme));
        }
    }

    fn dark(&self) -> bool {
        self.window().and_then(|w| w.theme().ok()) == Some(Theme::Dark)
    }

    fn set_background(&mut self, color: &str) {
        if let (Some(w), Some(c)) = (self.window(), parse_hex(color)) {
            let _ = w.set_background_color(Some(c));
        }
    }

    fn write(&mut self, text: &str) -> std::io::Result<()> {
        write_whole(&self.file, text)
    }

    fn warn(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

/// Whole or not at all: a start never reads half a file.
fn write_whole(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

/// Create the window, with the theme theme.json kept, and manage the theme keeper.
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let dir = ui_dir(app);
    let file = dir.join("theme.json");
    let text = std::fs::read_to_string(&file).ok();
    let keeper = ThemeKeeper::new(
        TauriThemeHost {
            app: app.clone(),
            file,
        },
        text.as_deref(),
    );
    let setting = keeper.setting();
    // light where nothing tells yet (「跟随系统」): the system's once the window exists, below
    let first_bg = if setting == ThemeSetting::Dark {
        crate::theme::WINDOW_BG_DARK
    } else {
        crate::theme::WINDOW_BG_LIGHT
    };

    // only the interface's own pages (navigation.rs); a new window never opens, a page of the
    // project goes to the browser (the help page sends its links to open_external; this catches
    // what slips past it)
    let dev = tauri::is_dev()
        .then(|| app.config().build.dev_url.clone())
        .flatten();
    let opener = app.clone();
    let builder = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::default())
        .on_navigation(move |url| {
            let own = crate::navigation::is_own_page(url, dev.as_ref());
            if !own {
                eprintln!("[window] not going to {url}: not a page of the interface");
            }
            own
        })
        .on_new_window(move |url, _features| {
            match crate::links::project_page(url.as_str()) {
                Some(page) => {
                    if let Err(err) = opener.opener().open_url(page, None::<&str>) {
                        eprintln!("[shell] could not open {url}: {err}");
                    }
                }
                None => eprintln!("[window] no window for {url}"),
            }
            NewWindowResponse::Deny
        })
        .title("SpaceKnife")
        .inner_size(1440.0, 900.0)
        .min_inner_size(1024.0, 640.0)
        .theme(tauri_theme(setting))
        .background_color(parse_hex(first_bg).unwrap_or(Color(0, 0, 0, 255)))
        .data_directory(dir.join("webview"));
    // macOS keeps its traffic lights over the page's title bar (TitleBar.vue leaves them room);
    // elsewhere the window has no frame at all and the page draws the window buttons
    #[cfg(target_os = "macos")]
    let builder = builder
        .decorations(true)
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    #[cfg(not(target_os = "macos"))]
    let builder = builder.decorations(false);
    let window = builder.build()?;

    app.manage::<Themes>(std::sync::Mutex::new(keeper));
    app.state::<Themes>()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .updated();

    // SPACEKNIFE_START_MINIMIZED=1 (milestone demos): appear in the taskbar without taking the focus
    if std::env::var("SPACEKNIFE_START_MINIMIZED").as_deref() == Ok("1") {
        let _ = window.minimize();
    }

    let maximized = Arc::new(AtomicBool::new(window.is_maximized().unwrap_or(false)));
    let w = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::Resized(_) => {
            let now = w.is_maximized().unwrap_or(false);
            if maximized.swap(now, Ordering::Relaxed) != now {
                let _ = w.emit(MAXIMIZED, now);
            }
        }
        // the system's theme changed: the window's own background follows
        WindowEvent::ThemeChanged(_) => {
            w.state::<Themes>()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .updated();
        }
        // a finished scan's flash stops once the window is looked at
        WindowEvent::Focused(true) => {
            let _ = w.request_user_attention(None);
        }
        _ => {}
    });
    Ok(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_window_colours() {
        let c = parse_hex(crate::theme::WINDOW_BG_DARK).unwrap();
        assert_eq!((c.0, c.1, c.2, c.3), (0x08, 0x09, 0x0B, 255));
        assert!(parse_hex("08090B").is_none());
        assert!(parse_hex("#GG0000").is_none());
    }

    #[test]
    fn writes_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a").join("theme.json");
        write_whole(&file, "{}\n").unwrap();
        write_whole(&file, "{\"theme\":\"dark\"}\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"theme\":\"dark\"}\n"
        );
        assert!(!file.with_extension("json.tmp").exists());
    }
}
