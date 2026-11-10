//! The few texts of the shell (M7.5): dialog titles, backend start-up errors and why a downloaded
//! update can't be started. The language follows the page (set_language) once it knows the
//! setting; before that the system's. (Was apps/electron/src/main/text.ts.)

use std::sync::RwLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    ZhCn,
    En,
}

impl Lang {
    /// "en" is English, anything else Simplified Chinese (the source text), as the page sends it.
    pub fn from_setting(value: &str) -> Self {
        if value == "en" { Self::En } else { Self::ZhCn }
    }

    /// A BCP 47 tag of the system's: Chinese ones are Chinese, the rest English.
    pub fn from_locale(tag: &str) -> Self {
        if tag.to_ascii_lowercase().starts_with("zh") {
            Self::ZhCn
        } else {
            Self::En
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKey {
    PickFolders,
    OpenSnapshot,
    SnapshotFilter,
    BackendTimeout,
    BackendBadLine,
    BackendExited,
    UpdateNoInstall,
    UpdateMissing,
    UpdateNotInUpdates,
    UpdateNotAnUpdate,
    UpdateStartFailed,
    UpdateUnpackFailed,
    UpdateReplaceFailed,
}

fn text(key: TextKey, lang: Lang) -> &'static str {
    use TextKey::*;
    let (zh, en) = match key {
        PickFolders => ("选择要扫描的文件夹", "Choose folders to scan"),
        OpenSnapshot => ("打开快照文件", "Open snapshot file"),
        SnapshotFilter => ("SpaceKnife 快照", "SpaceKnife snapshot"),
        BackendTimeout => (
            "后端 30 秒内没有就绪",
            "The backend wasn't ready within 30 seconds.",
        ),
        BackendBadLine => (
            "后端就绪行无法解析：{line}（{error}）",
            "Can't read the backend's ready line: {line} ({error})",
        ),
        BackendExited => (
            "后端启动失败（退出码 {code}）",
            "The backend failed to start (exit code {code}).",
        ),
        UpdateNoInstall => (
            "这份 SpaceKnife 的安装方式没有安装步骤",
            "This copy of SpaceKnife has no install step for updates.",
        ),
        UpdateMissing => (
            "找不到下载的更新文件",
            "The downloaded update file wasn't found.",
        ),
        UpdateNotInUpdates => (
            "更新文件不在 SpaceKnife 的更新文件夹里",
            "The update file isn't in SpaceKnife's updates folder.",
        ),
        UpdateNotAnUpdate => (
            "这不是这种安装方式的 SpaceKnife 更新文件",
            "This isn't a SpaceKnife update for this kind of install.",
        ),
        UpdateStartFailed => ("无法启动更新：{error}", "Can't start the update: {error}"),
        UpdateUnpackFailed => ("无法解压更新：{error}", "Can't unpack the update: {error}"),
        UpdateReplaceFailed => (
            "无法替换旧版本：{error}",
            "Can't replace the old version: {error}",
        ),
    };
    if lang == Lang::En { en } else { zh }
}

static LANG: RwLock<Option<Lang>> = RwLock::new(None);

/// The system's UI language (Windows: the user's preferred UI languages, not the formats' locale).
pub fn system_lang() -> Lang {
    Lang::from_locale(&sys_locale::get_locale().unwrap_or_default())
}

/// The page's language (set_language).
pub fn set_lang(value: &str) {
    *LANG.write().unwrap_or_else(|e| e.into_inner()) = Some(Lang::from_setting(value));
}

fn lang() -> Lang {
    LANG.read()
        .unwrap_or_else(|e| e.into_inner())
        .unwrap_or_else(system_lang)
}

/// `{name}` placeholders filled from `fields`; unknown ones stay as they are.
pub fn fill(text: &str, fields: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close)
                if !after[..close].is_empty()
                    && after[..close]
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_') =>
            {
                let name = &after[..close];
                match fields.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => out.push_str(v),
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A text of the shell in the current language, placeholders filled in.
pub fn mt(key: TextKey, fields: &[(&str, &str)]) -> String {
    mt_in(lang(), key, fields)
}

pub fn mt_in(lang: Lang, key: TextKey, fields: &[(&str, &str)]) -> String {
    fill(text(key, lang), fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_language_and_fills_placeholders() {
        assert_eq!(
            mt_in(Lang::En, TextKey::PickFolders, &[]),
            "Choose folders to scan"
        );
        assert_eq!(
            mt_in(Lang::ZhCn, TextKey::UpdateStartFailed, &[("error", "x")]),
            "无法启动更新：x"
        );
        assert_eq!(
            mt_in(Lang::En, TextKey::BackendExited, &[("code", "3")]),
            "The backend failed to start (exit code 3)."
        );
        // a placeholder without a value stays
        assert_eq!(
            mt_in(Lang::En, TextKey::BackendBadLine, &[("line", "?")]),
            "Can't read the backend's ready line: ? ({error})"
        );
    }

    #[test]
    fn reads_languages() {
        assert_eq!(Lang::from_setting("en"), Lang::En);
        assert_eq!(Lang::from_setting("zh-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_setting("fr"), Lang::ZhCn);
        assert_eq!(Lang::from_locale("zh-Hans-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_locale("ZH_tw"), Lang::ZhCn);
        assert_eq!(Lang::from_locale("en-US"), Lang::En);
        assert_eq!(Lang::from_locale(""), Lang::En);
    }

    #[test]
    fn fills_only_names() {
        assert_eq!(fill("{a} {b} { } {", &[("a", "1")]), "1 {b} { } {");
    }
}
