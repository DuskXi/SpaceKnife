//! The content stages (docs/dupes.md §2.4–§2.5): S2 reads the head and the tail (and two middle
//! blocks from 64 MiB on), S3 hashes whole files with BLAKE3 in steps that stop at checkpoints.
//!
//! A member is one file being read: its handle (handed from S2 to S3, closed after the lease, on
//! suspend, or when it is done), its hasher (already fed the head block, so S3 goes on at 4 KiB),
//! its offset and the stamps taken on the handle when it was opened. Every reopen checks identity,
//! size and both modification times against those stamps; a digest is only given out when exactly
//! `size` bytes were read, the next read hit EOF, and the stamps on the same handle are unchanged.
//!
//! One `Reader` serves one disk queue; batches on different threads work on different members
//! (each call takes its members out of the map and puts them back), so no lock is held during IO.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::codes;
use super::open::{self, BUFFERED_ATTRS, Handle, Meta, Stamp, read_full_at};

pub const BLOCK: u64 = 4096;
pub const PROBE_PARAMS: u16 = 1; // head 4 KiB + tail 4 KiB, two middle blocks from 64 MiB
pub const MID_FROM: u64 = 64 << 20;
const MIN_WHOLE: u64 = 2 * BLOCK; // head and tail overlap below this: read it all

pub fn align_down(x: u64) -> u64 {
    x & !(BLOCK - 1)
}

pub fn align_up(x: u64) -> u64 {
    (x + BLOCK - 1) & !(BLOCK - 1)
}

/// The byte ranges S2 reads of a file of `size` bytes (head, middle blocks, tail; may overlap).
pub fn probe_ranges(size: u64) -> Vec<(u64, u64)> {
    let mut r = vec![(0, size.min(BLOCK))];
    if size >= MID_FROM {
        for k in 1..=2u64 {
            let at = align_down(size / 3 * k);
            r.push((at, (at + BLOCK).min(size)));
        }
    }
    let t = align_down(size.saturating_sub(1));
    r.push((t, size));
    r
}

/// The quick digest of a probe: BLAKE3(size ‖ params ‖ each range), first 8 bytes.
pub fn quick_of(size: u64, parts: &[&[u8]]) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    h.update(&PROBE_PARAMS.to_le_bytes());
    for p in parts {
        h.update(p);
    }
    u64::from_le_bytes(h.finalize().as_bytes()[..8].try_into().unwrap())
}

struct ABuf {
    ptr: *mut u8,
    len: usize,
}

impl ABuf {
    fn new(len: usize) -> ABuf {
        let layout = Layout::from_size_align(len, BLOCK as usize).unwrap();
        // SAFETY: non-zero size, valid alignment.
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory");
        ABuf { ptr, len }
    }

    fn slice(&mut self, n: usize) -> &mut [u8] {
        assert!(n <= self.len);
        // SAFETY: `ptr` holds `len` initialised bytes we own.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, n) }
    }
}

impl Drop for ABuf {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with this layout.
        unsafe { dealloc(self.ptr, Layout::from_size_align(self.len, BLOCK as usize).unwrap()) }
    }
}

thread_local! {
    static BUF: RefCell<Option<ABuf>> = const { RefCell::new(None) };
}

/// A page-aligned buffer of at least `n` bytes for this thread (kept between calls).
fn with_buf<R>(n: usize, f: impl FnOnce(&mut ABuf) -> R) -> R {
    BUF.with_borrow_mut(|b| {
        if b.as_ref().is_none_or(|b| b.len < n) {
            *b = Some(ABuf::new(align_up(n as u64) as usize));
        }
        f(b.as_mut().unwrap())
    })
}

