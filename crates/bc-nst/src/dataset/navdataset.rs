//! The NavDataSet binary format and its JSON-RPC wrapper.
//!
//! Stream layout (`NavDataSet.WriteTo` / `ReadFrom`, `NavSerializer.WriteItem` /
//! `ReadItem` in Microsoft.Dynamics.Nav.Types.Data):
//!
//! ```text
//! DataSet:  name:string  lcid:i32  tableCount:i32
//!           tableCount x { tableName:string  lcid:i32  columnCount:i32
//!                          columnCount x { columnName:string  typeCode:u8  isLazy:bool
//!                                          extPropCount:i32  extPropCount x (typed key, typed value) } }
//!           then per table, in order: rowCount:i32, rowCount x columnCount cells
//! Cell:     marker:u8 [payload]   0 DBNull, 1 DBNotChanged, 2 value, 3 zero, 4 DBLazy
//!           (the reader treats markers >= 5 like 2)
//! typed:    a Byte cell holding the type code, then a cell of that type
//! ```
//!
//! JSON-RPC wrapper (`NavDataSetConverter`): `{"compressed":bool,"count":n,"buffers":[base64...]}`.
//! The .NET reader is positional, so the three properties must be written in exactly
//! this order. The `Serialize` impl below does that. A `serde_json::Value` built from
//! it keeps the order only with serde_json's `preserve_order` feature (enabled in
//! bc-nst's Cargo.toml).

use super::netbin::{BinError, BinReader, BinWriter};
use base64::Engine;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::{Read, Write};

/// `CultureInfo.InvariantCulture.LCID`.
pub const LCID_INVARIANT: i32 = 127;
/// Name of `new NavDataSet()` in .NET. The client never reads the name.
pub const DEFAULT_DATASET_NAME: &str = "NewDataSet";

/// `Microsoft.Dynamics.Nav.Types.Data.CellTypeCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CellTypeCode {
    Object = 0,
    DBNull = 1,
    Boolean = 2,
    Char = 3,
    SByte = 4,
    Byte = 5,
    Int16 = 6,
    UInt16 = 7,
    Int32 = 8,
    UInt32 = 9,
    Int64 = 10,
    UInt64 = 11,
    Single = 12,
    Double = 13,
    Decimal = 14,
    DateTime = 15,
    TimeSpan = 16,
    String = 17,
    Guid = 18,
    ByteArray = 19,
    Media = 20,
    MediaSet = 21,
    DBLazy = 22,
    DBNotChanged = 23,
}

impl CellTypeCode {
    pub fn from_u8(b: u8) -> Option<CellTypeCode> {
        use CellTypeCode::*;
        Some(match b {
            0 => Object,
            1 => DBNull,
            2 => Boolean,
            3 => Char,
            4 => SByte,
            5 => Byte,
            6 => Int16,
            7 => UInt16,
            8 => Int32,
            9 => UInt32,
            10 => Int64,
            11 => UInt64,
            12 => Single,
            13 => Double,
            14 => Decimal,
            15 => DateTime,
            16 => TimeSpan,
            17 => String,
            18 => Guid,
            19 => ByteArray,
            20 => Media,
            21 => MediaSet,
            22 => DBLazy,
            23 => DBNotChanged,
            _ => return None,
        })
    }

    fn is_numeric(self) -> bool {
        use CellTypeCode::*;
        matches!(
            self,
            SByte | Byte | Int16 | UInt16 | Int32 | UInt32 | Int64 | UInt64 | Single | Double | Decimal
        )
    }
}

/// `System.DateTimeKind`. The .NET reader turns `Unspecified` into `Local`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DateTimeKind {
    Unspecified = 0,
    Utc = 1,
    Local = 2,
}

impl DateTimeKind {
    fn from_i32(v: i32) -> DateTimeKind {
        match v {
            1 => DateTimeKind::Utc,
            2 => DateTimeKind::Local,
            _ => DateTimeKind::Unspecified,
        }
    }
}

