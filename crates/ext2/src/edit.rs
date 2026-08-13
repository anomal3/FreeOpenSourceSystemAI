//! Правка тома ext2 на месте: создание, дозапись, усечение, удаление.
//!
//! # Чем это отличается от форматирования
//!
//! Форматирование ([`crate::write`]) держит битовые карты в памяти и сбрасывает
//! их один раз в конце: том, которого ещё не существует, потерять невозможно —
//! прерванная разметка означает просто отсутствие файловой системы. Правка
//! живого тома так работать не может. Здесь **каждое выделение и освобождение
//! уходит на диск сразу**, и цена известна: чтение и запись сектора битовой
//! карты на блок. Взамен состояние карт на диске в любой момент совпадает с
//! тем, что занято на самом деле, — а это единственное, что защищает уже
//! записанные файлы от потери питания.
//!
//! В памяти остаются только **счётчики**: сколько свободных блоков и inode в
//! каждой группе. Они дублируют то, что можно пересчитать по картам, и живут в
//! суперблоке с дескрипторами групп; [`Editor::flush`] сбрасывает их туда. Если
//! питание пропадёт до сброса, счётчики разъедутся с картами — `e2fsck` это
//! чинит и именно так и пишет: «free blocks count wrong». Данные при этом целы.
//! Обещать большее без журнала нечестно, а журнал — это уже ext3 (см. заголовок
//! крейта).
//!
//! # Почему писатель один
//!
//! Раньше файлы и каталоги умел создавать только форматирующий писатель, и
//! умел он это ровно один раз — на свежем томе. Теперь то же самое делает
//! редактор, а форматирование сводится к «создать пустой том и отдать его
//! редактору». Выигрыш не в строчках: путь записи, которым установщик
//! раскладывает корневой раздел, стал тем же самым путём, которым пишет ядро.
//! Значит, его проверяет не только `cargo test`, но и каждая установка на
//! стенде.

use alloc::vec::Vec;

use disk::BlockDevice;

use crate::layout::*;
use crate::read::Ext2;
use crate::write::{InodeData, try_zeroed, write_entry};
use crate::{Error, Result, check_name};

/// Том, открытый на правку.
pub struct Editor {
    geometry: Geometry,
    /// Свободные блоки по группам — то, что лежит в дескрипторах групп.
    free_blocks_in_group: Vec<u32>,
    free_inodes_in_group: Vec<u32>,
    used_dirs_in_group: Vec<u16>,
    /// С какого блока искать свободный. Не «первый свободный», а подсказка:
    /// линейный поиск с начала тома на каждом выделении превратил бы запись
    /// файла в квадратичную операцию.
    block_hint: u32,
    inode_hint: u32,
    /// Штамп времени для создаваемых inode.
    time: u32,
    /// Что о состоянии тома сейчас написано **на диске**: `true` — «закрыт
    /// чисто». Держится в памяти, чтобы не читать суперблок ради ответа на
    /// вопрос, который редактор сам себе и задал: [`Editor::mark_dirty`]
    /// вызывается перед каждой правкой и обязан быть бесплатным, когда том уже
    /// помечен.
    clean_on_disk: bool,
}

impl Editor {
    /// Открыть существующий том на правку.
    pub fn open(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<Self> {
        let fs = Ext2::mount(dev, first_lba)?;
        let geometry = fs.geometry();
        let time = Ext2::write_time(dev, first_lba)?;
        let groups = geometry.groups as usize;

        let mut editor = Self {
            geometry,
            free_blocks_in_group: crate::write::try_vec(groups, 0)?,
            free_inodes_in_group: crate::write::try_vec(groups, 0)?,
            used_dirs_in_group: crate::write::try_vec(groups, 0)?,
            block_hint: geometry.first_data_block,
            inode_hint: FIRST_INODE,
            time,
            clean_on_disk: fs.was_clean(),
        };
        editor.load_group_descriptors(dev)?;
        // Том помечается используемым **сразу**, до первой правки, и это
        // единственный порядок, который что-то значит. Пометь мы его при первой
        // записи — между началом записи и пометкой оставалось бы окно, в
        // котором пропажа питания даёт том с устаревшими счётчиками и с
        // признаком «закрыт чисто» на диске: следующая загрузка поверила бы
        // счётчикам и выдала бы под новый файл уже занятый блок.
        editor.mark_dirty(dev)?;
        Ok(editor)
    }

    /// Собрать редактор поверх только что размеченного тома.
    ///
    /// Отличается от [`Editor::open`] лишь тем, что счётчики берутся не с
    /// диска: форматирование их только что посчитало, и перечитывать
    /// собственную запись незачем.
    pub(crate) fn adopt(
        geometry: Geometry,
        free_blocks_in_group: Vec<u32>,
        free_inodes_in_group: Vec<u32>,
        used_dirs_in_group: Vec<u16>,
        time: u32,
    ) -> Self {
        Self {
            geometry,
            free_blocks_in_group,
            free_inodes_in_group,
            used_dirs_in_group,
            block_hint: geometry.first_data_block,
            inode_hint: FIRST_INODE,
            time,
            // Разметка только что записала суперблок с признаком «чистый»;
            // первая же правка через [`Editor::mark_dirty`] его снимет.
            clean_on_disk: true,
        }
    }

    #[must_use]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Сколько байт ещё можно записать.
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        let free: u32 = self.free_blocks_in_group.iter().sum();
        u64::from(free) * u64::from(self.geometry.block_size.bytes())
    }

