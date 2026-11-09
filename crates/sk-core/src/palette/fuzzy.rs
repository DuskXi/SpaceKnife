//! Fuzzy matching for the command palette (UI-W4-30): case-insensitive subsequence with bonuses.
//!
//! A query matches a text when its characters appear in order (spaces in the query are ignored).
//! Contiguous runs, word starts and a match at the very start score higher; gaps cost a little.
//! The best alignment is found by dynamic programming, so "dkr" in "Docker" lights up D, k, r.
//!
//! Indices are character (code point) indices; [`utf16_spans`] converts them for the frontends.

pub const MATCH: i64 = 16;
pub const WORD_START: i64 = 10;
pub const FIRST_CHAR: i64 = 6;
pub const CONSECUTIVE: i64 = 12;
pub const GAP_OPEN: i64 = 3;
pub const GAP_EXTEND: i64 = 1;
pub const LEADING_MAX: i64 = 6;
pub const EXACT: i64 = 400;
pub const PREFIX: i64 = 120;

const NEG: i64 = -(1 << 30);
const SEPARATORS: &str = " _-./\\:;,()[]{}<>·…|+";

/// A match: its score and the matched character runs `[start, end)`.
pub type Scored = (i64, Vec<(usize, usize)>);

fn is_cjk(ch: char) -> bool {
    let o = ch as u32;
    (0x3400..=0x9FFF).contains(&o)
        || (0xF900..=0xFAFF).contains(&o)
        || (0x20000..=0x3134F).contains(&o)
}

fn word_start(text: &[char], j: usize) -> bool {
    if j == 0 {
        return true;
    }
    let (prev, cur) = (text[j - 1], text[j]);
    if SEPARATORS.contains(prev) || is_cjk(cur) {
        return true;
    }
    if prev.is_lowercase() && cur.is_uppercase() {
        return true;
    }
    prev.is_numeric() != cur.is_numeric()
}

/// A character lower-cased as Python's `str.lower` does it (up to three characters).
type Low = [char; 3];

fn low(c: char) -> Low {
    let mut out = ['\0'; 3];
    for (k, l) in c.to_lowercase().take(3).enumerate() {
        out[k] = l;
    }
    out
}

/// (score, matched index runs over `text`), or None if it does not match.
pub fn score(query: &str, text: &str) -> Option<Scored> {
    let q: Vec<Low> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(low)
        .collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let chars: Vec<char> = text.chars().collect();
    let t: Vec<Low> = chars.iter().map(|&c| low(c)).collect();
    let (n, m) = (q.len(), t.len());
    if n > m {
        return None;
    }
    // quick reject: subsequence test
    let mut k = 0;
    for ch in &t {
        if *ch == q[k] {
            k += 1;
            if k == n {
                break;
            }
        }
    }
    if k < n {
        return None;
    }
    let bonus: Vec<i64> = (0..m)
        .map(|j| if word_start(&chars, j) { WORD_START } else { 0 })
        .collect();
    let mut prev = vec![NEG; m];
    let mut back: Vec<Vec<isize>> = Vec::with_capacity(n);
    for j in 0..m {
        if t[j] == q[0] {
            prev[j] = MATCH
                + bonus[j]
                + if j == 0 {
                    FIRST_CHAR
                } else {
                    -(j as i64).min(LEADING_MAX)
                };
        }
    }
    back.push(vec![-1; m]);
    for (i, qi) in q.iter().enumerate().skip(1) {
        let mut cur = vec![NEG; m];
        let mut ptr = vec![-1isize; m];
        // best over k < j - 1 of prev[k] - gap(j - k - 1), kept incrementally
        let (mut best, mut best_k) = (NEG, -1isize);
        for j in i..m {
            if best > NEG {
                best -= GAP_EXTEND;
            }
            if j >= 2 && prev[j - 2] > NEG {
                let cand = prev[j - 2] - GAP_OPEN;
                if cand > best {
                    best = cand;
                    best_k = j as isize - 2;
                }
            }
            if t[j] != *qi {
                continue;
            }
            let via_gap = best;
            let via_run = if prev[j - 1] > NEG {
                prev[j - 1] + CONSECUTIVE
            } else {
                NEG
            };
            if via_run >= via_gap && via_run > NEG {
                cur[j] = via_run + MATCH + bonus[j];
                ptr[j] = j as isize - 1;
            } else if via_gap > NEG {
                cur[j] = via_gap + MATCH + bonus[j];
                ptr[j] = best_k;
            }
        }
        back.push(ptr);
        prev = cur;
    }
    // the first index of the maximum, as Python's max() picks it
    let mut end = 0;
    for j in 1..m {
        if prev[j] > prev[end] {
            end = j;
        }
    }
    let mut total = prev[end];
    if total <= NEG {
        return None;
    }
    let mut picks = vec![end];
    for i in (1..n).rev() {
        let p = back[i][*picks.last().unwrap()];
        picks.push(p as usize);
    }
    picks.reverse();
    let flat = |v: &[Low]| -> Vec<char> {
        v.iter()
            .flat_map(|l| l.iter().copied().filter(|&c| c != '\0'))
            .collect()
    };
    if flat(&t) == flat(&q) {
        total += EXACT;
    } else if picks[0] == 0 && picks[n - 1] == n - 1 {
        total += PREFIX;
    }
    Some((total, runs(&picks)))
}

/// Sorted indices as `[start, end)` runs.
pub fn runs(indices: &[usize]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in indices {
        match out.last_mut() {
            Some(last) if last.1 == i => last.1 = i + 1,
            _ => out.push((i, i + 1)),
        }
    }
    out
}

/// Character index runs -> UTF-16 offsets (what JavaScript and Qt strings index by).
pub fn utf16_spans(text: &str, spans: &[(usize, usize)]) -> Vec<(usize, usize)> {
    if text.chars().all(|c| (c as u32) < 0x10000) {
        return spans.to_vec();
    }
    let mut offsets = vec![0usize];
    for ch in text.chars() {
        offsets.push(offsets.last().unwrap() + ch.len_utf16());
    }
    spans
        .iter()
        .map(|&(a, b)| {
            (
                offsets[a.min(offsets.len() - 1)],
                offsets[b.min(offsets.len() - 1)],
            )
        })
        .collect()
}

/// [`utf16_spans`] as protocol spans.
pub fn protocol_spans(text: &str, spans: &[(usize, usize)]) -> Vec<sk_protocol::palette::Span> {
    utf16_spans(text, spans)
        .into_iter()
        .map(|(a, b)| sk_protocol::palette::Span {
            start: a as u32,
            end: b as u32,
        })
        .collect()
}
