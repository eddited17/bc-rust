//! Blob fields and streams: Blob.CreateOutStream / CreateInStream, InStream and OutStream methods,
//! CopyStream. Text is read and written as UTF-8 (TextEncoding arguments are accepted and ignored).
//!
//! In BC both streams of a Blob field wrap the field's NavBLOB (Ncl NavStream over
//! NavBLOB.InternalStream), each with its own position, so an InStream reads what an OutStream on
//! the same field writes after the InStream was created (Temp Blob: CreateInStream, then
//! CreateOutStream and write, then read). Reading the record again (Get, Find, Next, TransferFields)
//! puts a new NavBLOB in the record buffer, and streams made before keep the old one. Here an
//! InStream on a field is registered as bound to that record variable's field: writes, clears and
//! imports through this module (and DotNet writes to AL OutStreams) reach it, record reads do not.

use crate::interp::*;
use crate::value::*;
use al_syntax::Expr;
use bc_store::Value;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub struct InStreamState {
    pub data: Rc<Vec<u8>>,
    pub pos: usize,
}

/// Where an OutStream writes: a Blob field of a record, or a standalone buffer.
pub enum OutTarget {
    Field(crate::record::RecHandle, usize),
    Buffer(Rc<RefCell<Vec<u8>>>),
}

pub fn in_stream(data: Vec<u8>) -> Val {
    Val::InStream(Rc::new(RefCell::new(InStreamState { data: Rc::new(data), pos: 0 })))
}

type Bound = (Weak<crate::record::RecCell>, usize, Weak<RefCell<InStreamState>>);

thread_local! {
    /// InStreams created on a Blob field: (record variable, field index, stream).
    static BOUND: RefCell<Vec<Bound>> = const { RefCell::new(vec![]) };
}

/// An InStream on field `fi` of record variable `r`, bound to it.
fn bound_in_stream(r: &crate::record::RecHandle, fi: usize, data: Vec<u8>) -> Val {
    let st = Rc::new(RefCell::new(InStreamState { data: Rc::new(data), pos: 0 }));
    BOUND.with(|b| {
        let mut b = b.borrow_mut();
        if b.len() >= 64 {
            b.retain(|(r, _, s)| r.strong_count() > 0 && s.strong_count() > 0);
        }
        b.push((Rc::downgrade(r), fi, Rc::downgrade(&st)));
    });
    Val::InStream(st)
}

/// The live InStreams bound to field `fi` of `r`.
fn bound_to(r: &crate::record::RecHandle, fi: usize) -> Vec<Rc<RefCell<InStreamState>>> {
    BOUND.with(|b| {
        b.borrow()
            .iter()
            .filter(|(br, bf, _)| *bf == fi && std::ptr::eq(br.as_ptr(), Rc::as_ptr(r)))
            .filter_map(|(_, _, s)| s.upgrade())
            .collect()
    })
}

/// Bytes appended to a Blob field reach the InStreams bound to it.
fn notify_append(r: &crate::record::RecHandle, fi: usize, bytes: &[u8]) {
    for s in bound_to(r, fi) {
        if let Ok(mut st) = s.try_borrow_mut() {
            Rc::make_mut(&mut st.data).extend_from_slice(bytes);
        }
    }
}

/// A Blob field's content replaced (CreateOutStream clears it, Import loads a file).
fn notify_set(r: &crate::record::RecHandle, fi: usize, bytes: &[u8]) {
    for s in bound_to(r, fi) {
        if let Ok(mut st) = s.try_borrow_mut() {
            st.data = Rc::new(bytes.to_vec());
        }
    }
}

