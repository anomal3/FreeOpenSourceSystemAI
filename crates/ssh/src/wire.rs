//! Как SSH записывает данные на провод.
//!
//! Пять типов на весь протокол, и все они описаны в RFC 4251 §5: байт,
//! 32-битное число (старшим байтом вперёд), строка (длина плюс байты), список
//! имён (та же строка с запятыми внутри) и `mpint` — целое произвольной длины.
//!
//! # Про `mpint` и лишний нулевой байт
//!
//! `mpint` знаковое, и старший бит старшего байта — это знак. Число, у
//! которого он оказался единицей, приходится записывать с ведущим нулём, иначе
//! получатель прочтёт его как отрицательное. Общий секрет обмена ключами —
//! ровно такое число в половине случаев, и пропуск этого нуля даёт стек,
//! который соединяется через раз. Найти такую ошибку тяжело именно потому, что
//! она случайная: подпись не сходится только у половины ключей.

/// Буфер, в который пишут поля протокола.
///
/// Не `Vec`: кучи у программы нет. Место кончилось — запись помечается
/// отказавшей, и наружу это выходит одной проверкой в конце, а не проверкой
/// после каждого поля.
pub struct Writer<'a> {
    buffer: &'a mut [u8],
    at: usize,
    overflow: bool,
}

impl<'a> Writer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, at: 0, overflow: false }
    }

    pub fn len(&self) -> usize {
        self.at
    }

    pub fn is_empty(&self) -> bool {
        self.at == 0
    }

    /// Всё ли поместилось.
    pub fn ok(&self) -> bool {
        !self.overflow
    }

    pub fn bytes(&mut self, data: &[u8]) -> &mut Self {
        if self.at + data.len() > self.buffer.len() {
            self.overflow = true;
            return self;
        }
        self.buffer[self.at..self.at + data.len()].copy_from_slice(data);
        self.at += data.len();
        self
    }

    pub fn byte(&mut self, value: u8) -> &mut Self {
        self.bytes(&[value])
    }

    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    /// Строка: длина и байты.
    pub fn string(&mut self, data: &[u8]) -> &mut Self {
        self.u32(data.len() as u32);
        self.bytes(data)
    }

    /// Целое произвольной длины — см. заголовок модуля про ведущий ноль.
    pub fn mpint(&mut self, value: &[u8]) -> &mut Self {
        // Ведущие нули самого числа не значат ничего и на провод не едут.
        let start = value.iter().position(|byte| *byte != 0).unwrap_or(value.len());
        let trimmed = &value[start..];
        if trimmed.is_empty() {
            return self.u32(0);
        }
        if trimmed[0] & 0x80 != 0 {
            self.u32(trimmed.len() as u32 + 1);
            self.byte(0);
            self.bytes(trimmed)
        } else {
            self.string(trimmed)
        }
    }

    /// Готовый срез — то, что записано.
    pub fn finish(self) -> Option<&'a [u8]> {
        if self.overflow {
            return None;
        }
        Some(&self.buffer[..self.at])
    }
}

/// Разбор того же самого.
pub struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.at)
    }

    pub fn byte(&mut self) -> Option<u8> {
        let value = *self.data.get(self.at)?;
        self.at += 1;
        Some(value)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let slice = self.data.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    /// Строка. Длина приходит из-за границы доверия, поэтому проверяется
    /// против того, сколько байт вообще осталось.
    pub fn string(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        let slice = self.data.get(self.at..self.at + len)?;
        self.at += len;
        Some(slice)
    }

    pub fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.at..self.at + len)?;
        self.at += len;
        Some(slice)
    }

    /// Пропустить строку, не читая её.
    pub fn skip_string(&mut self) -> Option<()> {
        self.string().map(|_| ())
    }
}

/// Есть ли имя в списке, разделённом запятыми.
///
/// Так согласуются алгоритмы: клиент присылает свои по убыванию
/// предпочтения, и выбрать надо первый из его списка, который есть у нас.
/// Обратный порядок (первый наш, который есть у него) — тоже рабочее правило,
/// но RFC 4253 велит именно так, и расхождение здесь означало бы соединение,
/// которое устанавливается не тем алгоритмом, каким думает клиент.
pub fn list_contains(list: &[u8], name: &str) -> bool {
    list.split(|byte| *byte == b',').any(|item| item == name.as_bytes())
}
