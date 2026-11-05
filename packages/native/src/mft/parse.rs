//! Parallel $MFT parsing into the `MftTable` columns of docs/native.md 2.1.
//!
//! Three passes, all over immutable input:
//! 1. classify every record's header and collect the in-use extension records;
//! 2. parse the extension records (few) into owned accumulators;
//! 3. parse base records chunk by chunk (rayon), merge their extension records, emit rows.
//!
//! Extension records are tied to their base through the base reference in their header, which
//! also covers a non-resident `$ATTRIBUTE_LIST` (its content is outside the MFT).

use rayon::prelude::*;

use super::record::{
    Acc, Class, F_ADS, F_REPARSE, NS_DOS, REF_MASK, REPARSE_NAME_SURROGATE, Reparse, attr_flags,
    classify, fixup, u16_at, unix_secs, walk,
};

const CHUNK: usize = 8192; // records per parallel task (whole $MFT in memory)

#[derive(Default)]
pub struct Table {
    pub record: Vec<u64>,
    pub parent: Vec<u64>,
    pub name_off: Vec<u32>,
    pub name_len: Vec<u16>,
    pub kind: Vec<u8>,
    pub flags: Vec<u32>,
    pub size: Vec<i64>,
    pub alloc: Vec<i64>,
    pub ads_size: Vec<i64>,
    pub ads_alloc: Vec<i64>,
    pub mtime: Vec<i64>,
    pub ctime: Vec<i64>,
    pub atime: Vec<i64>,
    pub nlink: Vec<u16>,
    pub names: Vec<u8>,
    pub bad_records: u64,
}

impl Table {
    pub fn rows(&self) -> usize {
        self.record.len()
    }
}

#[derive(Clone, Copy)]
struct Ext {
    base: u64,     // base record number
    record: u64,   // this extension record
    base_seq: u16, // sequence number the base must still have
}

pub fn parse(data: &[u8], record_size: usize) -> Result<Table, String> {
    let n = data.len() / record_size;
    let rec = |i: usize| &data[i * record_size..(i + 1) * record_size];

    // pass 1: extension records, ordered by (base, record)
    let mut exts: Vec<Ext> = (0..n)
        .into_par_iter()
        .with_min_len(CHUNK)
        .filter_map(|i| match classify(rec(i)) {
            Class::Ext(base_ref) => Some(Ext {
                base: base_ref & REF_MASK,
                record: i as u64,
                base_seq: (base_ref >> 48) as u16,
            }),
            _ => None,
        })
        .collect();
    exts.par_sort_unstable_by_key(|e| (e.base, e.record));

    // pass 2: parse them
    let parsed: Vec<Option<Acc>> = exts
        .par_iter()
        .map_init(Vec::new, |buf, e| {
            let mut acc = Acc::default();
            (fixup(rec(e.record as usize), buf) && walk(buf, &mut acc)).then_some(acc)
        })
        .collect();
    let ext_bad = parsed.iter().filter(|p| p.is_none()).count() as u64;

    // pass 3: base records
    let chunks: Vec<Table> = (0..n.div_ceil(CHUNK))
        .into_par_iter()
        .map(|c| {
            let lo = c * CHUNK;
            let hi = (lo + CHUNK).min(n);
            let first_ext = exts.partition_point(|e| e.base < lo as u64);
            let mut chunk = Chunk::new(hi - lo);
            let mut cursor = first_ext;
            for i in lo..hi {
                while cursor < exts.len() && exts[cursor].base < i as u64 {
                    cursor += 1;
                }
                let mut end = cursor;
                while end < exts.len() && exts[end].base == i as u64 {
                    end += 1;
                }
                chunk.record(i, rec(i), &exts[cursor..end], &parsed[cursor..end]);
                cursor = end;
            }
            chunk.out
        })
        .collect();
    concat(chunks, ext_bad)
}

/// A base record with an `$ATTRIBUTE_LIST`, parsed, kept for [`Stream::finish`] (its extension
/// records may come in any window).
struct Deferred {
    record: u64,
    seq: u16,
    is_dir: bool,
    acc: Acc,
}

