//! What the user is told when SpaceKnife's window can't open (startup_check.rs): what is missing,
//! where Microsoft offers it, that the data folder is untouched, that replacing the files by hand
//! won't help until the runtime is there, and that headless mode works meanwhile. In the system's
//! UI language, Chinese or English (no setting is read: the page's language is not known yet).
//!
//! On Windows a task dialog with a button to Microsoft's download page (comctl32 6, which the
//! program's manifest asks for; looked up when it is shown, and a message box where it is missing),
//! after the same words on standard error and in the log. Elsewhere standard error and the log
//! only: no GUI library more. [`message`] picks the words; the tests read it.

use std::io::Write as _;

use crate::startup_check::{Problem, min_webview2};
use crate::text::{Lang, fill};

/// Microsoft's download page for the WebView2 Runtime.
pub const WEBVIEW2_PAGE: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// Who meets the problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// This program was started and can't open its window.
    Start,
    /// The swap helper refused to put this version in place (`--startup-problem`): the copy in
    /// use keeps running.
    Update,
}

/// The button to Microsoft's download page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub label: String,
    /// under the label: which site, and what to take there
    pub note: String,
    /// the message box's question, where there is no task dialog
    pub ask: String,
    pub url: &'static str,
}

/// The words of the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    /// the main instruction: what is wrong, in one line
    pub heading: String,
    /// paragraphs, a blank line apart
    pub body: String,
    /// for a missing, old or broken WebView2 Runtime
    pub download: Option<Download>,
    /// what was found, behind 「详细信息」 / "See details"
    pub details: String,
    /// the program can't start (an error) or an update was not installed (a warning)
    pub error: bool,
}

