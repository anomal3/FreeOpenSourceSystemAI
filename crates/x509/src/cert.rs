//! Сертификат X.509: разбор и проверка подписи под ним.
//!
//! # Что разбирается, а что нет
//!
//! Разбирается ровно то, от чего зависит решение «верить или нет»:
//! `TBSCertificate` целиком как байты (по ним считается подпись), издатель и
//! subject (по ним ищется следующее звено), срок действия, открытый ключ,
//! `subjectAltName`, `basicConstraints`, `keyUsage` и `extendedKeyUsage`.
//!
//! Не разбирается — и это сказано вслух — всё остальное: `CRL`, `OCSP`,
//! `nameConstraints`, `policyConstraints`, `authorityKeyIdentifier`. Отзыв
//! сертификата эта система не проверяет **вовсе**: ни списком, ни по сети.
//! Причина в том, что и то и другое требует ходить в сеть до того, как сеть
//! признана исправной, — а доверие к обновлению у нас держится не на TLS, а на
//! подписи Ed25519 под самим образом (фаза 39). TLS здесь нужен затем, чтобы
//! GitHub вообще ответил.
//!
//! # Критические расширения
//!
//! Расширение, помеченное критическим и незнакомое нам, — повод отказать.
//! Так требует RFC 5280, и требование не формальное: `nameConstraints` бывает
//! критическим, и сертификат, которому запрещено выписывать имена в чужой зоне,
//! без этой проверки выписывал бы их беспрепятственно.

use crate::der::{self, Reader, tag};
use crate::ecdsa::Curve;
use crate::hash::Hash;
use crate::oid;
use crate::rsa;
use crate::time;

/// Чем сертификат оказался негоден.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Байты не складываются в DER.
    Der(der::Error),
    /// Версия не третья, а расширения есть: так не бывает.
    BadVersion,
    /// Ключ такого вида эта система не проверяет.
    UnknownKey,
    /// Подпись таким алгоритмом эта система не проверяет.
    UnknownSignature,
    /// Алгоритм внутри `TBSCertificate` и снаружи разные.
    SignatureMismatch,
    /// Дата записана не так, как её пишет DER.
    BadTime,
    /// Критическое расширение, о котором мы ничего не знаем.
    UnknownCritical,
    /// Расширение есть, но записано не по правилам.
    BadExtension,
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Der(inner) => inner.text(),
            Self::BadVersion => "a certificate version that cannot carry what it carries",
            Self::UnknownKey => "a public key of a kind this system cannot check",
            Self::UnknownSignature => "a signature algorithm this system cannot check",
            Self::SignatureMismatch => "the certificate names two different signature algorithms",
            Self::BadTime => "a validity date that is not written the way DER writes dates",
            Self::UnknownCritical => "a critical extension this system does not understand",
            Self::BadExtension => "an extension that is not written the way it must be",
        }
    }
}

impl From<der::Error> for Error {
    fn from(inner: der::Error) -> Self {
        Self::Der(inner)
    }
}

/// Открытый ключ из `subjectPublicKeyInfo`.
#[derive(Debug, Clone, Copy)]
pub enum PublicKey<'a> {
    Rsa(rsa::PublicKey<'a>),
    Ec { curve: Curve, point: &'a [u8] },
}

/// Чем подписан сертификат.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// `RSASSA-PKCS1-v1_5` — то, чем подписано почти всё в сегодняшнем интернете.
    RsaPkcs1(Hash),
    /// `RSASSA-PSS`. В сертификатах встречается редко, в `CertificateVerify`
    /// TLS 1.3 — единственно допустимая форма RSA.
    RsaPss(Hash),
    Ecdsa(Hash),
}

impl Algorithm {
    /// Проверить подпись этим алгоритмом ключом `key`.
    #[must_use]
    pub fn verify(self, key: &PublicKey<'_>, message: &[&[u8]], signature: &[u8]) -> bool {
        match (self, key) {
            (Self::RsaPkcs1(hash), PublicKey::Rsa(key)) => {
                rsa::verify_pkcs1(*key, hash, message, signature)
            }
            (Self::RsaPss(hash), PublicKey::Rsa(key)) => {
                rsa::verify_pss(*key, hash, message, signature)
            }
            (Self::Ecdsa(hash), PublicKey::Ec { curve, point }) => {
                crate::ecdsa::verify(*curve, point, hash, message, signature)
            }
            // Подпись RSA ключом на кривой (и наоборот) — не ошибка разбора, а
            // попытка выдать одно за другое. Отказ обязан быть здесь, а не
            // где-то в арифметике.
            _ => false,
        }
    }
}