#[derive(Clone, Debug)]
pub struct Opts {
    pub unbuffered: bool,
    pub keep_read: bool,
    pub small_whole: u64,
    pub lease: Duration,
    pub chunk: usize,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            unbuffered: true,
            keep_read: false,
            small_whole: 128 << 10,
            lease: Duration::from_secs(2),
            chunk: 1 << 20,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProbeIn {
    pub key: u64,
    pub path: String,
    pub dev: u64,
    pub fid_lo: u64,
    pub fid_hi: u64,
    pub size: u64,
    pub attrs: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ProbeOut {
    pub key: u64,
    pub reason: u8,
    pub quick: u64,
    pub full: Option<[u8; 32]>,
    pub mtime: i64,
    pub change: i64,
    pub atime_unkept: bool,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct StepOut {
    pub key: u64,
    pub reason: u8,
    pub offset: u64,
    /// 0 not at its target yet, 1 at a checkpoint (`interim` set), 2 done (`fin` set)
    pub at: u8,
    pub interim: [u8; 16],
    pub fin: [u8; 32],
    pub bytes: u64,
    pub reopens: u32,
}

struct Member {
    path: String,
    size: u64,
    attrs: u32,
    stamp: Stamp,
    handle: Option<Handle>,
    hasher: blake3::Hasher,
    offset: u64,
    opened: Instant,
}

#[derive(Default)]
pub struct Counters {
    pub data_opens: AtomicU64,
    pub reopens: AtomicU64,
    pub bytes_probe: AtomicU64,
    pub bytes_full: AtomicU64,
    pub atime_unkept: AtomicU64,
    pub changed: AtomicU64,
}

pub struct Reader {
    pub opts: Opts,
    members: Mutex<HashMap<u64, Member>>,
    open: Arc<AtomicUsize>,
    pub counters: Counters,
}

/// A test seam: called between two chunks of a step (tests change the file there).
pub type Hook = dyn Fn(u64, u64) + Send + Sync;

impl Reader {
    pub fn new(opts: Opts) -> Reader {
        Reader {
            opts,
            members: Mutex::new(HashMap::new()),
            open: Arc::new(AtomicUsize::new(0)),
            counters: Counters::default(),
        }
    }

    pub fn open_handles(&self) -> usize {
        self.open.load(Ordering::Relaxed)
    }

    pub fn members(&self) -> usize {
        self.members.lock().unwrap().len()
    }

    fn open(&self, path: &str, attrs: u32) -> Result<Handle, u8> {
        let unbuf = self.opts.unbuffered && attrs & BUFFERED_ATTRS == 0;
        let h = open::open_data(path, unbuf, self.opts.keep_read, &self.open)?;
        self.counters.data_opens.fetch_add(1, Ordering::Relaxed);
        if h.atime_unkept {
            self.counters.atime_unkept.fetch_add(1, Ordering::Relaxed);
        }
        Ok(h)
    }

    fn changed(&self, r: &mut u8) {
        *r = codes::CHANGED;
        self.counters.changed.fetch_add(1, Ordering::Relaxed);
    }

    /// S2 for each item: one data open, head and tail (and middle blocks), stamps before and
    /// after. Files below `small_whole` (or 8 KiB) are read whole and get their full digest here.
    /// The others stay as members (hasher fed the head block); with `hand_off` their handle stays
    /// open for S3.
    pub fn probe(&self, items: &[ProbeIn], hand_off: bool) -> Vec<ProbeOut> {
        let mut outs = Vec::with_capacity(items.len());
        let mut keep: Vec<(u64, Member)> = Vec::new();
        for it in items {
            let (out, member) = self.probe_one(it, hand_off);
            if let Some(m) = member {
                keep.push((it.key, m));
            }
            outs.push(out);
        }
        if !keep.is_empty() {
            let mut map = self.members.lock().unwrap();
            for (k, m) in keep {
                map.insert(k, m);
            }
        }
        outs
    }

    fn probe_one(&self, it: &ProbeIn, hand_off: bool) -> (ProbeOut, Option<Member>) {
        let mut out = ProbeOut {
            key: it.key,
            ..ProbeOut::default()
        };
        let mut h = match self.open(&it.path, it.attrs) {
            Ok(h) => h,
            Err(r) => {
                out.reason = r;
                return (out, None);
            }
        };
        out.atime_unkept = h.atime_unkept;
        let m0 = match Meta::of_file(&h.file) {
            Ok(m) => m,
            Err(_) => {
                out.reason = codes::READ_ERROR;
                return (out, None);
            }
        };
        if m0.kind_reason != codes::OK {
            out.reason = m0.kind_reason;
            return (out, None);
        }
        if !m0.stamp.same_id(it.dev, it.fid_lo, it.fid_hi) || m0.stamp.size != it.size {
            self.changed(&mut out.reason);
            return (out, None);
        }
        out.mtime = m0.stamp.mtime;
        out.change = m0.stamp.change;
        let size = it.size;
        let whole = size <= self.opts.small_whole.max(MIN_WHOLE);
        let res = if whole {
            self.read_whole(&mut h, size)
        } else {
            self.read_probe(&mut h, size)
        };
        let (quick, full, head, bytes) = match res {
            Ok(v) => v,
            Err(r) => {
                if r == codes::CHANGED {
                    self.changed(&mut out.reason);
                } else {
                    out.reason = r;
                }
                return (out, None);
            }
        };
        out.bytes = bytes;
        self.counters.bytes_probe.fetch_add(bytes, Ordering::Relaxed);
        match Meta::of_file(&h.file) {
            Ok(m1) if m1.stamp == m0.stamp => {}
            Ok(_) => {
                self.changed(&mut out.reason);
                return (out, None);
            }
            Err(_) => {
                out.reason = codes::READ_ERROR;
                return (out, None);
            }
        }
        out.quick = quick;
        if whole {
            out.full = full;
            return (out, None);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&head);
        let member = Member {
            path: it.path.clone(),
            size,
            attrs: it.attrs,
            stamp: m0.stamp,
            handle: if hand_off { Some(h) } else { None },
            hasher,
            offset: BLOCK,
            opened: Instant::now(),
        };
        (out, Some(member))
    }

    /// The whole of a small file: (quick, full digest, -, bytes read).
    #[allow(clippy::type_complexity)]
    fn read_whole(&self, h: &mut Handle, size: u64) -> Result<(u64, Option<[u8; 32]>, Vec<u8>, u64), u8> {
        let req = align_up(size + 1) as usize; // one block more than the file: EOF must show
        with_buf(req, |buf| {
            let b = buf.slice(req);
            let n = read_full_at(h, b, 0).map_err(|e| open::io_reason(&e))? as u64;
            if n != size {
                return Err(codes::CHANGED);
            }
            let data = &b[..n as usize];
            let parts: Vec<&[u8]> = probe_ranges(size)
                .iter()
                .map(|&(a, z)| &data[a as usize..z as usize])
                .collect();
            let quick = quick_of(size, &parts);
            let full = *blake3::hash(data).as_bytes();
            Ok((quick, Some(full), Vec::new(), n))
        })
    }

    /// Head, middle blocks and tail of a larger file: (quick, -, head block, bytes read).
    #[allow(clippy::type_complexity)]
    fn read_probe(&self, h: &mut Handle, size: u64) -> Result<(u64, Option<[u8; 32]>, Vec<u8>, u64), u8> {
        let ranges = probe_ranges(size);
        let mut blocks: Vec<Vec<u8>> = Vec::with_capacity(ranges.len());
        let mut bytes = 0u64;
        with_buf(BLOCK as usize, |buf| {
            for &(a, z) in &ranges {
                let b = buf.slice(BLOCK as usize);
                let n = read_full_at(h, b, a).map_err(|e| open::io_reason(&e))? as u64;
                if n != z - a {
                    return Err(codes::CHANGED); // shorter (or, at the tail, longer) than its size
                }
                bytes += n;
                blocks.push(b[..n as usize].to_vec());
            }
            Ok(())
        })?;
        let parts: Vec<&[u8]> = blocks.iter().map(|b| b.as_slice()).collect();
        let quick = quick_of(size, &parts);
        Ok((quick, None, blocks.swap_remove(0), bytes))
    }

    /// S3: advances each member towards `until[i]` (a checkpoint, or its size) in chunks, round
    /// robin, until every member is there, `max_bytes` were read or `max_s` passed. A read never
    /// crosses the member's target.
    pub fn step(&self, keys: &[u64], until: &[u64], max_bytes: u64, max_s: f64, hook: Option<&Hook>) -> Vec<StepOut> {
        let t0 = Instant::now();
        let deadline = Duration::from_secs_f64(max_s.max(0.0));
        let mut work: Vec<Option<Member>> = {
            let mut map = self.members.lock().unwrap();
            keys.iter().map(|k| map.remove(k)).collect()
        };
        let mut outs: Vec<StepOut> = keys
            .iter()
            .map(|&k| StepOut {
                key: k,
                ..StepOut::default()
            })
            .collect();
        for (i, w) in work.iter().enumerate() {
            if w.is_none() {
                outs[i].reason = codes::UNKNOWN_KEY;
            }
        }
        let mut budget = max_bytes;
        let chunk = self.opts.chunk.max(BLOCK as usize) as u64;
        with_buf((chunk + BLOCK) as usize, |buf| {
            'rounds: loop {
                let mut progressed = false;
                for i in 0..work.len() {
                    let Some(m) = work[i].as_mut() else { continue };
                    if outs[i].reason != codes::OK {
                        continue;
                    }
                    let target = until[i].min(m.size);
                    if m.offset >= target {
                        continue;
                    }
                    if budget == 0 || t0.elapsed() >= deadline {
                        break 'rounds;
                    }
                    if m.handle.is_none() {
                        match self.reopen(m) {
                            Ok(()) => outs[i].reopens += 1,
                            Err(r) => {
                                outs[i].reason = r;
                                continue;
                            }
                        }
                    }
                    let len = chunk.min(target - m.offset);
                    // the read that reaches the end asks for more than is left: EOF must show
                    let last = m.offset + len == m.size;
                    let req = if last { align_up(len + 1) } else { len };
                    let h = m.handle.as_mut().unwrap();
                    let b = buf.slice(req as usize);
                    let n = match read_full_at(h, b, m.offset) {
                        Ok(n) => n as u64,
                        Err(e) => {
                            outs[i].reason = open::io_reason(&e);
                            continue;
                        }
                    };
                    if n != len {
                        self.changed(&mut outs[i].reason); // shorter or longer than its size
                        continue;
                    }
                    m.hasher.update(&b[..n as usize]);
                    m.offset += n;
                    outs[i].bytes += n;
                    budget = budget.saturating_sub(n);
                    progressed = true;
                    if let Some(f) = hook {
                        f(keys[i], m.offset);
                    }
                    if m.offset == m.size {
                        match Meta::of_file(&h.file) {
                            Ok(m1) if m1.stamp == m.stamp => {}
                            Ok(_) => {
                                self.changed(&mut outs[i].reason);
                                continue;
                            }
                            Err(_) => {
                                outs[i].reason = codes::READ_ERROR;
                                continue;
                            }
                        }
                    }
                    if m.opened.elapsed() >= self.opts.lease {
                        m.handle = None; // the lease ran out: reopened (and checked) next time
                    }
                }
                if !progressed {
                    break;
                }
            }
        });
        let mut back: Vec<(u64, Member)> = Vec::new();
        let mut total = 0u64;
        for (i, w) in work.into_iter().enumerate() {
            let o = &mut outs[i];
            total += o.bytes;
            let Some(m) = w else { continue };
            o.offset = m.offset;
            if o.reason != codes::OK {
                continue; // dropped, its handle closes
            }
            let target = until[i].min(m.size);
            if m.offset == m.size {
                o.at = 2;
                o.fin = *m.hasher.finalize().as_bytes();
                continue;
            }
            if m.offset == target {
                o.at = 1;
                o.interim = m.hasher.finalize().as_bytes()[..16].try_into().unwrap();
            }
            back.push((keys[i], m));
        }
        self.counters.bytes_full.fetch_add(total, Ordering::Relaxed);
        if !back.is_empty() {
            let mut map = self.members.lock().unwrap();
            for (k, m) in back {
                map.insert(k, m);
            }
        }
        outs
    }

    fn reopen(&self, m: &mut Member) -> Result<(), u8> {
        let h = self.open(&m.path, m.attrs)?;
        self.counters.reopens.fetch_add(1, Ordering::Relaxed);
        let meta = Meta::of_file(&h.file).map_err(|_| codes::READ_ERROR)?;
        if meta.kind_reason != codes::OK {
            return Err(meta.kind_reason);
        }
        if meta.stamp != m.stamp {
            self.counters.changed.fetch_add(1, Ordering::Relaxed);
            return Err(codes::CHANGED);
        }
        m.handle = Some(h);
        m.opened = Instant::now();
        Ok(())
    }

    /// Closes the handles of `keys` (all members with None); hashers and offsets stay.
    pub fn suspend(&self, keys: Option<&[u64]>) {
        let mut map = self.members.lock().unwrap();
        match keys {
            None => map.values_mut().for_each(|m| m.handle = None),
            Some(ks) => {
                for k in ks {
                    if let Some(m) = map.get_mut(k) {
                        m.handle = None;
                    }
                }
            }
        }
    }

    /// Forgets members (all with None).
    pub fn drop_members(&self, keys: Option<&[u64]>) {
        let mut map = self.members.lock().unwrap();
        match keys {
            None => map.clear(),
            Some(ks) => {
                for k in ks {
                    map.remove(k);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("sk-dupes-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        fn file(&self, name: &str, data: &[u8]) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, data).unwrap();
            p
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn item(key: u64, p: &Path) -> ProbeIn {
        let path = p.to_str().unwrap().to_string();
        let m = open::meta_of_path(&path, false).unwrap();
        ProbeIn {
            key,
            path,
            dev: m.stamp.dev,
            fid_lo: m.stamp.fid_lo,
            fid_hi: m.stamp.fid_hi,
            size: m.stamp.size,
            attrs: m.attrs,
        }
    }

    fn data(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u32).wrapping_mul(2654435761).to_le_bytes()[1] ^ seed).collect()
    }

    fn opts() -> Opts {
        Opts {
            small_whole: 0,
            ..Opts::default()
        }
    }

    /// Steps one member to the end through the checkpoints; returns interims and the digest.
    fn run(r: &Reader, key: u64, size: u64, cps: &[u64]) -> (Vec<[u8; 16]>, [u8; 32]) {
        let mut interims = Vec::new();
        for &cp in cps.iter().chain(std::iter::once(&size)) {
            loop {
                let o = r.step(&[key], &[cp], 3 << 20, 5.0, None).remove(0);
                assert_eq!(o.reason, codes::OK);
                if o.at == 2 {
                    return (interims, o.fin);
                }
                if o.at == 1 {
                    interims.push(o.interim);
                    break;
                }
            }
        }
        unreachable!()
    }

    #[test]
    fn checkpoints_equal_one_shot_hashes() {
        let d = Dir::new("cp");
        let size = (5 << 20) + 123;
        let bytes = data(size, 1);
        let p = d.file("a.bin", &bytes);
        let r = Reader::new(opts());
        let o = r.probe(&[item(1, &p)], true).remove(0);
        assert_eq!(o.reason, codes::OK);
        assert_eq!(o.bytes, BLOCK + (size as u64 - align_down(size as u64 - 1)));
        let (interims, fin) = run(&r, 1, size as u64, &[1 << 20, 4 << 20]);
        assert_eq!(&interims[0], &blake3::hash(&bytes[..1 << 20]).as_bytes()[..16]);
        assert_eq!(&interims[1], &blake3::hash(&bytes[..4 << 20]).as_bytes()[..16]);
        assert_eq!(&fin, blake3::hash(&bytes).as_bytes());
        // the head block was not read again: S3 read size − 4 KiB
        assert_eq!(r.counters.bytes_full.load(Ordering::Relaxed), size as u64 - BLOCK);
        assert_eq!(r.open_handles(), 0);
        assert_eq!(r.members(), 0);
    }

    #[test]
    fn aligned_sizes_check_eof_too() {
        let d = Dir::new("al");
        let size = 3 << 20;
        let bytes = data(size, 2);
        let p = d.file("a.bin", &bytes);
        let r = Reader::new(opts());
        r.probe(&[item(1, &p)], true);
        let (_, fin) = run(&r, 1, size as u64, &[1 << 20]);
        assert_eq!(&fin, blake3::hash(&bytes).as_bytes());
    }

    #[test]
    fn quick_digests_tell_tails_apart_but_not_middles() {
        let d = Dir::new("q");
        let size = 1 << 20;
        let a = data(size, 3);
        let mut tail = a.clone();
        *tail.last_mut().unwrap() ^= 1;
        let mut mid = a.clone();
        mid[size / 2] ^= 1;
        let pa = d.file("a", &a);
        let pt = d.file("t", &tail);
        let pm = d.file("m", &mid);
        let r = Reader::new(opts());
        let o = r.probe(&[item(1, &pa), item(2, &pt), item(3, &pm)], false);
        assert!(o.iter().all(|x| x.reason == codes::OK));
        assert_ne!(o[0].quick, o[1].quick);
        assert_eq!(o[0].quick, o[2].quick, "the middle is not sampled below 64 MiB");
        assert_eq!(r.open_handles(), 0, "no hand-off: closed after the probe");
        // S3 then reopens (checked) and separates them at the first checkpoint
        let s = r.step(&[1, 3], &[1 << 20, 1 << 20], 8 << 20, 5.0, None);
        assert!(s.iter().all(|x| x.reason == codes::OK && x.at == 2));
        assert_ne!(s[0].fin, s[1].fin);
        assert_eq!(s[0].reopens, 1);
    }

    #[test]
    fn small_files_are_read_whole() {
        let d = Dir::new("sm");
        let bytes = data(5000, 4);
        let p = d.file("s", &bytes);
        let empty = d.file("e", &[]);
        let r = Reader::new(Opts::default());
        let o = r.probe(&[item(1, &p), item(2, &empty)], true);
        assert_eq!(o[0].full.unwrap(), *blake3::hash(&bytes).as_bytes());
        assert_eq!(o[0].bytes, 5000);
        assert_eq!(o[1].full.unwrap(), *blake3::hash(&[]).as_bytes());
        assert_eq!(r.members(), 0);
        assert_eq!(r.open_handles(), 0);
    }

    #[test]
    fn a_file_that_changes_while_read_is_dropped() {
        let d = Dir::new("ch");
        let size = 3 << 20;
        let p = d.file("a.bin", &data(size, 5));
        let r = Reader::new(opts());
        r.probe(&[item(7, &p)], true);
        let pp = p.clone();
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = move |_k: u64, _off: u64| {
            if !once.swap(true, Ordering::Relaxed) {
                let mut f = std::fs::OpenOptions::new().append(true).open(&pp).unwrap();
                f.write_all(b"more").unwrap();
            }
        };
        let o = r.step(&[7], &[size as u64], 64 << 20, 5.0, Some(&hook)).remove(0);
        assert_eq!(o.reason, codes::CHANGED);
        assert_eq!(r.members(), 0);
    }

    #[test]
    fn a_file_rewritten_in_place_is_dropped() {
        let d = Dir::new("rw");
        let size = 2 << 20;
        let p = d.file("a.bin", &data(size, 6));
        let r = Reader::new(opts());
        r.probe(&[item(7, &p)], true);
        let pp = p.clone();
        let once = std::sync::atomic::AtomicBool::new(false);
        let hook = move |_k: u64, _off: u64| {
            if !once.swap(true, Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(20));
                let mut f = std::fs::OpenOptions::new().write(true).open(&pp).unwrap();
                f.write_all(b"xx").unwrap();
            }
        };
        let o = r.step(&[7], &[size as u64], 64 << 20, 5.0, Some(&hook)).remove(0);
        assert_eq!(o.reason, codes::CHANGED);
    }

    #[test]
    fn a_swapped_file_is_not_read_on() {
        let d = Dir::new("sw");
        let size = 2 << 20;
        let p = d.file("a.bin", &data(size, 7));
        let r = Reader::new(opts());
        let it = item(1, &p);
        r.probe(std::slice::from_ref(&it), false);
        // another file of the same size takes the name between S2 and S3
        std::fs::remove_file(&p).unwrap();
        d.file("a.bin", &data(size, 8));
        let o = r.step(&[1], &[size as u64], 64 << 20, 5.0, None).remove(0);
        assert_eq!(o.reason, codes::CHANGED);
        // and an identity that no longer matches at S2 is refused too
        let mut stale = it.clone();
        stale.fid_lo ^= 1;
        assert_eq!(r.probe(&[stale], false)[0].reason, codes::CHANGED);
    }

    #[test]
    fn suspend_closes_and_reopen_checks() {
        let d = Dir::new("su");
        let size = 3 << 20;
        let bytes = data(size, 9);
        let p = d.file("a.bin", &bytes);
        let r = Reader::new(opts());
        r.probe(&[item(1, &p)], true);
        assert_eq!(r.open_handles(), 1);
        let o = r.step(&[1], &[1 << 20], 64 << 20, 5.0, None).remove(0);
        assert_eq!(o.at, 1);
        r.suspend(None);
        assert_eq!(r.open_handles(), 0);
        let o = r.step(&[1], &[size as u64], 64 << 20, 5.0, None).remove(0);
        assert_eq!((o.at, o.reopens), (2, 1));
        assert_eq!(&o.fin, blake3::hash(&bytes).as_bytes());
        assert_eq!(r.counters.bytes_full.load(Ordering::Relaxed), size as u64 - BLOCK, "nothing read twice");
    }

    #[test]
    fn leases_close_handles() {
        let d = Dir::new("le");
        let size = 4 << 20;
        let bytes = data(size, 10);
        let p = d.file("a.bin", &bytes);
        let r = Reader::new(Opts {
            lease: Duration::from_millis(0),
            ..opts()
        });
        r.probe(&[item(1, &p)], true);
        let o = r.step(&[1], &[size as u64], 64 << 20, 5.0, None).remove(0);
        assert_eq!(o.at, 2);
        assert!(o.reopens >= 2, "reopened after every chunk: {}", o.reopens);
        assert_eq!(&o.fin, blake3::hash(&bytes).as_bytes());
    }

    #[test]
    fn budgets_stop_a_step_early() {
        let d = Dir::new("bu");
        let size = 8 << 20;
        let p = d.file("a.bin", &data(size, 11));
        let r = Reader::new(opts());
        r.probe(&[item(1, &p)], true);
        let o = r.step(&[1], &[size as u64], 2 << 20, 5.0, None).remove(0);
        assert_eq!(o.at, 0);
        assert!(o.bytes <= 2 << 20);
        assert_eq!(r.members(), 1);
        let o = r.step(&[9], &[1], 1, 5.0, None).remove(0);
        assert_eq!(o.reason, codes::UNKNOWN_KEY);
        r.drop_members(None);
        assert_eq!(r.open_handles(), 0);
    }

    #[test]
    fn interim_digests_are_non_destructive() {
        let mut h = blake3::Hasher::new();
        h.update(b"abc");
        let a = h.finalize();
        h.update(b"def");
        assert_eq!(a, blake3::hash(b"abc"));
        assert_eq!(h.finalize(), blake3::hash(b"abcdef"));
    }

    #[test]
    fn probe_ranges_cover_head_middle_tail() {
        assert_eq!(probe_ranges(10_000), vec![(0, 4096), (8192, 10_000)]);
        assert_eq!(probe_ranges(8192), vec![(0, 4096), (4096, 8192)]);
        let big = 300 << 20;
        let r = probe_ranges(big);
        assert_eq!(r.len(), 4);
        assert_eq!(r[1].0 % BLOCK, 0);
        assert_eq!(r[3], (big - BLOCK, big));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_keep_the_access_time() {
        use std::os::unix::fs::MetadataExt;
        let d = Dir::new("at");
        let size = 2 << 20;
        let p = d.file("a.bin", &data(size, 12));
        // age the access time by 30 days
        let old = std::time::SystemTime::now() - Duration::from_secs(30 * 86400);
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_accessed(old)).unwrap();
        drop(f);
        let before = std::fs::metadata(&p).unwrap().atime();
        let r = Reader::new(opts());
        r.probe(&[item(1, &p)], true);
        r.step(&[1], &[size as u64], 64 << 20, 5.0, None);
        assert_eq!(std::fs::metadata(&p).unwrap().atime(), before);
    }
}
