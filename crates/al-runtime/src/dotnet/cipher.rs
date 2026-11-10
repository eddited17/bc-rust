//! Symmetric encryption: RijndaelManaged / Aes (AES, FIPS-197, 128-bit blocks as in .NET Core),
//! DES and TripleDES (FIPS 46-3, EDE with 2 or 3 keys) in CBC, ECB and CFB modes with None / PKCS7 / Zeros / ANSIX923 / ISO10126 padding,
//! ICryptoTransform, CryptoStream (write mode buffers until FlushFinalBlock / Close; read mode
//! transforms its source up front) and KeySizes.

use super::*;
use std::sync::OnceLock;

pub const TYPES: &[&str] = &[
    "System.Security.Cryptography.RijndaelManaged",
    "System.Security.Cryptography.Rijndael",
    "System.Security.Cryptography.Aes",
    "System.Security.Cryptography.AesManaged",
    "System.Security.Cryptography.AesCryptoServiceProvider",
    "System.Security.Cryptography.SymmetricAlgorithm",
    "System.Security.Cryptography.DES",
    "System.Security.Cryptography.DESCryptoServiceProvider",
    "System.Security.Cryptography.TripleDES",
    "System.Security.Cryptography.TripleDESCryptoServiceProvider",
    "System.Security.Cryptography.ICryptoTransform",
    "System.Security.Cryptography.CryptoStream",
    "System.Security.Cryptography.KeySizes",
    "System.Security.Cryptography.CipherMode",
    "System.Security.Cryptography.PaddingMode",
    "System.Security.Cryptography.CryptoStreamMode",
];

const KEY_SIZES: &str = "System.Security.Cryptography.KeySizes";
pub const CBC: i32 = 1;
const ECB: i32 = 2;
const CFB: i32 = 4;
const PAD_NONE: i32 = 1;
pub const PKCS7: i32 = 2;
const ZEROS: i32 = 3;
const ANSIX923: i32 = 4;
const ISO10126: i32 = 5;

/// Algorithm settings (shared by the algorithm object and the transforms it creates).
#[derive(Clone)]
pub struct Sym {
    pub ty: &'static str,
    pub key: Vec<u8>,
    pub iv: Vec<u8>,
    pub mode: i32,
    pub padding: i32,
    /// feedback size in bytes (CFB)
    pub feedback: usize,
}

/// A CryptoStream in write mode.
pub struct CryptoW {
    pub target: Val,
    pub sym: Sym,
    pub encrypt: bool,
    pub buf: Vec<u8>,
    pub done: bool,
}

pub fn enum_names(t: &str) -> Option<&'static [(&'static str, i64)]> {
    Some(match t {
        "System.Security.Cryptography.CipherMode" => &[("CBC", 1), ("ECB", 2), ("OFB", 3), ("CFB", 4), ("CTS", 5)],
        "System.Security.Cryptography.PaddingMode" => &[("None", 1), ("PKCS7", 2), ("Zeros", 3), ("ANSIX923", 4), ("ISO10126", 5)],
        "System.Security.Cryptography.CryptoStreamMode" => &[("Read", 0), ("Write", 1)],
        _ => return super::x509::enum_names(t),
    })
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    random(n)
}

fn random(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        out.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    out.truncate(n);
    out
}

// ---------------------------------------------------------------- AES block cipher

fn gmul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0;
    while b != 0 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

/// (S-box, inverse S-box), derived from the GF(2^8) inverse and the affine map.
fn sboxes() -> &'static ([u8; 256], [u8; 256]) {
    static S: OnceLock<([u8; 256], [u8; 256])> = OnceLock::new();
    S.get_or_init(|| {
        let (mut s, mut inv) = ([0u8; 256], [0u8; 256]);
        for i in 0..256usize {
            let x = i as u8;
            let y = if x == 0 { 0 } else { (1..=255u8).find(|c| gmul(x, *c) == 1).unwrap_or(0) };
            let r = y ^ y.rotate_left(1) ^ y.rotate_left(2) ^ y.rotate_left(3) ^ y.rotate_left(4) ^ 0x63;
            s[i] = r;
            inv[r as usize] = x;
        }
        (s, inv)
    })
}

pub struct Aes {
    rk: Vec<[u8; 16]>,
}