/// Что говорит `basicConstraints`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BasicConstraints {
    /// Расширение вообще присутствует.
    pub present: bool,
    /// Этому сертификату разрешено подписывать другие.
    pub ca: bool,
    /// Сколько промежуточных звеньев разрешено ниже него.
    pub path_len: Option<u32>,
}

/// Биты `keyUsage`, которые нас касаются.
pub mod usage {
    /// `digitalSignature` — бит 0.
    pub const DIGITAL_SIGNATURE: u16 = 1 << 0;
    /// `keyCertSign` — бит 5.
    pub const KEY_CERT_SIGN: u16 = 1 << 5;
}

/// Разобранный сертификат. Все поля — срезы поверх исходного буфера.
#[derive(Debug, Clone, Copy)]
pub struct Certificate<'a> {
    /// Байты сертификата целиком.
    pub raw: &'a [u8],
    /// `TBSCertificate` вместе с заголовком — то, по чему считается подпись.
    pub tbs: &'a [u8],
    /// Серийный номер как есть.
    pub serial: &'a [u8],
    /// DER издателя целиком, вместе с заголовком.
    pub issuer: &'a [u8],
    /// DER subject целиком, вместе с заголовком.
    pub subject: &'a [u8],
    /// Начало и конец срока действия в секундах эпохи Unix.
    pub not_before: i64,
    pub not_after: i64,
    /// Открытый ключ.
    pub key: PublicKey<'a>,
    /// Чем подписан этот сертификат (подписывал издатель).
    pub algorithm: Algorithm,
    /// Сама подпись.
    pub signature: &'a [u8],
    /// Содержимое `subjectAltName`, если оно есть.
    san: Option<&'a [u8]>,
    pub basic: BasicConstraints,
    /// `keyUsage`, если расширение есть.
    pub key_usage: Option<u16>,
    /// Есть ли `extendedKeyUsage` и разрешает ли он серверную проверку
    /// подлинности. `None` — расширения нет, то есть разрешено всё.
    pub server_auth: Option<bool>,
}

impl<'a> Certificate<'a> {
    /// Разобрать сертификат из DER.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut outer = Reader::new(bytes);
        let whole = {
            let mut probe = Reader::new(bytes);
            probe.next_raw()?
        };
        let mut certificate = outer.sequence()?;

        // TBSCertificate берётся дважды: сырым (по нему считается подпись) и
        // разобранным. Пересобрать сырые байты из полей нельзя — чужой
        // кодировщик мог записать длину иначе.
        let tbs_raw = {
            let mut probe = certificate;
            probe.next_raw()?
        };
        let mut tbs = certificate.sequence()?;

        // Внешний AlgorithmIdentifier и подпись.
        let outer_algorithm = certificate.expect(tag::SEQUENCE)?;
        let signature = der::bit_string(certificate.expect(tag::BIT_STRING)?)?;
        if !certificate.is_empty() {
            return Err(Error::Der(der::Error::BadValue));
        }

        // version [0] EXPLICIT — по умолчанию v1, то есть поля может не быть.
        let version = match tbs.context_if(0)? {
            Some(body) => {
                let mut reader = Reader::new(body);
                let raw = reader.expect(tag::INTEGER)?;
                match raw {
                    [0] => 1,
                    [1] => 2,
                    [2] => 3,
                    _ => return Err(Error::BadVersion),
                }
            }
            None => 1,
        };

        let serial = tbs.expect(tag::INTEGER)?;
        let inner_algorithm = tbs.expect(tag::SEQUENCE)?;
        let issuer = tbs.next_raw()?;
        let validity = tbs.expect(tag::SEQUENCE)?;
        let subject = tbs.next_raw()?;
        let spki = tbs.expect(tag::SEQUENCE)?;

