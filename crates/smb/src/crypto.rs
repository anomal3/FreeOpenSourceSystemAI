//! The small primitives NTLM and SMB2/3 signing need: MD4, MD5, HMAC-MD5,
//! HMAC-SHA256, SHA-512, RC4, AES-128 (encryption only), AES-CMAC and the
//! SP800-108 counter-mode KDF of SMB 3.
//!
//! MD4/MD5/RC4 are broken as general-purpose primitives, and they are here
//! only because NTLM is defined with them. Nothing in this file is meant for
//! anything else.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256, Sha512};

// ---------------------------------------------------------------------------
// MD4 (RFC 1320) and MD5 (RFC 1321)
// ---------------------------------------------------------------------------

/// Pad a message the MD4/MD5 way and feed 64-byte blocks to `block`.
fn md_blocks(data: &[u8], mut block: impl FnMut(&[u32; 16])) {
    let mut words = [0u32; 16];
    let mut chunks = data.chunks_exact(64);
    for chunk in &mut chunks {
        for (i, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        block(&words);
    }
    let rest = chunks.remainder();
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let total = if rest.len() < 56 { 64 } else { 128 };
    let bits = (data.len() as u64).wrapping_mul(8);
    tail[total - 8..total].copy_from_slice(&bits.to_le_bytes());
    for chunk in tail[..total].chunks_exact(64) {
        for (i, word) in words.iter_mut().enumerate() {
            *word = u32::from_le_bytes([chunk[i * 4], chunk[i * 4 + 1], chunk[i * 4 + 2], chunk[i * 4 + 3]]);
        }
        block(&words);
    }
}

fn md_digest(state: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// MD4 — the NT hash is `md4(utf16le(password))`.
#[must_use]
pub fn md4(data: &[u8]) -> [u8; 16] {
    let mut h = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    md_blocks(data, |x| {
        let [mut a, mut b, mut c, mut d] = h;
        let f = |x: u32, y: u32, z: u32| (x & y) | (!x & z);
        let g = |x: u32, y: u32, z: u32| (x & y) | (x & z) | (y & z);
        let hh = |x: u32, y: u32, z: u32| x ^ y ^ z;
        // Round 1.
        for i in 0..4 {
            a = a.wrapping_add(f(b, c, d)).wrapping_add(x[i * 4]).rotate_left(3);
            d = d.wrapping_add(f(a, b, c)).wrapping_add(x[i * 4 + 1]).rotate_left(7);
            c = c.wrapping_add(f(d, a, b)).wrapping_add(x[i * 4 + 2]).rotate_left(11);
            b = b.wrapping_add(f(c, d, a)).wrapping_add(x[i * 4 + 3]).rotate_left(19);
        }
        // Round 2.
        for i in 0..4 {
            let k = 0x5A82_7999u32;
            a = a.wrapping_add(g(b, c, d)).wrapping_add(x[i]).wrapping_add(k).rotate_left(3);
            d = d.wrapping_add(g(a, b, c)).wrapping_add(x[i + 4]).wrapping_add(k).rotate_left(5);
            c = c.wrapping_add(g(d, a, b)).wrapping_add(x[i + 8]).wrapping_add(k).rotate_left(9);
            b = b.wrapping_add(g(c, d, a)).wrapping_add(x[i + 12]).wrapping_add(k).rotate_left(13);
        }
        // Round 3.
        for i in [0usize, 2, 1, 3] {
            let k = 0x6ED9_EBA1u32;
            a = a.wrapping_add(hh(b, c, d)).wrapping_add(x[i]).wrapping_add(k).rotate_left(3);
            d = d.wrapping_add(hh(a, b, c)).wrapping_add(x[i + 8]).wrapping_add(k).rotate_left(9);
            c = c.wrapping_add(hh(d, a, b)).wrapping_add(x[i + 4]).wrapping_add(k).rotate_left(11);
            b = b.wrapping_add(hh(c, d, a)).wrapping_add(x[i + 12]).wrapping_add(k).rotate_left(15);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    });
    md_digest(h)
}

const MD5_K: [u32; 64] = [
    0xd76a_a478, 0xe8c7_b756, 0x2420_70db, 0xc1bd_ceee, 0xf57c_0faf, 0x4787_c62a, 0xa830_4613, 0xfd46_9501,
    0x6980_98d8, 0x8b44_f7af, 0xffff_5bb1, 0x895c_d7be, 0x6b90_1122, 0xfd98_7193, 0xa679_438e, 0x49b4_0821,
    0xf61e_2562, 0xc040_b340, 0x265e_5a51, 0xe9b6_c7aa, 0xd62f_105d, 0x0244_1453, 0xd8a1_e681, 0xe7d3_fbc8,
    0x21e1_cde6, 0xc337_07d6, 0xf4d5_0d87, 0x455a_14ed, 0xa9e3_e905, 0xfcef_a3f8, 0x676f_02d9, 0x8d2a_4c8a,
    0xfffa_3942, 0x8771_f681, 0x6d9d_6122, 0xfde5_380c, 0xa4be_ea44, 0x4bde_cfa9, 0xf6bb_4b60, 0xbebf_bc70,
    0x289b_7ec6, 0xeaa1_27fa, 0xd4ef_3085, 0x0488_1d05, 0xd9d4_d039, 0xe6db_99e5, 0x1fa2_7cf8, 0xc4ac_5665,
    0xf429_2244, 0x432a_ff97, 0xab94_23a7, 0xfc93_a039, 0x655b_59c3, 0x8f0c_cc92, 0xffef_f47d, 0x8584_5dd1,
    0x6fa8_7e4f, 0xfe2c_e6e0, 0xa301_4314, 0x4e08_11a1, 0xf753_7e82, 0xbd3a_f235, 0x2ad7_d2bb, 0xeb86_d391,
];

const MD5_S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];

/// MD5 — inside HMAC-MD5, which NTLMv2 is built of.
#[must_use]
pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut h = [0x6745_2301u32, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    md_blocks(data, |x| {
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let s = MD5_S[(i / 16) * 4 + i % 4];
            let t = d;
            d = c;
            c = b;
            b = b.wrapping_add(a.wrapping_add(f).wrapping_add(MD5_K[i]).wrapping_add(x[g]).rotate_left(s));
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
    });
    md_digest(h)
}

/// HMAC-MD5 over the concatenation of `parts` (RFC 2104).
#[must_use]
pub fn hmac_md5(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..16].copy_from_slice(&md5(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let total: usize = parts.iter().map(|part| part.len()).sum();
    let mut inner = alloc::vec::Vec::with_capacity(64 + total);
    inner.extend(block.iter().map(|b| b ^ 0x36));
    for part in parts {
        inner.extend_from_slice(part);
    }
    let inner_hash = md5(&inner);
    let mut outer = [0u8; 80];
    for (slot, b) in outer.iter_mut().zip(block.iter()) {
        *slot = b ^ 0x5c;
    }
    outer[64..].copy_from_slice(&inner_hash);
    md5(&outer)
}

// ---------------------------------------------------------------------------
// SHA-2
// ---------------------------------------------------------------------------

/// HMAC-SHA256 over the concatenation of `parts`.
#[must_use]
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    // A key of any length is accepted by HMAC; `new_from_slice` cannot fail.
    let Ok(mut mac) = <Hmac<Sha256> as Mac>::new_from_slice(key) else {
        return [0; 32];
    };
    for part in parts {
        mac.update(part);
    }
    mac.finalize().into_bytes().into()
}

/// SHA-512 of `a || b` — one step of the SMB 3.1.1 pre-authentication hash.
#[must_use]
pub fn sha512_chain(previous: &[u8; 64], message: &[u8]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    hasher.update(previous);
    hasher.update(message);
    hasher.finalize().into()
}

/// SP800-108 counter-mode KDF with HMAC-SHA256, L = 128 (MS-SMB2 3.1.4.2).
/// `label` and `context` are passed as the spec writes them, including their
/// trailing NUL; the separator byte is added here.
#[must_use]
pub fn smb3_kdf(key: &[u8], label: &[u8], context: &[u8]) -> [u8; 16] {
    let full = hmac_sha256(key, &[&[0, 0, 0, 1], label, &[0], context, &[0, 0, 0, 128]]);
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

// ---------------------------------------------------------------------------
// RC4 — NTLM key exchange only
// ---------------------------------------------------------------------------

/// RC4 of `data` in place with `key`.
pub fn rc4(key: &[u8], data: &mut [u8]) {
    let mut s = [0u8; 256];
    for (i, slot) in s.iter_mut().enumerate() {
        *slot = i as u8;
    }
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    for byte in data.iter_mut() {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        *byte ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}

// ---------------------------------------------------------------------------
// AES-128 encryption (FIPS-197) and AES-CMAC (RFC 4493)
// ---------------------------------------------------------------------------

const SBOX: [u8; 256] = {
    // Built from the multiplicative inverse and the affine map at compile
    // time — the same construction as wpa/src/aes.rs.
    let mut sbox = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        p = p ^ (p << 1) ^ if p & 0x80 != 0 { 0x1b } else { 0 };
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    sbox
};

const fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

/// Expanded AES-128 key: eleven round keys.
#[derive(Clone)]
pub struct Aes128 {
    rounds: [[u8; 16]; 11],
}

impl Aes128 {
    #[must_use]
    pub fn new(key: &[u8; 16]) -> Self {
        let mut w = [[0u8; 4]; 44];
        for (i, word) in w.iter_mut().take(4).enumerate() {
            word.copy_from_slice(&key[i * 4..i * 4 + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            }
            for k in 0..4 {
                w[i][k] = w[i - 4][k] ^ t[k];
            }
        }
        let mut rounds = [[0u8; 16]; 11];
        for (r, round) in rounds.iter_mut().enumerate() {
            for c in 0..4 {
                round[c * 4..c * 4 + 4].copy_from_slice(&w[r * 4 + c]);
            }
        }
        Self { rounds }
    }

    /// Encrypt one block in place.
    pub fn encrypt(&self, block: &mut [u8; 16]) {
        for (b, k) in block.iter_mut().zip(&self.rounds[0]) {
            *b ^= k;
        }
        for round in 1..11 {
            for b in block.iter_mut() {
                *b = SBOX[*b as usize];
            }
            // Shift rows: row r (bytes r, r+4, r+8, r+12) left by r.
            let s = *block;
            for c in 0..4 {
                for r in 0..4 {
                    block[c * 4 + r] = s[((c + r) % 4) * 4 + r];
                }
            }
            if round < 10 {
                for c in 0..4 {
                    let a = [block[c * 4], block[c * 4 + 1], block[c * 4 + 2], block[c * 4 + 3]];
                    let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                    for r in 0..4 {
                        block[c * 4 + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                    }
                }
            }
            for (b, k) in block.iter_mut().zip(&self.rounds[round]) {
                *b ^= k;
            }
        }
    }
}

fn dbl(block: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut carry = 0u8;
    for i in (0..16).rev() {
        out[i] = (block[i] << 1) | carry;
        carry = block[i] >> 7;
    }
    if carry != 0 {
        out[15] ^= 0x87;
    }
    out
}

/// AES-CMAC (RFC 4493) — SMB 3.x signing.
#[must_use]
pub fn aes_cmac(key: &[u8; 16], message: &[u8]) -> [u8; 16] {
    let aes = Aes128::new(key);
    let mut l = [0u8; 16];
    aes.encrypt(&mut l);
    let k1 = dbl(&l);
    let k2 = dbl(&k1);
    let mut x = [0u8; 16];
    let blocks = message.len().div_ceil(16).max(1);
    for index in 0..blocks {
        let start = index * 16;
        let chunk = &message[start.min(message.len())..(start + 16).min(message.len())];
        let mut block = [0u8; 16];
        block[..chunk.len()].copy_from_slice(chunk);
        if index == blocks - 1 {
            if chunk.len() == 16 {
                block.iter_mut().zip(&k1).for_each(|(b, k)| *b ^= k);
            } else {
                block[chunk.len()] = 0x80;
                block.iter_mut().zip(&k2).for_each(|(b, k)| *b ^= k);
            }
        }
        x.iter_mut().zip(&block).for_each(|(a, b)| *a ^= b);
        aes.encrypt(&mut x);
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> alloc::vec::Vec<u8> {
        let clean: alloc::vec::Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        clean.chunks(2).map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
    }

    #[test]
    fn md4_vectors() {
        assert_eq!(md4(b"").to_vec(), hex("31d6cfe0d16ae931b73c59d7e0c089c0"));
        assert_eq!(md4(b"abc").to_vec(), hex("a448017aaf21d8525fc10ae87aa6729d"));
        assert_eq!(
            md4(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890").to_vec(),
            hex("e33b4ddc9c38f2199c3e7b164fcc0536")
        );
    }

    #[test]
    fn md5_vectors() {
        assert_eq!(md5(b"").to_vec(), hex("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(md5(b"abc").to_vec(), hex("900150983cd24fb0d6963f7d28e17f72"));
        assert_eq!(
            md5(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890").to_vec(),
            hex("57edf4a22be3c955ac49da2e2107b67a")
        );
    }

    #[test]
    fn hmac_md5_rfc2104() {
        assert_eq!(hmac_md5(&[0x0b; 16], &[b"Hi There"]).to_vec(), hex("9294727a3638bb1c13f48ef8158bfc9d"));
        assert_eq!(hmac_md5(b"Jefe", &[b"what do ya want ", b"for nothing?"]).to_vec(), hex("750c783e6ab0b503eaa86e310a5db738"));
    }

    #[test]
    fn rc4_vector() {
        let mut data = *b"Plaintext";
        rc4(b"Key", &mut data);
        assert_eq!(data.to_vec(), hex("bbf316e8d940af0ad3"));
    }

    #[test]
    fn aes_fips197() {
        let key: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
        let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff").try_into().unwrap();
        Aes128::new(&key).encrypt(&mut block);
        assert_eq!(block.to_vec(), hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
    }

    #[test]
    fn cmac_rfc4493() {
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
        assert_eq!(aes_cmac(&key, b"").to_vec(), hex("bb1d6929e95937287fa37d129b756746"));
        let m16 = hex("6bc1bee22e409f96e93d7e117393172a");
        assert_eq!(aes_cmac(&key, &m16).to_vec(), hex("070a16b46b4d4144f79bdd9dd04a287c"));
        let m40 = hex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411");
        assert_eq!(aes_cmac(&key, &m40).to_vec(), hex("dfa66747de9ae63030ca32611497c827"));
    }
}