/// The dialog's words for `problem` met in `context`, in `lang`, on Windows (`windows`) or
/// elsewhere; `version` is this program's.
pub fn message(
    problem: &Problem,
    context: Context,
    lang: Lang,
    windows: bool,
    version: &str,
) -> Message {
    let zh = lang == Lang::ZhCn;
    let t = |zh_text: &str, en_text: &str, fields: &[(&str, &str)]| {
        fill(if zh { zh_text } else { en_text }, fields)
    };
    let app = match context {
        Context::Start => "SpaceKnife".to_owned(),
        Context::Update => format!("SpaceKnife {version}"),
    };
    let min = min_webview2();
    // a runtime to install: missing, too old or (on Windows) broken
    let runtime = match problem {
        Problem::Missing(_) | Problem::TooOld(_) => true,
        Problem::WindowFailed { .. } => windows,
        Problem::Unknown => false,
    };
    // the fix is WebView2 from Microsoft's page
    let webview2 = runtime && windows;

    let heading = match problem {
        Problem::Missing(_) if windows => t(
            "{app} 需要 Microsoft Edge WebView2 Runtime，这台电脑上没有",
            "{app} needs the Microsoft Edge WebView2 Runtime, which this computer doesn't have",
            &[("app", &app)],
        ),
        Problem::Missing(_) => t(
            "{app} 找不到系统的网页视图组件",
            "{app} can't find the system's web view",
            &[("app", &app)],
        ),
        Problem::TooOld(_) => t(
            "{app} 需要更新版本的 Microsoft Edge WebView2 Runtime",
            "{app} needs a newer Microsoft Edge WebView2 Runtime",
            &[("app", &app)],
        ),
        Problem::WindowFailed { .. } => t(
            "{app} 打不开窗口",
            "{app} couldn't open its window",
            &[("app", &app)],
        ),
        Problem::Unknown => t(
            "{app} 无法在这台电脑上启动",
            "{app} can't start on this computer",
            &[("app", &app)],
        ),
    };

    let mut body = Vec::new();
    // what is wrong
    body.push(match problem {
        Problem::Missing(_) if windows => t(
            "SpaceKnife 的界面由 WebView2 显示。它是微软免费提供的 Windows 组件，Windows 11 \
             自带；有些 Windows 10 上没有装，或者被卸载了。",
            "SpaceKnife shows its interface with WebView2, a free Windows component from \
             Microsoft. Windows 11 comes with it; some Windows 10 computers don't have it, or it \
             was removed.",
            &[],
        ),
        Problem::Missing(_) => t(
            "SpaceKnife 的界面由系统的网页视图显示，Linux 上是 WebKitGTK 4.1\
             （libwebkit2gtk-4.1），请用发行版的软件包管理器安装。",
            "SpaceKnife shows its interface with the system's web view. On Linux that is \
             WebKitGTK 4.1 (libwebkit2gtk-4.1), which your distribution's package manager \
             installs.",
            &[],
        ),
        Problem::TooOld(found) => t(
            "SpaceKnife 需要 WebView2 {min} 或更新的版本，这台电脑上的是 {found}。",
            "SpaceKnife needs WebView2 {min} or later; this computer has {found}.",
            &[("min", &min), ("found", found)],
        ),
        Problem::WindowFailed { webview, .. } if windows => {
            let found = webview.as_deref().map(|v| format!(" {v}")).unwrap_or_default();
            t(
                "这台电脑装有 WebView2{found}，但用它建不起窗口，它可能已经损坏。从微软重新安装\
                 一次通常就能解决。",
                "This computer has WebView2{found}, but the window couldn't be made with it; it \
                 may be damaged. Installing it again from Microsoft usually fixes this.",
                &[("found", &found)],
            )
        }
        Problem::WindowFailed { .. } => t(
            "系统的网页视图建不起窗口。",
            "The system's web view couldn't make the window.",
            &[],
        ),
        Problem::Unknown => match context {
            Context::Update => t(
                "这个版本在这台电脑上启动不了，原因写在 SpaceKnife 的 updates 文件夹里的更新日志中。",
                "This version doesn't start on this computer; the update log in SpaceKnife's \
                 “updates” folder says why.",
                &[],
            ),
            Context::Start => t("原因写在 SpaceKnife 的日志里。", "SpaceKnife's log says why.", &[]),
        },
    });
    // what to do
    if webview2 {
        body.push(match context {
            Context::Start => t(
                "请用下面的按钮从微软官网下载并安装 WebView2 Runtime（免费），然后重新启动 SpaceKnife。",
                "Install the WebView2 Runtime from Microsoft's website with the button below \
                 (it's free), then start SpaceKnife again.",
                &[],
            ),
            Context::Update => t(
                "请用下面的按钮从微软官网下载并安装 WebView2 Runtime（免费），然后再更新一次。",
                "Install the WebView2 Runtime from Microsoft's website with the button below \
                 (it's free), then update again.",
                &[],
            ),
        });
    }
    // nothing lost
    body.push(match context {
        Context::Start => t(
            "你的数据文件夹（快照和设置）没有任何改动。",
            "Your data folder, with its snapshots and settings, is untouched.",
            &[],
        ),
        Context::Update => t(
            "更新没有安装：现在用的 SpaceKnife 照常可用，它的数据文件夹也没有任何改动。",
            "The update was not installed: the SpaceKnife you have keeps working, and its data \
             folder is untouched.",
            &[],
        ),
    });
    // by hand
    let wanted = if windows {
        "WebView2 Runtime"
    } else {
        "WebKitGTK 4.1"
    };
    if runtime {
        body.push(t(
            "在装好 {wanted} 之前，手动替换 SpaceKnife 的文件（比如把下载的新版本解压覆盖过去）\
             也没有用。",
            "Until the {wanted} is installed, replacing SpaceKnife's files by hand (extracting \
             the download over them, say) won't help.",
            &[("wanted", wanted)],
        ));
    } else if context == Context::Update {
        body.push(t(
            "手动替换 SpaceKnife 的文件（比如把下载的新版本解压覆盖过去）也没有用：新版本同样启动\
             不了。",
            "Replacing SpaceKnife's files by hand (extracting the download over them, say) won't \
             help: the new version won't start either.",
            &[],
        ));
    }
    // meanwhile
    if runtime {
        body.push(if windows {
            t(
                "在此之前，也可以不开窗口使用 SpaceKnife：在 SpaceKnife 所在的文件夹里打开终端，运行 \
                 SpaceKnife --headless，再用浏览器打开它显示的地址。",
                "Meanwhile SpaceKnife works without its window: open a terminal in SpaceKnife's \
                 folder, run SpaceKnife --headless, and open the address it prints in a browser.",
                &[],
            )
        } else {
            t(
                "在此之前，也可以不开窗口使用 SpaceKnife：在 SpaceKnife 所在的文件夹里打开终端，运行 \
                 ./spaceknife-cli headless，再用浏览器打开它显示的地址。",
                "Meanwhile SpaceKnife works without its window: open a terminal in SpaceKnife's \
                 folder, run ./spaceknife-cli headless, and open the address it prints in a \
                 browser.",
                &[],
            )
        });
    }

    let download = webview2.then(|| Download {
        label: t(
            "下载 WebView2 Runtime",
            "Download the WebView2 Runtime",
            &[],
        ),
        note: t(
            "免费，在微软官网（developer.microsoft.com）上选 “Evergreen Bootstrapper”",
            "Free, on Microsoft's website (developer.microsoft.com): choose “Evergreen \
             Bootstrapper”",
            &[],
        ),
        ask: t(
            "现在打开微软的下载页面吗？",
            "Open Microsoft's download page now?",
            &[],
        ),
        url: WEBVIEW2_PAGE,
    });

    let mut details = vec![format!("SpaceKnife {version}")];
    match problem {
        Problem::Missing(detail) => details.push(t(
            "网页视图：{detail}",
            "Web view: {detail}",
            &[("detail", detail)],
        )),
        Problem::TooOld(found) => details.push(t(
            "WebView2 Runtime {found}，需要 {min} 或更新",
            "WebView2 Runtime {found}; needs {min} or later",
            &[("found", found), ("min", &min)],
        )),
        Problem::WindowFailed { webview, error } => {
            if let Some(found) = webview {
                details.push(t(
                    "网页视图 {found}",
                    "Web view {found}",
                    &[("found", found)],
                ));
            }
            details.push(error.clone());
        }
        Problem::Unknown => details.push(t(
            "现在的启动检查没有发现问题。",
            "The startup check finds nothing wrong now.",
            &[],
        )),
    }

    Message {
        title: "SpaceKnife".to_owned(),
        heading,
        body: body.join("\n\n"),
        download,
        details: details.join("\n"),
        error: context == Context::Start,
    }
}

