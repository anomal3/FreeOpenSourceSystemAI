//! Разметка тома ext2 — и только она.
//!
//! Здесь том **создаётся**: считается геометрия, обнуляются служебные области,
//! размечаются битовые карты, пишутся суперблок с дескрипторами групп и
//! корневой каталог. Всё, что происходит с томом дальше — файлы, каталоги,
//! удаление, — умеет [`Editor`], и [`format`] отдаёт наружу именно его.
//!
//! Раньше файлы умел создавать и этот модуль, отдельным кодом, работавшим
//! только на свежем томе. Двух писателей одного формата больше нет: тот путь,
//! которым установщик раскладывает корневой раздел, и тот, которым пишет ядро,
//! — один и тот же, а значит и проверяется он вдвое чаще.
//!
//! # Битовые карты держатся в памяти
//!
//! Только на время разметки. Обе карты всех групп заполняются в памяти и
//! уходят на диск один раз — до того, как том кому-нибудь достанется. Дело в
//! размере: у тома в 512 МиБ это 16 КиБ на карты блоков и столько же на карты
//! inode, и правка бита в памяти вместо чтения-записи сектора экономит на
//! разметке тысячи обращений. Живой том так вести себя уже не вправе — почему,
//! написано в заголовке [`crate::edit`].

use alloc::vec::Vec;

use disk::BlockDevice;

use crate::layout::*;
use crate::edit::Editor;
use crate::{Error, Result};

/// Параметры форматирования.
pub struct FormatOptions<'a> {
    /// Метка тома: до 16 байт.
    pub label: &'a str,
    /// Идентификатор тома. Уникальность нужна тому, кто монтирует по UUID;
    /// криптостойкость — нет.
    pub uuid: [u8; 16],
    /// Время создания в секундах эпохи Unix.
    ///
    /// Задаётся снаружи, а не берётся из часов: в UEFI часы доступны только
    /// через runtime-сервисы, о которых этот крейт знать не должен, а хостовой
    /// сборке фиксированное время даёт побайтово воспроизводимый образ.
    pub time: u32,
}

/// Разметка тома: битовые карты в памяти и один сброс на диск в конце.
///
/// Существует только внутри [`format`]: наружу отдаётся [`Editor`], который
/// работает уже по-другому — каждым выделением на диск (см. заголовок
/// `crate::edit`). Здесь же том ещё никому не принадлежит, терять нечего, и
/// разметка полутора сотен килобайт карт превращается в правку байтов в
/// памяти.
struct Writer {
    geometry: Geometry,
    /// Битовые карты блоков всех групп подряд, по одному блоку на группу.
    block_bitmaps: Vec<u8>,
    /// Битовые карты inode всех групп подряд.
    inode_bitmaps: Vec<u8>,
    free_blocks_in_group: Vec<u32>,
    free_inodes_in_group: Vec<u32>,
    used_dirs_in_group: Vec<u16>,
    /// С какого блока начинать поиск свободного. Не «первый свободный», а
    /// подсказка: блоки раздаются подряд, и линейный поиск с нуля на каждом
    /// выделении превратил бы запись файла в квадратичную операцию.
    block_hint: u32,
    time: u32,
}

/// Отформатировать раздел под ext2 и открыть его на правку.
///
/// Возвращается [`Editor`], а не отдельный писатель: наполнять свежий том и
/// править живой — одно и то же действие над одним и тем же форматом, и делать
/// его двумя разными кусками кода значило бы проверять только один из них.
pub fn format(
    dev: &mut dyn BlockDevice,
    first_lba: u64,
    sectors: u64,
    options: &FormatOptions,
) -> Result<Editor> {
    disk::check_device(dev)?;
    if first_lba + sectors > dev.sector_count() {
        return Err(Error::TooSmall);
    }
    // Размер сектора берётся у носителя: он решает, сколько секторов занимает
    // блок файловой системы и по какому LBA лежит каждый из них. Догадка здесь
    // означала бы том, у которого все адреса смещены в восемь раз.
    let geometry = Geometry::plan_on(first_lba, sectors, dev.sector_size())?;
    Writer::create(dev, geometry, options)?.into_editor(dev, options)
}