impl Aes {
    /// Key of 16, 24 or 32 bytes.
    pub fn new(key: &[u8]) -> Aes {
        let (s, _) = sboxes();
        let nk = key.len() / 4;
        let nr = nk + 6;
        let mut w: Vec<[u8; 4]> = key.chunks(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
        let mut rcon = 1u8;
        for i in nk..4 * (nr + 1) {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [s[t[1] as usize] ^ rcon, s[t[2] as usize], s[t[3] as usize], s[t[0] as usize]];
                rcon = gmul(rcon, 2);
            } else if nk > 6 && i % nk == 4 {
                t = t.map(|b| s[b as usize]);
            }
            let p = w[i - nk];
            w.push([p[0] ^ t[0], p[1] ^ t[1], p[2] ^ t[2], p[3] ^ t[3]]);
        }
        let rk = w
            .chunks(4)
            .map(|c| {
                let mut r = [0u8; 16];
                for (j, word) in c.iter().enumerate() {
                    r[4 * j..4 * j + 4].copy_from_slice(word);
                }
                r
            })
            .collect();
        Aes { rk }
    }

    fn add(b: &mut [u8; 16], k: &[u8; 16]) {
        for (x, y) in b.iter_mut().zip(k) {
            *x ^= y;
        }
    }

    pub fn encrypt(&self, b: &mut [u8; 16]) {
        let (s, _) = sboxes();
        let nr = self.rk.len() - 1;
        Self::add(b, &self.rk[0]);
        for round in 1..=nr {
            let mut t = [0u8; 16];
            // SubBytes + ShiftRows
            for c in 0..4 {
                for r in 0..4 {
                    t[r + 4 * c] = s[b[r + 4 * ((c + r) % 4)] as usize];
                }
            }
            if round < nr {
                for c in 0..4 {
                    let (a0, a1, a2, a3) = (t[4 * c], t[4 * c + 1], t[4 * c + 2], t[4 * c + 3]);
                    t[4 * c] = gmul(a0, 2) ^ gmul(a1, 3) ^ a2 ^ a3;
                    t[4 * c + 1] = a0 ^ gmul(a1, 2) ^ gmul(a2, 3) ^ a3;
                    t[4 * c + 2] = a0 ^ a1 ^ gmul(a2, 2) ^ gmul(a3, 3);
                    t[4 * c + 3] = gmul(a0, 3) ^ a1 ^ a2 ^ gmul(a3, 2);
                }
            }
            *b = t;
            Self::add(b, &self.rk[round]);
        }
    }

    pub fn decrypt(&self, b: &mut [u8; 16]) {
        let (_, inv) = sboxes();
        let nr = self.rk.len() - 1;
        Self::add(b, &self.rk[nr]);
        for round in (0..nr).rev() {
            let mut t = [0u8; 16];
            // InvShiftRows + InvSubBytes
            for c in 0..4 {
                for r in 0..4 {
                    t[r + 4 * ((c + r) % 4)] = inv[b[r + 4 * c] as usize];
                }
            }
            Self::add(&mut t, &self.rk[round]);
            if round > 0 {
                for c in 0..4 {
                    let (a0, a1, a2, a3) = (t[4 * c], t[4 * c + 1], t[4 * c + 2], t[4 * c + 3]);
                    t[4 * c] = gmul(a0, 14) ^ gmul(a1, 11) ^ gmul(a2, 13) ^ gmul(a3, 9);
                    t[4 * c + 1] = gmul(a0, 9) ^ gmul(a1, 14) ^ gmul(a2, 11) ^ gmul(a3, 13);
                    t[4 * c + 2] = gmul(a0, 13) ^ gmul(a1, 9) ^ gmul(a2, 14) ^ gmul(a3, 11);
                    t[4 * c + 3] = gmul(a0, 11) ^ gmul(a1, 13) ^ gmul(a2, 9) ^ gmul(a3, 14);
                }
            }
            *b = t;
        }
    }
}

// ---------------------------------------------------------------- DES

#[rustfmt::skip]
const IP: [u8; 64] = [58, 50, 42, 34, 26, 18, 10, 2, 60, 52, 44, 36, 28, 20, 12, 4, 62, 54, 46, 38, 30, 22, 14, 6, 64, 56, 48, 40, 32, 24, 16, 8,
    57, 49, 41, 33, 25, 17, 9, 1, 59, 51, 43, 35, 27, 19, 11, 3, 61, 53, 45, 37, 29, 21, 13, 5, 63, 55, 47, 39, 31, 23, 15, 7];
#[rustfmt::skip]
const E: [u8; 48] = [32, 1, 2, 3, 4, 5, 4, 5, 6, 7, 8, 9, 8, 9, 10, 11, 12, 13, 12, 13, 14, 15, 16, 17,
    16, 17, 18, 19, 20, 21, 20, 21, 22, 23, 24, 25, 24, 25, 26, 27, 28, 29, 28, 29, 30, 31, 32, 1];
