//! DotNet System.IO.Compression.GZipStream (and CompressionMode / CompressionLevel) over AL
//! streams and MemoryStreams, with the gzip and deflate codecs in pure Rust. Compression writes
//! stored deflate blocks (valid gzip that any reader decodes, without the size reduction);
//! decompression inflates any deflate stream (stored, fixed and dynamic Huffman blocks) and checks
//! the CRC. Microsoft's AL uses it in "Config. Pckg. Compression Mgt." (RapidStart packages).
//!
//! A compressing GZipStream collects what is written to it (CopyStream(GZip, InStream)) and writes
//! the gzip data to its stream on Close/Dispose; a decompressing one decodes its stream on the
//! first read (CopyStream(OutStream, GZip), CopyTo).

use crate::dotnet::{enum_val, net, Args, NetObj, NetRef, Out};
use crate::interp::*;
use crate::value::*;

/// State of a GZipStream object (NetObj::GZip).
pub struct GZip {
    pub compress: bool,
    /// The wrapped stream: an AL OutStream / InStream or a DotNet MemoryStream.
    pub base: Val,
    /// Compression: the bytes written so far, compressed on Close.
    pub buf: Vec<u8>,
    /// Decompression: the decoded content and the read position.
    pub decoded: Option<(Vec<u8>, usize)>,
    pub closed: bool,
}

/// `GZipStream.GZipStream(Stream, CompressionMode | CompressionLevel [, LeaveOpen])`.
pub fn construct(a: &Args) -> R<Val> {
    let compress = match a.raw(1) {
        Some(Val::Net(m)) => match &*m.borrow() {
            NetObj::Prim(t, _) if t.ends_with("CompressionLevel") => true,
            NetObj::Prim(_, v) => v.as_i64() == 1,
            _ => false,
        },
        Some(v) => v.as_i64() == 1,
        None => false,
    };
    let base = a.raw(0).cloned().unwrap_or(Val::Void);
    Ok(net(NetObj::GZip(Box::new(GZip { compress, base, buf: vec![], decoded: None, closed: false }))))
}

pub fn is_gzip(n: &NetRef) -> bool {
    matches!(&*n.borrow(), NetObj::GZip(_))
}

/// Write to a compressing GZipStream.
pub fn write(n: &NetRef, bytes: &[u8]) -> R<()> {
    match &mut *n.borrow_mut() {
        NetObj::GZip(g) if g.compress && !g.closed => {
            g.buf.extend_from_slice(bytes);
            Ok(())
        }
        NetObj::GZip(_) => err("Stream does not support writing."),
        _ => Ok(()),
    }
}

/// Read up to `max` bytes (all with None) from a decompressing GZipStream.
pub fn read(n: &NetRef, max: Option<usize>) -> R<Vec<u8>> {
    let mut o = n.borrow_mut();
    let NetObj::GZip(g) = &mut *o else { return Ok(vec![]) };
    if g.compress {
        return err("Stream does not support reading.");
    }
    if g.decoded.is_none() {
        let src: Vec<u8> = match &g.base {
            Val::InStream(s) => {
                let mut st = s.borrow_mut();
                let rest = st.data[st.pos.min(st.data.len())..].to_vec();
                st.pos = st.data.len();
                rest
            }
            Val::Net(m) => match &mut *m.borrow_mut() {
                NetObj::Stream(b, pos) => {
                    let rest = b[(*pos).min(b.len())..].to_vec();
                    *pos = b.len();
                    rest
                }
                _ => vec![],
            },
            _ => vec![],
        };
        let data = gzip_decode(&src).map_err(|e| {
            let mut err = AlError::new(format!("A call to System.IO.Compression.GZipStream failed with this message: {e}"));
            // .NET's InvalidDataException (crate::dotnet::dotnet_error_code)
            err.code = "DotNetInvoke:InvalidData";
            Flow::Err(Box::new(err))
        })?;
        g.decoded = Some((data, 0));
    }
    let (data, pos) = g.decoded.as_mut().unwrap();
    let rest = &data[(*pos).min(data.len())..];
    let take = max.unwrap_or(rest.len()).min(rest.len());
    let out = rest[..take].to_vec();
    *pos += take;
    Ok(out)
}