/// То же с заданным размером блока — нужно тестам.
pub fn format_with(
    dev: &mut dyn BlockDevice,
    first_lba: u64,
    sectors: u64,
    block_size: BlockSize,
    options: &FormatOptions,
) -> Result<Editor> {
    disk::check_device(dev)?;
    let geometry = Geometry::plan_with_on(first_lba, sectors, block_size, dev.sector_size())?;
    Writer::create(dev, geometry, options)?.into_editor(dev, options)
}

impl Writer {
    fn create(
        dev: &mut dyn BlockDevice,
        geometry: Geometry,
        options: &FormatOptions,
    ) -> Result<Self> {
        let block_bytes = geometry.block_size.bytes() as usize;
        let groups = geometry.groups as usize;

        let mut writer = Self {
            geometry,
            block_bitmaps: try_zeroed(groups * block_bytes)?,
            inode_bitmaps: try_zeroed(groups * block_bytes)?,
            free_blocks_in_group: try_vec(groups, 0)?,
            free_inodes_in_group: try_vec(groups, geometry.inodes_per_group)?,
            used_dirs_in_group: try_vec(groups, 0)?,
            block_hint: 0,
            time: options.time,
        };

        // Служебные структуры обнуляются на диске: в битовых картах и таблицах
        // inode значение по умолчанию — ноль, а на разделе после прежней
        // установки лежит что угодно. Область данных не трогаем: она на
        // порядки больше, а её содержимое за пределами файлов никого не
        // интересует.
        for group in 0..geometry.groups {
            // Обнуляется вся служебная область группы, включая резервные копии
            // суперблока и таблицы дескрипторов: от прежней файловой системы
            // там могла остаться её подпись, и утилита восстановления нашла бы
            // чужой суперблок там, где мы свой ещё не написали.
            writer.zero_blocks(
                dev,
                geometry.group_first_block(group),
                geometry.group_overhead_blocks(group),
            )?;
        }
        // Нулевой блок тома в служебную область группы не входит: при блоке в
        // 1024 байта группа начинается с блока 1, а нулевой — место под
        // загрузочный сектор.
        if geometry.first_data_block == 1 {
            writer.zero_blocks(dev, 0, 1)?;
        }

        writer.reserve_metadata()?;
        writer.create_root(dev)?;
        writer.write_superblocks(dev, options)?;

        Ok(writer)
    }

    /// Сбросить битовые карты на диск и отдать том редактору.
    ///
    /// Счётчики уезжают в редактор как есть, а не перечитываются с диска:
    /// разметка их только что посчитала, и читать собственную запись обратно
    /// значило бы проверять носитель, а не себя.
    fn into_editor(
        self,
        dev: &mut dyn BlockDevice,
        options: &FormatOptions,
    ) -> Result<Editor> {
        let block_bytes = self.geometry.block_size.bytes() as usize;
        for group in 0..self.geometry.groups {
            let at = group as usize * block_bytes;
            let bitmap = self.block_bitmaps[at..at + block_bytes].to_vec();
            self.write_block(dev, self.geometry.block_bitmap_block(group), &bitmap)?;
            let bitmap = self.inode_bitmaps[at..at + block_bytes].to_vec();
            self.write_block(dev, self.geometry.inode_bitmap_block(group), &bitmap)?;
        }
        self.write_superblocks(dev, options)?;
        dev.flush()?;

        Ok(Editor::adopt(
            self.geometry,
            self.free_blocks_in_group,
            self.free_inodes_in_group,
            self.used_dirs_in_group,
            self.time,
        ))
    }

    /// Пометить занятым всё, что занимают служебные структуры.
    fn reserve_metadata(&mut self) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        for group in 0..geometry.groups {
            let in_group = geometry.blocks_in_group(group);
            let overhead = geometry.group_overhead_blocks(group);
            if overhead >= in_group {
                return Err(Error::TooSmall);
            }
            for index in 0..overhead {
                set_bit(&mut self.block_bitmaps, block_bytes, group, index);
            }
            self.free_blocks_in_group[group as usize] = in_group - overhead;

            // Биты за концом группы обязаны стоять единицами. Последняя группа
            // короче битовой карты, и нулевой бит там означал бы свободный
            // блок за пределами тома; `e2fsck` считает это повреждением, а
            // попытка выделить такой блок ушла бы за край раздела.
            for index in in_group..geometry.blocks_per_group {
                set_bit(&mut self.block_bitmaps, block_bytes, group, index);
            }
            // То же для битовой карты inode: их в группе ровно
            // `inodes_per_group`, а бит в карте — на каждый бит блока.
            for index in geometry.inodes_per_group..geometry.blocks_per_group {
                set_bit(&mut self.inode_bitmaps, block_bytes, group, index);
            }
        }