#[rustfmt::skip]
const P: [u8; 32] = [16, 7, 20, 21, 29, 12, 28, 17, 1, 15, 23, 26, 5, 18, 31, 10, 2, 8, 24, 14, 32, 27, 3, 9, 19, 13, 30, 6, 22, 11, 4, 25];
#[rustfmt::skip]
const PC1: [u8; 56] = [57, 49, 41, 33, 25, 17, 9, 1, 58, 50, 42, 34, 26, 18, 10, 2, 59, 51, 43, 35, 27, 19, 11, 3, 60, 52, 44, 36,
    63, 55, 47, 39, 31, 23, 15, 7, 62, 54, 46, 38, 30, 22, 14, 6, 61, 53, 45, 37, 29, 21, 13, 5, 28, 20, 12, 4];
#[rustfmt::skip]
const PC2: [u8; 48] = [14, 17, 11, 24, 1, 5, 3, 28, 15, 6, 21, 10, 23, 19, 12, 4, 26, 8, 16, 7, 27, 20, 13, 2,
    41, 52, 31, 37, 47, 55, 30, 40, 51, 45, 33, 48, 44, 49, 39, 56, 34, 53, 46, 42, 50, 36, 29, 32];
const SHIFTS: [u32; 16] = [1, 1, 2, 2, 2, 2, 2, 2, 1, 2, 2, 2, 2, 2, 2, 1];
#[rustfmt::skip]
const SBOX: [[u8; 64]; 8] = [
    [14, 4, 13, 1, 2, 15, 11, 8, 3, 10, 6, 12, 5, 9, 0, 7, 0, 15, 7, 4, 14, 2, 13, 1, 10, 6, 12, 11, 9, 5, 3, 8,
     4, 1, 14, 8, 13, 6, 2, 11, 15, 12, 9, 7, 3, 10, 5, 0, 15, 12, 8, 2, 4, 9, 1, 7, 5, 11, 3, 14, 10, 0, 6, 13],
    [15, 1, 8, 14, 6, 11, 3, 4, 9, 7, 2, 13, 12, 0, 5, 10, 3, 13, 4, 7, 15, 2, 8, 14, 12, 0, 1, 10, 6, 9, 11, 5,
     0, 14, 7, 11, 10, 4, 13, 1, 5, 8, 12, 6, 9, 3, 2, 15, 13, 8, 10, 1, 3, 15, 4, 2, 11, 6, 7, 12, 0, 5, 14, 9],
    [10, 0, 9, 14, 6, 3, 15, 5, 1, 13, 12, 7, 11, 4, 2, 8, 13, 7, 0, 9, 3, 4, 6, 10, 2, 8, 5, 14, 12, 11, 15, 1,
     13, 6, 4, 9, 8, 15, 3, 0, 11, 1, 2, 12, 5, 10, 14, 7, 1, 10, 13, 0, 6, 9, 8, 7, 4, 15, 14, 3, 11, 5, 2, 12],
    [7, 13, 14, 3, 0, 6, 9, 10, 1, 2, 8, 5, 11, 12, 4, 15, 13, 8, 11, 5, 6, 15, 0, 3, 4, 7, 2, 12, 1, 10, 14, 9,
     10, 6, 9, 0, 12, 11, 7, 13, 15, 1, 3, 14, 5, 2, 8, 4, 3, 15, 0, 6, 10, 1, 13, 8, 9, 4, 5, 11, 12, 7, 2, 14],
    [2, 12, 4, 1, 7, 10, 11, 6, 8, 5, 3, 15, 13, 0, 14, 9, 14, 11, 2, 12, 4, 7, 13, 1, 5, 0, 15, 10, 3, 9, 8, 6,
     4, 2, 1, 11, 10, 13, 7, 8, 15, 9, 12, 5, 6, 3, 0, 14, 11, 8, 12, 7, 1, 14, 2, 13, 6, 15, 0, 9, 10, 4, 5, 3],
    [12, 1, 10, 15, 9, 2, 6, 8, 0, 13, 3, 4, 14, 7, 5, 11, 10, 15, 4, 2, 7, 12, 9, 5, 6, 1, 13, 14, 0, 11, 3, 8,
     9, 14, 15, 5, 2, 8, 12, 3, 7, 0, 4, 10, 1, 13, 11, 6, 4, 3, 2, 12, 9, 5, 15, 10, 11, 14, 1, 7, 6, 0, 8, 13],
    [4, 11, 2, 14, 15, 0, 8, 13, 3, 12, 9, 7, 5, 10, 6, 1, 13, 0, 11, 7, 4, 9, 1, 10, 14, 3, 5, 12, 2, 15, 8, 6,
     1, 4, 11, 13, 12, 3, 7, 14, 10, 15, 6, 8, 0, 5, 9, 2, 6, 11, 13, 8, 1, 4, 10, 7, 9, 5, 0, 15, 14, 2, 3, 12],
    [13, 2, 8, 4, 6, 15, 11, 1, 10, 9, 3, 14, 5, 0, 12, 7, 1, 15, 13, 8, 10, 3, 7, 4, 12, 5, 6, 11, 0, 14, 9, 2,
     7, 11, 4, 1, 9, 12, 14, 2, 0, 6, 10, 13, 15, 3, 5, 8, 2, 1, 14, 7, 4, 10, 8, 13, 15, 12, 9, 0, 3, 5, 6, 11],
];