/// What one window of records parses into (see [`Stream`]).
pub struct Part {
    table: Table,
    deferred: Vec<Deferred>,
    exts: Vec<Ext>,
    parsed: Vec<Option<Acc>>,
    records: u64,
}

/// Parse the records of one window (`first`: record number of its first record) on this thread,
/// into `table` (a table [`Stream::push`] gave back, so its memory is reused; or a new one): rows
/// of the base records without `$ATTRIBUTE_LIST`, the raw bases with one (kept for
/// [`Stream::finish`]) and every extension record, parsed. A pure function of its input: windows
/// can be parsed in any order and on any thread; [`Stream::push`] puts them back in record order.
pub fn parse_window(window: &[u8], first: u64, record_size: usize, table: Table) -> Part {
    let rs = record_size;
    let n = window.len() / rs;
    let mut chunk = Chunk::reuse(table, n);
    let (mut deferred, mut exts, mut parsed) = (Vec::new(), Vec::new(), Vec::new());
    for i in 0..n {
        let raw = &window[i * rs..(i + 1) * rs];
        let number = first + i as u64;
        match classify(raw) {
            Class::Unused => {}
            Class::Bad => chunk.out.bad_records += 1,
            Class::Ext(base_ref) => {
                let mut acc = Acc::default();
                let ok = fixup(raw, &mut chunk.buf) && walk(&chunk.buf, &mut acc);
                exts.push(Ext {
                    base: base_ref & REF_MASK,
                    record: number,
                    base_seq: (base_ref >> 48) as u16,
                });
                parsed.push(ok.then_some(acc));
            }
            Class::Base => {
                chunk.acc.reset();
                if !(fixup(raw, &mut chunk.buf) && walk(&chunk.buf, &mut chunk.acc)) {
                    chunk.out.bad_records += 1;
                } else if chunk.acc.has_list {
                    deferred.push(Deferred {
                        record: number,
                        seq: u16_at(&chunk.buf, 0x10),
                        is_dir: u16_at(&chunk.buf, 0x16) & 2 != 0,
                        acc: std::mem::take(&mut chunk.acc),
                    });
                } else {
                    let is_dir = u16_at(&chunk.buf, 0x16) & 2 != 0;
                    chunk.emit(number, is_dir);
                }
            }
        }
    }
    Part {
        table: chunk.out,
        deferred,
        exts,
        parsed,
        records: n as u64,
    }
}

/// The same rows as [`parse`], fed window by window: the whole $MFT is never in memory at once.
///
/// A base record without an `$ATTRIBUTE_LIST` cannot have extension records, so it is emitted at
/// once. The few with a list are kept raw (with every extension record, parsed) until
/// [`Stream::finish`], whose rows come after all others; rows are otherwise in record order.
/// Windows may be parsed concurrently ([`parse_window`]); they are pushed in record order and
/// their rows appended to the output columns right away, so the rows (and their order) do not
/// depend on the window size or on which window finished first, and no second copy of the table
/// is ever made.
pub struct Stream {
    record_size: usize,
    next: u64,               // record number of the next record pushed
    records: u64,            // records expected in all (sizes the output columns), 0: unknown
    out: Table,              // the rows so far, in order (names in `names`)
    names: Vec<Vec<u8>>,     // the name blob in pieces, one per window (moved in, never copied)
    names_len: u64,          // bytes of names pushed
    deferred: Vec<Deferred>, // in record order
    exts: Vec<Ext>,
    parsed: Vec<Option<Acc>>,
}

/// A finished stream: the columns (names empty) and the name blob in pieces.
pub struct Joined {
    pub table: Table,
    pub names: Vec<Vec<u8>>,
    pub names_len: usize,
}

impl Joined {
    /// The blob in one piece (copied, each piece freed as it goes).
    pub fn into_table(self) -> Table {
        let mut table = self.table;
        let mut blob = Vec::with_capacity(self.names_len);
        for piece in self.names {
            blob.extend_from_slice(&piece);
        }
        table.names = blob;
        table
    }

