//! Хранилище корневых сертификатов: файл, который человек может прочитать.
//!
//! # Почему файл, а не константы в коде
//!
//! По той же причине, по которой ключи обновления лежат в `/os-keys`, а не в
//! ядре (фаза 39): набор корней меняется чаще, чем система. Он едет в образе
//! (`/usr/share/defaults/etc/ca.pem`), заменяется вместе с ним, и машина,
//! которой нужен свой корень, кладёт файл в `/etc` — обновление его не тронет.
//!
//! # Почему PEM
//!
//! Потому что это единственный формат, который человек умеет открыть тем, что у
//! него уже есть: `openssl x509 -in ca.pem -text`. Вопрос «а кому доверяет эта
//! машина» обязан иметь ответ, который можно получить не нашими же руками.
//!
//! Base64 стоит одного прохода по файлу и буфера под результат — цена, которую
//! стоит платить за читаемость.

use crate::cert::Certificate;

/// Сколько корней помещается в хранилище.
///
/// Шестнадцать при трёх-четырёх нужных: запас на смену корня, когда какое-то
/// время живут и старый, и новый. Предел существует потому, что список лежит в
/// массиве, — расти без предела значило бы отдать размер списка тому, кто
/// прислал файл.
pub const MAX_ROOTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// В буфере не хватило места под расшифрованные сертификаты.
    NoRoom,
    /// В файле больше корней, чем помещается в список.
    TooMany,
    /// Блок `-----BEGIN CERTIFICATE-----` не закрыт.
    Unterminated,
    /// Base64 в блоке не разбирается.
    BadBase64,
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NoRoom => "the root store is larger than the buffer set aside for it",
            Self::TooMany => "the root store lists more roots than this system will hold",
            Self::Unterminated => "a BEGIN CERTIFICATE block with no END",
            Self::BadBase64 => "a certificate block that is not base64",
        }
    }
}

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// Корни, которым доверяет эта система.
#[derive(Debug, Clone, Copy)]
pub struct Store<'a> {
    entries: [&'a [u8]; MAX_ROOTS],
    len: usize,
}

impl Default for Store<'_> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<'a> Store<'a> {
    /// Пустое хранилище: не доверяет ничему.
    ///
    /// Это и есть безопасное умолчание. «Корней нет, значит проверять нечего,
    /// значит верим» — ровно та ошибка, ради которой хранилище и заводится.
    #[must_use]
    pub const fn empty() -> Self {
        Self { entries: [&[]; MAX_ROOTS], len: 0 }
    }

    /// Разобрать PEM, сложив DER в `out`.
    ///
    /// `out` принадлежит вызывающему и обязан пережить хранилище: срезы
    /// указывают в него. Своего буфера у крейта нет — кучи нет ни у программы,
    /// ни у ядра.
    pub fn parse_pem(text: &str, out: &'a mut [u8]) -> Result<Self, Error> {
        let mut spans = [(0usize, 0usize); MAX_ROOTS];
        let mut count = 0usize;
        let mut filled = 0usize;

        let mut rest = text;
        while let Some(start) = rest.find(BEGIN) {
            let body = &rest[start + BEGIN.len()..];
            let Some(end) = body.find(END) else {
                return Err(Error::Unterminated);
            };
            if count == MAX_ROOTS {
                return Err(Error::TooMany);
            }
            let written = base64(&body[..end], &mut out[filled..])?;
            spans[count] = (filled, written);
            filled += written;
            count += 1;
            rest = &body[end + END.len()..];
        }

        // Мутабельная ссылка отпускается здесь: дальше буфер только читается, и
        // срезы в нём живут столько же, сколько он сам.
        let out: &'a [u8] = out;
        let mut store = Self::empty();
        for (index, (start, len)) in spans[..count].iter().enumerate() {
            store.entries[index] = &out[*start..*start + *len];
        }
        store.len = count;
        Ok(store)
    }

    /// Сколько корней в хранилище.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Сертификаты корней в том порядке, в котором они записаны в файле.
    #[must_use]
    pub fn certificates(&self) -> &[&'a [u8]] {
        &self.entries[..self.len]
    }

    /// Корни, чей subject совпадает с этим издателем.
    ///
    /// Сравнение байтовое, по DER. RFC 5280 описывает правила сравнения имён
    /// куда подробнее (регистр, пробелы, кодировка строк), но удостоверяющие
    /// центры записывают своё имя одними и теми же байтами в обоих местах —
    /// иначе цепочка не сошлась бы ни у кого. Байтовое сравнение поэтому строже
    /// правил и не пропускает того, что правила запрещают.
    pub fn find(&self, issuer: &'a [u8]) -> impl Iterator<Item = Certificate<'a>> + '_ {
        self.certificates().iter().filter_map(move |bytes| {
            let certificate = Certificate::parse(bytes).ok()?;
            (certificate.subject == issuer).then_some(certificate)
        })
    }
}

/// Расшифровать base64, пропуская переводы строк и пробелы.
fn base64(text: &str, out: &mut [u8]) -> Result<usize, Error> {
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    let mut written = 0usize;
    let mut padding = 0usize;

    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(Error::BadBase64),
        };
        if padding != 0 {
            // Данные после «=» — это не base64, а два блока, склеенных без
            // разделителя.
            return Err(Error::BadBase64);
        }
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if written == out.len() {
                return Err(Error::NoRoom);
            }
            out[written] = (accumulator >> bits) as u8;
            written += 1;
        }
    }
    // Остаток обязан быть нулями: иначе последний символ нёс биты, которым
    // некуда деться, — то есть длина не та.
    if accumulator & ((1 << bits) - 1) != 0 {
        return Err(Error::BadBase64);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::{Error, Store, base64};

    #[test]
    fn base64_decodes_with_whitespace_and_padding() {
        let mut out = [0u8; 16];
        let len = base64("aGVs\nbG8g\r\nd29ybGQ=", &mut out).expect("разбирается");
        assert_eq!(&out[..len], b"hello world");
    }

    #[test]
    fn base64_refuses_what_is_not_base64() {
        let mut out = [0u8; 16];
        assert_eq!(base64("aGV*", &mut out).unwrap_err(), Error::BadBase64);
        // Данные после дополнения.
        assert_eq!(base64("aGVs=bG8=", &mut out).unwrap_err(), Error::BadBase64);
    }

    #[test]
    fn a_buffer_that_is_too_small_is_an_error_not_a_truncation() {
        let mut out = [0u8; 4];
        assert_eq!(base64("aGVsbG8gd29ybGQ=", &mut out).unwrap_err(), Error::NoRoom);
    }

    #[test]
    fn an_unterminated_block_is_refused() {
        let mut out = [0u8; 64];
        let text = "-----BEGIN CERTIFICATE-----\nAAAA\n";
        assert_eq!(Store::parse_pem(text, &mut out).unwrap_err(), Error::Unterminated);
    }

    #[test]
    fn an_empty_store_trusts_nothing() {
        let mut out = [0u8; 8];
        let store = Store::parse_pem("нет здесь ничего", &mut out).expect("пустой файл — не ошибка");
        assert!(store.is_empty());
    }
}
