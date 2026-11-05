//! 「在终端中打开」: a terminal in a folder, the system's own (was openTerminal in
//! apps/electron/src/main/index.ts). Windows Terminal on Windows, Terminal.app on macOS, the
//! desktop's default terminal (x-terminal-emulator) on Linux.

use std::path::Path;
use std::process::{Command, Stdio};

/// The program and arguments that open a terminal in `dir`, and the folder to start it in.
pub fn terminal_command(platform: &str, dir: &str) -> (String, Vec<String>, Option<String>) {
    match platform {
        // `start` returns at once and leaves no console behind; "" is start's window title
        "windows" => ("cmd.exe".into(), vec!["/c".into(), "start".into(), String::new(), "wt.exe".into(), "-d".into(), dir.into()], None),
        "macos" => ("open".into(), vec!["-a".into(), "Terminal".into(), dir.into()], None),
        _ => ("x-terminal-emulator".into(), vec![], Some(dir.into())),
    }
}

/// Open a terminal in `dir`; it outlives the app. Errors are logged, as the page expects nothing back.
pub fn open_terminal(dir: &str) {
    if !Path::new(dir).is_dir() {
        eprintln!("[shell] no terminal for {dir}: not a folder");
        return;
    }
    let (program, args, cwd) = terminal_command(std::env::consts::OS, dir);
    let mut cmd = Command::new(&program);
    cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if let Err(err) = cmd.spawn() {
        eprintln!("[shell] could not start {program}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_the_systems_terminal() {
        let (p, a, c) = terminal_command("windows", "D:\\x y");
        assert_eq!((p.as_str(), a, c), ("cmd.exe", vec!["/c".to_string(), "start".into(), "".into(), "wt.exe".into(), "-d".into(), "D:\\x y".into()], None));
        let (p, a, _) = terminal_command("macos", "/Users/a");
        assert_eq!((p.as_str(), a), ("open", vec!["-a".to_string(), "Terminal".into(), "/Users/a".into()]));
        let (p, a, c) = terminal_command("linux", "/home/a");
        assert_eq!((p.as_str(), a.len(), c.as_deref()), ("x-terminal-emulator", 0, Some("/home/a")));
    }
}
