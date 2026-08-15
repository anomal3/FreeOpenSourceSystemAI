//! Проверка подписей RSA: PKCS#1 v1.5 и PSS.
//!
//! # Зачем оба
//!
//! Потому что TLS 1.3 развёл их по разным местам, и это ловушка, на которой
//! легко потерять день. **Сертификаты** в сегодняшнем интернете подписаны
//! почти исключительно `sha256WithRSAEncryption` — это PKCS#1 v1.5. А
//! `CertificateVerify` в TLS 1.3 подписывать v1.5 **запрещено** (RFC 8446,
//! §4.4.3): там разрешены только `rsa_pss_rsae_*`. Реализовав одно из двух,
//! получаешь клиента, который либо не разбирает ни одной цепочки, либо
//! разбирает их все и падает на последнем сообщении рукопожатия — и выглядит
//! это как «сервер прислал чужой сертификат».
//!
//! # Что здесь не делается
//!
//! Не делается **подпись**: у нас нет закрытых ключей RSA и не будет. Значит
//! нет и нужды в постоянном времени исполнения (см. `bigint`), и нет случайных
//! чисел: соль при проверке не выбирается, а вычитается из заполнения.
//!
//! # Соль в PSS
//!
//! Её длина принимается равной длине хеша. Так требует TLS 1.3 для
//! `CertificateVerify` и так делает всё, что выписывает сертификаты с PSS.
//! Сказать вслух: подпись с солью другой длины будет отвергнута — не потому,
//! что она неверна, а потому что мы не берёмся угадывать длину из заполнения.

use crate::bigint::Modulus;
use crate::hash::{Hash, MAX_LEN};

/// Наибольшая длина подписи, с которой работает этот модуль.
///
/// Совпадает с пределом [`crate::bigint::MAX_BITS`]: подпись RSA ровно такой же
/// длины, как модуль.
pub const MAX_SIGNATURE: usize = crate::bigint::MAX_BITS / 8;

/// Открытый ключ RSA — срезами поверх сертификата.
#[derive(Debug, Clone, Copy)]
pub struct PublicKey<'a> {
    /// Модуль, старшим байтом вперёд, без знакового нуля.
    pub modulus: &'a [u8],
    /// Открытая экспонента, старшим байтом вперёд.
    pub exponent: &'a [u8],
}

/// Заголовок `DigestInfo` — то, что в PKCS#1 v1.5 стоит перед самим хешем.
///
/// Это DER-запись `SEQUENCE { AlgorithmIdentifier, OCTET STRING }` с уже
/// подставленной длиной. Собирать её на месте было бы можно, но она константа, а
/// константа, выписанная байтами, сверяется с RFC 8017 глазами за минуту.
const fn digest_info(hash: Hash) -> &'static [u8] {
    match hash {
        Hash::Sha256 => &[
            0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x01, 0x05, 0x00, 0x04, 0x20,
        ],
        Hash::Sha384 => &[
            0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x02, 0x05, 0x00, 0x04, 0x30,
        ],
        Hash::Sha512 => &[
            0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
            0x03, 0x05, 0x00, 0x04, 0x40,
        ],
    }
}

/// Проверить подпись `RSASSA-PKCS1-v1_5`.
///
/// `message` — части подписанного сообщения подряд (для сертификата это одна
/// часть: байты `TBSCertificate` как есть).
#[must_use]
pub fn verify_pkcs1(key: PublicKey<'_>, hash: Hash, message: &[&[u8]], signature: &[u8]) -> bool {
    let Some(modulus) = Modulus::new(key.modulus) else {
        return false;
    };
    let k = modulus.bytes();
    // Подпись обязана быть ровно длины модуля. Короче — её дополнили бы нулями
    // и она «сошлась» бы для другого числа; длиннее — это не подпись этим
    // ключом.
    if signature.len() != k || k > MAX_SIGNATURE {
        return false;
    }
    let mut em = [0u8; MAX_SIGNATURE];
    let em = &mut em[..k];
    if !modulus.modexp(signature, key.exponent, em) {
        return false;
    }

    let prefix = digest_info(hash);
    let hash_len = hash.len();
    // 0x00 0x01 | PS (не меньше восьми байт 0xFF) | 0x00 | DigestInfo | хеш
    let Some(padding_len) = k.checked_sub(3 + prefix.len() + hash_len) else {
        return false;
    };
    if padding_len < 8 {
        return false;
    }

    let mut digest = [0u8; MAX_LEN];
    let digest = hash.compute(message, &mut digest);

    let mut expected = [0u8; MAX_SIGNATURE];
    expected[0] = 0x00;
    expected[1] = 0x01;
    for byte in &mut expected[2..2 + padding_len] {
        *byte = 0xFF;
    }
    expected[2 + padding_len] = 0x00;
    let at = 3 + padding_len;
    expected[at..at + prefix.len()].copy_from_slice(prefix);
    expected[at + prefix.len()..k].copy_from_slice(digest);

    equal(em, &expected[..k])
}

