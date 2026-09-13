//! Чтение тома btrfs: монтирование, каталоги, файлы.
//!
//! # Почему методы чтения берут `&mut self`
//!
//! В `ext2` они берут `&self`, и это не небрежность одного из двух крейтов, а
//! разница форматов. Там указатели на блоки файла лежат в inode — он уже на
//! руках у вызывающего, и прочитать 512 байт стоит одного обращения к диску.
//! Здесь их нет нигде: каждое чтение — это спуск по дереву, и ещё один по
//! дереву сумм. Без памяти между вызовами полукилобайт обходился бы в четыре
//! узла по 16 КиБ.
//!
//! Поэтому том держит небольшой кэш узлов (`crate::tree`), а значит меняется
//! при чтении — отсюда и `&mut`. Страничного кэша это не заменяет и не
//! притворяется им: кэшируются **метаданные дерева**, данные файлов идут мимо.
//!
//! # Что значит «сверка каждого блока»
//!
//! Метаданные проверяются всегда: сумма лежит в самом узле, и [`crate::tree`]
//! сверяет её при каждом чтении. С данными сложнее — их суммы лежат в отдельном
//! дереве, по четыре байта на сектор, и чтобы сверить сектор, надо сначала
//! найти его сумму.
//!
//! Здесь это делается, и это главное, ради чего btrfs вообще появился в
//! проекте. Цена честная: чтение файла ходит в два дерева вместо одного.
//! Смягчает её то, что сумма ищется не для каждого сектора заново — обход по
//! дереву сумм идёт вперёд вместе с чтением, и на подряд идущем файле это один
//! спуск на несколько мегабайт.
//!
//! Сектор, для которого суммы не нашлось, — это **ошибка**, а не повод его
//! пропустить. Исключений ровно два, и оба записаны в самом томе:
//! предвыделенный экстент (данных ещё нет) и файл с признаком `NODATASUM`.

use alloc::string::String;
use alloc::vec::Vec;

use disk::BlockDevice;

use crate::chunk::ChunkMap;
use crate::crc32c;
use crate::layout::*;
use crate::tree::{Cursor, Volume};
use crate::{Error, Result, try_zeroed};

/// Тип узла файловой системы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    /// Всё прочее: символические ссылки, устройства, сокеты. Читать их нечем,
    /// но скрывать их существование при перечислении каталога — значит врать о
    /// содержимом.
    Other,
}

/// Inode в разобранном виде.
#[derive(Debug, Clone)]
pub struct Inode {
    pub number: u64,
    pub kind: FileType,
    /// Права в unix-нотации: `rwxrwxrwx` в младших девяти битах.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub links: u32,
    /// Время последнего изменения, секунды эпохи Unix.
    pub mtime: i64,
    /// Флаги inode. Наружу не выставляются: наружу от них нужно ровно одно —
    /// надо ли сверять суммы, и это решает сам крейт.
    flags: u64,
}

/// Запись каталога.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub inode: u64,
    pub kind: FileType,
}

/// Смонтированный на чтение том btrfs.
pub struct Btrfs {
    volume: Volume,
    /// Корень дерева файловой системы и его высота.
    fs_tree: (u64, u8),
    /// Корень дерева контрольных сумм.
    csum_tree: (u64, u8),
    label: String,
    generation: u64,
    total_bytes: u64,
    bytes_used: u64,
    /// Был ли том отмонтирован чисто.
    ///
    /// Снято в момент монтирования: непустое дерево журнала означает, что
    /// систему уронили с незаписанной транзакцией. Читать такой том можно —
    /// суперблок указывает на последнее **завершённое** состояние, — но
    /// последних записей в нём не будет, и молчать об этом нельзя.
    was_clean: bool,
}

