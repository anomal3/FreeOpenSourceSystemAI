//! Расшифровка одного блока AES-128 (FIPS-197) — только то, что нужно
//! распаковке группового ключа (RFC 3394).
//!
//! Своя, а не крейт `aes`: тот на x86-64 выбирает AES-NI и векторные типы, и
//! отладочная сборка ядра (без плавающей точки и SIMD) падала в LLVM —
//! «Do not know how to split the result of this operator» (03.11). Здесь —
//! таблицы и байты, ничего сверх: один блок на ключ за обмен ключами,
//! скорость не важна.

const SBOX: [u8; 256] = {
    // S-блок строится из обратного по модулю x^8+x^4+x^3+x+1 и аффинного
    // преобразования — на этапе компиляции, без набитой руками таблицы.
    let mut sbox = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p — степени 3, q — обратные к ним (умножение на 3⁻¹).
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

const INV_SBOX: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
};

const fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut out = 0;
    while b != 0 {
        if b & 1 != 0 {
            out ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    out
}

/// Ключи раундов AES-128: 11 по 16 байт.
fn expand(key: &[u8; 16]) -> [[u8; 16]; 11] {
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
    rounds
}

/// Расшифровать блок на месте.
pub fn decrypt_block(key: &[u8; 16], block: &mut [u8; 16]) {
    let rounds = expand(key);
    let add = |s: &mut [u8; 16], k: &[u8; 16]| s.iter_mut().zip(k).for_each(|(a, b)| *a ^= b);
    add(block, &rounds[10]);
    for round in (0..10).rev() {
        // Обратный сдвиг строк: строка r (байты r, r+4, r+8, r+12) — вправо на r.
        let s = *block;
        for c in 0..4 {
            for r in 0..4 {
                block[((c + r) % 4) * 4 + r] = s[c * 4 + r];
            }
        }
        for b in block.iter_mut() {
            *b = INV_SBOX[*b as usize];
        }
        add(block, &rounds[round]);
        if round > 0 {
            for c in 0..4 {
                let col = [block[c * 4], block[c * 4 + 1], block[c * 4 + 2], block[c * 4 + 3]];
                for r in 0..4 {
                    block[c * 4 + r] = mul(col[r], 14) ^ mul(col[(r + 1) % 4], 11) ^ mul(col[(r + 2) % 4], 13) ^ mul(col[(r + 3) % 4], 9);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    /// FIPS-197, приложение C.1: AES-128.
    #[test]
    fn fips_197_c1() {
        let key: [u8; 16] = core::array::from_fn(|i| i as u8);
        let mut block = [
            0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4, 0xc5, 0x5a,
        ];
        super::decrypt_block(&key, &mut block);
        let plain: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        assert_eq!(block, plain);
    }
}