/// A .NET write through an InStream (`Bitmap.Save(InStr, ...)`, `XmlDocument.Save(InStr)`): BC
/// passes .NET the NavBLOB's shared stream at the InStream's position (NavDotNet:
/// `InternalStreamChecked`), so the bytes land in the Blob field at that position without
/// truncating it; the InStream's own position (`Target.Position`) does not move.
pub fn in_stream_write(s: &Rc<RefCell<InStreamState>>, bytes: &[u8]) {
    fn put(d: &mut Vec<u8>, pos: usize, bytes: &[u8]) {
        let end = pos + bytes.len();
        if d.len() < end {
            d.resize(end, 0);
        }
        d[pos..end].copy_from_slice(bytes);
    }
    let pos = s.borrow().pos;
    let field = BOUND.with(|b| {
        b.borrow().iter().find(|(_, _, st)| std::ptr::eq(st.as_ptr(), Rc::as_ptr(s))).and_then(|(r, fi, _)| r.upgrade().map(|r| (r, *fi)))
    });
    match field {
        Some((r, fi)) => {
            let new = {
                let mut rb = r.borrow_mut();
                let mut d = match &rb.row[fi] {
                    Value::Blob(b) => b.clone(),
                    _ => vec![],
                };
                put(&mut d, pos, bytes);
                rb.row[fi] = Value::Blob(d.clone());
                d
            };
            notify_set(&r, fi, &new);
        }
        None => {
            let mut st = s.borrow_mut();
            put(Rc::make_mut(&mut st.data), pos, bytes);
        }
    }
}

/// Append bytes to an OutStream's target (a Blob field reaches the InStreams bound to it).
pub fn append(t: &OutTarget, bytes: &[u8]) {
    match t {
        OutTarget::Field(r, fi) => {
            {
                let mut rb = r.borrow_mut();
                match &mut rb.row[*fi] {
                    Value::Blob(b) => b.extend_from_slice(bytes),
                    other => *other = Value::Blob(bytes.to_vec()),
                }
            }
            notify_append(r, *fi, bytes);
        }
        OutTarget::Buffer(b) => b.borrow_mut().extend_from_slice(bytes),
    }
}

/// Bytes of a value written with OutStream.Write.
fn value_bytes(v: &Val) -> Vec<u8> {
    match v {
        Val::Int(i) => i.to_le_bytes().to_vec(),
        Val::BigInt(i) => i.to_le_bytes().to_vec(),
        Val::Bool(b) => vec![*b as u8],
        Val::Char(c) => c.to_string().into_bytes(),
        Val::Guid(g) => g.to_vec(),
        Val::Blob(b) => b.clone(),
        o => o.to_text().into_bytes(),
    }
}

impl<'r> Session<'r> {
    /// Methods on a Blob field value (`Rec.Blob.CreateOutStream(...)`); `base` is the field expression.
    pub fn blob_method(&mut self, f: &mut Frame, bytes: Vec<u8>, base: &Expr, m: &str, args: &[Expr]) -> R<Val> {
        match m {
            "createoutstream" | "createinstream" => {
                let target = self.lvalue(f, base)?;
                let Some(sarg) = args.first() else { return err(format!("{m} requires a stream variable")) };
                let slv = self.lvalue(f, sarg)?;
                for a in &args[1..] {
                    let _ = self.eval(f, a)?;
                }
                let stream = if m == "createinstream" {
                    match target {
                        LVal::Field(r, fi) => bound_in_stream(&r, fi, bytes),
                        _ => in_stream(bytes),
                    }
                } else {
                    match target {
                        LVal::Field(r, fi) => {
                            // BC's CreateOutStream keeps the field's NavBLOB: InStreams made
                            // before it still read what it held (DESCryptoServiceProvider Test)
                            r.borrow_mut().row[fi] = Value::Blob(vec![]);
                            Val::OutStream(Rc::new(OutTarget::Field(r, fi)))
                        }
                        _ => Val::OutStream(Rc::new(OutTarget::Buffer(Rc::new(RefCell::new(vec![]))))),
                    }
                };
                self.write_lval(&slv, stream)?;
                Ok(Val::Void)
            }
            "hasvalue" => Ok(Val::Bool(!bytes.is_empty())),
            "length" => Ok(Val::Int(bytes.len() as i32)),
            // Blob.Import(FileName) / Blob.Export(FileName): a server file (the session's
            // crate::files::Vfs) into or out of the field; both return the file name
            "import" | "export" if !args.is_empty() => {
                let path = self.eval(f, &args[0])?.to_text();
                for a in &args[1..] {
                    let _ = self.eval(f, a)?;
                }
                if m == "export" {
                    self.files.write(&path, bytes);
                    return Ok(Val::Text(path));
                }
                let Some(content) = self.files.read(&path) else {
                    return err(format!("The file '{}' does not exist.", crate::files::full_path(&path)));
                };
                match self.lvalue(f, base)? {
                    LVal::Field(r, fi) => {
                        notify_set(&r, fi, &content);
                        r.borrow_mut().row[fi] = Value::Blob(content);
                    }
                    lv => self.write_lval(&lv, Val::Blob(content))?,
                }
                Ok(Val::Text(path))
            }
            _ => {
                *self.missing.entry(format!("Blob.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: Blob.{m}"))
            }
        }
    }

