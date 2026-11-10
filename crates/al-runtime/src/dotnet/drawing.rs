//! System.Drawing: Bitmap, Graphics (Clear, FillRectangle, FillEllipse), Color, ColorTranslator,
//! SolidBrush and ImageFormat, with a small PNG encoder (stored deflate blocks) for Bitmap.Save.
//! Enough for the pictures BC generates (brand colors); no text rendering or image decoding.

use super::*;

pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    /// RGBA, row-major.
    pub px: Vec<[u8; 4]>,
}

pub struct ImageData {
    pub bytes: Vec<u8>,
    pub w: u32,
    pub h: u32,
    /// Lower-case ImageFormat name: png, jpeg, gif, bmp, tiff, icon.
    pub fmt: &'static str,
}

/// Size and format from the encoded image's header (None = not an image).
pub fn sniff(b: &[u8]) -> Option<(u32, u32, &'static str)> {
    let be32 = |i: usize| -> Option<u32> { Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?)) };
    let le16 = |i: usize| -> Option<u32> { Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32) };
    let le32 = |i: usize| -> Option<i32> { Some(i32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)) };
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some((be32(16)?, be32(20)?, "png"));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?, "gif"));
    }
    if b.starts_with(b"BM") {
        return Some((le32(18)?.unsigned_abs(), le32(22)?.unsigned_abs(), "bmp"));
    }
    if b.starts_with(&[0, 0, 1, 0]) {
        return Some((*b.get(6)? as u32, *b.get(7)? as u32, "icon"));
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some((0, 0, "tiff"));
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        // walk the JPEG segments to a start-of-frame marker
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some((w, h, "jpeg"));
            }
            i += 2 + len;
        }
        return None;
    }
    None
}

fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

fn rgba(c: u32) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, (c >> 24) as u8]
}

const NAMED: &[(&str, u32)] = &[
    ("black", 0xFF000000),
    ("white", 0xFFFFFFFF),
    ("red", 0xFFFF0000),
    ("green", 0xFF008000),
    ("lime", 0xFF00FF00),
    ("blue", 0xFF0000FF),
    ("yellow", 0xFFFFFF00),
    ("gray", 0xFF808080),
    ("grey", 0xFF808080),
    ("silver", 0xFFC0C0C0),
    ("orange", 0xFFFFA500),
    ("purple", 0xFF800080),
    ("navy", 0xFF000080),
    ("teal", 0xFF008080),
    ("maroon", 0xFF800000),
    ("olive", 0xFF808000),
    ("aqua", 0xFF00FFFF),
    ("fuchsia", 0xFFFF00FF),
    ("transparent", 0x00FFFFFF),
];

/// ColorTranslator.FromHtml: `#RGB`, `#RRGGBB`, `#AARRGGBB` or a known color name.
pub fn from_html(s: &str) -> Result<u32, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(0);
    }
    if let Some(h) = t.strip_prefix('#') {
        let v = u32::from_str_radix(h, 16).map_err(|_| format!("{t} is not a valid value for Int32."))?;
        return Ok(match h.len() {
            3 => {
                let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
                argb(255, (r * 17) as u8, (g * 17) as u8, (b * 17) as u8)
            }
            6 => 0xFF00_0000 | v,
            8 => v,
            _ => return Err(format!("{t} is not a valid value for Int32.")),
        });
    }
    NAMED.iter().find(|(n, _)| n.eq_ignore_ascii_case(t)).map(|(_, c)| *c).ok_or_else(|| format!("{t} is not a valid value for Int32."))
}

