//! Перевод логического адреса в физический.
//!
//! # Зачем в btrfs два адресных пространства
//!
//! Всё, кроме суперблока, адресуется логически. Логическое пространство
//! разбито на **куски** (chunk), и каждый кусок сказано, где лежит физически —
//! на каком устройстве и с какого смещения. Ради этого и заводится отдельное
//! пространство: том может состоять из нескольких дисков, кусок может лежать в
//! двух копиях (`dup`) или полосами (RAID), и логический адрес от всего этого
//! не зависит.
//!
//! Нам достаточно одного устройства и двух простейших размещений, но **перевод
//! обязателен всё равно**: первый кусок начинается не с нуля, и код, забывший
//! перевести адрес, читает не мусор, а соседние структуры — то есть ошибается
//! правдоподобно.
//!
//! # Откуда берётся карта
//!
//! Дважды:
//!
//! 1. из массива `sys_chunk_array` в суперблоке — там ровно те куски, без
//!    которых не прочитать сам корень дерева кусков;
//! 2. из дерева кусков целиком, когда оно стало читаемо.
//!
//! Второй проход добавляет к первому, а не заменяет его: куски из суперблока
//! есть и в дереве, и запись обязана совпасть. Расхождение — признак того, что
//! суперблок и дерево из разных поколений, и это `Corrupt`, а не повод верить
//! кому-то одному.

use alloc::vec::Vec;

use crate::layout::*;
use crate::{Error, Result};

/// Один кусок: отрезок логического пространства, лежащий подряд на диске.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub logical: u64,
    pub length: u64,
    pub physical: u64,
}

/// Карта кусков, отсортированная по логическому адресу.
///
/// `Vec` с двоичным поиском, а не дерево: кусков на нашем томе десятки, и
/// заводить под них структуру со своим временем жизни — это код, который
/// нечем проверить, ради поиска, которого не видно в профиле.
#[derive(Debug, Default)]
pub(crate) struct ChunkMap {
    entries: Vec<Chunk>,
}

impl ChunkMap {
    pub(crate) const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Добавить кусок, разобрав его описание.
    ///
    /// `raw` — описание куска (`btrfs_chunk`) без ключа.
    pub(crate) fn add(&mut self, logical: u64, raw: &[u8]) -> Result<()> {
        if raw.len() < CHUNK_HEAD_SIZE {
            return Err(Error::Corrupt);
        }
        let length = u64_at(raw, CHUNK_LENGTH);
        let kind = u64_at(raw, CHUNK_TYPE);
        let stripes = u16_at(raw, CHUNK_NUM_STRIPES) as usize;

        if length == 0 || stripes == 0 {
            return Err(Error::Corrupt);
        }
        if raw.len() < CHUNK_HEAD_SIZE + stripes * STRIPE_SIZE {
            return Err(Error::Corrupt);
        }

        // Профиль решает, где лежит байт с данным логическим адресом. `single`
        // — подряд с начала первой полосы, `dup` — то же самое, просто копий
        // две. Любой RAID раскладывает адрес по полосам, и брать первую полосу
        // там значило бы читать каждый второй килобайт не оттуда.
        let profile = kind & BLOCK_GROUP_PROFILE_MASK;
        if profile != 0 && profile != BLOCK_GROUP_DUP {
            return Err(Error::Unsupported);
        }

        // Устройство одно. Проверяется явно: том из двух дисков, у которого мы
        // видим только первый, читался бы наполовину — и молча.
        let devid = u64_at(raw, CHUNK_HEAD_SIZE + STRIPE_DEVID);
        if devid != 1 {
            return Err(Error::Unsupported);
        }
        let physical = u64_at(raw, CHUNK_HEAD_SIZE + STRIPE_OFFSET);

        let chunk = Chunk { logical, length, physical };
        match self.entries.binary_search_by_key(&logical, |entry| entry.logical) {
            // Тот же кусок из суперблока и из дерева — не ошибка, если он
            // описан одинаково.
            Ok(at) => {
                if self.entries[at] != chunk {
                    return Err(Error::Corrupt);
                }
            }
            Err(at) => {
                self.entries.try_reserve(1).map_err(|_| Error::NoMemory)?;
                self.entries.insert(at, chunk);
            }
        }
        Ok(())
    }

    /// Где лежит логический адрес и сколько байт после него идут подряд.
    ///
    /// Второе значение существует, чтобы чтение большого экстента не
    /// разбивалось на сектора: длина отрезка ограничена концом куска, а не
    /// нашей фантазией.
    pub(crate) fn translate(&self, logical: u64) -> Result<(u64, u64)> {
        // `partition_point` даёт первый кусок, начинающийся ПОСЛЕ адреса;
        // нужный — предыдущий.
        let at = self.entries.partition_point(|entry| entry.logical <= logical);
        if at == 0 {
            return Err(Error::Corrupt);
        }
        let chunk = self.entries[at - 1];
        let within = logical - chunk.logical;
        if within >= chunk.length {
            // Адрес попал в дыру между кусками. Это не «не нашли», это
            // противоречие внутри тома: дерево ссылается туда, где по его же
            // карте ничего не размещено.
            return Err(Error::Corrupt);
        }
        Ok((chunk.physical + within, chunk.length - within))
    }

    /// Разобрать массив кусков из суперблока.
    ///
    /// Формат — пары «ключ, описание» подряд, без выравнивания и без счётчика:
    /// длина в байтах лежит в суперблоке отдельным полем, и конец массива
    /// определяется только ею.
    pub(crate) fn load_system_array(&mut self, sb: &[u8]) -> Result<()> {
        let size = u32_at(sb, SB_SYS_ARRAY_SIZE) as usize;
        if size > SB_SYS_CHUNK_ARRAY_SIZE {
            return Err(Error::Corrupt);
        }
        let array = &sb[SB_SYS_CHUNK_ARRAY..SB_SYS_CHUNK_ARRAY + size];

        let mut at = 0usize;
        while at < size {
            if at + KEY_SIZE + CHUNK_HEAD_SIZE > size {
                return Err(Error::Corrupt);
            }
            let key = Key::parse(array, at);
            if key.kind != item_type::CHUNK_ITEM {
                return Err(Error::Corrupt);
            }
            let body = &array[at + KEY_SIZE..];
            let stripes = u16_at(body, CHUNK_NUM_STRIPES) as usize;
            let len = CHUNK_HEAD_SIZE + stripes * STRIPE_SIZE;
            if at + KEY_SIZE + len > size {
                return Err(Error::Corrupt);
            }
            self.add(key.offset, &body[..len])?;
            at += KEY_SIZE + len;
        }

        if self.entries.is_empty() {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
}
