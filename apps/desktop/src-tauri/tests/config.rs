//! What the Tauri configuration must keep: the program's name 0.3.4's update looks for, and the
//! page's Content-Security-Policy (the Electron shell's csp.ts sends the same directives).

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
