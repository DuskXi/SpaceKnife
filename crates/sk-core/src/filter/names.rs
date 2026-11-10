//! Glob matching over every name of a store without an object per node (Python `filter/names.py`).
//!
//! The names are searched where the store keeps them: NUL-separated runs (`\0name\0name…`) in its
//! name heap, viewed without copying ([`Nodes::names_run`]). The index keeps, per segment of up to
//! [`SEGMENT_NODES`] nodes, the NUL offsets (4 B a name) and, once the palette asked, a 64-bit
//! character set per name (8 B). Segments are searched in parallel. Segments are appended as the
//! store grows (scans in progress), so earlier work is reused; results are cached per pattern as
//! complete up to the end of the last segment they covered.
//!
//! Case-insensitivity uses simple per-character case variants (the same rule on every platform):
//! an ASCII pattern compares against ASCII-lower-cased names; any other pattern compares each
//! literal character against its single-character lower / upper / title-case variants.
//!
//! One index serves every thread of its store. It is owned by whoever owns the store's other
//! per-scan caches (the server's scan); [`NameIndex::forget`] drops everything when the store's
//! content was replaced (a compaction adopted). A replaced store is also detected on the next query.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use parking_lot::{Mutex, RwLock};
use rayon::prelude::*;
use sk_protocol::filter_syntax::{GlobPart, glob_parts};
use sk_store::{NodeId, Nodes};

/// Nodes per segment: bounds the work of one parallel task.
pub const SEGMENT_NODES: usize = 1 << 18;
const CACHE_PATTERNS: usize = 16;
const CACHE_BYTES: usize = 256 << 20;

// --------------------------------------------------------------------------- glob matching

#[inline]
fn lower(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

/// Length of the character at `s[i..]` as the Python regex `_ONE_CHAR` takes it: a whole UTF-8
/// sequence, else one stray byte.
#[inline]
fn one_char_len(s: &[u8], i: usize) -> usize {
    let b = s[i];
    let cont = |k: usize| s.get(i + k).is_some_and(|c| (0x80..=0xbf).contains(c));
    match b {
        0xc2..=0xdf if cont(1) => 2,
        0xe0..=0xef if cont(1) && cont(2) => 3,
        0xf0..=0xf4 if cont(1) && cont(2) && cont(3) => 4,
        _ => 1,
    }
}

/// Title case of a character where it differs from upper case (the Latin digraphs).
fn title_of(c: char) -> Option<char> {
    match c {
        '\u{1c4}'..='\u{1c6}' => Some('\u{1c5}'),
        '\u{1c7}'..='\u{1c9}' => Some('\u{1c8}'),
        '\u{1ca}'..='\u{1cc}' => Some('\u{1cb}'),
        '\u{1f1}'..='\u{1f3}' => Some('\u{1f2}'),
        _ => None,
    }
}

fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    let c = it.next()?;
    if it.next().is_some() { None } else { Some(c) }
}

