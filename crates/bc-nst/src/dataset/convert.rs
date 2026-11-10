//! bc-rust values → NavDataSet cells.
//!
//! | AL type            | column     | cell                                                      |
//! |--------------------|------------|-----------------------------------------------------------|
//! | Code, Text         | String     | stored text, blank = `""`                                 |
//! | Integer            | Int32      |                                                           |
//! | BigInteger         | Int64      |                                                           |
//! | Decimal            | Decimal    | scale kept                                                |
//! | Boolean            | Boolean    |                                                           |
//! | Option, Enum       | Int32      | ordinal                                                   |
//! | Date               | DateTime   | midnight, kind Unspecified; closing date = 23:59:59.000; `0D` = ticks 0 |
//! | Time               | DateTime   | 0001-01-02 + time of day, kind Unspecified; `0T` = ticks 0 |
//! | DateTime           | DateTime   | UTC ticks, kind Utc; `0DT` = ticks 0 (kind Unspecified)    |
//! | Duration           | TimeSpan   | ticks                                                     |
//! | Guid               | Guid       |                                                           |
//! | DateFormula, TableFilter, other | String |                                                |
//! | RecordId           | ByteArray  | NavRecordId's binary form ([`record_id_bytes`])           |
//! | BLOB               | ByteArray  |                                                           |
//! | Media, MediaSet    | (no column, unconfirmed)                                               |
//!
//! bc-rust's own encodings (bc_store::value): Date = days since 0001-01-01 (CE day
//! number) times two, plus one for a closing date, 0 = undefined; Time = ms since
//! midnight plus one, 0 = undefined; DateTime = ms since the Unix epoch,
//! `i64::MIN` = undefined; Duration = ms.

use super::navdataset::{Cell, Column, DatasetError, DateTimeKind, CellTypeCode};
use al_model::{FieldDef, FieldType};
use al_runtime::Val;
use bc_store::{Value, DATETIME_UNDEFINED};

pub const TICKS_PER_MS: i64 = 10_000;
pub const TICKS_PER_SECOND: i64 = 10_000_000;
pub const TICKS_PER_DAY: i64 = 864_000_000_000;
/// `DateTime.UnixEpoch.Ticks`.
pub const UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;
/// Time of day of a closing date: 23:59:59.000.
pub const CLOSING_TIME_TICKS: i64 = (23 * 3600 + 59 * 60 + 59) * TICKS_PER_SECOND;

/// bc-rust Date → .NET ticks.
pub fn date_to_ticks(d: i32) -> i64 {
    if d <= 0 {
        return 0;
    }
    let day = (d / 2) as i64 - 1; // CE day 1 = 0001-01-01 = tick 0
    let t = day * TICKS_PER_DAY;
    if d % 2 == 1 {
        t + CLOSING_TIME_TICKS
    } else {
        t
    }
}

/// .NET ticks → bc-rust Date (time of day 23:59:59.000 means a closing date).
pub fn ticks_to_date(ticks: i64) -> i32 {
    if ticks <= 0 {
        return 0;
    }
    let day = ticks / TICKS_PER_DAY;
    let tod = ticks % TICKS_PER_DAY;
    ((day + 1) * 2) as i32 + (tod == CLOSING_TIME_TICKS) as i32
}

/// bc-rust Time → .NET ticks on 0001-01-02.
pub fn time_to_ticks(t: i32) -> i64 {
    if t <= 0 {
        return 0;
    }
    TICKS_PER_DAY + (t as i64 - 1) * TICKS_PER_MS
}

/// .NET ticks → bc-rust Time (the date part is ignored).
pub fn ticks_to_time(ticks: i64) -> i32 {
    if ticks <= 0 {
        return 0;
    }
    ((ticks % TICKS_PER_DAY) / TICKS_PER_MS) as i32 + 1
}

/// bc-rust DateTime (Unix ms) → .NET UTC ticks.
pub fn datetime_to_ticks(ms: i64) -> i64 {
    if ms == DATETIME_UNDEFINED {
        return 0;
    }
    ms.saturating_mul(TICKS_PER_MS).saturating_add(UNIX_EPOCH_TICKS).max(0)
}

pub fn ticks_to_datetime(ticks: i64) -> i64 {
    if ticks <= 0 {
        return DATETIME_UNDEFINED;
    }
    (ticks - UNIX_EPOCH_TICKS).div_euclid(TICKS_PER_MS)
}