/// One cell. The column's type code decides how a value is written, so the cell
/// variant has to match it (checked when rows are added).
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// marker 0
    Null,
    /// marker 1. Do not use in page data: the JSON-RPC reader has row
    /// deduplication off and keeps the marker as a literal value.
    NotChanged,
    /// marker 4
    Lazy,
    /// marker 3, the compact form of `0` / `0m`. Numeric columns only.
    Zero,
    Bool(bool),
    Char(char),
    SByte(i8),
    Byte(u8),
    Int16(i16),
    UInt16(u16),
    Int32(i32),
    UInt32(u32),
    Int64(i64),
    UInt64(u64),
    Single(f32),
    Double(f64),
    Decimal(Decimal),
    /// .NET `DateTime`: ticks of 100 ns since 0001-01-01 plus kind.
    DateTime {
        ticks: i64,
        kind: DateTimeKind,
    },
    /// .NET `TimeSpan` ticks.
    TimeSpan(i64),
    String(String),
    /// GUID in textual (RFC 4122) byte order; written in `Guid.ToByteArray()` order.
    Guid([u8; 16]),
    Bytes(Vec<u8>),
    /// Media / MediaSet payload: `wrapped` is the NSMediaWrapper flag.
    Media {
        wrapped: bool,
        data: Vec<u8>,
    },
}

impl Cell {
    fn marker(&self) -> u8 {
        match self {
            Cell::Null => 0,
            Cell::NotChanged => 1,
            Cell::Zero => 3,
            Cell::Lazy => 4,
            _ => 2,
        }
    }

    /// Can this cell be written into a column of type `t`?
    pub fn fits(&self, t: CellTypeCode) -> bool {
        use CellTypeCode as T;
        match self {
            Cell::Null | Cell::NotChanged | Cell::Lazy => true,
            Cell::Zero => t.is_numeric(),
            Cell::Bool(_) => t == T::Boolean,
            Cell::Char(_) => t == T::Char,
            Cell::SByte(_) => t == T::SByte,
            Cell::Byte(_) => t == T::Byte,
            Cell::Int16(_) => t == T::Int16,
            Cell::UInt16(_) => t == T::UInt16,
            Cell::Int32(_) => t == T::Int32,
            Cell::UInt32(_) => t == T::UInt32,
            Cell::Int64(_) => t == T::Int64,
            Cell::UInt64(_) => t == T::UInt64,
            Cell::Single(_) => t == T::Single,
            Cell::Double(_) => t == T::Double,
            Cell::Decimal(_) => t == T::Decimal,
            Cell::DateTime { .. } => t == T::DateTime,
            Cell::TimeSpan(_) => t == T::TimeSpan,
            Cell::String(_) => t == T::String,
            Cell::Guid(_) => t == T::Guid,
            Cell::Bytes(_) => t == T::ByteArray,
            Cell::Media { .. } => t == T::Media || t == T::MediaSet,
        }
    }

    /// `NavSerializer.WriteItem`.
    pub fn write(&self, t: CellTypeCode, w: &mut BinWriter) -> Result<(), DatasetError> {
        if !self.fits(t) {
            return Err(DatasetError(format!("cell {self:?} does not fit a {t:?} column")));
        }
        w.u8(self.marker());
        match self {
            Cell::Null | Cell::NotChanged | Cell::Lazy | Cell::Zero => {}
            Cell::Bool(v) => w.bool(*v),
            Cell::Char(c) => w.char(*c)?,
            Cell::SByte(v) => w.i8(*v),
            Cell::Byte(v) => w.u8(*v),
            Cell::Int16(v) => w.i16(*v),
            Cell::UInt16(v) => w.u16(*v),
            Cell::Int32(v) => w.i32(*v),
            Cell::UInt32(v) => w.u32(*v),
            Cell::Int64(v) => w.i64(*v),
            Cell::UInt64(v) => w.u64(*v),
            Cell::Single(v) => w.f32(*v),
            Cell::Double(v) => w.f64(*v),
            Cell::Decimal(d) => w.decimal(*d),
            Cell::DateTime { ticks, kind } => {
                w.i64(*ticks);
                w.i32(*kind as i32);
            }
            Cell::TimeSpan(t) => w.i64(*t),
            Cell::String(s) => w.string(s),
            Cell::Guid(g) => w.guid(g),
            Cell::Bytes(b) => {
                w.i32(len_i32(b.len())?);
                w.bytes(b);
            }
            Cell::Media { wrapped, data } => {
                w.bool(*wrapped);
                w.i32(len_i32(data.len())?);
                w.bytes(data);
            }
        }
        Ok(())
    }

