//! Чтение тома ext2.
//!
//! Эта половина крейта нужна ядру: установщик только пишет, а система потом
//! только читает. Реализации при этом общие ровно в том, что важно, — в
//! [`crate::layout`], где живёт разбор геометрии и смещения полей. Разойтись
//! они не могут.
//!
//! # Ничего не кэшируется
//!
//! Каждое обращение читает блок с носителя заново. Кэш здесь был бы
//! преждевременным: сегодняшний потребитель — оболочка, выводящая содержимое
//! файла по команде человека, а страничный кэш — это отдельная подсистема со
//! своей политикой вытеснения, и делать её мимоходом внутри драйвера ФС
//! неправильно.

use alloc::string::String;
use alloc::vec::Vec;

use disk::BlockDevice;

use crate::layout::*;
use crate::write::{try_vec, try_zeroed};
use crate::{Error, Result};

/// Тип узла файловой системы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    /// Всё прочее: символические ссылки, устройства, сокеты. Читать их нечем,
    /// но скрывать их существование при перечислении каталога — значит врать
    /// о содержимом.
    Other,
}

/// Inode в разобранном виде.
#[derive(Debug, Clone)]
pub struct Inode {
    pub number: u32,
    pub kind: FileType,
    /// Права в unix-нотации: `rwxrwxrwx` в младших девяти битах.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub links: u16,
    /// Время последнего изменения, секунды эпохи Unix.
    pub mtime: u32,
    blocks: [u32; BLOCK_POINTERS],
}

/// Запись каталога.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub inode: u32,
    pub kind: FileType,
}

/// Смонтированный на чтение том ext2.
pub struct Ext2 {
    geometry: Geometry,
    /// Начала таблиц inode по группам — единственное, что читается один раз и
    /// запоминается. Это не кэш данных: таблица дескрипторов групп неизменна,
    /// а без неё нельзя добраться ни до одного inode, то есть её пришлось бы
    /// перечитывать при каждом обращении к любому файлу.
    inode_tables: Vec<u32>,
}


/// Прочитать суперблок тома, начинающегося с сектора `first_lba`.
///
/// Суперблок ext2 лежит по **байтовому** смещению 1024 от начала тома, и это
/// смещение не зависит ни от размера блока, ни от размера сектора. На носителе
/// с сектором 512 оно равно ровно двум секторам, и до Phase 26c формула
/// `first_lba + 1024 / 512` была верна по совпадению. На 4Kn-диске то же
/// смещение попадает **внутрь первого сектора**, и та же формула прочитала бы
/// восьмой сектор — то есть чужие данные с уверенным видом.
fn read_superblock(
    dev: &mut dyn BlockDevice,
    first_lba: u64,
) -> Result<[u8; SUPERBLOCK_SIZE]> {
    let sector = dev.sector_size() as u64;
    if sector == 0 {
        return Err(Error::Unsupported);
    }
    let lba = first_lba + SUPERBLOCK_OFFSET / sector;
    let within = (SUPERBLOCK_OFFSET % sector) as usize;
    // Сколько секторов накрывает суперблок вместе со смещением внутри первого.
    let span = (within + SUPERBLOCK_SIZE).div_ceil(sector as usize);

    let mut raw = crate::write::try_zeroed(span * sector as usize)?;
    dev.read(lba, &mut raw)?;

    let mut sb = [0u8; SUPERBLOCK_SIZE];
    sb.copy_from_slice(&raw[within..within + SUPERBLOCK_SIZE]);
    Ok(sb)
}

