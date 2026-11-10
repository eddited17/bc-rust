//! System.Security.Cryptography hashing: HashAlgorithm / KeyedHashAlgorithm.Create(name), MD5,
//! SHA1, SHA256, SHA384, SHA512 and their HMAC variants (HMAC is computed here over the hash).

use super::*;

pub const HASH_TYPES: &[&str] = &[
    "System.Security.Cryptography.Rfc2898DeriveBytes",
    "System.Security.Cryptography.HashAlgorithm",
    "System.Security.Cryptography.KeyedHashAlgorithm",
    "System.Security.Cryptography.HMAC",
    "System.Security.Cryptography.MD5",
    "System.Security.Cryptography.SHA1",
    "System.Security.Cryptography.SHA256",
    "System.Security.Cryptography.SHA384",
    "System.Security.Cryptography.SHA512",
    "System.Security.Cryptography.SHA1Managed",
    "System.Security.Cryptography.SHA256Managed",
    "System.Security.Cryptography.SHA512Managed",
    "System.Security.Cryptography.MD5CryptoServiceProvider",
    "System.Security.Cryptography.HMACMD5",
    "System.Security.Cryptography.HMACSHA1",
    "System.Security.Cryptography.HMACSHA256",
    "System.Security.Cryptography.HMACSHA384",
    "System.Security.Cryptography.HMACSHA512",
];

/// Canonical algorithm for a .NET name: (hash, keyed).
fn algorithm(name: &str) -> Option<(&'static str, bool)> {
    let n = name.trim().to_ascii_uppercase().replace(['-', '_'], "");
    let n = n.trim_start_matches("SYSTEM.SECURITY.CRYPTOGRAPHY.").trim_end_matches("MANAGED").trim_end_matches("CRYPTOSERVICEPROVIDER").to_string();
    let (keyed, base) = match n.strip_prefix("HMAC") {
        Some(b) => (true, b.to_string()),
        None => (false, n),
    };
    let h = match base.as_str() {
        "MD5" => "MD5",
        "SHA1" | "SHA" => "SHA1",
        "SHA256" => "SHA256",
        "SHA384" => "SHA384",
        "SHA512" => "SHA512",
        _ => return None,
    };
    Some((h, keyed))
}

pub fn digest(alg: &str, data: &[u8]) -> Vec<u8> {
    match alg {
        "MD5" => {
            use md5::Digest;
            md5::Md5::digest(data).to_vec()
        }
        "SHA1" => {
            use sha1::Digest;
            sha1::Sha1::digest(data).to_vec()
        }
        "SHA384" => {
            use sha2::Digest;
            sha2::Sha384::digest(data).to_vec()
        }
        "SHA512" => {
            use sha2::Digest;
            sha2::Sha512::digest(data).to_vec()
        }
        _ => {
            use sha2::Digest;
            sha2::Sha256::digest(data).to_vec()
        }
    }
}

/// HMAC (RFC 2104).
pub fn hmac(alg: &str, key: &[u8], data: &[u8]) -> Vec<u8> {
    let block = if matches!(alg, "SHA384" | "SHA512") { 128 } else { 64 };
    let mut k = if key.len() > block { digest(alg, key) } else { key.to_vec() };
    k.resize(block, 0);
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(data);
    let ih = digest(alg, &inner);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&ih);
    digest(alg, &outer)
}

/// PBKDF2 (RFC 2898) with HMAC-`alg`: `len` bytes.
pub fn pbkdf2(alg: &str, password: &[u8], salt: &[u8], iterations: u32, len: usize) -> Vec<u8> {
    let mut out = vec![];
    let mut block = 1u32;
    while out.len() < len {
        let mut s = salt.to_vec();
        s.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac(alg, password, &s);
        let mut t = u.clone();
        for _ in 1..iterations.max(1) {
            u = hmac(alg, password, &u);
            for (x, y) in t.iter_mut().zip(u.iter()) {
                *x ^= y;
            }
        }
        out.extend_from_slice(&t);
        block += 1;
    }
    out.truncate(len);
    out
}

