//! Чтение DER: единственный способ ошибиться здесь — смещением, и увидеть это
//! можно только проверкой на настоящем сертификате.
//!
//! # Почему свой разбор, а не крейт
//!
//! Потому что нужен ровно один: пройти по TBSCertificate, достать поля и
//! указать на них срезами. Крейты ASN.1 общего назначения либо тянут кучу, либо
//! тянут макросы, порождающие код, который в этом дереве нечем прочесть глазом.
//! Здесь двести строк, и каждая проверяется тестом на файле.
//!
//! # Что этот разбор обещает
//!
//! Ничего не выделяет: значения — срезы поверх буфера вызывающего. Не принимает
//! неопределённых длин (в DER их не бывает), не принимает длин, не помещающихся
//! в `usize`, и не принимает пустых значений там, где стандарт их запрещает.

/// Метки, которые встречаются в сертификате.
pub mod tag {
    pub const BOOLEAN: u8 = 0x01;
    pub const INTEGER: u8 = 0x02;
    pub const BIT_STRING: u8 = 0x03;
    pub const OCTET_STRING: u8 = 0x04;
    pub const NULL: u8 = 0x05;
    pub const OID: u8 = 0x06;
    pub const UTF8_STRING: u8 = 0x0C;
    pub const PRINTABLE_STRING: u8 = 0x13;
    pub const IA5_STRING: u8 = 0x16;
    pub const UTC_TIME: u8 = 0x17;
    pub const GENERALIZED_TIME: u8 = 0x18;
    pub const SEQUENCE: u8 = 0x30;
    pub const SET: u8 = 0x31;

    /// Контекстная метка `[n]` в конструкторной форме — та, что стоит у полей с
    /// ключевым словом `EXPLICIT`: `[0] version`, `[3] extensions`.
    #[must_use]
    pub const fn context(n: u8) -> u8 {
        0xA0 | n
    }

    /// Контекстная метка `[n]` в примитивной форме — та, что стоит у полей
    /// `IMPLICIT` с примитивным содержимым: `dNSName` в SAN, `issuerUniqueID`.
    ///
    /// Различие не косметическое: `[2]` у имени в SAN — это `0x82`, а не
    /// `0xA2`, и разбор, ищущий конструкторную форму, не найдёт ни одного имени
    /// в исправном сертификате.
    #[must_use]
    pub const fn context_primitive(n: u8) -> u8 {
        0x80 | n
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Данные кончились посреди значения.
    Truncated,
    /// Длина записана не по правилам DER (неопределённая, с ведущим нулём или
    /// длиннее, чем помещается в машинное слово).
    BadLength,
    /// Ожидали одну метку, встретили другую.
    Unexpected { want: u8, got: u8 },
    /// Значение есть, но записано не так, как обязано быть записано.
    BadValue,
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Truncated => "the DER value ends before its length says",
            Self::BadLength => "that length is not how DER writes lengths",
            Self::Unexpected { .. } => "an unexpected DER tag",
            Self::BadValue => "a DER value that is not written the way it must be",
        }
    }
}

/// Одно значение: метка и содержимое.
#[derive(Debug, Clone, Copy)]
pub struct Value<'a> {
    pub tag: u8,
    pub bytes: &'a [u8],
}

