//! `filter_syntax::casefold` is Python 3.12's `str.casefold`, the one 0.3.x compared class names
//! and spellings with, for every code point of Unicode (the list in `data/` is Python's own output:
//! tools/gen_casefold.py).

use std::collections::HashMap;

use sk_protocol::filter_syntax::casefold;

fn python() -> HashMap<u32, String> {
    let text = include_str!("data/casefold_python312.txt");
    let mut out = HashMap::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (from, to) = line.split_once(';').expect("code point; folded");
        let cp = |h: &str| u32::from_str_radix(h.trim(), 16).expect("hex code point");
        let folded: String = to
            .split_whitespace()
            .map(|h| char::from_u32(cp(h)).expect("a scalar value"))
            .collect();
        out.insert(cp(from), folded);
    }
    out
}

#[test]
fn every_code_point_folds_as_python_312() {
    let py = python();
    assert!(
        py.len() > 1500,
        "the list is complete: {} entries",
        py.len()
    );
    let mut wrong = Vec::new();
    for cp in (0..=0x10FFFFu32).filter(|c| !(0xD800..=0xDFFF).contains(c)) {
        let c = char::from_u32(cp).unwrap();
        let want = py.get(&cp).cloned().unwrap_or_else(|| c.to_string());
        let got = casefold(c.encode_utf8(&mut [0; 4]));
        if got != want {
            wrong.push(format!("U+{cp:04X}: {got:?} (Python {want:?})"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} code points fold otherwise than in Python 3.12, e.g. {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(12)]
    );
}

/// A string folds code point by code point (no context: Python's final sigma is lower(), not
/// casefold()).
#[test]
fn strings_fold_code_point_by_code_point() {
    assert_eq!(casefold("Straße ΣΑΣ"), "strasse σασ");
    assert_eq!(casefold("İstanbul"), "i\u{307}stanbul");
    assert_eq!(casefold("ﬃ ŉ"), "ffi ʼn");
    // Cherokee folds to the capital letters
    assert_eq!(casefold("ꭰᏸ"), "ᎠᏰ");
    // letters newer than Unicode 15.0 (0.3.4's Python) stay as they are
    assert_eq!(casefold("\u{A7CB}"), "\u{A7CB}");
    assert_eq!(casefold("视频 AUDIO/Music"), "视频 audio/music");
}
