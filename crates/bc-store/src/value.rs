//! Stored scalar values and their order-preserving (memcomparable) key encoding.

use rust_decimal::Decimal;
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColType {
    Int,
    BigInt,
    Dec,
    Bool,
    Code,
    Text,
    /// Days since 0001-01-01 times two, plus one for closing dates; 0 = undefined.
    Date,
    /// Milliseconds since midnight plus one; 0 = undefined.
    Time,
    /// Milliseconds since Unix epoch; i64::MIN = undefined.
    DateTime,
    Duration,
    Guid,
    Option,
    Blob,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    BigInt(i64),
    Dec(Decimal),
    Bool(bool),
    Text(String),
    Date(i32),
    Time(i32),
    DateTime(i64),
    Duration(i64),
    Guid([u8; 16]),
    Option(i32),
    Blob(Vec<u8>),
}

pub type Row = Vec<Value>;

pub const DATETIME_UNDEFINED: i64 = i64::MIN;

/// A DateTime as BC stores it: SQL Server `datetime` keeps 1/300 s, which reads back as .000,
/// .003 or .007 s (SQL Server's rounding: x.998 -> x.997, x.999 -> the next .000). A day is a
/// whole number of 10 ms steps, so the last digit of the milliseconds decides.
pub fn sql_datetime_ms(ms: i64) -> i64 {
    if ms == DATETIME_UNDEFINED {
        return ms;
    }
    let r = ms.rem_euclid(10);
    ms - r
        + match r {
            0 | 1 => 0,
            2..=4 => 3,
            5..=8 => 7,
            _ => 10,
        }
}

impl Value {
    pub fn default_for(t: ColType) -> Value {
        match t {
            ColType::Int => Value::Int(0),
            ColType::BigInt => Value::BigInt(0),
            ColType::Dec => Value::Dec(Decimal::ZERO),
            ColType::Bool => Value::Bool(false),
            ColType::Code | ColType::Text => Value::Text(String::new()),
            ColType::Date => Value::Date(0),
            ColType::Time => Value::Time(0),
            ColType::DateTime => Value::DateTime(DATETIME_UNDEFINED),
            ColType::Duration => Value::Duration(0),
            ColType::Guid => Value::Guid([0; 16]),
            ColType::Option => Value::Option(0),
            ColType::Blob => Value::Blob(Vec::new()),
        }
    }

    /// Compare two values the way SQL Server (CI_AS collation) would order them.
    pub fn cmp_sql(&self, o: &Value) -> Ordering {
        use Value::*;
        match (self, o) {
            (Int(a), Int(b)) | (Option(a), Option(b)) | (Date(a), Date(b)) | (Time(a), Time(b)) => a.cmp(b),
            (BigInt(a), BigInt(b)) | (DateTime(a), DateTime(b)) | (Duration(a), Duration(b)) => a.cmp(b),
            (Dec(a), Dec(b)) => a.cmp(b),
            (Bool(a), Bool(b)) => a.cmp(b),
            (Text(a), Text(b)) => cmp_ci(a, b),
            (Guid(a), Guid(b)) => guid_sort_key(a).cmp(&guid_sort_key(b)),
            (Blob(a), Blob(b)) => a.cmp(b),
            _ => Ordering::Equal,
        }
    }
}

pub fn cmp_ci(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(|c| c.to_lowercase());
    let mut bi = b.chars().flat_map(|c| c.to_lowercase());
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                o => return o,
            },
        }
    }
}

/// `BC_GUID_ORDER=sql`: Guids sort as BC sorts them, in SQL Server uniqueidentifier order (Ncl
/// SqlGuidComparer compares the .NET layout bytes 10..15, 8, 9, 6, 7, 4, 5, 0..3). Off by default:
/// display (RFC 4122) byte order, which PostgreSQL `uuid` columns also use.
pub fn guid_sql_order() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BC_GUID_ORDER").is_ok_and(|v| v.eq_ignore_ascii_case("sql")))
}

/// The bytes of a Guid (display order) in the order they sort: with `BC_GUID_ORDER=sql` the last
/// group, then the fourth, then the third, second and first groups byte-reversed (their .NET
/// little-endian storage order); otherwise unchanged.
pub fn guid_sort_key(g: &[u8; 16]) -> [u8; 16] {
    if !guid_sql_order() {
        return *g;
    }
    const ORDER: [usize; 16] = [10, 11, 12, 13, 14, 15, 8, 9, 7, 6, 5, 4, 3, 2, 1, 0];
    let mut k = [0u8; 16];
    for (i, &j) in ORDER.iter().enumerate() {
        k[i] = g[j];
    }
    k
}