/// The single-character case variants of `ch`, UTF-8 encoded.
fn char_variants(ch: char) -> Vec<Vec<u8>> {
    let mut out: Vec<char> = vec![ch];
    for v in [
        single(ch.to_lowercase()),
        single(ch.to_uppercase()),
        title_of(ch).or_else(|| single(ch.to_uppercase())),
    ]
    .into_iter()
    .flatten()
    {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out.iter()
        .map(|c| {
            let mut b = [0u8; 4];
            c.encode_utf8(&mut b).as_bytes().to_vec()
        })
        .collect()
}

#[derive(Clone, Debug)]
enum Tok {
    Any,
    One,
    /// ASCII-lower-cased byte (lower mode)
    Byte(u8),
    /// one character in any of its case variants
    Char(Vec<Vec<u8>>),
}

/// A normalised glob compiled for matching name bytes (`*` any bytes, `?` one character).
#[derive(Clone, Debug)]
pub struct Glob {
    toks: Vec<Tok>,
    /// `*text*` with an ASCII text: a plain substring search
    contains: Option<Vec<u8>>,
    /// `*`: every name
    all: bool,
}

impl Glob {
    pub fn new(pattern: &str) -> Glob {
        let parts = glob_parts(pattern);
        let ascii = pattern.is_ascii();
        let all = parts == [GlobPart::Any];
        let contains = match parts.as_slice() {
            [GlobPart::Any, GlobPart::Lit(t), GlobPart::Any] if ascii => {
                Some(t.bytes().map(lower).collect())
            }
            _ => None,
        };
        let mut toks = Vec::new();
        for p in &parts {
            match p {
                GlobPart::Any => toks.push(Tok::Any),
                GlobPart::One => toks.push(Tok::One),
                GlobPart::Lit(t) => {
                    for ch in t.chars() {
                        if ascii {
                            toks.push(Tok::Byte(lower(ch as u8)));
                        } else {
                            toks.push(Tok::Char(char_variants(ch)));
                        }
                    }
                }
            }
        }
        Glob {
            toks,

            contains,
            all,
        }
    }

    /// Whether the whole name matches.
    pub fn matches(&self, name: &[u8]) -> bool {
        if self.all {
            return true;
        }
        if let Some(lit) = &self.contains {
            return contains_lower(name, lit);
        }
        let toks = &self.toks;
        let (mut ti, mut si) = (0usize, 0usize);
        let mut star: Option<(usize, usize)> = None; // (token after the star, name position)
        loop {
            if ti < toks.len() {
                match &toks[ti] {
                    Tok::Any => {
                        ti += 1;
                        star = Some((ti, si));
                        continue;
                    }
                    Tok::One => {
                        if si < name.len() {
                            si += one_char_len(name, si);
                            ti += 1;
                            continue;
                        }
                    }
                    Tok::Byte(b) => {
                        if si < name.len() && lower(name[si]) == *b {
                            si += 1;
                            ti += 1;
                            continue;
                        }
                    }
                    Tok::Char(vs) => {
                        if let Some(v) = vs.iter().find(|v| name[si..].starts_with(v)) {
                            si += v.len();
                            ti += 1;
                            continue;
                        }
                    }
                }
            } else if si == name.len() {
                return true;
            }
            // mismatch: let the last star take one more byte
            match star {
                Some((t, s)) if s < name.len() => {
                    star = Some((t, s + 1));
                    ti = t;
                    si = s + 1;
                }
                _ => return false,
            }
        }
    }
}

/// `name` (ASCII-lower-cased) contains `lit` (lower-case ASCII).
#[inline]
fn contains_lower(name: &[u8], lit: &[u8]) -> bool {
    if lit.is_empty() {
        return true;
    }
    if name.len() < lit.len() {
        return false;
    }
    let first = lit[0];
    let last = name.len() - lit.len();
    let mut i = 0;
    while i <= last {
        if lower(name[i]) == first
            && name[i + 1..i + lit.len()]
                .iter()
                .zip(&lit[1..])
                .all(|(a, b)| lower(*a) == *b)
        {
            return true;
        }
        i += 1;
    }
    false
}

// --------------------------------------------------------------------------- character sets

const fn sig_bit(b: u8) -> u64 {
    let bit = if b == 0 {
        return 0;
    } else if b >= b'a' && b <= b'z' {
        b - b'a'
    } else if b >= b'A' && b <= b'Z' {
        b - b'A'
    } else if b >= b'0' && b <= b'9' {
        26 + b - b'0'
    } else if b == b'.' {
        36
    } else if b == b'_' {
        37
    } else if b == b'-' {
        38
    } else if b == b' ' {
        39
    } else if b < 128 {
        40 + b % 22
    } else {
        62
    };
    1u64 << bit
}

static SIG: [u64; 256] = {
    let mut t = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = sig_bit(i as u8);
        i += 1;
    }
    t
};

fn signature(name: &[u8]) -> u64 {
    name.iter().fold(0u64, |acc, &b| acc | SIG[b as usize])
}

// --------------------------------------------------------------------------- the index

struct Segment {
    first: NodeId,
    stop: NodeId,
    /// name offset of `first`: detects a replaced store
    first_off: u64,
    /// position of every name's leading NUL in the run
    seps: Vec<u32>,
    /// per name: the (case-folded) characters it holds (palette searches)
    sigs: OnceLock<Vec<u64>>,
}

impl Segment {
    fn names<'a>(&self, nodes: &'a Nodes) -> Option<&'a [u8]> {
        let r = nodes.names_run(self.first, self.stop);
        (r.stop == self.stop).then_some(r.run)
    }

    /// Calls `f(local index, name bytes)` for every name.
    #[inline]
    fn each<'a>(&self, run: &'a [u8], mut f: impl FnMut(usize, &'a [u8])) {
        let seps = &self.seps;
        for k in 0..seps.len() {
            let a = seps[k] as usize + 1;
            let b = seps.get(k + 1).map_or(run.len(), |&s| s as usize);
            f(k, &run[a..b]);
        }
    }

    #[inline]
    fn name<'a>(&self, run: &'a [u8], k: usize) -> &'a [u8] {
        let a = self.seps[k] as usize + 1;
        let b = self.seps.get(k + 1).map_or(run.len(), |&s| s as usize);
        &run[a..b]
    }

    fn sigs(&self, run: &[u8]) -> &[u64] {
        self.sigs.get_or_init(|| {
            let mut out = Vec::with_capacity(self.seps.len());
            self.each(run, |_, name| out.push(signature(name)));
            out
        })
    }
}