    /// Каким временем помечать создаваемые файлы.
    ///
    /// Задаётся снаружи, потому что часов у этого крейта нет и быть не должно:
    /// в UEFI время лежит за runtime-сервисами, в ядре — за ACPI или RTC, и
    /// знание об этом не имеет отношения к формату файловой системы.
    pub fn set_time(&mut self, seconds: u32) {
        self.time = seconds;
    }

    // --- каталоги ----------------------------------------------------------

    /// Найти запись в каталоге: номер inode и тип из [`DIR_TYPE_REGULAR`] /
    /// [`DIR_TYPE_DIRECTORY`].
    pub fn lookup(
        &mut self,
        dev: &mut dyn BlockDevice,
        dir: u32,
        name: &str,
    ) -> Result<Option<(u32, u8)>> {
        let inode = self.read_inode(dev, dir)?;
        self.find_entry(dev, &inode, name)
    }

    /// Создать каталог по пути вида `etc` или `home/roman`, создавая
    /// недостающие звенья.
    pub fn create_dir_path(
        &mut self,
        dev: &mut dyn BlockDevice,
        path: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32> {
        let mut dir = ROOT_INODE;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            dir = self.ensure_dir(dev, dir, component, mode, uid, gid)?;
        }
        Ok(dir)
    }

