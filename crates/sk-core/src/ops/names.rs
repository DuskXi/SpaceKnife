//! Name checks shared with the scanner (stand-ins for the parts of `scan/worker.py` that file
//! operations need).
// TODO(scan): use the scan worker's `names_below`, `task_is`, `unnameable`, `final_name` once
// they are ported, and delete this module.

/// A name no Windows path can hold: a separator, the stream separator `:`, a wildcard or DOS
/// wildcard (`* ? " < >`), `|` or a control character.
pub fn unnameable_windows(name: &str) -> bool {
    name.chars()
        .any(|c| "\\/:*?\"<>|".contains(c) || ('\u{1}'..='\u{1f}').contains(&c))
}

/// A name no path of this system can hold (a delete by such a path would reach something else).
pub fn unnameable(name: &str) -> bool {
    if cfg!(windows) {
        unnameable_windows(name)
    } else {
        name.contains('/')
    }
}

/// The names of `path` below the scan root `root` as given, or None for the root itself, a
/// path not below it, or no root.
pub fn names_below(path: &str, root: &str) -> Option<Vec<String>> {
    if root.is_empty() || !path.starts_with(root) {
        return None;
    }
    let rest = &path[root.len()..];
    if !(root.ends_with(['\\', '/']) || rest.starts_with(['\\', '/'])) {
        return None; // C:\data2 is not below C:\data
    }
    let below: Vec<String> = rest
        .split(['\\', '/'])
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();
    (!below.is_empty()).then_some(below)
}

fn names(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|n| !n.is_empty()).collect()
}

/// Whether a folder is stored under the names its path gives it below the scan root, exactly:
/// its stored name is the root's own (`root`) followed by `below`. Without the root's, its last
/// names must be `below`.
pub fn task_is(stored: &str, root: Option<&str>, below: &[String]) -> bool {
    let got = names(stored);
    match root {
        Some(r) => {
            let mut want = names(r);
            want.extend(below.iter().map(String::as_str));
            got == want
        }
        None => {
            got.len() >= below.len()
                && got[got.len() - below.len()..]
                    .iter()
                    .zip(below)
                    .all(|(a, b)| *a == b.as_str())
        }
    }
}

/// The stored name of what `path` opens (Windows), else None.
pub fn final_name(path: &str) -> Option<String> {
    #[cfg(windows)]
    {
        super::win::final_name(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

/// `path` as Win32 calls must get it (long or trailing-dot names: `\\?\`); elsewhere itself.
pub fn long_path(path: &str) -> String {
    #[cfg(windows)]
    {
        super::win::long_path(path)
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_and_task() {
        assert_eq!(names_below("C:\\data\\a\\b", "C:\\data"), Some(vec!["a".into(), "b".into()]));
        assert_eq!(names_below("C:\\data2\\a", "C:\\data"), None);
        assert_eq!(names_below("C:\\data", "C:\\data"), None);
        let below = vec!["a".to_string(), "B".to_string()];
        assert!(task_is("\\data\\a\\B", Some("\\data"), &below));
        assert!(!task_is("\\data\\a\\b", Some("\\data"), &below));
        assert!(task_is("\\x\\a\\B", None, &below));
        assert!(unnameable_windows("a:b") && !unnameable_windows("ab"));
    }
}
