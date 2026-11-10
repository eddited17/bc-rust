//! .NET `BinaryWriter` / `BinaryReader` primitives (UTF-8 encoding, little endian),
//! the building blocks of the NavDataSet stream.
//!
//! Layouts follow the .NET runtime:
//! - integers and floats: little endian, fixed width;
//! - bool: one byte (0 / 1);
//! - string: `Write7BitEncodedInt(byteCount)` then the UTF-8 bytes;
//! - char: the UTF-8 bytes of the character (BMP only);
//! - decimal: four int32 words `lo, mid, hi, flags`; flags hold the scale in
//!   bits 16..23 and the sign in bit 31 (`decimal.GetBits` order);
//! - Guid: `Guid.ToByteArray()` order (first three groups little endian).

use rust_decimal::Decimal;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinError(pub String);

impl fmt::Display for BinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BinError {}

#[derive(Default, Debug, Clone)]
pub struct BinWriter {
    pub buf: Vec<u8>,
}

impl BinWriter {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn bytes(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    /// `BinaryWriter.Write7BitEncodedInt`: 7 bits per byte, low bits first,
    /// high bit set while more bytes follow. Negative values take 5 bytes.
    pub fn var_u32(&mut self, v: u32) {
        let mut v = v;
        while v >= 0x80 {
            self.buf.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }

    /// `BinaryWriter.Write(string)`.
    pub fn string(&mut self, s: &str) {
        self.var_u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
    }

    /// `BinaryWriter.Write(char)` with UTF-8 encoding. .NET chars are UTF-16 code
    /// units, so only BMP characters can be written.
    pub fn char(&mut self, c: char) -> Result<(), BinError> {
        if (c as u32) > 0xFFFF {
            return Err(BinError(format!("character U+{:X} is outside the BMP and cannot be a .NET char", c as u32)));
        }
        let mut b = [0u8; 4];
        self.buf.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        Ok(())
    }

    /// `BinaryWriter.Write(decimal)`.
    pub fn decimal(&mut self, d: Decimal) {
        self.buf.extend_from_slice(&decimal_bytes(d));
    }

    /// `BinaryWriter.Write(guid.ToByteArray())`, from a GUID in textual (RFC 4122) byte order.
    pub fn guid(&mut self, g: &[u8; 16]) {
        self.buf.extend_from_slice(&guid_to_dotnet(g));
    }
}

/// The 16 bytes `BinaryWriter.Write(decimal)` produces.
pub fn decimal_bytes(d: Decimal) -> [u8; 16] {
    let m = d.mantissa().unsigned_abs();
    let lo = (m & 0xFFFF_FFFF) as u32;
    let mid = ((m >> 32) & 0xFFFF_FFFF) as u32;
    let hi = ((m >> 64) & 0xFFFF_FFFF) as u32;
    let mut flags = d.scale() << 16;
    if d.is_sign_negative() {
        flags |= 0x8000_0000;
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&lo.to_le_bytes());
    out[4..8].copy_from_slice(&mid.to_le_bytes());
    out[8..12].copy_from_slice(&hi.to_le_bytes());
    out[12..16].copy_from_slice(&flags.to_le_bytes());
    out
}

/// Textual GUID byte order → .NET `Guid.ToByteArray()` order (and back: the swap is
/// its own inverse).
pub fn guid_to_dotnet(g: &[u8; 16]) -> [u8; 16] {
    let mut o = *g;
    o[0..4].reverse();
    o[4..6].reverse();
    o[6..8].reverse();
    o
}

pub fn guid_from_dotnet(b: &[u8; 16]) -> [u8; 16] {
    guid_to_dotnet(b)
}

pub struct BinReader<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> BinReader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        BinReader { b, p: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.b.len() - self.p
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], BinError> {
        let end = self.p.checked_add(n).filter(|&e| e <= self.b.len());
        match end {
            Some(e) => {
                let s = &self.b[self.p..e];
                self.p = e;
                Ok(s)
            }
            None => Err(BinError(format!("unexpected end of stream at byte {} (wanted {n} more)", self.p))),
        }
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], BinError> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8, BinError> {
        Ok(self.take(1)?[0])
    }
    pub fn i8(&mut self) -> Result<i8, BinError> {
        Ok(self.u8()? as i8)
    }
    /// `BinaryReader.ReadBoolean`: any non-zero byte is true.
    pub fn bool(&mut self) -> Result<bool, BinError> {
        Ok(self.u8()? != 0)
    }
    pub fn i16(&mut self) -> Result<i16, BinError> {
        Ok(i16::from_le_bytes(self.arr()?))
    }
    pub fn u16(&mut self) -> Result<u16, BinError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn i32(&mut self) -> Result<i32, BinError> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32, BinError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn i64(&mut self) -> Result<i64, BinError> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64, BinError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    pub fn f32(&mut self) -> Result<f32, BinError> {
        Ok(f32::from_le_bytes(self.arr()?))
    }
    pub fn f64(&mut self) -> Result<f64, BinError> {
        Ok(f64::from_le_bytes(self.arr()?))
    }

    /// `BinaryReader.Read7BitEncodedInt` (at most 5 bytes).
    pub fn var_u32(&mut self) -> Result<u32, BinError> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 4 && b > 0x0F {
                return Err(BinError("bad 7-bit encoded int".into()));
            }
            v |= ((b & 0x7F) as u32) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(BinError("bad 7-bit encoded int".into()))
    }

