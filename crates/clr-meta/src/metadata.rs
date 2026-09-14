//! Корень метаданных: сигнатура `BSJB`, строка версии и заголовки потоков.

use crate::Error;
use crate::bytes::{u16_at, u32_at};
use crate::heaps::{Blobs, Guids, Strings, UserStrings};

/// Сигнатура корня метаданных — `BSJB` в little-endian.
const SIGNATURE: u32 = 0x424A_5342;
/// Самое длинное имя потока, которое разбор принимает.
///
/// В ECMA-335 имени отведено 32 байта вместе с нулём; без предела разбор
/// повреждённого заголовка читал бы имя до конца файла.
const STREAM_NAME_MAX: usize = 32;

/// Корень метаданных и найденные в нём потоки.
#[derive(Clone, Copy)]
pub struct Root<'a> {
    /// Строка версии: у сборок .NET это всегда `v4.0.30319`, даже у .NET 10, —
    /// номер формата, а не среды.
    pub version: &'a str,
    /// Поток таблиц (`#~` или `#-`).
    pub tables: &'a [u8],
    /// Где поток таблиц начинается от начала метаданных.
    pub tables_offset: usize,
    /// Таблицы в несжатом виде (`#-`): бывают у сборок после «правки и
    /// продолжения» и могут содержать таблицы-указатели.
    pub uncompressed: bool,
    pub strings: Strings<'a>,
    pub user_strings: UserStrings<'a>,
    pub blobs: Blobs<'a>,
    pub guids: Guids<'a>,
}

impl<'a> Root<'a> {
    pub fn parse(metadata: &'a [u8]) -> Result<Self, Error> {
        if u32_at(metadata, 0) != Some(SIGNATURE) {
            return Err(Error::BadMagic("BSJB"));
        }
        let version_len =
            u32_at(metadata, 12).ok_or(Error::Truncated("metadata root"))? as usize;
        let version_bytes = metadata
            .get(16..16usize.checked_add(version_len).ok_or(Error::Truncated("metadata root"))?)
            .ok_or(Error::Truncated("metadata version"))?;
        // Строка дополнена нулями до кратного четырём, и длина считает их тоже.
        let version_end = version_bytes.iter().position(|&b| b == 0).unwrap_or(version_bytes.len());
        let version = core::str::from_utf8(&version_bytes[..version_end])
            .map_err(|_| Error::BadHeap("metadata version"))?;

        let mut at = 16 + version_len;
        let stream_count =
            u16_at(metadata, at + 2).ok_or(Error::Truncated("metadata root"))?;
        at += 4;

        let mut tables = None;
        let mut uncompressed = false;
        let mut strings: &[u8] = &[];
        let mut user_strings: &[u8] = &[];
        let mut blobs: &[u8] = &[];
        let mut guids: &[u8] = &[];

        for _ in 0..stream_count {
            let offset = u32_at(metadata, at).ok_or(Error::Truncated("stream header"))? as usize;
            let size = u32_at(metadata, at + 4).ok_or(Error::Truncated("stream header"))? as usize;
            let name_area = metadata
                .get(at + 8..(at + 8 + STREAM_NAME_MAX).min(metadata.len()))
                .ok_or(Error::Truncated("stream name"))?;
            let name_len = name_area
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::BadHeap("stream name"))?;
            let name = &name_area[..name_len];
            // Имя вместе с нулём выровнено до четырёх байт.
            at += 8 + (name_len + 1).div_ceil(4) * 4;

            let data = metadata
                .get(offset..offset.checked_add(size).ok_or(Error::Truncated("stream"))?)
                .ok_or(Error::Truncated("stream"))?;
            match name {
                b"#~" => tables = Some((data, offset)),
                b"#-" => {
                    tables = Some((data, offset));
                    uncompressed = true;
                }
                b"#Strings" => strings = data,
                b"#US" => user_strings = data,
                b"#Blob" => blobs = data,
                b"#GUID" => guids = data,
                // `#Pdb`, `#JTD` и прочее, чего среда выполнения не читает.
                _ => {}
            }
        }

        let (tables, tables_offset) = tables.ok_or(Error::BadHeap("#~ stream missing"))?;
        Ok(Self {
            version,
            tables,
            tables_offset,
            uncompressed,
            strings: Strings(strings),
            user_strings: UserStrings(user_strings),
            blobs: Blobs(blobs),
            guids: Guids(guids),
        })
    }
}