        // Зарезервированные inode 1..=10. Второй из них — корневой каталог, он
        // будет создан следом.
        for inode in 1..FIRST_INODE {
            self.mark_inode_used(inode)?;
        }
        Ok(())
    }

    /// Создать корневой каталог.
    fn create_root(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let block = self.alloc_block()?;
        self.zero_blocks(dev, block, 1)?;
        // У корня и «.», и «..» указывают на него самого: подниматься из корня
        // некуда, и спецификация требует именно этого.
        self.init_directory_block(dev, block, ROOT_INODE, ROOT_INODE)?;

        let mut inode = InodeData::new(MODE_DIRECTORY | 0o755, 0, 0, self.time);
        inode.size = u64::from(self.geometry.block_size.bytes());
        // Две ссылки: запись «.» внутри самого каталога и запись «..» — тоже
        // на него. Каждый созданный подкаталог добавит третью и далее.
        inode.links = 2;
        inode.blocks[0] = block;
        inode.sectors = self.geometry.sectors_per_block();
        self.write_inode(dev, ROOT_INODE, &inode)?;
        self.used_dirs_in_group[0] += 1;
        Ok(())
    }

    // --- выделение ---------------------------------------------------------

    fn alloc_block(&mut self) -> Result<u32> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let mut block = self.block_hint.max(geometry.first_data_block);
        while block < geometry.blocks {
            let group = (block - geometry.first_data_block) / geometry.blocks_per_group;
            let index = (block - geometry.first_data_block) % geometry.blocks_per_group;
            if !test_bit(&self.block_bitmaps, block_bytes, group, index) {
                set_bit(&mut self.block_bitmaps, block_bytes, group, index);
                self.free_blocks_in_group[group as usize] -= 1;
                self.block_hint = block + 1;
                return Ok(block);
            }
            block += 1;
        }
        Err(Error::NoSpace)
    }

    fn mark_inode_used(&mut self, inode: u32) -> Result<()> {
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let (group, index) = self.geometry.locate_inode(inode)?;
        if !test_bit(&self.inode_bitmaps, block_bytes, group, index) {
            set_bit(&mut self.inode_bitmaps, block_bytes, group, index);
            self.free_inodes_in_group[group as usize] -= 1;
        }
        Ok(())
    }

    // --- запись структур ---------------------------------------------------

    fn write_block(&self, dev: &mut dyn BlockDevice, block: u32, data: &[u8]) -> Result<()> {
        debug_assert_eq!(data.len(), self.geometry.block_size.bytes() as usize);
        dev.write(self.geometry.block_lba(block), data)?;
        Ok(())
    }

    fn read_block(&self, dev: &mut dyn BlockDevice, block: u32) -> Result<Vec<u8>> {
        let mut buf = try_zeroed(self.geometry.block_size.bytes() as usize)?;
        dev.read(self.geometry.block_lba(block), &mut buf)?;
        Ok(buf)
    }

    fn zero_blocks(&self, dev: &mut dyn BlockDevice, block: u32, count: u32) -> Result<()> {
        /// Размер источника нулей в байтах, а не в секторах: при секторе 4096
        /// шестнадцать секторов — это уже 64 КиБ статической памяти впустую.
        const CHUNK: usize = 8 * 1024;
        static ZEROS: [u8; CHUNK] = [0; CHUNK];

        let sector = self.geometry.sector_size as usize;
        let per_write = (CHUNK / sector).max(1);
        let sectors = u64::from(count) * u64::from(self.geometry.sectors_per_block());
        let mut done = 0u64;
        while done < sectors {
            let batch = ((sectors - done) as usize).min(per_write);
            dev.write(
                self.geometry.block_lba(block) + done,
                &ZEROS[..batch * sector],
            )?;
            done += batch as u64;
        }
        Ok(())
    }

    /// Заполнить свежий блок каталога записями «.» и «..».
    fn init_directory_block(
        &self,
        dev: &mut dyn BlockDevice,
        block: u32,
        self_inode: u32,
        parent: u32,
    ) -> Result<()> {
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let mut buf = try_zeroed(block_bytes)?;

        // «.» занимает 12 байт: 8 заголовка плюс имя, выровненное до четырёх.
        put_u32(&mut buf, 0, self_inode);
        put_u16(&mut buf, 4, 12);
        buf[6] = 1;
        buf[7] = DIR_TYPE_DIRECTORY;
        buf[8] = b'.';

        // «..» забирает весь остаток блока: последняя запись обязана
        // дотягиваться до его конца, иначе разбор упрётся в мусор.
        put_u32(&mut buf, 12, parent);
        put_u16(&mut buf, 16, (block_bytes - 12) as u16);
        buf[18] = 2;
        buf[19] = DIR_TYPE_DIRECTORY;
        buf[20] = b'.';
        buf[21] = b'.';

        self.write_block(dev, block, &buf)
    }

    fn write_inode(
        &self,
        dev: &mut dyn BlockDevice,
        number: u32,
        inode: &InodeData,
    ) -> Result<()> {
        let (group, index) = self.geometry.locate_inode(number)?;
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let byte_offset = index as usize * INODE_SIZE;
        let block = self.geometry.inode_table_block(group) + (byte_offset / block_bytes) as u32;
        let within = byte_offset % block_bytes;

        let mut buf = self.read_block(dev, block)?;
        inode.encode(&mut buf[within..within + INODE_SIZE]);
        self.write_block(dev, block, &buf)
    }

    // --- суперблок и дескрипторы групп -------------------------------------

    fn write_superblocks(
        &self,
        dev: &mut dyn BlockDevice,
        options: &FormatOptions,
    ) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;

        let free_blocks: u32 = self.free_blocks_in_group.iter().sum();
        let free_inodes: u32 = self.free_inodes_in_group.iter().sum();

        let descriptors = self.encode_group_descriptors()?;

        for group in 0..geometry.groups {
            if !geometry.group_has_super(group) {
                continue;
            }
            let mut sb = [0u8; SUPERBLOCK_SIZE];
            self.encode_superblock(&mut sb, options, free_blocks, free_inodes, group);

            // Суперблок группы — это её первый блок. Смещение внутри блока
            // ненулевое ровно в одном случае: у нулевой группы при блоке
            // крупнее килобайта, потому что первый килобайт тома отдан под
            // загрузочный сектор и суперблок начинается за ним. Ошибка здесь
            // даёт том, который монтируется, но не чинится.
            let block = geometry.group_first_block(group);
            let within = if group == 0 && geometry.block_size != BlockSize::B1024 {
                SUPERBLOCK_OFFSET as usize
            } else {
                0
            };

            let mut buf = self.read_block(dev, block)?;
            buf[within..within + SUPERBLOCK_SIZE].copy_from_slice(&sb);
            self.write_block(dev, block, &buf)?;

            // Таблица дескрипторов идёт сразу за блоком суперблока.
            let table_start = block + 1;
            for (index, chunk) in descriptors.chunks(block_bytes).enumerate() {
                let mut buf = try_zeroed(block_bytes)?;
                buf[..chunk.len()].copy_from_slice(chunk);
                self.write_block(dev, table_start + index as u32, &buf)?;
            }
        }
        Ok(())
    }

    fn encode_group_descriptors(&self) -> Result<Vec<u8>> {
        let geometry = self.geometry;
        let mut out = try_zeroed(geometry.groups as usize * GROUP_DESC_SIZE)?;
        for group in 0..geometry.groups {
            let at = group as usize * GROUP_DESC_SIZE;
            put_u32(&mut out, at, geometry.block_bitmap_block(group));
            put_u32(&mut out, at + 4, geometry.inode_bitmap_block(group));
            put_u32(&mut out, at + 8, geometry.inode_table_block(group));
            put_u16(&mut out, at + 12, self.free_blocks_in_group[group as usize] as u16);
            put_u16(&mut out, at + 14, self.free_inodes_in_group[group as usize] as u16);
            put_u16(&mut out, at + 16, self.used_dirs_in_group[group as usize]);
        }
        Ok(out)
    }

    fn encode_superblock(
        &self,
        sb: &mut [u8; SUPERBLOCK_SIZE],
        options: &FormatOptions,
        free_blocks: u32,
        free_inodes: u32,
        group: u32,
    ) {
        let geometry = self.geometry;
        put_u32(sb, 0, geometry.inodes());
        put_u32(sb, 4, geometry.blocks);
        // Резерв под root: обычно 5%, но здесь ноль. Резерв существует, чтобы
        // переполнение диска пользователем не помешало системным службам; на
        // корневом разделе, куда установщик кладёт десяток файлов, он лишь
        // отнимал бы место.
        put_u32(sb, 8, 0);
        put_u32(sb, 12, free_blocks);
        put_u32(sb, 16, free_inodes);
        put_u32(sb, 20, geometry.first_data_block);
        put_u32(sb, 24, geometry.block_size.log());
        put_u32(sb, 28, geometry.block_size.log());
        put_u32(sb, 32, geometry.blocks_per_group);
        put_u32(sb, 36, geometry.blocks_per_group);
        put_u32(sb, 40, geometry.inodes_per_group);
        put_u32(sb, 44, options.time);
        put_u32(sb, 48, options.time);
        put_u16(sb, 52, 0);
        // «Проверять после стольких монтирований»: -1 означает «не проверять по
        // счётчику». Иначе первое же монтирование в Linux потребовало бы fsck.
        put_u16(sb, 54, 0xFFFF);
        put_u16(sb, 56, MAGIC);
        // Состояние «чистая». Ставится сразу: том никто не монтировал.
        put_u16(sb, 58, 1);
        // Что делать при ошибке: продолжать.
        put_u16(sb, 60, 1);
        put_u16(sb, 62, 0);
        put_u32(sb, 64, options.time);
        // Интервал проверки по времени — тоже выключен.
        put_u32(sb, 68, 0);
        // Создатель: Linux. Значение определяет разбор поля `i_osd2`, и «свой»
        // код здесь означал бы, что чужие инструменты не знают, как его читать.
        put_u32(sb, 72, 0);
        // Ревизия 1 (dynamic): только в ней существуют поля размера inode и
        // возможностей, без которых `s_inode_size` читать неоткуда.
        put_u32(sb, 76, 1);
        put_u16(sb, 80, 0);
        put_u16(sb, 82, 0);
        put_u32(sb, 84, FIRST_INODE);
        put_u16(sb, 88, INODE_SIZE as u16);
        put_u16(sb, 90, group as u16);
        put_u32(sb, 92, 0);
        put_u32(sb, 96, FEATURE_INCOMPAT_FILETYPE);
        put_u32(sb, 100, 0);
        sb[104..120].copy_from_slice(&options.uuid);

        let label = options.label.as_bytes();
        let len = label.len().min(16);
        sb[120..120 + len].copy_from_slice(&label[..len]);
    }
}