        // Подпись стоит в сертификате дважды: внутри подписанной части и
        // снаружи. Совпадать они обязаны — иначе злоумышленник подменил бы
        // внешнюю на более слабую, а проверяли бы мы по ней.
        if inner_algorithm != outer_algorithm {
            return Err(Error::SignatureMismatch);
        }
        let algorithm = parse_algorithm(inner_algorithm)?;

        let mut validity = Reader::new(validity);
        let not_before = parse_time(validity.next()?)?;
        let not_after = parse_time(validity.next()?)?;

        let key = parse_key(spki)?;

        // issuerUniqueID [1] и subjectUniqueID [2] — поля версии 2, IMPLICIT
        // BIT STRING, то есть примитивная метка. Встречаются в старых
        // сертификатах; нам они не нужны, но пропустить их надо, иначе
        // расширения не найдутся.
        tbs.skip_if(tag::context_primitive(1))?;
        tbs.skip_if(tag::context_primitive(2))?;

        let mut san = None;
        let mut basic = BasicConstraints::default();
        let mut key_usage = None;
        let mut server_auth = None;

        if let Some(body) = tbs.context_if(3)? {
            if version != 3 {
                return Err(Error::BadVersion);
            }
            let mut list = Reader::new(body).sequence()?;
            while !list.is_empty() {
                let mut extension = list.sequence()?;
                let id = extension.expect(tag::OID)?;
                let critical = match extension.peek() {
                    Some(tag::BOOLEAN) => extension.expect(tag::BOOLEAN)? != [0x00],
                    _ => false,
                };
                let value = extension.expect(tag::OCTET_STRING)?;
                match id {
                    _ if id == oid::SUBJECT_ALT_NAME => san = Some(value),
                    _ if id == oid::BASIC_CONSTRAINTS => basic = parse_basic(value)?,
                    _ if id == oid::KEY_USAGE => key_usage = Some(parse_key_usage(value)?),
                    _ if id == oid::EXT_KEY_USAGE => {
                        server_auth = Some(parse_ext_key_usage(value)?);
                    }
                    _ if critical => return Err(Error::UnknownCritical),
                    _ => {}
                }
            }
        }

        Ok(Self {
            raw: whole,
            tbs: tbs_raw,
            serial,
            issuer,
            subject,
            not_before,
            not_after,
            key,
            algorithm,
            signature,
            san,
            basic,
            key_usage,
            server_auth,
        })
    }

    /// Действителен ли сертификат в этот момент (секунды эпохи Unix).
    #[must_use]
    pub const fn valid_at(&self, now: i64) -> bool {
        now >= self.not_before && now <= self.not_after
    }

    /// Проверить подпись под `child` этим сертификатом как издателем.
    #[must_use]
    pub fn signed(&self, child: &Certificate<'_>) -> bool {
        child.algorithm.verify(&self.key, &[child.tbs], child.signature)
    }

    /// Имена из `subjectAltName`.
    #[must_use]
    pub fn dns_names(&self) -> DnsNames<'a> {
        let reader = match self.san {
            Some(bytes) => sequence_of(Reader::new(bytes)),
            None => Reader::new(&[]),
        };
        DnsNames { reader, done: false }
    }

    /// Адреса из `subjectAltName` — те, что записаны числами, а не именем.
    #[must_use]
    pub fn ip_names(&self) -> IpNames<'a> {
        let reader = match self.san {
            Some(bytes) => sequence_of(Reader::new(bytes)),
            None => Reader::new(&[]),
        };
        IpNames { reader }
    }

    /// Годится ли этот сертификат для имени, к которому мы подключались.
    ///
    /// Имя, записанное числами (`10.0.2.2`), сверяется с `iPAddress`, а не с
    /// `dNSName`, и это не придирка: сертификат с `dNSName = 10.0.2.2` выписать
    /// можно, но означает он другое — «сервер, чьё доменное имя выглядит как
    /// адрес». Путать их значит принимать один за другой.
    ///
    /// `commonName` не смотрится **намеренно**: RFC 6125 объявил его негодным
    /// для этой цели ещё в 2011 году, и браузеры перестали его читать. Читать
    /// его сегодня значит принимать сертификат, который сегодняшний удостоверяющий
    /// центр выписал, не думая про это поле.
    #[must_use]
    pub fn matches(&self, host: &str) -> bool {
        match ipv4(host) {
            Some(address) => self.ip_names().any(|name| name == address),
            None => self.dns_names().any(|name| matches_name(name, host)),
        }
    }
}

