//! 「在终端中打开」: a terminal in a folder, the system's own (the Electron shell's
//! apps/electron-shell/src/main/terminal.ts is the same list). The folder never goes through a
//! shell: 0.3.4 and the first Tauri version ran `cmd.exe /c start "" wt.exe -d <folder>`, and cmd ran
//! whatever followed a `&` in a folder's name (review issue 1). Now each terminal is started
//! directly with an argument list, and the folder is also its working directory.
//!
//! - Windows: Windows Terminal (`wt.exe -d <folder>`). Windows Terminal splits its own command line
//!   at a `;` and keeps a `\;` as it is, so a folder with a `;` in its name is passed as `-d .` with
//!   the folder as the working directory. A folder Win32 can't name (a part ending in a dot or a
//!   space, `setup.`) gets no terminal: it would open in the folder without the dot.
//! - macOS: Terminal.app (`open -a Terminal <folder>`).
//! - Linux: the desktop's default terminal (x-terminal-emulator, Debian and Ubuntu), else the first
//!   of the common terminals that is installed (RHEL 9 has no x-terminal-emulator: GNOME Terminal).

use std::path::Path;
use std::process::{Command, Stdio};

/// One terminal to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCommand {
    pub program: &'static str,
    pub args: Vec<String>,
    /// the terminal's working directory: the folder
    pub cwd: String,
}

/// Whether a Windows path has a part Win32 doesn't keep as it is: one ending in a dot or a space
/// ("." and ".." aside), which CreateProcess and the terminals would open without that end.
pub fn win32_unaddressable(path: &str) -> bool {
    let body = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix(r"\\.\"))
        .or_else(|| path.strip_prefix(r"\\"))
        .unwrap_or(path);
    body.split(['\\', '/'])
        .any(|part| part != "." && part != ".." && (part.ends_with('.') || part.ends_with(' ')))
}

/// Absolute in the platform's own sense: a drive (also `\\?\D:\`) or a share on Windows, "/" elsewhere.
pub fn is_absolute_for(platform: &str, dir: &str) -> bool {
    if platform != "windows" {
        return dir.starts_with('/');
    }
    let drive = |s: &str| {
        let b = s.as_bytes();
        b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/')
    };
    if drive(dir) || dir.strip_prefix(r"\\?\").is_some_and(drive) {
        return true;
    }
    // \\server\share…
    let Some(rest) = dir.strip_prefix(r"\\") else {
        return false;
    };
    let mut parts = rest.splitn(2, ['\\', '/']);
    let server = parts.next().unwrap_or("");
    let share = parts.next().unwrap_or("");
    !server.is_empty()
        && !server.starts_with(['?', '.'])
        && !share.is_empty()
        && !share.starts_with(['\\', '/'])
}

/// The terminals to try, in order, for a folder of `platform` (`std::env::consts::OS`); none when
/// there is none for it.
pub fn terminal_commands(platform: &str, dir: &str) -> Vec<TerminalCommand> {
    if dir.is_empty() || dir.contains('\0') || !is_absolute_for(platform, dir) {
        return Vec::new();
    }
    let one = |program: &'static str, args: Vec<String>| TerminalCommand {
        program,
        args,
        cwd: dir.to_owned(),
    };
    match platform {
        "windows" if win32_unaddressable(dir) => Vec::new(),
        "windows" => {
            let at = if dir.contains(';') { "." } else { dir };
            vec![one("wt.exe", vec!["-d".into(), at.into()])]
        }
        "macos" => vec![one(
            "open",
            vec!["-a".into(), "Terminal".into(), dir.into()],
        )],
        _ => {
            let at = || vec![format!("--working-directory={dir}")];
            vec![
                one("x-terminal-emulator", vec![]),
                one("gnome-terminal", at()),
                one(
                    "ptyxis",
                    vec!["--new-window".into(), format!("--working-directory={dir}")],
                ),
                one("kgx", at()),
                one("konsole", vec!["--workdir".into(), dir.into()]),
                one("xfce4-terminal", at()),
                one("mate-terminal", at()),
                one("tilix", at()),
                one("xterm", vec![]),
            ]
        }
    }
}

/// Open a terminal in `dir`: the first of [`terminal_commands`] that starts; it outlives the app.
/// Errors are logged, as the page expects nothing back.
pub fn open_terminal(dir: &str) {
    if !Path::new(dir).is_dir() {
        eprintln!("[shell] no terminal for {dir}: not a folder");
        return;
    }
    let tries = terminal_commands(std::env::consts::OS, dir);
    if tries.is_empty() {
        eprintln!("[shell] no terminal for {dir}: a folder no terminal can open");
        return;
    }
    for t in &tries {
        let mut cmd = Command::new(t.program);
        cmd.args(&t.args)
            .current_dir(&t.cwd)
            // the backend's own variable (lib.rs prepare_environment) is not the terminal's
            .env_remove("SPACEKNIFE_SHELL")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(_) => return,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                eprintln!("[shell] could not start {}: {err}", t.program);
                return;
            }
        }
    }
    let names: Vec<&str> = tries.iter().map(|t| t.program).collect();
    eprintln!(
        "[shell] no terminal for {dir}: none of {} could start",
        names.join(", ")
    );
}
#[cfg(test)]
mod tests {
    use super::*;