    pub fn instream_method(&mut self, f: &mut Frame, s: &Rc<RefCell<InStreamState>>, m: &str, args: &[Expr]) -> R<Val> {
        match m {
            "eos" => {
                let st = s.borrow();
                Ok(Val::Bool(st.pos >= st.data.len()))
            }
            "length" => Ok(Val::Int(s.borrow().data.len() as i32)),
            "position" => {
                if let Some(a) = args.first() {
                    let p = self.eval(f, a)?.as_i64().max(1) as usize - 1;
                    let mut st = s.borrow_mut();
                    st.pos = p.min(st.data.len());
                }
                Ok(Val::Int(s.borrow().pos as i32 + 1))
            }
            "resetposition" => {
                s.borrow_mut().pos = 0;
                Ok(Val::Void)
            }
            "readtext" | "read" => {
                let Some(target) = args.first() else { return err(format!("InStream.{m} requires a variable")) };
                let lv = self.lvalue(f, target)?;
                let max = match args.get(1) {
                    Some(a) => Some(self.eval(f, a)?.as_i64().max(0) as usize),
                    None => None,
                };
                let cur = self.read_lval(&lv);
                let (val, n) = {
                    let mut st = s.borrow_mut();
                    let data = st.data.clone();
                    let rest = &data[st.pos.min(data.len())..];
                    match (&cur, m) {
                        (Val::Int(_), "read") | (Val::BigInt(_), "read") | (Val::Bool(_), "read") | (Val::Guid(_), "read") => {
                            let size = match cur {
                                Val::Int(_) => 4,
                                Val::BigInt(_) => 8,
                                Val::Bool(_) => 1,
                                _ => 16,
                            };
                            let take = size.min(rest.len());
                            let b = &rest[..take];
                            let v = match cur {
                                Val::Int(_) => Val::Int(i32::from_le_bytes(pad::<4>(b))),
                                Val::BigInt(_) => Val::BigInt(i64::from_le_bytes(pad::<8>(b))),
                                Val::Bool(_) => Val::Bool(b.first().is_some_and(|x| *x != 0)),
                                _ => Val::Guid(pad::<16>(b)),
                            };
                            st.pos += take;
                            (v, take)
                        }
                        _ if m == "readtext" => {
                            // ALStream.ReadText (StopOnZeroOrLFIgnoreCR): up to a line feed or a zero
                            // byte, which are consumed; every CR is dropped; at most `max` characters
                            // are kept (the terminator after them stays unread). Returns the byte
                            // count of the kept text, so an empty line gives 0.
                            let limit = max.unwrap_or(usize::MAX);
                            let (mut kept, mut chars, mut i) = (Vec::new(), 0usize, 0usize);
                            while i < rest.len() {
                                let b = rest[i];
                                if b == b'\n' || b == 0 {
                                    i += 1;
                                    break;
                                }
                                if b == b'\r' {
                                    i += 1;
                                    continue;
                                }
                                if chars >= limit {
                                    break;
                                }
                                let n = (1 + rest[i + 1..].iter().take_while(|c| (**c & 0xC0) == 0x80).count()).min(4);
                                kept.extend_from_slice(&rest[i..i + n]);
                                i += n;
                                chars += 1;
                            }
                            st.pos += i;
                            let n = kept.len();
                            (Val::Text(String::from_utf8_lossy(&kept).into_owned()), n)
                        }
                        _ => {
                            // Read: up to a NUL or the end
                            let mut end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
                            if let Some(mx) = max {
                                end = end.min(char_boundary(rest, mx));
                            }
                            let consumed = if end < rest.len() && (rest[end] == b'\n' || rest[end] == 0) && max.is_none_or(|mx| end < mx || mx == 0) { end + 1 } else { end };
                            let text = String::from_utf8_lossy(&rest[..end]).into_owned();
                            st.pos += consumed;
                            (Val::Text(text), consumed)
                        }
                    }
                };
                self.write_lval(&lv, val)?;
                Ok(Val::Int(n as i32))
            }
            _ => {
                *self.missing.entry(format!("InStream.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: InStream.{m}"))
            }
        }
    }