/// Members of GZipStream, CompressionMode and CompressionLevel.
pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    let _ = a;
    Ok(Out::Val(match (ty, m) {
        ("System.IO.Compression.CompressionMode", "decompress") => enum_val(ty, 0),
        ("System.IO.Compression.CompressionMode", "compress") => enum_val(ty, 1),
        ("System.IO.Compression.CompressionLevel", "optimal") => enum_val(ty, 0),
        ("System.IO.Compression.CompressionLevel", "fastest") => enum_val(ty, 1),
        ("System.IO.Compression.CompressionLevel", "nocompression") => enum_val(ty, 2),
        ("System.IO.Compression.CompressionLevel", "smallestsize") => enum_val(ty, 3),
        ("System.IO.Compression.GZipStream", "close" | "dispose") => {
            let mut o = n.borrow_mut();
            if let NetObj::GZip(g) = &mut *o {
                if g.compress && !g.closed {
                    crate::dotnet::text::stream_write(&g.base, &gzip_encode(&g.buf));
                }
                g.closed = true;
            }
            Val::Void
        }
        ("System.IO.Compression.GZipStream", "flush") => Val::Void,
        ("System.IO.Compression.GZipStream", "canread") => Val::Bool(matches!(&*n.borrow(), NetObj::GZip(g) if !g.compress && !g.closed)),
        ("System.IO.Compression.GZipStream", "canwrite") => Val::Bool(matches!(&*n.borrow(), NetObj::GZip(g) if g.compress && !g.closed)),
        ("System.IO.Compression.GZipStream", "copyto") => {
            let bytes = read(n, None)?;
            if let Some(dest) = a.raw(0) {
                crate::dotnet::text::stream_write(dest, &bytes);
            }
            Val::Void
        }
        _ => return Ok(Out::Unknown),
    }))
}

// ---------------------------------------------------------------- codecs

fn crc_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

/// CRC-32 (IEEE), as gzip stores it.
pub fn crc32(data: &[u8]) -> u32 {
    let t = crc_table();
    !data.iter().fold(!0u32, |c, &b| t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

/// A gzip member of `data` with stored (uncompressed) deflate blocks: valid gzip that any reader
/// decodes, without the size reduction.
pub fn gzip_encode(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(65535).collect() };
    let last = chunks.len() - 1;
    for (i, c) in chunks.iter().enumerate() {
        out.push((i == last) as u8);
        let n = c.len() as u16;
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(!n).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out
}

/// The content of a gzip stream (its first member), checking the CRC.
pub fn gzip_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err("The magic number in GZip header is not correct. Make sure you are passing in a GZip stream.".into());
    }
    let flags = data[3];
    let mut p = 10;
    let at = |p: usize| data.get(p).copied().ok_or_else(|| "Unexpected end of stream".to_string());
    if flags & 4 != 0 {
        let xlen = at(p)? as usize | (at(p + 1)? as usize) << 8;
        p += 2 + xlen;
    }
    for flag in [8u8, 16] {
        if flags & flag != 0 {
            while at(p)? != 0 {
                p += 1;
            }
            p += 1;
        }
    }
    if flags & 2 != 0 {
        p += 2;
    }
    let (out, used) = inflate(data.get(p..).ok_or("Unexpected end of stream")?)?;
    let tail = data.get(p + used..p + used + 8).ok_or("Unexpected end of stream")?;
    let crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    if crc != crc32(&out) {
        return Err("The CRC in GZip footer does not match the CRC calculated from the decompressed data.".into());
    }
    Ok(out)
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl Bits<'_> {
    fn bits(&mut self, need: u32) -> Result<u32, String> {
        while self.cnt < need {
            let b = *self.d.get(self.pos).ok_or("Unexpected end of stream")?;
            self.pos += 1;
            self.buf |= (b as u64) << self.cnt;
            self.cnt += 8;
        }
        let v = (self.buf & ((1u64 << need) - 1)) as u32;
        self.buf >>= need;
        self.cnt -= need;
        Ok(v)
    }
}

