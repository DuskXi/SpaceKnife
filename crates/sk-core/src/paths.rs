//! Per-user application directories (the same places platformdirs gave the Python backend).
//!
//! `SPACEKNIFE_HOME` overrides everything (tests and isolated demos use it so they never touch the
//! real user data): `<home>/data`, `<home>/config`, `<home>/cache`.
//!
//! | | data | config | cache |
//! |---|---|---|---|
//! | Windows | `%LOCALAPPDATA%\SpaceKnife` | same as data | `%LOCALAPPDATA%\SpaceKnife\Cache` |
//! | macOS | `~/Library/Application Support/SpaceKnife` | same as data | `~/Library/Caches/SpaceKnife` |
//! | Linux | `$XDG_DATA_HOME/SpaceKnife` (`~/.local/share`) | `$XDG_CONFIG_HOME/SpaceKnife` (`~/.config`) | `$XDG_CACHE_HOME/SpaceKnife` (`~/.cache`) |

use std::path::PathBuf;

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
    let value = std::env::var("SPACEKNIFE_HOME").ok().filter(|v| !v.is_empty())?;
    let p = expand_user(&value);
    Some(std::path::absolute(&p).unwrap_or(p))
}

#[cfg(windows)]
fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("AppData").join("Local"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty() && std::path::Path::new(v).is_absolute())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(fallback))
}

fn platform_data_dir() -> PathBuf {
    #[cfg(windows)]
    return local_app_data().join(APP_NAME);
    #[cfg(target_os = "macos")]
    return home_dir().join("Library/Application Support").join(APP_NAME);
    #[cfg(all(unix, not(target_os = "macos")))]
    return xdg("XDG_DATA_HOME", ".local/share").join(APP_NAME);
}

fn platform_config_dir() -> PathBuf {
    #[cfg(any(windows, target_os = "macos"))]
    return platform_data_dir();
    #[cfg(all(unix, not(target_os = "macos")))]
    return xdg("XDG_CONFIG_HOME", ".config").join(APP_NAME);
}

fn platform_cache_dir() -> PathBuf {
    #[cfg(windows)]
    return local_app_data().join(APP_NAME).join("Cache");
    #[cfg(target_os = "macos")]
    return home_dir().join("Library/Caches").join(APP_NAME);
    #[cfg(all(unix, not(target_os = "macos")))]
    return xdg("XDG_CACHE_HOME", ".cache").join(APP_NAME);
}

pub fn data_dir() -> PathBuf {
    home_override().map_or_else(platform_data_dir, |h| h.join("data"))
}

pub fn config_dir() -> PathBuf {
    home_override().map_or_else(platform_config_dir, |h| h.join("config"))
}

pub fn cache_dir() -> PathBuf {
    home_override().map_or_else(platform_cache_dir, |h| h.join("cache"))
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