/// Append the memcomparable encoding of `v` to `out`.
pub fn encode_key_part(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Int(i) | Value::Option(i) | Value::Date(i) | Value::Time(i) => {
            out.extend_from_slice(&((*i as u32) ^ 0x8000_0000).to_be_bytes())
        }
        Value::BigInt(i) | Value::DateTime(i) | Value::Duration(i) => {
            out.extend_from_slice(&((*i as u64) ^ 0x8000_0000_0000_0000).to_be_bytes())
        }
        Value::Dec(d) => {
            // fixed point with 12 decimals in an i128
            let (m, sc) = (d.mantissa(), d.scale());
            let m: i128 = if sc <= 12 { m.saturating_mul(10i128.pow(12 - sc)) } else { m / 10i128.pow(sc - 12) };
            out.extend_from_slice(&((m as u128) ^ (1u128 << 127)).to_be_bytes());
        }
        Value::Bool(b) => out.push(*b as u8),
        Value::Text(s) => {
            for c in s.chars().flat_map(|c| c.to_lowercase()) {
                let mut buf = [0u8; 4];
                for &b in c.encode_utf8(&mut buf).as_bytes() {
                    if b == 0 {
                        out.extend_from_slice(&[0, 0xFF]);
                    } else {
                        out.push(b);
                    }
                }
            }
            out.extend_from_slice(&[0, 0]);
        }
        Value::Guid(g) => out.extend_from_slice(&guid_sort_key(g)),
        Value::Blob(b) => {
            out.extend_from_slice(&(b.len() as u32).to_be_bytes());
            out.extend_from_slice(b);
        }
    }
}

// ---------------------------------------------------------------- row serialization (WAL)

pub fn write_value(v: &Value, out: &mut Vec<u8>) {
    match v {
        Value::Int(i) => {
            out.push(1);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::BigInt(i) => {
            out.push(2);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Dec(d) => {
            out.push(3);
            out.extend_from_slice(&d.serialize());
        }
        Value::Bool(b) => out.push(if *b { 5 } else { 4 }),
        Value::Text(s) => {
            out.push(6);
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        Value::Date(i) => {
            out.push(7);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Time(i) => {
            out.push(8);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::DateTime(i) => {
            out.push(9);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Duration(i) => {
            out.push(10);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Guid(g) => {
            out.push(11);
            out.extend_from_slice(g);
        }
        Value::Option(i) => {
            out.push(12);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Blob(b) => {
            out.push(13);
            out.extend_from_slice(&(b.len() as u32).to_le_bytes());
            out.extend_from_slice(b);
        }
    }
}

pub struct Reader<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Reader<'a> {
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn str(&mut self) -> Option<String> {
        Some(String::from_utf8_lossy(self.bytes()?).into_owned())
    }
    pub fn value(&mut self) -> Option<Value> {
        Some(match self.u8()? {
            1 => Value::Int(self.i32()?),
            2 => Value::BigInt(self.i64()?),
            3 => Value::Dec(Decimal::deserialize(self.take(16)?.try_into().ok()?)),
            4 => Value::Bool(false),
            5 => Value::Bool(true),
            6 => Value::Text(self.str()?),
            7 => Value::Date(self.i32()?),
            8 => Value::Time(self.i32()?),
            9 => Value::DateTime(self.i64()?),
            10 => Value::Duration(self.i64()?),
            11 => Value::Guid(self.take(16)?.try_into().ok()?),
            12 => Value::Option(self.i32()?),
            13 => Value::Blob(self.bytes()?.to_vec()),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod sql_datetime_tests {
    use super::*;

    #[test]
    fn rounds_like_sql_server_datetime() {
        // SQL Server's documented examples (23:59:59.990 ... .999), here as the last digit
        let base = 1_700_000_000_000i64; // a whole second
        let got: Vec<i64> = (0..10).map(|d| sql_datetime_ms(base + 990 + d) - base).collect();
        assert_eq!(got, [990, 990, 993, 993, 993, 997, 997, 997, 997, 1000]);
        // before 1970 the same steps apply
        assert_eq!(sql_datetime_ms(-1), 0);
        assert_eq!(sql_datetime_ms(-12), -13);
        assert_eq!(sql_datetime_ms(DATETIME_UNDEFINED), DATETIME_UNDEFINED);
    }
}