/// The message as text: standard error, the message box.
pub fn plain(m: &Message) -> String {
    let mut out = format!("{}\n\n{}", m.heading, m.body);
    if let Some(d) = &m.download {
        out.push_str(&format!("\n\n{}: {}", d.label, d.url));
    }
    out.push_str("\n\n");
    out.push_str(&m.details);
    out
}

/// Text for a task dialog that shows links: none but its own (a loader's error could hold one).
pub fn no_links(text: &str) -> String {
    text.replace("<a", "‹a").replace("<A", "‹A")
}

/// Tell the user what keeps the window from opening: the log (once it runs), standard error (a
/// terminal, when there is one) and on Windows the dialog; back when it is closed.
pub fn tell(problem: &Problem, context: Context) {
    let windows = cfg!(windows);
    tracing::error!("the window can't open: {}", problem.error(windows));
    let m = message(
        problem,
        context,
        crate::text::system_lang(),
        windows,
        env!("CARGO_PKG_VERSION"),
    );
    // a closed standard error is no reason to fail
    let _ = writeln!(std::io::stderr(), "{}", plain(&m));
    #[cfg(windows)]
    win::show(&m);
}

#[cfg(windows)]
mod win {
    use std::ffi::CStr;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, S_FALSE, S_OK, WPARAM};
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows_sys::Win32::UI::Controls::{
        TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_ERROR_ICON, TD_WARNING_ICON,
        TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_ENABLE_HYPERLINKS,
        TDF_USE_COMMAND_LINKS, TDN_BUTTON_CLICKED, TDN_CREATED, TDN_HYPERLINK_CLICKED,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDCLOSE, IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_YESNO,
        MessageBoxW, SW_SHOWNORMAL, SetForegroundWindow,
    };
    use windows_sys::core::{BOOL, HRESULT};

    use super::{Message, WEBVIEW2_PAGE, no_links, plain};

    /// The download button's ID (the common buttons' are below 10).
    const DOWNLOAD: i32 = 1000;

    type Proc = unsafe extern "system" fn() -> isize;
    type TaskDialogIndirect = unsafe extern "system" fn(
        *const TASKDIALOGCONFIG,
        *mut i32,
        *mut i32,
        *mut BOOL,
    ) -> HRESULT;
    type SetProcessDpiAwarenessContext = unsafe extern "system" fn(isize) -> BOOL;

    /// DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
    const PER_MONITOR_AWARE_V2: isize = -4;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    /// A function of a system library, looked up: one this Windows (or this process's activation
    /// context, for comctl32 6) may not have is no reason for the program not to load.
    fn proc_address(library: &str, name: &CStr) -> Option<Proc> {
        let library = wide(library);
        // SAFETY: NUL-terminated names that outlive the calls; a loaded system library stays
        // loaded (the process ends soon after)
        unsafe {
            let module = LoadLibraryW(library.as_ptr());
            if module.is_null() {
                return None;
            }
            GetProcAddress(module, name.as_ptr().cast())
        }
    }

    /// The dialog, back when it is closed: a task dialog, else a message box.
    pub fn show(m: &Message) {
        // SAFETY: plain calls without pointers of ours. COM for ShellExecute (Microsoft asks for
        // it before; a thread that has it already keeps it), sharp text on a scaled screen (a
        // process that set its DPI awareness keeps it: tao's, when the window failed), and the
        // taskbar groups the dialog with SpaceKnife's shortcuts and window.
        unsafe {
            CoInitializeEx(
                null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            );
            if let Some(f) = proc_address("user32.dll", c"SetProcessDpiAwarenessContext") {
                let set = std::mem::transmute::<Proc, SetProcessDpiAwarenessContext>(f);
                set(PER_MONITOR_AWARE_V2);
            }
        }
        crate::window::set_app_user_model_id();
        if !task_dialog(m) {
            message_box(m);
        }
    }

    /// Microsoft's download page in the browser.
    fn open_page() {
        let verb = wide("open");
        let url = wide(WEBVIEW2_PAGE);
        // SAFETY: NUL-terminated strings that outlive the call
        unsafe {
            ShellExecuteW(
                null_mut(),
                verb.as_ptr(),
                url.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            );
        }
    }

    /// The task dialog's notifications: to the front once it is there; the button and the link
    /// open Microsoft's page, and the dialog stays for the rest of what it says.
    unsafe extern "system" fn on_notice(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        _lparam: LPARAM,
        _data: isize,
    ) -> HRESULT {
        match msg as i32 {
            TDN_CREATED => {
                // SAFETY: the dialog's own window; refused when another program has the focus
                // (the taskbar button flashes then)
                unsafe { SetForegroundWindow(hwnd) };
                S_OK
            }
            TDN_HYPERLINK_CLICKED => {
                open_page();
                S_OK
            }
            TDN_BUTTON_CLICKED if wparam as i32 == DOWNLOAD => {
                open_page();
                S_FALSE
            }
            _ => S_OK,
        }
    }

    /// Whether the task dialog was shown (comctl32 6 is there and took it).
    fn task_dialog(m: &Message) -> bool {
        let Some(f) = proc_address("comctl32.dll", c"TaskDialogIndirect") else {
            return false;
        };
        // SAFETY: TaskDialogIndirect has this signature (CommCtrl.h)
        let task_dialog_indirect = unsafe { std::mem::transmute::<Proc, TaskDialogIndirect>(f) };
        let title = wide(&m.title);
        let heading = wide(&no_links(&m.heading));
        let body = wide(&no_links(&m.body));
        let details = wide(&no_links(&m.details));
        // a command link: the label, then the note under it
        let button = m
            .download
            .as_ref()
            .map(|d| wide(&no_links(&format!("{}\n{}", d.label, d.note))));
        let footer = m
            .download
            .as_ref()
            .map(|d| wide(&format!("<a href=\"{0}\">{0}</a>", d.url)));
        let buttons: Vec<TASKDIALOG_BUTTON> = button
            .iter()
            .map(|b| TASKDIALOG_BUTTON {
                nButtonID: DOWNLOAD,
                pszButtonText: b.as_ptr(),
            })
            .collect();
        let config = TASKDIALOGCONFIG {
            cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
            dwFlags: TDF_ENABLE_HYPERLINKS | TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION,
            dwCommonButtons: TDCBF_CLOSE_BUTTON,
            pszWindowTitle: title.as_ptr(),
            Anonymous1: TASKDIALOGCONFIG_0 {
                pszMainIcon: if m.error {
                    TD_ERROR_ICON
                } else {
                    TD_WARNING_ICON
                },
            },
            pszMainInstruction: heading.as_ptr(),
            pszContent: body.as_ptr(),
            cButtons: buttons.len() as u32,
            pButtons: if buttons.is_empty() {
                null()
            } else {
                buttons.as_ptr()
            },
            nDefaultButton: if buttons.is_empty() {
                IDCLOSE
            } else {
                DOWNLOAD
            },
            pszExpandedInformation: details.as_ptr(),
            pszFooter: footer.as_ref().map_or(null(), |f| f.as_ptr()),
            pfCallback: Some(on_notice),
            ..Default::default()
        };
        // SAFETY: every string and the buttons outlive the call, which returns once the dialog
        // is closed
        let hr = unsafe { task_dialog_indirect(&config, null_mut(), null_mut(), null_mut()) };
        hr >= 0
    }

    /// Where there is no task dialog: the same words; Yes opens Microsoft's page.
    fn message_box(m: &Message) {
        let mut text = plain(m);
        let icon = if m.error {
            MB_ICONERROR
        } else {
            MB_ICONWARNING
        };
        let buttons = match &m.download {
            Some(d) => {
                text.push_str("\n\n");
                text.push_str(&d.ask);
                MB_YESNO
            }
            None => MB_OK,
        };
        let text = wide(&text);
        let title = wide(&m.title);
        // SAFETY: NUL-terminated strings that outlive the call
        let answer = unsafe {
            MessageBoxW(
                null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                icon | buttons | MB_SETFOREGROUND,
            )
        };
        if answer == IDYES && m.download.is_some() {
            open_page();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_task_dialogs_settings_are_packed_as_commctrl_h_has_them() {
            // #include <pshpack1.h>: 160 bytes with 64-bit pointers, 96 with 32-bit ones
            let expected = if cfg!(target_pointer_width = "64") {
                160
            } else {
                96
            };
            assert_eq!(size_of::<TASKDIALOGCONFIG>(), expected);
        }

        #[test]
        fn looks_up_what_a_system_library_has_and_not_what_it_lacks() {
            assert!(proc_address("user32.dll", c"MessageBoxW").is_some());
            assert!(proc_address("user32.dll", c"NoSuchFunctionInUser32").is_none());
            assert!(proc_address("no-such-library-of-spaceknife.dll", c"F").is_none());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "0.3.5";

    fn missing() -> Problem {
        Problem::Missing("WebView2 error: HRESULT(0x80070002)".into())
    }

    /// Every problem, every context, both languages, on Windows and elsewhere.
    fn every_message() -> Vec<(Problem, Context, Lang, bool, Message)> {
        let problems = [
            missing(),
            Problem::TooOld("88.0.705.81".into()),
            Problem::WindowFailed {
                webview: Some("120.0.2210.91".into()),
                error: "WebView2 error: E_NOINTERFACE".into(),
            },
            Problem::WindowFailed {
                webview: None,
                error: "no display".into(),
            },
            Problem::Unknown,
        ];
        let mut all = Vec::new();
        for p in problems {
            for c in [Context::Start, Context::Update] {
                for l in [Lang::ZhCn, Lang::En] {
                    for w in [true, false] {
                        let m = message(&p, c, l, w, V);
                        all.push((p.clone(), c, l, w, m));
                    }
                }
            }
        }
        all
    }

    #[test]
    fn a_missing_webview2_says_what_where_and_that_nothing_is_lost() {
        let m = message(&missing(), Context::Start, Lang::En, true, V);
        assert_eq!(m.title, "SpaceKnife");
        assert_eq!(
            m.heading,
            "SpaceKnife needs the Microsoft Edge WebView2 Runtime, which this computer doesn't have"
        );
        assert!(m.error);
        for said in [
            "WebView2, a free Windows component from Microsoft",
            "with the button below (it's free), then start SpaceKnife again",
            "Your data folder, with its snapshots and settings, is untouched.",
            "Until the WebView2 Runtime is installed, replacing SpaceKnife's files by hand",
            "run SpaceKnife --headless",
        ] {
            assert!(m.body.contains(said), "{said:?} in {}", m.body);
        }
        let d = m.download.as_ref().unwrap();
        assert_eq!(d.url, WEBVIEW2_PAGE);
        assert_eq!(d.label, "Download the WebView2 Runtime");
        assert!(d.note.contains("Evergreen Bootstrapper"));
        assert_eq!(
            m.details,
            "SpaceKnife 0.3.5\nWeb view: WebView2 error: HRESULT(0x80070002)"
        );
    }

    #[test]
    fn says_it_in_chinese_on_a_chinese_system() {
        let m = message(&missing(), Context::Start, Lang::ZhCn, true, V);
        assert_eq!(
            m.heading,
            "SpaceKnife 需要 Microsoft Edge WebView2 Runtime，这台电脑上没有"
        );
        for said in [
            "微软免费提供的 Windows 组件",
            "然后重新启动 SpaceKnife",
            "你的数据文件夹（快照和设置）没有任何改动。",
            "在装好 WebView2 Runtime 之前，手动替换 SpaceKnife 的文件",
            "运行 SpaceKnife --headless",
        ] {
            assert!(m.body.contains(said), "{said:?} in {}", m.body);
        }
        let d = m.download.unwrap();
        assert_eq!(d.label, "下载 WebView2 Runtime");
        assert_eq!(d.ask, "现在打开微软的下载页面吗？");
        assert_eq!(
            m.details,
            "SpaceKnife 0.3.5\n网页视图：WebView2 error: HRESULT(0x80070002)"
        );
        // the language is the system's UI language's: Chinese ones Chinese, the rest English
        assert_eq!(Lang::from_locale("zh-Hans-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_locale("de-DE"), Lang::En);
    }

    #[test]
    fn an_old_runtime_says_which_and_which_it_needs() {
        let m = message(
            &Problem::TooOld("88.0.705.81".into()),
            Context::Start,
            Lang::En,
            true,
            V,
        );
        assert_eq!(
            m.heading,
            "SpaceKnife needs a newer Microsoft Edge WebView2 Runtime"
        );
        assert!(m.body.starts_with(
            "SpaceKnife needs WebView2 89.0.774.44 or later; this computer has 88.0.705.81."
        ));
        assert!(m.download.is_some());
        assert!(
            m.details
                .ends_with("WebView2 Runtime 88.0.705.81; needs 89.0.774.44 or later")
        );
        let zh = message(
            &Problem::TooOld("88.0.705.81".into()),
            Context::Start,
            Lang::ZhCn,
            true,
            V,
        );
        assert!(zh.body.starts_with(
            "SpaceKnife 需要 WebView2 89.0.774.44 或更新的版本，这台电脑上的是 88.0.705.81。"
        ));
    }

    #[test]
    fn a_window_that_failed_points_at_a_damaged_runtime() {
        let p = Problem::WindowFailed {
            webview: Some("120.0.2210.91".into()),
            error: "WebView2 error: E_NOINTERFACE".into(),
        };
        let m = message(&p, Context::Start, Lang::En, true, V);
        assert_eq!(m.heading, "SpaceKnife couldn't open its window");
        assert!(m.body.starts_with(
            "This computer has WebView2 120.0.2210.91, but the window couldn't be made with it; \
             it may be damaged."
        ));
        assert!(m.download.is_some());
        assert_eq!(
            m.details,
            "SpaceKnife 0.3.5\nWeb view 120.0.2210.91\nWebView2 error: E_NOINTERFACE"
        );
        // elsewhere there is nothing of Microsoft's to install
        let m = message(&p, Context::Start, Lang::En, false, V);
        assert!(m.download.is_none());
        assert!(!m.body.contains("WebView2") && !m.body.contains("--headless"));
    }

    #[test]
    fn a_refused_update_says_the_copy_in_use_keeps_working() {
        let m = message(&missing(), Context::Update, Lang::En, true, V);
        assert_eq!(
            m.heading,
            "SpaceKnife 0.3.5 needs the Microsoft Edge WebView2 Runtime, which this computer \
             doesn't have"
        );
        assert!(!m.error, "a warning: nothing broke");
        assert!(m.body.contains("then update again"));
        assert!(m.body.contains(
            "The update was not installed: the SpaceKnife you have keeps working, and its data \
             folder is untouched."
        ));
        assert!(m.body.contains("replacing SpaceKnife's files by hand"));
        assert!(m.body.contains("SpaceKnife --headless"));
        let zh = message(&missing(), Context::Update, Lang::ZhCn, true, V);
        assert!(zh.heading.starts_with("SpaceKnife 0.3.5 需要"));
        assert!(
            zh.body
                .contains("更新没有安装：现在用的 SpaceKnife 照常可用")
        );
    }

    #[test]
    fn nothing_found_is_said_in_general_words() {
        let m = message(&Problem::Unknown, Context::Update, Lang::En, true, V);
        assert_eq!(m.heading, "SpaceKnife 0.3.5 can't start on this computer");
        assert!(m.download.is_none(), "WebView2 is not what is wrong");
        assert!(
            m.body
                .contains("the update log in SpaceKnife's “updates” folder says why")
        );
        assert!(m.body.contains("The update was not installed"));
        assert!(
            m.body
                .contains("won't help: the new version won't start either")
        );
        assert!(!m.body.contains("WebView2") && !m.body.contains("--headless"));
        let zh = message(&Problem::Unknown, Context::Update, Lang::ZhCn, true, V);
        assert_eq!(zh.heading, "SpaceKnife 0.3.5 无法在这台电脑上启动");
    }

    #[test]
    fn elsewhere_there_is_no_webview2_to_download() {
        let m = message(&missing(), Context::Update, Lang::En, false, V);
        assert_eq!(
            m.heading,
            "SpaceKnife 0.3.5 can't find the system's web view"
        );
        assert!(m.body.contains("WebKitGTK 4.1 (libwebkit2gtk-4.1)"));
        assert!(m.body.contains("Until the WebKitGTK 4.1 is installed"));
        assert!(m.body.contains("./spaceknife-cli headless"));
        assert!(m.download.is_none());
        assert!(!m.body.contains("WebView2"));
    }

    #[test]
    fn every_message_is_whole_in_its_language() {
        for (p, c, l, w, m) in every_message() {
            let what = format!("{p:?} {c:?} {l:?} windows={w}");
            assert!(!m.heading.is_empty() && !m.body.is_empty(), "{what}");
            assert!(m.details.starts_with("SpaceKnife 0.3.5"), "{what}");
            // filled in, one blank line between paragraphs
            for text in [&m.heading, &m.body, &m.details] {
                assert!(!text.contains('{') && !text.contains('}'), "{what}: {text}");
            }
            assert!(
                !m.body.contains("\n\n\n") && !m.body.starts_with('\n'),
                "{what}"
            );
            // the data folder is always said to be untouched
            assert!(
                m.body.contains("untouched") || m.body.contains("没有任何改动"),
                "{what}"
            );
            // Chinese text in Chinese, English text in English
            let has_han = |s: &str| s.chars().any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch));
            assert_eq!(has_han(&m.heading), l == Lang::ZhCn, "{what}");
            assert_eq!(has_han(&m.body), l == Lang::ZhCn, "{what}");
            // Microsoft's page only on Windows, and only for its runtime
            assert_eq!(
                m.download.is_some(),
                w && !matches!(p, Problem::Unknown),
                "{what}"
            );
            if m.download.is_some() {
                assert!(m.body.contains("--headless"), "{what}");
            }
            // the update's words only for an update
            assert_eq!(m.heading.contains(V), c == Context::Update, "{what}");
            assert_eq!(m.error, c == Context::Start, "{what}");
            // a task dialog shows links: the words hold none
            assert_eq!(no_links(&m.body), m.body, "{what}");
            assert_eq!(no_links(&m.heading), m.heading, "{what}");
        }
    }

    #[test]
    fn the_plain_text_has_it_all() {
        let m = message(&missing(), Context::Start, Lang::En, true, V);
        let text = plain(&m);
        assert!(text.starts_with(&format!("{}\n\n{}", m.heading, m.body)));
        assert!(text.contains(&format!(
            "\n\nDownload the WebView2 Runtime: {WEBVIEW2_PAGE}\n\n"
        )));
        assert!(text.ends_with(&m.details));
        let m = message(&Problem::Unknown, Context::Update, Lang::En, false, V);
        assert!(!plain(&m).contains("http"));
    }

    #[test]
    fn a_loaders_words_carry_no_link_into_the_dialog() {
        assert_eq!(
            no_links(r#"bad <a href="x">y</a> <A HREF="z">"#),
            r#"bad ‹a href="x">y</a> ‹A HREF="z">"#
        );
        assert_eq!(
            no_links("HRESULT(0x80070002) < 3"),
            "HRESULT(0x80070002) < 3"
        );
    }
}
