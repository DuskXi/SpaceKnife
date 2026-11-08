//! What the Tauri configuration must keep: the program's name 0.3.4's update looks for, the macOS
//! folder prompts' reasons and floor 0.3.4 had, and the page's Content-Security-Policy (the
//! Electron shell's csp.ts sends the same directives).

use serde_json::Value;

fn config(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

/// The main program's file name on a platform: the platform's file over the base one.
fn main_binary(platform_file: &str) -> String {
    let platform = config(platform_file);
    let base = config("tauri.conf.json");
    platform
        .get("mainBinaryName")
        .or_else(|| base.get("mainBinaryName"))
        .and_then(Value::as_str)
        .unwrap()
        .to_owned()
}

#[test]
fn the_program_keeps_the_name_0_3_4_updates_look_for() {
    // 0.3.4's 「无缝更新」 of a portable copy checks for SpaceKnife.exe in the new version and starts
    // …/SpaceKnife.app/Contents/MacOS/SpaceKnife (plan §5.2–5.3); Linux keeps `spaceknife`
    assert_eq!(main_binary("tauri.windows.conf.json"), "SpaceKnife");
    assert_eq!(main_binary("tauri.macos.conf.json"), "SpaceKnife");
    assert_eq!(main_binary("tauri.linux.conf.json"), "spaceknife");
}

/// The `<key>…</key>` / `<string>…</string>` pairs of a flat plist dictionary.
fn plist_strings(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("<key>") {
        let after = &rest[at + 5..];
        let Some(end) = after.find("</key>") else {
            break;
        };
        let key = after[..end].to_owned();
        let tail = after[end + 6..].trim_start();
        if let Some(value) = tail.strip_prefix("<string>")
            && let Some(close) = value.find("</string>")
        {
            out.push((key, value[..close].to_owned()));
        }
        rest = &after[end..];
    }
    out
}

#[test]
fn macos_asks_for_folders_with_the_reasons_0_3_4_gave() {
    // macOS shows these in its Files and Folders prompts; 0.3.4 had them in electron-builder.yml
    // mac.extendInfo, word for word
    let want = [
        (
            "NSDesktopFolderUsageDescription",
            "SpaceKnife measures the files on your Desktop to show what takes up space.",
        ),
        (
            "NSDocumentsFolderUsageDescription",
            "SpaceKnife measures the files in Documents to show what takes up space.",
        ),
        (
            "NSDownloadsFolderUsageDescription",
            "SpaceKnife measures the files in Downloads to show what takes up space.",
        ),
        (
            "NSRemovableVolumesUsageDescription",
            "SpaceKnife measures the files on removable drives to show what takes up space.",
        ),
        (
            "NSNetworkVolumesUsageDescription",
            "SpaceKnife measures the files on network drives to show what takes up space.",
        ),
    ];
    let mac = config("tauri.macos.conf.json");
    let file = mac["bundle"]["macOS"]["infoPlist"].as_str().unwrap();
    assert_eq!(file, "Info.plist");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let got = plist_strings(&std::fs::read_to_string(path).unwrap());
    let want: Vec<(String, String)> = want
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    assert_eq!(got, want);
    // the floor stays macOS 14, whatever runners CI has
    assert_eq!(
        config("tauri.conf.json")["bundle"]["macOS"]["minimumSystemVersion"],
        "14.0"
    );
    assert!(mac["bundle"]["macOS"].get("minimumSystemVersion").is_none());
}

#[test]
fn the_page_may_load_only_its_own_files_and_reach_only_its_backend() {
    let csp = config("tauri.conf.json")["app"]["security"]["csp"].clone();
    let get = |k: &str| csp[k].as_str().unwrap_or_default().to_owned();
    assert_eq!(get("default-src"), "'self'");
    // Tauri adds the hashes of the page's inline scripts; nothing else inline, no eval
    assert_eq!(get("script-src"), "'self'");
    assert!(get("connect-src").contains("ws://127.0.0.1:*"));
    for k in ["object-src", "base-uri", "form-action", "frame-ancestors"] {
        assert_eq!(get(k), "'none'", "{k}");
    }
    // Tauri's own IPC on Windows is the one web address
    let all = csp.to_string().replace("http://ipc.localhost", "");
    assert!(!all.contains("unsafe-eval"));
    assert!(!all.contains("http:") && !all.contains("https:"), "{all}");
}

#[test]
fn the_window_groups_with_the_installers_shortcuts() {
    // The installer's shortcuts (0.3.4's electron-builder ones and the forked Tauri template's,
    // utils.nsh SetLnkAppUserModelId: ${BUNDLEID}) carry the identifier as their AppUserModelID;
    // the running window must say the same, or Windows shows it apart from the pinned icon
    let id = config("tauri.conf.json")["identifier"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(id, "io.github.duskxi.spaceknife");
    assert_eq!(spaceknife_desktop::window::APP_USER_MODEL_ID, id);
}

#[test]
fn the_installer_brings_webview2_up_to_what_the_window_needs() {
    // The window needs WebView2 89.0.774.44 or later (startup_check::MIN_WEBVIEW2: wry sets its
    // background colour through ICoreWebView2Controller2). The swap helper's --startup-check keeps
    // a portable copy from moving to it on an older runtime; the installer, which takes over 0.3.4
    // (Electron, no WebView2 needed) in place, asks WebView2's updater for at least that version
    // (the forked template's MINIMUMWEBVIEW2VERSION)
    let windows = config("tauri.bundle.windows.json");
    let want = spaceknife_desktop::startup_check::MIN_WEBVIEW2
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    assert_eq!(
        windows["bundle"]["windows"]["minimumWebview2Version"].as_str(),
        Some(want.as_str())
    );
}