#[derive(Clone)]
enum Cached {
    Ids(Arc<Vec<NodeId>>),
    Tiers(Arc<[Vec<NodeId>; 4]>),
}

impl Cached {
    fn bytes(&self) -> usize {
        match self {
            Cached::Ids(v) => v.len() * 4,
            Cached::Tiers(t) => t.iter().map(|v| v.len() * 4).sum(),
        }
    }
}

#[derive(Default)]
struct State {
    /// nodes covered by the segments
    n: usize,
    segments: Vec<Arc<Segment>>,
}

/// The name index of one store (see the module docs). Thread-safe.
#[derive(Default)]
pub struct NameIndex {
    state: RwLock<State>,
    /// one thread at a time adds segments
    build: Mutex<()>,
    /// key -> (complete below, result), least recently used first
    cache: Mutex<VecDeque<(String, usize, Cached)>>,
    segment_nodes: usize,
}

/// Snapshot of the segments covering at least the nodes asked for.
type Snapshot = (Vec<Arc<Segment>>, usize);

impl NameIndex {
    pub fn new() -> NameIndex {
        NameIndex::with_segment_nodes(SEGMENT_NODES)
    }

    /// An index with segments of `nodes` names (tests: many segments).
    pub fn with_segment_nodes(nodes: usize) -> NameIndex {
        NameIndex {
            segment_nodes: nodes.max(1),
            ..NameIndex::default()
        }
    }

    /// Drop everything (the store's content was replaced): the next query starts anew.
    pub fn forget(&self) {
        let _b = self.build.lock();
        *self.state.write() = State::default();
        self.cache.lock().clear();
    }

