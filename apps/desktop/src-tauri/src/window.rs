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

/// The AppUserModelID of the process, the identifier (tauri.conf.json): what the installer's
/// shortcuts carry (0.3.4's electron-builder ones and the forked NSIS template's, utils.nsh
/// SetLnkAppUserModelId), and what 0.3.4's Electron app set (app.setAppUserModelId). Without it
/// Windows derives an ID from the program's path, and the running window stands apart from the
/// pinned taskbar icon. Tauri, tao and wry set none.
pub const APP_USER_MODEL_ID: &str = "io.github.duskxi.spaceknife";

/// Give the process [`APP_USER_MODEL_ID`]: before the first window (lib.rs run), since a window
/// takes its ID when it is created. Windows only; no-op elsewhere.
pub fn set_app_user_model_id() {
    #[cfg(windows)]
    {
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn SetCurrentProcessExplicitAppUserModelID(id: *const u16) -> i32;
        }
        let id: Vec<u16> = APP_USER_MODEL_ID.encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated UTF-16 string that outlives the call (the system copies it)
        let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
        if hr < 0 {
            eprintln!("[window] could not set the AppUserModelID: HRESULT {hr:#010x}");
        }
    }
}

/// An isolated home: SPACEKNIFE_HOME (demos, tests), else a portable copy's data folder.
fn isolated_home() -> Option<PathBuf> {
    std::env::var_os("SPACEKNIFE_HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::portable_dir().map(std::path::Path::to_path_buf))
}

/// Where the shell keeps its own files (theme.json, the webview's profile): `<SPACEKNIFE_HOME>/desktop`
/// for an isolated home (demos, tests, portable copies), else the app's local data folder; apart
/// from the backend's files.
pub fn ui_dir(app: &AppHandle) -> PathBuf {
    match isolated_home() {
        Some(home) => home.join("desktop"),
        None => app
            .path()
            .app_local_data_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("SpaceKnife")),
    }
}

/// 0.3.4's Electron app's theme.json for this home (theme.rs electron_theme_file): the same home
/// 0.3.4 used, as 0.3.4 set SPACEKNIFE_HOME to a portable copy's data folder.
fn electron_theme_file(app: &AppHandle) -> Option<PathBuf> {
    let config = app.path().config_dir().ok();
    crate::theme::electron_theme_file(isolated_home().as_deref(), config.as_deref())
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

/// Create the window, with the theme theme.json kept (on the first start after 0.3.4, the theme
/// its Electron app kept), and manage the theme keeper.
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let dir = ui_dir(app);
    let file = dir.join("theme.json");
    let text = std::fs::read_to_string(&file).ok();
    let earlier = if text.is_none() {
        electron_theme_file(app).and_then(|f| std::fs::read_to_string(f).ok())
    } else {
        None
    };
    let keeper = ThemeKeeper::restore(
        TauriThemeHost {
            app: app.clone(),
            file,
        },
        text.as_deref(),
        earlier.as_deref(),
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
        // the window's theme changed (the system's, or set_theme's): its own background follows.
        // Not through the theme keeper: set_theme holds it while it changes the window's theme,
        // and this event can come inside that call, on the same thread (on Windows tao's
        // set_theme sends the window a message, and the event goes straight to this handler);
        // locking the keeper again there waits for ever (0.3.5: the window hung at the first
        // theme change, and at every start after it)
        WindowEvent::ThemeChanged(theme) => {
            let bg = crate::theme::window_background(*theme == Theme::Dark);
            if let Some(c) = parse_hex(bg) {
                let _ = w.set_background_color(Some(c));
            }
        }
        // a finished scan's flash stops once the window is looked at
        WindowEvent::Focused(true) => {
            let _ = w.request_user_attention(None);
        }
        _ => {}
    });
    Ok(window)
}

/// The class of tao's window for its event loop: a top-level window it shows (it needs WM_PAINT
/// during resizes; layered, so nobody sees it).
#[cfg(windows)]
const EVENT_TARGET_CLASS: &str = "Tao Thread Event Target";