pub fn duration_to_ticks(ms: i64) -> i64 {
    ms.saturating_mul(TICKS_PER_MS)
}

pub fn date_cell(d: i32) -> Cell {
    Cell::DateTime {
        ticks: date_to_ticks(d),
        kind: DateTimeKind::Unspecified,
    }
}

pub fn time_cell(t: i32) -> Cell {
    Cell::DateTime {
        ticks: time_to_ticks(t),
        kind: DateTimeKind::Unspecified,
    }
}

pub fn datetime_cell(ms: i64) -> Cell {
    if ms == DATETIME_UNDEFINED {
        // DateTime.MinValue; Unspecified so the client never converts it to local time
        Cell::DateTime {
            ticks: 0,
            kind: DateTimeKind::Unspecified,
        }
    } else {
        Cell::DateTime {
            ticks: datetime_to_ticks(ms),
            kind: DateTimeKind::Utc,
        }
    }
}

/// Column type for a table field, `None` for types left out of the data set
/// (Media, MediaSet).
pub fn field_type_code(fd: &FieldDef) -> Option<CellTypeCode> {
    Some(match &fd.ty {
        FieldType::Integer | FieldType::Option | FieldType::Enum(_) => CellTypeCode::Int32,
        FieldType::BigInteger => CellTypeCode::Int64,
        FieldType::Decimal => CellTypeCode::Decimal,
        FieldType::Boolean => CellTypeCode::Boolean,
        FieldType::Code(_) | FieldType::Text(_) | FieldType::DateFormula | FieldType::TableFilter | FieldType::Other(_) => CellTypeCode::String,
        FieldType::Date | FieldType::Time | FieldType::DateTime => CellTypeCode::DateTime,
        FieldType::Duration => CellTypeCode::TimeSpan,
        FieldType::Guid => CellTypeCode::Guid,
        // the client types RecordID fields as Byte[] (MetaField ClrType) and fails the whole
        // session on a String cell ("Invalid cast from 'System.String' to 'System.Byte[]'")
        FieldType::Blob | FieldType::RecordId => CellTypeCode::ByteArray,
        FieldType::Media | FieldType::MediaSet => return None,
    })
}

/// Column named by the field number (`MetaField.DataColumnName`).
pub fn field_column(fd: &FieldDef) -> Option<Column> {
    field_type_code(fd).map(|t| Column::new(fd.id.to_string(), t))
}

/// Stored value of field `fd` → cell for its column.
pub fn field_cell(fd: &FieldDef, v: &Value) -> Result<Cell, DatasetError> {
    let t = field_type_code(fd).ok_or_else(|| DatasetError(format!("field {} ({:?}) has no data set column", fd.name, fd.ty)))?;
    value_cell(t, &fd.ty, v)
}

/// Stored value → cell for a column of type `t`; `ty` tells Date, Time and DateTime
/// apart for numeric store values.
pub fn value_cell(t: CellTypeCode, ty: &FieldType, v: &Value) -> Result<Cell, DatasetError> {
    use CellTypeCode as T;
    let bad = || DatasetError(format!("stored value {v:?} does not fit a {t:?} column ({ty:?})"));
    Ok(match (t, v) {
        (T::Int32, Value::Int(i)) | (T::Int32, Value::Option(i)) => Cell::Int32(*i),
        (T::Int64, Value::BigInt(i)) => Cell::Int64(*i),
        (T::Int64, Value::Int(i)) => Cell::Int64(*i as i64),
        (T::Decimal, Value::Dec(d)) => Cell::Decimal(*d),
        (T::Boolean, Value::Bool(b)) => Cell::Bool(*b),
        (T::String, Value::Text(s)) => Cell::String(s.clone()),
        (T::DateTime, Value::Date(d)) => date_cell(*d),
        (T::DateTime, Value::Time(x)) => time_cell(*x),
        (T::DateTime, Value::DateTime(ms)) => datetime_cell(*ms),
        (T::TimeSpan, Value::Duration(ms)) => Cell::TimeSpan(duration_to_ticks(*ms)),
        (T::Guid, Value::Guid(g)) => Cell::Guid(*g),
        (T::ByteArray, Value::Blob(b)) => Cell::Bytes(b.clone()),
        (T::ByteArray, Value::Text(s)) if matches!(ty, FieldType::RecordId) => Cell::Bytes(record_id_bytes(s)),
        // a column typed from the field but a differently stored value (e.g. a
        // schema change): fall back on the declared AL type
        (T::DateTime, Value::Int(i)) => match ty {
            FieldType::Time => time_cell(*i),
            _ => date_cell(*i),
        },
        _ => return Err(bad()),
    })
}