    pub fn string(&mut self) -> Result<String, BinError> {
        let n = self.var_u32()?;
        if n > i32::MAX as u32 {
            return Err(BinError("negative string length".into()));
        }
        let s = self.take(n as usize)?;
        String::from_utf8(s.to_vec()).map_err(|_| BinError("string is not valid UTF-8".into()))
    }

    /// `BinaryReader.ReadChar` with UTF-8 encoding.
    pub fn char(&mut self) -> Result<char, BinError> {
        let first = self.u8()?;
        let len = match first {
            0x00..=0x7F => 1,
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            _ => return Err(BinError(format!("bad UTF-8 lead byte {first:#x} for a char"))),
        };
        let mut b = vec![first];
        b.extend_from_slice(self.take(len - 1)?);
        let s = std::str::from_utf8(&b).map_err(|_| BinError("char is not valid UTF-8".into()))?;
        s.chars().next().ok_or_else(|| BinError("empty char".into()))
    }

    /// `BinaryReader.ReadDecimal` (rejects flags .NET would reject).
    pub fn decimal(&mut self) -> Result<Decimal, BinError> {
        let lo = self.u32()?;
        let mid = self.u32()?;
        let hi = self.u32()?;
        let flags = self.u32()?;
        let scale = (flags >> 16) & 0xFF;
        if flags & 0x7F00_FFFF != 0 || scale > 28 {
            return Err(BinError(format!("invalid decimal flags {flags:#010x}")));
        }
        Ok(Decimal::from_parts(lo, mid, hi, flags & 0x8000_0000 != 0, scale))
    }

