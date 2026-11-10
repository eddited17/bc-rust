//! Record bookmarks: the `bookmark` column of a row and the bytes the web server
//! hands back as `StartingBookmark`, `NavRecordState.CurrentRecord`,
//! `SelectedRecords`, deep links, ...
//!
//! The web server never looks inside. It wraps the bytes in `NavBookmark`, whose
//! string key is `ByteArrayHelper.ToUrlSafeString`: the length in hex, `_`, and
//! URL-safe base64 of the bytes with trailing zero bytes trimmed. Parsing pads the
//! zeros back, so the bytes round-trip, but an empty or all-zero array has an empty
//! key and `new Bookmark(byte[])` throws. Every bookmark here starts with a non-zero
//! magic byte.
//!
//! bc-rust format (version 1):
//!
//! ```text
//! 0xBC  magic
//! 0x01  version
//! var   table id      (7-bit encoded, like .NET Write7BitEncodedInt)
//! u8    n             number of primary key values
//! n x   tag:u8 payload
//!       1 Int i32 | 2 BigInt i64 | 3 Decimal (16 bytes, .NET layout) | 4 Bool u8
//!       5 Text (.NET string) | 6 Date i32 | 7 Time i32 | 8 DateTime i64
//!       9 Duration i64 | 10 Guid (16 bytes, textual order) | 11 Option i32
//!       12 Blob (7-bit length + bytes)
//! ```
//!
//! Integers are little endian and the values are bc-store's own encodings, so a
//! bookmark is stable as long as the stored primary key is.

use super::netbin::{BinReader, BinWriter};
use bc_store::{ColType, Row, TableSchema, Value};
use std::fmt;

pub const BOOKMARK_MAGIC: u8 = 0xBC;
pub const BOOKMARK_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkError(pub String);

impl fmt::Display for BookmarkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BookmarkError {}

fn tag(v: &Value) -> u8 {
    match v {
        Value::Int(_) => 1,
        Value::BigInt(_) => 2,
        Value::Dec(_) => 3,
        Value::Bool(_) => 4,
        Value::Text(_) => 5,
        Value::Date(_) => 6,
        Value::Time(_) => 7,
        Value::DateTime(_) => 8,
        Value::Duration(_) => 9,
        Value::Guid(_) => 10,
        Value::Option(_) => 11,
        Value::Blob(_) => 12,
    }
}

/// Encode primary key values (in `schema.keys[0]` order).
pub fn encode_pk(table_id: i64, pk: &[Value]) -> Vec<u8> {
    let mut w = BinWriter::new();
    w.u8(BOOKMARK_MAGIC);
    w.u8(BOOKMARK_VERSION);
    w.var_u32(table_id as u32);
    w.u8(pk.len() as u8);
    for v in pk {
        w.u8(tag(v));
        match v {
            Value::Int(i) | Value::Date(i) | Value::Time(i) | Value::Option(i) => w.i32(*i),
            Value::BigInt(i) | Value::DateTime(i) | Value::Duration(i) => w.i64(*i),
            Value::Dec(d) => w.decimal(*d),
            Value::Bool(b) => w.bool(*b),
            Value::Text(s) => w.string(s),
            Value::Guid(g) => w.bytes(g),
            Value::Blob(b) => {
                w.var_u32(b.len() as u32);
                w.bytes(b);
            }
        }
    }
    w.into_bytes()
}

/// Bookmark of a full row of `schema`.
pub fn encode_bookmark(schema: &TableSchema, row: &Row) -> Vec<u8> {
    let pk: Vec<Value> = schema.keys[0].iter().map(|&c| row[c].clone()).collect();
    encode_pk(schema.id, &pk)
}

fn fits(t: ColType, v: &Value) -> bool {
    matches!(
        (t, v),
        (ColType::Int, Value::Int(_))
            | (ColType::BigInt, Value::BigInt(_))
            | (ColType::Dec, Value::Dec(_))
            | (ColType::Bool, Value::Bool(_))
            | (ColType::Code | ColType::Text, Value::Text(_))
            | (ColType::Date, Value::Date(_))
            | (ColType::Time, Value::Time(_))
            | (ColType::DateTime, Value::DateTime(_))
            | (ColType::Duration, Value::Duration(_))
            | (ColType::Guid, Value::Guid(_))
            | (ColType::Option, Value::Option(_))
            | (ColType::Blob, Value::Blob(_))
    )
}