    /// Copy the blob into `dst` (`names_len` bytes), in parallel.
    pub fn copy_names(names: Vec<Vec<u8>>, dst: &mut [u8]) {
        let mut parts: Vec<(&mut [u8], Vec<u8>)> = Vec::with_capacity(names.len());
        let mut rest = dst;
        for piece in names {
            let (a, b) = rest.split_at_mut(piece.len());
            parts.push((a, piece));
            rest = b;
        }
        assert!(rest.is_empty());
        parts
            .into_par_iter()
            .for_each(|(d, piece)| d.copy_from_slice(&piece));
    }
}

impl Stream {
    pub fn new(record_size: usize) -> Self {
        Self::with_records(record_size, 0)
    }

    /// A stream of `records` records in all: once the first windows are in, the output columns
    /// are sized for the rows the rest of the $MFT will have at the same density (and grown the
    /// same way when a window brings more).
    pub fn with_records(record_size: usize, records: u64) -> Self {
        Stream {
            record_size,
            next: 0,
            records,
            out: Table::default(),
            names: Vec::new(),
            names_len: 0,
            deferred: Vec::new(),
            exts: Vec::new(),
            parsed: Vec::new(),
        }
    }

    /// Parse the next records (`window.len()` is a multiple of the record size).
    pub fn feed(&mut self, window: &[u8]) {
        let part = parse_window(window, self.next, self.record_size, Table::default());
        self.push(part);
    }

    /// Add the next window's [`Part`] (windows must come in record order); returns its table,
    /// emptied, for the next window's parse.
    pub fn push(&mut self, part: Part) -> Table {
        let mut t = part.table;
        self.next += part.records;
        self.append(&mut t);
        self.deferred.extend(part.deferred);
        self.exts.extend(part.exts);
        self.parsed.extend(part.parsed);
        t.clear();
        t
    }

    /// Append a table's rows; its names move over as one more piece of the blob.
    fn append(&mut self, t: &mut Table) {
        let rows = t.rows();
        let out = &mut self.out;
        out.bad_records += t.bad_records;
        let have = out.record.len() + rows;
        if have > out.record.capacity() {
            // room for the rows the rest of the $MFT will bring at the density seen so far
            // (2 % more), never less than an eighth more than now
            let seen = self.next.max(1) as u128;
            let projected = (have as u128 * self.records.max(self.next) as u128 / seen) as usize;
            let want = projected + projected / 50 + 1024;
            out.reserve_rows(want.max(have + have / 8) - out.record.len());
        }
        let base = self.names_len;
        self.names_len += t.names.len() as u64;
        let base = base.min(u32::MAX as u64) as u32; // past 4 GiB finish fails anyway
        out.name_off
            .extend(t.name_off.iter().map(|&o| o.wrapping_add(base)));
        if !t.names.is_empty() {
            self.names.push(std::mem::take(&mut t.names));
        }
        macro_rules! cat {
            ($($col:ident),*) => { $( out.$col.extend_from_slice(&t.$col); )* };
        }
        cat!(
            record, parent, name_len, kind, flags, size, alloc, ads_size, ads_alloc, mtime, ctime,
            atime, nlink
        );
    }

    /// Record number of the next record to push.
    pub fn next_record(&self) -> u64 {
        self.next
    }

    /// The records kept for their extension records, then all rows as one table.
    ///
    /// `fetch(record)` reads one raw record again: an extension record whose base has no
    /// `$ATTRIBUTE_LIST` (never written by NTFS, but [`parse`] merges it all the same) finds its
    /// base already emitted; that base is parsed again with it and replaces its earlier rows.
    pub fn finish(self, fetch: impl Fn(u64) -> Option<Vec<u8>>) -> Result<Table, String> {
        self.finish_counted(fetch).map(|(j, _)| j.into_table())
    }