    /// 16 bytes in .NET order, returned in textual order.
    pub fn guid(&mut self) -> Result<[u8; 16], BinError> {
        let a: [u8; 16] = self.arr()?;
        Ok(guid_from_dotnet(&a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn w(f: impl FnOnce(&mut BinWriter)) -> Vec<u8> {
        let mut b = BinWriter::new();
        f(&mut b);
        b.into_bytes()
    }

    #[test]
    fn seven_bit_lengths() {
        assert_eq!(w(|b| b.var_u32(0)), [0x00]);
        assert_eq!(w(|b| b.var_u32(127)), [0x7F]);
        assert_eq!(w(|b| b.var_u32(128)), [0x80, 0x01]);
        assert_eq!(w(|b| b.var_u32(200)), [0xC8, 0x01]);
        assert_eq!(w(|b| b.var_u32(16384)), [0x80, 0x80, 0x01]);
        assert_eq!(w(|b| b.var_u32(u32::MAX)), [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
        for v in [0u32, 1, 127, 128, 300, 16383, 16384, 2_097_151, 2_097_152, u32::MAX] {
            let bytes = w(|b| b.var_u32(v));
            assert_eq!(BinReader::new(&bytes).var_u32().unwrap(), v);
        }
    }

    #[test]
    fn strings() {
        assert_eq!(w(|b| b.string("")), [0x00]);
        assert_eq!(w(|b| b.string("abc")), [0x03, b'a', b'b', b'c']);
        // byte count, not char count: "æ" is C3 A6 in UTF-8
        assert_eq!(w(|b| b.string("æ")), [0x02, 0xC3, 0xA6]);
        // 200 bytes need a two-byte length prefix
        let long = "x".repeat(200);
        let bytes = w(|b| b.string(&long));
        assert_eq!(&bytes[..2], &[0xC8, 0x01]);
        assert_eq!(bytes.len(), 202);
        assert_eq!(BinReader::new(&bytes).string().unwrap(), long);
    }

    #[test]
    fn decimals() {
        // 123.45m: mantissa 12345 = 0x3039, scale 2
        assert_eq!(
            decimal_bytes(Decimal::from_str("123.45").unwrap()),
            [0x39, 0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x00]
        );
        // -1.5m: mantissa 15, scale 1, sign bit 31
        assert_eq!(
            decimal_bytes(Decimal::from_str("-1.5").unwrap()),
            [0x0F, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x80]
        );
        // 0m and 1.00m (scale is kept)
        assert_eq!(decimal_bytes(Decimal::ZERO), [0; 16]);
        assert_eq!(
            decimal_bytes(Decimal::from_str("1.00").unwrap()),
            [0x64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x00]
        );
        // decimal.MaxValue: all 96 bits set, scale 0
        assert_eq!(
            decimal_bytes(Decimal::MAX),
            [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]
        );
        // 2^32 + 1 = mid word 1, lo word 1
        assert_eq!(decimal_bytes(Decimal::from(4_294_967_297i64)), [1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        for s in [
            "0",
            "1",
            "-1",
            "123.45",
            "-0.0001",
            "79228162514264337593543950335",
            "0.0000000000000000000000000001",
        ] {
            let d = Decimal::from_str(s).unwrap();
            let b = decimal_bytes(d);
            let back = BinReader::new(&b).decimal().unwrap();
            assert_eq!(back, d);
            assert_eq!(back.scale(), d.scale());
        }
        // flags .NET rejects
        let mut bad = [0u8; 16];
        bad[12] = 1;
        assert!(BinReader::new(&bad).decimal().is_err());
    }

    #[test]
    fn guid_order() {
        // {01234567-89AB-CDEF-0123-456789ABCDEF}.ToByteArray()
        let g = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF];
        assert_eq!(
            w(|b| b.guid(&g)),
            [0x67, 0x45, 0x23, 0x01, 0xAB, 0x89, 0xEF, 0xCD, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]
        );
        let bytes = w(|b| b.guid(&g));
        assert_eq!(BinReader::new(&bytes).guid().unwrap(), g);
    }

    #[test]
    fn chars_and_scalars() {
        assert_eq!(w(|b| b.char('A').unwrap()), [0x41]);
        assert_eq!(w(|b| b.char('é').unwrap()), [0xC3, 0xA9]);
        assert_eq!(w(|b| b.char('€').unwrap()), [0xE2, 0x82, 0xAC]);
        assert!(BinWriter::new().char('😀').is_err());
        assert_eq!(BinReader::new(&[0xE2, 0x82, 0xAC]).char().unwrap(), '€');
        assert_eq!(w(|b| b.i32(-2)), [0xFE, 0xFF, 0xFF, 0xFF]);
        assert_eq!(w(|b| b.i64(1)), [1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(w(|b| b.f64(1.0)), [0, 0, 0, 0, 0, 0, 0xF0, 0x3F]);
        assert_eq!(w(|b| b.bool(true)), [1]);
        assert!(BinReader::new(&[1, 2]).i32().is_err());
    }
}
