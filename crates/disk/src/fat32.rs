//! Форматирование FAT32 и запись в свежий том.
//!
//! Реализация намеренно однобокая: она умеет **создавать** том и класть в него
//! файлы, но не умеет ни удалять, ни дописывать, ни искать свободное место.
//! Это не упрощение ради экономии — это ровно та задача, которая здесь стоит.
//! Установщик пишет на только что отформатированный раздел, поэтому:
//!
//! * свободные кластеры идут подряд от второго — искать их не нужно, достаточно
//!   курсора [`Volume::next_free`];
//! * файл всегда занимает непрерывный отрезок — цепочка FAT получается
//!   тривиальной, а данные пишутся одним последовательным проходом;
//! * фрагментации не бывает, поэтому не бывает и кода, который её разбирает.
//!
//! Полноценный драйвер FAT в ядре уже есть — на чтение (`kernel::fs::fat`).
//! Второй такой же, но на запись, ради установки не нужен, а неиспользуемый код
//! в разметке диска опаснее отсутствующего: проверить его нечем.
//!
//! # Длинных имён нет
//!
//! Пишутся только имена 8.3. Всё, что установщик кладёт на ESP, в 8.3
//! помещается (`BOOTX64.EFI`, `kernel.elf`, `initrd.img`), а цепочки LFN — это
//! отдельный формат с контрольной суммой, кодировкой UCS-2 и собственными
//! правилами усечения. Имя, которое в 8.3 не влезает, отвергается ошибкой
//! [`Error::BadName`] — молча укоротить его значит записать не тот файл,
//! который просили.
//!
//! Регистр при этом не теряется: для полностью строчных имени и расширения
//! выставляются флаги регистра (байт 12 записи каталога), и `kernel.elf`
//! остаётся `kernel.elf`, а не превращается в `KERNEL.ELF`.

use crate::gpt::Range;
use crate::{
    BlockDevice, Error, Result, SECTOR_SIZE, check_device, put_u16, put_u32, zero_sectors,
};

/// Зарезервированных секторов в начале тома.
///
/// 32 — то, что ставит Windows и ожидает увидеть любой инструмент. Формально
/// хватило бы и 3 (загрузочный сектор, FSInfo и резервная копия), но резервная
/// копия по спецификации живёт на секторе 6, и запас до 32 оставляет место под
/// загрузочный код, который на ESP кто-нибудь ещё может дописать.
const RESERVED_SECTORS: u32 = 32;

/// Копий таблицы FAT. Две — стандарт; одна означала бы, что единственный
/// сбойный сектор уносит с собой весь том.
const FAT_COUNT: u32 = 2;

/// Сектор, на котором лежит резервная копия загрузочного сектора.
const BACKUP_BOOT_SECTOR: u32 = 6;

/// Первый кластер данных. У FAT32 корневой каталог — обычная цепочка
/// кластеров, и начинается она здесь.
const ROOT_CLUSTER: u32 = 2;

/// Записей FAT в одном секторе: 512 / 4.
const FAT_ENTRIES_PER_SECTOR: u32 = SECTOR_SIZE as u32 / 4;

/// Конец цепочки кластеров.
const END_OF_CHAIN: u32 = 0x0FFF_FFFF;

/// Значащая часть записи FAT32: старшие четыре бита зарезервированы и обязаны
/// сохраняться при записи.
const ENTRY_MASK: u32 = 0x0FFF_FFFF;

/// Минимальное число кластеров тома FAT32.
///
/// Тип FAT определяется исключительно числом кластеров (Microsoft FAT
/// specification, «FAT Type Determination»): меньше 65525 — и том обязан
/// считаться FAT16, каким бы ни был его BPB. Драйвер, честно следующий
/// спецификации, такой «FAT32» просто не прочитает.
const MIN_CLUSTERS: u32 = 65525;

/// Размер записи каталога.
const DIR_ENTRY_SIZE: usize = 32;

/// Атрибуты записи каталога.
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
/// Признак части длинного имени: все четыре младших бита сразу.
const ATTR_LONG_NAME: u8 = 0x0F;

/// Флаги регистра в байте 12 записи каталога.
const CASE_BASE_LOWER: u8 = 0x08;
const CASE_EXT_LOWER: u8 = 0x10;

/// Сколько секторов писать за один вызов носителя.
///
/// 32 КиБ — достаточно, чтобы запись 40-мегабайтного образа не превратилась в
/// восемьдесят тысяч отдельных обращений к диску, и достаточно мало, чтобы
/// буфер не пришлось выделять: данные пишутся прямо из среза вызывающего.
const MAX_BATCH_SECTORS: usize = 64;

/// Дата и время для записей каталога.
///
/// Отдельный тип, а не «взять текущее время», по двум причинам. В UEFI часы
/// доступны только через runtime-сервисы, которых у крейта нет и быть не
/// должно. А на хосте фиксированная метка делает образ побайтово
/// воспроизводимым — то же решение и по той же причине, что в сборщике initrd.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timestamp {
    date: u16,
    time: u16,
}

impl Timestamp {
    /// 1980-01-01 00:00 — начало эпохи FAT и минимальная представимая дата.
    pub const EPOCH: Self = Self { date: 0x0021, time: 0 };

    /// Собрать метку. Значения вне допустимого диапазона обрезаются: метка
    /// времени не тот повод, из-за которого установка должна прерваться.
    #[must_use]
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        let year = year.clamp(1980, 2107) - 1980;
        let month = u16::from(month.clamp(1, 12));
        let day = u16::from(day.clamp(1, 31));
        let hour = u16::from(hour.min(23));
        let minute = u16::from(minute.min(59));
        // В FAT секунды хранятся с шагом в две.
        let second = u16::from(second.min(59)) / 2;
        Self {
            date: (year << 9) | (month << 5) | day,
            time: (hour << 11) | (minute << 5) | second,
        }
    }
}

/// Геометрия тома — всё, что вычисляется из его размера.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Первый сектор тома на носителе.
    pub first_lba: u64,
    pub total_sectors: u32,
    pub sectors_per_cluster: u32,
    pub sectors_per_fat: u32,
    /// Число кластеров данных.
    pub cluster_count: u32,
}