    /// `NavSerializer.ReadItem`. A compact zero (marker 3) comes back as the value
    /// .NET would produce: `0m` in Decimal columns, `Int32(0)` elsewhere.
    pub fn read(t: CellTypeCode, r: &mut BinReader) -> Result<Cell, DatasetError> {
        use CellTypeCode as T;
        match r.u8()? {
            0 => return Ok(Cell::Null),
            1 => return Ok(Cell::NotChanged),
            4 => return Ok(Cell::Lazy),
            3 => return Ok(if t == T::Decimal { Cell::Decimal(Decimal::ZERO) } else { Cell::Int32(0) }),
            _ => {}
        }
        Ok(match t {
            T::Boolean => Cell::Bool(r.bool()?),
            T::Byte => Cell::Byte(r.u8()?),
            T::Media | T::MediaSet => {
                let wrapped = r.bool()?;
                let n = r.i32()?;
                Cell::Media {
                    wrapped,
                    data: r.take(usize::try_from(n).map_err(|_| DatasetError("negative media length".into()))?)?.to_vec(),
                }
            }
            T::ByteArray => {
                let n = r.i32()?;
                Cell::Bytes(
                    r.take(usize::try_from(n).map_err(|_| DatasetError("negative byte array length".into()))?)?
                        .to_vec(),
                )
            }
            T::Char => Cell::Char(r.char()?),
            T::DateTime => {
                let ticks = r.i64()?;
                let kind = r.i32()?;
                Cell::DateTime {
                    ticks,
                    kind: DateTimeKind::from_i32(kind),
                }
            }
            T::DBNull => Cell::Null,
            T::DBLazy => Cell::Lazy,
            T::Decimal => Cell::Decimal(r.decimal()?),
            T::Double => Cell::Double(r.f64()?),
            T::Guid => Cell::Guid(r.guid()?),
            T::Int16 => Cell::Int16(r.i16()?),
            T::Int32 => Cell::Int32(r.i32()?),
            T::Int64 => Cell::Int64(r.i64()?),
            T::SByte => Cell::SByte(r.i8()?),
            T::Single => Cell::Single(r.f32()?),
            T::String => Cell::String(r.string()?),
            T::TimeSpan => Cell::TimeSpan(r.i64()?),
            T::UInt16 => Cell::UInt16(r.u16()?),
            T::UInt32 => Cell::UInt32(r.u32()?),
            T::UInt64 => Cell::UInt64(r.u64()?),
            // .NET returns null and reads nothing more
            T::Object | T::DBNotChanged => Cell::Null,
        })
    }
}

