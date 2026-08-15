//! Какие хеши встречаются в цепочке — и почему их три.
//!
//! Хотелось бы один. Но подписи в настоящих цепочках устроены так: лист
//! подписан SHA-256, промежуточный — SHA-384, а SHA-512 попадается у тех, кто
//! выбирал «самое надёжное». Отказ понимать SHA-384 означал бы, что цепочка
//! `github.com` не проверяется вовсе: её промежуточный сертификат подписан
//! именно им.
//!
//! Все три — из одного крейта `sha2`, то есть цена третьего алгоритма здесь не
//! в коде, а в размере таблиц.

/// Самый длинный хеш, который эта система считает.
pub const MAX_LEN: usize = 64;

/// Алгоритм хеширования.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    /// Длина результата в байтах.
    #[must_use]
    pub const fn len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    /// Посчитать хеш по нескольким кускам подряд.
    ///
    /// Кусками, а не одним срезом, потому что подписывается не всегда один
    /// непрерывный кусок памяти: `CertificateVerify` в TLS 1.3 подписывает
    /// склейку из четырёх частей, и собирать её в буфер значило бы завести
    /// буфер под чужую длину.
    ///
    /// Возвращает срез результата — ровно [`Self::len`] байт от `out`.
    pub fn compute<'a>(self, parts: &[&[u8]], out: &'a mut [u8; MAX_LEN]) -> &'a [u8] {
        use sha2::Digest as _;
        match self {
            Self::Sha256 => {
                let mut state = sha2::Sha256::new();
                for part in parts {
                    state.update(part);
                }
                out[..32].copy_from_slice(&state.finalize());
            }
            Self::Sha384 => {
                let mut state = sha2::Sha384::new();
                for part in parts {
                    state.update(part);
                }
                out[..48].copy_from_slice(&state.finalize());
            }
            Self::Sha512 => {
                let mut state = sha2::Sha512::new();
                for part in parts {
                    state.update(part);
                }
                out[..64].copy_from_slice(&state.finalize());
            }
        }
        &out[..self.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::{Hash, MAX_LEN};

    /// Известные значения для пустой строки — самая дешёвая проверка того, что
    /// алгоритмы не перепутаны местами.
    #[test]
    fn empty_input_matches_the_published_digests() {
        let mut buffer = [0u8; MAX_LEN];
        assert_eq!(
            hex::encode(Hash::Sha256.compute(&[], &mut buffer)),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex::encode(Hash::Sha384.compute(&[], &mut buffer)),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da\
             274edebfe76f65fbd51ad2f14898b95b"
        );
    }

    /// Куски склеиваются, а не хешируются по отдельности.
    #[test]
    fn parts_are_one_message() {
        let mut whole = [0u8; MAX_LEN];
        let mut split = [0u8; MAX_LEN];
        let a = Hash::Sha256.compute(&[b"abcdef"], &mut whole).to_vec();
        let b = Hash::Sha256.compute(&[b"abc", b"def"], &mut split);
        assert_eq!(a, b);
    }
}