/// Курсор по `GeneralNames`, отдающий только `iPAddress` длиной в четыре байта.
pub struct IpNames<'a> {
    reader: Reader<'a>,
}

impl Iterator for IpNames<'_> {
    type Item = [u8; 4];

    fn next(&mut self) -> Option<[u8; 4]> {
        while !self.reader.is_empty() {
            let value = self.reader.next().ok()?;
            // `iPAddress` — это `[7] IMPLICIT OCTET STRING`. Шестнадцать байт
            // означают IPv6, которого у этой системы нет вовсе.
            if value.tag == tag::context_primitive(7) && value.bytes.len() == 4 {
                return Some([value.bytes[0], value.bytes[1], value.bytes[2], value.bytes[3]]);
            }
        }
        None
    }
}

/// Разобрать `10.0.2.2`; `None` — это не адрес, а имя.
fn ipv4(text: &str) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut seen = 0usize;
    for (index, part) in text.split('.').enumerate() {
        if index >= 4 || part.is_empty() {
            return None;
        }
        out[index] = part.parse::<u8>().ok()?;
        seen = index + 1;
    }
    (seen == 4).then_some(out)
}

/// Курсор по `GeneralNames`, отдающий только `dNSName`.
pub struct DnsNames<'a> {
    reader: Reader<'a>,
    done: bool,
}

/// Развернуть `SEQUENCE OF` в курсор по его содержимому.
fn sequence_of(mut reader: Reader<'_>) -> Reader<'_> {
    match reader.sequence() {
        Ok(inner) => inner,
        // Испорченное расширение даёт пустой список имён, а не панику: решение
        // «не подходит ни одному имени» — правильный ответ на такой сертификат.
        Err(_) => Reader::new(&[]),
    }
}

impl<'a> Iterator for DnsNames<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.done {
            return None;
        }
        while !self.reader.is_empty() {
            let Ok(value) = self.reader.next() else {
                self.done = true;
                return None;
            };
            // `dNSName` — это `[2] IMPLICIT IA5String`, то есть примитивная
            // метка `0x82`. Всё остальное (`iPAddress`, `rfc822Name`,
            // `otherName`) пропускается: мы подключаемся по имени.
            if value.tag == tag::context_primitive(2) {
                if let Ok(name) = core::str::from_utf8(value.bytes) {
                    return Some(name);
                }
            }
        }
        self.done = true;
        None
    }
}

/// Совпадает ли имя из сертификата с именем, к которому подключались.
///
/// Звёздочка допускается ровно одна, ровно в самой левой метке и ровно вместо
/// **всей** метки: `*.github.io` подходит `pages.github.io` и не подходит ни
/// `github.io`, ни `a.b.github.io`. Ослабление любого из трёх условий — это
/// известные способы выписать себе сертификат на чужую зону.
#[must_use]
pub fn matches_name(pattern: &str, host: &str) -> bool {
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    let Some(rest) = pattern.strip_prefix("*.") else {
        return equal_ignoring_case(pattern, host);
    };
    // Звёздочка не подменяет метку верхнего уровня: `*.com` не должен подходить
    // ничему.
    if rest.split('.').count() < 2 {
        return false;
    }
    let Some((label, tail)) = host.split_once('.') else {
        return false;
    };
    !label.is_empty() && equal_ignoring_case(rest, tail)
}

fn equal_ignoring_case(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
}

/// Разобрать `AlgorithmIdentifier` подписи.
fn parse_algorithm(bytes: &[u8]) -> Result<Algorithm, Error> {
    let (id, parameters) = der::algorithm(bytes)?;
    let algorithm = match id {
        _ if id == oid::SHA256_WITH_RSA => Algorithm::RsaPkcs1(Hash::Sha256),
        _ if id == oid::SHA384_WITH_RSA => Algorithm::RsaPkcs1(Hash::Sha384),
        _ if id == oid::SHA512_WITH_RSA => Algorithm::RsaPkcs1(Hash::Sha512),
        _ if id == oid::ECDSA_WITH_SHA256 => Algorithm::Ecdsa(Hash::Sha256),
        _ if id == oid::ECDSA_WITH_SHA384 => Algorithm::Ecdsa(Hash::Sha384),
        _ if id == oid::ECDSA_WITH_SHA512 => Algorithm::Ecdsa(Hash::Sha512),
        _ if id == oid::RSASSA_PSS => {
            let body = parameters.ok_or(Error::UnknownSignature)?;
            return Ok(Algorithm::RsaPss(parse_pss_hash(body.bytes)?));
        }
        _ => return Err(Error::UnknownSignature),
    };
    Ok(algorithm)
}

