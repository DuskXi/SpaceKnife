//! Per-user application directories (the same places platformdirs 4.13 gave the Python backend,
//! so 0.3.x's data is found where it left it).
//!
//! `SPACEKNIFE_HOME` overrides everything (tests and isolated demos use it so they never touch the
//! real user data): `<home>/data`, `<home>/config`, `<home>/cache`.
//!
//! | | data | config | cache |
//! |---|---|---|---|
//! | Windows | `%LOCALAPPDATA%\SpaceKnife` | same as data | `%LOCALAPPDATA%\SpaceKnife\Cache` |
//! | macOS | `$XDG_DATA_HOME/SpaceKnife`, else `~/Library/Application Support/SpaceKnife` | `$XDG_CONFIG_HOME/SpaceKnife`, else as data | `$XDG_CACHE_HOME/SpaceKnife`, else `~/Library/Caches/SpaceKnife` |
//! | Linux | `$XDG_DATA_HOME/SpaceKnife` (`~/.local/share`) | `$XDG_CONFIG_HOME/SpaceKnife` (`~/.config`) | `$XDG_CACHE_HOME/SpaceKnife` (`~/.cache`) |
//!
//! An `XDG_*` variable counts when, without the white space around it, it is an absolute path
//! (platformdirs' rule; on macOS too, where platformdirs has honoured them since 4.4).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "SpaceKnife";

