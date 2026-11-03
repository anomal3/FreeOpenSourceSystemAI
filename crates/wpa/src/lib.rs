//! WPA2-PSK со стороны станции: ключи и кадры четырёхстороннего обмена
//! (IEEE 802.11i, 03.11).
//!
//! # Зачем отдельным крейтом
//!
//! Это числа, а не железо: PMK из пароля, PTK из двух случайных чисел и
//! адресов, подпись (MIC) кадра EAPOL-Key и распаковка группового ключа.
//! Проверяются они эталонными векторами стандарта на хосте, а не на
//! телефоне, где ошибка выглядит одинаково при любой причине — точка доступа
//! молча не отвечает на второе сообщение.
//!
//! # Что здесь есть
//!
//! Только WPA2-Personal с CCMP: PMK = PBKDF2-HMAC-SHA1(пароль, SSID, 4096, 32),
//! PTK = PRF-384 («Pairwise key expansion»), MIC = HMAC-SHA1 (версия ключа 2),
//! ключевые данные третьего сообщения — AES Key Wrap (RFC 3394) под KEK.
//! WPA3 (SAE), TKIP и 802.1X — нет.

#![no_std]

use aes::Aes128;
use aes::cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray};
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// PMK из пароля и имени сети.
#[must_use]
pub fn pmk(password: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(password, ssid, 4096, &mut out);
    out
}

/// PRF-X из 802.11i: HMAC-SHA1(K, A || 0 || B || i), блоки по 20 байт.
fn prf(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) {
    let mut counter = 0u8;
    let mut done = 0;
    while done < out.len() {
        let mut mac = <HmacSha1 as Mac>::new_from_slice(key).expect("HMAC takes any key length");
        mac.update(label);
        mac.update(&[0]);
        mac.update(data);
        mac.update(&[counter]);
        let block = mac.finalize().into_bytes();
        let take = (out.len() - done).min(block.len());
        out[done..done + take].copy_from_slice(&block[..take]);
        done += take;
        counter += 1;
    }
}

/// Ключи сеанса для CCMP: KCK (подпись), KEK (распаковка), TK (шифр кадров).
#[derive(Clone, Copy)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

/// PTK: PRF-384 от PMK, адресов и двух случайных чисел, меньшее — первым.
#[must_use]
pub fn ptk(pmk: &[u8; 32], aa: &[u8; 6], spa: &[u8; 6], anonce: &[u8; 32], snonce: &[u8; 32]) -> Ptk {
    let mut data = [0u8; 6 * 2 + 32 * 2];
    let (lo, hi) = if aa < spa { (aa, spa) } else { (spa, aa) };
    data[0..6].copy_from_slice(lo);
    data[6..12].copy_from_slice(hi);
    let (lo, hi) = if anonce < snonce { (anonce, snonce) } else { (snonce, anonce) };
    data[12..44].copy_from_slice(lo);
    data[44..76].copy_from_slice(hi);
    let mut raw = [0u8; 48];
    prf(pmk, b"Pairwise key expansion", &data, &mut raw);
    let mut ptk = Ptk { kck: [0; 16], kek: [0; 16], tk: [0; 16] };
    ptk.kck.copy_from_slice(&raw[0..16]);
    ptk.kek.copy_from_slice(&raw[16..32]);
    ptk.tk.copy_from_slice(&raw[32..48]);
    ptk
}

/// MIC кадра EAPOL (весь кадр 802.1X с нулями на месте MIC), первые 16 байт.
#[must_use]
pub fn mic(kck: &[u8; 16], eapol: &[u8]) -> [u8; 16] {
    let mut mac = <HmacSha1 as Mac>::new_from_slice(kck).expect("HMAC takes any key length");
    mac.update(eapol);
    let full = mac.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

/// AES Key Unwrap (RFC 3394) под KEK. `None` — проверка целостности не
/// сошлась (не тот ключ или порча).
#[must_use]
pub fn unwrap_key(kek: &[u8; 16], wrapped: &[u8], out: &mut [u8]) -> Option<usize> {
    if wrapped.len() < 24 || wrapped.len() % 8 != 0 || out.len() < wrapped.len() - 8 {
        return None;
    }
    let n = wrapped.len() / 8 - 1;
    let cipher = Aes128::new(GenericArray::from_slice(kek));
    let mut a = [0u8; 8];
    a.copy_from_slice(&wrapped[..8]);
    out[..n * 8].copy_from_slice(&wrapped[8..]);
    for j in (0..6).rev() {
        for i in (1..=n).rev() {
            let t = (n * j + i) as u64;
            let mut block = [0u8; 16];
            for (k, byte) in t.to_be_bytes().iter().enumerate() {
                block[k] = a[k] ^ byte;
            }
            block[8..].copy_from_slice(&out[(i - 1) * 8..i * 8]);
            let mut ga = GenericArray::clone_from_slice(&block);
            cipher.decrypt_block(&mut ga);
            a.copy_from_slice(&ga[..8]);
            out[(i - 1) * 8..i * 8].copy_from_slice(&ga[8..]);
        }
    }
    (a == [0xa6; 8]).then_some(n * 8)
}

// ---------------------------------------------------------------------------
// Кадры EAPOL-Key
// ---------------------------------------------------------------------------

/// Биты Key Information.
pub mod info {
    pub const VERSION_HMAC_SHA1_AES: u16 = 2;
    pub const PAIRWISE: u16 = 1 << 3;
    pub const INSTALL: u16 = 1 << 6;
    pub const ACK: u16 = 1 << 7;
    pub const MIC: u16 = 1 << 8;
    pub const SECURE: u16 = 1 << 9;
    pub const ENCRYPTED_DATA: u16 = 1 << 12;
}

/// Смещения внутри кадра 802.1X (4 байта заголовка + дескриптор ключа).
const KEY_INFO: usize = 5;
const REPLAY: usize = 9;
const NONCE: usize = 17;
const MIC_AT: usize = 81;
const DATA_LEN: usize = 97;
const DATA: usize = 99;

/// Разобранное сообщение точки доступа.
pub struct KeyFrame<'a> {
    pub info: u16,
    pub replay: [u8; 8],
    pub nonce: [u8; 32],
    pub mic: [u8; 16],
    pub data: &'a [u8],
}