impl Geometry {
    /// Рассчитать геометрию тома длиной `total_sectors`.
    pub fn plan(first_lba: u64, total_sectors: u64) -> Result<Self> {
        let total_sectors = u32::try_from(total_sectors).map_err(|_| Error::OutOfRange)?;
        if total_sectors <= RESERVED_SECTORS {
            return Err(Error::TooSmall);
        }

        // Кластер берём настолько крупным, насколько позволяет нижняя граница
        // в 65525 кластеров: крупный кластер уменьшает таблицу FAT (а её надо
        // и записать, и прочитать), мелкий — потери на хвостах файлов. Порядок
        // перебора от крупного к мелкому означает «самый дешёвый из
        // допустимых».
        for &sectors_per_cluster in &[64u32, 32, 16, 8, 4, 2, 1] {
            let Some(candidate) = Self::try_geometry(first_lba, total_sectors, sectors_per_cluster)
            else {
                continue;
            };
            if candidate.cluster_count >= MIN_CLUSTERS {
                return Ok(candidate);
            }
        }
        Err(Error::TooSmall)
    }

    fn try_geometry(first_lba: u64, total_sectors: u32, sectors_per_cluster: u32) -> Option<Self> {
        // Формула из Microsoft FAT specification («FAT Size Determination»).
        // Она даёт лёгкий перебор в размере таблицы — это допустимо и
        // безопасно; недобор означал бы, что последним кластерам не хватило
        // записей.
        let tmp1 = total_sectors.checked_sub(RESERVED_SECTORS)?;
        let tmp2 = ((SECTOR_SIZE as u32 / 2) * sectors_per_cluster + FAT_COUNT) / 2;
        let sectors_per_fat = tmp1.div_ceil(tmp2.max(1));

        let overhead = RESERVED_SECTORS.checked_add(FAT_COUNT.checked_mul(sectors_per_fat)?)?;
        let data_sectors = total_sectors.checked_sub(overhead)?;
        let cluster_count = data_sectors / sectors_per_cluster;
        if cluster_count == 0 {
            return None;
        }

        // Таблица обязана вмещать записи для всех кластеров плюс две служебные.
        if sectors_per_fat.checked_mul(FAT_ENTRIES_PER_SECTOR)? < cluster_count + 2 {
            return None;
        }

        Some(Self {
            first_lba,
            total_sectors,
            sectors_per_cluster,
            sectors_per_fat,
            cluster_count,
        })
    }

    /// Первый сектор копии таблицы FAT с номером `index`.
    #[must_use]
    pub const fn fat_lba(&self, index: u32) -> u64 {
        self.first_lba + (RESERVED_SECTORS + index * self.sectors_per_fat) as u64
    }

    /// Первый сектор области данных.
    #[must_use]
    pub const fn data_lba(&self) -> u64 {
        self.first_lba + (RESERVED_SECTORS + FAT_COUNT * self.sectors_per_fat) as u64
    }

    /// Первый сектор кластера.
    ///
    /// Умножение идёт в 64 битах намеренно: у тома на терабайт номер кластера,
    /// помноженный на секторы в кластере, из `u32` выходит, и переполнение
    /// вернуло бы адрес в начале носителя — то есть запись файла поверх
    /// таблицы разделов.
    #[must_use]
    pub const fn cluster_lba(&self, cluster: u32) -> u64 {
        self.data_lba() + (cluster - ROOT_CLUSTER) as u64 * self.sectors_per_cluster as u64
    }

    /// Наибольший существующий номер кластера.
    #[must_use]
    pub const fn max_cluster(&self) -> u32 {
        self.cluster_count + 1
    }

    #[must_use]
    pub const fn cluster_bytes(&self) -> u32 {
        self.sectors_per_cluster * SECTOR_SIZE as u32
    }
}

/// Параметры форматирования.
pub struct FormatOptions<'a> {
    /// Метка тома: до 11 символов, приводится к верхнему регистру.
    pub label: &'a str,
    /// Серийный номер тома. Уникальным быть не обязан — он лишь помогает
    /// заметить подмену носителя; на хосте его удобно выводить из содержимого.
    pub volume_id: u32,
    pub timestamp: Timestamp,
}

/// Свежеотформатированный том, открытый на запись.
pub struct Volume {
    geometry: Geometry,
    /// Курсор выделения. Кластеры раздаются подряд — см. заголовок модуля.
    next_free: u32,
    free_clusters: u32,
    timestamp: Timestamp,
}

/// Отформатировать раздел под FAT32 и открыть его на запись.
pub fn format(dev: &mut dyn BlockDevice, range: Range, options: &FormatOptions) -> Result<Volume> {
    check_device(dev)?;
    if range.last_lba >= dev.sector_count() || range.first_lba > range.last_lba {
        return Err(Error::OutOfRange);
    }

    let geometry = Geometry::plan(range.first_lba, range.sectors())?;

    // Таблицы FAT и зарезервированная область обнуляются целиком: в них
    // значение по умолчанию — ноль («кластер свободен»), а на носителе после
    // прежней установки лежит что угодно. Область данных не трогаем — она
    // многократно больше, а её содержимое за пределами файлов никого не
    // интересует.
    zero_sectors(
        dev,
        geometry.first_lba,
        u64::from(RESERVED_SECTORS + FAT_COUNT * geometry.sectors_per_fat),
    )?;

    let boot = boot_sector(&geometry, options);
    dev.write(geometry.first_lba, &boot)?;
    dev.write(geometry.first_lba + u64::from(BACKUP_BOOT_SECTOR), &boot)?;

    let fsinfo = fsinfo_sector(geometry.cluster_count - 1, ROOT_CLUSTER + 1);
    dev.write(geometry.first_lba + 1, &fsinfo)?;
    dev.write(geometry.first_lba + u64::from(BACKUP_BOOT_SECTOR) + 1, &fsinfo)?;

    // Первые две записи FAT служебные: нулевая повторяет байт носителя, первая
    // хранит признаки состояния тома. Третья — корневой каталог, он уже занят.
    let mut first_fat = [0u8; SECTOR_SIZE];
    put_u32(&mut first_fat, 0, 0x0FFF_FF00 | 0xF8);
    put_u32(&mut first_fat, 4, END_OF_CHAIN);
    put_u32(&mut first_fat, 8, END_OF_CHAIN);
    for index in 0..FAT_COUNT {
        dev.write(geometry.fat_lba(index), &first_fat)?;
    }

    // Кластер корневого каталога обязан быть нулевым: ненулевой первый байт
    // записи означает «здесь файл», и мусор от прежнего тома превратился бы в
    // каталог из бессмысленных имён.
    zero_sectors(
        dev,
        geometry.cluster_lba(ROOT_CLUSTER),
        u64::from(geometry.sectors_per_cluster),
    )?;

    let mut volume = Volume {
        geometry,
        next_free: ROOT_CLUSTER + 1,
        free_clusters: geometry.cluster_count - 1,
        timestamp: options.timestamp,
    };

    // Метка тома живёт не только в BPB, но и записью в корневом каталоге —
    // именно её показывают файловые менеджеры. Без неё том выглядит
    // безымянным, хотя метка в BPB стоит.
    if !options.label.is_empty() {
        let mut entry = [0u8; DIR_ENTRY_SIZE];
        entry[..11].copy_from_slice(&label_bytes(options.label));
        entry[11] = ATTR_VOLUME_ID;
        volume.stamp(&mut entry);
        volume.dir_add_entry(dev, ROOT_CLUSTER, &entry)?;
    }

    Ok(volume)
}

