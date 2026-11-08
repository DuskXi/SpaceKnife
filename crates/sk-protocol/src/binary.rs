//! Binary-frame codec for bulk data (draw lists, child lists, scan batches), docs/protocol.md §4.
//!
//! Wire layout of one binary WebSocket frame:
//!
//! ```text
//! [u32 little-endian: header length H]
//! [H bytes: BinaryFrameHeader as UTF-8 JSON]
//! [zero padding so the data section starts on an 8-byte boundary]
//! [data section: each column little-endian, starting on an 8-byte boundary,
//!  then each blob (raw bytes, e.g. UTF-8 names), also 8-byte aligned]
//! ```
//!
//! Column and blob offsets in the header are relative to the start of the data section. Columns
//! all have `count` rows; a blob is an opaque byte string whose meaning the `layout` defines.
//!
//! [`FrameBuilder`] borrows the columns and writes header and data into one exactly sized
//! `Vec<u8>`: every column byte is copied once, straight into the frame (a plain `memcpy` on
//! little-endian targets). [`decode_frame`] is zero-copy: columns and blobs are slices of the input.

use std::borrow::Cow;
use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::base::yes;
use crate::envelope::RequestId;
use crate::errors::ProtocolError;

pub const ALIGN: usize = 8;
pub const MAX_HEADER_BYTES: usize = 64 * 1024;
const LEN: usize = 4;

const fn pad(n: usize) -> usize {
    (ALIGN - n % ALIGN) % ALIGN
}

// ------------------------------------------------------------------------ dtypes

/// Column element types: only fixed-width little-endian numbers, which every frontend maps to
/// typed arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dtype {
    U1,
    I1,
    U2,
    I2,
    U4,
    I4,
    U8,
    I8,
    F4,
    F8,
}

impl Dtype {
    pub const ALL: [Dtype; 10] = [
        Dtype::U1,
        Dtype::I1,
        Dtype::U2,
        Dtype::I2,
        Dtype::U4,
        Dtype::I4,
        Dtype::U8,
        Dtype::I8,
        Dtype::F4,
        Dtype::F8,
    ];

    /// The numpy spelling used on the wire (`<u4`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Dtype::U1 => "<u1",
            Dtype::I1 => "<i1",
            Dtype::U2 => "<u2",
            Dtype::I2 => "<i2",
            Dtype::U4 => "<u4",
            Dtype::I4 => "<i4",
            Dtype::U8 => "<u8",
            Dtype::I8 => "<i8",
            Dtype::F4 => "<f4",
            Dtype::F8 => "<f8",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Dtype::ALL.into_iter().find(|d| d.as_str() == s)
    }

    /// Bytes per element.
    pub const fn size(self) -> usize {
        match self {
            Dtype::U1 | Dtype::I1 => 1,
            Dtype::U2 | Dtype::I2 => 2,
            Dtype::U4 | Dtype::I4 | Dtype::F4 => 4,
            Dtype::U8 | Dtype::I8 | Dtype::F8 => 8,
        }
    }
}

impl std::fmt::Display for Dtype {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Dtype {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Dtype {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = Cow::<'de, str>::deserialize(d)?;
        Dtype::from_str(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unsupported dtype {s}")))
    }
}

impl JsonSchema for Dtype {
    fn inline_schema() -> bool {
        true
    }
    fn schema_name() -> Cow<'static, str> {
        "Dtype".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string" })
    }
}

mod sealed {
    pub trait Sealed {}
}

/// A Rust number type that is a column element.
pub trait Element: Copy + sealed::Sealed + 'static {
    const DTYPE: Dtype;
    /// Read one element from `Self::DTYPE.size()` little-endian bytes.
    fn from_le_slice(b: &[u8]) -> Self;
    /// Append the little-endian bytes of `values` to `out`.
    fn extend_le(values: &[Self], out: &mut Vec<u8>);
}

