//! `chacha20-poly1305@openssh.com`: шифрование пакетов.
//!
//! # Почему не готовый AEAD
//!
//! Потому что это **не** конструкция RFC 8439, хотя названа похоже. Отличий
//! четыре, и каждое ломает совместимость, если его не заметить:
//!
//! 1. **Ключей два.** Шестьдесят четыре байта ключевого материала делятся
//!    пополам: первая половина (`K_1`) шифрует только четырёхбайтовую длину
//!    пакета, вторая (`K_2`) — всё остальное. Смысл в том, чтобы длину можно
//!    было расшифровать и прочитать **до** проверки подписи: не зная длины,
//!    непонятно, где кончается пакет и откуда брать саму подпись.
//! 2. **Ключ Poly1305 берётся из потока.** Первые 32 байта нулевого блока
//!    ChaCha20 под ключом `K_2` — это одноразовый ключ подписи, и он же
//!    выбрасывает остаток блока: содержимое шифруется начиная с блока номер 1.
//! 3. **Nonce — это номер пакета.** Восемь байт счётчика, старшим вперёд, а не
//!    случайные байты и не часть ключа. Счётчик растёт у каждой стороны свой и
//!    никогда не повторяется — на этом держится вся стойкость.
//! 4. **Подписывается зашифрованное.** Poly1305 считается по зашифрованной
//!    длине и зашифрованному содержимому — encrypt-then-MAC. Проверять подпись
//!    надо **до** расшифровки содержимого, иначе мы обрабатываем то, что
//!    подделал кто угодно.
//!
//! Собрать это из готового `chacha20poly1305` невозможно: там другой порядок и
//! один ключ. Поэтому здесь ChaCha20 и Poly1305 берутся по отдельности, а
//! склейка — своя, зато ровно та, которую ждёт OpenSSH.

use chacha20::ChaCha20Legacy;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use poly1305::Poly1305;
use poly1305::universal_hash::KeyInit;

/// Длина ключевого материала: два ключа по 32 байта.
pub const KEY_LEN: usize = 64;

/// Длина подписи Poly1305.
pub const TAG_LEN: usize = 16;

/// Длина поля длины пакета.
pub const LENGTH_LEN: usize = 4;

/// Пара ключей одного направления.
#[derive(Clone)]
pub struct Cipher {
    /// Ключ длины.
    length_key: [u8; 32],
    /// Ключ содержимого.
    payload_key: [u8; 32],
}

impl Cipher {
    /// Разобрать ключевой материал на два ключа.
    ///
    /// Порядок половин обратный тому, которого ждёшь: **первые** 32 байта —
    /// это `K_2` (содержимое), вторые — `K_1` (длина). Так записано в
    /// спецификации OpenSSH, и перепутанные половины дают соединение, в котором
    /// длина расшифровывается мусором.
    pub fn new(material: &[u8; KEY_LEN]) -> Self {
        let mut payload_key = [0u8; 32];
        let mut length_key = [0u8; 32];
        payload_key.copy_from_slice(&material[..32]);
        length_key.copy_from_slice(&material[32..]);
        Self { length_key, payload_key }
    }

    /// Расшифровать четыре байта длины.
    pub fn decrypt_length(&self, sequence: u64, encrypted: [u8; LENGTH_LEN]) -> u32 {
        let mut buffer = encrypted;
        let mut stream = stream(&self.length_key, sequence);
        stream.apply_keystream(&mut buffer);
        u32::from_be_bytes(buffer)
    }

    /// Зашифровать четыре байта длины.
    pub fn encrypt_length(&self, sequence: u64, length: u32) -> [u8; LENGTH_LEN] {
        let mut buffer = length.to_be_bytes();
        let mut stream = stream(&self.length_key, sequence);
        stream.apply_keystream(&mut buffer);
        buffer
    }

    /// Одноразовый ключ подписи для этого пакета.
    fn tag_key(&self, sequence: u64) -> [u8; 32] {
        let mut key = [0u8; 32];
        let mut stream = stream(&self.payload_key, sequence);
        // Нулевой блок целиком: первые 32 байта — ключ, остальные 32
        // выбрасываются. Именно поэтому содержимое шифруется с блока 1.
        stream.apply_keystream(&mut key);
        key
    }

    /// Проверить подпись пакета.
    ///
    /// `framed` — зашифрованная длина вместе с зашифрованным содержимым, ровно
    /// как они пришли с провода.
    pub fn verify(&self, sequence: u64, framed: &[u8], tag: &[u8; TAG_LEN]) -> bool {
        let expected = self.sign(sequence, framed);
        // Сравнение постоянного времени: побайтовое с ранним выходом
        // рассказывает тому, кто подбирает подпись, сколько байт он угадал.
        let mut difference = 0u8;
        for (a, b) in expected.iter().zip(tag.iter()) {
            difference |= a ^ b;
        }
        difference == 0
    }

    /// Подписать пакет.
    ///
    /// `compute_unpadded`, а не `update_padded` с последующим `finalize`, и
    /// разница здесь не стилистическая. Второе — это Poly1305 из конструкции
    /// AEAD RFC 8439, где каждая часть сообщения дополняется нулями до
    /// шестнадцати байт. OpenSSH считает подпись **по всему пакету целиком**,
    /// как оригинальный `poly1305_auth`, и лишнее дополнение даёт тег, который
    /// не сходится ни у одной стороны. Снаружи это выглядит так: длина пакета
    /// расшифровалась правильно, а подпись «неверна» — то есть ключи в
    /// порядке, а MAC считается не тот.
    pub fn sign(&self, sequence: u64, framed: &[u8]) -> [u8; TAG_LEN] {
        let key = self.tag_key(sequence);
        let mac = Poly1305::new(&key.into());
        mac.compute_unpadded(framed).into()
    }

    /// Зашифровать или расшифровать содержимое пакета на месте.
    ///
    /// Одна функция на оба направления: поточный шифр — это исключающее «или»
    /// с ключевым потоком, и обратная операция совпадает с прямой.
    pub fn apply_payload(&self, sequence: u64, data: &mut [u8]) {
        let mut stream = stream(&self.payload_key, sequence);
        // Пропускаем нулевой блок: он ушёл на ключ подписи.
        stream.seek(64u64);
        stream.apply_keystream(data);
    }
}

/// Поток ChaCha20 с nonce из номера пакета.
///
/// `ChaCha20Legacy` — это вариант с 64-битным nonce и 64-битным счётчиком
/// блоков (оригинал Бернштейна), и именно его требует OpenSSH. Обычный
/// `ChaCha20` из RFC 8439 делит те же 128 бит иначе — 96 на nonce и 32 на
/// счётчик, — и ключевой поток получается другим при тех же входных данных.
fn stream(key: &[u8; 32], sequence: u64) -> ChaCha20Legacy {
    let nonce = sequence.to_be_bytes();
    ChaCha20Legacy::new(key.into(), &nonce.into())
}