    /// Nodes indexed so far.
    pub fn len(&self) -> usize {
        self.state.read().n
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `(first id, names)` of every segment, in order (tests, diagnostics).
    pub fn segments(&self) -> Vec<(NodeId, usize)> {
        self.state
            .read()
            .segments
            .iter()
            .map(|s| (s.first, s.seps.len()))
            .collect()
    }

    /// Bytes held: separator offsets, character sets and cached results.
    pub fn memory_bytes(&self) -> usize {
        let st = self.state.read();
        let segs: usize = st
            .segments
            .iter()
            .map(|s| s.seps.len() * 4 + s.sigs.get().map_or(0, |v| v.len() * 8))
            .sum();
        segs + self
            .cache
            .lock()
            .iter()
            .map(|(_, _, c)| c.bytes())
            .sum::<usize>()
    }

    fn stale(&self, nodes: &Nodes) -> bool {
        let st = self.state.read();
        if st.n > nodes.len() {
            return true;
        }
        st.segments
            .last()
            .is_some_and(|s| nodes.name_off(s.first) != s.first_off)
    }

    /// Index nodes up to `n` (at most `nodes.len()`).
    pub fn update(&self, nodes: &Nodes, n: usize) {
        let n = n.min(nodes.len());
        if self.stale(nodes) {
            self.forget();
        }
        if self.state.read().n >= n {
            return;
        }
        let _b = self.build.lock();
        loop {
            let a = self.state.read().n;
            if a >= n {
                break;
            }
            let b = n.min(a + self.segment_nodes);
            let r = nodes.names_run(a as NodeId, b as NodeId);
            let seg = Segment {
                first: a as NodeId,
                stop: r.stop,
                first_off: nodes.name_off(a as NodeId),
                seps: r.seps(),
                sigs: OnceLock::new(),
            };
            let mut st = self.state.write();
            st.n = r.stop as usize;
            st.segments.push(Arc::new(seg));
        }
    }

    fn snapshot(&self, nodes: &Nodes, n: usize) -> Snapshot {
        self.update(nodes, n);
        let st = self.state.read();
        (st.segments.clone(), st.n)
    }

    fn cached(&self, key: &str) -> Option<(usize, Cached)> {
        let mut c = self.cache.lock();
        let i = c.iter().position(|(k, _, _)| k == key)?;
        let e = c.remove(i)?;
        let out = (e.1, e.2.clone());
        c.push_back(e);
        Some(out)
    }

    fn peek(&self, key: &str) -> Option<(usize, Cached)> {
        let c = self.cache.lock();
        c.iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, d, r)| (*d, r.clone()))
    }

    fn remember(&self, key: &str, done: usize, result: Cached) {
        let mut c = self.cache.lock();
        if let Some(i) = c.iter().position(|(k, _, _)| k == key) {
            let old = c.remove(i).expect("present");
            if old.1 >= done {
                c.push_back(old);
                return;
            }
        }
        c.push_back((key.to_owned(), done, result));
        // bounded by count and by bytes; the newest two stay (typing refines the previous query)
        while c.len() > 2
            && (c.len() > CACHE_PATTERNS
                || c.iter().map(|(_, _, r)| r.bytes()).sum::<usize>() > CACHE_BYTES)
        {
            c.pop_front();
        }
    }

    /// Ids of names (of the segments not done yet) for which `pred` holds, in order.
    fn scan(
        segments: &[Arc<Segment>],
        nodes: &Nodes,
        done: usize,
        pred: &(dyn Fn(&[u8]) -> bool + Sync),
    ) -> Vec<NodeId> {
        let parts: Vec<Vec<NodeId>> = segments
            .par_iter()
            .filter(|s| s.stop as usize > done)
            .map(|s| {
                let mut out = Vec::new();
                let Some(run) = s.names(nodes) else {
                    return out;
                };
                s.each(run, |k, name| {
                    let id = s.first + k as NodeId;
                    if id as usize >= done && pred(name) {
                        out.push(id);
                    }
                });
                out
            })
            .collect();
        parts.concat()
    }

    fn cached_scan(
        &self,
        nodes: &Nodes,
        key: &str,
        n: usize,
        pred: &(dyn Fn(&[u8]) -> bool + Sync),
    ) -> Vec<NodeId> {
        let n = n.min(nodes.len());
        let (segments, covered) = self.snapshot(nodes, n);
        let (done, found) = match self.cached(key) {
            Some((d, Cached::Ids(v))) => (d, v),
            _ => (0, Arc::new(Vec::new())),
        };
        let found = if done < n {
            let mut all = (*found).clone();
            all.extend(Self::scan(&segments, nodes, done, pred));
            let all = Arc::new(all);
            self.remember(key, covered, Cached::Ids(all.clone()));
            all
        } else {
            found
        };
        let end = found.partition_point(|&i| (i as usize) < n);
        found[..end].to_vec()
    }

    /// Sorted ids (< `n`) of nodes whose name matches the normalised glob `pattern`.
    pub fn match_glob(&self, nodes: &Nodes, pattern: &str, n: usize) -> Vec<NodeId> {
        let glob = Glob::new(pattern);
        self.cached_scan(nodes, pattern, n, &|name| glob.matches(name))
    }

    /// Sorted ids (< `n`) of nodes whose name satisfies `pred` (the palette's searches; `pred` sees
    /// the raw name bytes). Results are cached under `key` like [`NameIndex::match_glob`]: the
    /// same key must always mean the same predicate.
    pub fn search(
        &self,
        nodes: &Nodes,
        key: &str,
        pred: &(dyn Fn(&[u8]) -> bool + Sync),
        n: usize,
    ) -> Vec<NodeId> {
        let ckey = format!("\0re:{key}");
        self.cached_scan(nodes, &ckey, n, pred)
    }

    /// Ids (< `n`) whose name, ASCII-case-insensitively, equals / starts with / contains `query` /
    /// holds its letters in order (with `scattered`, two letters or more): four disjoint sorted
    /// tiers (palette). While the user types on, the hits of the previous, shorter query are a
    /// superset, so only those names are searched. `query` must be ASCII.
    pub fn ascii_tiers(
        &self,
        nodes: &Nodes,
        query: &str,
        n: usize,
        scattered: bool,
    ) -> [Vec<NodeId>; 4] {
        let n = n.min(nodes.len());
        let q: Vec<u8> = query.bytes().map(lower).collect();
        let (segments, covered) = self.snapshot(nodes, n);
        let key = tier_key(&q, scattered);
        let tiers = match self.cached(&key) {
            Some((d, Cached::Tiers(t))) if d >= n => t,
            cached => {
                let t = self
                    .refined(nodes, &q, scattered, &segments, covered)
                    .unwrap_or_else(|| {
                        let prev = match cached {
                            Some((d, Cached::Tiers(t))) => Some((d, t)),
                            _ => None,
                        };
                        scan_tiers(nodes, &q, scattered, prev, &segments)
                    });
                let t = Arc::new(t);
                self.remember(&key, covered, Cached::Tiers(t.clone()));
                t
            }
        };
        tiers.each_ref().map(|t| {
            let end = t.partition_point(|&i| (i as usize) < n);
            t[..end].to_vec()
        })
    }

    /// Typing on: search only the hits of the longest shorter query already answered.
    fn refined(
        &self,
        nodes: &Nodes,
        q: &[u8],
        scattered: bool,
        segments: &[Arc<Segment>],
        covered: usize,
    ) -> Option<[Vec<NodeId>; 4]> {
        for cut in (1..q.len()).rev() {
            let Some((done, Cached::Tiers(prev))) = self.peek(&tier_key(&q[..cut], scattered))
            else {
                continue;
            };
            if done < covered {
                continue;
            }
            let mut cand: Vec<NodeId> = prev
                .iter()
                .flatten()
                .copied()
                .filter(|&i| (i as usize) < covered)
                .collect();
            cand.sort_unstable();
            let mut out: [Vec<NodeId>; 4] = Default::default();
            let mut k = 0;
            for &id in &cand {
                while k + 1 < segments.len() && segments[k + 1].first <= id {
                    k += 1;
                }
                let s = &segments[k];
                let run = s.names(nodes)?;
                if let Some(t) = tier_of(s.name(run, (id - s.first) as usize), q, scattered) {
                    out[t].push(id);
                }
            }
            return Some(out);
        }
        None
    }
}