macro_rules! element {
    ($t:ty, $d:expr) => {
        impl sealed::Sealed for $t {}
        impl Element for $t {
            const DTYPE: Dtype = $d;
            #[inline]
            fn from_le_slice(b: &[u8]) -> Self {
                <$t>::from_le_bytes(b.try_into().expect("element width"))
            }
            #[inline]
            fn extend_le(values: &[Self], out: &mut Vec<u8>) {
                #[cfg(target_endian = "little")]
                {
                    // SAFETY: a plain number type has no padding and no invalid bit patterns, and
                    // its in-memory representation on a little-endian target is the wire format.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            values.as_ptr().cast::<u8>(),
                            std::mem::size_of_val(values),
                        )
                    };
                    out.extend_from_slice(bytes);
                }
                #[cfg(not(target_endian = "little"))]
                {
                    out.reserve(std::mem::size_of_val(values));
                    for v in values {
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
    };
}

element!(u8, Dtype::U1);
element!(i8, Dtype::I1);
element!(u16, Dtype::U2);
element!(i16, Dtype::I2);
element!(u32, Dtype::U4);
element!(i32, Dtype::I4);
element!(u64, Dtype::U8);
element!(i64, Dtype::I8);
element!(f32, Dtype::F4);
element!(f64, Dtype::F8);

/// A borrowed column of any element type.
#[derive(Clone, Copy, Debug)]
pub enum Column<'a> {
    U1(&'a [u8]),
    I1(&'a [i8]),
    U2(&'a [u16]),
    I2(&'a [i16]),
    U4(&'a [u32]),
    I4(&'a [i32]),
    U8(&'a [u64]),
    I8(&'a [i64]),
    F4(&'a [f32]),
    F8(&'a [f64]),
}

macro_rules! column_from {
    ($($t:ty => $v:ident),*) => {
        $(
            impl<'a> From<&'a [$t]> for Column<'a> {
                fn from(s: &'a [$t]) -> Self { Column::$v(s) }
            }
            impl<'a> From<&'a Vec<$t>> for Column<'a> {
                fn from(s: &'a Vec<$t>) -> Self { Column::$v(s) }
            }
        )*
    };
}

column_from!(u8 => U1, i8 => I1, u16 => U2, i16 => I2, u32 => U4, i32 => I4, u64 => U8, i64 => I8, f32 => F4, f64 => F8);

impl Column<'_> {
    pub fn dtype(&self) -> Dtype {
        match self {
            Column::U1(_) => Dtype::U1,
            Column::I1(_) => Dtype::I1,
            Column::U2(_) => Dtype::U2,
            Column::I2(_) => Dtype::I2,
            Column::U4(_) => Dtype::U4,
            Column::I4(_) => Dtype::I4,
            Column::U8(_) => Dtype::U8,
            Column::I8(_) => Dtype::I8,
            Column::F4(_) => Dtype::F4,
            Column::F8(_) => Dtype::F8,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Column::U1(s) => s.len(),
            Column::I1(s) => s.len(),
            Column::U2(s) => s.len(),
            Column::I2(s) => s.len(),
            Column::U4(s) => s.len(),
            Column::I4(s) => s.len(),
            Column::U8(s) => s.len(),
            Column::I8(s) => s.len(),
            Column::F4(s) => s.len(),
            Column::F8(s) => s.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Column::U1(s) => u8::extend_le(s, out),
            Column::I1(s) => i8::extend_le(s, out),
            Column::U2(s) => u16::extend_le(s, out),
            Column::I2(s) => i16::extend_le(s, out),
            Column::U4(s) => u32::extend_le(s, out),
            Column::I4(s) => i32::extend_le(s, out),
            Column::U8(s) => u64::extend_le(s, out),
            Column::I8(s) => i64::extend_le(s, out),
            Column::F4(s) => f32::extend_le(s, out),
            Column::F8(s) => f64::extend_le(s, out),
        }
    }
}

// ------------------------------------------------------------------------ header

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ColumnSpec {
    pub offset: u64,
    pub dtype: Dtype,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BlobSpec {
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BinaryFrameHeader {
    /// Logical stream, e.g. 'drawlist'
    pub stream: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    pub seq: u64,
    #[serde(rename = "final", default = "yes")]
    pub is_final: bool,
    /// Versioned column layout, e.g. 'rect.v1'
    pub layout: String,
    /// Rows; every column has exactly this many elements
    pub count: u64,
    pub columns: BTreeMap<String, ColumnSpec>,
    #[serde(default)]
    pub blobs: BTreeMap<String, BlobSpec>,
}

impl BinaryFrameHeader {
    fn check(&self) -> Result<(), ProtocolError> {
        for (field, v) in [("stream", &self.stream), ("layout", &self.layout)] {
            let n = v.chars().count();
            if n == 0 || n > 64 {
                return Err(ProtocolError(format!(
                    "invalid binary frame header: {field} must have 1-64 characters"
                )));
            }
        }
        Ok(())
    }
}

/// The header as the encoder writes it: borrowed, columns and blobs in the order they were added.
struct HeaderOut<'a> {
    stream: &'a str,
    request_id: Option<&'a RequestId>,
    seq: u64,
    is_final: bool,
    layout: &'a str,
    count: usize,
    columns: &'a [Entry<'a>],
    blobs: &'a [BlobEntry<'a>],
}

struct ColumnsOut<'a>(&'a [Entry<'a>]);
struct BlobsOut<'a>(&'a [BlobEntry<'a>]);

impl Serialize for ColumnsOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for e in self.0 {
            m.serialize_entry(
                e.name,
                &ColumnSpec {
                    offset: e.offset as u64,
                    dtype: e.column.dtype(),
                },
            )?;
        }
        m.end()
    }
}

impl Serialize for BlobsOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for b in self.0 {
            m.serialize_entry(
                b.name,
                &BlobSpec {
                    offset: b.offset as u64,
                    length: b.data.len() as u64,
                },
            )?;
        }
        m.end()
    }
}

impl Serialize for HeaderOut<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("BinaryFrameHeader", 8)?;
        st.serialize_field("stream", self.stream)?;
        if let Some(id) = self.request_id {
            st.serialize_field("request_id", id)?;
        }
        st.serialize_field("seq", &self.seq)?;
        st.serialize_field("final", &self.is_final)?;
        st.serialize_field("layout", self.layout)?;
        st.serialize_field("count", &self.count)?;
        st.serialize_field("columns", &ColumnsOut(self.columns))?;
        st.serialize_field("blobs", &BlobsOut(self.blobs))?;
        st.end()
    }
}

// ------------------------------------------------------------------------ encoder

struct Entry<'a> {
    name: &'a str,
    column: Column<'a>,
    offset: usize,
}

struct BlobEntry<'a> {
    name: &'a str,
    data: &'a [u8],
    offset: usize,
}

/// Builds one binary frame from borrowed columns and blobs.
///
/// ```
/// use sk_protocol::binary::{FrameBuilder, decode_frame};
/// let keys: Vec<u32> = vec![1, 2, 3];
/// let sizes: Vec<i64> = vec![10, 20, 30];
/// let frame = FrameBuilder::new("view:v1", "drawlist.v1")
///     .seq(7)
///     .column("key", &keys)
///     .column("size", &sizes)
///     .blob("names", b"abc")
///     .finish()
///     .unwrap();
/// let decoded = decode_frame(&frame).unwrap();
/// assert_eq!(decoded.column("size").unwrap().to_vec::<i64>().unwrap(), sizes);
/// ```
pub struct FrameBuilder<'a> {
    stream: &'a str,
    layout: &'a str,
    seq: u64,
    is_final: bool,
    request_id: Option<&'a RequestId>,
    columns: Vec<Entry<'a>>,
    blobs: Vec<BlobEntry<'a>>,
    data_len: usize,
    error: Option<ProtocolError>,
}

impl<'a> FrameBuilder<'a> {
    pub fn new(stream: &'a str, layout: &'a str) -> Self {
        Self {
            stream,
            layout,
            seq: 0,
            is_final: true,
            request_id: None,
            columns: Vec::with_capacity(20),
            blobs: Vec::new(),
            data_len: 0,
            error: None,
        }
    }

    #[must_use]
    pub fn seq(mut self, seq: u64) -> Self {
        self.seq = seq;
        self
    }

    #[must_use]
    pub fn is_final(mut self, is_final: bool) -> Self {
        self.is_final = is_final;
        self
    }

    #[must_use]
    pub fn request_id(mut self, id: Option<&'a RequestId>) -> Self {
        self.request_id = id;
        self
    }

    fn fail(&mut self, msg: String) {
        if self.error.is_none() {
            self.error = Some(ProtocolError(msg));
        }
    }

    fn taken(&self, name: &str) -> bool {
        self.columns.iter().any(|c| c.name == name) || self.blobs.iter().any(|b| b.name == name)
    }

    /// Add a column. All columns must have the same length; blobs must come after the columns.
    #[must_use]
    pub fn column(mut self, name: &'a str, column: impl Into<Column<'a>>) -> Self {
        self.push_column(name, column.into());
        self
    }

    /// [`column`](Self::column) by reference, for building in a loop.
    pub fn push_column(&mut self, name: &'a str, column: Column<'a>) -> &mut Self {
        if let Some(first) = self.columns.first()
            && column.len() != first.column.len()
        {
            let msg = format!(
                "column {name:?} has {} rows, expected {}",
                column.len(),
                first.column.len()
            );
            self.fail(msg);
            return self;
        }
        if !self.blobs.is_empty() {
            self.fail(format!("column {name:?} added after a blob"));
            return self;
        }
        if self.taken(name) {
            self.fail(format!("column {name:?} given twice"));
            return self;
        }
        let bytes = column.len() * column.dtype().size();
        self.columns.push(Entry {
            name,
            column,
            offset: self.data_len,
        });
        self.data_len += bytes + pad(bytes);
        self
    }

    /// Add a raw blob after the columns.
    #[must_use]
    pub fn blob(mut self, name: &'a str, data: &'a [u8]) -> Self {
        self.push_blob(name, data);
        self
    }

    pub fn push_blob(&mut self, name: &'a str, data: &'a [u8]) -> &mut Self {
        if self.taken(name) {
            self.fail(format!("blob {name:?} has the same name as a column"));
            return self;
        }
        self.blobs.push(BlobEntry {
            name,
            data,
            offset: self.data_len,
        });
        self.data_len += data.len() + pad(data.len());
        self
    }

    /// Rows of the frame (the columns' common length; 0 without columns).
    pub fn count(&self) -> usize {
        self.columns.first().map_or(0, |c| c.column.len())
    }

    /// Write the frame.
    pub fn finish(self) -> Result<Vec<u8>, ProtocolError> {
        let mut out = Vec::new();
        self.finish_into(&mut out)?;
        Ok(out)
    }

    /// Write the frame into `out` (cleared first), reusing its allocation.
    pub fn finish_into(self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        out.clear();
        // Header JSON is typically a few hundred bytes; reserve generously to write once.
        out.reserve(
            LEN + 256 + 48 * (self.columns.len() + self.blobs.len()) + ALIGN + self.data_len,
        );
        out.extend_from_slice(&[0; LEN]);
        let header = HeaderOut {
            stream: self.stream,
            request_id: self.request_id,
            seq: self.seq,
            is_final: self.is_final,
            layout: self.layout,
            count: self.count(),
            columns: &self.columns,
            blobs: &self.blobs,
        };
        serde_json::to_writer(&mut *out, &header).map_err(|e| ProtocolError(e.to_string()))?;
        let header_len = out.len() - LEN;
        if header_len > MAX_HEADER_BYTES {
            return Err(ProtocolError::new("binary frame header too large"));
        }
        out[..LEN].copy_from_slice(&(header_len as u32).to_le_bytes());
        out.resize(out.len() + pad(out.len()), 0);
        let data_start = out.len();
        out.reserve(self.data_len);
        for c in &self.columns {
            debug_assert_eq!(out.len() - data_start, c.offset);
            c.column.write(out);
            out.resize(out.len() + pad(out.len() - data_start), 0);
        }
        for b in &self.blobs {
            debug_assert_eq!(out.len() - data_start, b.offset);
            out.extend_from_slice(b.data);
            out.resize(out.len() + pad(out.len() - data_start), 0);
        }
        debug_assert_eq!(out.len() - data_start, self.data_len);
        Ok(())
    }
}

/// Header fields of a frame, for [`encode_frame`].
#[derive(Clone, Copy, Debug)]
pub struct FrameMeta<'a> {
    pub stream: &'a str,
    pub layout: &'a str,
    pub seq: u64,
    pub is_final: bool,
    pub request_id: Option<&'a RequestId>,
}

impl<'a> FrameMeta<'a> {
    pub fn new(stream: &'a str, layout: &'a str) -> Self {
        Self {
            stream,
            layout,
            seq: 0,
            is_final: true,
            request_id: None,
        }
    }
}

/// Pack equally long numeric columns, plus raw blobs, into one binary frame.
pub fn encode_frame(
    meta: &FrameMeta<'_>,
    columns: &[(&str, Column<'_>)],
    blobs: &[(&str, &[u8])],
) -> Result<Vec<u8>, ProtocolError> {
    let mut b = FrameBuilder::new(meta.stream, meta.layout)
        .seq(meta.seq)
        .is_final(meta.is_final)
        .request_id(meta.request_id);
    for (name, col) in columns {
        b.push_column(name, *col);
    }
    for (name, data) in blobs {
        b.push_blob(name, data);
    }
    b.finish()
}

// ------------------------------------------------------------------------ decoder

/// A decoded column: a slice of the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnView<'a> {
    pub dtype: Dtype,
    pub count: usize,
    /// `count * dtype.size()` little-endian bytes.
    pub bytes: &'a [u8],
}

impl ColumnView<'_> {
    /// Element `i` (panics when out of range or the type does not match the dtype).
    pub fn get<T: Element>(&self, i: usize) -> T {
        assert_eq!(T::DTYPE, self.dtype, "column dtype");
        let w = self.dtype.size();
        T::from_le_slice(&self.bytes[i * w..(i + 1) * w])
    }

    /// Copy the column out as `Vec<T>`; fails if `T` is not the column's dtype.
    pub fn to_vec<T: Element>(&self) -> Result<Vec<T>, ProtocolError> {
        if T::DTYPE != self.dtype {
            return Err(ProtocolError(format!(
                "column is {}, not {}",
                self.dtype,
                T::DTYPE
            )));
        }
        Ok(self
            .bytes
            .chunks_exact(self.dtype.size())
            .map(T::from_le_slice)
            .collect())
    }
}

/// A decoded frame: header plus zero-copy views of the columns and blobs.
#[derive(Clone, Debug)]
pub struct DecodedFrame<'a> {
    pub header: BinaryFrameHeader,
    pub columns: BTreeMap<String, ColumnView<'a>>,
    pub blobs: BTreeMap<String, &'a [u8]>,
}

