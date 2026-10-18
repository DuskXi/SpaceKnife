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

const CHUNK: usize = 8192; // records per parallel task

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
    fn rows(&self) -> usize {
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

/// The same rows as [`parse`], fed window by window: the whole $MFT is never in memory at once.
///
/// A base record without an `$ATTRIBUTE_LIST` cannot have extension records, so it is emitted at
/// once. The few with a list are kept raw (with every extension record, parsed) until
/// [`Stream::finish`], whose rows come after all others; rows are otherwise in record order.
pub struct Stream {
    record_size: usize,
    next: u64, // record number of the next record fed
    tables: Vec<Table>,
    deferred: Vec<(u64, Vec<u8>)>,
    exts: Vec<Ext>,
    parsed: Vec<Option<Acc>>,
}

impl Stream {
    pub fn new(record_size: usize) -> Self {
        Stream {
            record_size,
            next: 0,
            tables: Vec::new(),
            deferred: Vec::new(),
            exts: Vec::new(),
            parsed: Vec::new(),
        }
    }

    /// Parse the next records (`window.len()` is a multiple of the record size).
    pub fn feed(&mut self, window: &[u8]) {
        let rs = self.record_size;
        let n = window.len() / rs;
        let first = self.next as usize;
        let rec = |i: usize| &window[i * rs..(i + 1) * rs];
        type Part = (Table, Vec<(u64, Vec<u8>)>, Vec<Ext>, Vec<Option<Acc>>);
        let parts: Vec<Part> = (0..n.div_ceil(CHUNK))
            .into_par_iter()
            .map(|c| {
                let lo = c * CHUNK;
                let hi = (lo + CHUNK).min(n);
                let mut chunk = Chunk::new(hi - lo);
                let (mut deferred, mut exts, mut parsed) = (Vec::new(), Vec::new(), Vec::new());
                for i in lo..hi {
                    let raw = rec(i);
                    let number = (first + i) as u64;
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
                                deferred.push((number, raw.to_vec()));
                            } else {
                                let is_dir = u16_at(&chunk.buf, 0x16) & 2 != 0;
                                chunk.emit(number, is_dir);
                            }
                        }
                    }
                }
                (chunk.out, deferred, exts, parsed)
            })
            .collect();
        for (table, deferred, exts, parsed) in parts {
            self.tables.push(table);
            self.deferred.extend(deferred);
            self.exts.extend(exts);
            self.parsed.extend(parsed);
        }
        self.next += n as u64;
    }

    /// The records kept for their extension records, then all rows as one table.
    ///
    /// `fetch(record)` reads one raw record again: an extension record whose base has no
    /// `$ATTRIBUTE_LIST` (never written by NTFS, but [`parse`] merges it all the same) finds its
    /// base already emitted; that base is parsed again with it and replaces its earlier rows.
    pub fn finish(mut self, fetch: impl Fn(u64) -> Option<Vec<u8>>) -> Result<Table, String> {
        let mut order: Vec<usize> = (0..self.exts.len()).collect();
        order.sort_unstable_by_key(|&k| (self.exts[k].base, self.exts[k].record));
        let exts: Vec<Ext> = order.iter().map(|&k| self.exts[k]).collect();
        let parsed: Vec<Option<Acc>> = order.iter().map(|&k| self.parsed[k].take()).collect();
        let ext_bad = parsed.iter().filter(|p| p.is_none()).count() as u64;
        let span = |base: u64| {
            let lo = exts.partition_point(|e| e.base < base);
            (lo, lo + exts[lo..].partition_point(|e| e.base == base))
        };
        let mut chunk = Chunk::new(self.deferred.len());
        let mut kept = std::collections::HashSet::with_capacity(self.deferred.len());
        for (number, raw) in &self.deferred {
            let (lo, hi) = span(*number);
            chunk.record(*number as usize, raw, &exts[lo..hi], &parsed[lo..hi]);
            kept.insert(*number);
        }
        // bases that were emitted without their extension records: parse them again
        let mut redone = std::collections::HashSet::new();
        let mut bases: Vec<u64> = exts.iter().map(|e| e.base).filter(|b| !kept.contains(b)).collect();
        bases.dedup();
        for base in bases {
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
            for t in &mut self.tables {
                if t.record.iter().any(|r| redone.contains(r)) {
                    *t = t.without(&redone);
                }
            }
        }
        self.tables.push(chunk.out);
        concat(std::mem::take(&mut self.tables), ext_bad)
    }
}

impl Table {
    /// The rows of other records (the names blob is kept whole: offsets stay valid).
    fn without(&self, records: &std::collections::HashSet<u64>) -> Table {
        let keep: Vec<usize> = (0..self.rows())
            .filter(|&i| !records.contains(&self.record[i]))
            .collect();
        macro_rules! pick {
            ($($col:ident),*) => {
                Table {
                    $($col: keep.iter().map(|&i| self.$col[i]).collect(),)*
                    names: self.names.clone(),
                    bad_records: self.bad_records,
                }
            };
        }
        pick!(
            record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc, mtime,
            ctime, atime, nlink
        )
    }
}

struct Chunk {
    out: Table,
    buf: Vec<u8>,
    acc: Acc,
    keep: Vec<bool>,
}

impl Chunk {
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
        for (e, p) in exts.iter().zip(parsed) {
            if let Some(ext) = p
                && e.base_seq == seq
            {
                self.acc.merge(ext);
            }
        }
        self.emit(i as u64, is_dir);
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

/// Chunk tables -> one table: offsets fixed up, columns copied in parallel.
fn concat(mut chunks: Vec<Table>, ext_bad: u64) -> Result<Table, String> {
    let mut name_base = 0u64;
    let mut bases = Vec::with_capacity(chunks.len());
    for c in &chunks {
        bases.push(name_base);
        name_base += c.names.len() as u64;
    }
    if name_base > u32::MAX as u64 {
        return Err("name blob exceeds 4 GiB".into());
    }
    chunks.par_iter_mut().zip(&bases).for_each(|(c, &base)| {
        if base != 0 {
            c.name_off.iter_mut().for_each(|o| *o += base as u32);
        }
    });
    let rows: usize = chunks.iter().map(Table::rows).sum();
    let bad_records = ext_bad + chunks.iter().map(|c| c.bad_records).sum::<u64>();

    macro_rules! join_cols {
        ($($col:ident),*) => {{
            let mut out = Table { bad_records, ..Table::default() };
            rayon::scope(|s| {
                $(
                    let dst = &mut out.$col;
                    let parts = &chunks;
                    s.spawn(move |_| {
                        let total: usize = parts.iter().map(|c| c.$col.len()).sum();
                        dst.reserve_exact(total);
                        for c in parts {
                            dst.extend_from_slice(&c.$col);
                        }
                    });
                )*
            });
            out
        }};
    }
    let out = join_cols!(
        record, parent, name_off, name_len, kind, flags, size, alloc, ads_size, ads_alloc, mtime,
        ctime, atime, nlink, names
    );
    debug_assert_eq!(out.rows(), rows);
    Ok(out)
}
