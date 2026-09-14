//! Четыре кучи метаданных: имена, строки программы, двоичные данные, GUID.

use crate::Error;
use crate::bytes::compressed_u32;

/// `#Strings`: имена типов и членов, UTF-8 с завершающим нулём.
#[derive(Clone, Copy)]
pub struct Strings<'a>(pub(crate) &'a [u8]);

impl<'a> Strings<'a> {
    /// Строка по смещению. Ноль — пустая строка, так договорено форматом.
    pub fn get(&self, index: u32) -> Result<&'a str, Error> {
        if index == 0 {
            return Ok("");
        }
        let tail = self.0.get(index as usize..).ok_or(Error::BadIndex("#Strings"))?;
        let end = tail.iter().position(|&b| b == 0).ok_or(Error::BadHeap("#Strings"))?;
        core::str::from_utf8(&tail[..end]).map_err(|_| Error::BadHeap("#Strings utf-8"))
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        self.0.len()
    }
}

/// `#Blob`: сигнатуры, значения атрибутов, открытые ключи — с длиной впереди.
#[derive(Clone, Copy)]
pub struct Blobs<'a>(pub(crate) &'a [u8]);

impl<'a> Blobs<'a> {
    pub fn get(&self, index: u32) -> Result<&'a [u8], Error> {
        if index == 0 {
            return Ok(&[]);
        }
        let tail = self.0.get(index as usize..).ok_or(Error::BadIndex("#Blob"))?;
        let (len, used) = compressed_u32(tail).ok_or(Error::BadHeap("#Blob length"))?;
        tail.get(used..used + len as usize).ok_or(Error::BadHeap("#Blob"))
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        self.0.len()
    }
}

/// `#US`: строковые литералы программы (`ldstr`), UTF-16LE.
#[derive(Clone, Copy)]
pub struct UserStrings<'a>(pub(crate) &'a [u8]);

/// Строка из `#US`: единицы UTF-16, как их записал компилятор.
///
/// Отдаётся единицами, а не `char`: строка .NET — последовательность единиц
/// UTF-16, и одиночная половинка суррогатной пары в ней законна. Перевод в
/// `char` потерял бы её, а программа вправе на неё рассчитывать.
#[derive(Clone, Copy)]
pub struct UserString<'a> {
    bytes: &'a [u8],
}

impl<'a> UserString<'a> {
    /// Сколько единиц UTF-16.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.bytes.len() / 2
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.bytes.len() < 2
    }

    pub fn units(&self) -> impl Iterator<Item = u16> + 'a {
        self.bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
    }
}

impl<'a> UserStrings<'a> {
    pub fn get(&self, index: u32) -> Result<UserString<'a>, Error> {
        let tail = self.0.get(index as usize..).ok_or(Error::BadIndex("#US"))?;
        let (len, used) = compressed_u32(tail).ok_or(Error::BadHeap("#US length"))?;
        let entry = tail.get(used..used + len as usize).ok_or(Error::BadHeap("#US"))?;
        // Последний байт записи — не символ, а признак «есть ли в строке знаки,
        // требующие особой обработки при сравнении». Длина его считает, строка —
        // нет.
        let bytes = &entry[..entry.len() & !1];
        Ok(UserString { bytes })
    }

    /// Все строки кучи со смещениями, начиная с первой (смещение 0 — пустая
    /// запись, так договорено форматом).
    pub fn iter(&self) -> impl Iterator<Item = (u32, UserString<'a>)> + 'a {
        let data = self.0;
        let mut at = 1usize;
        core::iter::from_fn(move || {
            let tail = data.get(at..).filter(|tail| !tail.is_empty())?;
            let (len, used) = compressed_u32(tail)?;
            let entry = tail.get(used..used + len as usize)?;
            let offset = at as u32;
            at += used + len as usize;
            Some((offset, UserString { bytes: &entry[..entry.len() & !1] }))
        })
    }

    #[must_use]
    pub const fn size(&self) -> usize {
        self.0.len()
    }
}

/// `#GUID`: по 16 байт, нумерация с единицы.
#[derive(Clone, Copy)]
pub struct Guids<'a>(pub(crate) &'a [u8]);

impl Guids<'_> {
    /// GUID по номеру. Ноль — «нет GUID».
    pub fn get(&self, index: u32) -> Result<Option<[u8; 16]>, Error> {
        if index == 0 {
            return Ok(None);
        }
        let start = (index as usize - 1) * 16;
        let bytes = self.0.get(start..start + 16).ok_or(Error::BadIndex("#GUID"))?;
        let mut guid = [0u8; 16];
        guid.copy_from_slice(bytes);
        Ok(Some(guid))
    }
}