/// Курсор по последовательности значений.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { rest: bytes }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    /// То, что ещё не прочитано.
    #[must_use]
    pub const fn rest(&self) -> &'a [u8] {
        self.rest
    }

    /// Метка следующего значения, не двигая курсор.
    #[must_use]
    pub fn peek(&self) -> Option<u8> {
        self.rest.first().copied()
    }

    /// Прочитать следующее значение, каким бы оно ни было.
    pub fn next(&mut self) -> Result<Value<'a>, Error> {
        let (tag, body, rest) = split(self.rest)?;
        self.rest = rest;
        Ok(Value { tag, bytes: body })
    }

    /// Прочитать значение с ожидаемой меткой.
    pub fn expect(&mut self, want: u8) -> Result<&'a [u8], Error> {
        let value = self.next()?;
        if value.tag != want {
            return Err(Error::Unexpected { want, got: value.tag });
        }
        Ok(value.bytes)
    }

    /// Прочитать вложенную последовательность и вернуть курсор по ней.
    pub fn sequence(&mut self) -> Result<Reader<'a>, Error> {
        Ok(Reader::new(self.expect(tag::SEQUENCE)?))
    }

    /// Пропустить значение, если оно есть и метка совпала.
    pub fn skip_if(&mut self, want: u8) -> Result<bool, Error> {
        if self.peek() == Some(want) {
            self.next()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Содержимое необязательного поля `[n] EXPLICIT`, если оно есть.
    pub fn context_if(&mut self, n: u8) -> Result<Option<&'a [u8]>, Error> {
        if self.peek() != Some(tag::context(n)) {
            return Ok(None);
        }
        Ok(Some(self.expect(tag::context(n))?))
    }

    /// Значение вместе с его заголовком — то, по чему считается подпись.
    ///
    /// Подписываются **байты как есть**, включая метку и длину: пересобрать их
    /// из разобранных полей нельзя, потому что чужой кодировщик мог записать
    /// длину иначе, а подпись считалась по его записи.
    pub fn next_raw(&mut self) -> Result<&'a [u8], Error> {
        let before = self.rest;
        let (_, _, rest) = split(self.rest)?;
        self.rest = rest;
        let len = before.len() - rest.len();
        Ok(&before[..len])
    }
}

/// Разрезать одно значение: метка, содержимое, хвост.
fn split(bytes: &[u8]) -> Result<(u8, &[u8], &[u8]), Error> {
    let (&tag, rest) = bytes.split_first().ok_or(Error::Truncated)?;
    let (&first, rest) = rest.split_first().ok_or(Error::Truncated)?;

    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7F);
        if count == 0 || count > core::mem::size_of::<usize>() {
            // Ноль — неопределённая длина: в DER её не бывает, и принимать её
            // значит принимать BER, у которого конец значения ищется поиском.
            return Err(Error::BadLength);
        }
        if rest.len() < count {
            return Err(Error::Truncated);
        }
        let (raw, rest) = rest.split_at(count);
        if raw[0] == 0 {
            // Ведущий ноль в длине запрещён DER: одна и та же длина иначе
            // записывается двумя способами, и подпись по такой записи
            // проверялась бы не над теми байтами.
            return Err(Error::BadLength);
        }
        let mut len = 0usize;
        for byte in raw {
            len = (len << 8) | usize::from(*byte);
        }
        (len, rest)
    };

    if rest.len() < len {
        return Err(Error::Truncated);
    }
    let (body, rest) = rest.split_at(len);
    Ok((tag, body, rest))
}

/// Содержимое BIT STRING без байта «сколько бит не использовано».
///
/// Ключи и подписи в сертификате лежат именно так, и забытый первый байт — это
/// классическая ошибка: подпись «не сходится», а причина в одном байте сдвига.
pub fn bit_string(bytes: &[u8]) -> Result<&[u8], Error> {
    let (&unused, rest) = bytes.split_first().ok_or(Error::Truncated)?;
    if unused != 0 {
        // Ключи и подписи всегда кратны байту.
        return Err(Error::BadLength);
    }
    Ok(rest)
}

/// Содержимое INTEGER без знакового ведущего нуля.
///
/// В DER целое всегда со знаком, поэтому число со старшим взведённым битом
/// записывается с ведущим `0x00`. Модуль RSA в 2048 бит из-за этого занимает 257
/// байт, и оставленный ноль превращает его в число длиннее модуля — то есть в
/// «подпись не сходится».
pub fn unsigned(bytes: &[u8]) -> Result<&[u8], Error> {
    match bytes {
        [] => Err(Error::BadValue),
        // Отрицательных чисел в сертификате не бывает нигде, где мы читаем
        // числа: ни модуль, ни показатель, ни `r`/`s` подписи не бывают меньше
        // нуля. Молча взять их по модулю значило бы принять подделку.
        [first, ..] if *first & 0x80 != 0 => Err(Error::BadValue),
        [0x00] => Ok(bytes),
        // Ведущий ноль допустим ровно один и ровно там, где без него число
        // стало бы отрицательным.
        [0x00, second, ..] if *second & 0x80 != 0 => Ok(&bytes[1..]),
        [0x00, ..] => Err(Error::BadValue),
        _ => Ok(bytes),
    }
}