/// Записать запись каталога по смещению.
pub(crate) fn write_entry(buf: &mut [u8], at: usize, inode: u32, rec_len: usize, name: &str, kind: u8) {
    put_u32(buf, at, inode);
    put_u16(buf, at + 4, rec_len as u16);
    buf[at + 6] = name.len() as u8;
    buf[at + 7] = kind;
    buf[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
    // Хвост записи обнуляется: там мог лежать остаток прежнего имени, и
    // инструменты, читающие `rec_len` байт целиком, показали бы мусор.
    let end = at + rec_len;
    let name_end = at + 8 + name.len();
    if name_end < end {
        buf[name_end..end].fill(0);
    }
}

/// Inode в удобном для работы виде.
pub(crate) struct InodeData {
    pub(crate) mode: u16,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) size: u64,
    pub(crate) links: u16,
    /// Блоков по 512 байт — так считает поле `i_blocks`, вопреки названию.
    pub(crate) sectors: u32,
    pub(crate) blocks: [u32; BLOCK_POINTERS],
    pub(crate) time: u32,
    /// Время удаления. Ноль у живого inode; у освобождённого `e2fsck` требует
    /// ненулевого — иначе он считает его «deleted inode referenced».
    pub(crate) deleted: u32,
}

impl InodeData {
    pub(crate) fn new(mode: u16, uid: u32, gid: u32, time: u32) -> Self {
        Self {
            mode,
            uid,
            gid,
            size: 0,
            links: 0,
            sectors: 0,
            blocks: [0; BLOCK_POINTERS],
            time,
            deleted: 0,
        }
    }