/// Primary key values (in `schema.keys[0]` order) of a bookmark for `schema`.
pub fn decode_bookmark(schema: &TableSchema, b: &[u8]) -> Result<Row, BookmarkError> {
    let e = |m: &str| BookmarkError(format!("invalid bookmark for table {}: {m}", schema.id));
    let mut r = BinReader::new(b);
    if r.u8().ok() != Some(BOOKMARK_MAGIC) {
        return Err(e("not a bc-rust bookmark"));
    }
    match r.u8() {
        Ok(BOOKMARK_VERSION) => {}
        Ok(v) => return Err(e(&format!("unsupported version {v}"))),
        Err(_) => return Err(e("truncated")),
    }
    let tid = r.var_u32().map_err(|_| e("truncated"))?;
    if tid as i64 != schema.id {
        return Err(e(&format!("it belongs to table {tid}")));
    }
    let n = r.u8().map_err(|_| e("truncated"))? as usize;
    let pk_cols = &schema.keys[0];
    if n != pk_cols.len() {
        return Err(e(&format!("{n} key values, the primary key has {}", pk_cols.len())));
    }
    let mut out = Vec::with_capacity(n);
    for &c in pk_cols {
        let t = r.u8().map_err(|_| e("truncated"))?;
        let v = (|| -> Result<Value, super::netbin::BinError> {
            Ok(match t {
                1 => Value::Int(r.i32()?),
                2 => Value::BigInt(r.i64()?),
                3 => Value::Dec(r.decimal()?),
                4 => Value::Bool(r.bool()?),
                5 => Value::Text(r.string()?),
                6 => Value::Date(r.i32()?),
                7 => Value::Time(r.i32()?),
                8 => Value::DateTime(r.i64()?),
                9 => Value::Duration(r.i64()?),
                10 => {
                    let mut g = [0u8; 16];
                    g.copy_from_slice(r.take(16)?);
                    Value::Guid(g)
                }
                11 => Value::Option(r.i32()?),
                12 => {
                    let n = r.var_u32()? as usize;
                    Value::Blob(r.take(n)?.to_vec())
                }
                _ => return Err(super::netbin::BinError(format!("unknown value tag {t}"))),
            })
        })()
        .map_err(|x| e(&x.0))?;
        if !fits(schema.columns[c], &v) {
            return Err(e(&format!("key value {v:?} does not match column type {:?}", schema.columns[c])));
        }
        out.push(v);
    }
    // the deep-link form re-pads trailing zeros to the original length; tolerate
    // zero padding, reject anything else
    if r.b[r.p..].iter().any(|&x| x != 0) {
        return Err(e("trailing bytes"));
    }
    Ok(out)
}

/// A full-width row of `schema` with the primary key values set and everything
/// else at its default, ready for `Conn::get` / `Session::rec_get_pk`.
pub fn pk_row(schema: &TableSchema, pk: &[Value]) -> Row {
    let mut row = schema.new_row();
    for (&c, v) in schema.keys[0].iter().zip(pk) {
        row[c] = v.clone();
    }
    row
}

/// `decode_bookmark` + `pk_row`.
pub fn bookmark_row(schema: &TableSchema, b: &[u8]) -> Result<Row, BookmarkError> {
    decode_bookmark(schema, b).map(|pk| pk_row(schema, &pk))
}

/// `ByteArrayHelper.ToUrlSafeString`: the bookmark key the web server uses for row
/// identity and deep links. Empty for an all-zero array.
pub fn url_safe_string(b: &[u8]) -> String {
    use base64::Engine;
    let Some(last) = b.iter().rposition(|&x| x != 0) else { return String::new() };
    let s = base64::engine::general_purpose::STANDARD.encode(&b[..=last]);
    format!("{:X}_{}", b.len(), s.trim_end_matches('=').replace('+', "-").replace('/', "_"))
}