impl Ext2 {
    /// Смонтировать том, начинающийся с сектора `first_lba`.
    pub fn mount(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<Self> {
        let sb = read_superblock(dev, first_lba)?;

        let geometry = Geometry::from_superblock(first_lba, &sb, dev.sector_size())?;
        // Состояние тома читается, но монтирование не запрещается: «грязный»
        // том после сбоя всё ещё читается, а отказ смонтировать корень
        // означал бы систему, которая не загружается из-за отключения питания.
        let state = u16_at(&sb, 58);

        let block_bytes = geometry.block_size.bytes() as usize;
        let mut inode_tables = try_vec(geometry.groups as usize, 0u32)?;
        let table_start = geometry.group_first_block(0) + 1;
        let mut buf = try_zeroed(block_bytes)?;
        for group in 0..geometry.groups {
            let at = group as usize * GROUP_DESC_SIZE;
            let block = table_start + (at / block_bytes) as u32;
            dev.read(geometry.block_lba(block), &mut buf)?;
            let within = at % block_bytes;
            inode_tables[group as usize] = u32_at(&buf, within + 8);
            // Таблица inode обязана лежать внутри своей группы: чужой номер
            // здесь увёл бы чтение в произвольное место тома.
            let first = geometry.group_first_block(group);
            let table = inode_tables[group as usize];
            if table < first || table >= first + geometry.blocks_in_group(group) {
                return Err(Error::Corrupt);
            }
        }

        let fs = Self { geometry, inode_tables };
        // Проверяем, а не верим: корневой inode обязан быть каталогом, и это
        // самая дешёвая проверка того, что мы разобрали геометрию верно.
        let root = fs.inode(dev, ROOT_INODE)?;
        if root.kind != FileType::Directory {
            return Err(Error::Corrupt);
        }
        let _ = state;
        Ok(fs)
    }

    #[must_use]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// «Чистый» ли том по данным суперблока.
    pub fn is_clean(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<bool> {
        let sb = read_superblock(dev, first_lba)?;
        if u16_at(&sb, 56) != MAGIC {
            return Err(Error::Corrupt);
        }
        Ok(u16_at(&sb, 58) == 1)
    }

    /// Когда том записывали в последний раз, по данным суперблока.
    ///
    /// Нужно редактору: часов у крейта нет, а новые файлы обязаны получить
    /// какое-то время. Время последней записи — единственное осмысленное
    /// значение, которое можно взять, ничего не спрашивая у платформы.
    pub fn write_time(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<u32> {
        let sb = read_superblock(dev, first_lba)?;
        if u16_at(&sb, 56) != MAGIC {
            return Err(Error::Corrupt);
        }
        Ok(u32_at(&sb, 48))
    }

    /// Метка тома.
    pub fn label(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<String> {
        let sb = read_superblock(dev, first_lba)?;
        if u16_at(&sb, 56) != MAGIC {
            return Err(Error::Corrupt);
        }
        let raw = &sb[120..136];
        let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
        Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
    }

    /// Прочитать inode по номеру.
    pub fn inode(&self, dev: &mut dyn BlockDevice, number: u32) -> Result<Inode> {
        let (group, index) = self.geometry.locate_inode(number)?;
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let byte_offset = index as usize * INODE_SIZE;
        let block = self.inode_tables[group as usize] + (byte_offset / block_bytes) as u32;

        let mut buf = try_zeroed(block_bytes)?;
        dev.read(self.geometry.block_lba(block), &mut buf)?;
        let raw = &buf[byte_offset % block_bytes..][..INODE_SIZE];

        let mode = u16_at(raw, 0);
        let kind = match mode & 0xF000 {
            MODE_DIRECTORY => FileType::Directory,
            MODE_REGULAR => FileType::Regular,
            _ => FileType::Other,
        };
        let low = u64::from(u32_at(raw, 4));
        // Старшие 32 бита размера лежат в поле, которое у каталога означает
        // совсем другое, — поэтому склеиваем только для обычных файлов.
        let size = if kind == FileType::Regular {
            low | (u64::from(u32_at(raw, 108)) << 32)
        } else {
            low
        };

        let mut blocks = [0u32; BLOCK_POINTERS];
        for (index, block) in blocks.iter_mut().enumerate() {
            *block = u32_at(raw, 40 + index * 4);
        }

        Ok(Inode {
            number,
            kind,
            mode: mode & 0o7777,
            uid: u32::from(u16_at(raw, 2)) | (u32::from(u16_at(raw, 120)) << 16),
            gid: u32::from(u16_at(raw, 24)) | (u32::from(u16_at(raw, 122)) << 16),
            size,
            links: u16_at(raw, 26),
            mtime: u32_at(raw, 16),
            blocks,
        })
    }

    /// Корневой каталог.
    pub fn root(&self, dev: &mut dyn BlockDevice) -> Result<Inode> {
        self.inode(dev, ROOT_INODE)
    }

    /// Найти узел по абсолютному пути.
    pub fn resolve(&self, dev: &mut dyn BlockDevice, path: &str) -> Result<Inode> {
        let mut node = self.root(dev)?;
        for component in path.split('/').filter(|part| !part.is_empty() && *part != ".") {
            if node.kind != FileType::Directory {
                return Err(Error::NotADirectory);
            }
            let entry = self
                .lookup(dev, &node, component)?
                .ok_or(Error::NotFound)?;
            node = self.inode(dev, entry.inode)?;
        }
        Ok(node)
    }

    /// Найти запись по имени в каталоге.
    pub fn lookup(
        &self,
        dev: &mut dyn BlockDevice,
        dir: &Inode,
        name: &str,
    ) -> Result<Option<DirEntry>> {
        for entry in self.list(dev, dir)? {
            if entry.name == name {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Перечислить содержимое каталога.
    ///
    /// Записи «.» и «..» пропускаются: они есть в каждом каталоге, ничего не
    /// сообщают и только мешают тому, кто выводит список.
    pub fn list(&self, dev: &mut dyn BlockDevice, dir: &Inode) -> Result<Vec<DirEntry>> {
        if dir.kind != FileType::Directory {
            return Err(Error::NotADirectory);
        }
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let blocks = (dir.size as usize).div_ceil(block_bytes);
        let mut out = Vec::new();

        for index in 0..blocks {
            let Some(block) = self.block_of(dev, dir, index as u32)? else {
                continue;
            };
            let mut buf = try_zeroed(block_bytes)?;
            dev.read(self.geometry.block_lba(block), &mut buf)?;

            let mut at = 0usize;
            while at + 8 <= buf.len() {
                let inode = u32_at(&buf, at);
                let rec_len = u16_at(&buf, at + 4) as usize;
                // Нулевая или выходящая за блок длина — это зацикливание или
                // чтение за границей; данные пришли с носителя, доверять им
                // нельзя.
                if rec_len < 8 || at + rec_len > buf.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = buf[at + 6] as usize;
                if inode != 0 && at + 8 + name_len <= buf.len() {
                    let name = &buf[at + 8..at + 8 + name_len];
                    if name != b"." && name != b".." {
                        let kind = match buf[at + 7] {
                            DIR_TYPE_DIRECTORY => FileType::Directory,
                            DIR_TYPE_REGULAR => FileType::Regular,
                            _ => FileType::Other,
                        };
                        out.try_reserve(1).map_err(|_| Error::NoMemory)?;
                        out.push(DirEntry {
                            name: String::from_utf8_lossy(name).into_owned(),
                            inode,
                            kind,
                        });
                    }
                }
                at += rec_len;
            }
        }
        Ok(out)
    }

    /// Прочитать до `buf.len()` байт файла, начиная со смещения.
    ///
    /// Возвращает, сколько прочитано: у конца файла это меньше запрошенного, и
    /// это не ошибка.
    pub fn read_at(
        &self,
        dev: &mut dyn BlockDevice,
        inode: &Inode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize> {
        if inode.kind != FileType::Regular {
            return Err(Error::NotADirectory);
        }
        if offset >= inode.size {
            return Ok(0);
        }
        let block_bytes = self.geometry.block_size.bytes() as u64;
        let want = buf.len().min((inode.size - offset) as usize);

        let mut done = 0usize;
        let mut block_buf = try_zeroed(block_bytes as usize)?;
        while done < want {
            let position = offset + done as u64;
            let index = (position / block_bytes) as u32;
            let within = (position % block_bytes) as usize;
            let take = (block_bytes as usize - within).min(want - done);

            match self.block_of(dev, inode, index)? {
                Some(block) => {
                    dev.read(self.geometry.block_lba(block), &mut block_buf)?;
                    buf[done..done + take].copy_from_slice(&block_buf[within..within + take]);
                }
                // Дыра в файле: не выделенный блок читается нулями. Файлов с
                // дырами наш писатель не создаёт, но формат их допускает, и
                // ошибка здесь была бы неверна.
                None => buf[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(done)
    }

    /// Прочитать файл целиком.
    pub fn read_file(&self, dev: &mut dyn BlockDevice, inode: &Inode) -> Result<Vec<u8>> {
        let len = usize::try_from(inode.size).map_err(|_| Error::NoMemory)?;
        let mut out = try_zeroed(len)?;
        let read = self.read_at(dev, inode, 0, &mut out)?;
        out.truncate(read);
        Ok(out)
    }

    /// Номер `index`-го блока файла, с разбором косвенности.
    ///
    /// `None` означает дыру — блок, который не выделен.
    fn block_of(
        &self,
        dev: &mut dyn BlockDevice,
        inode: &Inode,
        index: u32,
    ) -> Result<Option<u32>> {
        let pointers = self.geometry.block_size.pointers_per_block();
        let index = index as usize;
        let pointers = pointers as usize;

        let direct_limit = DIRECT_BLOCKS;
        let single_limit = direct_limit + pointers;
        let double_limit = single_limit + pointers * pointers;

        let block = if index < direct_limit {
            inode.blocks[index]
        } else if index < single_limit {
            let table = inode.blocks[INDIRECT_INDEX];
            self.pointer_at(dev, table, index - direct_limit)?
        } else if index < double_limit {
            let offset = index - single_limit;
            let table = inode.blocks[DOUBLE_INDIRECT_INDEX];
            let leaf = self.pointer_at(dev, table, offset / pointers)?;
            self.pointer_at(dev, leaf, offset % pointers)?
        } else {
            // Тройная косвенность нужна файлам больше четырёх гигабайт при
            // блоке 4 КиБ. Такого потребителя нет, а ветка, которую нечем
            // проверить, в разборе указателей опаснее честного отказа.
            return Err(Error::Unsupported);
        };

        Ok(if block == 0 { None } else { Some(block) })
    }

    /// Указатель под номером `slot` в блоке косвенности.
    fn pointer_at(&self, dev: &mut dyn BlockDevice, table: u32, slot: usize) -> Result<u32> {
        if table == 0 {
            return Ok(0);
        }
        if table >= self.geometry.blocks {
            return Err(Error::Corrupt);
        }
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let mut buf = try_zeroed(block_bytes)?;
        dev.read(self.geometry.block_lba(table), &mut buf)?;
        Ok(u32_at(&buf, slot * 4))
    }
}
