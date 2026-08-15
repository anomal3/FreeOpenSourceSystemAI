//! Проверка подписей ECDSA на кривых P-256 и P-384.
//!
//! # Почему две кривые, а не одна
//!
//! Потому что настоящая цепочка их и требует. `github.com` подписан ключом
//! P-256, промежуточный Sectigo — тоже P-256, а корень, которым подписан
//! промежуточный, — P-384. Одной кривой хватает ровно до предпоследнего шага, и
//! отказ выглядел бы как «сертификат не проверяется», хотя проверять было
//! нечем.
//!
//! # Почему подпись приходит в DER
//!
//! Потому что и в сертификате, и в `CertificateVerify` TLS 1.3 подпись ECDSA
//! записана одинаково: `SEQUENCE { r INTEGER, s INTEGER }`. Это отличает её от
//! подписи RSA, которая и там и там лежит просто числом. Разбор здесь общий, и
//! это не совпадение — так написано в RFC 8446, §4.2.3.
//!
//! # Усечение хеша делаем не мы
//!
//! Хеш длиннее порядка кривой (SHA-384 при ключе P-256) обязан быть усечён по
//! правилу из FIPS 186-4, а короче — дополнен слева. Правило короткое, но
//! написать его самому означало бы написать его иначе, чем написали те, кто
//! подписывал. Поэтому берётся `verify_prehash` из крейта `ecdsa`: он и есть
//! эта операция.

use crate::hash::{Hash, MAX_LEN};

/// Кривая, на которой лежит ключ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Curve {
    /// Длина координаты в байтах — она же длина `r` и `s` в подписи.
    #[must_use]
    pub const fn field(self) -> usize {
        match self {
            Self::P256 => 32,
            Self::P384 => 48,
        }
    }
}

/// Проверить подпись ECDSA.
///
/// `point` — открытый ключ в форме SEC1 (`0x04 || x || y`), как он лежит в
/// `subjectPublicKey`. Сжатая форма тоже принимается: её понимает
/// `from_sec1_bytes`, и отказ от неё был бы отказом без причины.
#[must_use]
pub fn verify(
    curve: Curve,
    point: &[u8],
    hash: Hash,
    message: &[&[u8]],
    signature_der: &[u8],
) -> bool {
    let field = curve.field();
    let mut raw = [0u8; 96];
    let Some(raw) = decode_signature(signature_der, field, &mut raw) else {
        return false;
    };

    let mut digest = [0u8; MAX_LEN];
    let digest = hash.compute(message, &mut digest);

    match curve {
        Curve::P256 => {
            use p256::ecdsa::{Signature, VerifyingKey};
            use signature::hazmat::PrehashVerifier;
            let Ok(key) = VerifyingKey::from_sec1_bytes(point) else {
                return false;
            };
            let Ok(signature) = Signature::from_slice(raw) else {
                return false;
            };
            key.verify_prehash(digest, &signature).is_ok()
        }
        Curve::P384 => {
            use p384::ecdsa::{Signature, VerifyingKey};
            use signature::hazmat::PrehashVerifier;
            let Ok(key) = VerifyingKey::from_sec1_bytes(point) else {
                return false;
            };
            let Ok(signature) = Signature::from_slice(raw) else {
                return false;
            };
            key.verify_prehash(digest, &signature).is_ok()
        }
    }
}

/// Разложить `SEQUENCE { r, s }` в два числа фиксированной длины подряд.
///
/// Дополнение слева нулями обязательно: в DER числа записаны без ведущих нулей,
/// а крейт ждёт ровно по `field` байт на каждое. Подпись, у которой `r`
/// случайно оказалось коротким на байт, иначе не проверялась бы — примерно раз
/// на двести пятьдесят шесть.
fn decode_signature<'a>(der: &[u8], field: usize, out: &'a mut [u8; 96]) -> Option<&'a [u8]> {
    use crate::der::{Reader, tag, unsigned};

    let mut reader = Reader::new(der);
    let mut pair = reader.sequence().ok()?;
    // Хвост после последовательности — это не «лишние байты», а другая подпись,
    // приклеенная к нашей. Молча их игнорировать нельзя.
    if !reader.is_empty() {
        return None;
    }
    let r = unsigned(pair.expect(tag::INTEGER).ok()?).ok()?;
    let s = unsigned(pair.expect(tag::INTEGER).ok()?).ok()?;
    if !pair.is_empty() || r.len() > field || s.len() > field || 2 * field > out.len() {
        return None;
    }
    out.fill(0);
    out[field - r.len()..field].copy_from_slice(r);
    out[2 * field - s.len()..2 * field].copy_from_slice(s);
    Some(&out[..2 * field])
}

#[cfg(test)]
mod tests {
    use super::{Curve, decode_signature};
    use std::vec;

    /// Короткое `r` дополняется слева, а не прижимается к началу.
    #[test]
    fn short_scalars_are_left_padded() {
        // SEQUENCE { INTEGER 0x01, INTEGER 0x02 }
        let der = [0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x02];
        let mut out = [0u8; 96];
        let raw = decode_signature(&der, 32, &mut out).expect("подпись разбирается");
        assert_eq!(raw.len(), 64);
        assert_eq!(raw[31], 1);
        assert_eq!(raw[63], 2);
        assert!(raw[..31].iter().all(|byte| *byte == 0));
    }

    /// Число длиннее поля кривой — это не подпись на этой кривой.
    #[test]
    fn an_oversized_scalar_is_refused() {
        let mut der = vec![0x30, 0x27, 0x02, 0x22, 0x00];
        der.extend(core::iter::repeat_n(0xFF, 33));
        der.extend_from_slice(&[0x02, 0x01, 0x02]);
        let mut out = [0u8; 96];
        assert!(decode_signature(&der, 32, &mut out).is_none());
    }

    /// Байты после последовательности — повод отказать, а не пропустить.
    #[test]
    fn trailing_bytes_are_refused() {
        let der = [0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x02, 0x00];
        let mut out = [0u8; 96];
        assert!(decode_signature(&der, 32, &mut out).is_none());
    }

    #[test]
    fn field_sizes_are_the_curve_sizes() {
        assert_eq!(Curve::P256.field(), 32);
        assert_eq!(Curve::P384.field(), 48);
    }
}
