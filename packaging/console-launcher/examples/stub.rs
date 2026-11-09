//! A stand-in for spaceknife-cli.exe and SpaceKnife.exe in tests/launcher.rs: it says how it was
//! started (its own file name, then its arguments, one per line, argv[0] first; then, when
//! STUB_ENV names a variable, `env NAME=value`) on stdout and, when STUB_RECORD names a file, in
//! that file too; it ends with 7, after STUB_SLEEP_MS milliseconds when that is set.

fn main() {
    let me = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let mut lines = vec![format!("exe={me}")];
    lines.extend(std::env::args());
    if let Ok(name) = std::env::var("STUB_ENV") {
        let value = std::env::var(&name).unwrap_or_else(|_| "(unset)".into());
        lines.push(format!("env {name}={value}"));
    }
    if let Some(ms) = std::env::var("STUB_SLEEP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let text = lines.join("\n");
    if let Some(file) = std::env::var_os("STUB_RECORD") {
        let _ = std::fs::write(file, &text);
    }
    println!("{text}");
    std::process::exit(7);
}
