//! Every Chinese text the backend shows has an English entry (the check `tools/i18n_check.py` did
//! for the Python backend): message ids at the translation call sites of sk-core and sk-server —
//! `tr!(…)`, `t(…)`, `t_in(lang, …)`, `tr_with(…)`, `plural(n, …)`, the arms of
//! `t(match key { … => "…" })` tables — and `const NAME: &str = "…"` texts (translated where they
//! are used). Tests and the i18n module's own examples are skipped.

use std::path::{Path, PathBuf};

use sk_core::i18n::{Language, catalog};

/// Chinese texts that are not message ids (matched against system output, not shown).
const NOT_MESSAGES: &[&str] = &["拒绝"];

fn has_han(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if name != "tests" {
                rust_files(&p, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" {
            out.push(p);
        }
    }
}

/// The source without its `#[cfg(test)]` modules.
fn without_tests(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(at) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        // skip the item: up to its matching closing brace (or `;` for a one-line item)
        let body = after.find(['{', ';']).unwrap_or(after.len());
        if after[body..].starts_with(';') {
            rest = &after[body + 1..];
            continue;
        }
        match matching_brace(after, body) {
            Some(end) => rest = &after[end + 1..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Index of the `}` closing the `{` at `open` (string and char literals and comments skipped).
fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'"' => {
                i = string_at(s, i)?.1;
                continue;
            }
            b'r' if (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#'))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_')) =>
            {
                if let Some((_, end)) = string_at(s, i) {
                    i = end;
                    continue;
                }
            }
            b'\'' => {
                // a char literal ('x', '\n', '{'), not a lifetime
                if b.get(i + 2) == Some(&b'\'') {
                    i += 3;
                    continue;
                }
                if b.get(i + 1) == Some(&b'\\')
                    && let Some(close) = s[i + 2..].find('\'')
                {
                    i += close + 3;
                    continue;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = s[i..].find('\n').map_or(b.len(), |n| i + n);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The string literal starting at `at` ("…" or r"…" / r#"…"#) and the index after it.
fn string_at(s: &str, at: usize) -> Option<(String, usize)> {
    let b = s.as_bytes();
    if b[at] == b'r' {
        let hashes = s[at + 1..].bytes().take_while(|&c| c == b'#').count();
        let start = at + 1 + hashes;
        if b.get(start) != Some(&b'"') {
            return None;
        }
        let close = format!("\"{}", "#".repeat(hashes));
        let end = s[start + 1..].find(&close)? + start + 1;
        return Some((s[start + 1..end].to_string(), end + close.len()));
    }
    if b[at] != b'"' {
        return None;
    }
    let mut out = String::new();
    let mut chars = s[at + 1..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, at + 1 + i + 1)),
            '\\' => {
                let (_, e) = chars.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    '\\' | '"' | '\'' => out.push(e),
                    'u' => {
                        let mut hex = String::new();
                        for (_, h) in chars.by_ref() {
                            match h {
                                '{' => {}
                                '}' => break,
                                _ => hex.push(h),
                            }
                        }
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    'x' => {
                        let hex: String = [chars.next()?.1, chars.next()?.1].iter().collect();
                        out.push(u8::from_str_radix(&hex, 16).ok()? as char);
                    }
                    '\n' => {
                        // line continuation: the next line's leading whitespace goes too
                        while chars.clone().next().is_some_and(|(_, w)| w.is_whitespace()) {
                            chars.next();
                        }
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            c => out.push(c),
        }
    }
    None
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Index after the first top-level `,` from `i` within one argument list (None: the call ends).
fn after_first_arg(s: &str, mut i: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            b',' if depth == 0 => return Some(i + 1),
            b'"' => {
                i = string_at(s, i)?.1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Message ids of one source file: (line, text).
fn message_ids(src: &str) -> Vec<(usize, String)> {
    let line_of = |at: usize| src[..at].matches('\n').count() + 1;
    let ident_before = |at: usize| {
        at > 0 && {
            let c = src.as_bytes()[at - 1];
            c.is_ascii_alphanumeric() || c == b'_'
        }
    };
    let mut out = Vec::new();
    // call sites: (head, arguments before the text)
    for (head, skip) in [("tr!(", 0), ("t(", 0), ("tr_with(", 0), ("t_in(", 1), ("plural(", 1)] {
        let mut from = 0;
        while let Some(found) = src[from..].find(head) {
            let at = from + found;
            from = at + head.len();
            if ident_before(at) || src[..at].ends_with("fn ") {
                continue;
            }
            let mut i = at + head.len();
            if skip == 1 {
                match after_first_arg(src, i) {
                    Some(next) => i = next,
                    None => continue,
                }
            }
            i = skip_ws(src, i);
            if src[i..].starts_with("match ") {
                // t(match key { "k" => "text", … })
                let Some(open) = src[i..].find('{').map(|o| i + o) else { continue };
                let Some(close) = matching_brace(src, open) else { continue };
                let body = &src[open..close];
                let mut k = 0;
                while let Some(arrow) = body[k..].find("=>") {
                    let j = skip_ws(body, k + arrow + 2);
                    k = k + arrow + 2;
                    if let Some((text, _)) = (j < body.len()).then(|| string_at(body, j)).flatten() {
                        out.push((line_of(open + j), text));
                    }
                }
                continue;
            }
            if i < src.len()
                && let Some((text, _)) = string_at(src, i)
            {
                out.push((line_of(i), text));
            }
        }
    }
    // const NAME: &str = "…";
    let mut from = 0;
    while let Some(found) = src[from..].find("const ") {
        let at = from + found;
        from = at + 6;
        if ident_before(at) {
            continue;
        }
        let line_end = src[at..].find('\n').map_or(src.len(), |n| at + n);
        let line = &src[at..line_end];
        if !(line.contains(": &str = \"") || line.contains(": &'static str = \"")) {
            continue;
        }
        let q = at + line.find('"').unwrap();
        if let Some((text, _)) = string_at(src, q) {
            out.push((line_of(q), text));
        }
    }
    out.retain(|(_, t)| has_han(t) && !NOT_MESSAGES.contains(&t.as_str()));
    out
}

#[test]
fn every_message_id_has_an_english_entry() {
    let en = catalog(Language::En);
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for dir in ["sk-core/src", "sk-server/src"] {
        rust_files(&crates.join(dir), &mut files);
    }
    files.sort();
    let mut seen = 0;
    let mut missing = Vec::new();
    for file in &files {
        if file.ends_with("i18n.rs") {
            continue; // its documentation examples are not messages
        }
        let src = without_tests(&std::fs::read_to_string(file).unwrap());
        for (line, text) in message_ids(&src) {
            seen += 1;
            if !en.contains_key(text.as_str()) {
                let rel = file.strip_prefix(crates).unwrap_or(file).display().to_string();
                missing.push(format!("{rel}:{line}: {text:?}"));
            }
        }
    }
    assert!(seen > 500, "the scan found only {seen} message ids: is it still reading the sources?");
    assert!(missing.is_empty(), "{} texts without an English entry:\n{}", missing.len(), missing.join("\n"));
}

#[test]
fn the_scanner_finds_each_form() {
    let src = r##"
fn f(n: i64) {
    let a = tr!("甲 {x}", x = 1);
    let b = crate::i18n::t("乙");
    let c = t_in(Language::En, "丙");
    let d = plural(n, "{count} 丁", "1 thing", &[]);
    let e = tr!(
        "戊 \
         续",
        y = 2
    );
    let g = text(t(match k { "a" => "己", _ => "" }));
    let h = format("not a message 庚", &[]);
}
pub const X: &str = "辛";
#[cfg(test)]
mod tests {
    fn t() { tr!("壬"); }
}
"##;
    let got: Vec<String> = message_ids(&without_tests(src)).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, ["甲 {x}", "戊 续", "乙", "己", "丙", "{count} 丁", "辛"]);
}
