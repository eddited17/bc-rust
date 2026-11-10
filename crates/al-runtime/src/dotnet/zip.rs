//! System.IO.Compression.ZipArchive / ZipArchiveEntry over AL streams and MemoryStreams. Reading
//! takes stored and deflated entries (crate::gzip's inflate); writing stores entries uncompressed
//! (a valid zip any reader opens, without the size reduction, as gzip.rs does for GZipStream). A
//! Create or Update archive writes the whole zip to its stream on Dispose.

use super::*;

pub const ARCHIVE: &str = "System.IO.Compression.ZipArchive";
pub const ENTRY: &str = "System.IO.Compression.ZipArchiveEntry";
const MODE: &str = "System.IO.Compression.ZipArchiveMode";

pub const TYPES: &[&str] = &[ARCHIVE, ENTRY, MODE];

pub fn enum_names(t: &str) -> Option<&'static [(&'static str, i64)]> {
    (t == MODE).then_some(&[("Read", 0), ("Create", 1), ("Update", 2)])
}

pub struct EntryData {
    pub name: String,
    pub data: Vec<u8>,
    /// the stream Open() handed out for writing (its bytes are the entry's on Dispose)
    pub open: Option<NetRef>,
    pub deleted: bool,
}

pub type Entry = Rc<RefCell<EntryData>>;

pub struct ZipState {
    pub target: Val,
    pub mode: i64,
    pub entries: Vec<Entry>,
    pub done: bool,
}

impl EntryData {
    fn bytes(&self) -> Vec<u8> {
        match &self.open {
            Some(s) => match &*s.borrow() {
                NetObj::Stream(b, _) => b.clone(),
                _ => self.data.clone(),
            },
            None => self.data.clone(),
        }
    }
}

fn u16le(b: &[u8], i: usize) -> usize {
    b.get(i..i + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as usize).unwrap_or(0)
}

fn u32le(b: &[u8], i: usize) -> usize {
    b.get(i..i + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as usize).unwrap_or(0)
}

/// The entries of a zip file (central directory).
pub fn read_zip(b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bad = || "Central Directory corrupt.".to_string();
    if b.is_empty() {
        return Ok(vec![]);
    }
    let eocd = (0..b.len().saturating_sub(21)).rev().find(|&i| u32le(b, i) == 0x0605_4b50).ok_or("End of Central Directory record could not be found.")?;
    let count = u16le(b, eocd + 10);
    let mut p = u32le(b, eocd + 16);
    let mut out = vec![];
    for _ in 0..count {
        if u32le(b, p) != 0x0201_4b50 {
            return Err(bad());
        }
        let method = u16le(b, p + 10);
        let csize = u32le(b, p + 20);
        let (nlen, xlen, clen) = (u16le(b, p + 28), u16le(b, p + 30), u16le(b, p + 32));
        let local = u32le(b, p + 42);
        let name = String::from_utf8_lossy(b.get(p + 46..p + 46 + nlen).ok_or_else(bad)?).into_owned();
        p += 46 + nlen + xlen + clen;
        if u32le(b, local) != 0x0403_4b50 {
            return Err(bad());
        }
        let start = local + 30 + u16le(b, local + 26) + u16le(b, local + 28);
        let raw = b.get(start..start + csize).ok_or_else(bad)?;
        let data = match method {
            0 => raw.to_vec(),
            8 => crate::gzip::inflate(raw)?.0,
            m => return Err(format!("The archive entry was compressed using an unsupported compression method ({m}).")),
        };
        out.push((name, data));
    }
    Ok(out)
}