impl<'a> DecodedFrame<'a> {
    pub fn column(&self, name: &str) -> Option<&ColumnView<'a>> {
        self.columns.get(name)
    }

    pub fn blob(&self, name: &str) -> Option<&'a [u8]> {
        self.blobs.get(name).copied()
    }
}

/// Unpack a binary frame. Columns and blobs are slices of `buf`.
pub fn decode_frame(buf: &[u8]) -> Result<DecodedFrame<'_>, ProtocolError> {
    if buf.len() < LEN {
        return Err(ProtocolError::new(
            "binary frame shorter than its length prefix",
        ));
    }
    let header_len = u32::from_le_bytes(buf[..LEN].try_into().expect("4 bytes")) as usize;
    if header_len > MAX_HEADER_BYTES || LEN + header_len > buf.len() {
        return Err(ProtocolError::new(
            "binary frame header length out of range",
        ));
    }
    let header: BinaryFrameHeader = serde_json::from_slice(&buf[LEN..LEN + header_len])
        .map_err(|e| ProtocolError(format!("invalid binary frame header: {e}")))?;
    header.check()?;
    let mut data_start = LEN + header_len;
    data_start += pad(data_start);
    let data = buf.get(data_start..).unwrap_or(&[]);
    let count = usize::try_from(header.count)
        .map_err(|_| ProtocolError::new("binary frame count out of range"))?;
    let mut columns = BTreeMap::new();
    for (name, spec) in &header.columns {
        let offset = usize::try_from(spec.offset).unwrap_or(usize::MAX);
        if offset % ALIGN != 0 {
            return Err(ProtocolError(format!(
                "column {name:?} is not {ALIGN}-byte aligned"
            )));
        }
        let end = count
            .checked_mul(spec.dtype.size())
            .and_then(|n| n.checked_add(offset));
        match end {
            Some(end) if end <= data.len() => {
                columns.insert(
                    name.clone(),
                    ColumnView {
                        dtype: spec.dtype,
                        count,
                        bytes: &data[offset..end],
                    },
                );
            }
            _ => {
                return Err(ProtocolError(format!(
                    "column {name:?} runs past the end of the frame"
                )));
            }
        }
    }
    let mut blobs = BTreeMap::new();
    for (name, spec) in &header.blobs {
        if columns.contains_key(name) {
            return Err(ProtocolError(format!(
                "blob {name:?} has the same name as a column"
            )));
        }
        let offset = usize::try_from(spec.offset).unwrap_or(usize::MAX);
        let end = usize::try_from(spec.length)
            .ok()
            .and_then(|n| n.checked_add(offset));
        match end {
            Some(end) if offset % ALIGN == 0 && end <= data.len() => {
                blobs.insert(name.clone(), &data[offset..end]);
            }
            _ => {
                return Err(ProtocolError(format!(
                    "blob {name:?} is misaligned or runs past the end of the frame"
                )));
            }
        }
    }
    Ok(DecodedFrame {
        header,
        columns,
        blobs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed_dtypes_is_aligned_and_zero_copy() {
        let id: Vec<u32> = (0..7).collect();
        let x: Vec<f32> = (0..7).map(|i| i as f32 / 6.0).collect();
        let size: Vec<i64> = (0..7).map(|i| i * 1_000_000_000_000).collect();
        let flags: Vec<u8> = vec![1, 0, 1, 0, 1, 0, 1];
        let w: Vec<f64> = vec![1.0; 7];
        let rid = RequestId::Int(42);
        let frame = FrameBuilder::new("drawlist", "rect.v1")
            .seq(3)
            .is_final(false)
            .request_id(Some(&rid))
            .column("id", &id)
            .column("x", &x)
            .column("size", &size)
            .column("flags", &flags)
            .column("w", &w)
            .finish()
            .unwrap();
        let d = decode_frame(&frame).unwrap();
        let h = &d.header;
        assert_eq!(
            (
                h.stream.as_str(),
                h.layout.as_str(),
                h.seq,
                h.is_final,
                h.request_id.clone()
            ),
            ("drawlist", "rect.v1", 3, false, Some(RequestId::Int(42)))
        );
        assert_eq!(h.count, 7);
        assert_eq!(d.column("id").unwrap().to_vec::<u32>().unwrap(), id);
        assert_eq!(d.column("x").unwrap().to_vec::<f32>().unwrap(), x);
        assert_eq!(d.column("size").unwrap().to_vec::<i64>().unwrap(), size);
        assert_eq!(d.column("flags").unwrap().to_vec::<u8>().unwrap(), flags);
        assert_eq!(d.column("w").unwrap().to_vec::<f64>().unwrap(), w);
        assert_eq!(d.column("size").unwrap().get::<i64>(6), 6_000_000_000_000);
        assert_eq!(d.column("x").unwrap().dtype, Dtype::F4);
        for spec in h.columns.values() {
            assert_eq!(spec.offset % ALIGN as u64, 0);
        }
        // zero copy: the views point into the frame
        let range = frame.as_ptr_range();
        assert!(range.contains(&d.column("id").unwrap().bytes.as_ptr()));
        let hlen = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!((4 + hlen + pad(4 + hlen)) % ALIGN, 0);
        assert!(d.column("id").unwrap().to_vec::<i32>().is_err());
    }

    #[test]
    fn header_matches_the_python_shape() {
        let frame = FrameBuilder::new("s", "l")
            .column("a", &[1u16, 2][..])
            .finish()
            .unwrap();
        let hlen = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
        let header: serde_json::Value = serde_json::from_slice(&frame[4..4 + hlen]).unwrap();
        assert_eq!(
            header,
            serde_json::json!({
                "stream": "s", "seq": 0, "final": true, "layout": "l", "count": 2,
                "columns": {"a": {"offset": 0, "dtype": "<u2"}}, "blobs": {}
            })
        );
        assert_eq!(frame.len(), 4 + hlen + pad(4 + hlen) + 8); // prefix, header, pad, 4 data bytes + 4 pad
    }

    #[test]
    fn empty_frame() {
        let frame = FrameBuilder::new("s", "l")
            .column("a", &Vec::<f32>::new())
            .finish()
            .unwrap();
        let d = decode_frame(&frame).unwrap();
        assert_eq!(d.header.count, 0);
        assert_eq!(d.column("a").unwrap().count, 0);
        let frame = FrameBuilder::new("s", "l").finish().unwrap();
        assert_eq!(decode_frame(&frame).unwrap().header.count, 0);
    }

    #[test]
    fn encode_rejects_bad_columns() {
        let a = [0f32; 3];
        let b = [0f32; 4];
        assert!(
            FrameBuilder::new("s", "l")
                .column("a", &a[..])
                .column("b", &b[..])
                .finish()
                .is_err()
        );
        assert!(
            FrameBuilder::new("s", "l")
                .column("a", &a[..])
                .column("a", &a[..])
                .finish()
                .is_err()
        );
        let blob_first = FrameBuilder::new("s", "l")
            .blob("n", b"x")
            .column("a", &a[..])
            .finish();
        assert!(blob_first.is_err());
        let long = "x".repeat(MAX_HEADER_BYTES);
        assert!(FrameBuilder::new(&long, "l").finish().is_err());
    }

    #[test]
    fn decode_rejects_truncated_and_garbage() {
        let a: Vec<f64> = (0..100).map(f64::from).collect();
        let frame = FrameBuilder::new("s", "l")
            .column("a", &a)
            .finish()
            .unwrap();
        assert!(decode_frame(&frame[..frame.len() - 8]).is_err());
        assert!(decode_frame(b"\x01\x00").is_err());
        let mut garbage = 10u32.to_le_bytes().to_vec();
        garbage.extend_from_slice(b"not json!!");
        assert!(decode_frame(&garbage).is_err());
        assert!(decode_frame(&(1u32 << 30).to_le_bytes()).is_err());
        // a bad dtype in the header
        let header = br#"{"stream":"s","seq":0,"layout":"l","count":1,"columns":{"a":{"offset":0,"dtype":"<c8"}}}"#;
        let mut bad = (header.len() as u32).to_le_bytes().to_vec();
        bad.extend_from_slice(header);
        bad.resize(bad.len() + 32, 0);
        assert!(decode_frame(&bad).is_err());
        // misaligned column
        let header = br#"{"stream":"s","seq":0,"layout":"l","count":1,"columns":{"a":{"offset":4,"dtype":"<u1"}}}"#;
        let mut bad = (header.len() as u32).to_le_bytes().to_vec();
        bad.extend_from_slice(header);
        bad.resize(bad.len() + 32, 0);
        assert!(decode_frame(&bad).is_err());
    }

    #[test]
    fn blobs_travel_after_the_columns() {
        let names = "Users\0视频.mp4\0".as_bytes();
        let lens = [5u32, 10];
        let frame = encode_frame(
            &FrameMeta::new("view", "drawlist.v1"),
            &[("name_len", Column::from(&lens[..]))],
            &[("names", names)],
        )
        .unwrap();
        let d = decode_frame(&frame).unwrap();
        assert_eq!(d.header.blobs["names"].length, names.len() as u64);
        assert_eq!(d.header.blobs["names"].offset % ALIGN as u64, 0);
        assert_eq!(d.blob("names").unwrap(), names);
        assert_eq!(d.column("name_len").unwrap().to_vec::<u32>().unwrap(), lens);
    }

    #[test]
    fn blob_names_must_not_shadow_columns() {
        let a = [0u8];
        assert!(
            FrameBuilder::new("s", "l")
                .column("a", &a[..])
                .blob("a", b"x")
                .finish()
                .is_err()
        );
    }

    #[test]
    fn finish_into_reuses_the_buffer() {
        let a = [1i32, 2, 3];
        let mut buf = Vec::with_capacity(1024);
        FrameBuilder::new("s", "l")
            .column("a", &a[..])
            .finish_into(&mut buf)
            .unwrap();
        let first = buf.clone();
        FrameBuilder::new("s", "l")
            .column("a", &a[..])
            .finish_into(&mut buf)
            .unwrap();
        assert_eq!(first, buf);
        assert_eq!(
            decode_frame(&buf)
                .unwrap()
                .column("a")
                .unwrap()
                .to_vec::<i32>()
                .unwrap(),
            a
        );
    }
}