    /// [`Stream::finish`], with what it did: records kept for their extension records, extension
    /// records, records read again, bases parsed again.
    pub fn finish_counted(
        mut self,
        fetch: impl Fn(u64) -> Option<Vec<u8>>,
    ) -> Result<(Joined, [usize; 4]), String> {
        let mut fetched = 0usize;
        let mut order: Vec<usize> = (0..self.exts.len()).collect();
        order.par_sort_unstable_by_key(|&k| (self.exts[k].base, self.exts[k].record));
        let exts: Vec<Ext> = order.iter().map(|&k| self.exts[k]).collect();
        let parsed: Vec<Option<Acc>> = order.iter().map(|&k| self.parsed[k].take()).collect();
        drop(order);
        self.exts = Vec::new();
        self.parsed = Vec::new();
        let ext_bad = parsed.iter().filter(|p| p.is_none()).count() as u64;
        let span = |base: u64| {
            let lo = exts.partition_point(|e| e.base < base);
            (lo, lo + exts[lo..].partition_point(|e| e.base == base))
        };
        // the kept bases with their extension records, in record order (in parallel, in order)
        let kept: Vec<u64> = self.deferred.iter().map(|d| d.record).collect();
        let parts: Vec<Table> = self
            .deferred
            .par_chunks_mut(4096)
            .map(|ds| {
                let mut chunk = Chunk::new(ds.len());
                for d in ds {
                    let (lo, hi) = span(d.record);
                    chunk.acc = std::mem::take(&mut d.acc);
                    chunk.merge_and_emit(d.record, d.seq, d.is_dir, &exts[lo..hi], &parsed[lo..hi]);
                }
                chunk.out
            })
            .collect();
        let deferred = std::mem::take(&mut self.deferred).len();
        let mut chunk = Chunk::new(0);
        // bases that were emitted without their extension records: parse them again
        let mut redone = std::collections::HashSet::new();
        let mut bases: Vec<u64> = exts
            .iter()
            .map(|e| e.base)
            .filter(|b| kept.binary_search(b).is_err())
            .collect();
        bases.dedup();
        for base in bases {
            fetched += 1;
            let Some(raw) = fetch(base) else { continue };
            if raw.len() != self.record_size || classify(&raw) != Class::Base {
                continue; // a ghost: the base is gone, unused or not a base record
            }
            let (lo, hi) = span(base);
            let (rows, bad) = (chunk.out.record.len(), chunk.out.bad_records);
            chunk.record(base as usize, &raw, &exts[lo..hi], &parsed[lo..hi]);
            chunk.out.bad_records = bad; // a torn base was counted when it was first seen
            if chunk.out.record.len() > rows {
                redone.insert(base);
            }
        }
        if !redone.is_empty() {
            self.out.drop_records(&redone);
        }
        for mut t in parts {
            self.append(&mut t);
        }
        self.append(&mut chunk.out);
        if self.names_len > u32::MAX as u64 {
            return Err("name blob exceeds 4 GiB".into());
        }
        let mut out = std::mem::take(&mut self.out);
        out.bad_records += ext_bad;
        out.fit();
        let joined = Joined {
            table: out,
            names: std::mem::take(&mut self.names),
            names_len: self.names_len as usize,
        };
        Ok((joined, [deferred, exts.len(), fetched, redone.len()]))
    }
}

impl Table {
    /// The rows of other records (the names blob is kept whole: offsets stay valid).
    fn drop_records(&mut self, records: &std::collections::HashSet<u64>) {
        let keep: Vec<bool> = self.record.iter().map(|r| !records.contains(r)).collect();
        macro_rules! pick {
            ($($col:ident),*) => { $(
                let mut k = keep.iter();
                self.$col.retain(|_| *k.next().unwrap());
            )* };
        }
        pick!(
            record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc,
            mtime, ctime, atime, nlink
        );
    }

    /// Room for `rows` more rows in every column (the names grow as they come).
    fn reserve_rows(&mut self, rows: usize) {
        macro_rules! more {
            ($($col:ident),*) => { $( self.$col.reserve_exact(rows); )* };
        }
        more!(
            record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc,
            mtime, ctime, atime, nlink
        );
    }

    /// Every column emptied, its memory kept.
    fn clear(&mut self) {
        macro_rules! empty {
            ($($col:ident),*) => { $( self.$col.clear(); )* };
        }
        empty!(
            record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc,
            mtime, ctime, atime, nlink, names
        );
        self.bad_records = 0;
    }

    /// Give back the capacity no row uses, when it is more than an eighth (shrinking copies).
    fn fit(&mut self) {
        if self.record.capacity() - self.record.len() <= self.record.len() / 8 {
            return;
        }
        macro_rules! fit {
            ($($col:ident),*) => { $( self.$col.shrink_to_fit(); )* };
        }
        fit!(
            record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc,
            mtime, ctime, atime, nlink
        );
    }
}