/// Проверить подпись `RSASSA-PSS` (MGF1 с тем же хешем, соль длиной с хеш).
#[must_use]
pub fn verify_pss(key: PublicKey<'_>, hash: Hash, message: &[&[u8]], signature: &[u8]) -> bool {
    let Some(modulus) = Modulus::new(key.modulus) else {
        return false;
    };
    let k = modulus.bytes();
    if signature.len() != k || k > MAX_SIGNATURE {
        return false;
    }
    let mut raw = [0u8; MAX_SIGNATURE];
    if !modulus.modexp(signature, key.exponent, &mut raw[..k]) {
        return false;
    }

    // `emBits = modBits - 1`, и когда модуль занимает ровно целое число байт,
    // заполнение оказывается **на байт короче** результата возведения в
    // степень. Забытый здесь ведущий ноль — самая частая ошибка в PSS: всё
    // сходится на одних ключах и не сходится на других, в зависимости от
    // старшего бита модуля.
    let em_bits = modulus.bits() - 1;
    let em_len = em_bits.div_ceil(8);
    let em: &[u8] = match k - em_len {
        0 => &raw[..k],
        1 => {
            if raw[0] != 0 {
                return false;
            }
            &raw[1..k]
        }
        _ => return false,
    };

    let hash_len = hash.len();
    let salt_len = hash_len;
    if em_len < hash_len + salt_len + 2 {
        return false;
    }
    if em[em_len - 1] != 0xBC {
        return false;
    }

    let db_len = em_len - hash_len - 1;
    let (masked_db, h) = em.split_at(db_len);
    let h = &h[..hash_len];

    let mut db = [0u8; MAX_SIGNATURE];
    let db = &mut db[..db_len];
    mgf1(hash, h, db);
    for (byte, masked) in db.iter_mut().zip(masked_db) {
        *byte ^= masked;
    }
    // Старшие биты, которых нет в `emBits`, обязаны быть нулями. Проверять их
    // после снятия маски, а не до: до маски там что угодно.
    let spare = 8 * em_len - em_bits;
    if spare > 0 {
        if db[0] >> (8 - spare) != 0 {
            return false;
        }
        db[0] &= 0xFF >> spare;
    }

    let separator = db_len - salt_len - 1;
    if db[..separator].iter().any(|byte| *byte != 0) || db[separator] != 0x01 {
        return false;
    }
    let salt = &db[separator + 1..];

    let mut digest = [0u8; MAX_LEN];
    let digest = hash.compute(message, &mut digest);
    let mut expected = [0u8; MAX_LEN];
    // `M' = (0x00) * 8 || mHash || salt` — восемь нулей впереди не украшение:
    // они отделяют это сообщение от любого, которое кто-то мог подписать тем же
    // ключом без PSS.
    let expected = hash.compute(&[&[0u8; 8], digest, salt], &mut expected);
    equal(h, expected)
}

/// MGF1: поток нужной длины из хеша и счётчика.
fn mgf1(hash: Hash, seed: &[u8], out: &mut [u8]) {
    let mut counter: u32 = 0;
    let mut at = 0usize;
    while at < out.len() {
        let mut block = [0u8; MAX_LEN];
        let block = hash.compute(&[seed, &counter.to_be_bytes()], &mut block);
        let take = block.len().min(out.len() - at);
        out[at..at + take].copy_from_slice(&block[..take]);
        at += take;
        counter += 1;
    }
}

/// Сравнение без раннего выхода.
///
/// Секрета здесь нет, и утечка по времени никому ничего не сообщает. Написано
/// так по другой причине: подпись — единственное место, где «почти совпало» не
/// должно превращаться в решение, и цикл без `break` не даёт написать это
/// случайно.
fn equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::{PublicKey, verify_pkcs1, verify_pss};
    use crate::hash::Hash;

    /// Произвольное нечётное число длиной в 2048 бит.
    ///
    /// Это **не** ключ и не вектор: проверки ниже спрашивают, отвергается ли
    /// заведомо негодная подпись, и настоящий модуль для этого не нужен. Всё,
    /// что требует настоящего ключа, проверяется в `tests/chains.rs` — на
    /// сертификатах, выписанных чужими руками.
    const N: &str ="c47abacc2a84d56f3614d92fd62ed36ddde459664b9301dcd1d61781cfcc026b\
                     cb2399bee7e75681a80b7bf500e2d08ceae1c42ec0b707927f2b2fe92ae85208\
                     71c1c74f1b8f8dbdb5d40d3b1dcadf3f1c4f5e2b3c1a9d1b1f5e0e9b0d7f2a63\
                     8f0f27a7e51e0d8cbdfbdc9f4b2d9d1c0e1c0a8d1e9c0a1e2b3c4d5e6f70819a";

    /// Проверка «подпись не той длины отвергается» не требует настоящего ключа
    /// и ловит целый класс ошибок: дополнение нулями до длины модуля.
    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let modulus = hex::decode(N).expect("вектор записан шестнадцатеричным");
        let key = PublicKey { modulus: &modulus, exponent: &[0x01, 0x00, 0x01] };
        assert!(!verify_pkcs1(key, Hash::Sha256, &[b"message"], &[0u8; 128]));
        assert!(!verify_pss(key, Hash::Sha256, &[b"message"], &[0u8; 128]));
    }

    /// Подпись из одних нулей — это `0^e = 0`, и заполнение по нулям не сходится.
    #[test]
    fn an_all_zero_signature_is_refused() {
        let modulus = hex::decode(N).expect("вектор записан шестнадцатеричным");
        let key = PublicKey { modulus: &modulus, exponent: &[0x01, 0x00, 0x01] };
        assert!(!verify_pkcs1(key, Hash::Sha256, &[b"message"], &[0u8; 256]));
    }

    /// Настоящие подписи проверяются в `tests/chains.rs` — там, где лежат
    /// настоящие сертификаты. Здесь остаётся то, что проверяется без файлов.
    #[test]
    fn an_even_modulus_is_refused() {
        let key = PublicKey { modulus: &[0x01, 0x00], exponent: &[0x01, 0x00, 0x01] };
        assert!(!verify_pkcs1(key, Hash::Sha256, &[b"message"], &[0u8; 2]));
    }
}
