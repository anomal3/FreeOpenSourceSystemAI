//! HMAC, HKDF и расписание ключей TLS 1.3.
//!
//! # Почему написано здесь, а не взято крейтом
//!
//! Потому что это двадцать строк поверх SHA-256, а всё остальное в расписании
//! ключей — это **метки**: `"c hs traffic"`, `"s ap traffic"`, `"finished"`.
//! Крейт `hkdf` не знает про них ничего, а ошибиться можно ровно в них: HKDF
//! посчитается правильно, а секрет получится не тот, и рукопожатие сорвётся на
//! `Finished` с сообщением «подпись не сходится».
//!
//! Ловушка, которую стоит назвать вслух: `HkdfLabel.label` — это **не** метка,
//! а `"tls13 "` плюс метка. Пропущенный пробел или забытый префикс дают
//! ключевой материал, который сойдётся сам с собой на обоих концах нашей
//! реализации и ни с чем больше.

use sha2::{Digest, Sha256};

/// Длина хеша выбранного набора шифров. У нас он один — SHA-256.
pub const HASH_LEN: usize = 32;

/// Размер блока SHA-256 — он же длина ключа HMAC после дополнения.
const BLOCK: usize = 64;

/// Хеш от нескольких кусков подряд.
#[must_use]
pub fn sha256(parts: &[&[u8]]) -> [u8; HASH_LEN] {
    let mut state = Sha256::new();
    for part in parts {
        state.update(part);
    }
    state.finalize().into()
}

/// HMAC-SHA256.
#[must_use]
pub fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; HASH_LEN] {
    let mut padded = [0u8; BLOCK];
    if key.len() > BLOCK {
        padded[..HASH_LEN].copy_from_slice(&sha256(&[key]));
    } else {
        padded[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5Cu8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= padded[index];
        outer_pad[index] ^= padded[index];
    }

    let mut state = Sha256::new();
    state.update(inner_pad);
    for part in parts {
        state.update(part);
    }
    let inner: [u8; HASH_LEN] = state.finalize().into();
    sha256(&[&outer_pad, &inner])
}

/// `HKDF-Extract` (RFC 5869): соль в роли ключа, материал — в роли сообщения.
///
/// Порядок именно такой, и он выглядит перевёрнутым. Ошибка здесь даёт
/// работающий HKDF с другими результатами.
#[must_use]
pub fn extract(salt: &[u8], material: &[u8]) -> [u8; HASH_LEN] {
    hmac(salt, &[material])
}

/// `HKDF-Expand-Label` (RFC 8446, §7.1).
///
/// ```text
/// struct {
///     uint16 length;
///     opaque label<7..255>  = "tls13 " + Label;
///     opaque context<0..255>;
/// } HkdfLabel;
/// ```
pub fn expand_label(secret: &[u8; HASH_LEN], label: &str, context: &[u8], out: &mut [u8]) {
    // Метка целиком: `"tls13 "` не длиннее шести байт, а наши метки — не длиннее
    // двенадцати. Буфер на 32 покрывает всё с запасом.
    let mut full = [0u8; 32];
    full[..6].copy_from_slice(b"tls13 ");
    full[6..6 + label.len()].copy_from_slice(label.as_bytes());
    let full = &full[..6 + label.len()];

    let length = (out.len() as u16).to_be_bytes();
    let info: [&[u8]; 5] =
        [&length, &[full.len() as u8], full, &[context.len() as u8], context];

    // `HKDF-Expand`: T(1) = HMAC(secret, info || 0x01), T(2) = HMAC(secret,
    // T(1) || info || 0x02), и так далее.
    let mut previous = [0u8; HASH_LEN];
    let mut written = 0usize;
    let mut counter = 1u8;
    while written < out.len() {
        let mut parts: [&[u8]; 7] = [&[]; 7];
        let mut at = 0usize;
        if counter > 1 {
            parts[at] = &previous;
            at += 1;
        }
        for piece in info {
            parts[at] = piece;
            at += 1;
        }
        let step = [counter];
        parts[at] = &step;
        at += 1;
        previous = hmac(secret, &parts[..at]);
        let take = (out.len() - written).min(HASH_LEN);
        out[written..written + take].copy_from_slice(&previous[..take]);
        written += take;
        counter += 1;
    }
}

/// `Derive-Secret(secret, label, messages)`.
///
/// Второй аргумент — уже посчитанный хеш стенограммы, а не сами сообщения:
/// стенограмма считается по ходу дела и целиком нигде не хранится (иначе под
/// неё пришлось бы держать буфер под цепочку сертификатов).
#[must_use]
pub fn derive_secret(
    secret: &[u8; HASH_LEN],
    label: &str,
    transcript: &[u8; HASH_LEN],
) -> [u8; HASH_LEN] {
    let mut out = [0u8; HASH_LEN];
    expand_label(secret, label, transcript, &mut out);
    out
}

/// `Derive-Secret(secret, label, "")` — по хешу пустой стенограммы.
#[must_use]
pub fn derive_empty(secret: &[u8; HASH_LEN], label: &str) -> [u8; HASH_LEN] {
    derive_secret(secret, label, &sha256(&[]))
}