    pub(crate) fn encode(&self, out: &mut [u8]) {
        out.fill(0);
        put_u16(out, 0, self.mode);
        put_u16(out, 2, self.uid as u16);
        put_u32(out, 4, self.size as u32);
        put_u32(out, 8, self.time);
        put_u32(out, 12, self.time);
        put_u32(out, 16, self.time);
        put_u32(out, 20, self.deleted);
        put_u16(out, 24, self.gid as u16);
        put_u16(out, 26, self.links);
        put_u32(out, 28, self.sectors);
        put_u32(out, 32, 0);
        put_u32(out, 36, 0);
        for (index, block) in self.blocks.iter().enumerate() {
            put_u32(out, 40 + index * 4, *block);
        }
        put_u32(out, 100, 0);
        put_u32(out, 104, 0);
        // У обычного файла старшие 32 бита размера лежат в `i_dir_acl`; у
        // каталога это поле означает другое, поэтому пишем только для файлов.
        if self.mode & 0xF000 == MODE_REGULAR {
            put_u32(out, 108, (self.size >> 32) as u32);
        }
        // Старшие 16 бит uid и gid — в `i_osd2`, поля `l_i_uid_high` и
        // `l_i_gid_high`. Без них идентификаторы больше 65535 молча теряются.
        put_u16(out, 120, (self.uid >> 16) as u16);
        put_u16(out, 122, (self.gid >> 16) as u16);
    }