/// Разобрать кадр 802.1X EAPOL-Key (дескриптор RSN, тип 2).
#[must_use]
pub fn parse(eapol: &[u8]) -> Option<KeyFrame<'_>> {
    if eapol.len() < DATA || eapol[1] != 3 || eapol[4] != 2 {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([eapol[DATA_LEN], eapol[DATA_LEN + 1]]));
    let data = eapol.get(DATA..DATA + len)?;
    let mut frame = KeyFrame { info: u16::from_be_bytes([eapol[KEY_INFO], eapol[KEY_INFO + 1]]), replay: [0; 8], nonce: [0; 32], mic: [0; 16], data };
    frame.replay.copy_from_slice(&eapol[REPLAY..REPLAY + 8]);
    frame.nonce.copy_from_slice(&eapol[NONCE..NONCE + 32]);
    frame.mic.copy_from_slice(&eapol[MIC_AT..MIC_AT + 16]);
    Some(frame)
}

/// Проверить MIC сообщения точки доступа.
#[must_use]
pub fn verify(kck: &[u8; 16], eapol: &[u8]) -> bool {
    if eapol.len() < MIC_AT + 16 {
        return false;
    }
    let mut copy = [0u8; 512];
    let len = eapol.len().min(copy.len());
    copy[..len].copy_from_slice(&eapol[..len]);
    let got: [u8; 16] = copy[MIC_AT..MIC_AT + 16].try_into().unwrap_or([0; 16]);
    copy[MIC_AT..MIC_AT + 16].fill(0);
    mic(kck, &copy[..len]) == got
}

/// Собрать ответ станции (сообщение 2 или 4) с подписью. Возвращает длину.
pub fn build(out: &mut [u8], info: u16, replay: &[u8; 8], nonce: &[u8; 32], data: &[u8], kck: &[u8; 16]) -> usize {
    let len = DATA + data.len();
    out[..len].fill(0);
    out[0] = 2; // 802.1X-2004
    out[1] = 3; // EAPOL-Key
    out[2..4].copy_from_slice(&((len - 4) as u16).to_be_bytes());
    out[4] = 2; // RSN
    out[KEY_INFO..KEY_INFO + 2].copy_from_slice(&info.to_be_bytes());
    out[REPLAY..REPLAY + 8].copy_from_slice(replay);
    out[NONCE..NONCE + 32].copy_from_slice(nonce);
    out[DATA_LEN..DATA_LEN + 2].copy_from_slice(&(data.len() as u16).to_be_bytes());
    out[DATA..len].copy_from_slice(data);
    let sign = mic(kck, &out[..len]);
    out[MIC_AT..MIC_AT + 16].copy_from_slice(&sign);
    len
}

/// RSN IE станции: WPA2-PSK, CCMP для пар и группы.
pub const RSN_IE: [u8; 22] = [
    0x30, 0x14, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x04, 0x01, 0x00, 0x00, 0x0f, 0xac, 0x02, 0x00, 0x00,
];

/// Найти GTK в распакованных ключевых данных (KDE `dd .. 00-0f-ac 01`).
/// Возвращает номер ключа и сам ключ.
#[must_use]
pub fn find_gtk(data: &[u8]) -> Option<(u8, &[u8])> {
    let mut at = 0;
    while at + 2 <= data.len() {
        let (tag, len) = (data[at], usize::from(data[at + 1]));
        let body = data.get(at + 2..at + 2 + len)?;
        if tag == 0xdd && len >= 6 && body[..4] == [0x00, 0x0f, 0xac, 0x01] {
            return Some((body[4] & 3, &body[6..]));
        }
        if tag == 0xdd && len == 0 {
            break;
        }
        at += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    /// IEEE 802.11i, приложение H.4.1: «password», SSID «IEEE».
    #[test]
    fn pmk_matches_the_standard() {
        assert_eq!(pmk(b"password", b"IEEE"), hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"));
    }

    /// RFC 3394, 4.1: KEK 000102…0F, данные 00112233…FF.
    #[test]
    fn unwrap_matches_rfc_3394() {
        let kek: [u8; 16] = core::array::from_fn(|i| i as u8);
        let wrapped = [
            0x1F, 0xA6, 0x8B, 0x0A, 0x81, 0x12, 0xB4, 0x47, 0xAE, 0xF3, 0x4B, 0xD8, 0xFB, 0x5A, 0x7B, 0x82, 0x9D, 0x3E, 0x86, 0x23, 0x71, 0xD2,
            0xCF, 0xE5,
        ];
        let mut out = [0u8; 16];
        assert_eq!(unwrap_key(&kek, &wrapped, &mut out), Some(16));
        let plain: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
        assert_eq!(out, plain);
    }

    #[test]
    fn built_frame_verifies() {
        let kck = [7u8; 16];
        let mut buf = [0u8; 256];
        let len = build(&mut buf, info::VERSION_HMAC_SHA1_AES | info::PAIRWISE | info::MIC, &[0; 8], &[1; 32], &RSN_IE, &kck);
        assert!(verify(&kck, &buf[..len]));
        let parsed = parse(&buf[..len]).unwrap();
        assert_eq!(parsed.data, &RSN_IE);
    }
}