/// Bit permutation (table entries count from the most significant of `bits` input bits).
fn permute(x: u64, bits: u32, table: &[u8]) -> u64 {
    table.iter().fold(0, |out, &p| (out << 1) | ((x >> (bits - p as u32)) & 1))
}

/// DES subkeys of one 8-byte key.
fn des_keys(key: &[u8]) -> [u64; 16] {
    let k = permute(u64::from_be_bytes(key[..8].try_into().unwrap_or([0; 8])), 64, &PC1);
    let (mut c, mut d) = (k >> 28, k & 0x0fff_ffff);
    let mut out = [0u64; 16];
    for (i, sh) in SHIFTS.iter().enumerate() {
        c = ((c << sh) | (c >> (28 - sh))) & 0x0fff_ffff;
        d = ((d << sh) | (d >> (28 - sh))) & 0x0fff_ffff;
        out[i] = permute((c << 28) | d, 56, &PC2);
    }
    out
}

fn des_block(x: u64, keys: &[u64; 16], enc: bool) -> u64 {
    let x = permute(x, 64, &IP);
    let (mut l, mut r) = (x >> 32, x & 0xffff_ffff);
    for i in 0..16 {
        let k = if enc { keys[i] } else { keys[15 - i] };
        let e = permute(r, 32, &E) ^ k;
        let mut s = 0u64;
        for (j, sbox) in SBOX.iter().enumerate() {
            let six = (e >> (42 - 6 * j)) & 0x3f;
            let row = ((six & 0x20) >> 4) | (six & 1);
            let col = (six >> 1) & 0xf;
            s = (s << 4) | sbox[(row * 16 + col) as usize] as u64;
        }
        let t = r;
        r = l ^ permute(s, 32, &P);
        l = t;
    }
    // final permutation = inverse of IP
    let pre = (r << 32) | l;
    let mut out = 0u64;
    for (i, &p) in IP.iter().enumerate() {
        out |= ((pre >> (63 - i)) & 1) << (64 - p as u32);
    }
    out
}

// ---------------------------------------------------------------- modes and padding

/// Block cipher of an algorithm object.
enum Engine {
    Aes(Aes),
    /// DES (one key schedule) or TripleDES EDE (three)
    Des(Vec<[u64; 16]>),
}

impl Engine {
    fn new(sym: &Sym) -> Engine {
        if is_des(sym.ty) {
            let k = &sym.key;
            let parts: Vec<&[u8]> = match k.len() {
                8 => vec![k],
                16 => vec![&k[..8], &k[8..], &k[..8]],
                _ => vec![&k[..8], &k[8..16], &k[16..24]],
            };
            Engine::Des(parts.into_iter().map(des_keys).collect())
        } else {
            Engine::Aes(Aes::new(&sym.key))
        }
    }

    fn crypt(&self, b: &mut [u8], enc: bool) {
        match self {
            Engine::Aes(a) => {
                let mut x: [u8; 16] = b[..16].try_into().unwrap_or([0; 16]);
                if enc {
                    a.encrypt(&mut x)
                } else {
                    a.decrypt(&mut x)
                }
                b[..16].copy_from_slice(&x);
            }
            Engine::Des(keys) => {
                let mut x = u64::from_be_bytes(b[..8].try_into().unwrap_or([0; 8]));
                if keys.len() == 1 {
                    x = des_block(x, &keys[0], enc);
                } else if enc {
                    x = des_block(des_block(des_block(x, &keys[0], true), &keys[1], false), &keys[2], true);
                } else {
                    x = des_block(des_block(des_block(x, &keys[2], false), &keys[1], true), &keys[0], false);
                }
                b[..8].copy_from_slice(&x.to_be_bytes());
            }
        }
    }
}

fn is_des(ty: &str) -> bool {
    ty.contains(".DES") || ty.contains("TripleDES")
}

fn is_triple(ty: &str) -> bool {
    ty.contains("TripleDES")
}

/// Block size in bytes.
fn block_len(ty: &str) -> usize {
    if is_des(ty) { 8 } else { 16 }
}

fn valid_key_len(ty: &str, n: usize) -> bool {
    if is_triple(ty) {
        matches!(n, 16 | 24)
    } else if is_des(ty) {
        n == 8
    } else {
        matches!(n, 16 | 24 | 32)
    }
}

/// (min, max, skip) key sizes in bits.
fn legal_keys(ty: &str) -> (i32, i32, i32) {
    if is_triple(ty) {
        (128, 192, 64)
    } else if is_des(ty) {
        (64, 64, 0)
    } else {
        (128, 256, 64)
    }
}