/// Прочитать `AlgorithmIdentifier` и вернуть OID и параметры.
///
/// ```text
/// AlgorithmIdentifier ::= SEQUENCE { algorithm OBJECT IDENTIFIER,
///                                    parameters ANY DEFINED BY algorithm OPTIONAL }
/// ```
pub fn algorithm(bytes: &[u8]) -> Result<(&[u8], Option<Value<'_>>), Error> {
    let mut reader = Reader::new(bytes);
    let oid = reader.expect(tag::OID)?;
    let parameters = if reader.is_empty() { None } else { Some(reader.next()?) };
    Ok((oid, parameters))
}

#[cfg(test)]
mod tests {
    use super::{Error, Reader, bit_string, tag, unsigned};
    use std::vec;

    /// Короткая длина, длинная длина и вложенность.
    #[test]
    fn lengths_short_and_long() {
        // SEQUENCE { INTEGER 1, OCTET STRING (200 байт) }
        let mut inner = vec![tag::INTEGER, 0x01, 0x01, tag::OCTET_STRING, 0x81, 200];
        inner.extend(core::iter::repeat_n(0xAB, 200));
        let mut bytes = vec![tag::SEQUENCE, 0x81, inner.len() as u8];
        bytes.extend_from_slice(&inner);

        let mut reader = Reader::new(&bytes);
        let mut seq = reader.sequence().expect("последовательность разбирается");
        assert_eq!(seq.expect(tag::INTEGER).unwrap(), &[1]);
        let octets = seq.expect(tag::OCTET_STRING).unwrap();
        assert_eq!(octets.len(), 200);
        assert!(seq.is_empty());
    }

    /// Неопределённая длина — это BER, а не DER, и её надо отвергать.
    #[test]
    fn indefinite_length_is_refused() {
        let bytes = [tag::SEQUENCE, 0x80, 0x00, 0x00];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.sequence().unwrap_err(), Error::BadLength);
    }

    /// Ведущий ноль в длинной длине запрещён: одна длина — одна запись.
    #[test]
    fn a_leading_zero_in_the_length_is_refused() {
        let bytes = [tag::OCTET_STRING, 0x82, 0x00, 0x01, 0xFF];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.next().unwrap_err(), Error::BadLength);
    }

    /// Значение, которое кончилось раньше объявленного.
    #[test]
    fn a_truncated_value_is_refused() {
        let bytes = [tag::OCTET_STRING, 0x05, 1, 2, 3];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.next().unwrap_err(), Error::Truncated);
    }

    /// `next_raw` отдаёт байты вместе с заголовком — то, по чему считается подпись.
    #[test]
    fn raw_keeps_the_header() {
        let bytes = [tag::SEQUENCE, 0x03, tag::INTEGER, 0x01, 0x07];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.next_raw().unwrap(), &bytes[..]);
    }

    /// Первый байт BIT STRING — не данные.
    #[test]
    fn a_bit_string_drops_the_unused_bits_byte() {
        assert_eq!(bit_string(&[0x00, 0xAA, 0xBB]).unwrap(), &[0xAA, 0xBB]);
        assert!(bit_string(&[0x03, 0xAA]).is_err());
        assert!(bit_string(&[]).is_err());
    }

    /// Знаковый ведущий ноль снимается ровно там, где он знаковый.
    #[test]
    fn an_integer_drops_only_the_sign_byte() {
        assert_eq!(unsigned(&[0x00, 0xC0, 0x01]).unwrap(), &[0xC0, 0x01]);
        assert_eq!(unsigned(&[0x01, 0x00, 0x01]).unwrap(), &[0x01, 0x00, 0x01]);
        assert_eq!(unsigned(&[0x00]).unwrap(), &[0x00]);
        // Лишний ноль — другая запись того же числа, и в DER её не бывает.
        assert_eq!(unsigned(&[0x00, 0x01]).unwrap_err(), Error::BadValue);
        // Отрицательное там, где числа не бывают отрицательными.
        assert_eq!(unsigned(&[0xFF, 0x01]).unwrap_err(), Error::BadValue);
        assert_eq!(unsigned(&[]).unwrap_err(), Error::BadValue);
    }

    /// Контекстная метка в двух формах — это две разные метки.
    #[test]
    fn context_tags_are_two_different_tags() {
        assert_eq!(tag::context(3), 0xA3);
        assert_eq!(tag::context_primitive(2), 0x82);
    }
}