impl Volume {
    #[must_use]
    pub const fn geometry(&self) -> Geometry {
        self.geometry
    }

    /// Кластер корневого каталога.
    #[must_use]
    pub const fn root(&self) -> u32 {
        ROOT_CLUSTER
    }

    /// Сколько байт ещё можно записать.
    ///
    /// Оценка сверху: каждый файл теряет остаток последнего кластера, каждый
    /// новый каталог занимает кластер целиком.
    #[must_use]
    pub const fn free_bytes(&self) -> u64 {
        self.free_clusters as u64 * self.geometry.cluster_bytes() as u64
    }

    /// Создать каталог по пути вида `EFI/BOOT`, создавая недостающие звенья.
    ///
    /// Возвращает кластер последнего каталога в пути.
    pub fn create_dir_path(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u32> {
        let mut dir = ROOT_CLUSTER;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            dir = self.ensure_dir(dev, dir, component)?;
        }
        Ok(dir)
    }

    /// Записать файл по пути вида `EFI/BOOT/BOOTX64.EFI`.
    ///
    /// Промежуточные каталоги создаются при необходимости.
    pub fn write_file_path(
        &mut self,
        dev: &mut dyn BlockDevice,
        path: &str,
        data: &[u8],
    ) -> Result<()> {
        let (parent, name) = match path.rsplit_once('/') {
            Some((parent, name)) => (self.create_dir_path(dev, parent)?, name),
            None => (ROOT_CLUSTER, path),
        };
        self.create_file(dev, parent, name, data)
    }

    /// Создать файл в каталоге `parent`.
    pub fn create_file(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u32,
        name: &str,
        data: &[u8],
    ) -> Result<()> {
        let (short, case) = short_name(name)?;
        if self.dir_find(dev, parent, &short)?.is_some() {
            // Перезапись потребовала бы освобождения прежней цепочки, то есть
            // ровно того кода, которого здесь нет по замыслу. На свежем томе
            // повторное имя означает ошибку вызывающего.
            return Err(Error::BadName);
        }

        let cluster_bytes = self.geometry.cluster_bytes() as usize;
        let clusters = u32::try_from(data.len().div_ceil(cluster_bytes.max(1)))
            .map_err(|_| Error::NoSpace)?;
        // Пустой файл не занимает ни одного кластера, и его первый кластер — 0.
        let first = if clusters == 0 {
            0
        } else {
            let first = self.allocate_run(dev, clusters)?;
            self.write_data(dev, first, data)?;
            first
        };

        let mut entry = [0u8; DIR_ENTRY_SIZE];
        entry[..11].copy_from_slice(&short);
        entry[11] = ATTR_ARCHIVE;
        entry[12] = case;
        self.stamp(&mut entry);
        put_u16(&mut entry, 20, (first >> 16) as u16);
        put_u16(&mut entry, 26, (first & 0xFFFF) as u16);
        put_u32(
            &mut entry,
            28,
            u32::try_from(data.len()).map_err(|_| Error::NoSpace)?,
        );
        self.dir_add_entry(dev, parent, &entry)
    }

    /// Найти подкаталог или создать его.
    pub fn ensure_dir(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u32,
        name: &str,
    ) -> Result<u32> {
        let (short, case) = short_name(name)?;
        if let Some(found) = self.dir_find(dev, parent, &short)? {
            if found.attributes & ATTR_DIRECTORY == 0 {
                return Err(Error::NotADirectory);
            }
            return Ok(found.cluster);
        }

        let cluster = self.allocate_run(dev, 1)?;
        zero_sectors(
            dev,
            self.geometry.cluster_lba(cluster),
            u64::from(self.geometry.sectors_per_cluster),
        )?;

        // «.» и «..» — обязательные первые две записи подкаталога. У «..»,
        // указывающего на корень, номер кластера обязан быть нулём: корень у
        // FAT32 хоть и живёт в кластере, но снаружи адресуется нулём
        // (Microsoft FAT specification, описание DIR_FstClusLO). Ровно так
        // размечает тома Windows, и ровно это должен уметь разбирать драйвер.
        let parent_link = if parent == ROOT_CLUSTER { 0 } else { parent };
        let mut dot = [0u8; DIR_ENTRY_SIZE];
        dot[..11].copy_from_slice(b".          ");
        dot[11] = ATTR_DIRECTORY;
        self.stamp(&mut dot);
        put_u16(&mut dot, 20, (cluster >> 16) as u16);
        put_u16(&mut dot, 26, (cluster & 0xFFFF) as u16);

        let mut dotdot = [0u8; DIR_ENTRY_SIZE];
        dotdot[..11].copy_from_slice(b"..         ");
        dotdot[11] = ATTR_DIRECTORY;
        self.stamp(&mut dotdot);
        put_u16(&mut dotdot, 20, (parent_link >> 16) as u16);
        put_u16(&mut dotdot, 26, (parent_link & 0xFFFF) as u16);

        let mut sector = [0u8; SECTOR_SIZE];
        sector[..DIR_ENTRY_SIZE].copy_from_slice(&dot);
        sector[DIR_ENTRY_SIZE..2 * DIR_ENTRY_SIZE].copy_from_slice(&dotdot);
        dev.write(self.geometry.cluster_lba(cluster), &sector)?;

        let mut entry = [0u8; DIR_ENTRY_SIZE];
        entry[..11].copy_from_slice(&short);
        entry[11] = ATTR_DIRECTORY;
        entry[12] = case;
        self.stamp(&mut entry);
        put_u16(&mut entry, 20, (cluster >> 16) as u16);
        put_u16(&mut entry, 26, (cluster & 0xFFFF) as u16);
        // Размер каталога в записи всегда нулевой — это требование формата, а
        // не упущение: длину каталога задаёт его цепочка кластеров.
        self.dir_add_entry(dev, parent, &entry)?;

        Ok(cluster)
    }

