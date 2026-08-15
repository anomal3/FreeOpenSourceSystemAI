//! Уровень записей: рамка на проводе и шифрование под ней.
//!
//! # Что видно снаружи, а что нет
//!
//! У зашифрованной записи TLS 1.3 снаружи остаётся пять байт: тип
//! `application_data` (23), версия `0x0303` и длина. И тип, и версия — вранье,
//! сохранённое ради посредников, которые иначе рвут соединение: настоящий тип
//! содержимого лежит **внутри**, последним байтом расшифрованного, а версия
//! договаривается расширением. Разбор, который поверит внешнему типу, примет
//! рукопожатие за данные приложения.
//!
//! # Nonce считается, а не передаётся
//!
//! Каждая запись шифруется на `iv XOR порядковый_номер`. Номер нигде не едет —
//! обе стороны считают его сами и обязаны считать одинаково. Отсюда правило,
//! которое стоит держать в голове: **счётчик сбрасывается в ноль при каждой
//! смене ключей**, а не продолжается. Не сброшенный счётчик даёт «подпись не
//! сходится» на первой же записи после рукопожатия.

use chacha20poly1305::aead::AeadInPlace;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};

use crate::hkdf::{HASH_LEN, expand_label};

/// Длина подписи Poly1305.
pub const TAG_LEN: usize = 16;

/// Длина заголовка записи.
pub const HEADER_LEN: usize = 5;

/// Наибольшее содержимое одной записи по стандарту.
pub const MAX_PLAINTEXT: usize = 16_384;

/// Наибольшая длина зашифрованной части, которую мы согласны принять.
///
/// Стандарт разрешает `2^14 + 256`; больше — это не запись, а попытка занять
/// память. Отказ обязан быть здесь, до чтения тела.
pub const MAX_CIPHERTEXT: usize = MAX_PLAINTEXT + 256;

/// Типы содержимого.
pub mod content {
    pub const CHANGE_CIPHER_SPEC: u8 = 20;
    pub const ALERT: u8 = 21;
    pub const HANDSHAKE: u8 = 22;
    pub const APPLICATION_DATA: u8 = 23;
}

/// Ключи одного направления.
pub struct Keys {
    cipher: ChaCha20Poly1305,
    iv: [u8; 12],
    /// Порядковый номер записи в этом направлении.
    sequence: u64,
    /// Секрет, из которого выведены ключи, — нужен для `KeyUpdate`.
    secret: [u8; HASH_LEN],
}

impl Keys {
    /// Вывести ключ и `iv` из секрета направления.
    #[must_use]
    pub fn new(secret: &[u8; HASH_LEN]) -> Self {
        let mut key = [0u8; 32];
        let mut iv = [0u8; 12];
        expand_label(secret, "key", &[], &mut key);
        expand_label(secret, "iv", &[], &mut iv);
        Self {
            cipher: ChaCha20Poly1305::new((&key).into()),
            iv,
            sequence: 0,
            secret: *secret,
        }
    }

    /// Обновить ключи по `KeyUpdate` — и сбросить счётчик.
    pub fn update(&mut self) {
        let mut next = [0u8; HASH_LEN];
        expand_label(&self.secret, "traffic upd", &[], &mut next);
        *self = Self::new(&next);
    }

    /// Nonce этой записи: `iv`, сложенный по модулю два с номером.
    fn nonce(&self) -> [u8; 12] {
        let mut nonce = self.iv;
        let counter = self.sequence.to_be_bytes();
        for (index, byte) in counter.iter().enumerate() {
            nonce[4 + index] ^= byte;
        }
        nonce
    }

    /// Собрать зашифрованную запись целиком в `out` и вернуть её длину.
    ///
    /// `out` обязан вместить `plaintext.len() + 1 + TAG_LEN + HEADER_LEN`.
    pub fn seal(&mut self, kind: u8, plaintext: &[u8], out: &mut [u8]) -> Option<usize> {
        let inner = plaintext.len() + 1;
        let total = HEADER_LEN + inner + TAG_LEN;
        if plaintext.len() > MAX_PLAINTEXT || out.len() < total {
            return None;
        }
        // Заголовок пишется **до** шифрования: он же и есть присоединённые
        // данные, по которым считается подпись.
        let length = (inner + TAG_LEN) as u16;
        out[0] = content::APPLICATION_DATA;
        out[1] = 0x03;
        out[2] = 0x03;
        out[3..5].copy_from_slice(&length.to_be_bytes());

        out[HEADER_LEN..HEADER_LEN + plaintext.len()].copy_from_slice(plaintext);
        out[HEADER_LEN + plaintext.len()] = kind;

        let nonce = self.nonce();
        let (header, body) = out.split_at_mut(HEADER_LEN);
        let tag = self
            .cipher
            .encrypt_in_place_detached((&nonce).into(), header, &mut body[..inner])
            .ok()?;
        body[inner..inner + TAG_LEN].copy_from_slice(&tag);
        self.sequence += 1;
        Some(total)
    }

    /// Расшифровать тело записи на месте.
    ///
    /// Возвращает длину содержимого и его настоящий тип. Дополнение нулями,
    /// которое сервер вправе добавить, снимается здесь же: искать тип надо с
    /// конца, пропуская нули, а не брать последний байт.
    pub fn open(&mut self, header: &[u8], body: &mut [u8]) -> Option<(usize, u8)> {
        if body.len() < TAG_LEN {
            return None;
        }
        let (data, tag) = body.split_at_mut(body.len() - TAG_LEN);
        let nonce = self.nonce();
        self.cipher
            .decrypt_in_place_detached(
                (&nonce).into(),
                header,
                data,
                chacha20poly1305::Tag::from_slice(tag),
            )
            .ok()?;
        self.sequence += 1;

        let mut end = data.len();
        while end > 0 && data[end - 1] == 0 {
            end -= 1;
        }
        // Запись без единого ненулевого байта не имеет типа вовсе: по стандарту
        // это ошибка, а не пустое сообщение.
        if end == 0 {
            return None;
        }
        Some((end - 1, data[end - 1]))
    }
}

/// Прочитать заголовок записи: тип и длину тела.
#[must_use]
pub fn header(bytes: &[u8]) -> Option<(u8, usize)> {
    if bytes.len() < HEADER_LEN {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([bytes[3], bytes[4]]));
    Some((bytes[0], length))
}