/// Column type for a runtime value (expression columns whose type is only known
/// from the value).
pub fn val_type_code(v: &Val) -> Option<CellTypeCode> {
    Some(match v {
        Val::Bool(_) => CellTypeCode::Boolean,
        Val::Int(_) | Val::Opt { .. } => CellTypeCode::Int32,
        Val::BigInt(_) => CellTypeCode::Int64,
        Val::Dec(_) => CellTypeCode::Decimal,
        Val::Text(_) | Val::Char(_) | Val::DateFormula(_) => CellTypeCode::String,
        Val::Date(_) | Val::Time(_) | Val::DateTime(_) => CellTypeCode::DateTime,
        Val::Duration(_) => CellTypeCode::TimeSpan,
        Val::Guid(_) => CellTypeCode::Guid,
        _ => return None,
    })
}

/// Runtime value → cell for a column of type `t`. Numbers are widened or narrowed
/// to the column type, anything goes into a String column as its text form, and
/// `Void` is DBNull.
pub fn val_cell(t: CellTypeCode, v: &Val) -> Result<Cell, DatasetError> {
    use CellTypeCode as T;
    let bad = || DatasetError(format!("value {v:?} ({}) does not fit a {t:?} column", v.type_name()));
    if matches!(v, Val::Void) {
        return Ok(Cell::Null);
    }
    Ok(match (t, v) {
        (T::Boolean, Val::Bool(b)) => Cell::Bool(*b),
        (T::Int32, Val::Int(i)) => Cell::Int32(*i),
        (T::Int32, Val::Opt { ord, .. }) => Cell::Int32(*ord),
        (T::Int32, Val::BigInt(i)) => Cell::Int32(i32::try_from(*i).map_err(|_| bad())?),
        (T::Int64, Val::BigInt(i)) => Cell::Int64(*i),
        (T::Int64, Val::Int(i)) => Cell::Int64(*i as i64),
        (T::Int64, Val::Opt { ord, .. }) => Cell::Int64(*ord as i64),
        (T::Decimal, Val::Dec(_) | Val::Int(_) | Val::BigInt(_)) => Cell::Decimal(v.as_dec()),
        (T::DateTime, Val::Date(d)) => date_cell(*d),
        (T::DateTime, Val::Time(x)) => time_cell(*x),
        (T::DateTime, Val::DateTime(ms)) => datetime_cell(*ms),
        (T::TimeSpan, Val::Duration(ms)) => Cell::TimeSpan(duration_to_ticks(*ms)),
        (T::Guid, Val::Guid(g)) => Cell::Guid(*g),
        (T::Char, Val::Char(c)) => Cell::Char(*c),
        (T::String, Val::Text(s) | Val::DateFormula(s)) => Cell::String(s.clone()),
        (T::String, Val::Char(c)) => Cell::String(c.to_string()),
        (T::String, other) => Cell::String(other.to_text()),
        _ => return Err(bad()),
    })
}

