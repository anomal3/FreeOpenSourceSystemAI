//! Байты на проводе: сборка сообщений с длинами и чтение их обратно.
//!
//! # Почему длины пишутся задним числом
//!
//! Потому что в TLS каждое второе поле — это «длина, а за ней столько-то
//! байт», и длина известна только после того, как содержимое написано.
//! Считать её заранее означало бы считать её дважды и однажды разойтись.
//! [`Writer::open`] оставляет место, [`Writer::close`] вписывает в него
//! настоящее число.

/// Не поместилось: у сборки нет кучи, буфер конечен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

/// Сборщик сообщения.
pub struct Writer<'a> {
    buffer: &'a mut [u8],
    at: usize,
}

/// Место, оставленное под длину.
#[derive(Debug, Clone, Copy)]
pub struct Hole {
    at: usize,
    width: usize,
}

impl<'a> Writer<'a> {
    #[must_use]
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, at: 0 }
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.at
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.at == 0
    }

    pub fn u8(&mut self, value: u8) -> Result<(), Full> {
        self.bytes(&[value])
    }

    pub fn u16(&mut self, value: u16) -> Result<(), Full> {
        self.bytes(&value.to_be_bytes())
    }

    pub fn bytes(&mut self, value: &[u8]) -> Result<(), Full> {
        if self.at + value.len() > self.buffer.len() {
            return Err(Full);
        }
        self.buffer[self.at..self.at + value.len()].copy_from_slice(value);
        self.at += value.len();
        Ok(())
    }

    /// Оставить место под длину шириной в `width` байт.
    pub fn open(&mut self, width: usize) -> Result<Hole, Full> {
        if self.at + width > self.buffer.len() {
            return Err(Full);
        }
        let hole = Hole { at: self.at, width };
        self.at += width;
        Ok(hole)
    }

    /// Вписать в оставленное место длину того, что написано после него.
    pub fn close(&mut self, hole: Hole) {
        let length = self.at - hole.at - hole.width;
        for index in 0..hole.width {
            let shift = 8 * (hole.width - 1 - index);
            self.buffer[hole.at + index] = (length >> shift) as u8;
        }
    }

    /// Написанное целиком.
    #[must_use]
    pub fn finish(self) -> &'a [u8] {
        &self.buffer[..self.at]
    }
}

/// Чтение того же самого.
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

    #[must_use]
    pub const fn rest(&self) -> &'a [u8] {
        self.rest
    }

    pub fn u8(&mut self) -> Option<u8> {
        let (&first, rest) = self.rest.split_first()?;
        self.rest = rest;
        Some(first)
    }

    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes([self.u8()?, self.u8()?]))
    }

    pub fn u24(&mut self) -> Option<usize> {
        let (a, b, c) = (self.u8()?, self.u8()?, self.u8()?);
        Some((usize::from(a) << 16) | (usize::from(b) << 8) | usize::from(c))
    }

    pub fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.rest.len() < len {
            return None;
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Some(head)
    }

    /// Вектор с однобайтовой длиной.
    pub fn vector8(&mut self) -> Option<&'a [u8]> {
        let len = usize::from(self.u8()?);
        self.take(len)
    }

    /// Вектор с двухбайтовой длиной.
    pub fn vector16(&mut self) -> Option<&'a [u8]> {
        let len = usize::from(self.u16()?);
        self.take(len)
    }

    /// Вектор с трёхбайтовой длиной.
    pub fn vector24(&mut self) -> Option<&'a [u8]> {
        let len = self.u24()?;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, Writer};

    /// Длина вписывается после содержимого и совпадает с ним.
    #[test]
    fn a_hole_is_filled_with_the_real_length() {
        let mut buffer = [0u8; 32];
        let mut writer = Writer::new(&mut buffer);
        writer.u8(0x16).unwrap();
        let hole = writer.open(3).unwrap();
        writer.bytes(b"hello").unwrap();
        writer.close(hole);
        let bytes = writer.finish();
        assert_eq!(bytes, &[0x16, 0x00, 0x00, 0x05, b'h', b'e', b'l', b'l', b'o']);

        let mut reader = Reader::new(&bytes[1..]);
        assert_eq!(reader.vector24().unwrap(), b"hello");
        assert!(reader.is_empty());
    }

    /// Переполнение — это отказ, а не обрезанное сообщение.
    #[test]
    fn a_full_buffer_is_an_error() {
        let mut buffer = [0u8; 4];
        let mut writer = Writer::new(&mut buffer);
        assert!(writer.bytes(b"abcd").is_ok());
        assert!(writer.u8(1).is_err());
    }

    /// Чтение за концом возвращает `None`, а не панику.
    #[test]
    fn reading_past_the_end_says_so() {
        let mut reader = Reader::new(&[0x00, 0x05, 0x01]);
        assert!(reader.vector16().is_none());
    }
}
