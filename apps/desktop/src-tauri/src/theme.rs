//! The interface theme in the shell (W7 外观 · 主题; was apps/electron/src/main/theme.ts and
//! shared/theme.ts). The window's theme decides the page's `prefers-color-scheme` (tokens.css is
//! light unless it says dark) and the native UI's; 「跟随系统」 follows the system. The last choice
//! is kept in `<ui data>/theme.json` so a start is in the right theme from its first frame, before
//! the backend's settings arrive. Tauri and the file system come in through [`ThemeHost`]
//! (lib.rs), so this is tested without them.

use serde::{Deserialize, Serialize};

/// 「跟随系统」 (the default), or one chosen in the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeSetting {
    System,
    Light,
    Dark,
}

impl ThemeSetting {
    /// One of the three settings, by its name ("system", "light", "dark"); anything else is none.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// The window's own background before the page paints and while it resizes: each theme's
/// color.bg.window (design/tokens; app/lib/theme.test.ts keeps them equal to the tokens).
pub const WINDOW_BG_DARK: &str = "#08090B";
pub const WINDOW_BG_LIGHT: &str = "#E9EBEF";

/// The theme theme.json keeps; anything unreadable is 「跟随系统」.
pub fn parse_theme_file(text: Option<&str>) -> ThemeSetting {
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return ThemeSetting::System;
    };
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| {
            v.get("theme")
                .and_then(|t| t.as_str())
                .and_then(ThemeSetting::parse)
        })
        .unwrap_or(ThemeSetting::System)
}

/// What theme.json holds for a choice.
pub fn theme_file_text(theme: ThemeSetting) -> String {
    format!("{{\"theme\":\"{}\"}}\n", theme.as_str())
}

/// The window and the file, as the keeper sees them.
pub trait ThemeHost {
    /// The window's theme (Tauri's `set_theme`: None for 「跟随系统」).
    fn set_source(&mut self, theme: ThemeSetting);
    /// Whether the window shows dark colours now (the chosen theme, or the system's).
    fn dark(&self) -> bool;
    /// The window's own background, when there is a window.
    fn set_background(&mut self, color: &str);
    /// Replace theme.json with this text (whole or not at all).
    fn write(&mut self, text: &str) -> std::io::Result<()>;
    /// A write that failed: the next start waits for the settings.
    fn warn(&mut self, message: &str);
}

pub struct ThemeKeeper<H: ThemeHost> {
    host: H,
    current: ThemeSetting,
    /// what theme.json holds (None: no file, or a write failed)
    saved: Option<ThemeSetting>,
}

impl<H: ThemeHost> ThemeKeeper<H> {
    /// Restore the theme of theme.json's text (None: no file, 「跟随系统」); before the window exists.
    pub fn new(mut host: H, file_text: Option<&str>) -> Self {
        let theme = parse_theme_file(file_text);
        host.set_source(theme);
        Self {
            host,
            current: theme,
            saved: file_text.map(|_| theme),
        }
    }

    /// The setting in force.
    pub fn setting(&self) -> ThemeSetting {
        self.current
    }