/// A cell the client sent for field `fd` (a record data set in ValidateField,
/// ModifyRecord, ...) → runtime value, ready for `Validate` or `to_store`. `None` for
/// cells that carry no value: DBNull, DBLazy, DBNotChanged, BLOB / Media payloads.
pub fn cell_val(fd: &FieldDef, c: &Cell) -> Option<Val> {
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    let num = |c: &Cell| -> Option<Decimal> {
        Some(match c {
            Cell::Zero => Decimal::ZERO,
            Cell::Int32(i) => Decimal::from(*i),
            Cell::Int64(i) => Decimal::from(*i),
            Cell::Int16(i) => Decimal::from(*i),
            Cell::Byte(i) => Decimal::from(*i),
            Cell::Decimal(d) => *d,
            Cell::Double(f) => Decimal::try_from(*f).ok()?,
            Cell::Single(f) => Decimal::try_from(*f as f64).ok()?,
            Cell::String(s) => s.trim().parse().ok()?,
            _ => return None,
        })
    };
    let ticks = |c: &Cell| match c {
        Cell::DateTime { ticks, .. } => Some(*ticks),
        _ => None,
    };
    Some(match &fd.ty {
        FieldType::Integer | FieldType::Option | FieldType::Enum(_) => Val::Int(num(c)?.to_i32()?),
        FieldType::BigInteger => Val::BigInt(num(c)?.to_i64()?),
        FieldType::Decimal => Val::Dec(num(c)?),
        FieldType::Boolean => match c {
            Cell::Bool(b) => Val::Bool(*b),
            Cell::Zero => Val::Bool(false),
            _ => return None,
        },
        FieldType::RecordId => match c {
            // blank or a key we cannot read: keep the stored value (the client never edits one)
            Cell::Bytes(b) => {
                let (t, k) = record_id_from_bytes(b)?;
                Val::RecordId(t, k)
            }
            Cell::String(s) => Val::Text(s.clone()),
            _ => return None,
        },
        FieldType::Code(_) | FieldType::Text(_) | FieldType::DateFormula | FieldType::TableFilter | FieldType::Other(_) => match c {
            Cell::String(s) => Val::Text(s.clone()),
            Cell::Char(ch) => Val::Text(ch.to_string()),
            _ => return None,
        },
        FieldType::Date => Val::Date(ticks_to_date(ticks(c)?)),
        FieldType::Time => Val::Time(ticks_to_time(ticks(c)?)),
        FieldType::DateTime => Val::DateTime(ticks_to_datetime(ticks(c)?)),
        FieldType::Duration => match c {
            Cell::TimeSpan(t) => Val::Duration(t / TICKS_PER_MS),
            Cell::Zero => Val::Duration(0),
            _ => return None,
        },
        FieldType::Guid => match c {
            Cell::Guid(g) => Val::Guid(*g),
            _ => return None,
        },
        FieldType::Blob | FieldType::Media | FieldType::MediaSet => return None,
    })
}

// NavType numbers of RecordID key values
const NT_CODE: u16 = 31490;
const NT_TEXT: u16 = 31489;
const NT_INTEGER: u16 = 34560;
const NT_OPTION: u16 = 35584;
const NT_BIGINTEGER: u16 = 36096;
const NT_BOOLEAN: u16 = 34048;
const NT_GUID: u16 = 37120;