fn color_of(v: Option<&Val>) -> u32 {
    match v {
        Some(Val::Net(n)) => match &*n.borrow() {
            NetObj::Color(c) | NetObj::Brush(c) => *c,
            _ => 0xFF000000,
        },
        Some(v) => v.as_i64() as u32,
        None => 0xFF000000,
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// PNG (RGBA, 8 bit) with uncompressed (stored) deflate blocks.
pub fn encode_png(bmp: &Bitmap) -> Vec<u8> {
    let mut raw = Vec::with_capacity(bmp.h * (bmp.w * 4 + 1));
    for y in 0..bmp.h {
        raw.push(0);
        for x in 0..bmp.w {
            raw.extend_from_slice(&bmp.px[y * bmp.w + x]);
        }
    }
    // zlib: header, stored blocks, adler32
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, c) in chunks.iter().enumerate() {
        z.push(if i + 1 == chunks.len() { 1 } else { 0 });
        let len = c.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(c);
    }
    if chunks.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + *x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    };
    let mut ihdr = vec![];
    ihdr.extend_from_slice(&(bmp.w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(bmp.h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    out
}

fn fill(bmp: &mut Bitmap, c: u32, inside: impl Fn(f64, f64) -> bool) {
    let p = rgba(c);
    for y in 0..bmp.h {
        for x in 0..bmp.w {
            if inside(x as f64 + 0.5, y as f64 + 0.5) {
                bmp.px[y * bmp.w + x] = p;
            }
        }
    }
}

/// Write image bytes to a Save target: an AL InStream (its content is replaced, as BC's Temp Blob
/// streams share one buffer), an OutStream, or a .NET stream.
fn save_to(target: Option<&Val>, bytes: Vec<u8>) {
    match target {
        // the InStream's Blob field, at the stream's position (crate::streams::in_stream_write)
        Some(Val::InStream(s)) => crate::streams::in_stream_write(s, &bytes),
        Some(t) => super::text::stream_write(t, &bytes),
        None => {}
    }
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    Ok(match ty {
        "System.Drawing.SolidBrush" => net(NetObj::Brush(color_of(a.raw(0)))),
        // Bitmap(Image [, w, h]): a blank bitmap of the image's (or the given) size
        "System.Drawing.Bitmap" if matches!(a.raw(0), Some(Val::Net(_))) => {
            let (w, h) = match a.raw(0) {
                Some(Val::Net(x)) => match &*x.borrow() {
                    NetObj::Image(i) => (i.w as usize, i.h as usize),
                    NetObj::Bitmap(b) => (b.w, b.h),
                    _ => (1, 1),
                },
                _ => (1, 1),
            };
            let (w, h) = if a.len() >= 3 { (a.int(1).max(1) as usize, a.int(2).max(1) as usize) } else { (w.max(1), h.max(1)) };
            net(NetObj::Bitmap(Bitmap { w, h, px: vec![[0, 0, 0, 0]; w * h] }))
        }
        "System.Drawing.Bitmap" => {
            let (w, h) = (a.int(0).clamp(1, 10_000) as usize, a.int(1).clamp(1, 10_000) as usize);
            net(NetObj::Bitmap(Bitmap { w, h, px: vec![[0, 0, 0, 0]; w * h] }))
        }
        "System.Drawing.ImageFormatConverter" => net(NetObj::Prim(ty, Val::Void)),
        // Image Impl.EncodeToImage's quality settings (Encoder(Guid), EncoderParameter(Encoder, N),
        // EncoderParameters(N).Param) and Rectangle(X, Y, W, H): inert values here, Bitmap.Save
        // writes the bitmap as PNG whatever the codec and parameters
        "System.Drawing.Imaging.Encoder" | "System.Drawing.Imaging.EncoderParameter" | "System.Drawing.Imaging.EncoderParameters" | "System.Drawing.Rectangle" => {
            net(NetObj::Prim(ty, Val::Text((0..a.len()).map(|i| a.get(i).to_text()).collect::<Vec<_>>().join(","))))
        }
        _ => net(NetObj::Null(ty)),
    })
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    match (ty, m) {
        ("System.Drawing.Image", "fromstream") | ("System.Drawing.Image", "fromfile") | ("System.Drawing.Bitmap", "fromstream") => {
            let bytes = super::text::arg_bytes(a.raw(0));
            if let Some(Val::InStream(s)) = a.raw(0) {
                let mut st = s.borrow_mut();
                st.pos = st.data.len();
            }
            return match sniff(&bytes) {
                Some((w, h, fmt)) => ok(net(NetObj::Image(Rc::new(ImageData { bytes, w, h, fmt })))),
                None => net_err("Image", "FromStream", "Parameter is not valid."),
            };
        }
        ("System.Drawing.ImageFormatConverter", "converttostring") | ("System.Drawing.ImageFormatConverter", "converttoinvariantstring") | ("System.Drawing.Imaging.ImageFormat", "tostring") => {
            let v = if ty.ends_with("ImageFormat") { Val::Net(n.clone()) } else { a.get(0) };
            let name = match &v {
                Val::Net(x) => match &*x.borrow() {
                    NetObj::Prim(_, f) => f.to_text(),
                    _ => String::new(),
                },
                o => o.to_text(),
            };
            let mut c = name.chars();
            return ok(Val::Text(c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()));
        }
        ("System.Drawing.ColorTranslator", "fromhtml") => {
            return match from_html(&a.text(0)) {
                Ok(c) => ok(net(NetObj::Color(c))),
                Err(e) => net_err("ColorTranslator", "FromHtml", e),
            }
        }
        ("System.Drawing.ColorTranslator", "tohtml") => {
            let c = color_of(a.raw(0));
            return ok(Val::Text(format!("#{:06X}", c & 0xFF_FFFF)));
        }
        ("System.Drawing.Color", "fromargb") => {
            let c = match a.len() {
                1 => a.int(0) as u32,
                3 => argb(255, a.int(0) as u8, a.int(1) as u8, a.int(2) as u8),
                _ => argb(a.int(0) as u8, a.int(1) as u8, a.int(2) as u8, a.int(3) as u8),
            };
            return ok(net(NetObj::Color(c)));
        }
        ("System.Drawing.Color", name) if matches!(&*n.borrow(), NetObj::Null(_)) => {
            if let Some((_, c)) = NAMED.iter().find(|(x, _)| *x == name) {
                return ok(net(NetObj::Color(*c)));
            }
        }
        ("System.Drawing.Imaging.EncoderParameters", "param") => return void(),
        ("System.Drawing.Rectangle", f) if matches!(f, "x" | "y" | "width" | "height") => {
            let parts: Vec<i32> = match &*n.borrow() {
                NetObj::Prim(_, Val::Text(t)) => t.split(',').map(|x| x.trim().parse().unwrap_or(0)).collect(),
                _ => vec![],
            };
            let i = ["x", "y", "width", "height"].iter().position(|x| *x == f).unwrap_or(0);
            return ok(Val::Int(parts.get(i).copied().unwrap_or(0)));
        }
        ("System.Drawing.Graphics", "fromimage") => {
            return match a.net(0) {
                Some(b) if matches!(&*b.borrow(), NetObj::Bitmap(_)) => ok(net(NetObj::Graphics(b))),
                _ => null_ref("Graphics", "FromImage"),
            }
        }
        ("System.Drawing.Imaging.ImageFormat", f) if matches!(f, "png" | "jpeg" | "bmp" | "gif" | "tiff" | "icon" | "emf" | "wmf" | "exif") => {
            return ok(net(NetObj::Prim("System.Drawing.Imaging.ImageFormat", Val::Text(f.to_string()))));
        }
        _ => {}
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Image(img) => {
            let img = img.clone();
            drop(o);
            ok(match m {
                "width" => Val::Int(img.w as i32),
                "height" => Val::Int(img.h as i32),
                "rawformat" => net(NetObj::Prim("System.Drawing.Imaging.ImageFormat", Val::Text(img.fmt.to_string()))),
                "save" => {
                    save_to(a.raw(0), img.bytes.clone());
                    Val::Void
                }
                "clone" => net(NetObj::Image(img.clone())),
                "dispose" => Val::Void,
                "horizontalresolution" | "verticalresolution" => Val::Dec(rust_decimal::Decimal::from(96)),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Bitmap(b) => ok(match m {
            "width" => Val::Int(b.w as i32),
            "height" => Val::Int(b.h as i32),
            "save" => {
                let png = encode_png(b);
                drop(o);
                save_to(a.raw(0), png);
                Val::Void
            }
            "setpixel" => {
                let (x, y) = (a.int(0) as usize, a.int(1) as usize);
                if x < b.w && y < b.h {
                    b.px[y * b.w + x] = rgba(color_of(a.raw(2)));
                }
                Val::Void
            }
            "getpixel" => {
                let (x, y) = (a.int(0) as usize, a.int(1) as usize);
                let p = b.px.get(y * b.w + x).copied().unwrap_or([0; 4]);
                net(NetObj::Color(argb(p[3], p[0], p[1], p[2])))
            }
            "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::Graphics(bref) => {
            let bref = bref.clone();
            drop(o);
            let mut bo = bref.borrow_mut();
            let NetObj::Bitmap(b) = &mut *bo else { return Ok(Out::Unknown) };
            let c = color_of(a.raw(0));
            let (x, y, w, h) = (a.get(1).as_dec(), a.get(2).as_dec(), a.get(3).as_dec(), a.get(4).as_dec());
            let f = |d: rust_decimal::Decimal| d.to_string().parse::<f64>().unwrap_or(0.0);
            let (x, y, w, h) = (f(x), f(y), f(w), f(h));
            ok(match m {
                "clear" => {
                    fill(b, c, |_, _| true);
                    Val::Void
                }
                "fillrectangle" => {
                    fill(b, c, |px, py| px >= x && px < x + w && py >= y && py < y + h);
                    Val::Void
                }
                "fillellipse" => {
                    let (cx, cy, rx, ry) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
                    if rx > 0.0 && ry > 0.0 {
                        fill(b, c, |px, py| ((px - cx) / rx).powi(2) + ((py - cy) / ry).powi(2) <= 1.0);
                    }
                    Val::Void
                }
                "dispose" | "flush" | "smoothingmode" | "interpolationmode" | "drawimage" | "drawstring" | "drawrectangle" | "drawellipse" | "drawline" => Val::Void,
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Color(c) => {
            let c = *c;
            ok(match m {
                "a" => Val::Int((c >> 24) as i32 & 0xFF),
                "r" => Val::Int((c >> 16) as i32 & 0xFF),
                "g" => Val::Int((c >> 8) as i32 & 0xFF),
                "b" => Val::Int(c as i32 & 0xFF),
                "toargb" => Val::Int(c as i32),
                "name" | "tostring" => Val::Text(NAMED.iter().find(|(_, x)| *x == c).map(|(n, _)| n.to_string()).unwrap_or_else(|| format!("{c:x}"))),
                "isempty" => Val::Bool(c == 0),
                _ => return Ok(Out::Unknown),
            })
        }
        NetObj::Brush(_) => match m {
            "dispose" => void(),
            "color" => ok(net(NetObj::Color(color_of(Some(&Val::Net(n.clone())))))),
            _ => Ok(Out::Unknown),
        },
        _ => Ok(Out::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_has_signature_and_chunks() {
        let b = Bitmap { w: 3, h: 2, px: vec![[255, 0, 0, 255]; 6] };
        let p = encode_png(&b);
        assert_eq!(&p[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        assert_eq!(&p[12..16], b"IHDR");
        assert_eq!(&p[p.len() - 8..p.len() - 4], b"IEND");
        assert_eq!(from_html("#FF0000").unwrap(), 0xFFFF0000);
    }
}