fn len_i32(n: usize) -> Result<i32, DatasetError> {
    i32::try_from(n).map_err(|_| DatasetError(format!("{n} bytes do not fit an Int32 length")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetError(pub String);

impl fmt::Display for DatasetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DatasetError {}

impl From<BinError> for DatasetError {
    fn from(e: BinError) -> Self {
        DatasetError(e.0)
    }
}

/// A typed value as used by column extended properties (`WriteTypeAndItem`).
#[derive(Debug, Clone, PartialEq)]
pub struct TypedCell {
    pub ty: CellTypeCode,
    pub cell: Cell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub ty: CellTypeCode,
    pub lazy: bool,
    /// Normally empty.
    pub ext_props: Vec<(TypedCell, TypedCell)>,
}

impl Column {
    pub fn new(name: impl Into<String>, ty: CellTypeCode) -> Column {
        Column {
            name: name.into(),
            ty,
            lazy: false,
            ext_props: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DataTable {
    pub name: String,
    pub lcid: i32,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
}

impl DataTable {
    pub fn new(name: impl Into<String>, columns: Vec<Column>) -> Result<DataTable, DatasetError> {
        for (i, c) in columns.iter().enumerate() {
            // the .NET column collection renames duplicates ("x_1"), which would break
            // lookups by name, so reject them
            if columns[..i].iter().any(|o| o.name == c.name) {
                return Err(DatasetError(format!("duplicate column name '{}'", c.name)));
            }
        }
        Ok(DataTable {
            name: name.into(),
            lcid: LCID_INVARIANT,
            columns,
            rows: vec![],
        })
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn push_row(&mut self, cells: Vec<Cell>) -> Result<(), DatasetError> {
        if cells.len() != self.columns.len() {
            return Err(DatasetError(format!(
                "row has {} cells, table '{}' has {} columns",
                cells.len(),
                self.name,
                self.columns.len()
            )));
        }
        for (c, col) in cells.iter().zip(&self.columns) {
            if !c.fits(col.ty) {
                return Err(DatasetError(format!("cell {c:?} does not fit column '{}' ({:?})", col.name, col.ty)));
            }
        }
        self.rows.push(cells);
        Ok(())
    }

    /// Cell by row index and column name.
    pub fn get(&self, row: usize, column: &str) -> Option<&Cell> {
        let ci = self.column_index(column)?;
        self.rows.get(row).map(|r| &r[ci])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NavDataSet {
    pub name: String,
    pub lcid: i32,
    pub tables: Vec<DataTable>,
}

impl Default for NavDataSet {
    fn default() -> Self {
        NavDataSet {
            name: DEFAULT_DATASET_NAME.into(),
            lcid: LCID_INVARIANT,
            tables: vec![],
        }
    }
}

impl NavDataSet {
    pub fn new() -> NavDataSet {
        NavDataSet::default()
    }

    pub fn add_table(&mut self, t: DataTable) -> Result<(), DatasetError> {
        // NavDataTableCollection.Add throws on a duplicate name (case-insensitive)
        if self.tables.iter().any(|o| o.name.to_lowercase() == t.name.to_lowercase()) {
            return Err(DatasetError(format!("duplicate table name '{}'", t.name)));
        }
        self.tables.push(t);
        Ok(())
    }

    /// `NavDataSet.WriteTo` (non-compact: marker 3 only where a cell is `Cell::Zero`).
    pub fn to_bytes(&self) -> Result<Vec<u8>, DatasetError> {
        if self.name.is_empty() {
            return Err(DatasetError("a NavDataSet name cannot be empty".into()));
        }
        let mut w = BinWriter::new();
        w.string(&self.name);
        w.i32(self.lcid);
        w.i32(len_i32(self.tables.len())?);
        for t in &self.tables {
            w.string(&t.name);
            w.i32(t.lcid);
            w.i32(len_i32(t.columns.len())?);
            for c in &t.columns {
                w.string(&c.name);
                w.u8(c.ty as u8);
                w.bool(c.lazy);
                w.i32(len_i32(c.ext_props.len())?);
                for (k, v) in &c.ext_props {
                    for tc in [k, v] {
                        Cell::Byte(tc.ty as u8).write(CellTypeCode::Byte, &mut w)?;
                        tc.cell.write(tc.ty, &mut w)?;
                    }
                }
            }
        }
        for t in &self.tables {
            w.i32(len_i32(t.rows.len())?);
            for r in &t.rows {
                if r.len() != t.columns.len() {
                    return Err(DatasetError(format!(
                        "row width {} != {} columns in table '{}'",
                        r.len(),
                        t.columns.len(),
                        t.name
                    )));
                }
                for (cell, col) in r.iter().zip(&t.columns) {
                    cell.write(col.ty, &mut w)?;
                }
            }
        }
        Ok(w.into_bytes())
    }

    /// `NavDataSet.ReadFrom` without row deduplication (the JSON-RPC setting): a
    /// `NotChanged` marker stays a literal `Cell::NotChanged`.
    pub fn from_bytes(b: &[u8]) -> Result<NavDataSet, DatasetError> {
        let mut r = BinReader::new(b);
        let name = r.string()?;
        let lcid = r.i32()?;
        let ntab = count(r.i32()?)?;
        let mut tables = Vec::with_capacity(ntab.min(64));
        for _ in 0..ntab {
            let tname = r.string()?;
            let tlcid = r.i32()?;
            let ncol = count(r.i32()?)?;
            let mut columns = Vec::with_capacity(ncol.min(1024));
            for _ in 0..ncol {
                let cname = r.string()?;
                let code = r.u8()?;
                let ty = CellTypeCode::from_u8(code).ok_or_else(|| DatasetError(format!("unknown type code {code} for column '{cname}'")))?;
                let lazy = r.bool()?;
                let nprop = r.i32()?;
                let mut ext_props = vec![];
                for _ in 0..nprop.max(0) {
                    let k = read_typed(&mut r)?;
                    let v = read_typed(&mut r)?;
                    ext_props.push((k, v));
                }
                columns.push(Column {
                    name: cname,
                    ty,
                    lazy,
                    ext_props,
                });
            }
            tables.push(DataTable {
                name: tname,
                lcid: tlcid,
                columns,
                rows: vec![],
            });
        }
        for t in &mut tables {
            let nrow = count(r.i32()?)?;
            for _ in 0..nrow {
                let mut row = Vec::with_capacity(t.columns.len());
                for c in &t.columns {
                    row.push(Cell::read(c.ty, &mut r)?);
                }
                t.rows.push(row);
            }
        }
        if r.remaining() != 0 {
            return Err(DatasetError(format!("{} trailing bytes after the data set", r.remaining())));
        }
        Ok(NavDataSet { name, lcid, tables })
    }

    /// The JSON-RPC wrapper. With `compress_above = Some(n)` the stream is sent as raw
    /// DEFLATE once it is larger than `n` bytes (what the NST does with its configured
    /// threshold); `None` never compresses.
    pub fn to_wire(&self, compress_above: Option<usize>) -> Result<WireDataSet, DatasetError> {
        let raw = self.to_bytes()?;
        let (compressed, bytes) = match compress_above {
            Some(n) if raw.len() > n => {
                let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                enc.write_all(&raw).map_err(|e| DatasetError(e.to_string()))?;
                (true, enc.finish().map_err(|e| DatasetError(e.to_string()))?)
            }
            _ => (false, raw),
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(WireDataSet {
            compressed,
            count: 1,
            buffers: vec![b64],
        })
    }

    pub fn from_wire(w: &WireDataSet) -> Result<NavDataSet, DatasetError> {
        let mut joined = vec![];
        for b in &w.buffers {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b)
                .map_err(|e| DatasetError(format!("bad base64 buffer: {e}")))?;
            joined.extend_from_slice(&bytes);
        }
        let raw = if w.compressed {
            let mut out = vec![];
            flate2::read::DeflateDecoder::new(&joined[..])
                .read_to_end(&mut out)
                .map_err(|e| DatasetError(format!("bad DEFLATE stream: {e}")))?;
            out
        } else {
            joined
        };
        NavDataSet::from_bytes(&raw)
    }

    /// Wrapper as JSON text with the property order the .NET reader needs.
    pub fn to_json_string(&self, compress_above: Option<usize>) -> Result<String, DatasetError> {
        serde_json::to_string(&self.to_wire(compress_above)?).map_err(|e| DatasetError(e.to_string()))
    }

    /// Wrapper as a `serde_json::Value`. Order-safe only because bc-nst builds
    /// serde_json with `preserve_order`; without it the keys come out sorted
    /// (`buffers` first) and the .NET reader fails.
    pub fn to_json_value(&self, compress_above: Option<usize>) -> Result<serde_json::Value, DatasetError> {
        serde_json::to_value(self.to_wire(compress_above)?).map_err(|e| DatasetError(e.to_string()))
    }

    /// A data set without tables as a JSON value: what responses such as `OpenForm`
    /// carry when they have no rows (the web server needs a non-null `DataSet`).
    pub fn empty_json() -> serde_json::Value {
        NavDataSet::new().to_json_value(None).expect("an empty data set always encodes")
    }

    pub fn from_json_value(v: &serde_json::Value) -> Result<NavDataSet, DatasetError> {
        let w: WireDataSet = serde_json::from_value(v.clone()).map_err(|e| DatasetError(format!("bad NavDataSet wrapper: {e}")))?;
        NavDataSet::from_wire(&w)
    }
}

fn count(n: i32) -> Result<usize, DatasetError> {
    usize::try_from(n).map_err(|_| DatasetError(format!("negative count {n}")))
}

fn read_typed(r: &mut BinReader) -> Result<TypedCell, DatasetError> {
    let code = match Cell::read(CellTypeCode::Byte, r)? {
        Cell::Byte(b) => b,
        other => return Err(DatasetError(format!("expected a type code byte, got {other:?}"))),
    };
    let ty = CellTypeCode::from_u8(code).ok_or_else(|| DatasetError(format!("unknown type code {code}")))?;
    Ok(TypedCell { ty, cell: Cell::read(ty, r)? })
}

/// The JSON shape of a NavDataSet on the wire. Field order is significant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireDataSet {
    pub compressed: bool,
    pub count: usize,
    pub buffers: Vec<String>,
}

impl Serialize for NavDataSet {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_wire(None).map_err(serde::ser::Error::custom)?.serialize(s)
    }
}

impl<'de> Deserialize<'de> for NavDataSet {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = WireDataSet::deserialize(d)?;
        NavDataSet::from_wire(&w).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn cell_bytes(c: Cell, t: CellTypeCode) -> Vec<u8> {
        let mut w = BinWriter::new();
        c.write(t, &mut w).unwrap();
        w.into_bytes()
    }

    #[test]
    fn cell_markers_and_payloads() {
        use CellTypeCode as T;
        assert_eq!(cell_bytes(Cell::Null, T::String), [0]);
        assert_eq!(cell_bytes(Cell::NotChanged, T::String), [1]);
        assert_eq!(cell_bytes(Cell::Zero, T::Decimal), [3]);
        assert_eq!(cell_bytes(Cell::Lazy, T::ByteArray), [4]);
        assert_eq!(cell_bytes(Cell::Bool(true), T::Boolean), [2, 1]);
        assert_eq!(cell_bytes(Cell::Bool(false), T::Boolean), [2, 0]);
        assert_eq!(cell_bytes(Cell::Int32(10000), T::Int32), [2, 0x10, 0x27, 0, 0]);
        assert_eq!(cell_bytes(Cell::Int32(-1), T::Int32), [2, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(cell_bytes(Cell::Int64(0x0102030405060708), T::Int64), [2, 8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(cell_bytes(Cell::String("C10000".into()), T::String), [2, 6, b'C', b'1', b'0', b'0', b'0', b'0']);
        assert_eq!(cell_bytes(Cell::String(String::new()), T::String), [2, 0]);
        assert_eq!(
            cell_bytes(Cell::Decimal(Decimal::from_str("123.45").unwrap()), T::Decimal),
            [2, 0x39, 0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x00]
        );
        // 2000-01-01 00:00, DateTime(2000,1,1).Ticks = 630822816000000000 = 0x08C1_2202_47E4_4000
        assert_eq!(
            cell_bytes(
                Cell::DateTime {
                    ticks: 630_822_816_000_000_000,
                    kind: DateTimeKind::Utc
                },
                T::DateTime
            ),
            [2, 0x00, 0x40, 0xE4, 0x47, 0x02, 0x22, 0xC1, 0x08, 1, 0, 0, 0]
        );
        assert_eq!(cell_bytes(Cell::TimeSpan(10_000), T::TimeSpan), [2, 0x10, 0x27, 0, 0, 0, 0, 0, 0]);
        assert_eq!(cell_bytes(Cell::Bytes(vec![0xAB, 0xCD]), T::ByteArray), [2, 2, 0, 0, 0, 0xAB, 0xCD]);
        assert_eq!(cell_bytes(Cell::Media { wrapped: true, data: vec![9] }, T::Media), [2, 1, 1, 0, 0, 0, 9]);
        let g = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        assert_eq!(
            cell_bytes(Cell::Guid(g), T::Guid),
            [2, 0x67, 0x45, 0x23, 0x01, 0xAB, 0x89, 0xEF, 0xCD, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]
        );
        // wrong variant for the column
        assert!(Cell::String("x".into()).write(T::Int32, &mut BinWriter::new()).is_err());
        assert!(Cell::Zero.write(T::String, &mut BinWriter::new()).is_err());
    }

    #[test]
    fn compact_zero_reads_like_dotnet() {
        assert_eq!(
            Cell::read(CellTypeCode::Decimal, &mut BinReader::new(&[3])).unwrap(),
            Cell::Decimal(Decimal::ZERO)
        );
        assert_eq!(Cell::read(CellTypeCode::Int64, &mut BinReader::new(&[3])).unwrap(), Cell::Int32(0));
        // markers >= 5 are read like 2
        assert_eq!(Cell::read(CellTypeCode::Int32, &mut BinReader::new(&[7, 5, 0, 0, 0])).unwrap(), Cell::Int32(5));
    }

    /// Hand-assembled stream: one table "18" with columns "0" (Int64) and "1" (String),
    /// one row (7, "AB").
    #[test]
    fn whole_stream_layout() {
        let mut t = DataTable::new("18", vec![Column::new("0", CellTypeCode::Int64), Column::new("1", CellTypeCode::String)]).unwrap();
        t.push_row(vec![Cell::Int64(7), Cell::String("AB".into())]).unwrap();
        let mut ds = NavDataSet::new();
        ds.add_table(t).unwrap();
        let expect: Vec<u8> = [
            &[10u8, b'N', b'e', b'w', b'D', b'a', b't', b'a', b'S', b'e', b't'][..], // name "NewDataSet"
            &[127, 0, 0, 0],                                                         // LCID 127
            &[1, 0, 0, 0],                                                           // 1 table
            &[2, b'1', b'8'],                                                        // table name "18"
            &[127, 0, 0, 0],                                                         // table LCID
            &[2, 0, 0, 0],                                                           // 2 columns
            &[1, b'0', 10, 0, 0, 0, 0, 0],                                           // "0" Int64, not lazy, 0 ext props
            &[1, b'1', 17, 0, 0, 0, 0, 0],                                           // "1" String, not lazy, 0 ext props
            &[1, 0, 0, 0],                                                           // 1 row
            &[2, 7, 0, 0, 0, 0, 0, 0, 0],                                            // Int64 7
            &[2, 2, b'A', b'B'],                                                     // "AB"
        ]
        .concat();
        assert_eq!(ds.to_bytes().unwrap(), expect);
        assert_eq!(NavDataSet::from_bytes(&expect).unwrap(), ds);
    }

    #[test]
    fn empty_set() {
        let v = NavDataSet::empty_json();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["compressed", "count", "buffers"]);
        let b = base64::engine::general_purpose::STANDARD.decode(v["buffers"][0].as_str().unwrap()).unwrap();
        // "NewDataSet", LCID 127, 0 tables
        assert_eq!(b, [&[10u8][..], b"NewDataSet", &[127, 0, 0, 0], &[0, 0, 0, 0]].concat());
        assert_eq!(NavDataSet::from_json_value(&v).unwrap(), NavDataSet::new());
    }

    #[test]
    fn ext_props_round_trip() {
        let mut c = Column::new("x", CellTypeCode::String);
        c.ext_props.push((
            TypedCell {
                ty: CellTypeCode::String,
                cell: Cell::String("k".into()),
            },
            TypedCell {
                ty: CellTypeCode::Int32,
                cell: Cell::Int32(3),
            },
        ));
        let mut ds = NavDataSet::new();
        ds.add_table(DataTable::new("t", vec![c]).unwrap()).unwrap();
        let b = ds.to_bytes().unwrap();
        // ... "x" String lazy=0, count 1, then [2,17] (type code String as Byte cell), [2,1,'k'], [2,8], [2,3,0,0,0]
        let tail = [1u8, 0, 0, 0, 2, 17, 2, 1, b'k', 2, 8, 2, 3, 0, 0, 0, 0, 0, 0, 0];
        assert!(b.ends_with(&tail), "{b:?}");
        assert_eq!(NavDataSet::from_bytes(&b).unwrap(), ds);
    }

    #[test]
    fn duplicates_rejected() {
        assert!(DataTable::new("t", vec![Column::new("a", CellTypeCode::String), Column::new("a", CellTypeCode::Int32)]).is_err());
        let mut ds = NavDataSet::new();
        ds.add_table(DataTable::new("Tab", vec![]).unwrap()).unwrap();
        assert!(ds.add_table(DataTable::new("tab", vec![]).unwrap()).is_err());
    }

    #[test]
    fn wrapper_order_and_compression() {
        let mut t = DataTable::new("18", vec![Column::new("1", CellTypeCode::String)]).unwrap();
        for i in 0..200 {
            t.push_row(vec![Cell::String(format!("row {i} with some repeated text"))]).unwrap();
        }
        let mut ds = NavDataSet::new();
        ds.add_table(t).unwrap();

        let plain = ds.to_json_string(None).unwrap();
        assert!(plain.starts_with(r#"{"compressed":false,"count":1,"buffers":[""#), "{plain}");
        let via_serde = serde_json::to_string(&ds).unwrap();
        assert_eq!(via_serde, plain);
        // through serde_json::Value (needs serde_json/preserve_order)
        let via_value = serde_json::to_string(&serde_json::json!({ "dataSet": ds.to_json_value(None).unwrap() })).unwrap();
        assert!(via_value.starts_with(r#"{"dataSet":{"compressed":false,"count":1,"buffers":["#), "{via_value}");

        let packed = ds.to_wire(Some(256)).unwrap();
        assert!(packed.compressed);
        assert!(packed.buffers[0].len() < ds.to_wire(None).unwrap().buffers[0].len());
        // raw DEFLATE, no zlib header: a zlib stream would start with 0x78
        let raw = base64::engine::general_purpose::STANDARD.decode(&packed.buffers[0]).unwrap();
        assert_ne!(raw[0], 0x78);
        assert_eq!(NavDataSet::from_wire(&packed).unwrap(), ds);

        // several buffers are concatenated before decoding
        let full = base64::engine::general_purpose::STANDARD.decode(&ds.to_wire(None).unwrap().buffers[0]).unwrap();
        let (a, b) = full.split_at(17);
        let split = WireDataSet {
            compressed: false,
            count: 2,
            buffers: vec![
                base64::engine::general_purpose::STANDARD.encode(a),
                base64::engine::general_purpose::STANDARD.encode(b),
            ],
        };
        assert_eq!(NavDataSet::from_wire(&split).unwrap(), ds);
        let back: NavDataSet = serde_json::from_str(&plain).unwrap();
        assert_eq!(back, ds);
    }
}