fn tier_key(q: &[u8], scattered: bool) -> String {
    format!("\0tiers:{}:{}", scattered as u8, String::from_utf8_lossy(q))
}

/// Which tier a name falls in for the lower-case ASCII query `q`.
#[inline]
fn tier_of(name: &[u8], q: &[u8], scattered: bool) -> Option<usize> {
    let k = q.len();
    if k == 0 {
        return None;
    }
    if name.len() >= k {
        if name[..k].iter().zip(q).all(|(a, b)| lower(*a) == *b) {
            return Some(if name.len() == k { 0 } else { 1 });
        }
        if contains_lower(&name[1..], q) {
            return Some(2);
        }
    }
    if !scattered || k < 2 {
        return None;
    }
    let mut j = 0;
    for &b in name {
        if lower(b) == q[j] {
            j += 1;
            if j == k {
                return Some(3);
            }
        }
    }
    None
}

fn scan_tiers(
    nodes: &Nodes,
    q: &[u8],
    scattered: bool,
    prev: Option<(usize, Arc<[Vec<NodeId>; 4]>)>,
    segments: &[Arc<Segment>],
) -> [Vec<NodeId>; 4] {
    let done = prev.as_ref().map_or(0, |p| p.0);
    let need = signature(q);
    let parts: Vec<[Vec<NodeId>; 4]> = segments
        .par_iter()
        .filter(|s| s.stop as usize > done)
        .map(|s| {
            let mut out: [Vec<NodeId>; 4] = Default::default();
            let Some(run) = s.names(nodes) else {
                return out;
            };
            let sigs = s.sigs(run);
            for (k, &sig) in sigs.iter().enumerate() {
                let id = s.first + k as NodeId;
                if (id as usize) < done || sig & need != need {
                    continue;
                }
                if let Some(t) = tier_of(s.name(run, k), q, scattered) {
                    out[t].push(id);
                }
            }
            out
        })
        .collect();
    let mut out: [Vec<NodeId>; 4] = match prev {
        Some((_, p)) => (*p).clone(),
        None => Default::default(),
    };
    for p in parts {
        for (o, t) in out.iter_mut().zip(p) {
            o.extend(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let g = |p: &str, n: &str| Glob::new(p).matches(n.as_bytes());
        assert!(g("*.txt", "A.TXT"));
        assert!(!g("*.txt", "a.txt.bak"));
        assert!(g("?", "é"));
        assert!(!g("??", "é"));
        assert!(g("中?", "中é"));
        assert!(g("ä*", "Äbc"));
        assert!(g("*a*b", "xxaxxb"));
        assert!(!g("*a*b", "xxaxxbc"));
        assert!(g("a?c*", "abcdef"));
        assert!(g("*", ""));
        assert!(g("*o*", "photo.jpg"));
    }

    #[test]
    fn tiers() {
        assert_eq!(tier_of(b"F12", b"f12", true), Some(0));
        assert_eq!(tier_of(b"f123.txt", b"f12", true), Some(1));
        assert_eq!(tier_of(b"xf12", b"f12", true), Some(2));
        assert_eq!(tier_of(b"xf1x2", b"f12", true), Some(3));
        assert_eq!(tier_of(b"xf1x2", b"f12", false), None);
    }
}