struct Chunk {
    out: Table,
    buf: Vec<u8>,
    acc: Acc,
    keep: Vec<bool>,
}

impl Chunk {
    /// A chunk writing into `out` (emptied; its memory reused), room made for `records` records.
    fn reuse(mut out: Table, records: usize) -> Self {
        let rows = records + records / 16; // a few files have several names
        if out.record.capacity() < rows {
            out.reserve_rows(rows);
        }
        out.names.reserve(rows * 24); // the last window's names moved on with its rows
        Chunk {
            out,
            buf: Vec::new(),
            acc: Acc::default(),
            keep: Vec::new(),
        }
    }

    fn new(records: usize) -> Self {
        let mut out = Table::default();
        let rows = records + records / 16; // a few files have several names
        out.record.reserve(rows);
        out.parent.reserve(rows);
        out.name_off.reserve(rows);
        out.name_len.reserve(rows);
        out.kind.reserve(rows);
        out.flags.reserve(rows);
        out.size.reserve(rows);
        out.alloc.reserve(rows);
        out.ads_size.reserve(rows);
        out.ads_alloc.reserve(rows);
        out.mtime.reserve(rows);
        out.ctime.reserve(rows);
        out.atime.reserve(rows);
        out.nlink.reserve(rows);
        out.names.reserve(rows * 24);
        Chunk {
            out,
            buf: Vec::new(),
            acc: Acc::default(),
            keep: Vec::new(),
        }
    }

    fn record(&mut self, i: usize, raw: &[u8], exts: &[Ext], parsed: &[Option<Acc>]) {
        match classify(raw) {
            Class::Base => {}
            Class::Bad => {
                self.out.bad_records += 1;
                return;
            }
            Class::Unused | Class::Ext(_) => return,
        }
        self.acc.reset();
        if !(fixup(raw, &mut self.buf) && walk(&self.buf, &mut self.acc)) {
            self.out.bad_records += 1;
            return;
        }
        let seq = u16_at(&self.buf, 0x10);
        let is_dir = u16_at(&self.buf, 0x16) & 2 != 0;
        self.merge_and_emit(i as u64, seq, is_dir, exts, parsed);
    }

    /// Fold in the extension records that still belong to this base (same sequence number), in
    /// record order, then emit its rows.
    fn merge_and_emit(
        &mut self,
        record: u64,
        seq: u16,
        is_dir: bool,
        exts: &[Ext],
        parsed: &[Option<Acc>],
    ) {
        for (e, p) in exts.iter().zip(parsed) {
            if let Some(ext) = p
                && e.base_seq == seq
            {
                self.acc.merge(ext);
            }
        }
        self.emit(record, is_dir);
    }

    fn emit(&mut self, record: u64, is_dir: bool) {
        let acc = &self.acc;
        if acc.names.is_empty() {
            return;
        }
        // a DOS 8.3 alias is dropped when a long name exists in the same directory
        self.keep.clear();
        self.keep.extend(acc.names.iter().map(|nm| {
            nm.ns != NS_DOS
                || !acc
                    .names
                    .iter()
                    .any(|o| o.ns != NS_DOS && (o.parent & REF_MASK) == (nm.parent & REF_MASK))
        }));
        let nlink = self
            .keep
            .iter()
            .filter(|k| **k)
            .count()
            .min(u16::MAX as usize) as u16;
        let surrogate = match acc.reparse {
            Reparse::None => false,
            Reparse::Tag(tag) => tag & REPARSE_NAME_SURROGATE != 0,
            Reparse::Unknown => true,
        };
        let kind = if surrogate { 2 } else { u8::from(is_dir) };
        let si = acc.si.unwrap_or_default();
        let mut flags = attr_flags(si.attrs);
        if acc.reparse != Reparse::None {
            flags |= F_REPARSE;
        }
        if acc.has_ads {
            flags |= F_ADS;
        }
        let (size, alloc) = if is_dir {
            (0, 0)
        } else {
            acc.data.unwrap_or((0, 0))
        };
        let (mtime, ctime, atime) = (
            unix_secs(si.mtime),
            unix_secs(si.crtime),
            unix_secs(si.atime),
        );
        let out = &mut self.out;
        for (nm, keep) in acc.names.iter().zip(&self.keep) {
            if !*keep {
                continue;
            }
            out.record.push(record);
            out.parent.push(nm.parent & REF_MASK);
            out.name_off.push(out.names.len() as u32);
            out.name_len.push(nm.len as u16);
            out.names
                .extend_from_slice(&acc.name_buf[nm.start as usize..(nm.start + nm.len) as usize]);
            out.kind.push(kind);
            out.flags.push(flags);
            out.size.push(size);
            out.alloc.push(alloc);
            out.ads_size.push(acc.ads_size);
            out.ads_alloc.push(acc.ads_alloc);
            out.mtime.push(mtime);
            out.ctime.push(ctime);
            out.atime.push(atime);
            out.nlink.push(nlink);
        }
    }
}