impl Btrfs {
    /// Смонтировать том, начинающийся с сектора `first_lba`.
    pub fn mount(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<Self> {
        let sb = read_superblock(dev, first_lba)?;

        let sectorsize = u32_at(&sb, SB_SECTORSIZE);
        let nodesize = u32_at(&sb, SB_NODESIZE);
        // Оба размера — степени двойки, и на этом стоит вся арифметика ниже.
        // Ноль или нечётное число здесь означают не «странный том», а деление
        // на ноль в десяти местах.
        if !sectorsize.is_power_of_two()
            || !nodesize.is_power_of_two()
            || sectorsize < 512
            || nodesize < sectorsize
            || nodesize > 64 * 1024
        {
            return Err(Error::Corrupt);
        }
        if u16_at(&sb, SB_CSUM_TYPE) != CSUM_TYPE_CRC32C {
            // xxhash, sha256 и blake2 — законные варианты формата, которых
            // здесь нет. Читать том, не умея его сумму, значит объявить
            // проверку выполненной, ничего не проверив.
            return Err(Error::Unsupported);
        }
        let incompat = u64_at(&sb, SB_INCOMPAT_FLAGS);
        if incompat & !INCOMPAT_SUPPORTED != 0 {
            return Err(Error::Unsupported);
        }
        if u64_at(&sb, SB_NUM_DEVICES) != 1 {
            return Err(Error::Unsupported);
        }

        let mut fsid = [0u8; 16];
        fsid.copy_from_slice(&sb[SB_FSID..SB_FSID + 16]);

        let mut volume = Volume::new(
            ChunkMap::new(),
            nodesize,
            sectorsize,
            fsid,
            u64_at(&sb, SB_GENERATION),
            first_lba,
        );
        // Курица и яйцо: дерево кусков само лежит по логическому адресу.
        // Разрывается массивом в суперблоке — в нём ровно те куски, которых
        // хватает, чтобы добраться до корня дерева.
        volume.chunks.load_system_array(&sb)?;

        let chunk_root = u64_at(&sb, SB_CHUNK_ROOT);
        let chunk_level = sb[SB_CHUNK_ROOT_LEVEL];
        load_chunk_tree(&mut volume, dev, chunk_root, chunk_level)?;

        let root_tree = (u64_at(&sb, SB_ROOT), sb[SB_ROOT_LEVEL]);
        let fs_tree = find_root(&mut volume, dev, root_tree, objectid::FS_TREE)?;
        let csum_tree = find_root(&mut volume, dev, root_tree, objectid::CSUM_TREE)?;

        let raw_label = &sb[SB_LABEL..SB_LABEL + SB_LABEL_SIZE];
        let end = raw_label.iter().position(|&byte| byte == 0).unwrap_or(raw_label.len());
        let mut label = String::new();
        label
            .try_reserve_exact(end)
            .map_err(|_| Error::NoMemory)?;
        label.push_str(&String::from_utf8_lossy(&raw_label[..end]));

        let mut fs = Self {
            volume,
            fs_tree,
            csum_tree,
            label,
            generation: u64_at(&sb, SB_GENERATION),
            total_bytes: u64_at(&sb, SB_TOTAL_BYTES),
            bytes_used: u64_at(&sb, SB_BYTES_USED),
            was_clean: u64_at(&sb, SB_LOG_ROOT) == 0,
        };

        // Проверяем, а не верим: корневой каталог обязан быть каталогом. Это
        // самая дешёвая проверка того, что разобраны и куски, и деревья.
        if fs.root(dev)?.kind != FileType::Directory {
            return Err(Error::Corrupt);
        }
        Ok(fs)
    }

    /// Метка тома.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Номер последней завершённой транзакции.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Размер тома и сколько в нём занято, в байтах.
    #[must_use]
    pub const fn usage(&self) -> (u64, u64) {
        (self.total_bytes, self.bytes_used)
    }

    /// Размер сектора данных: единица, на которой считается контрольная сумма.
    #[must_use]
    pub const fn sector_size(&self) -> u32 {
        self.volume.sectorsize
    }

    /// Размер узла дерева.
    #[must_use]
    pub const fn node_size(&self) -> u32 {
        self.volume.nodesize
    }

    /// Сколько кусков описывает карту тома.
    #[must_use]
    pub fn chunks(&self) -> usize {
        self.volume.chunks.len()
    }

    /// Закрыли ли том в прошлый раз чисто.
    #[must_use]
    pub const fn was_clean(&self) -> bool {
        self.was_clean
    }

    /// Корень дерева файловой системы и его высота — для проверки тома.
    pub(crate) const fn fs_root(&self) -> (u64, u8) {
        self.fs_tree
    }

    /// Геометрия тома — для проверки тома, которая ходит по дереву сама.
    pub(crate) const fn volume_mut(&mut self) -> &mut Volume {
        &mut self.volume
    }

    /// Корневой каталог.
    pub fn root(&mut self, dev: &mut dyn BlockDevice) -> Result<Inode> {
        self.inode(dev, objectid::FIRST_FREE)
    }

    /// Прочитать inode по номеру.
    pub fn inode(&mut self, dev: &mut dyn BlockDevice, number: u64) -> Result<Inode> {
        let key = Key::new(number, item_type::INODE_ITEM, 0);
        let (root, level) = self.fs_tree;
        let cursor = Cursor::seek(&mut self.volume, dev, root, level, key)?;
        if cursor.key() != Some(key) {
            return Err(Error::NotFound);
        }
        let raw = cursor.item()?;
        if raw.len() < INODE_ITEM_SIZE {
            return Err(Error::Corrupt);
        }

        let mode = u32_at(raw, INODE_MODE);
        let kind = match mode & MODE_FORMAT_MASK {
            MODE_DIRECTORY => FileType::Directory,
            MODE_REGULAR => FileType::Regular,
            _ => FileType::Other,
        };
        Ok(Inode {
            number,
            kind,
            mode: (mode & 0o7777) as u16,
            uid: u32_at(raw, INODE_UID),
            gid: u32_at(raw, INODE_GID),
            size: u64_at(raw, INODE_SIZE_FIELD),
            links: u32_at(raw, INODE_NLINK),
            mtime: u64_at(raw, INODE_MTIME) as i64,
            flags: u64_at(raw, INODE_FLAGS),
        })
    }

    /// Найти узел по абсолютному пути.
    pub fn resolve(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<Inode> {
        let mut node = self.root(dev)?;
        for component in path.split('/').filter(|part| !part.is_empty() && *part != ".") {
            if node.kind != FileType::Directory {
                return Err(Error::NotADirectory);
            }
            let entry = self.lookup(dev, &node, component)?.ok_or(Error::NotFound)?;
            node = self.inode(dev, entry.inode)?;
        }
        Ok(node)
    }

    /// Найти запись по имени в каталоге.
    ///
    /// Имя ищется не перебором, а по ключу: в нём лежит хеш имени, и один спуск
    /// по дереву приводит прямо к записи. Перебор пришлось бы делать и на
    /// тысяче файлов в каталоге, а это ровно тот случай, где ext2 и становится
    /// медленным.
    pub fn lookup(
        &mut self,
        dev: &mut dyn BlockDevice,
        dir: &Inode,
        name: &str,
    ) -> Result<Option<DirEntry>> {
        if dir.kind != FileType::Directory {
            return Err(Error::NotADirectory);
        }
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(Error::BadName);
        }
        let key = Key::new(
            dir.number,
            item_type::DIR_ITEM,
            crc32c::name_hash(name.as_bytes()),
        );
        let (root, level) = self.fs_tree;
        let cursor = Cursor::seek(&mut self.volume, dev, root, level, key)?;
        if cursor.key() != Some(key) {
            return Ok(None);
        }

        // Совпадение хешей — обычное дело, и тогда в одном элементе лежат
        // несколько записей подряд. Взять первую значило бы иногда открывать не
        // тот файл, причём воспроизводилось бы это на одном имени из миллиарда.
        let mut raw = cursor.item()?;
        while !raw.is_empty() {
            let (entry, used) = parse_dir_item(raw)?;
            if entry.name == name {
                return Ok(Some(entry));
            }
            raw = &raw[used..];
        }
        Ok(None)
    }

    /// Перечислить содержимое каталога.
    ///
    /// Идём по `DIR_INDEX`, а не по `DIR_ITEM`: второй упорядочен хешем имени,
    /// то есть случайно, а первый — порядком создания. Для человека, который
    /// смотрит на список, это разница между «файлы вперемешку» и «файлы так,
    /// как их клали».
    pub fn list(&mut self, dev: &mut dyn BlockDevice, dir: &Inode) -> Result<Vec<DirEntry>> {
        if dir.kind != FileType::Directory {
            return Err(Error::NotADirectory);
        }
        let start = Key::first(dir.number, item_type::DIR_INDEX);
        let (root, level) = self.fs_tree;
        let mut cursor = Cursor::seek(&mut self.volume, dev, root, level, start)?;

        let mut out = Vec::new();
        while let Some(key) = cursor.key() {
            if key.objectid != dir.number || key.kind != item_type::DIR_INDEX {
                break;
            }
            let (entry, _) = parse_dir_item(cursor.item()?)?;
            out.try_reserve(1).map_err(|_| Error::NoMemory)?;
            out.push(entry);
            cursor.next(&mut self.volume, dev)?;
        }
        Ok(out)
    }

    /// Прочитать файл целиком.
    pub fn read_file(&mut self, dev: &mut dyn BlockDevice, inode: &Inode) -> Result<Vec<u8>> {
        let len = usize::try_from(inode.size).map_err(|_| Error::NoMemory)?;
        let mut out = try_zeroed(len)?;
        let read = self.read_at(dev, inode, 0, &mut out)?;
        out.truncate(read);
        Ok(out)
    }

    /// Прочитать до `buf.len()` байт файла, начиная со смещения.
    ///
    /// Возвращает, сколько прочитано: у конца файла это меньше запрошенного, и
    /// это не ошибка.
    pub fn read_at(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &Inode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize> {
        match inode.kind {
            FileType::Regular => {}
            FileType::Directory => return Err(Error::IsADirectory),
            FileType::Other => return Err(Error::Unsupported),
        }
        if offset >= inode.size {
            return Ok(0);
        }
        let want = buf.len().min((inode.size - offset) as usize);
        if want == 0 {
            return Ok(0);
        }

        let start = Key::new(inode.number, item_type::EXTENT_DATA, offset);
        let (root, level) = self.fs_tree;
        let mut cursor = Cursor::seek(&mut self.volume, dev, root, level, start)?;
        // Поиск даёт первый ключ **не меньше** искомого, а нужен последний не
        // больший: экстент, покрывающий смещение, начинается раньше него.
        if cursor.key() != Some(start) {
            cursor.prev(&mut self.volume, dev)?;
        }

        // Обход по дереву сумм идёт вперёд вместе с чтением. Заводится он
        // лениво: у файла из одного встроенного экстента сумм нет вовсе, и
        // спуск по дереву был бы чистой тратой.
        let mut csums: Option<Cursor> = None;

        let mut done = 0usize;
        while done < want {
            let position = offset + done as u64;

            // Где кончается пустота перед следующим экстентом — или весь файл,
            // если экстентов больше нет.
            let mut gap_end = inode.size;
            let mut covered = false;

            if let Some(key) = cursor.key() {
                if key.objectid == inode.number && key.kind == item_type::EXTENT_DATA {
                    let raw = cursor.item()?;
                    let span = extent_span(raw)?;
                    let end = key.offset.saturating_add(span);
                    if key.offset <= position && position < end {
                        covered = true;
                    } else if key.offset > position {
                        gap_end = key.offset;
                    } else {
                        // Экстент целиком позади: шагаем вперёд и пробуем
                        // снова, не тронув ни байта буфера.
                        cursor.next(&mut self.volume, dev)?;
                        continue;
                    }
                }
            }

            if !covered {
                // Дыра. При `NO_HOLES` её ничто не описывает — просто нет
                // экстента, — и принять это за конец файла значит обрезать
                // разрежённый файл на первой же дыре.
                let take = ((gap_end - position) as usize).min(want - done);
                if take == 0 {
                    return Err(Error::Corrupt);
                }
                buf[done..done + take].fill(0);
                done += take;
                continue;
            }

            let key = cursor.key().ok_or(Error::Corrupt)?;
            let raw = cursor.item()?;
            let within = position - key.offset;
            let taken = self.read_extent(
                dev,
                inode,
                raw,
                within,
                &mut buf[done..want],
                &mut csums,
            )?;
            if taken == 0 {
                return Err(Error::Corrupt);
            }
            done += taken;
        }
        Ok(done)
    }

    /// Прочитать кусок одного экстента, начиная со смещения `within` внутри него.
    fn read_extent(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &Inode,
        raw: &[u8],
        within: u64,
        buf: &mut [u8],
        csums: &mut Option<Cursor>,
    ) -> Result<usize> {
        if raw.len() <= EXTENT_TYPE {
            return Err(Error::Corrupt);
        }
        if raw[EXTENT_COMPRESSION] != 0 || raw[EXTENT_ENCRYPTION] != 0 {
            // Сжатый экстент — это законный том, который мы не умеем читать.
            // Отдать его как есть значит вернуть человеку мусор под видом
            // содержимого файла.
            return Err(Error::Unsupported);
        }
        let kind = raw[EXTENT_TYPE];

        if kind == EXTENT_TYPE_INLINE {
            let ram = u64_at(raw, EXTENT_RAM_BYTES);
            let data = &raw[EXTENT_INLINE_DATA..];
            if (data.len() as u64) < ram || within >= ram {
                return Err(Error::Corrupt);
            }
            // Встроенные данные лежат внутри самого узла дерева, а его сумма
            // уже сверена при чтении. Отдельной суммы у них нет и быть не может.
            let take = ((ram - within) as usize).min(buf.len());
            buf[..take].copy_from_slice(&data[within as usize..within as usize + take]);
            return Ok(take);
        }
        if kind != EXTENT_TYPE_REGULAR && kind != EXTENT_TYPE_PREALLOC {
            return Err(Error::Corrupt);
        }
        if raw.len() < EXTENT_REGULAR_SIZE {
            return Err(Error::Corrupt);
        }

        let disk_bytenr = u64_at(raw, EXTENT_DISK_BYTENR);
        let extent_offset = u64_at(raw, EXTENT_OFFSET);
        let num_bytes = u64_at(raw, EXTENT_NUM_BYTES);
        if within >= num_bytes {
            return Err(Error::Corrupt);
        }
        let take = ((num_bytes - within) as usize).min(buf.len());

        if disk_bytenr == 0 {
            // Явно записанная дыра: формат до `NO_HOLES` описывал её именно так,
            // и такие экстенты остаются на томах, созданных старым mkfs.
            buf[..take].fill(0);
            return Ok(take);
        }

        let sector = u64::from(self.volume.sectorsize);
        let logical = disk_bytenr + extent_offset + within;
        // Не сверять суммы можно ровно в двух случаях, и оба записаны в томе:
        // предвыделенный экстент (данных в нём ещё нет) и файл, помеченный
        // `NODATASUM`. Всё остальное обязано сойтись.
        let verify =
            kind == EXTENT_TYPE_REGULAR && inode.flags & INODE_FLAG_NODATASUM == 0;

        // Ровное чтение идёт прямо в буфер вызывающего и сразу целым числом
        // секторов — только так сумму можно посчитать по месту, не копируя
        // мегабайты ради проверки.
        if logical % sector == 0 && take as u64 >= sector {
            let run = self.volume.contiguous(logical)?;
            let mut bytes = (take as u64 / sector) * sector;
            bytes = bytes.min(run - run % sector);
            if bytes > 0 {
                let bytes = bytes as usize;
                self.volume.read_logical(dev, logical, &mut buf[..bytes])?;
                if verify {
                    self.check_data(dev, logical, &buf[..bytes], csums)?;
                }
                return Ok(bytes);
            }
        }

        // Хвост, не выровненный по сектору: сектор читается целиком во
        // временный буфер, сверяется и обрезается.
        let aligned = logical - logical % sector;
        let mut whole = try_zeroed(sector as usize)?;
        self.volume.read_logical(dev, aligned, &mut whole)?;
        if verify {
            self.check_data(dev, aligned, &whole, csums)?;
        }
        let skip = (logical - aligned) as usize;
        let take = take.min(sector as usize - skip);
        buf[..take].copy_from_slice(&whole[skip..skip + take]);
        Ok(take)
    }

    /// Прочитать диапазон данных и сверить его контрольные суммы.
    ///
    /// Нужен проверке тома, и подпись у него другая не случайно: проверке не
    /// нужны сами байты — ей нужен ответ «сошлось или нет». Поэтому буфер
    /// приходит снаружи и переиспользуется, а результат чтения никуда не
    /// уезжает.
    ///
    /// `logical` выровнен по сектору, `bytes` кратен ему.
    pub(crate) fn verify_range(
        &mut self,
        dev: &mut dyn BlockDevice,
        logical: u64,
        bytes: u64,
        scratch: &mut [u8],
        csums: &mut Option<Cursor>,
    ) -> Result<()> {
        let sector = u64::from(self.volume.sectorsize);
        let step = (scratch.len() as u64 / sector) * sector;
        if step == 0 {
            return Err(Error::NoMemory);
        }
        let mut done = 0u64;
        while done < bytes {
            let at = logical + done;
            // Отрезок не должен пересекать границу куска: за ней физический
            // адрес считается по другой записи карты.
            let run = self.volume.contiguous(at)?;
            let take = step.min(bytes - done).min(run - run % sector);
            if take == 0 {
                return Err(Error::Corrupt);
            }
            let take = take as usize;
            self.volume.read_logical(dev, at, &mut scratch[..take])?;
            self.check_data(dev, at, &scratch[..take], csums)?;
            done += take as u64;
        }
        Ok(())
    }

    /// Сверить контрольные суммы прочитанных секторов данных.
    ///
    /// `logical` выровнен по сектору, длина данных кратна сектору.
    fn check_data(
        &mut self,
        dev: &mut dyn BlockDevice,
        logical: u64,
        data: &[u8],
        csums: &mut Option<Cursor>,
    ) -> Result<()> {
        let sector = self.volume.sectorsize as usize;
        for (index, block) in data.chunks_exact(sector).enumerate() {
            let at = logical + (index * sector) as u64;
            let stored = self.csum_of(dev, at, csums)?;
            if crc32c::checksum(block) != stored {
                return Err(Error::BadChecksum);
            }
        }
        Ok(())
    }

    /// Записанная в томе сумма сектора с логическим адресом `at`.
    ///
    /// Обход по дереву сумм переиспользуется между вызовами и двигается только
    /// вперёд: у файла, читаемого подряд, это один спуск на несколько
    /// мегабайт вместо спуска на каждый сектор.
    fn csum_of(
        &mut self,
        dev: &mut dyn BlockDevice,
        at: u64,
        csums: &mut Option<Cursor>,
    ) -> Result<u32> {
        let sector = u64::from(self.volume.sectorsize);
        let key = Key::new(objectid::EXTENT_CSUM, item_type::EXTENT_CSUM, at);

        // Заводим обход или возвращаем его назад, если чтение пошло не вперёд.
        let stale = match csums.as_ref().and_then(Cursor::key) {
            Some(current) => current.offset > at,
            None => csums.is_some(),
        };
        if csums.is_none() || stale {
            let (root, level) = self.csum_tree;
            let mut cursor = Cursor::seek(&mut self.volume, dev, root, level, key)?;
            if cursor.key() != Some(key) {
                cursor.prev(&mut self.volume, dev)?;
            }
            *csums = Some(cursor);
        }
        let cursor = csums.as_mut().ok_or(Error::Corrupt)?;

        loop {
            let Some(current) = cursor.key() else {
                return Err(Error::Corrupt);
            };
            if current.objectid != objectid::EXTENT_CSUM
                || current.kind != item_type::EXTENT_CSUM
            {
                return Err(Error::Corrupt);
            }
            let raw = cursor.item()?;
            let covered = (raw.len() / 4) as u64 * sector;
            if at >= current.offset && at < current.offset + covered {
                let index = ((at - current.offset) / sector) as usize;
                return Ok(u32_at(raw, index * 4));
            }
            if current.offset > at {
                // Суммы для этого сектора в томе нет. Это не «нечего
                // проверять»: том обещал сумму каждому сектору данных, и её
                // отсутствие — такая же порча, как несовпадение.
                return Err(Error::Corrupt);
            }
            cursor.next(&mut self.volume, dev)?;
        }
    }
}

/// Похож ли том на btrfs.
///
/// Существует ради выбора файловой системы при монтировании: тип раздела в GPT
/// говорит, **зачем** раздел, а не **чем** он отформатирован, и на одном и том
/// же типе за время переезда будут встречаться оба формата.
///
/// Проверяется только подпись и собственный адрес суперблока — то есть ровно
/// столько, сколько нужно, чтобы решить, кому отдать том. Полный разбор делает
/// [`Btrfs::mount`], и его ошибки означают уже не «это не btrfs», а «это
/// испорченный btrfs», что для человека совсем другая новость.
pub fn detect(dev: &mut dyn BlockDevice, first_lba: u64) -> bool {
    let sector = u64::from(dev.sector_size());
    if sector == 0 {
        return false;
    }
    let base = match first_lba
        .checked_mul(sector)
        .and_then(|start| start.checked_add(SUPERBLOCK_OFFSET))
    {
        Some(base) => base,
        None => return false,
    };
    let within = (base % sector) as usize;
    let span = (within + SUPERBLOCK_SIZE).div_ceil(sector as usize);
    let Ok(mut raw) = try_zeroed(span * sector as usize) else {
        return false;
    };
    if dev.read(base / sector, &mut raw).is_err() {
        return false;
    }
    let sb = &raw[within..within + SUPERBLOCK_SIZE];
    u64_at(sb, SB_MAGIC) == MAGIC && u64_at(sb, SB_BYTENR) == SUPERBLOCK_OFFSET
}

/// Сколько байт файла покрывает экстент.
fn extent_span(raw: &[u8]) -> Result<u64> {
    if raw.len() <= EXTENT_TYPE {
        return Err(Error::Corrupt);
    }
    if raw[EXTENT_TYPE] == EXTENT_TYPE_INLINE {
        Ok(u64_at(raw, EXTENT_RAM_BYTES))
    } else if raw.len() >= EXTENT_REGULAR_SIZE {
        Ok(u64_at(raw, EXTENT_NUM_BYTES))
    } else {
        Err(Error::Corrupt)
    }
}

/// Разобрать одну запись каталога; возвращает её и свою длину в байтах.
fn parse_dir_item(raw: &[u8]) -> Result<(DirEntry, usize)> {
    if raw.len() < DIR_ITEM_HEAD_SIZE {
        return Err(Error::Corrupt);
    }
    let location = Key::parse(raw, DIR_ITEM_LOCATION);
    let data_len = u16_at(raw, DIR_ITEM_DATA_LEN) as usize;
    let name_len = u16_at(raw, DIR_ITEM_NAME_LEN) as usize;
    let total = DIR_ITEM_HEAD_SIZE + name_len + data_len;
    if name_len == 0 || name_len > 255 || total > raw.len() {
        return Err(Error::Corrupt);
    }
    // Запись, указывающая на подтом, ссылается не на inode, а на корень
    // другого дерева. Подтомов у нас нет, и показать такую запись как обычный
    // каталог значило бы завести путь, по которому ничего не прочитать.
    if location.kind != item_type::INODE_ITEM {
        return Err(Error::Unsupported);
    }

    let name = &raw[DIR_ITEM_HEAD_SIZE..DIR_ITEM_HEAD_SIZE + name_len];
    let mut owned = String::new();
    owned.try_reserve_exact(name_len).map_err(|_| Error::NoMemory)?;
    owned.push_str(&String::from_utf8_lossy(name));

    // Тип в записи каталога — подсказка, чтобы не читать inode ради одной
    // буквы в списке. Единственное место, где на него нельзя опираться, —
    // решение «можно ли сюда войти»: там всё равно читается inode.
    let kind = match raw[DIR_ITEM_HEAD_SIZE - 1] {
        1 => FileType::Regular,
        2 => FileType::Directory,
        _ => FileType::Other,
    };

    Ok((DirEntry { name: owned, inode: location.objectid, kind }, total))
}

/// Прочитать суперблок тома, начинающегося с сектора `first_lba`.
///
/// Копий суперблока на томе до четырёх, и читаются они по очереди, пока
/// какая-нибудь не сойдётся. Выбора «самой свежей по поколению», как делает
/// ядро Linux, здесь нет намеренно: мы пока только читаем, а значит все копии
/// написаны одним `mkfs` или одной чужой системой и различаться не могут.
/// Фаза записи обязана вернуться сюда — с этого момента копии начнут расходиться
/// при обрыве питания.
fn read_superblock(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<Vec<u8>> {
    let sector = u64::from(dev.sector_size());
    if sector == 0 {
        return Err(Error::Unsupported);
    }
    let device_bytes = dev.sector_count().saturating_mul(sector);
    let volume_start = first_lba.saturating_mul(sector);

    let mut last = Error::TooSmall;
    for offset in SUPERBLOCK_COPIES {
        if volume_start + offset + SUPERBLOCK_SIZE as u64 > device_bytes {
            break;
        }
        let base = volume_start + offset;
        let lba = base / sector;
        let within = (base % sector) as usize;
        let span = (within + SUPERBLOCK_SIZE).div_ceil(sector as usize);

        let mut raw = try_zeroed(span * sector as usize)?;
        match dev.read(lba, &mut raw) {
            Ok(()) => {}
            Err(err) => {
                last = err.into();
                continue;
            }
        }
        let sb = &raw[within..within + SUPERBLOCK_SIZE];

        if u64_at(sb, SB_MAGIC) != MAGIC {
            last = Error::Corrupt;
            continue;
        }
        // Суперблок знает собственное место. Совпадение доказывает, что это
        // копия именно этого тома, а не чужого раздела, оказавшегося рядом.
        if u64_at(sb, SB_BYTENR) != offset {
            last = Error::Corrupt;
            continue;
        }
        if verify_block(sb).is_err() {
            last = Error::BadChecksum;
            continue;
        }
        let mut out = Vec::new();
        out.try_reserve_exact(SUPERBLOCK_SIZE).map_err(|_| Error::NoMemory)?;
        out.extend_from_slice(sb);
        return Ok(out);
    }
    Err(last)
}

/// Дочитать карту кусков из дерева кусков.
fn load_chunk_tree(
    volume: &mut Volume,
    dev: &mut dyn BlockDevice,
    root: u64,
    level: u8,
) -> Result<()> {
    let start = Key::first(objectid::FIRST_CHUNK_TREE, item_type::CHUNK_ITEM);
    // Обход заводится на карте из суперблока, и этого хватает: корень дерева
    // кусков лежит внутри системного куска, а системные куски в массиве есть
    // все — иначе том не смонтировала бы и Linux.
    let mut cursor = Cursor::seek(volume, dev, root, level, start)?;

    // Куски накапливаются отдельно, а не кладутся в карту по ходу обхода:
    // менять карту, по которой прямо сейчас читается дерево, — верный способ
    // получить обход, зависящий от порядка вставки.
    let mut found: Vec<(u64, Vec<u8>)> = Vec::new();
    while let Some(key) = cursor.key() {
        // Записи об устройствах лежат под объектом 1, то есть **до** кусков, и
        // поиск с ключа (256, CHUNK_ITEM, 0) их уже пропустил. Всё, что идёт
        // после последнего куска, дереву кусков не принадлежит.
        if key.objectid != objectid::FIRST_CHUNK_TREE || key.kind != item_type::CHUNK_ITEM {
            break;
        }
        let raw = cursor.item()?;
        let mut copy = Vec::new();
        copy.try_reserve_exact(raw.len()).map_err(|_| Error::NoMemory)?;
        copy.extend_from_slice(raw);
        found.try_reserve(1).map_err(|_| Error::NoMemory)?;
        found.push((key.offset, copy));
        cursor.next(volume, dev)?;
    }

    for (logical, raw) in found {
        volume.chunks.add(logical, &raw)?;
    }
    Ok(())
}

/// Найти корень дерева по номеру в дереве корней.
fn find_root(
    volume: &mut Volume,
    dev: &mut dyn BlockDevice,
    root_tree: (u64, u8),
    which: u64,
) -> Result<(u64, u8)> {
    let key = Key::new(which, item_type::ROOT_ITEM, 0);
    let cursor = Cursor::seek(volume, dev, root_tree.0, root_tree.1, key)?;
    let Some(found) = cursor.key() else {
        return Err(Error::Corrupt);
    };
    // Смещение в ключе корня — номер поколения снимка; нам нужен тот, у
    // которого оно нулевое, то есть сам подтом, а не его снимок.
    if found.objectid != which || found.kind != item_type::ROOT_ITEM {
        return Err(Error::Corrupt);
    }
    let raw = cursor.item()?;
    if raw.len() < ROOT_ITEM_MIN_SIZE {
        return Err(Error::Corrupt);
    }
    Ok((u64_at(raw, ROOT_ITEM_BYTENR), raw[ROOT_ITEM_LEVEL]))
}