    /// Дописать в FSInfo фактические счётчики и сбросить всё на носитель.
    ///
    /// Вызывать обязательно: FSInfo с неверным числом свободных кластеров —
    /// не фатальная, но настоящая порча тома, и `chkdsk` о ней сообщит.
    pub fn finish(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let next = if self.next_free > self.geometry.max_cluster() {
            // Свободных не осталось; спецификация велит писать 0xFFFFFFFF.
            0xFFFF_FFFF
        } else {
            self.next_free
        };
        let fsinfo = fsinfo_sector(self.free_clusters, next);
        dev.write(self.geometry.first_lba + 1, &fsinfo)?;
        dev.write(
            self.geometry.first_lba + u64::from(BACKUP_BOOT_SECTOR) + 1,
            &fsinfo,
        )?;
        dev.flush()
    }

    /// Проставить в записи каталога метки времени создания, изменения и доступа.
    fn stamp(&self, entry: &mut [u8; DIR_ENTRY_SIZE]) {
        put_u16(entry, 14, self.timestamp.time);
        put_u16(entry, 16, self.timestamp.date);
        put_u16(entry, 18, self.timestamp.date);
        put_u16(entry, 22, self.timestamp.time);
        put_u16(entry, 24, self.timestamp.date);
    }

    /// Выделить `count` идущих подряд кластеров и связать их в цепочку.
    fn allocate_run(&mut self, dev: &mut dyn BlockDevice, count: u32) -> Result<u32> {
        if count == 0 {
            return Err(Error::NoSpace);
        }
        let first = self.next_free;
        let last = first
            .checked_add(count - 1)
            .filter(|last| *last <= self.geometry.max_cluster())
            .ok_or(Error::NoSpace)?;

        // Записи FAT правятся посекторно, а не по одной: цепочка из десяти
        // тысяч кластеров (сорокамегабайтный initrd) иначе превратилась бы в
        // десятки тысяч отдельных чтений-записей носителя.
        let mut cluster = first;
        while cluster <= last {
            let sector_index = cluster / FAT_ENTRIES_PER_SECTOR;
            let sector_last =
                ((sector_index + 1) * FAT_ENTRIES_PER_SECTOR - 1).min(last);

            let mut buf = [0u8; SECTOR_SIZE];
            // Сектор читается, а не собирается с нуля: в него могли попасть
            // записи ранее выделенных кластеров.
            dev.read(self.geometry.fat_lba(0) + u64::from(sector_index), &mut buf)?;
            for current in cluster..=sector_last {
                let value = if current == last { END_OF_CHAIN } else { current + 1 };
                let at = ((current % FAT_ENTRIES_PER_SECTOR) * 4) as usize;
                let reserved = u32::from_le_bytes([
                    buf[at],
                    buf[at + 1],
                    buf[at + 2],
                    buf[at + 3],
                ]) & !ENTRY_MASK;
                put_u32(&mut buf, at, reserved | (value & ENTRY_MASK));
            }
            for index in 0..FAT_COUNT {
                dev.write(self.geometry.fat_lba(index) + u64::from(sector_index), &buf)?;
            }

            cluster = sector_last + 1;
        }

        self.next_free = last + 1;
        self.free_clusters -= count;
        Ok(first)
    }