/// Canonical Huffman code: code lengths' counts and the symbols ordered by code.
struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Huff {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Huff { count, symbol }
    }

    fn decode(&self, b: &mut Bits) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.bits(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("The archive entry was compressed using an unsupported compression method.".into())
    }
}

const LBASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEXT: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DBASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DEXT: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn codes(b: &mut Bits, out: &mut Vec<u8>, lens: &Huff, dists: &Huff) -> Result<(), String> {
    let bad = || "Block length does not match with its complement.".to_string();
    loop {
        let sym = lens.decode(b)? as usize;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            _ => {
                let s = sym - 257;
                if s >= 29 {
                    return Err(bad());
                }
                let len = LBASE[s] as usize + b.bits(LEXT[s] as u32)? as usize;
                let d = dists.decode(b)? as usize;
                if d >= 30 {
                    return Err(bad());
                }
                let dist = DBASE[d] as usize + b.bits(DEXT[d] as u32)? as usize;
                if dist > out.len() {
                    return Err(bad());
                }
                for _ in 0..len {
                    out.push(out[out.len() - dist]);
                }
            }
        }
    }
}

/// Inflate a raw deflate stream: the data and the number of input bytes it took.
pub fn inflate(data: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let mut b = Bits { d: data, pos: 0, buf: 0, cnt: 0 };
    let mut out = vec![];
    loop {
        let last = b.bits(1)?;
        match b.bits(2)? {
            0 => {
                b.buf = 0;
                b.cnt = 0;
                let h = data.get(b.pos..b.pos + 4).ok_or("Unexpected end of stream")?;
                let n = u16::from_le_bytes([h[0], h[1]]);
                if n != !u16::from_le_bytes([h[2], h[3]]) {
                    return Err("Block length does not match with its complement.".into());
                }
                b.pos += 4;
                out.extend_from_slice(data.get(b.pos..b.pos + n as usize).ok_or("Unexpected end of stream")?);
                b.pos += n as usize;
            }
            1 => {
                let mut l = [0u8; 288];
                l[..144].fill(8);
                l[144..256].fill(9);
                l[256..280].fill(7);
                l[280..].fill(8);
                codes(&mut b, &mut out, &Huff::new(&l), &Huff::new(&[5u8; 30]))?;
            }
            2 => {
                let nlen = b.bits(5)? as usize + 257;
                let ndist = b.bits(5)? as usize + 1;
                let ncode = b.bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(ncode) {
                    cl[o] = b.bits(3)? as u8;
                }
                let ch = Huff::new(&cl);
                let mut lengths = vec![0u8; nlen + ndist];
                let mut i = 0;
                while i < nlen + ndist {
                    let sym = ch.decode(&mut b)?;
                    let (val, rep) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 if i > 0 => (lengths[i - 1], 3 + b.bits(2)? as usize),
                        17 => (0, 3 + b.bits(3)? as usize),
                        18 => (0, 11 + b.bits(7)? as usize),
                        _ => return Err("Invalid code lengths in deflate block.".into()),
                    };
                    if i + rep > nlen + ndist {
                        return Err("Invalid code lengths in deflate block.".into());
                    }
                    lengths[i..i + rep].fill(val);
                    i += rep;
                }
                codes(&mut b, &mut out, &Huff::new(&lengths[..nlen]), &Huff::new(&lengths[nlen..]))?;
            }
            _ => return Err("The archive entry was compressed using an unsupported compression method.".into()),
        }
        if last == 1 {
            return Ok((out, b.pos));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_codec() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        assert_eq!(gzip_decode(&gzip_encode(&data)).unwrap(), data);
        assert_eq!(gzip_decode(&gzip_encode(b"")).unwrap(), b"");
        // `printf 'hello hello hello\n' | gzip -9n`: fixed Huffman block with a back reference
        let gz = [
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x5c, 0x00, 0x3b, 0x7c, 0x8a, 0xdf, 0x12, 0x00, 0x00, 0x00,
        ];
        assert_eq!(gzip_decode(&gz).unwrap(), b"hello hello hello\n");
        assert!(gzip_decode(b"not gzip data at all").is_err());
    }
}