/// A zip file with the entries stored.
pub fn write_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let (mut out, mut central) = (vec![], vec![]);
    // DOS time 00:00, date 1980-01-01
    let (time, date) = (0u16, 0x21u16);
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crate::gzip::crc32(data);
        let n = name.as_bytes();
        let header = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes()); // version needed
            h.extend(0x0800u16.to_le_bytes()); // UTF-8 names
            h.extend(0u16.to_le_bytes()); // stored
            h.extend(time.to_le_bytes());
            h.extend(date.to_le_bytes());
            h.extend(crc.to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((n.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                h.extend([0u8; 6]); // comment len, disk, internal attrs
                h.extend(0u32.to_le_bytes()); // external attrs
                h.extend(offset.to_le_bytes());
            }
            h.extend_from_slice(n);
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(data);
        central.extend(header(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(cd_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

fn entry_val(e: &Entry) -> Val {
    net(NetObj::ZipEntry(e.clone()))
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    if ty != ARCHIVE {
        return Ok(net(NetObj::Null(ty)));
    }
    let target = a.raw(0).cloned().unwrap_or(Val::Void);
    let mode = if a.len() > 1 { a.int(1) } else { 0 };
    let mut entries = vec![];
    if mode != 1 {
        let bytes = match &target {
            Val::Net(n) => match &*n.borrow() {
                NetObj::Stream(b, _) => b.clone(),
                _ => super::text::arg_bytes(Some(&target)),
            },
            v => super::text::arg_bytes(Some(v)),
        };
        match read_zip(&bytes) {
            Ok(list) => entries = list.into_iter().map(|(name, data)| Rc::new(RefCell::new(EntryData { name, data, open: None, deleted: false }))).collect(),
            Err(e) => return net_err("ZipArchive", "ZipArchive", e),
        }
    }
    Ok(net(NetObj::Zip(Box::new(ZipState { target, mode, entries, done: false }))))
}

/// Dispose of a Create/Update archive: the zip goes to its stream (once).
fn finish(z: &mut ZipState) {
    if z.done || z.mode == 0 {
        z.done = true;
        return;
    }
    z.done = true;
    let list: Vec<(String, Vec<u8>)> = z.entries.iter().filter(|e| !e.borrow().deleted).map(|e| (e.borrow().name.clone(), e.borrow().bytes())).collect();
    let bytes = write_zip(&list);
    if let Val::Net(n) = &z.target {
        if let NetObj::Stream(b, pos) = &mut *n.borrow_mut() {
            // an Update archive replaces what its stream held
            b.clear();
            *pos = 0;
        }
    }
    super::text::stream_write(&z.target, &bytes);
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if ty == MODE {
        // ZipArchiveMode.Create ... on the enum variable
        return match enum_names(MODE).and_then(|v| v.iter().find(|(x, _)| x.eq_ignore_ascii_case(m))) {
            Some((_, v)) => ok(enum_val(MODE, *v)),
            None => Ok(Out::Unknown),
        };
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Zip(z) => ok(match m {
            "entries" => net(NetObj::Array(ENTRY, z.entries.iter().filter(|e| !e.borrow().deleted).map(entry_val).collect())),
            "getentry" => {
                let name = a.text(0);
                match z.entries.iter().find(|e| !e.borrow().deleted && e.borrow().name == name) {
                    Some(e) => entry_val(e),
                    None => net(NetObj::Null(ENTRY)),
                }
            }
            "createentry" => {
                if z.mode == 0 {
                    return net_err("ZipArchive", "CreateEntry", "Cannot modify read-only archive.");
                }
                let e: Entry = Rc::new(RefCell::new(EntryData { name: a.text(0), data: vec![], open: None, deleted: false }));
                z.entries.push(e.clone());
                entry_val(&e)
            }
            "mode" => enum_val(MODE, z.mode),
            "dispose" | "close" => {
                finish(z);
                Val::Void
            }
            _ => return Ok(Out::Unknown),
        }),
        NetObj::ZipEntry(e) => {
            let e = e.clone();
            drop(o);
            ok(match m {
                "fullname" => Val::Text(e.borrow().name.clone()),
                "name" => Val::Text(e.borrow().name.rsplit(['/', '\\']).next().unwrap_or("").to_string()),
                "length" | "compressedlength" => Val::BigInt(e.borrow().bytes().len() as i64),
                "crc32" => Val::BigInt(crate::gzip::crc32(&e.borrow().bytes()) as i64),
                "open" => {
                    let current = e.borrow().bytes();
                    let s = Rc::new(RefCell::new(NetObj::Stream(current, 0)));
                    e.borrow_mut().open = Some(s.clone());
                    Val::Net(s)
                }
                "delete" => {
                    e.borrow_mut().deleted = true;
                    Val::Void
                }
                _ => return Ok(Out::Unknown),
            })
        }
        _ => Ok(Out::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_round_trip() {
        let z = write_zip(&[("a.txt".into(), b"hello".to_vec()), ("dir/b.bin".into(), vec![0, 1, 2])]);
        let r = read_zip(&z).unwrap();
        assert_eq!(r, vec![("a.txt".to_string(), b"hello".to_vec()), ("dir/b.bin".to_string(), vec![0, 1, 2])]);
    }

    #[test]
    fn interop_files() {
        // deflated entries written by Python's zipfile; our output for an external check
        if let Ok(b) = std::fs::read("/tmp/p2-zip-defl.zip") {
            let r = read_zip(&b).unwrap();
            assert_eq!(r[0].0, "x/y.txt");
            assert_eq!(r[0].1, "hello deflate ".repeat(50).into_bytes());
            assert!(r[1].1.is_empty());
        }
        let _ = std::fs::write("/tmp/p2-zip-ours.zip", write_zip(&[("a.txt".into(), b"hello".to_vec())]));
    }
}