fn pad_unit(sym: &Sym) -> usize {
    if sym.mode == CFB { sym.feedback.max(1) } else { block_len(sym.ty) }
}

/// Encrypt (`enc`) or decrypt `data` with padding: the whole-message transform .NET does on
/// TransformFinalBlock / FlushFinalBlock.
pub fn transform(sym: &Sym, enc: bool, data: &[u8]) -> Result<Vec<u8>, String> {
    if !valid_key_len(sym.ty, sym.key.len()) {
        return Err("Specified key is not a valid size for this algorithm.".into());
    }
    if !matches!(sym.mode, CBC | ECB | CFB) {
        return Err("Specified cipher mode is not valid for this algorithm.".into());
    }
    let unit = pad_unit(sym);
    let engine = Engine::new(sym);
    if enc {
        let mut p = data.to_vec();
        let fill = unit - p.len() % unit;
        match sym.padding {
            PAD_NONE if p.len() % unit != 0 => return Err("The input data is not a complete block.".into()),
            PAD_NONE => {}
            ZEROS if p.len() % unit == 0 => {}
            ZEROS => p.resize(p.len() + fill, 0),
            ANSIX923 => {
                p.resize(p.len() + fill - 1, 0);
                p.push(fill as u8);
            }
            ISO10126 => {
                p.extend(random(fill - 1));
                p.push(fill as u8);
            }
            _ => p.extend(std::iter::repeat_n(fill as u8, fill)),
        }
        Ok(run(&engine, sym, true, &p))
    } else {
        if data.len() % unit != 0 {
            return Err("The input data is not a complete block.".into());
        }
        let mut p = run(&engine, sym, false, data);
        let bad = || Err("Padding is invalid and cannot be removed.".to_string());
        match sym.padding {
            PKCS7 | ANSIX923 | ISO10126 => {
                let Some(&last) = p.last() else { return bad() };
                let n = last as usize;
                if n == 0 || n > unit || n > p.len() {
                    return bad();
                }
                let body = p.len() - n;
                let ok = match sym.padding {
                    PKCS7 => p[body..].iter().all(|x| *x == last),
                    ANSIX923 => p[body..p.len() - 1].iter().all(|x| *x == 0),
                    _ => true,
                };
                if !ok {
                    return bad();
                }
                p.truncate(body);
            }
            _ => {}
        }
        Ok(p)
    }
}