/// A stored RecordID (`<table id>:<tagged key values>`, al_runtime::record) in NavRecordId's
/// binary form: table no. (i32), then per key value its NavType (u16) and bytes, then a u16 0.
/// Code/Text are UTF-16LE with a two-byte terminator, Integer/Option/Boolean four bytes,
/// BigInteger eight, GUID sixteen (.NET order). Blank is six zero bytes. Text keys go out as
/// Code (both decode alike). A key of another type (Date, Time, DateTime, Duration, Decimal)
/// is not encoded here; the field then goes out blank.
pub fn record_id_bytes(stored: &str) -> Vec<u8> {
    let blank = vec![0u8; 6];
    let Some((table, key)) = al_runtime::record::decode_record_id(stored) else { return blank };
    let Ok(t) = i32::try_from(table) else { return blank };
    let mut out = t.to_le_bytes().to_vec();
    for v in &key {
        match v {
            Value::Text(s) => {
                out.extend_from_slice(&NT_CODE.to_le_bytes());
                for u in s.encode_utf16() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out.extend_from_slice(&[0, 0]);
            }
            Value::Int(i) => {
                out.extend_from_slice(&NT_INTEGER.to_le_bytes());
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Option(i) => {
                out.extend_from_slice(&NT_OPTION.to_le_bytes());
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::BigInt(i) => {
                out.extend_from_slice(&NT_BIGINTEGER.to_le_bytes());
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Bool(b) => {
                out.extend_from_slice(&NT_BOOLEAN.to_le_bytes());
                out.extend_from_slice(&(*b as i32).to_le_bytes());
            }
            Value::Guid(g) => {
                out.extend_from_slice(&NT_GUID.to_le_bytes());
                out.extend_from_slice(&super::netbin::guid_to_dotnet(g));
            }
            _ => return blank,
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// The client's NavRecordId bytes → table id and key values; `None` for a blank RecordID
/// and for key types [`record_id_bytes`] does not write.
pub fn record_id_from_bytes(b: &[u8]) -> Option<(i64, Vec<Value>)> {
    let i32_at = |p: usize| b.get(p..p + 4).map(|x| i32::from_le_bytes(x.try_into().unwrap()));
    let table = i32_at(0)?;
    if table == 0 {
        return None;
    }
    let mut p = 4;
    let mut key = vec![];
    loop {
        let ty = u16::from_le_bytes(b.get(p..p + 2)?.try_into().ok()?);
        p += 2;
        match ty {
            0 => return Some((table as i64, key)),
            NT_CODE | NT_TEXT => {
                let mut units = vec![];
                loop {
                    let u = u16::from_le_bytes(b.get(p..p + 2)?.try_into().ok()?);
                    p += 2;
                    if u == 0 {
                        break;
                    }
                    units.push(u);
                }
                key.push(Value::Text(String::from_utf16(&units).ok()?));
            }
            NT_INTEGER => {
                key.push(Value::Int(i32_at(p)?));
                p += 4;
            }
            NT_OPTION => {
                key.push(Value::Option(i32_at(p)?));
                p += 4;
            }
            NT_BOOLEAN => {
                key.push(Value::Bool(i32_at(p)? != 0));
                p += 4;
            }
            NT_BIGINTEGER => {
                key.push(Value::BigInt(i64::from_le_bytes(b.get(p..p + 8)?.try_into().ok()?)));
                p += 8;
            }
            NT_GUID => {
                let g: [u8; 16] = b.get(p..p + 16)?.try_into().ok()?;
                key.push(Value::Guid(super::netbin::guid_from_dotnet(&g)));
                p += 16;
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_runtime::value::date_from_ymd;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    // expected tick values computed independently (Python datetime arithmetic)
    const T_2024_01_15: i64 = 638_408_736_000_000_000;
    const T_2024_01_15_CLOSING: i64 = 638_409_599_990_000_000;

    #[test]
    fn dates() {
        let d = date_from_ymd(2024, 1, 15).unwrap();
        assert_eq!(d, 738_900 * 2);
        assert_eq!(date_to_ticks(d), T_2024_01_15);
        assert_eq!(date_to_ticks(d + 1), T_2024_01_15_CLOSING);
        assert_eq!(date_to_ticks(date_from_ymd(1, 1, 1).unwrap()), 0);
        assert_eq!(date_to_ticks(date_from_ymd(2000, 1, 1).unwrap()), 630_822_816_000_000_000);
        assert_eq!(date_to_ticks(0), 0);
        assert_eq!(ticks_to_date(T_2024_01_15), d);
        assert_eq!(ticks_to_date(T_2024_01_15_CLOSING), d + 1);
        assert_eq!(ticks_to_date(0), 0);
        let mut w = super::super::netbin::BinWriter::new();
        date_cell(d).write(CellTypeCode::DateTime, &mut w).unwrap();
        assert_eq!(w.into_bytes(), [2, 0, 64, 201, 234, 92, 21, 220, 8, 0, 0, 0, 0]);
        let mut w = super::super::netbin::BinWriter::new();
        date_cell(d + 1).write(CellTypeCode::DateTime, &mut w).unwrap();
        assert_eq!(w.into_bytes(), [2, 128, 105, 154, 20, 38, 22, 220, 8, 0, 0, 0, 0]);
    }

    #[test]
    fn times() {
        // 13:45:30.250 → 0001-01-02 13:45:30.250 = 1359302500000 ticks
        let t = (13 * 3_600_000 + 45 * 60_000 + 30_000 + 250) + 1;
        assert_eq!(time_to_ticks(t), 1_359_302_500_000);
        assert_eq!(ticks_to_time(1_359_302_500_000), t);
        assert_eq!(time_to_ticks(1), TICKS_PER_DAY); // 00:00:00.000
        assert_eq!(time_to_ticks(0), 0);
        let mut w = super::super::netbin::BinWriter::new();
        time_cell(t).write(CellTypeCode::DateTime, &mut w).unwrap();
        assert_eq!(w.into_bytes(), [2, 160, 30, 190, 124, 60, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn datetimes_and_durations() {
        // 2024-01-15T09:30:00Z = 1705311000000 ms
        assert_eq!(datetime_to_ticks(1_705_311_000_000), 638_409_078_000_000_000);
        assert_eq!(ticks_to_datetime(638_409_078_000_000_000), 1_705_311_000_000);
        assert_eq!(datetime_to_ticks(0), UNIX_EPOCH_TICKS);
        assert_eq!(
            datetime_cell(DATETIME_UNDEFINED),
            Cell::DateTime {
                ticks: 0,
                kind: DateTimeKind::Unspecified
            }
        );
        let mut w = super::super::netbin::BinWriter::new();
        datetime_cell(1_705_311_000_000).write(CellTypeCode::DateTime, &mut w).unwrap();
        assert_eq!(w.into_bytes(), [2, 0, 28, 147, 139, 172, 21, 220, 8, 1, 0, 0, 0]);
        // 90 minutes
        let mut w = super::super::netbin::BinWriter::new();
        Cell::TimeSpan(duration_to_ticks(90 * 60_000)).write(CellTypeCode::TimeSpan, &mut w).unwrap();
        assert_eq!(w.into_bytes(), [2, 0, 156, 166, 146, 12, 0, 0, 0]);
    }

    #[test]
    fn values_and_vals() {
        let ty = FieldType::Decimal;
        let d = Decimal::from_str("12.50").unwrap();
        assert_eq!(value_cell(CellTypeCode::Decimal, &ty, &Value::Dec(d)).unwrap(), Cell::Decimal(d));
        assert_eq!(value_cell(CellTypeCode::Int32, &FieldType::Option, &Value::Option(2)).unwrap(), Cell::Int32(2));
        assert_eq!(
            value_cell(CellTypeCode::String, &FieldType::Code(20), &Value::Text(String::new())).unwrap(),
            Cell::String(String::new())
        );
        assert!(value_cell(CellTypeCode::Int32, &FieldType::Integer, &Value::Text("x".into())).is_err());
        assert_eq!(val_cell(CellTypeCode::Int32, &Val::Opt { ord: 3, enum_id: Some(1) }).unwrap(), Cell::Int32(3));
        assert_eq!(val_cell(CellTypeCode::Decimal, &Val::Int(4)).unwrap(), Cell::Decimal(Decimal::from(4)));
        assert_eq!(val_cell(CellTypeCode::String, &Val::Int(4)).unwrap(), Cell::String("4".into()));
        assert_eq!(val_cell(CellTypeCode::Boolean, &Val::Void).unwrap(), Cell::Null);
        assert!(val_cell(CellTypeCode::Boolean, &Val::Int(1)).is_err());
        assert_eq!(val_type_code(&Val::Date(2)), Some(CellTypeCode::DateTime));
    }

    /// RecordID fields travel as NavRecordId bytes: the web client types them `Byte[]` and
    /// dropped the session on a String cell (E-Document factbox 6187, field 2 "Document
    /// Record ID", on Customer Ledger Entries opened from a role center cue).
    #[test]
    fn record_ids() {
        let ty = FieldType::RecordId;
        assert_eq!(value_cell(CellTypeCode::ByteArray, &ty, &Value::Text(String::new())).unwrap(), Cell::Bytes(vec![0; 6]));
        // Customer 10000: table 18, Code "10000"
        let mut want = 18i32.to_le_bytes().to_vec();
        want.extend_from_slice(&31490u16.to_le_bytes());
        for u in "10000".encode_utf16() {
            want.extend_from_slice(&u.to_le_bytes());
        }
        want.extend_from_slice(&[0, 0, 0, 0]);
        let stored = al_runtime::record::encode_record_id(18, &[Value::Text("10000".into())]);
        assert_eq!(record_id_bytes(&stored), want);
        assert_eq!(record_id_from_bytes(&want), Some((18, vec![Value::Text("10000".into())])));
        // several key types round-trip
        let g = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let key = vec![Value::Text("A,B".into()), Value::Int(-7), Value::Option(2), Value::BigInt(1 << 40), Value::Bool(true), Value::Guid(g)];
        let b = record_id_bytes(&al_runtime::record::encode_record_id(6121, &key));
        assert_eq!(record_id_from_bytes(&b), Some((6121, key)));
        // blank and unsupported keys
        assert_eq!(record_id_from_bytes(&[0; 6]), None);
        assert_eq!(record_id_from_bytes(&[]), None);
        assert_eq!(record_id_bytes(&al_runtime::record::encode_record_id(17, &[Value::Date(4)])), vec![0; 6]);
    }
}
