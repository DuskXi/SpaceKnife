//! What the Tauri configuration must keep: the page's Content-Security-Policy (the Electron shell's
//! csp.ts sends the same directives).

use serde_json::Value;

fn config(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
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