pub fn construct(ty: &'static str, a: &Args) -> R<Val> {
    if ty.ends_with("Rfc2898DeriveBytes") {
        // (password, salt [, iterations [, HashAlgorithmName]]); salt may be a byte array or a size
        let password = match a.raw(0) {
            Some(Val::Net(x)) if matches!(&*x.borrow(), NetObj::Array(..)) => super::text::arg_bytes(a.raw(0)),
            _ => a.text(0).into_bytes(),
        };
        let salt = match a.raw(1) {
            Some(Val::Int(n)) => (0..*n).map(|_| uuid::Uuid::new_v4().as_bytes()[0]).collect(),
            other => super::text::arg_bytes(other),
        };
        let iterations = if a.len() > 2 { a.int(2).max(1) as u32 } else { 1000 };
        let alg = if a.len() > 3 { algorithm(&a.text(3)).map(|x| x.0).unwrap_or("SHA1") } else { "SHA1" };
        return Ok(net(NetObj::Pbkdf2 { alg, password, salt, iterations, pos: 0 }));
    }
    match algorithm(super::short_name(ty)) {
        Some((h, keyed)) => {
            let key = if keyed && !a.is_empty() { Some(super::text::arg_bytes(a.raw(0))) } else if keyed { Some(vec![]) } else { None };
            Ok(net(NetObj::Hasher(h, key)))
        }
        None => Ok(net(NetObj::Null(ty))),
    }
}

pub fn call(n: &NetRef, ty: &'static str, m: &str, a: &Args) -> R<Out> {
    if m == "create" {
        // HashAlgorithm.Create(name) / SHA256.Create()
        let name = if a.is_empty() { super::short_name(ty).to_string() } else { a.text(0) };
        return match algorithm(&name) {
            Some((h, keyed)) => ok(net(NetObj::Hasher(h, if keyed || ty.contains("Keyed") || ty.contains("HMAC") { Some(vec![]) } else { None }))),
            None => ok(net(NetObj::Null(ty))),
        };
    }
    let mut o = n.borrow_mut();
    if let NetObj::Pbkdf2 { alg, password, salt, iterations, pos } = &mut *o {
        return ok(match m {
            // successive GetBytes calls continue the derived stream
            "getbytes" => {
                let k = a.int(0).max(0) as usize;
                let all = pbkdf2(alg, password, salt, *iterations, *pos + k);
                let r = super::text::bytes_val(&all[*pos..]);
                *pos += k;
                r
            }
            "salt" => super::text::bytes_val(salt),
            "iterationcount" => Val::Int(*iterations as i32),
            "reset" => {
                *pos = 0;
                Val::Void
            }
            "dispose" => Val::Void,
            _ => return Ok(Out::Unknown),
        });
    }
    let NetObj::Hasher(alg, key) = &mut *o else { return Ok(Out::Unknown) };
    ok(match m {
        "key" if a.len() == 1 => {
            *key = Some(super::text::arg_bytes(a.raw(0)));
            Val::Void
        }
        "key" => super::text::bytes_val(key.as_deref().unwrap_or(&[])),
        "computehash" => {
            let mut data = super::text::arg_bytes(a.raw(0));
            if a.len() == 3 {
                let (off, cnt) = (a.int(1).max(0) as usize, a.int(2).max(0) as usize);
                data = data.get(off..(off + cnt).min(data.len())).unwrap_or(&[]).to_vec();
            }
            if let Some(Val::InStream(s)) = a.raw(0) {
                let mut st = s.borrow_mut();
                st.pos = st.data.len();
            }
            let h = match key {
                Some(k) => hmac(alg, k, &data),
                None => digest(alg, &data),
            };
            super::text::bytes_val(&h)
        }
        "hashsize" => Val::Int(digest(alg, b"").len() as i32 * 8),
        "dispose" | "clear" | "initialize" => Val::Void,
        _ => return Ok(Out::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn known_vectors() {
        assert_eq!(hex(&digest("MD5", b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(hex(&digest("SHA1", b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&digest("SHA256", b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        // RFC 4231 test case 2
        assert_eq!(hex(&hmac("SHA256", b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(algorithm("HMACSHA512"), Some(("SHA512", true)));
        // RFC 6070 test vector 2 (PBKDF2-HMAC-SHA1, 2 iterations)
        assert_eq!(hex(&pbkdf2("SHA1", b"password", b"salt", 2, 20)), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
    }
}