/// Какой хеш назван в параметрах `RSASSA-PSS`.
///
/// ```text
/// RSASSA-PSS-params ::= SEQUENCE {
///     hashAlgorithm    [0] HashAlgorithm    DEFAULT sha1,
///     maskGenAlgorithm [1] MaskGenAlgorithm DEFAULT mgf1SHA1,
///     saltLength       [2] INTEGER          DEFAULT 20,
///     trailerField     [3] INTEGER          DEFAULT 1 }
/// ```
///
/// Умолчания все до одного означают SHA-1, и принимать их — значит принимать
/// SHA-1. Поэтому отсутствие поля здесь отказ, а не «по умолчанию».
fn parse_pss_hash(bytes: &[u8]) -> Result<Hash, Error> {
    let mut params = Reader::new(bytes);
    let body = params.context_if(0)?.ok_or(Error::UnknownSignature)?;
    let (id, _) = der::algorithm(body)?;
    let hash = match id {
        _ if id == oid::SHA256 => Hash::Sha256,
        _ if id == oid::SHA384 => Hash::Sha384,
        _ if id == oid::SHA512 => Hash::Sha512,
        _ => return Err(Error::UnknownSignature),
    };
    // Маска обязана считаться тем же хешем: MGF1 с другим — законная запись, но
    // проверять её этим кодом нельзя, а «почти проверили» здесь не бывает.
    let mask = params.context_if(1)?.ok_or(Error::UnknownSignature)?;
    let (mask_id, mask_hash) = der::algorithm(mask)?;
    if mask_id != oid::MGF1 {
        return Err(Error::UnknownSignature);
    }
    let mask_hash = mask_hash.ok_or(Error::UnknownSignature)?;
    let (inner, _) = der::algorithm(mask_hash.bytes)?;
    if inner != id {
        return Err(Error::UnknownSignature);
    }
    // Соль длиной с хеш — единственное, что этот код умеет проверять.
    if let Some(salt) = params.context_if(2)? {
        let mut reader = Reader::new(salt);
        let value = reader.expect(tag::INTEGER)?;
        let mut length = 0usize;
        for byte in der::unsigned(value)? {
            length = (length << 8) | usize::from(*byte);
        }
        if length != hash.len() {
            return Err(Error::UnknownSignature);
        }
    } else {
        return Err(Error::UnknownSignature);
    }
    Ok(hash)
}

/// Разобрать `SubjectPublicKeyInfo`.
fn parse_key(bytes: &[u8]) -> Result<PublicKey<'_>, Error> {
    let mut spki = Reader::new(bytes);
    let algorithm = spki.expect(tag::SEQUENCE)?;
    let key = der::bit_string(spki.expect(tag::BIT_STRING)?)?;
    let (id, parameters) = der::algorithm(algorithm)?;

    if id == oid::RSA_ENCRYPTION {
        let mut sequence = Reader::new(key).sequence()?;
        let modulus = der::unsigned(sequence.expect(tag::INTEGER)?)?;
        let exponent = der::unsigned(sequence.expect(tag::INTEGER)?)?;
        return Ok(PublicKey::Rsa(rsa::PublicKey { modulus, exponent }));
    }
    if id == oid::EC_PUBLIC_KEY {
        let parameters = parameters.ok_or(Error::UnknownKey)?;
        if parameters.tag != tag::OID {
            return Err(Error::UnknownKey);
        }
        let curve = match parameters.bytes {
            bytes if bytes == oid::PRIME256V1 => Curve::P256,
            bytes if bytes == oid::SECP384R1 => Curve::P384,
            _ => return Err(Error::UnknownKey),
        };
        return Ok(PublicKey::Ec { curve, point: key });
    }
    Err(Error::UnknownKey)
}

