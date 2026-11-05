//! The built interface's files (.output/public): their types, and a URL path resolved inside the
//! folder. Tauri serves the window's files itself; this is for serving them over HTTP (headless
//! mode, once the Rust server takes it over). (Was apps/electron/src/main/files.ts.)

use std::path::{Path, PathBuf};

/// The content type of a file, by its extension.
pub fn mime_of(file: &Path) -> &'static str {
    let ext = file.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("json") => "application/json; charset=utf-8",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// decodeURIComponent: every % starts two hex digits, and the bytes are UTF-8.
fn decode_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let ok = bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit) && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit);
            if !ok {
                return None;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    percent_encoding::percent_decode_str(s).decode_utf8().ok().map(|c| c.into_owned())
}

/// The file a URL path names inside `dir` ("/" is index.html), or None when it can't be decoded.
/// ".." stops at the folder, as it does at a site's root; a backslash separates on Windows only.
pub fn file_in(dir: &Path, pathname: &str) -> Option<PathBuf> {
    let decoded = decode_component(pathname)?;
    if decoded.contains('\0') {
        return None;
    }
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let mut parts: Vec<&str> = Vec::new();
    for part in decoded.split(is_sep) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            // a drive or stream name would leave the folder (C:, file:stream)
            p if cfg!(windows) && p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Some(dir.join("index.html"));
    }
    let mut file = dir.to_path_buf();
    for p in parts {
        file.push(p);
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_types() {
        assert_eq!(mime_of(Path::new("a/index.HTML")), "text/html; charset=utf-8");
        assert_eq!(mime_of(Path::new("x.woff2")), "font/woff2");
        assert_eq!(mime_of(Path::new("x")), "application/octet-stream");
    }

    #[test]
    fn keeps_paths_inside_the_folder() {
        let dir = std::env::temp_dir().join("r");
        assert_eq!(file_in(&dir, "/"), Some(dir.join("index.html")));
        assert_eq!(file_in(&dir, "/assets/a.js"), Some(dir.join("assets").join("a.js")));
        assert_eq!(file_in(&dir, "/../x"), Some(dir.join("x")));
        assert_eq!(file_in(&dir, "/%2e%2e/%2e%2e/x"), Some(dir.join("x")));
        assert!(file_in(&dir, "/a/..%5c..%5cx").unwrap().starts_with(&dir));
        assert_eq!(file_in(&dir, "/%E0%A4%A"), None);
        assert_eq!(file_in(&dir, "/%FF"), None);
        assert_eq!(file_in(&dir, "/a%00b"), None);
    }
}