    pub(crate) fn decode(raw: &[u8]) -> Self {
        let mut blocks = [0u32; BLOCK_POINTERS];
        for (index, block) in blocks.iter_mut().enumerate() {
            *block = u32_at(raw, 40 + index * 4);
        }
        let mode = u16_at(raw, 0);
        let low = u64::from(u32_at(raw, 4));
        let size = if mode & 0xF000 == MODE_REGULAR {
            low | (u64::from(u32_at(raw, 108)) << 32)
        } else {
            low
        };
        Self {
            mode,
            uid: u32::from(u16_at(raw, 2)) | (u32::from(u16_at(raw, 120)) << 16),
            gid: u32::from(u16_at(raw, 24)) | (u32::from(u16_at(raw, 122)) << 16),
            size,
            links: u16_at(raw, 26),
            sectors: u32_at(raw, 28),
            blocks,
            time: u32_at(raw, 8),
            deleted: u32_at(raw, 20),
        }
    }
}

/// Выделить нулевой буфер, вернув ошибку вместо паники при нехватке памяти.
pub(crate) fn try_zeroed(len: usize) -> Result<Vec<u8>> {
    try_vec(len, 0u8)
}

pub(crate) fn try_vec<T: Clone>(len: usize, value: T) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| Error::NoMemory)?;
    out.resize(len, value);
    Ok(out)
}

/// Бит в битовой карте группы.
///
/// Свободные функции, а не методы: иначе они заимствовали бы `self` целиком, и
/// правка карты рядом с чтением геометрии стала бы невозможна.
///
/// Порядок бит внутри байта — от младшего к старшему; перепутать его значит
/// пометить занятым не тот блок, причём том всё равно смонтируется.
pub(crate) fn set_bit(bitmaps: &mut [u8], block_bytes: usize, group: u32, index: u32) {
    let at = group as usize * block_bytes + (index / 8) as usize;
    bitmaps[at] |= 1 << (index % 8);
}

pub(crate) fn test_bit(bitmaps: &[u8], block_bytes: usize, group: u32, index: u32) -> bool {
    let at = group as usize * block_bytes + (index / 8) as usize;
    bitmaps[at] & (1 << (index % 8)) != 0
}