/// `basicConstraints ::= SEQUENCE { cA BOOLEAN DEFAULT FALSE, pathLenConstraint INTEGER OPTIONAL }`
fn parse_basic(bytes: &[u8]) -> Result<BasicConstraints, Error> {
    let mut reader = Reader::new(bytes);
    let mut sequence = reader.sequence()?;
    let mut out = BasicConstraints { present: true, ca: false, path_len: None };
    if sequence.peek() == Some(tag::BOOLEAN) {
        out.ca = sequence.expect(tag::BOOLEAN)? != [0x00];
    }
    if sequence.peek() == Some(tag::INTEGER) {
        let value = der::unsigned(sequence.expect(tag::INTEGER)?)?;
        let mut length = 0u32;
        for byte in value {
            length = length.checked_mul(256).ok_or(Error::BadExtension)?
                + u32::from(*byte);
        }
        out.path_len = Some(length);
    }
    Ok(out)
}

/// `keyUsage ::= BIT STRING` — биты нумеруются от старшего в первом байте.
fn parse_key_usage(bytes: &[u8]) -> Result<u16, Error> {
    let mut reader = Reader::new(bytes);
    let raw = reader.expect(tag::BIT_STRING)?;
    let (&unused, rest) = raw.split_first().ok_or(Error::BadExtension)?;
    if unused > 7 {
        return Err(Error::BadExtension);
    }
    let mut bits = 0u16;
    for (index, byte) in rest.iter().take(2).enumerate() {
        for offset in 0..8 {
            if byte & (0x80 >> offset) != 0 {
                bits |= 1 << (index * 8 + offset);
            }
        }
    }
    Ok(bits)
}

/// `extKeyUsage ::= SEQUENCE OF OBJECT IDENTIFIER` — годится ли для сервера.
fn parse_ext_key_usage(bytes: &[u8]) -> Result<bool, Error> {
    let mut reader = Reader::new(bytes);
    let mut list = reader.sequence()?;
    while !list.is_empty() {
        let id = list.expect(tag::OID)?;
        if id == oid::SERVER_AUTH || id == oid::ANY_EXT_KEY_USAGE {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Разобрать `UTCTime` или `GeneralizedTime` в секунды эпохи Unix.
fn parse_time(value: der::Value<'_>) -> Result<i64, Error> {
    match value.tag {
        tag::UTC_TIME => time::utc_time(value.bytes).ok_or(Error::BadTime),
        tag::GENERALIZED_TIME => time::generalized_time(value.bytes).ok_or(Error::BadTime),
        _ => Err(Error::BadTime),
    }
}

#[cfg(test)]
mod tests {
    use super::{ipv4, matches_name};

    /// Адрес отличается от имени, и отличается по правилам.
    #[test]
    fn an_address_is_not_a_name() {
        assert_eq!(ipv4("10.0.2.2"), Some([10, 0, 2, 2]));
        assert_eq!(ipv4("255.255.255.255"), Some([255, 255, 255, 255]));
        // Четвёртая доля обязана быть, и байт обязан помещаться в байт.
        assert_eq!(ipv4("10.0.2"), None);
        assert_eq!(ipv4("10.0.2.256"), None);
        assert_eq!(ipv4("10.0.2.2.2"), None);
        assert_eq!(ipv4("10.0..2"), None);
        assert_eq!(ipv4("github.com"), None);
    }

    /// Звёздочка подменяет ровно одну метку и только самую левую.
    #[test]
    fn a_wildcard_covers_exactly_one_label() {
        assert!(matches_name("*.github.io", "pages.github.io"));
        assert!(matches_name("*.GITHUB.IO", "pages.github.io"));
        assert!(!matches_name("*.github.io", "github.io"));
        assert!(!matches_name("*.github.io", "a.b.github.io"));
        assert!(!matches_name("*.github.io", "pages.github.com"));
        // Звёздочка на весь домен верхнего уровня — сертификат на весь интернет.
        assert!(!matches_name("*.com", "example.com"));
        assert!(!matches_name("*", "example.com"));
    }

    /// Без звёздочки — точное совпадение, без учёта регистра.
    #[test]
    fn a_plain_name_matches_exactly() {
        assert!(matches_name("github.com", "GitHub.com"));
        assert!(!matches_name("github.com", "www.github.com"));
        assert!(!matches_name("ithub.com", "github.com"));
        assert!(!matches_name("", "github.com"));
    }
}