    /// Записать одну запись FAT во все копии таблицы.
    fn fat_set(&mut self, dev: &mut dyn BlockDevice, cluster: u32, value: u32) -> Result<()> {
        if cluster < ROOT_CLUSTER || cluster > self.geometry.max_cluster() {
            return Err(Error::OutOfRange);
        }
        let sector_index = cluster / FAT_ENTRIES_PER_SECTOR;
        let at = ((cluster % FAT_ENTRIES_PER_SECTOR) * 4) as usize;

        let mut buf = [0u8; SECTOR_SIZE];
        dev.read(self.geometry.fat_lba(0) + u64::from(sector_index), &mut buf)?;
        let reserved =
            u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) & !ENTRY_MASK;
        put_u32(&mut buf, at, reserved | (value & ENTRY_MASK));
        for index in 0..FAT_COUNT {
            dev.write(self.geometry.fat_lba(index) + u64::from(sector_index), &buf)?;
        }
        Ok(())
    }

    /// Прочитать следующий кластер цепочки.
    fn fat_get(&mut self, dev: &mut dyn BlockDevice, cluster: u32) -> Result<u32> {
        if cluster < ROOT_CLUSTER || cluster > self.geometry.max_cluster() {
            return Err(Error::OutOfRange);
        }
        let sector_index = cluster / FAT_ENTRIES_PER_SECTOR;
        let at = ((cluster % FAT_ENTRIES_PER_SECTOR) * 4) as usize;
        let mut buf = [0u8; SECTOR_SIZE];
        dev.read(self.geometry.fat_lba(0) + u64::from(sector_index), &mut buf)?;
        Ok(u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) & ENTRY_MASK)
    }

    /// Записать данные файла, начиная с кластера `first`.
    fn write_data(&mut self, dev: &mut dyn BlockDevice, first: u32, data: &[u8]) -> Result<()> {
        // Кластеры файла идут подряд, поэтому весь файл — это один
        // последовательный отрезок секторов, и цепочку при записи разбирать не
        // нужно.
        let start = self.geometry.cluster_lba(first);

        let full_sectors = data.len() / SECTOR_SIZE;
        let mut done = 0usize;
        while done < full_sectors {
            let batch = (full_sectors - done).min(MAX_BATCH_SECTORS);
            dev.write(
                start + done as u64,
                &data[done * SECTOR_SIZE..(done + batch) * SECTOR_SIZE],
            )?;
            done += batch;
        }

        // Хвост короче сектора дописывается через буфер: носитель принимает
        // только целые сектора.
        let rest = data.len() % SECTOR_SIZE;
        if rest != 0 {
            let mut tail = [0u8; SECTOR_SIZE];
            tail[..rest].copy_from_slice(&data[full_sectors * SECTOR_SIZE..]);
            dev.write(start + full_sectors as u64, &tail)?;
        }
        Ok(())
    }

    /// Добавить запись в каталог, при необходимости удлинив его цепочку.
    fn dir_add_entry(
        &mut self,
        dev: &mut dyn BlockDevice,
        dir: u32,
        entry: &[u8; DIR_ENTRY_SIZE],
    ) -> Result<()> {
        let mut cluster = dir;
        loop {
            let base = self.geometry.cluster_lba(cluster);
            for sector in 0..self.geometry.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                dev.read(base + u64::from(sector), &mut buf)?;
                for slot in 0..(SECTOR_SIZE / DIR_ENTRY_SIZE) {
                    let at = slot * DIR_ENTRY_SIZE;
                    // 0x00 — «дальше записей нет», 0xE5 — «запись удалена».
                    if buf[at] != 0x00 && buf[at] != 0xE5 {
                        continue;
                    }
                    buf[at..at + DIR_ENTRY_SIZE].copy_from_slice(entry);
                    dev.write(base + u64::from(sector), &buf)?;
                    return Ok(());
                }
            }

            let next = self.fat_get(dev, cluster)?;
            if next >= 2 && next <= self.geometry.max_cluster() {
                cluster = next;
                continue;
            }

            // Свободных мест не осталось — каталогу нужен ещё один кластер.
            let fresh = self.allocate_run(dev, 1)?;
            zero_sectors(
                dev,
                self.geometry.cluster_lba(fresh),
                u64::from(self.geometry.sectors_per_cluster),
            )?;
            // Связывание идёт последним: до этого момента новый кластер для
            // цепочки не существует, и обрыв записи оставит каталог целым,
            // потеряв лишь один кластер.
            self.fat_set(dev, cluster, fresh)?;
            cluster = fresh;
        }
    }

    /// Найти запись по имени 8.3.
    fn dir_find(
        &mut self,
        dev: &mut dyn BlockDevice,
        dir: u32,
        short: &[u8; 11],
    ) -> Result<Option<Found>> {
        let mut cluster = dir;
        loop {
            let base = self.geometry.cluster_lba(cluster);
            for sector in 0..self.geometry.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                dev.read(base + u64::from(sector), &mut buf)?;
                for slot in 0..(SECTOR_SIZE / DIR_ENTRY_SIZE) {
                    let at = slot * DIR_ENTRY_SIZE;
                    let entry = &buf[at..at + DIR_ENTRY_SIZE];
                    if entry[0] == 0x00 {
                        // Конец каталога: дальше только нули.
                        return Ok(None);
                    }
                    if entry[0] == 0xE5 || entry[11] & ATTR_LONG_NAME == ATTR_LONG_NAME {
                        continue;
                    }
                    // Метка тома лежит в корневом каталоге такой же записью, как
                    // файл, и по имени совпадает с ним ровно так же. Пропускать
                    // её обязательно: том с меткой `FREEOS` иначе не даёт
                    // создать каталог `FREEOS`, и отказ выглядит бессмысленно —
                    // «компонент пути существует, но не каталог» про имя,
                    // которого в каталоге не видно. Ровно на этом и споткнулась
                    // сборка установочного ISO.
                    if entry[11] & ATTR_VOLUME_ID != 0 {
                        continue;
                    }
                    if &entry[..11] != short {
                        continue;
                    }
                    let high = u32::from(u16::from_le_bytes([entry[20], entry[21]]));
                    let low = u32::from(u16::from_le_bytes([entry[26], entry[27]]));
                    return Ok(Some(Found {
                        cluster: (high << 16) | low,
                        attributes: entry[11],
                    }));
                }
            }

            let next = self.fat_get(dev, cluster)?;
            if next >= 2 && next <= self.geometry.max_cluster() {
                cluster = next;
            } else {
                return Ok(None);
            }
        }
    }
}

/// Найденная запись каталога.
struct Found {
    cluster: u32,
    attributes: u8,
}