/// `NavBookmarkHelper.Parse` for both string forms (`len;base64` and `HEXLEN_urlsafe`).
pub fn parse_bookmark_string(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let (len, body, url) = match s.find(';') {
        Some(i) => (s[..i].parse::<usize>().ok()?, &s[i + 1..], false),
        None => {
            let i = s.find('_')?;
            (usize::from_str_radix(&s[..i], 16).ok()?, &s[i + 1..], true)
        }
    };
    let mut bytes = if url {
        let mut t = body.replace('_', "/").replace('-', "+");
        match t.len() % 4 {
            2 => t.push_str("=="),
            3 => t.push('='),
            _ => {}
        }
        base64::engine::general_purpose::STANDARD.decode(t).ok()?
    } else {
        base64::engine::general_purpose::STANDARD.decode(body).ok()?
    };
    if bytes.len() > len {
        return None;
    }
    bytes.resize(len, 0);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn schema(cols: Vec<ColType>, pk: Vec<usize>) -> TableSchema {
        TableSchema {
            id: 18,
            name: "T".into(),
            field_ids: (1..=cols.len() as i64).collect(),
            columns: cols,
            keys: vec![pk],
            per_company: true,
        }
    }

    #[test]
    fn layout() {
        let s = schema(vec![ColType::Code, ColType::Text], vec![0]);
        let row = vec![Value::Text("C10000".into()), Value::Text("Adatum".into())];
        let b = encode_bookmark(&s, &row);
        assert_eq!(b, [0xBC, 0x01, 18, 1, 5, 6, b'C', b'1', b'0', b'0', b'0', b'0']);
        assert_eq!(decode_bookmark(&s, &b).unwrap(), vec![Value::Text("C10000".into())]);
        // table ids above 127 take more bytes
        assert_eq!(&encode_pk(5050, &[])[..4], &[0xBC, 0x01, 0xBA, 0x27]);
    }

    #[test]
    fn round_trip_all_types() {
        let cols = vec![
            ColType::Option,
            ColType::Code,
            ColType::Int,
            ColType::BigInt,
            ColType::Dec,
            ColType::Bool,
            ColType::Date,
            ColType::Time,
            ColType::DateTime,
            ColType::Duration,
            ColType::Guid,
            ColType::Blob,
        ];
        let s = schema(cols, (0..12).collect());
        let row = vec![
            Value::Option(1),
            Value::Text("S-ORD101001".into()),
            Value::Int(0),
            Value::BigInt(-5),
            Value::Dec(Decimal::from_str("1.50").unwrap()),
            Value::Bool(false),
            Value::Date(1_477_800),
            Value::Time(0),
            Value::DateTime(bc_store::DATETIME_UNDEFINED),
            Value::Duration(0),
            Value::Guid([0; 16]),
            Value::Blob(vec![]),
        ];
        let b = encode_bookmark(&s, &row);
        assert_eq!(decode_bookmark(&s, &b).unwrap(), row);
        // trailing zero padding (deep links) is fine, other trailing bytes are not
        let mut padded = b.clone();
        padded.extend_from_slice(&[0, 0, 0]);
        assert_eq!(decode_bookmark(&s, &padded).unwrap(), row);
        let mut junk = b.clone();
        junk.push(7);
        assert!(decode_bookmark(&s, &junk).is_err());
        assert_eq!(bookmark_row(&s, &b).unwrap(), row);
    }

    #[test]
    fn rejects_foreign_bookmarks() {
        let s = schema(vec![ColType::Code], vec![0]);
        let other = TableSchema { id: 27, ..s.clone() };
        let b = encode_bookmark(&other, &vec![Value::Text("1000".into())]);
        assert!(decode_bookmark(&s, &b).is_err());
        assert!(decode_bookmark(&s, &[]).is_err());
        assert!(decode_bookmark(&s, &[1, 2, 3]).is_err());
        let int_key = schema(vec![ColType::Int], vec![0]);
        assert!(decode_bookmark(&int_key, &encode_bookmark(&s, &vec![Value::Text("X".into())])).is_err());
    }

    #[test]
    fn string_forms() {
        // a key ending in zero bytes survives the trimmed deep-link form
        let s = schema(vec![ColType::Int], vec![0]);
        let b = encode_bookmark(&s, &vec![Value::Int(0)]);
        assert_eq!(*b.last().unwrap(), 0);
        let key = url_safe_string(&b);
        assert!(key.starts_with(&format!("{:X}_", b.len())));
        assert_eq!(parse_bookmark_string(&key).unwrap(), b);
        assert_eq!(decode_bookmark(&s, &parse_bookmark_string(&key).unwrap()).unwrap(), vec![Value::Int(0)]);
        assert_eq!(url_safe_string(&[0, 0]), "");
        assert_eq!(parse_bookmark_string("3;AQI=").unwrap(), vec![1, 2, 0]);
    }
}