    /// Найти подкаталог или создать его.
    pub fn ensure_dir(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32> {
        check_name(name)?;
        if let Some((number, kind)) = self.lookup(dev, parent, name)? {
            if kind != DIR_TYPE_DIRECTORY {
                return Err(Error::NotADirectory);
            }
            return Ok(number);
        }
        self.mkdir(dev, parent, name, mode, uid, gid)
    }

    /// Создать подкаталог. Отказывает, если имя занято.
    pub fn mkdir(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32> {
        check_name(name)?;
        if self.lookup(dev, parent, name)?.is_some() {
            return Err(Error::Exists);
        }

        let number = self.alloc_inode(dev)?;
        let block = self.alloc_block(dev)?;
        self.init_directory_block(dev, block, number, parent)?;

        let mut inode = InodeData::new(MODE_DIRECTORY | (mode & 0o7777), uid, gid, self.time);
        inode.size = u64::from(self.geometry.block_size.bytes());
        // Две ссылки: запись «.» внутри самого каталога и запись о нём в
        // родителе. Каждый вложенный каталог добавит третью — свою «..».
        inode.links = 2;
        inode.blocks[0] = block;
        inode.sectors = self.geometry.sectors_per_block();
        self.write_inode(dev, number, &inode)?;

        self.add_entry(dev, parent, name, number, DIR_TYPE_DIRECTORY)?;
        // Каждый подкаталог добавляет родителю ссылку — свою запись «..».
        // Забыть это значит получить том, который `e2fsck` чинит при первом же
        // запуске.
        self.bump_links(dev, parent, 1)?;

        let (group, _) = self.geometry.locate_inode(number)?;
        self.used_dirs_in_group[group as usize] += 1;
        Ok(number)
    }

    /// Удалить пустой каталог.
    ///
    /// Непустой не удаляется, и рекурсивного удаления здесь нет намеренно:
    /// обход дерева с освобождением — это то место, где ошибка стирает не то,
    /// что просили, и делать её вне поля зрения вызывающего не стоит.
    pub fn rmdir(&mut self, dev: &mut dyn BlockDevice, parent: u32, name: &str) -> Result<()> {
        check_name(name)?;
        let Some((number, kind)) = self.lookup(dev, parent, name)? else {
            return Err(Error::NotFound);
        };
        if kind != DIR_TYPE_DIRECTORY {
            return Err(Error::NotADirectory);
        }
        if !self.directory_is_empty(dev, number)? {
            return Err(Error::NotEmpty);
        }

        self.remove_entry(dev, parent, name)?;
        // Ссылка «..» удаляемого каталога исчезает вместе с ним.
        self.bump_links(dev, parent, -1)?;

        self.truncate(dev, number, 0)?;
        self.free_inode(dev, number, true)?;
        Ok(())
    }

    /// Есть ли в каталоге что-нибудь, кроме «.» и «..».
    fn directory_is_empty(&mut self, dev: &mut dyn BlockDevice, dir: u32) -> Result<bool> {
        let inode = self.read_inode(dev, dir)?;
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let blocks = (inode.size as usize).div_ceil(block_bytes);
        for index in 0..blocks {
            let block = self.directory_block(dev, &inode, index)?;
            if block == 0 {
                continue;
            }
            let buf = self.read_block(dev, block)?;
            let mut at = 0usize;
            while at + 8 <= buf.len() {
                let entry = u32_at(&buf, at);
                let rec_len = u16_at(&buf, at + 4) as usize;
                if rec_len < 8 || at + rec_len > buf.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = buf[at + 6] as usize;
                let name = &buf[at + 8..(at + 8 + name_len).min(buf.len())];
                if entry != 0 && name != b"." && name != b".." {
                    return Ok(false);
                }
                at += rec_len;
            }
        }
        Ok(true)
    }

    // --- файлы -------------------------------------------------------------

    /// Создать пустой файл в каталоге `parent`.
    pub fn create(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u32,
        name: &str,
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32> {
        check_name(name)?;
        if self.lookup(dev, parent, name)?.is_some() {
            return Err(Error::Exists);
        }

        let number = self.alloc_inode(dev)?;
        let mut inode = InodeData::new(MODE_REGULAR | (mode & 0o7777), uid, gid, self.time);
        inode.links = 1;
        self.write_inode(dev, number, &inode)?;
        self.add_entry(dev, parent, name, number, DIR_TYPE_REGULAR)?;
        Ok(number)
    }

    /// Создать файл с содержимым — то, чем пользуется установщик.
    pub fn create_file(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u32,
        name: &str,
        data: &[u8],
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32> {
        let number = self.create(dev, parent, name, mode, uid, gid)?;
        if !data.is_empty() {
            self.write_at(dev, number, 0, data)?;
        }
        Ok(number)
    }

    /// Записать файл по пути вида `etc/passwd`, создавая каталоги пути.
    pub fn write_file_path(
        &mut self,
        dev: &mut dyn BlockDevice,
        path: &str,
        data: &[u8],
        mode: u16,
        uid: u32,
        gid: u32,
    ) -> Result<u32> {
        let (parent, name) = match path.rsplit_once('/') {
            // Промежуточным каталогам права даются исполняемыми: каталог без
            // бита `x` невозможно пройти насквозь, и файл внутри него стал бы
            // недоступен.
            Some((parent, name)) => (self.create_dir_path(dev, parent, 0o755, uid, gid)?, name),
            None => (ROOT_INODE, path),
        };
        self.create_file(dev, parent, name, data, mode, uid, gid)
    }

    /// Записать данные в файл по смещению, выделяя блоки по мере надобности.
    ///
    /// Смещение за концом файла допустимо: промежуток остаётся дырой, то есть
    /// невыделенными блоками, которые читаются нулями. Так же ведёт себя любая
    /// файловая система Unix, и подделывать нули записью на диск значило бы
    /// тратить место на то, чего не писали.
    pub fn write_at(
        &mut self,
        dev: &mut dyn BlockDevice,
        number: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let mut inode = self.read_inode(dev, number)?;
        if inode.mode & 0xF000 != MODE_REGULAR {
            return Err(Error::NotADirectory);
        }

        let block_bytes = self.geometry.block_size.bytes() as usize;
        let mut done = 0usize;
        while done < data.len() {
            let position = offset + done as u64;
            let index = (position / block_bytes as u64) as usize;
            let within = (position % block_bytes as u64) as usize;
            let take = (block_bytes - within).min(data.len() - done);

            let block = self.block_for_write(dev, &mut inode, index)?;
            if take == block_bytes {
                self.write_block(dev, block, &data[done..done + take])?;
            } else {
                // Частичный блок читается перед записью: остальное в нём —
                // либо уже записанные данные файла, либо нули свежего блока, и
                // затирать их куском буфера нельзя.
                let mut buf = self.read_block(dev, block)?;
                buf[within..within + take].copy_from_slice(&data[done..done + take]);
                self.write_block(dev, block, &buf)?;
            }
            done += take;
        }

        let end = offset + data.len() as u64;
        if end > inode.size {
            inode.size = end;
        }
        inode.time = self.time;
        self.write_inode(dev, number, &inode)?;
        Ok(done)
    }

    /// Усечь или продлить файл.
    ///
    /// Продление не выделяет блоков: файл получает дыру, и она займёт место
    /// только тогда, когда в неё что-нибудь запишут.
    pub fn truncate(&mut self, dev: &mut dyn BlockDevice, number: u32, size: u64) -> Result<()> {
        let mut inode = self.read_inode(dev, number)?;
        let block_bytes = self.geometry.block_size.bytes() as u64;

        if size < inode.size {
            let keep = size.div_ceil(block_bytes) as usize;
            let had = inode.size.div_ceil(block_bytes) as usize;
            for index in (keep..had).rev() {
                self.free_file_block(dev, &mut inode, index)?;
            }
            // Хвост последнего блока обнуляется: если файл потом снова
            // вырастет, там иначе проступит прежнее содержимое — то самое,
            // которое усечением и убирали.
            let tail = (size % block_bytes) as usize;
            if tail != 0 {
                if let Some(block) = self.block_of(dev, &inode, keep - 1)? {
                    let mut buf = self.read_block(dev, block)?;
                    buf[tail..].fill(0);
                    self.write_block(dev, block, &buf)?;
                }
            }
        }

        inode.size = size;
        inode.time = self.time;
        self.write_inode(dev, number, &inode)
    }

    /// Удалить файл из каталога.
    ///
    /// Каталог этим не удаляется — для него есть [`Editor::rmdir`], и разница
    /// не в удобстве: у каталога есть содержимое, о судьбе которого вызывающий
    /// должен знать.
    pub fn unlink(&mut self, dev: &mut dyn BlockDevice, parent: u32, name: &str) -> Result<()> {
        check_name(name)?;
        let Some((number, kind)) = self.lookup(dev, parent, name)? else {
            return Err(Error::NotFound);
        };
        if kind == DIR_TYPE_DIRECTORY {
            return Err(Error::IsADirectory);
        }

        self.remove_entry(dev, parent, name)?;

        let mut inode = self.read_inode(dev, number)?;
        inode.links = inode.links.saturating_sub(1);
        if inode.links > 0 {
            // Жёстких ссылок этот крейт не создаёт, но формат их допускает, и
            // освободить inode, на который ссылается чужая запись, значило бы
            // отдать чужой файл под перезапись.
            self.write_inode(dev, number, &inode)?;
            return Ok(());
        }

        self.truncate(dev, number, 0)?;
        self.free_inode(dev, number, false)
    }

    // --- состояние тома ----------------------------------------------------

    /// Пометить том используемым — до того, как что-нибудь на нём изменится.
    ///
    /// Дёшево, когда том уже помечен: признак хранится в памяти, и повторный
    /// вызов не трогает диск вовсе. Поэтому его можно ставить перед каждой
    /// правкой, не думая о цене.
    pub fn mark_dirty(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        if !self.clean_on_disk {
            return Ok(());
        }
        self.write_state(dev, STATE_MOUNTED)?;
        self.clean_on_disk = false;
        Ok(())
    }

    /// Пометить том закрытым чисто.
    ///
    /// Вызывать **после** [`Editor::flush`] и только тогда, когда всё
    /// записанное действительно на носителе: этот признак — обещание
    /// следующей загрузке, что счётчикам можно верить.
    pub fn mark_clean(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        if self.clean_on_disk {
            return Ok(());
        }
        self.write_state(dev, STATE_CLEAN)?;
        self.clean_on_disk = true;
        Ok(())
    }

    /// Записать `s_state` в **основной** суперблок.
    ///
    /// Резервные копии не трогаются намеренно. Их читают только тогда, когда
    /// основной суперблок уничтожен, и в этом случае состояние тома — далеко
    /// не первый вопрос; зато записывать их пришлось бы при каждом
    /// монтировании, по одной записи блока на группу.
    fn write_state(&mut self, dev: &mut dyn BlockDevice, state: u16) -> Result<()> {
        let geometry = self.geometry;
        let block = geometry.group_first_block(0);
        let within = if geometry.block_size == BlockSize::B1024 {
            0
        } else {
            SUPERBLOCK_OFFSET as usize
        };
        let mut buf = self.read_block(dev, block)?;
        put_u16(&mut buf, within + SUPERBLOCK_STATE, state);
        self.write_block(dev, block, &buf)?;
        // Признак обязан дойти до пластин, а не остаться в кеше записи диска:
        // весь его смысл в том, чтобы пережить пропажу питания.
        dev.flush()?;
        Ok(())
    }

    // --- сброс на диск -----------------------------------------------------

    /// Записать счётчики в суперблок и дескрипторы групп.
    ///
    /// Обязателен после правок: до него на диске лежат счётчики от предыдущего
    /// сброса, и `e2fsck` объявит том требующим починки — данные при этом
    /// целы, но система, которую надо чинить после каждой записи файла, никуда
    /// не годится.
    ///
    /// Суперблок **правится**, а не собирается заново: метка, UUID, счётчик
    /// монтирований и всё прочее принадлежат тому, а не редактору, и
    /// пересобирать их из того, что редактор о томе знает, значило бы потерять
    /// остальное.
    pub fn flush(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let free_blocks: u32 = self.free_blocks_in_group.iter().sum();
        let free_inodes: u32 = self.free_inodes_in_group.iter().sum();

        let mut descriptors = try_zeroed(geometry.groups as usize * GROUP_DESC_SIZE)?;
        for group in 0..geometry.groups {
            let at = group as usize * GROUP_DESC_SIZE;
            put_u32(&mut descriptors, at, geometry.block_bitmap_block(group));
            put_u32(&mut descriptors, at + 4, geometry.inode_bitmap_block(group));
            put_u32(&mut descriptors, at + 8, geometry.inode_table_block(group));
            put_u16(&mut descriptors, at + 12, self.free_blocks_in_group[group as usize] as u16);
            put_u16(&mut descriptors, at + 14, self.free_inodes_in_group[group as usize] as u16);
            put_u16(&mut descriptors, at + 16, self.used_dirs_in_group[group as usize]);
        }

        for group in 0..geometry.groups {
            if !geometry.group_has_super(group) {
                continue;
            }
            let block = geometry.group_first_block(group);
            // Смещение суперблока внутри блока ненулевое ровно в одном случае:
            // у нулевой группы при блоке крупнее килобайта, потому что первый
            // килобайт тома отдан под загрузочный сектор.
            let within = if group == 0 && geometry.block_size != BlockSize::B1024 {
                SUPERBLOCK_OFFSET as usize
            } else {
                0
            };
            let mut buf = self.read_block(dev, block)?;
            put_u32(&mut buf, within + 12, free_blocks);
            put_u32(&mut buf, within + 16, free_inodes);
            put_u32(&mut buf, within + 48, self.time);
            self.write_block(dev, block, &buf)?;

            let table_start = block + 1;
            for (index, chunk) in descriptors.chunks(block_bytes).enumerate() {
                let mut buf = try_zeroed(block_bytes)?;
                buf[..chunk.len()].copy_from_slice(chunk);
                self.write_block(dev, table_start + index as u32, &buf)?;
            }
        }
        dev.flush()?;
        Ok(())
    }

    /// Прочитать счётчики групп с диска.
    fn load_group_descriptors(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let table_start = geometry.group_first_block(0) + 1;
        let bytes = geometry.groups as usize * GROUP_DESC_SIZE;

        let mut table = try_zeroed(bytes.next_multiple_of(block_bytes))?;
        for index in 0..(table.len() / block_bytes) {
            let buf = self.read_block(dev, table_start + index as u32)?;
            let at = index * block_bytes;
            table[at..at + block_bytes].copy_from_slice(&buf);
        }

        for group in 0..geometry.groups {
            let at = group as usize * GROUP_DESC_SIZE;
            self.free_blocks_in_group[group as usize] = u32::from(u16_at(&table, at + 12));
            self.free_inodes_in_group[group as usize] = u32::from(u16_at(&table, at + 14));
            self.used_dirs_in_group[group as usize] = u16_at(&table, at + 16);
        }
        Ok(())
    }

    // --- выделение ---------------------------------------------------------

    /// Занять свободный блок.
    ///
    /// Битовая карта читается и пишется на каждом выделении — см. заголовок
    /// модуля о том, почему это не оптимизируется.
    fn alloc_block(&mut self, dev: &mut dyn BlockDevice) -> Result<u32> {
        let geometry = self.geometry;
        // Два прохода: от подсказки до конца тома и от начала до подсказки.
        // Второй нужен после удалений — освободившиеся блоки лежат позади, и
        // без него том «кончался» бы, имея половину свободного места.
        let start = self.block_hint.max(geometry.first_data_block);
        for block in (start..geometry.blocks).chain(geometry.first_data_block..start) {
            let (group, index) = self.locate_block(block);
            if self.take_bit(dev, geometry.block_bitmap_block(group), index)? {
                self.free_blocks_in_group[group as usize] -= 1;
                self.block_hint = block + 1;
                self.zero_block(dev, block)?;
                return Ok(block);
            }
        }
        Err(Error::NoSpace)
    }

    fn free_block(&mut self, dev: &mut dyn BlockDevice, block: u32) -> Result<()> {
        if block < self.geometry.first_data_block || block >= self.geometry.blocks {
            return Err(Error::Corrupt);
        }
        let (group, index) = self.locate_block(block);
        if self.clear_bit(dev, self.geometry.block_bitmap_block(group), index)? {
            self.free_blocks_in_group[group as usize] += 1;
        }
        // Подсказка отводится назад: следующий файл займёт то, что только что
        // освободилось, вместо того чтобы уходить в конец тома.
        self.block_hint = self.block_hint.min(block);
        Ok(())
    }

    fn alloc_inode(&mut self, dev: &mut dyn BlockDevice) -> Result<u32> {
        let total = self.geometry.inodes();
        let start = self.inode_hint.max(FIRST_INODE);
        for number in (start..=total).chain(FIRST_INODE..start) {
            let (group, index) = self.geometry.locate_inode(number)?;
            if self.take_bit(dev, self.geometry.inode_bitmap_block(group), index)? {
                self.free_inodes_in_group[group as usize] -= 1;
                self.inode_hint = number + 1;
                return Ok(number);
            }
        }
        Err(Error::NoInodes)
    }

    fn free_inode(&mut self, dev: &mut dyn BlockDevice, number: u32, is_dir: bool) -> Result<()> {
        let (group, index) = self.geometry.locate_inode(number)?;
        if self.clear_bit(dev, self.geometry.inode_bitmap_block(group), index)? {
            self.free_inodes_in_group[group as usize] += 1;
            if is_dir {
                self.used_dirs_in_group[group as usize] =
                    self.used_dirs_in_group[group as usize].saturating_sub(1);
            }
        }
        self.inode_hint = self.inode_hint.min(number);

        // Освобождённый inode обнуляется, кроме времени удаления: `e2fsck`
        // сверяет `dtime` с битовой картой и жалуется на «deleted inode
        // referenced» либо на нулевое `dtime` у свободного inode.
        let mut inode = InodeData::new(0, 0, 0, self.time);
        inode.deleted = self.time;
        self.write_inode(dev, number, &inode)
    }

    /// Группа и номер бита внутри неё для блока тома.
    const fn locate_block(&self, block: u32) -> (u32, u32) {
        let relative = block - self.geometry.first_data_block;
        (
            relative / self.geometry.blocks_per_group,
            relative % self.geometry.blocks_per_group,
        )
    }

    /// Занять бит. `false` — он уже был занят.
    fn take_bit(&mut self, dev: &mut dyn BlockDevice, bitmap: u32, index: u32) -> Result<bool> {
        let mut buf = self.read_block(dev, bitmap)?;
        let at = (index / 8) as usize;
        let mask = 1u8 << (index % 8);
        if buf[at] & mask != 0 {
            return Ok(false);
        }
        buf[at] |= mask;
        self.write_block(dev, bitmap, &buf)?;
        Ok(true)
    }

    /// Освободить бит. `false` — он уже был свободен, и счётчик трогать нельзя.
    fn clear_bit(&mut self, dev: &mut dyn BlockDevice, bitmap: u32, index: u32) -> Result<bool> {
        let mut buf = self.read_block(dev, bitmap)?;
        let at = (index / 8) as usize;
        let mask = 1u8 << (index % 8);
        if buf[at] & mask == 0 {
            return Ok(false);
        }
        buf[at] &= !mask;
        self.write_block(dev, bitmap, &buf)?;
        Ok(true)
    }

    // --- блоки файла -------------------------------------------------------

    /// Номер `index`-го блока файла, выделяя его и таблицы косвенности.
    fn block_for_write(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
        index: usize,
    ) -> Result<u32> {
        let pointers = self.geometry.block_size.pointers_per_block() as usize;
        let single_limit = DIRECT_BLOCKS + pointers;
        let double_limit = single_limit + pointers * pointers;

        if index < DIRECT_BLOCKS {
            if inode.blocks[index] == 0 {
                inode.blocks[index] = self.alloc_data_block(dev, inode)?;
            }
            return Ok(inode.blocks[index]);
        }
        if index < single_limit {
            let table = self.ensure_table(dev, inode, INDIRECT_INDEX)?;
            return self.ensure_pointer(dev, inode, table, index - DIRECT_BLOCKS);
        }
        if index < double_limit {
            let offset = index - single_limit;
            let outer = self.ensure_table(dev, inode, DOUBLE_INDIRECT_INDEX)?;
            let leaf = self.ensure_leaf(dev, inode, outer, offset / pointers)?;
            return self.ensure_pointer(dev, inode, leaf, offset % pointers);
        }
        // Тройная косвенность нужна файлам больше четырёх гигабайт при блоке
        // 4 КиБ. Такого потребителя нет, а ветка, которую нечем проверить, в
        // разборе указателей опаснее честного отказа.
        Err(Error::Unsupported)
    }

    /// Выделить блок данных, учтя его в `i_blocks`.
    fn alloc_data_block(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
    ) -> Result<u32> {
        let block = self.alloc_block(dev)?;
        inode.sectors += self.geometry.sectors_per_block();
        Ok(block)
    }

    /// Таблица косвенности в самом inode: взять или создать.
    fn ensure_table(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
        slot: usize,
    ) -> Result<u32> {
        if inode.blocks[slot] == 0 {
            // Блок косвенности тоже занимает место, и `i_blocks` обязан его
            // учитывать: `e2fsck` сверяет это поле с фактическим числом блоков.
            inode.blocks[slot] = self.alloc_data_block(dev, inode)?;
        }
        Ok(inode.blocks[slot])
    }

    /// Лист двойной косвенности: взять или создать.
    fn ensure_leaf(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
        table: u32,
        slot: usize,
    ) -> Result<u32> {
        let mut buf = self.read_block(dev, table)?;
        let existing = u32_at(&buf, slot * 4);
        if existing != 0 {
            return Ok(existing);
        }
        let leaf = self.alloc_data_block(dev, inode)?;
        put_u32(&mut buf, slot * 4, leaf);
        self.write_block(dev, table, &buf)?;
        Ok(leaf)
    }

    /// Указатель на блок данных в таблице: взять или создать.
    fn ensure_pointer(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
        table: u32,
        slot: usize,
    ) -> Result<u32> {
        let mut buf = self.read_block(dev, table)?;
        let existing = u32_at(&buf, slot * 4);
        if existing != 0 {
            return Ok(existing);
        }
        let block = self.alloc_data_block(dev, inode)?;
        put_u32(&mut buf, slot * 4, block);
        self.write_block(dev, table, &buf)?;
        Ok(block)
    }

    /// Номер `index`-го блока файла без выделения. `None` — дыра.
    fn block_of(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &InodeData,
        index: usize,
    ) -> Result<Option<u32>> {
        let pointers = self.geometry.block_size.pointers_per_block() as usize;
        let single_limit = DIRECT_BLOCKS + pointers;
        let double_limit = single_limit + pointers * pointers;

        let block = if index < DIRECT_BLOCKS {
            inode.blocks[index]
        } else if index < single_limit {
            self.pointer_at(dev, inode.blocks[INDIRECT_INDEX], index - DIRECT_BLOCKS)?
        } else if index < double_limit {
            let offset = index - single_limit;
            let leaf =
                self.pointer_at(dev, inode.blocks[DOUBLE_INDIRECT_INDEX], offset / pointers)?;
            self.pointer_at(dev, leaf, offset % pointers)?
        } else {
            return Err(Error::Unsupported);
        };
        Ok(if block == 0 { None } else { Some(block) })
    }

    fn pointer_at(&mut self, dev: &mut dyn BlockDevice, table: u32, slot: usize) -> Result<u32> {
        if table == 0 {
            return Ok(0);
        }
        let buf = self.read_block(dev, table)?;
        Ok(u32_at(&buf, slot * 4))
    }

    /// Освободить `index`-й блок файла и, если она опустела, таблицу над ним.
    ///
    /// Вызывается от конца файла к началу — только при таком порядке
    /// опустевшая таблица обнаруживается сразу, а не остаётся висеть занятой.
    fn free_file_block(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
        index: usize,
    ) -> Result<()> {
        let pointers = self.geometry.block_size.pointers_per_block() as usize;
        let single_limit = DIRECT_BLOCKS + pointers;
        let double_limit = single_limit + pointers * pointers;

        if index < DIRECT_BLOCKS {
            let block = inode.blocks[index];
            if block != 0 {
                self.free_block(dev, block)?;
                inode.blocks[index] = 0;
                inode.sectors = inode.sectors.saturating_sub(self.geometry.sectors_per_block());
            }
            return Ok(());
        }

        if index < single_limit {
            let table = inode.blocks[INDIRECT_INDEX];
            let empty = self.free_pointer(dev, inode, table, index - DIRECT_BLOCKS)?;
            if empty {
                self.free_block(dev, table)?;
                inode.blocks[INDIRECT_INDEX] = 0;
                inode.sectors = inode.sectors.saturating_sub(self.geometry.sectors_per_block());
            }
            return Ok(());
        }

        if index < double_limit {
            let outer = inode.blocks[DOUBLE_INDIRECT_INDEX];
            if outer == 0 {
                return Ok(());
            }
            let offset = index - single_limit;
            let leaf = self.pointer_at(dev, outer, offset / pointers)?;
            let leaf_empty = self.free_pointer(dev, inode, leaf, offset % pointers)?;
            if leaf_empty {
                self.free_block(dev, leaf)?;
                inode.sectors = inode.sectors.saturating_sub(self.geometry.sectors_per_block());
                let outer_empty = self.clear_pointer(dev, outer, offset / pointers)?;
                if outer_empty {
                    self.free_block(dev, outer)?;
                    inode.blocks[DOUBLE_INDIRECT_INDEX] = 0;
                    inode.sectors =
                        inode.sectors.saturating_sub(self.geometry.sectors_per_block());
                }
            }
            return Ok(());
        }
        Err(Error::Unsupported)
    }

    /// Освободить блок, на который указывает `slot` таблицы. Возвращает
    /// `true`, если таблица опустела.
    fn free_pointer(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &mut InodeData,
        table: u32,
        slot: usize,
    ) -> Result<bool> {
        if table == 0 {
            return Ok(false);
        }
        let mut buf = self.read_block(dev, table)?;
        let block = u32_at(&buf, slot * 4);
        if block != 0 {
            self.free_block(dev, block)?;
            inode.sectors = inode.sectors.saturating_sub(self.geometry.sectors_per_block());
            put_u32(&mut buf, slot * 4, 0);
            self.write_block(dev, table, &buf)?;
        }
        Ok(buf.chunks(4).all(|word| word == [0, 0, 0, 0]))
    }

    /// Обнулить указатель в таблице. Возвращает `true`, если таблица опустела.
    fn clear_pointer(
        &mut self,
        dev: &mut dyn BlockDevice,
        table: u32,
        slot: usize,
    ) -> Result<bool> {
        let mut buf = self.read_block(dev, table)?;
        put_u32(&mut buf, slot * 4, 0);
        self.write_block(dev, table, &buf)?;
        Ok(buf.chunks(4).all(|word| word == [0, 0, 0, 0]))
    }

    // --- inode -------------------------------------------------------------

    pub(crate) fn read_inode(
        &mut self,
        dev: &mut dyn BlockDevice,
        number: u32,
    ) -> Result<InodeData> {
        let (block, within) = self.inode_place(number)?;
        let buf = self.read_block(dev, block)?;
        Ok(InodeData::decode(&buf[within..within + INODE_SIZE]))
    }

    pub(crate) fn write_inode(
        &mut self,
        dev: &mut dyn BlockDevice,
        number: u32,
        inode: &InodeData,
    ) -> Result<()> {
        let (block, within) = self.inode_place(number)?;
        let mut buf = self.read_block(dev, block)?;
        inode.encode(&mut buf[within..within + INODE_SIZE]);
        self.write_block(dev, block, &buf)
    }

    /// Блок таблицы inode и смещение записи в нём.
    fn inode_place(&self, number: u32) -> Result<(u32, usize)> {
        let (group, index) = self.geometry.locate_inode(number)?;
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let byte_offset = index as usize * INODE_SIZE;
        let block = self.geometry.inode_table_block(group) + (byte_offset / block_bytes) as u32;
        Ok((block, byte_offset % block_bytes))
    }

    /// Изменить счётчик ссылок каталога на `delta`.
    fn bump_links(&mut self, dev: &mut dyn BlockDevice, number: u32, delta: i32) -> Result<()> {
        let mut inode = self.read_inode(dev, number)?;
        inode.links = if delta < 0 {
            inode.links.saturating_sub(delta.unsigned_abs() as u16)
        } else {
            inode.links + delta as u16
        };
        self.write_inode(dev, number, &inode)
    }

    // --- записи каталога ---------------------------------------------------

    fn find_entry(
        &mut self,
        dev: &mut dyn BlockDevice,
        dir: &InodeData,
        name: &str,
    ) -> Result<Option<(u32, u8)>> {
        if dir.mode & 0xF000 != MODE_DIRECTORY {
            return Err(Error::NotADirectory);
        }
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let blocks = (dir.size as usize).div_ceil(block_bytes);

        for index in 0..blocks {
            let block = self.directory_block(dev, dir, index)?;
            if block == 0 {
                continue;
            }
            let buf = self.read_block(dev, block)?;
            let mut at = 0usize;
            while at + 8 <= buf.len() {
                let entry = u32_at(&buf, at);
                let rec_len = u16_at(&buf, at + 4) as usize;
                if rec_len < 8 || at + rec_len > buf.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = buf[at + 6] as usize;
                if entry != 0
                    && name_len == name.len()
                    && at + 8 + name_len <= buf.len()
                    && &buf[at + 8..at + 8 + name_len] == name.as_bytes()
                {
                    return Ok(Some((entry, buf[at + 7])));
                }
                at += rec_len;
            }
        }
        Ok(None)
    }

    /// Номер `index`-го блока каталога.
    ///
    /// Каталоги здесь короткие, но не обязаны помещаться в двенадцать прямых
    /// блоков: при блоке в 1024 байта это всего 12 КиБ, то есть несколько сотен
    /// имён.
    fn directory_block(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &InodeData,
        index: usize,
    ) -> Result<u32> {
        if index < DIRECT_BLOCKS {
            return Ok(inode.blocks[index]);
        }
        let pointers = self.geometry.block_size.pointers_per_block() as usize;
        let slot = index - DIRECT_BLOCKS;
        if slot >= pointers {
            return Err(Error::Unsupported);
        }
        self.pointer_at(dev, inode.blocks[INDIRECT_INDEX], slot)
    }

    /// Заполнить свежий блок каталога записями «.» и «..».
    fn init_directory_block(
        &mut self,
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

    /// Добавить запись в каталог, при необходимости выделив ему ещё блок.
    fn add_entry(
        &mut self,
        dev: &mut dyn BlockDevice,
        dir: u32,
        name: &str,
        target: u32,
        file_type: u8,
    ) -> Result<()> {
        let mut inode = self.read_inode(dev, dir)?;
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let blocks = (inode.size as usize).div_ceil(block_bytes);
        // Запись выравнивается по четыре байта: `rec_len` кратен четырём во
        // всех существующих реализациях, и невыровненная запись ломает разбор
        // у тех, кто на это рассчитывает.
        let needed = (8 + name.len()).next_multiple_of(4);

        for index in 0..blocks {
            let block = self.directory_block(dev, &inode, index)?;
            if block == 0 {
                continue;
            }
            let mut buf = self.read_block(dev, block)?;
            let mut at = 0usize;
            while at + 8 <= buf.len() {
                let entry = u32_at(&buf, at);
                let rec_len = u16_at(&buf, at + 4) as usize;
                if rec_len < 8 || at + rec_len > buf.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = buf[at + 6] as usize;
                // Фактически занятое записью место: остальное — «хвост»,
                // который она держит про запас. Именно в него и вставляются
                // новые записи; свободного места между записями не бывает.
                let used = if entry == 0 { 0 } else { (8 + name_len).next_multiple_of(4) };
                if rec_len - used >= needed {
                    if used == 0 {
                        write_entry(&mut buf, at, target, rec_len, name, file_type);
                    } else {
                        put_u16(&mut buf, at + 4, used as u16);
                        write_entry(&mut buf, at + used, target, rec_len - used, name, file_type);
                    }
                    self.write_block(dev, block, &buf)?;
                    return Ok(());
                }
                at += rec_len;
            }
        }

        // Места не нашлось — каталогу нужен ещё блок.
        if blocks >= DIRECT_BLOCKS + self.geometry.block_size.pointers_per_block() as usize {
            return Err(Error::Unsupported);
        }
        let block = self.block_for_write(dev, &mut inode, blocks)?;
        let mut buf = try_zeroed(block_bytes)?;
        write_entry(&mut buf, 0, target, block_bytes, name, file_type);
        self.write_block(dev, block, &buf)?;
        inode.size += block_bytes as u64;
        self.write_inode(dev, dir, &inode)
    }

    /// Убрать запись из каталога.
    ///
    /// Запись не стирается, а поглощается предыдущей: её `rec_len` растёт на
    /// длину удаляемой. Так это делает и Linux, и причина в формате — записи
    /// идут подряд без промежутков, и «дыра» в середине блока выражается
    /// только через удлинение соседа. У первой записи блока соседа нет, и ей
    /// обнуляется номер inode.
    fn remove_entry(&mut self, dev: &mut dyn BlockDevice, dir: u32, name: &str) -> Result<()> {
        let inode = self.read_inode(dev, dir)?;
        let block_bytes = self.geometry.block_size.bytes() as usize;
        let blocks = (inode.size as usize).div_ceil(block_bytes);

        for index in 0..blocks {
            let block = self.directory_block(dev, &inode, index)?;
            if block == 0 {
                continue;
            }
            let mut buf = self.read_block(dev, block)?;
            let mut at = 0usize;
            let mut previous: Option<usize> = None;
            while at + 8 <= buf.len() {
                let entry = u32_at(&buf, at);
                let rec_len = u16_at(&buf, at + 4) as usize;
                if rec_len < 8 || at + rec_len > buf.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = buf[at + 6] as usize;
                if entry != 0
                    && name_len == name.len()
                    && at + 8 + name_len <= buf.len()
                    && &buf[at + 8..at + 8 + name_len] == name.as_bytes()
                {
                    match previous {
                        Some(before) => {
                            let grown = u16_at(&buf, before + 4) as usize + rec_len;
                            put_u16(&mut buf, before + 4, grown as u16);
                        }
                        None => put_u32(&mut buf, at, 0),
                    }
                    self.write_block(dev, block, &buf)?;
                    return Ok(());
                }
                previous = Some(at);
                at += rec_len;
            }
        }
        Err(Error::NotFound)
    }

    // --- носитель ----------------------------------------------------------

    fn read_block(&self, dev: &mut dyn BlockDevice, block: u32) -> Result<Vec<u8>> {
        if block >= self.geometry.blocks {
            return Err(Error::Corrupt);
        }
        let mut buf = try_zeroed(self.geometry.block_size.bytes() as usize)?;
        dev.read(self.geometry.block_lba(block), &mut buf)?;
        Ok(buf)
    }

    fn write_block(&self, dev: &mut dyn BlockDevice, block: u32, data: &[u8]) -> Result<()> {
        if block >= self.geometry.blocks {
            return Err(Error::Corrupt);
        }
        debug_assert_eq!(data.len(), self.geometry.block_size.bytes() as usize);
        dev.write(self.geometry.block_lba(block), data)?;
        Ok(())
    }

    /// Обнулить свежевыделенный блок.
    ///
    /// Обязательно, а не для порядка: в блоке лежит то, что было до него, и
    /// хвост файла за его размером иначе показал бы чужие данные — в том числе
    /// удалённого файла другого пользователя.
    fn zero_block(&self, dev: &mut dyn BlockDevice, block: u32) -> Result<()> {
        let zeros = try_zeroed(self.geometry.block_size.bytes() as usize)?;
        self.write_block(dev, block, &zeros)
    }
}