    pub fn outstream_method(&mut self, f: &mut Frame, t: &Rc<OutTarget>, m: &str, args: &[Expr]) -> R<Val> {
        let mut a = Vec::with_capacity(args.len());
        for x in args {
            a.push(self.eval(f, x)?);
        }
        match m {
            "writetext" | "write" => {
                let mut bytes = match (m, a.first()) {
                    ("writetext", Some(v)) => v.to_text().into_bytes(),
                    ("writetext", None) => b"\r\n".to_vec(),
                    (_, Some(v)) => value_bytes(v),
                    (_, None) => vec![],
                };
                if let Some(n) = a.get(1).map(|v| v.as_i64().max(0) as usize) {
                    bytes.truncate(char_boundary(&bytes, n));
                }
                append(t, &bytes);
                Ok(Val::Int(bytes.len() as i32))
            }
            _ => {
                *self.missing.entry(format!("OutStream.{m}")).or_default() += 1;
                err(format!("Not implemented in bc-rust: OutStream.{m}"))
            }
        }
    }

    pub fn outstream_write_text(&mut self, t: &Rc<OutTarget>, s: &str) {
        append(t, s.as_bytes());
    }

    /// CopyStream(OutStream, InStream [, Bytes]).
    pub fn copy_stream(&mut self, f: &mut Frame, args: &[Expr]) -> R<Val> {
        let out = self.eval(f, &args[0])?;
        let inp = self.eval(f, &args[1])?;
        let n = match args.get(2) {
            Some(a) => Some(self.eval(f, a)?.as_i64().max(0) as usize),
            None => None,
        };
        match (out, inp) {
            (Val::OutStream(t), Val::InStream(s)) => {
                let bytes = {
                    let mut st = s.borrow_mut();
                    let data = st.data.clone();
                    let rest = &data[st.pos.min(data.len())..];
                    let take = n.unwrap_or(rest.len()).min(rest.len());
                    st.pos += take;
                    rest[..take].to_vec()
                };
                append(&t, &bytes);
                Ok(Val::Bool(true))
            }
            // DotNet GZipStream (crate::gzip): compress what is copied in, decompress what is read
            (Val::Net(g), Val::InStream(s)) if crate::gzip::is_gzip(&g) => {
                let bytes = {
                    let mut st = s.borrow_mut();
                    let data = st.data.clone();
                    let rest = &data[st.pos.min(data.len())..];
                    let take = n.unwrap_or(rest.len()).min(rest.len());
                    st.pos += take;
                    rest[..take].to_vec()
                };
                crate::gzip::write(&g, &bytes)?;
                Ok(Val::Bool(true))
            }
            (Val::OutStream(t), Val::Net(g)) if crate::gzip::is_gzip(&g) => {
                let bytes = crate::gzip::read(&g, n)?;
                append(&t, &bytes);
                Ok(Val::Bool(true))
            }
            (o, i) if matches!(o, Val::Net(_)) || matches!(i, Val::Net(_)) => crate::dotnet::text::copy_stream(&o, &i, n),
            _ => err("CopyStream requires an OutStream and an InStream that are initialized"),
        }
    }
}

fn pad<const N: usize>(b: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    out[..b.len().min(N)].copy_from_slice(&b[..b.len().min(N)]);
    out
}

/// Byte offset of the `chars`-th character (UTF-8), capped at the slice length.
fn char_boundary(b: &[u8], chars: usize) -> usize {
    let s = String::from_utf8_lossy(b);
    s.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(b.len()).min(b.len())
}