/// The user's home folder (`~`).
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let var = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let var = std::env::var_os("HOME");
    var.filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~` / `~/x` expanded to the home folder.
pub fn expand_user(path: &str) -> PathBuf {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

fn home_override() -> Option<PathBuf> {
    let value = std::env::var("SPACEKNIFE_HOME")
        .ok()
        .filter(|v| !v.is_empty())?;
    let p = expand_user(&value);
    Some(std::path::absolute(&p).unwrap_or(p))
}

/// Whose rules the folders follow (a parameter, so the tests try each everywhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum System {
    Windows,
    Mac,
    Linux,
}

impl System {
    const NATIVE: System = if cfg!(windows) {
        System::Windows
    } else if cfg!(target_os = "macos") {
        System::Mac
    } else {
        System::Linux
    };
}

/// The data, config and cache folders (before `SPACEKNIFE_HOME`).
struct Dirs {
    data: PathBuf,
    config: PathBuf,
    cache: PathBuf,
}

/// Python's `str.strip()` (its white space: Rust's and the four separators `\x1c`–`\x1f`).
fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// platformdirs' `_xdg_dir`: the variable without the white space around it, if that is an
/// absolute POSIX path.
fn xdg_dir(value: Option<OsString>) -> Option<PathBuf> {
    let value = value?;
    if let Some(s) = value.to_str() {
        let s = py_strip(s);
        return s.starts_with('/').then(|| PathBuf::from(s));
    }
    #[cfg(unix)]
    {
        // not UTF-8: the bytes as they are, without the ASCII white space around them
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let b = value.as_bytes();
        let ws = |c: &u8| matches!(c, b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | 0x1c..=0x1f | b' ');
        let start = b.iter().position(|c| !ws(c)).unwrap_or(b.len());
        let end = b.iter().rposition(|c| !ws(c)).map_or(start, |e| e + 1);
        let b = &b[start..end];
        b.starts_with(b"/")
            .then(|| PathBuf::from(OsString::from_vec(b.to_vec())))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn dirs_for(system: System, var: &dyn Fn(&str) -> Option<OsString>, home: &Path) -> Dirs {
    let xdg = |name: &str, fallback: PathBuf| xdg_dir(var(name)).unwrap_or(fallback).join(APP_NAME);
    match system {
        System::Windows => {
            let local = var("LOCALAPPDATA")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData").join("Local"));
            let data = local.join(APP_NAME);
            Dirs {
                config: data.clone(),
                cache: data.join("Cache"),
                data,
            }
        }
        System::Mac => {
            let support = home.join("Library/Application Support");
            Dirs {
                data: xdg("XDG_DATA_HOME", support.clone()),
                config: xdg("XDG_CONFIG_HOME", support),
                cache: xdg("XDG_CACHE_HOME", home.join("Library/Caches")),
            }
        }
        System::Linux => Dirs {
            data: xdg("XDG_DATA_HOME", home.join(".local/share")),
            config: xdg("XDG_CONFIG_HOME", home.join(".config")),
            cache: xdg("XDG_CACHE_HOME", home.join(".cache")),
        },
    }
}

fn platform_dirs() -> Dirs {
    dirs_for(System::NATIVE, &|name| std::env::var_os(name), &home_dir())
}

pub fn data_dir() -> PathBuf {
    home_override().map_or_else(|| platform_dirs().data, |h| h.join("data"))
}

pub fn config_dir() -> PathBuf {
    home_override().map_or_else(|| platform_dirs().config, |h| h.join("config"))
}

pub fn cache_dir() -> PathBuf {
    home_override().map_or_else(|| platform_dirs().cache, |h| h.join("cache"))
}

pub fn snapshots_dir() -> PathBuf {
    data_dir().join("snapshots")
}

pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// The rotating log file (`logs/spaceknife.log`).
pub fn log_file() -> PathBuf {
    logs_dir().join("spaceknife.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(system: System, env: &[(&str, &str)]) -> (PathBuf, PathBuf, PathBuf) {
        let env: Vec<(String, OsString)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        let var = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let d = dirs_for(system, &var, Path::new("/home/lenny"));
        (d.data, d.config, d.cache)
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// macOS: what platformdirs 4.13 gave 0.3.x, `XDG_*` included (a user who set them, or
    /// started it from a shell that did, finds the data where 0.3.x put it).
    #[test]
    fn macos_follows_platformdirs() {
        let support = p("/home/lenny/Library/Application Support/SpaceKnife");
        assert_eq!(
            dirs(System::Mac, &[]),
            (
                support.clone(),
                support.clone(),
                p("/home/lenny/Library/Caches/SpaceKnife")
            )
        );
        assert_eq!(
            dirs(
                System::Mac,
                &[
                    ("XDG_DATA_HOME", "/data/xdg"),
                    ("XDG_CONFIG_HOME", " /conf/xdg\n"),
                    ("XDG_CACHE_HOME", "/cache/xdg")
                ]
            ),
            (
                p("/data/xdg/SpaceKnife"),
                p("/conf/xdg/SpaceKnife"),
                p("/cache/xdg/SpaceKnife")
            )
        );
        // relative or empty: as if not set
        assert_eq!(
            dirs(
                System::Mac,
                &[("XDG_DATA_HOME", "rel/x"), ("XDG_CACHE_HOME", "")]
            ),
            (
                support.clone(),
                support,
                p("/home/lenny/Library/Caches/SpaceKnife")
            )
        );
    }

    #[test]
    fn linux_follows_platformdirs() {
        assert_eq!(
            dirs(System::Linux, &[]),
            (
                p("/home/lenny/.local/share/SpaceKnife"),
                p("/home/lenny/.config/SpaceKnife"),
                p("/home/lenny/.cache/SpaceKnife")
            )
        );
        assert_eq!(
            dirs(
                System::Linux,
                &[
                    ("XDG_DATA_HOME", "  /x/data  "),
                    ("XDG_CONFIG_HOME", "~/conf")
                ]
            ),
            (
                p("/x/data/SpaceKnife"),
                p("/home/lenny/.config/SpaceKnife"),
                p("/home/lenny/.cache/SpaceKnife")
            )
        );
    }

    #[test]
    fn windows_keeps_its_folders() {
        let (data, config, cache) = dirs(
            System::Windows,
            &[
                ("LOCALAPPDATA", "C:/Users/lenny/AppData/Local"),
                ("XDG_DATA_HOME", "/x"),
            ],
        );
        assert_eq!(data, p("C:/Users/lenny/AppData/Local").join("SpaceKnife"));
        assert_eq!(config, data);
        assert_eq!(cache, data.join("Cache"));
    }
}