/// Загрузочный сектор с BPB.
fn boot_sector(geometry: &Geometry, options: &FormatOptions) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];

    // Переход через BPB. Кода за ним нет — грузит прошивка, а не этот сектор, —
    // но сама последовательность обязана присутствовать: часть драйверов
    // считает том без неё неформатированным.
    sector[0] = 0xEB;
    sector[1] = 0x58;
    sector[2] = 0x90;
    // Поле OEM. Осмысленного значения не несёт, но исторически некоторые
    // драйверы придирчивы к нему; "MSWIN4.1" — то, что пишет Windows.
    sector[3..11].copy_from_slice(b"MSWIN4.1");

    put_u16(&mut sector, 11, SECTOR_SIZE as u16);
    sector[13] = geometry.sectors_per_cluster as u8;
    put_u16(&mut sector, 14, RESERVED_SECTORS as u16);
    sector[16] = FAT_COUNT as u8;
    // RootEntCnt и TotSec16 у FAT32 обязаны быть нулевыми.
    put_u16(&mut sector, 17, 0);
    put_u16(&mut sector, 19, 0);
    // Media descriptor: 0xF8 — «несъёмный носитель». То же значение обязано
    // стоять в младшем байте нулевой записи FAT.
    sector[21] = 0xF8;
    put_u16(&mut sector, 22, 0); // FATSz16 — у FAT32 нулевой
    // Геометрия CHS. Никем давно не используется, но нули здесь смущают
    // отдельные инструменты; пишем классические 63 сектора на дорожку.
    put_u16(&mut sector, 24, 63);
    put_u16(&mut sector, 26, 255);
    // Скрытые сектора — смещение раздела от начала носителя. Ошибка в этом
    // поле незаметна ровно до тех пор, пока том не попробует загрузиться.
    put_u32(
        &mut sector,
        28,
        u32::try_from(geometry.first_lba).unwrap_or(0),
    );
    put_u32(&mut sector, 32, geometry.total_sectors);

    put_u32(&mut sector, 36, geometry.sectors_per_fat);
    put_u16(&mut sector, 40, 0); // ExtFlags: FAT зеркалируются
    put_u16(&mut sector, 42, 0); // FSVer 0.0
    put_u32(&mut sector, 44, ROOT_CLUSTER);
    put_u16(&mut sector, 48, 1); // сектор FSInfo
    put_u16(&mut sector, 50, BACKUP_BOOT_SECTOR as u16);

    sector[64] = 0x80; // номер устройства BIOS; для несъёмного носителя 0x80
    sector[66] = 0x29; // расширенная подпись: дальше идут серийник и метка
    put_u32(&mut sector, 67, options.volume_id);
    sector[71..82].copy_from_slice(&label_bytes(options.label));
    sector[82..90].copy_from_slice(b"FAT32   ");

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

/// Сектор FSInfo.
fn fsinfo_sector(free_clusters: u32, next_free: u32) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];
    put_u32(&mut sector, 0, 0x4161_5252); // "RRaA"
    put_u32(&mut sector, 484, 0x6141_7272); // "rrAa"
    put_u32(&mut sector, 488, free_clusters);
    put_u32(&mut sector, 492, next_free);
    put_u32(&mut sector, 508, 0xAA55_0000);
    sector
}

/// Метка тома: ровно 11 байт, дополненных пробелами.
fn label_bytes(label: &str) -> [u8; 11] {
    let mut out = *b"NO NAME    ";
    if label.is_empty() {
        return out;
    }
    out = *b"           ";
    for (slot, ch) in out.iter_mut().zip(label.chars()) {
        // Метка хранится в однобайтовой кодировке; всё, что в неё не
        // укладывается, заменяем, а не теряем молча.
        *slot = if ch.is_ascii() {
            (ch as u8).to_ascii_uppercase()
        } else {
            b'_'
        };
    }
    out
}