    /// What each terminal is started with: program, arguments, working directory.
    fn tries(platform: &str, dir: &str) -> Vec<(String, Vec<String>, Option<String>)> {
        terminal_commands(platform, dir)
            .into_iter()
            .map(|t| (t.program.to_owned(), t.args, Some(t.cwd)))
            .collect()
    }

    /// Folder names a shell would read as more than a name (review issue 1: `x&whoami` ran whoami).
    const AWKWARD: [&str; 9] = [
        "x&whoami",
        "a^b",
        "%PATH%",
        "(x)",
        "two  spaces",
        "it's",
        "$(id)",
        "`id`",
        "!x!",
    ];

    #[test]
    fn windows_starts_windows_terminal_itself_with_the_folder_as_one_argument() {
        for name in AWKWARD {
            let dir = format!("D:\\Work\\{name}");
            assert_eq!(
                tries("windows", &dir),
                vec![(
                    "wt.exe".to_string(),
                    vec!["-d".to_string(), dir.clone()],
                    Some(dir.clone())
                )],
                "{name}"
            );
        }
        for dir in [
            "C:\\Program Files (x86)",
            "D:\\x y\\",
            "\\\\server\\share\\a & b",
            "E:\\",
        ] {
            assert_eq!(tries("windows", dir)[0].1, ["-d", dir], "{dir}");
        }
    }

    #[test]
    fn windows_never_goes_through_cmd() {
        for dir in ["D:\\x&calc", "D:\\a;b", "D:\\Work"] {
            assert!(
                tries("windows", dir)
                    .iter()
                    .all(|(p, _, _)| !p.eq_ignore_ascii_case("cmd.exe")),
                "{dir}"
            );
        }
    }

    #[test]
    fn windows_passes_a_folder_with_a_semicolon_as_dot_and_its_working_directory() {
        // Windows Terminal splits its command line at ';' (and keeps a "\;" as it is)
        for dir in ["D:\\a;b", "D:\\a\\;b", "D:\\x;new-tab -p cmd calc.exe"] {
            assert_eq!(
                tries("windows", dir),
                vec![(
                    "wt.exe".to_string(),
                    vec!["-d".to_string(), ".".to_string()],
                    Some(dir.to_string())
                )],
                "{dir}"
            );
        }
    }

    #[test]
    fn windows_gives_no_terminal_for_a_folder_win32_cannot_name() {
        // a trailing dot or space: the terminal would open the folder without it
        for dir in [
            "D:\\setup.",
            "D:\\setup.\\sub",
            "D:\\trail ",
            "D:\\a\\...",
            "\\\\?\\D:\\setup.",
        ] {
            assert!(win32_unaddressable(dir), "{dir}");
            assert!(tries("windows", dir).is_empty(), "{dir}");
        }
        for dir in [
            "D:\\a.b",
            "D:\\.git",
            "D:\\a\\..\\b",
            "D:\\",
            "\\\\server\\share",
        ] {
            assert!(!win32_unaddressable(dir), "{dir}");
        }
    }

    #[test]
    fn only_absolute_folders() {
        for dir in ["", "relative\\x", "\\x", "D:x", "-d", "\\\\?\\", "D:\\a\0b"] {
            assert!(tries("windows", dir).is_empty(), "{dir:?}");
        }
        for dir in ["", "relative", "~/x", "-e", "/a\0b"] {
            assert!(tries("linux", dir).is_empty(), "{dir:?}");
            assert!(tries("macos", dir).is_empty(), "{dir:?}");
        }
    }

    #[test]
    fn macos_opens_terminal_app_with_the_folder_as_one_argument() {
        for name in AWKWARD.iter().chain(&["a;b", "q\"uote", "a|b"]) {
            let dir = format!("/Users/lenny/{name}");
            assert_eq!(
                tries("macos", &dir),
                vec![(
                    "open".to_string(),
                    vec!["-a".to_string(), "Terminal".into(), dir.clone()],
                    Some(dir.clone())
                )]
            );
        }
    }

    #[test]
    fn linux_tries_the_default_terminal_then_the_common_ones() {
        // RHEL 9 has no x-terminal-emulator: GNOME Terminal there
        let dir = "/home/lenny/x&whoami;$(id)";
        let all = tries("linux", dir);
        let names: Vec<&str> = all.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "x-terminal-emulator",
                "gnome-terminal",
                "ptyxis",
                "kgx",
                "konsole",
                "xfce4-terminal",
                "mate-terminal",
                "tilix",
                "xterm"
            ]
        );
        assert_eq!(
            all[0],
            ("x-terminal-emulator".into(), vec![], Some(dir.into()))
        );
        assert_eq!(all[1].1, [format!("--working-directory={dir}")]);
        assert_eq!(all[4].1, ["--workdir", dir]);
        for (_, args, cwd) in &all {
            assert_eq!(cwd.as_deref(), Some(dir));
            assert!(args.iter().all(|a| a == dir
                || *a == format!("--working-directory={dir}")
                || a.starts_with("--")));
        }
    }
}