/// Block chaining over whole (padded) data.
fn run(engine: &Engine, sym: &Sym, enc: bool, data: &[u8]) -> Vec<u8> {
    let bl = block_len(sym.ty);
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; bl];
    for (i, x) in sym.iv.iter().take(bl).enumerate() {
        prev[i] = *x;
    }
    match sym.mode {
        ECB | CBC => {
            for chunk in data.chunks(bl) {
                let mut b = vec![0u8; bl];
                b[..chunk.len()].copy_from_slice(chunk);
                if enc {
                    if sym.mode == CBC {
                        for k in 0..bl {
                            b[k] ^= prev[k];
                        }
                    }
                    engine.crypt(&mut b, true);
                    prev.copy_from_slice(&b);
                } else {
                    let c = b.clone();
                    engine.crypt(&mut b, false);
                    if sym.mode == CBC {
                        for k in 0..bl {
                            b[k] ^= prev[k];
                        }
                    }
                    prev = c;
                }
                out.extend_from_slice(&b);
            }
        }
        _ => {
            // CFB-s: shift register of the IV, s-byte segments
            let s = sym.feedback.clamp(1, bl);
            let mut reg = prev;
            for seg in data.chunks(s) {
                let mut ks = reg.clone();
                engine.crypt(&mut ks, true);
                let res: Vec<u8> = seg.iter().zip(ks.iter()).map(|(x, k)| x ^ k).collect();
                let cipher = if enc { &res[..] } else { seg };
                reg.copy_within(s.., 0);
                reg[bl - s..bl - s + cipher.len()].copy_from_slice(cipher);
                out.extend_from_slice(&res);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- objects

pub fn is_cipher_obj(o: &NetObj) -> bool {
    matches!(o, NetObj::Sym(_) | NetObj::Transform(..) | NetObj::CryptoStream(_)) || matches!(o, NetObj::Bag(KEY_SIZES, _))
}

fn new_sym(ty: &'static str) -> Sym {
    let bl = block_len(ty);
    let key = if is_triple(ty) { 24 } else if is_des(ty) { 8 } else { 32 };
    Sym { ty, key: random(key), iv: random(bl), mode: CBC, padding: PKCS7, feedback: if ty.contains("Aes") { 1 } else { bl } }
}

fn key_sizes(min: i32, max: i32, skip: i32) -> Val {
    net(NetObj::Bag(KEY_SIZES, vec![("minsize".into(), Val::Int(min)), ("maxsize".into(), Val::Int(max)), ("skipsize".into(), Val::Int(skip))]))
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    Ok(match ty {
        "System.Security.Cryptography.KeySizes" => key_sizes(a.int(0) as i32, a.int(1) as i32, a.int(2) as i32),
        "System.Security.Cryptography.CryptoStream" => crypto_stream(a)?,
        "System.Security.Cryptography.ICryptoTransform"
        | "System.Security.Cryptography.CipherMode"
        | "System.Security.Cryptography.PaddingMode"
        | "System.Security.Cryptography.CryptoStreamMode" => net(NetObj::Null(ty)),
        _ => net(NetObj::Sym(Box::new(new_sym(ty)))),
    })
}

fn crypto_stream(a: &Args) -> R<Val> {
    let (sym, encrypt) = match a.net(1).as_ref().map(|t| t.borrow()) {
        Some(t) => match &*t {
            NetObj::Transform(s, e) => ((**s).clone(), *e),
            _ => return net_err("CryptoStream", "CryptoStream", "Value cannot be null. (Parameter 'transform')"),
        },
        None => return net_err("CryptoStream", "CryptoStream", "Value cannot be null. (Parameter 'transform')"),
    };
    let target = a.raw(0).cloned().unwrap_or(Val::Void);
    if a.int(2) == 0 {
        // read mode: the transformed source, readable like a MemoryStream
        let data = match &target {
            Val::Net(n) => match &mut *n.borrow_mut() {
                NetObj::Stream(b, pos) => {
                    let r = b[(*pos).min(b.len())..].to_vec();
                    *pos = b.len();
                    r
                }
                _ => super::text::arg_bytes(Some(&target)),
            },
            other => super::text::arg_bytes(Some(other)),
        };
        return match transform(&sym, encrypt, &data) {
            Ok(out) => Ok(net(NetObj::Stream(out, 0))),
            Err(e) => net_err("CryptoStream", "CryptoStream", &e),
        };
    }
    Ok(net(NetObj::CryptoStream(Box::new(CryptoW { target, sym, encrypt, buf: vec![], done: false }))))
}

/// Bytes written into a write-mode CryptoStream (stream_write / StreamWriter).
pub fn buffer(w: &mut CryptoW, bytes: &[u8]) {
    if !w.done {
        w.buf.extend_from_slice(bytes);
    }
}

/// FlushFinalBlock: transform the buffered bytes into the target stream (once).
pub fn finish(n: &NetRef) -> R<()> {
    let (target, out) = {
        let mut o = n.borrow_mut();
        let NetObj::CryptoStream(w) = &mut *o else { return Ok(()) };
        if w.done {
            return Ok(());
        }
        w.done = true;
        let data = std::mem::take(&mut w.buf);
        match transform(&w.sym, w.encrypt, &data) {
            Ok(out) => (w.target.clone(), out),
            Err(e) => return net_err::<()>("CryptoStream", "FlushFinalBlock", &e),
        }
    };
    super::text::stream_write(&target, &out);
    Ok(())
}

fn bytes_arg(a: &Args, i: usize) -> Vec<u8> {
    super::text::arg_bytes(a.raw(i))
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if m == "create" {
        // Aes.Create() / SymmetricAlgorithm.Create(name)
        return ok(net(NetObj::Sym(Box::new(new_sym(if ty.ends_with("SymmetricAlgorithm") { "System.Security.Cryptography.Aes" } else { ty })))));
    }
    if matches!(&*n.borrow(), NetObj::CryptoStream(_)) {
        return crypto_stream_call(n, m, a);
    }
    let mut o = n.borrow_mut();
    match &mut *o {
        NetObj::Bag(KEY_SIZES, p) => ok(p.iter().find(|(k, _)| k == m).map(|(_, v)| v.clone()).unwrap_or(Val::Void)),
        NetObj::Transform(s, enc) => ok(match m {
            "transformfinalblock" => {
                let data = bytes_arg(a, 0);
                let (off, cnt) = if a.len() >= 3 { (a.int(1).max(0) as usize, a.int(2).max(0) as usize) } else { (0, data.len()) };
                let chunk = data.get(off..(off + cnt).min(data.len())).unwrap_or(&[]);
                match transform(s, *enc, chunk) {
                    Ok(out) => super::text::bytes_val(&out),
                    Err(e) => return net_err("ICryptoTransform", "TransformFinalBlock", &e),
                }
            }
            "inputblocksize" | "outputblocksize" => Val::Int(block_len(s.ty) as i32),
            "canreusetransform" | "cantransformmultipleblocks" => Val::Bool(true),
            "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        }),
        NetObj::Sym(s) => {
            let set = a.len() == 1;
            ok(match m {
                "key" if set => {
                    let k = bytes_arg(a, 0);
                    if !valid_key_len(s.ty, k.len()) {
                        return net_err(super::short_name(ty), "Key", "Specified key is not a valid size for this algorithm.");
                    }
                    s.key = k;
                    Val::Void
                }
                "key" => super::text::bytes_val(&s.key),
                "iv" if set => {
                    let v = bytes_arg(a, 0);
                    if v.len() != block_len(s.ty) {
                        return net_err(super::short_name(ty), "IV", "Specified initialization vector (IV) does not match the block size for this algorithm.");
                    }
                    s.iv = v;
                    Val::Void
                }
                "iv" => super::text::bytes_val(&s.iv),
                "keysize" if set => {
                    let bits = a.int(0);
                    if bits % 8 != 0 || !valid_key_len(s.ty, bits as usize / 8) {
                        return net_err(super::short_name(ty), "KeySize", "Specified key is not a valid size for this algorithm.");
                    }
                    s.key = random(bits as usize / 8);
                    Val::Void
                }
                "keysize" => Val::Int(s.key.len() as i32 * 8),
                "blocksize" if set => {
                    if a.int(0) != block_len(s.ty) as i64 * 8 {
                        return net_err(super::short_name(ty), "BlockSize", "Specified block size is not valid for this algorithm.");
                    }
                    Val::Void
                }
                "blocksize" => Val::Int(block_len(s.ty) as i32 * 8),
                "feedbacksize" if set => {
                    let bits = a.int(0);
                    if bits != 8 && bits != block_len(s.ty) as i64 * 8 {
                        return net_err(super::short_name(ty), "FeedbackSize", "Specified feedback size is invalid.");
                    }
                    s.feedback = bits as usize / 8;
                    Val::Void
                }
                "feedbacksize" => Val::Int(s.feedback as i32 * 8),
                "mode" if set => {
                    let v = a.int(0) as i32;
                    if !matches!(v, CBC | ECB | CFB) {
                        return net_err(super::short_name(ty), "Mode", "Specified cipher mode is not valid for this algorithm.");
                    }
                    s.mode = v;
                    Val::Void
                }
                "mode" => enum_val("System.Security.Cryptography.CipherMode", s.mode as i64),
                "padding" if set => {
                    let v = a.int(0) as i32;
                    if !(PAD_NONE..=ISO10126).contains(&v) {
                        return net_err(super::short_name(ty), "Padding", "Specified padding mode is not valid for this algorithm.");
                    }
                    s.padding = v;
                    Val::Void
                }
                "padding" => enum_val("System.Security.Cryptography.PaddingMode", s.padding as i64),
                "generatekey" => {
                    s.key = random(s.key.len().max(8));
                    Val::Void
                }
                "generateiv" => {
                    s.iv = random(block_len(s.ty));
                    Val::Void
                }
                "validkeysize" => Val::Bool(a.int(0) % 8 == 0 && valid_key_len(s.ty, a.int(0) as usize / 8)),
                "legalkeysizes" => {
                    let (min, max, skip) = legal_keys(s.ty);
                    net(NetObj::Array(KEY_SIZES, vec![key_sizes(min, max, skip)]))
                }
                "legalblocksizes" => {
                    let b = block_len(s.ty) as i32 * 8;
                    net(NetObj::Array(KEY_SIZES, vec![key_sizes(b, b, 0)]))
                }
                "createencryptor" | "createdecryptor" => {
                    let mut t = (**s).clone();
                    if a.len() >= 2 {
                        t.key = bytes_arg(a, 0);
                        t.iv = bytes_arg(a, 1);
                        if !valid_key_len(t.ty, t.key.len()) {
                            return net_err(super::short_name(ty), if m == "createencryptor" { "CreateEncryptor" } else { "CreateDecryptor" }, "Specified key is not a valid size for this algorithm.");
                        }
                    }
                    net(NetObj::Transform(Box::new(t), m == "createencryptor"))
                }
                "encryptecb" | "decryptecb" | "encryptcbc" | "decryptcbc" => {
                    // .NET 6+ one-shot helpers: (data [, iv], padding)
                    let mut t = (**s).clone();
                    let cbc = m.ends_with("cbc");
                    t.mode = if cbc { CBC } else { ECB };
                    if cbc {
                        t.iv = bytes_arg(a, 1);
                    }
                    t.padding = if a.len() > if cbc { 2 } else { 1 } { a.int(if cbc { 2 } else { 1 }) as i32 } else { PKCS7 };
                    match transform(&t, m.starts_with("encrypt"), &bytes_arg(a, 0)) {
                        Ok(out) => super::text::bytes_val(&out),
                        Err(e) => return net_err(super::short_name(ty), m, &e),
                    }
                }
                "clear" | "dispose" => Val::Void,
                _ => return Ok(Out::Unknown),
            })
        }
        _ => Ok(Out::Unknown),
    }
}

fn crypto_stream_call(n: &NetRef, m: &str, a: &Args) -> R<Out> {
    match m {
        "write" => {
            let src = bytes_arg(a, 0);
            let (off, cnt) = if a.len() >= 3 { (a.int(1).max(0) as usize, a.int(2).max(0) as usize) } else { (0, src.len()) };
            let chunk = src.get(off..(off + cnt).min(src.len())).unwrap_or(&[]).to_vec();
            if let NetObj::CryptoStream(w) = &mut *n.borrow_mut() {
                buffer(w, &chunk);
            }
            void()
        }
        "writebyte" => {
            if let NetObj::CryptoStream(w) = &mut *n.borrow_mut() {
                buffer(w, &[a.int(0) as u8]);
            }
            void()
        }
        "flushfinalblock" | "close" | "dispose" | "clear" => {
            finish(n)?;
            void()
        }
        "hasflushedfinalblock" => ok(Val::Bool(matches!(&*n.borrow(), NetObj::CryptoStream(w) if w.done))),
        "flush" => void(),
        "canwrite" => ok(Val::Bool(true)),
        "canread" | "canseek" => ok(Val::Bool(false)),
        _ => Ok(Out::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn fips197_vectors() {
        let pt = unhex("00112233445566778899aabbccddeeff");
        for (key, ct) in [
            ("000102030405060708090a0b0c0d0e0f", "69c4e0d86a7b0430d8cdb78070b4c55a"),
            ("000102030405060708090a0b0c0d0e0f1011121314151617", "dda97ca4864cdfe06eaf70a0ec0d7191"),
            ("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "8ea2b7ca516745bfeafc49904b496089"),
        ] {
            let aes = Aes::new(&unhex(key));
            let mut b: [u8; 16] = pt.clone().try_into().unwrap();
            aes.encrypt(&mut b);
            assert_eq!(hex(&b), ct);
            aes.decrypt(&mut b);
            assert_eq!(b.to_vec(), pt);
        }
    }

    #[test]
    fn des_vectors() {
        let des = |ty: &'static str, key: &str| Sym { ty, key: unhex(key), iv: vec![0; 8], mode: ECB, padding: PAD_NONE, feedback: 8 };
        // classic DES example and the 3DES (2-key, 3-key) results of openssl des-ede / des-ede3
        let s = des("System.Security.Cryptography.DESCryptoServiceProvider", "133457799bbcdff1");
        let ct = transform(&s, true, &unhex("0123456789abcdef")).unwrap();
        assert_eq!(hex(&ct), "85e813540f0ab405");
        assert_eq!(hex(&transform(&s, false, &ct).unwrap()), "0123456789abcdef");
        let t = des("System.Security.Cryptography.TripleDESCryptoServiceProvider", "0123456789abcdeffedcba987654321089abcdef01234567");
        let ct3 = transform(&t, true, &unhex("0123456789abcdef")).unwrap();
        assert_eq!(hex(&transform(&t, false, &ct3).unwrap()), "0123456789abcdef");
        assert_eq!(hex(&ct3), "691747fd88b6d228");
    }

    #[test]
    fn modes_round_trip() {
        // SP 800-38A F.2.1 CBC-AES128.Encrypt, first block
        let sym = Sym {
            ty: "System.Security.Cryptography.Aes",
            key: unhex("2b7e151628aed2a6abf7158809cf4f3c"),
            iv: unhex("000102030405060708090a0b0c0d0e0f"),
            mode: CBC,
            padding: PAD_NONE,
            feedback: 16,
        };
        let ct = transform(&sym, true, &unhex("6bc1bee22e409f96e93d7e117393172a")).unwrap();
        assert_eq!(hex(&ct), "7649abac8119b246cee98e9b12e9197d");
        // CFB8 (F.3.7 first byte 0x3b)
        let cfb = Sym { mode: CFB, feedback: 1, ..sym.clone() };
        assert_eq!(hex(&transform(&cfb, true, &unhex("6bc1bee22e409f96")).unwrap()), "3b79424c9c0dd436");
        for (mode, padding) in [(CBC, PKCS7), (ECB, PKCS7), (CBC, ANSIX923), (CBC, ISO10126), (CFB, PKCS7)] {
            let s = Sym { mode, padding, ..sym.clone() };
            let data = b"hello rijndael, this is 37 bytes long";
            let e = transform(&s, true, data).unwrap();
            assert_eq!(transform(&s, false, &e).unwrap(), data.to_vec());
        }
    }
}