    /// The window's background for the theme on screen: each theme's color.bg.window.
    pub fn background(&self) -> &'static str {
        if self.host.dark() {
            WINDOW_BG_DARK
        } else {
            WINDOW_BG_LIGHT
        }
    }

    /// set_theme from the page: it applied the setting to itself; here the window and the file.
    /// Anything but the three settings is ignored.
    pub fn set(&mut self, theme: &str) {
        let Some(theme) = ThemeSetting::parse(theme) else {
            return;
        };
        self.current = theme;
        self.host.set_source(theme);
        let bg = self.background();
        self.host.set_background(bg);
        if self.saved == Some(theme) {
            return;
        }
        match self.host.write(&theme_file_text(theme)) {
            Ok(()) => self.saved = Some(theme),
            Err(err) => {
                self.saved = None; // written again at the next change
                self.host.warn(&format!("[theme] not saved: {err}"));
            }
        }
    }

    /// The system's theme changed (or the window's did): the window's background follows.
    pub fn updated(&mut self) {
        let bg = self.background();
        self.host.set_background(bg);
    }

    #[cfg(test)]
    fn host(&self) -> &H {
        &self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window as Tauri behaves: dark colours for 「深色」, for 「浅色」 never, else the system's.
    struct Fake {
        source: ThemeSetting,
        system_dark: bool,
        log: Vec<String>,
        files: Vec<String>,
        fail: bool,
    }

    impl Fake {
        fn new(system_dark: bool) -> Self {
            Self {
                source: ThemeSetting::System,
                system_dark,
                log: vec![],
                files: vec![],
                fail: false,
            }
        }
    }

    impl ThemeHost for Fake {
        fn set_source(&mut self, theme: ThemeSetting) {
            self.source = theme;
        }
        fn dark(&self) -> bool {
            self.source == ThemeSetting::Dark
                || (self.source == ThemeSetting::System && self.system_dark)
        }
        fn set_background(&mut self, color: &str) {
            self.log.push(format!("bg {color}"));
        }
        fn write(&mut self, text: &str) -> std::io::Result<()> {
            if self.fail {
                return Err(std::io::Error::other("disk full"));
            }
            self.files.push(text.to_string());
            Ok(())
        }
        fn warn(&mut self, message: &str) {
            self.log.push(message.to_string());
        }
    }

    #[test]
    fn reads_and_writes_the_file() {
        assert_eq!(
            parse_theme_file(Some("{\"theme\":\"dark\"}\n")),
            ThemeSetting::Dark
        );
        assert_eq!(
            parse_theme_file(Some(&theme_file_text(ThemeSetting::Light))),
            ThemeSetting::Light
        );
        for text in [
            None,
            Some(""),
            Some("{"),
            Some("null"),
            Some("[]"),
            Some("{\"theme\":\"sepia\"}"),
            Some("{\"theme\":1}"),
        ] {
            assert_eq!(parse_theme_file(text), ThemeSetting::System, "{text:?}");
        }
        assert_eq!(
            theme_file_text(ThemeSetting::Dark),
            "{\"theme\":\"dark\"}\n"
        );
    }

    #[test]
    fn takes_only_the_three_settings() {
        assert!(
            ["system", "light", "dark"]
                .iter()
                .all(|v| ThemeSetting::parse(v).is_some())
        );
        assert!(
            ["", "Light", "0", "null"]
                .iter()
                .all(|v| ThemeSetting::parse(v).is_none())
        );
    }

    #[test]
    fn restores_the_files_choice_before_the_window() {
        let keeper = ThemeKeeper::new(Fake::new(false), Some("{\"theme\":\"dark\"}\n"));
        assert_eq!(keeper.host().source, ThemeSetting::Dark);
        assert_eq!(keeper.background(), WINDOW_BG_DARK);
        let keeper = ThemeKeeper::new(Fake::new(false), None);
        assert_eq!(keeper.background(), WINDOW_BG_LIGHT);
        assert_eq!(keeper.setting(), ThemeSetting::System);
        let keeper = ThemeKeeper::new(Fake::new(true), Some("not json"));
        assert_eq!(keeper.background(), WINDOW_BG_DARK);
        assert_eq!(keeper.host().source, ThemeSetting::System);
    }

    #[test]
    fn takes_the_pages_setting_once_per_change() {
        let mut keeper = ThemeKeeper::new(Fake::new(false), Some("{\"theme\":\"light\"}\n"));
        keeper.set("light"); // what the file already says: not written again
        keeper.set("dark");
        keeper.set("dark");
        keeper.set("bogus"); // not a setting: ignored
        keeper.set("system");
        let host = keeper.host();
        assert_eq!(host.source, ThemeSetting::System);
        assert_eq!(
            host.log,
            [
                format!("bg {WINDOW_BG_LIGHT}"),
                format!("bg {WINDOW_BG_DARK}"),
                format!("bg {WINDOW_BG_DARK}"),
                format!("bg {WINDOW_BG_LIGHT}")
            ]
        );
        assert_eq!(
            host.files,
            ["{\"theme\":\"dark\"}\n", "{\"theme\":\"system\"}\n"]
        );
    }

    #[test]
    fn writes_again_after_a_write_that_failed() {
        let mut fake = Fake::new(false);
        fake.fail = true;
        let mut keeper = ThemeKeeper::new(fake, None);
        keeper.set("dark");
        assert_eq!(
            keeper.host().log.last().unwrap(),
            "[theme] not saved: disk full"
        );
        keeper.host.fail = false;
        keeper.set("dark"); // the same choice: the file does not hold it yet
        assert_eq!(keeper.host().files, ["{\"theme\":\"dark\"}\n"]);
    }

    #[test]
    fn follows_the_systems_theme() {
        let mut keeper = ThemeKeeper::new(Fake::new(false), None);
        keeper.host.system_dark = true;
        keeper.updated();
        assert_eq!(keeper.host().log, [format!("bg {WINDOW_BG_DARK}")]);
    }
}