/// Преобразовать имя в формат 8.3 и флаги регистра.
///
/// Возвращает [`Error::BadName`], если имя в 8.3 не помещается: усечь его
/// молча значит записать файл под другим именем, а на ESP это означает, что
/// прошивка его не найдёт.
fn short_name(name: &str) -> Result<([u8; 11], u8)> {
    let (base, ext) = match name.rsplit_once('.') {
        // Имя, начинающееся с точки, — не «пустая база с расширением», а
        // просто недопустимое в 8.3 имя.
        Some(("", _)) => return Err(Error::BadName),
        Some((base, ext)) => (base, ext),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return Err(Error::BadName);
    }

    let mut out = *b"           ";
    let base_case = encode_part(base, &mut out[..8])?;
    let ext_case = encode_part(ext, &mut out[8..])?;

    let mut flags = 0;
    if base_case == Case::Lower {
        flags |= CASE_BASE_LOWER;
    }
    if ext_case == Case::Lower {
        flags |= CASE_EXT_LOWER;
    }
    Ok((out, flags))
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Case {
    /// Часть имени целиком в нижнем регистре — регистр можно сохранить флагом.
    Lower,
    /// Всё остальное: на диске останется верхний регистр.
    Other,
}

/// Записать часть имени в поле, вернув её регистр.
fn encode_part(part: &str, out: &mut [u8]) -> Result<Case> {
    /// Символы сверх букв и цифр, разрешённые в коротком имени (Microsoft FAT
    /// specification). Пробел в список не входит намеренно: он допустим, но
    /// служит заполнителем, и имя с пробелом внутри разобрать обратно нельзя.
    const EXTRA: &[u8] = b"$%'-_@~`!(){}^#&";

    let mut lower = false;
    let mut upper = false;
    for (slot, ch) in out.iter_mut().zip(part.chars()) {
        if !ch.is_ascii() {
            return Err(Error::BadName);
        }
        let byte = ch as u8;
        if byte.is_ascii_lowercase() {
            lower = true;
        } else if byte.is_ascii_uppercase() {
            upper = true;
        } else if !byte.is_ascii_digit() && !EXTRA.contains(&byte) {
            return Err(Error::BadName);
        }
        *slot = byte.to_ascii_uppercase();
    }

    // Смешанный регистр флагом не передаётся — такое имя останется в верхнем.
    Ok(if lower && !upper { Case::Lower } else { Case::Other })
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;
    use std::io::{Cursor, Read as _};

    use super::*;
    use crate::mem::{MemDisk, junk_disk};

    /// 64 МиБ: заведомо больше минимума для FAT32 и достаточно, чтобы в томе
    /// был не один сектор таблицы FAT.
    const PART_SECTORS: u64 = 128 * 1024;

    fn formatted() -> (MemDisk, Volume) {
        let mut dev = junk_disk(PART_SECTORS).expect("образ");
        let volume = format(
            &mut dev,
            Range {
                first_lba: 0,
                last_lba: PART_SECTORS - 1,
            },
            &FormatOptions {
                label: "FREEOS ESP",
                volume_id: 0x1234_5678,
                timestamp: Timestamp::EPOCH,
            },
        )
        .expect("форматирование");
        (dev, volume)
    }

    /// Смонтировать образ посторонней реализацией FAT.
    fn mount(dev: &MemDisk) -> fatfs::FileSystem<Cursor<Vec<u8>>> {
        let image = Cursor::new(dev.as_bytes().to_vec());
        fatfs::FileSystem::new(image, fatfs::FsOptions::new()).expect("чужой драйвер монтирует том")
    }

    #[test]
    fn geometry_keeps_the_volume_fat32() {
        // Ровно та граница, из-за которой существует MIN_CLUSTERS.
        let small = Geometry::plan(0, 34 * 2048).expect("34 МиБ хватает на FAT32");
        assert_eq!(small.sectors_per_cluster, 1);
        assert!(small.cluster_count >= MIN_CLUSTERS);

        let big = Geometry::plan(0, 512 * 2048).expect("512 МиБ");
        assert!(big.sectors_per_cluster > 1, "крупный том — крупный кластер");
        assert!(big.cluster_count >= MIN_CLUSTERS);

        // Меньше минимума FAT32 не бывает вовсе.
        assert_eq!(Geometry::plan(0, 16 * 2048), Err(Error::TooSmall));
    }

    #[test]
    fn foreign_driver_sees_fat32_and_the_label() {
        let (dev, _) = formatted();
        let fs = mount(&dev);
        assert_eq!(fs.fat_type(), fatfs::FatType::Fat32);
        assert_eq!(fs.volume_label(), "FREEOS ESP");
    }

    #[test]
    fn files_and_directories_read_back() {
        let (mut dev, mut volume) = formatted();

        // Файл в несколько кластеров: цепочка FAT должна пройти не по одному
        // сектору таблицы.
        let big: Vec<u8> = (0..300_000u32).map(|index| (index % 251) as u8).collect();
        volume
            .write_file_path(&mut dev, "EFI/BOOT/BOOTX64.EFI", &big)
            .expect("файл в подкаталоге");
        volume
            .write_file_path(&mut dev, "kernel.elf", b"kernel bytes")
            .expect("файл в корне");
        volume
            .write_file_path(&mut dev, "FREEOS/ETC/PASSWD", b"root:0:0")
            .expect("файл в двух уровнях каталогов");
        volume
            .write_file_path(&mut dev, "empty.bin", b"")
            .expect("пустой файл");
        volume.finish(&mut dev).expect("завершение");

        let fs = mount(&dev);
        let root = fs.root_dir();

        let mut read = Vec::new();
        root.open_file("EFI/BOOT/BOOTX64.EFI")
            .expect("BOOTX64.EFI на месте")
            .read_to_end(&mut read)
            .expect("чтение");
        assert_eq!(read, big);

        read.clear();
        root.open_file("kernel.elf")
            .expect("kernel.elf на месте")
            .read_to_end(&mut read)
            .expect("чтение");
        assert_eq!(read, b"kernel bytes");

        read.clear();
        root.open_file("FREEOS/ETC/PASSWD")
            .expect("PASSWD на месте")
            .read_to_end(&mut read)
            .expect("чтение");
        assert_eq!(read, b"root:0:0");

        read.clear();
        root.open_file("empty.bin")
            .expect("пустой файл на месте")
            .read_to_end(&mut read)
            .expect("чтение");
        assert!(read.is_empty());
    }

    /// Регистр имени обязан пережить запись: прошивка ищет файл без учёта
    /// регистра, а вот человек, смонтировавший ESP, видит именно имя.
    #[test]
    fn lowercase_names_survive() {
        let (mut dev, mut volume) = formatted();
        volume
            .write_file_path(&mut dev, "kernel.elf", b"x")
            .expect("файл");
        volume
            .write_file_path(&mut dev, "BOOTX64.EFI", b"x")
            .expect("файл");
        volume.finish(&mut dev).expect("завершение");

        let fs = mount(&dev);
        let names: Vec<String> = fs
            .root_dir()
            .iter()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .collect();
        assert!(names.iter().any(|name| name == "kernel.elf"), "{names:?}");
        assert!(names.iter().any(|name| name == "BOOTX64.EFI"), "{names:?}");
    }

    /// Каталог длиннее одного кластера — единственный путь, на котором
    /// работает удлинение цепочки записей.
    #[test]
    fn directory_grows_past_one_cluster() {
        let (mut dev, mut volume) = formatted();
        let per_cluster = volume.geometry.cluster_bytes() as usize / DIR_ENTRY_SIZE;
        let count = per_cluster + 5;

        let dir = volume.create_dir_path(&mut dev, "MANY").expect("каталог");
        for index in 0..count {
            let name = alloc::format!("F{index:05}.BIN");
            volume
                .create_file(&mut dev, dir, &name, &[index as u8])
                .expect("файл");
        }
        volume.finish(&mut dev).expect("завершение");

        let fs = mount(&dev);
        let listed = fs
            .root_dir()
            .open_dir("MANY")
            .expect("каталог на месте")
            .iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| !entry.file_name().starts_with('.'))
            .count();
        assert_eq!(listed, count);
    }

    #[test]
    fn bad_names_are_refused_not_truncated() {
        assert_eq!(short_name("waytoolongname.txt"), Err(Error::BadName));
        assert_eq!(short_name("name.toolong"), Err(Error::BadName));
        assert_eq!(short_name(".hidden"), Err(Error::BadName));
        assert_eq!(short_name(""), Err(Error::BadName));
        assert_eq!(short_name("has space.txt"), Err(Error::BadName));
        assert_eq!(short_name("кириллица.txt"), Err(Error::BadName));

        let (name, flags) = short_name("kernel.elf").expect("допустимое имя");
        assert_eq!(&name, b"KERNEL  ELF");
        assert_eq!(flags, CASE_BASE_LOWER | CASE_EXT_LOWER);

        let (name, flags) = short_name("BOOTX64.EFI").expect("допустимое имя");
        assert_eq!(&name, b"BOOTX64 EFI");
        assert_eq!(flags, 0);
    }

    /// Метка тома не должна мешать созданию каталога с тем же именем.
    ///
    /// Проверка появилась после того, как сборка установочного ISO упала на
    /// томе с меткой `FREEOS`, в котором надо было создать каталог `FREEOS`:
    /// поиск пути находил запись метки и объявлял её не-каталогом.
    #[test]
    fn a_volume_label_is_not_a_directory_entry() {
        let mut dev = junk_disk(PART_SECTORS).expect("образ");
        let mut volume = format(
            &mut dev,
            Range { first_lba: 0, last_lba: PART_SECTORS - 1 },
            &FormatOptions {
                // Метка совпадает с именем каталога, который создаётся ниже, —
                // в этом вся суть проверки.
                label: "FREEOS",
                volume_id: 0x1234_5678,
                timestamp: Timestamp::EPOCH,
            },
        )
        .expect("форматирование");

        volume
            .write_file_path(&mut dev, "FREEOS/KERNEL.ELF", b"kernel")
            .expect("каталог с именем метки тома");
        volume.finish(&mut dev).expect("завершение тома");

        // И чужой читатель обязан увидеть то же самое: каталог, а не метку.
        let fs = mount(&dev);
        let dir = fs.root_dir().open_dir("FREEOS").expect("каталога FREEOS нет");
        let names: Vec<String> = dir
            .iter()
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect();
        assert!(names.iter().any(|name| name == "KERNEL.ELF"), "файл не найден: {names:?}");
    }

    #[test]
    fn duplicate_name_is_an_error() {
        let (mut dev, mut volume) = formatted();
        volume
            .write_file_path(&mut dev, "a.bin", b"first")
            .expect("файл");
        assert_eq!(
            volume.write_file_path(&mut dev, "a.bin", b"second"),
            Err(Error::BadName)
        );
    }

    #[test]
    fn free_space_is_tracked() {
        let (mut dev, mut volume) = formatted();
        let before = volume.free_bytes();
        let payload = vec![7u8; 1_000_000];
        volume
            .write_file_path(&mut dev, "big.bin", &payload)
            .expect("файл");
        let after = volume.free_bytes();

        let cluster = u64::from(volume.geometry.cluster_bytes());
        let expected = (payload.len() as u64).div_ceil(cluster) * cluster;
        assert_eq!(before - after, expected);
    }

    #[test]
    fn running_out_of_space_is_an_error_not_a_panic() {
        let (mut dev, mut volume) = formatted();
        let free = volume.free_bytes();
        let payload = vec![0u8; free as usize + 1];
        assert_eq!(
            volume.write_file_path(&mut dev, "toobig.bin", &payload),
            Err(Error::NoSpace)
        );
    }

    /// Том, лежащий не в начале носителя: LBA-арифметика и поле «скрытых
    /// секторов» — самое подходящее место, чтобы ошибиться на смещение.
    #[test]
    fn volume_at_an_offset_is_self_consistent() {
        const OFFSET: u64 = 2048;
        let mut dev = junk_disk(OFFSET + PART_SECTORS).expect("образ");
        let mut volume = format(
            &mut dev,
            Range {
                first_lba: OFFSET,
                last_lba: OFFSET + PART_SECTORS - 1,
            },
            &FormatOptions {
                label: "OFFSET",
                volume_id: 1,
                timestamp: Timestamp::EPOCH,
            },
        )
        .expect("форматирование");
        volume
            .write_file_path(&mut dev, "EFI/BOOT/BOOTX64.EFI", b"payload")
            .expect("файл");
        volume.finish(&mut dev).expect("завершение");

        // Проверяем, что раздел вообще не вышел за свои границы.
        let mut before = [0u8; SECTOR_SIZE];
        dev.read(OFFSET - 1, &mut before).expect("сектор перед томом");
        assert!(
            before.iter().any(|&byte| byte != 0),
            "форматирование затронуло сектор перед разделом"
        );

        // И что содержимое раздела читается чужим драйвером, если вырезать его
        // из носителя.
        let partition = dev.as_bytes()[(OFFSET as usize * SECTOR_SIZE)..].to_vec();
        let fs = fatfs::FileSystem::new(Cursor::new(partition), fatfs::FsOptions::new())
            .expect("том со смещением");
        let mut read = Vec::new();
        fs.root_dir()
            .open_file("EFI/BOOT/BOOTX64.EFI")
            .expect("файл на месте")
            .read_to_end(&mut read)
            .expect("чтение");
        assert_eq!(read, b"payload");

        // Поле скрытых секторов обязано указывать на начало раздела.
        let mut boot = [0u8; SECTOR_SIZE];
        dev.read(OFFSET, &mut boot).expect("загрузочный сектор");
        assert_eq!(
            u32::from_le_bytes(boot[28..32].try_into().unwrap()) as u64,
            OFFSET
        );
    }

    /// Резервная копия загрузочного сектора обязана совпадать с основной:
    /// прошивка восстанавливает том именно по ней.
    #[test]
    fn backup_boot_sector_matches() {
        let (mut dev, _) = formatted();
        let mut primary = [0u8; SECTOR_SIZE];
        let mut backup = [0u8; SECTOR_SIZE];
        dev.read(0, &mut primary).expect("основной");
        dev.read(u64::from(BACKUP_BOOT_SECTOR), &mut backup)
            .expect("резервный");
        assert_eq!(primary, backup);
        assert_eq!(primary[510], 0x55);
        assert_eq!(primary[511], 0xAA);
    }

    /// Обе копии таблицы FAT обязаны совпадать байт в байт — иначе `chkdsk`
    /// объявит том повреждённым, и будет прав.
    #[test]
    fn both_fat_copies_agree() {
        let (mut dev, mut volume) = formatted();
        volume
            .write_file_path(&mut dev, "EFI/BOOT/BOOTX64.EFI", &vec![1u8; 200_000])
            .expect("файл");
        volume.finish(&mut dev).expect("завершение");

        let geometry = volume.geometry();
        let mut first = vec![0u8; geometry.sectors_per_fat as usize * SECTOR_SIZE];
        let mut second = first.clone();
        dev.read(geometry.fat_lba(0), &mut first).expect("FAT #0");
        dev.read(geometry.fat_lba(1), &mut second).expect("FAT #1");
        assert_eq!(first, second);
    }
}