/// Chunk tables -> one table (the whole-buffer parser): offsets fixed up, each column copied in
/// parallel and freed from the chunks as it goes (one column more than the table at the peak).
fn concat(mut chunks: Vec<Table>, ext_bad: u64) -> Result<Table, String> {
    let mut base = 0u64;
    let mut bases = Vec::with_capacity(chunks.len());
    for c in &chunks {
        bases.push(base as u32);
        base += c.names.len() as u64;
    }
    if base > u32::MAX as u64 {
        return Err("name blob exceeds 4 GiB".into());
    }
    let mut out = Table {
        bad_records: ext_bad + chunks.iter().map(|c| c.bad_records).sum::<u64>(),
        ..Table::default()
    };
    let c = &mut chunks;
    out.record = join(c, |t| &mut t.record, |_, v| v)?;
    out.parent = join(c, |t| &mut t.parent, |_, v| v)?;
    out.name_off = join(c, |t| &mut t.name_off, |i, v| v + bases[i])?;
    out.name_len = join(c, |t| &mut t.name_len, |_, v| v)?;
    out.kind = join(c, |t| &mut t.kind, |_, v| v)?;
    out.flags = join(c, |t| &mut t.flags, |_, v| v)?;
    out.size = join(c, |t| &mut t.size, |_, v| v)?;
    out.alloc = join(c, |t| &mut t.alloc, |_, v| v)?;
    out.ads_size = join(c, |t| &mut t.ads_size, |_, v| v)?;
    out.ads_alloc = join(c, |t| &mut t.ads_alloc, |_, v| v)?;
    out.mtime = join(c, |t| &mut t.mtime, |_, v| v)?;
    out.ctime = join(c, |t| &mut t.ctime, |_, v| v)?;
    out.atime = join(c, |t| &mut t.atime, |_, v| v)?;
    out.nlink = join(c, |t| &mut t.nlink, |_, v| v)?;
    out.names = join(c, |t| &mut t.names, |_, v| v)?;
    Ok(out)
}

/// Copy one column of every chunk into one vector, the chunks' copies freed as soon as they are
/// in; `map(chunk, value)` rebases `name_off` (by the chunk's offset of its names in the blob).
fn join<T: Copy + Send + Sync>(
    chunks: &mut [Table],
    col: impl Fn(&mut Table) -> &mut Vec<T> + Sync,
    map: impl Fn(usize, T) -> T + Sync,
) -> Result<Vec<T>, String> {
    let lens: Vec<usize> = chunks.iter_mut().map(|c| col(c).len()).collect();
    let total: usize = lens.iter().sum();
    let mut out: Vec<T> = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|_| format!("out of memory joining {total} rows"))?;
    let mut dst: Vec<&mut [std::mem::MaybeUninit<T>]> = Vec::with_capacity(chunks.len());
    let mut rest = &mut out.spare_capacity_mut()[..total];
    for &n in &lens {
        let (a, b) = rest.split_at_mut(n);
        dst.push(a);
        rest = b;
    }
    chunks
        .par_iter_mut()
        .zip(dst)
        .enumerate()
        .for_each(|(i, (c, d))| {
            let src = std::mem::take(col(c));
            for (o, &v) in d.iter_mut().zip(&src) {
                o.write(map(i, v));
            }
        });
    // SAFETY: the chunks' lengths add up to `total` and every slot was written above.
    unsafe { out.set_len(total) };
    Ok(out)
}