/// Keep tao's event-loop window through a WM_CLOSE. Tools that close an app by sending WM_CLOSE to
/// each of its visible top-level windows (taskkill, the installer closing a running SpaceKnife:
/// spaceknife.nsh SkPostClose) reach that window too; tao leaves the message to DefWindowProc,
/// which destroys it, and the window's own close, which goes through it, never comes: the app
/// stayed open until the installer ended it after 5 s. Nothing of tao's sends it a WM_CLOSE, so
/// it now ignores one. On the thread that made the window (setup, in the event loop).
#[cfg(windows)]
pub fn keep_event_target() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowExW, GetWindowThreadProcessId, WM_CLOSE,
    };

    unsafe extern "system" fn ignore_close(
        window: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        if msg == WM_CLOSE {
            return 0;
        }
        unsafe { DefSubclassProc(window, msg, wparam, lparam) }
    }

    let class: Vec<u16> = EVENT_TARGET_CLASS.encode_utf16().chain([0]).collect();
    let me = std::process::id();
    let mut at: HWND = std::ptr::null_mut();
    loop {
        // SAFETY: plain Win32 calls on window handles of this process; the subclass procedure
        // only reads its arguments
        at = unsafe { FindWindowExW(std::ptr::null_mut(), at, class.as_ptr(), std::ptr::null()) };
        if at.is_null() {
            break;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(at, &mut pid) };
        if pid == me && unsafe { SetWindowSubclass(at, Some(ignore_close), 1, 0) } == 0 {
            eprintln!("[window] could not keep the event-loop window through WM_CLOSE");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// tao's event-loop window outlives a WM_CLOSE once kept (a window of its class made here as
    /// tao makes it, on this thread); a plain window does not, as DefWindowProc closes it.
    #[cfg(windows)]
    #[test]
    fn the_event_loop_window_outlives_a_wm_close() {
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, IsWindow, RegisterClassW, SendMessageW,
            WM_CLOSE, WNDCLASSW, WS_POPUP,
        };
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (target_class, plain_class) =
            (wide(EVENT_TARGET_CLASS), wide("SpaceKnife test window"));
        // SAFETY: windows of this test's own classes, made, sent to and destroyed on this thread
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let make = |class: &Vec<u16>| {
                let wc = WNDCLASSW {
                    lpfnWndProc: Some(DefWindowProcW),
                    hInstance: instance,
                    lpszClassName: class.as_ptr(),
                    ..std::mem::zeroed()
                };
                RegisterClassW(&wc);
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                )
            };
            let (target, plain) = (make(&target_class), make(&plain_class));
            assert!(!target.is_null() && !plain.is_null());
            keep_event_target();
            SendMessageW(target, WM_CLOSE, 0, 0);
            SendMessageW(plain, WM_CLOSE, 0, 0);
            assert!(
                IsWindow(target) != 0,
                "a WM_CLOSE destroyed the event-loop window"
            );
            assert!(IsWindow(plain) == 0, "a plain window outlived a WM_CLOSE");
            DestroyWindow(target);
        }
    }

    /// The process's explicit AppUserModelID, as the taskbar reads it (None when it has none).
    #[cfg(windows)]
    fn process_app_user_model_id() -> Option<String> {
        use std::ffi::c_void;
        #[link(name = "shell32")]
        unsafe extern "system" {
            fn GetCurrentProcessExplicitAppUserModelID(id: *mut *mut u16) -> i32;
        }
        #[link(name = "ole32")]
        unsafe extern "system" {
            fn CoTaskMemFree(p: *mut c_void);
        }
        let mut p: *mut u16 = std::ptr::null_mut();
        // SAFETY: an out pointer; on success the system allocated a NUL-terminated string
        if unsafe { GetCurrentProcessExplicitAppUserModelID(&mut p) } < 0 || p.is_null() {
            return None;
        }
        // SAFETY: the string is NUL-terminated and stays valid until it is freed below
        let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        let id = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) });
        // SAFETY: allocated with CoTaskMemAlloc by the call above
        unsafe { CoTaskMemFree(p.cast()) };
        Some(id)
    }

    #[cfg(windows)]
    #[test]
    fn the_process_takes_the_shortcuts_app_user_model_id() {
        // what lib.rs run() does before the window: taskbar and Start group the window with the
        // pinned shortcut (both carry this ID), not by the program's path
        set_app_user_model_id();
        assert_eq!(
            process_app_user_model_id().as_deref(),
            Some(APP_USER_MODEL_ID)
        );
    }

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
