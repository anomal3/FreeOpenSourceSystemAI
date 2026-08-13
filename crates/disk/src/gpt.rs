//! Таблица разделов GPT: защитный MBR, основной и резервный заголовки,
//! массив записей о разделах.
//!
//! Формат описан в спецификации UEFI, раздел «GPT Disk Layout». Ниже
//! проговорены только те его места, ошибка в которых не проявляется сразу, а
//! всплывает как «прошивка не видит раздел».
//!
//! # Раскладка носителя
//!
//! ```text
//!  LBA 0            защитный MBR: один раздел типа 0xEE на весь диск
//!  LBA 1            основной заголовок GPT
//!  LBA 2..33        массив из 128 записей по 128 байт (16 КиБ = 32 сектора)
//!  LBA 34..         первый пригодный сектор; отсюда начинаются разделы
//!  ...
//!  LBA n-33..n-1    резервная копия массива записей
//!  LBA n            резервный заголовок GPT (последний сектор носителя)
//! ```
//!
//! # Чем защитный MBR действительно защищает
//!
//! Не прошивку — она про GPT знает. Он защищает от старых утилит, которые
//! знают только MBR: увидев один раздел неизвестного типа на весь диск, такая
//! утилита не решит, что диск пуст, и не предложит его разметить.

use alloc::vec;
use alloc::vec::Vec;

use crate::crc32::Crc32;
use crate::guid::Guid;
use crate::{BlockDevice, Error, Result, check_device, put_u32, put_u64, zero_sectors};

/// Число записей в таблице.
///
/// 128 — минимум, который спецификация требует уметь читать, и ровно то, что
/// пишут все существующие инструменты. Меньше делать нельзя: часть прошивок
/// просто не рассматривает таблицы другого размера.
pub const ENTRY_COUNT: u32 = 128;

/// Размер одной записи. Спецификация допускает больше (кратно 128), но не
/// меньше.
pub const ENTRY_SIZE: u32 = 128;

/// Сколько байт занимает массив записей: 128 * 128 = 16 КиБ.
const TABLE_BYTES: u64 = ENTRY_COUNT as u64 * ENTRY_SIZE as u64;

/// Сколько секторов занимает массив записей.
///
/// Зависит от носителя, а не от формата: 32 сектора по 512 байт и 4 сектора по
/// 4096 — это одни и те же шестнадцать килобайт. С Phase 26c это функция, а не
/// константа, и именно здесь раскладка 4Kn-диска расходится с обычной: у него
/// таблица кончается на четвёртом секторе, а не на тридцать третьем.
#[must_use]
pub const fn entry_sectors(sector: usize) -> u64 {
    let sector = sector as u64;
    (TABLE_BYTES + sector - 1) / sector
}

/// Первый сектор, доступный разделам: MBR + заголовок + таблица.
#[must_use]
pub const fn first_usable_lba(sector: usize) -> u64 {
    2 + entry_sectors(sector)
}

/// Сколько секторов в хвосте носителя занимает резервная копия.
#[must_use]
pub const fn backup_sectors(sector: usize) -> u64 {
    entry_sectors(sector) + 1
}

/// Выравнивание начала разделов — 1 МиБ, выраженный в секторах носителя.
///
/// Это не эстетика. Накопитель с физическим сектором 4 КиБ (а таковы все
/// современные) на невыровненном разделе выполняет каждую запись как
/// «прочитать-изменить-записать», теряя кратно в скорости; у SSD то же
/// касается страницы стирания. 1 МиБ кратен любому реально существующему
/// физическому сектору и странице, поэтому его и выбрали стандартом де-факто.
/// Выравнивание считается в **мегабайте**, а не в числе секторов: на 4Kn-диске
/// те же 2048 секторов были бы восемью мегабайтами.
#[must_use]
pub const fn alignment_sectors(sector: usize) -> u64 {
    (1024 * 1024 / sector) as u64
}

/// Тип раздела: EFI System Partition (спецификация UEFI).
pub const ESP_TYPE: Guid = Guid::new(
    0xC12A_7328,
    0xF81F,
    0x11D2,
    [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
);

/// Тип раздела: корневая ФС FreeOS.
///
/// Значение выбрано произвольно и зафиксировано здесь навсегда — ровно так же
/// заводят свои типы Linux и FreeBSD. Смысл GUID типа не в случайности, а в
/// том, чтобы не совпасть с чужим: раздел с типом «Linux filesystem» чужой
/// установщик вправе счесть своим и предложить отформатировать.
pub const FREEOS_ROOT_TYPE: Guid = Guid::new(
    0x0F7B_3A4E,
    0x2C58,
    0x4D91,
    [0x9E, 0x6A, 0x1B, 0x84, 0xC2, 0xD5, 0xA7, 0x30],
);

/// Атрибут записи: «не трогать» (bit 0, Required Partition).
pub const ATTR_REQUIRED: u64 = 1 << 0;

/// Раздел, который нужно создать.
pub struct PartitionSpec<'a> {
    pub type_guid: Guid,
    pub unique_guid: Guid,
    /// Первый сектор раздела.
    pub first_lba: u64,
    /// Последний сектор раздела, включительно. Именно включительно — так
    /// устроена запись GPT, и смещение на единицу здесь означает раздел,
    /// который перекрывает соседа.
    pub last_lba: u64,
    pub attributes: u64,
    /// Имя раздела. На диске это 36 символов UTF-16; лишнее обрезается.
    pub name: &'a str,
}

impl PartitionSpec<'_> {
    /// Размер раздела в секторах.
    #[must_use]
    pub const fn sectors(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }
}

/// Границы одного раздела в готовой раскладке.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub first_lba: u64,
    pub last_lba: u64,
}

impl Range {
    #[must_use]
    pub const fn sectors(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }

    /// Размер раздела в байтах.
    ///
    /// Размер сектора приходится передавать: диапазон измеряется в секторах
    /// носителя, а сколько это байт — свойство носителя, а не диапазона.
    /// Догадаться неоткуда, и молчаливое «наверное, 512» здесь ошиблось бы
    /// ровно в восемь раз.
    #[must_use]
    pub const fn bytes(&self, sector: usize) -> u64 {
        self.sectors() * sector as u64
    }
}

/// Раскладка разделов на носителе.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub esp: Range,
    /// Корневой раздел. `None`, если места на него не осталось или он не
    /// запрашивался.
    pub root: Option<Range>,
}

/// Спланировать раскладку: ESP заданного размера, следом — корневой раздел на
/// всё оставшееся место.
///
/// `esp_bytes` округляется вверх до выравнивания. `want_root` = `false` даёт
/// носитель с одним ESP — так устроен образ, который собирает `xtask image`:
/// корневой ФС у системы пока нет, и пустой раздел под неё в образе,
/// предназначенном для запуска, был бы враньём.
pub fn plan(sector_count: u64, sector: usize, esp_bytes: u64, want_root: bool) -> Result<Layout> {
    let first_usable = first_usable_lba(sector);
    let backup = backup_sectors(sector);
    let alignment = alignment_sectors(sector);

    if sector_count <= first_usable + backup + alignment {
        return Err(Error::TooSmall);
    }
    let last_usable = sector_count - 1 - backup;

    // Начало первого раздела выравнивается вверх, а не просто ставится в
    // первый доступный сектор: см. `alignment_sectors`.
    let esp_first = align_up(first_usable, alignment);
    let esp_sectors = align_up(esp_bytes.div_ceil(sector as u64), alignment);
    let esp_last = esp_first
        .checked_add(esp_sectors)
        .ok_or(Error::TooSmall)?
        .checked_sub(1)
        .ok_or(Error::TooSmall)?;
    if esp_last > last_usable {
        return Err(Error::TooSmall);
    }

    let root = if want_root {
        let root_first = align_up(esp_last + 1, alignment);
        // Раздел меньше выравнивания смысла не имеет: под ФС там всё равно
        // ничего не разместить, а запись в таблице появится и будет вводить в
        // заблуждение.
        if root_first + alignment <= last_usable {
            Some(Range {
                first_lba: root_first,
                last_lba: last_usable,
            })
        } else {
            None
        }
    } else {
        None
    };

    Ok(Layout {
        esp: Range {
            first_lba: esp_first,
            last_lba: esp_last,
        },
        root,
    })
}

const fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

/// Затереть всё, что на носителе выглядело разметкой.
///
/// Обнуляются первый мегабайт (там MBR, GPT и суперблоки почти всех файловых
/// систем) и хвост под резервную копию GPT. Без второго прошивка нашла бы
/// резервный заголовок от прежней разметки и восстановила бы по нему чужую
/// таблицу — диск «сам собой» вернул бы старые разделы.
pub fn wipe(dev: &mut dyn BlockDevice) -> Result<()> {
    check_device(dev)?;
    let sector = dev.sector_size() as usize;
    let sectors = dev.sector_count();

    let head = alignment_sectors(sector).min(sectors);
    zero_sectors(dev, 0, head)?;

    let tail = backup_sectors(sector).min(sectors);
    zero_sectors(dev, sectors - tail, tail)?;

    dev.flush()
}

/// Записать таблицу разделов.
///
/// Носитель перед этим стоит прогнать через [`wipe`]: запись GPT не трогает
/// содержимое разделов, и остатки прежней файловой системы внутри нового
/// раздела прошивка вполне может принять за настоящие.
pub fn write(dev: &mut dyn BlockDevice, disk_guid: Guid, parts: &[PartitionSpec]) -> Result<()> {
    check_device(dev)?;
    let sector = dev.sector_size() as usize;
    let sectors = dev.sector_count();
    if sectors <= first_usable_lba(sector) + backup_sectors(sector) {
        return Err(Error::TooSmall);
    }
    if parts.len() > ENTRY_COUNT as usize {
        return Err(Error::NoSpace);
    }

    let first_usable = first_usable_lba(sector);
    let last_usable = sectors - 1 - backup_sectors(sector);

    // Проверяем раскладку до первой записи: половина размеченного диска хуже,
    // чем неразмеченный.
    let mut previous_end = first_usable - 1;
    for part in parts {
        if part.first_lba < first_usable
            || part.last_lba > last_usable
            || part.first_lba > part.last_lba
        {
            return Err(Error::OutOfRange);
        }
        // Записи обязаны идти по возрастанию и не перекрываться. Спецификация
        // порядка не требует, но раскладку строим мы сами, и перекрытие здесь
        // означало бы ошибку в расчёте, а не экзотический носитель.
        if part.first_lba <= previous_end {
            return Err(Error::OutOfRange);
        }
        previous_end = part.last_lba;
    }

    let entries = build_entries(parts);
    let entries_crc = {
        let mut crc = Crc32::new();
        crc.update(&entries);
        crc.finish()
    };

    let backup_entries_lba = sectors - backup_sectors(sector);
    let backup_header_lba = sectors - 1;

    // Порядок записи выбран так, чтобы носитель как можно меньше времени
    // выглядел размеченным наполовину: сначала всё, на что ссылается
    // заголовок, и только потом сами заголовки.
    dev.write(2, &entries)?;
    dev.write(backup_entries_lba, &entries)?;

    let primary = header(
        disk_guid,
        1,
        backup_header_lba,
        2,
        first_usable,
        last_usable,
        entries_crc,
        sector,
    );
    let backup = header(
        disk_guid,
        backup_header_lba,
        1,
        backup_entries_lba,
        first_usable,
        last_usable,
        entries_crc,
        sector,
    );

    // Резервный заголовок пишется раньше основного. Если запись оборвётся
    // между ними, диск останется без действующей разметки — и это лучше, чем
    // основной заголовок, ссылающийся на несуществующую копию: такой диск
    // прошивка считает размеченным и пытается им пользоваться.
    dev.write(backup_header_lba, &backup[..sector])?;
    dev.write(0, &protective_mbr(sectors, sector)[..sector])?;
    dev.write(1, &primary[..sector])?;

    dev.flush()
}

/// Раздел, вычитанный с носителя.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    /// Номер записи в таблице, начиная с нуля.
    pub index: u32,
    pub type_guid: Guid,
    pub unique_guid: Guid,
    pub first_lba: u64,
    pub last_lba: u64,
    pub attributes: u64,
    /// Имя раздела; лишнее за пределами массива отброшено.
    pub name: [u8; 72],
}

impl Partition {
    #[must_use]
    pub const fn range(&self) -> Range {
        Range {
            first_lba: self.first_lba,
            last_lba: self.last_lba,
        }
    }

    /// Имя раздела строкой. UTF-16 на диске, поэтому пары байт склеиваются;
    /// суррогатные пары не поддерживаются и заменяются вопросительным знаком.
    #[must_use]
    pub fn name_string(&self) -> alloc::string::String {
        let units: Vec<u16> = self
            .name
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        char::decode_utf16(units)
            .map(|ch| ch.unwrap_or('?'))
            .collect()
    }
}

/// Прочитанная таблица разделов.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub disk_guid: Guid,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub partitions: Vec<Partition>,
}

impl Table {
    /// Первый раздел заданного типа.
    #[must_use]
    pub fn find(&self, type_guid: Guid) -> Option<&Partition> {
        self.partitions
            .iter()
            .find(|partition| partition.type_guid == type_guid)
    }
}

/// Прочитать таблицу разделов.
///
/// Проверяются обе контрольные суммы — заголовка и массива записей. Это не
/// формальность: заголовок приходит из-за границы доверия, а по его полям
/// вычисляются адреса, по которым мы потом читаем. Заголовок с испорченным
/// `partition_entry_lba` без проверки увёл бы чтение в произвольное место
/// носителя.
///
/// Резервная копия при провале основной **не** используется. Восстановление
/// разметки — операция, которая меняет диск, и делать её мимоходом внутри
/// функции чтения нельзя; вызывающему честнее получить отказ.
pub fn read(dev: &mut dyn BlockDevice) -> Result<Table> {
    if !crate::sector_size_supported(dev.sector_size()) {
        return Err(Error::UnsupportedSectorSize(dev.sector_size()));
    }
    let sector = dev.sector_size() as usize;
    if dev.sector_count() <= first_usable_lba(sector) {
        return Err(Error::TooSmall);
    }

    // Буфер по наибольшему сектору, читается ровно один сектор носителя.
    // Массив на стеке, а не `Vec`: чтение таблицы бывает и в установщике, где
    // куча есть, но лишняя аллокация в коде, который разбирает чужие данные, —
    // это ещё один путь отказа там, где он не нужен.
    let mut buffer = [0u8; crate::MAX_SECTOR_SIZE];
    let header = &mut buffer[..sector];
    dev.read(1, header)?;
    if &header[0..8] != b"EFI PART" {
        return Err(Error::NotPartitioned);
    }

    let header_size = u32_at(header, 12) as usize;
    // Заголовок короче обязательных 92 байт или длиннее сектора — это не наша
    // ревизия формата, и считать по нему CRC бессмысленно.
    if !(92..=sector).contains(&header_size) {
        return Err(Error::NotPartitioned);
    }
    let stored_crc = u32_at(header, 16);
    let mut check = [0u8; crate::MAX_SECTOR_SIZE];
    check[..sector].copy_from_slice(header);
    check[16..20].fill(0);
    let mut crc = Crc32::new();
    crc.update(&check[..header_size]);
    if crc.finish() != stored_crc {
        return Err(Error::NotPartitioned);
    }

    let entry_size = u32_at(header, 84) as usize;
    let entry_count = u32_at(header, 80);
    // Запись меньше 128 байт формат запрещает; слишком большая таблица — это
    // либо мусор, либо носитель, который мы всё равно не размечали.
    if entry_size < ENTRY_SIZE as usize || entry_count > 4096 {
        return Err(Error::NotPartitioned);
    }
    let entries_lba = u64_at(header, 72);
    let table_bytes = entry_size * entry_count as usize;
    let table_sectors = table_bytes.div_ceil(sector);
    if entries_lba + table_sectors as u64 > dev.sector_count() {
        return Err(Error::OutOfRange);
    }

    let entries_crc = u32_at(header, 88);
    let mut table = vec![0u8; table_sectors * sector];
    dev.read(entries_lba, &mut table)?;
    let mut crc = Crc32::new();
    crc.update(&table[..table_bytes]);
    if crc.finish() != entries_crc {
        return Err(Error::NotPartitioned);
    }

    let mut partitions = Vec::new();
    for index in 0..entry_count {
        let at = index as usize * entry_size;
        let slot = &table[at..at + ENTRY_SIZE as usize];
        let type_guid = Guid::from_bytes(slot[0..16].try_into().expect("ровно 16 байт"));
        // Нулевой тип означает пустую запись — их в таблице большинство.
        if type_guid.is_zero() {
            continue;
        }
        let mut name = [0u8; 72];
        name.copy_from_slice(&slot[56..128]);
        partitions.push(Partition {
            index,
            type_guid,
            unique_guid: Guid::from_bytes(slot[16..32].try_into().expect("ровно 16 байт")),
            first_lba: u64_at(slot, 32),
            last_lba: u64_at(slot, 40),
            attributes: u64_at(slot, 48),
            name,
        });
    }

    Ok(Table {
        disk_guid: Guid::from_bytes(header[56..72].try_into().expect("ровно 16 байт")),
        first_usable_lba: u64_at(header, 40),
        last_usable_lba: u64_at(header, 48),
        partitions,
    })
}

#[inline]
fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[inline]
fn u64_at(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// Массив записей о разделах — 16 КиБ, как он лежит на диске.
fn build_entries(parts: &[PartitionSpec]) -> Vec<u8> {
    let mut entries = vec![0u8; (ENTRY_COUNT * ENTRY_SIZE) as usize];
    for (index, part) in parts.iter().enumerate() {
        let at = index * ENTRY_SIZE as usize;
        let slot = &mut entries[at..at + ENTRY_SIZE as usize];
        slot[0..16].copy_from_slice(&part.type_guid.to_bytes());
        slot[16..32].copy_from_slice(&part.unique_guid.to_bytes());
        put_u64(slot, 32, part.first_lba);
        put_u64(slot, 40, part.last_lba);
        put_u64(slot, 48, part.attributes);
        // Имя — 36 символов UTF-16LE. Символы вне BMP заняли бы две позиции;
        // такие имена мы просто не выдаём, но обрезка по позициям, а не по
        // символам, оставляет запись корректной в любом случае.
        let mut at = 56;
        for unit in part.name.encode_utf16() {
            if at + 2 > ENTRY_SIZE as usize {
                break;
            }
            slot[at..at + 2].copy_from_slice(&unit.to_le_bytes());
            at += 2;
        }
    }
    entries
}

/// Сектор заголовка GPT.
#[allow(clippy::too_many_arguments)]
fn header(
    disk_guid: Guid,
    my_lba: u64,
    alternate_lba: u64,
    entries_lba: u64,
    first_usable: u64,
    last_usable: u64,
    entries_crc: u32,
    sector_size: usize,
) -> [u8; crate::MAX_SECTOR_SIZE] {
    /// Длина заголовка по спецификации. Именно это число, а не размер сектора:
    /// CRC считается по первым 92 байтам, и хвост сектора в неё не входит.
    /// Ровно поэтому заголовок 4Kn-диска отличается от обычного только тем,
    /// **где он лежит**, а не тем, что в нём написано.
    const HEADER_SIZE: u32 = 92;

    debug_assert!(sector_size <= crate::MAX_SECTOR_SIZE);
    let mut sector = [0u8; crate::MAX_SECTOR_SIZE];
    sector[0..8].copy_from_slice(b"EFI PART");
    // Ревизия 1.0 записывается как 0x00010000.
    put_u32(&mut sector, 8, 0x0001_0000);
    put_u32(&mut sector, 12, HEADER_SIZE);
    // Байты 16..20 — CRC самого заголовка; на время подсчёта они нулевые.
    put_u32(&mut sector, 20, 0); // Reserved
    put_u64(&mut sector, 24, my_lba);
    put_u64(&mut sector, 32, alternate_lba);
    put_u64(&mut sector, 40, first_usable);
    put_u64(&mut sector, 48, last_usable);
    sector[56..72].copy_from_slice(&disk_guid.to_bytes());
    put_u64(&mut sector, 72, entries_lba);
    put_u32(&mut sector, 80, ENTRY_COUNT);
    put_u32(&mut sector, 84, ENTRY_SIZE);
    put_u32(&mut sector, 88, entries_crc);

    let mut crc = Crc32::new();
    crc.update(&sector[..HEADER_SIZE as usize]);
    put_u32(&mut sector, 16, crc.finish());

    sector
}

/// Защитный MBR: одна запись типа 0xEE на весь носитель.
///
/// Подпись `0x55AA` стоит на 510-м байте **сектора**, а не в его конце: у
/// 4Kn-диска после неё остаётся ещё три с половиной килобайта нулей, и это
/// правильно — MBR описан в байтах от начала носителя, а не долями сектора.
fn protective_mbr(sectors: u64, sector_size: usize) -> [u8; crate::MAX_SECTOR_SIZE] {
    debug_assert!(sector_size <= crate::MAX_SECTOR_SIZE);
    let mut sector = [0u8; crate::MAX_SECTOR_SIZE];
    let entry = &mut sector[446..462];

    entry[0] = 0x00; // не загрузочный: грузит прошивка, а не код MBR
    // CHS начала — 0/0/2, то есть «сразу после MBR». Поля CHS давно
    // бессмысленны, но спецификация фиксирует именно эти значения, и
    // отклонение от них смущает старые утилиты.
    entry[1] = 0x00;
    entry[2] = 0x02;
    entry[3] = 0x00;
    entry[4] = 0xEE; // тип: GPT protective
    // CHS конца — 0xFFFFFF, «дальше, чем CHS умеет описать».
    entry[5] = 0xFF;
    entry[6] = 0xFF;
    entry[7] = 0xFF;
    put_u32(entry, 8, 1); // начальный LBA
    // Размер в секторах. Диск больше 2 ТиБ в 32 бита не помещается — по
    // спецификации в этом случае пишется 0xFFFFFFFF.
    let size = u32::try_from(sectors - 1).unwrap_or(0xFFFF_FFFF);
    put_u32(entry, 12, size);

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{MemDisk, junk_disk};

    const DISK_SECTORS: u64 = 128 * 1024; // 64 МиБ
    /// Размер сектора образа, на котором стоят эти проверки.
    const SECTOR: usize = crate::DEFAULT_SECTOR_SIZE;

    fn sample_disk() -> MemDisk {
        junk_disk(DISK_SECTORS).expect("образ на 64 МиБ должен размещаться")
    }

    fn read_sector(dev: &mut MemDisk, lba: u64) -> [u8; crate::DEFAULT_SECTOR_SIZE] {
        let mut sector = [0u8; crate::DEFAULT_SECTOR_SIZE];
        dev.read(lba, &mut sector).expect("сектор в пределах образа");
        sector
    }

    fn write_sample(dev: &mut MemDisk) -> Layout {
        let layout = plan(dev.sector_count(), SECTOR, 16 * 1024 * 1024, true).expect("раскладка");
        let root = layout.root.expect("корневой раздел помещается");
        wipe(dev).expect("затирание");
        write(
            dev,
            Guid::from_entropy([1; 16]),
            &[
                PartitionSpec {
                    type_guid: ESP_TYPE,
                    unique_guid: Guid::from_entropy([2; 16]),
                    first_lba: layout.esp.first_lba,
                    last_lba: layout.esp.last_lba,
                    attributes: 0,
                    name: "FreeOS ESP",
                },
                PartitionSpec {
                    type_guid: FREEOS_ROOT_TYPE,
                    unique_guid: Guid::from_entropy([3; 16]),
                    first_lba: root.first_lba,
                    last_lba: root.last_lba,
                    attributes: 0,
                    name: "FreeOS root",
                },
            ],
        )
        .expect("запись GPT");
        layout
    }

    #[test]
    fn layout_is_aligned_and_ordered() {
        let layout = plan(DISK_SECTORS, SECTOR, 16 * 1024 * 1024, true).expect("раскладка");
        let root = layout.root.expect("корневой раздел");
        assert_eq!(layout.esp.first_lba % alignment_sectors(SECTOR), 0);
        assert_eq!(root.first_lba % alignment_sectors(SECTOR), 0);
        assert!(layout.esp.last_lba < root.first_lba);
        assert_eq!(root.last_lba, DISK_SECTORS - 1 - backup_sectors(SECTOR));
        assert_eq!(layout.esp.bytes(SECTOR), 16 * 1024 * 1024);
    }

    #[test]
    fn single_partition_layout_has_no_root() {
        let layout = plan(DISK_SECTORS, SECTOR, 16 * 1024 * 1024, false).expect("раскладка");
        assert!(layout.root.is_none());
    }

    #[test]
    fn tiny_disk_is_rejected() {
        assert_eq!(plan(64, SECTOR, 1024 * 1024, false), Err(Error::TooSmall));
        // Носитель есть, но ESP запрошен больше него.
        assert_eq!(
            plan(DISK_SECTORS, SECTOR, 512 * 1024 * 1024, false),
            Err(Error::TooSmall)
        );
    }

    #[test]
    fn protective_mbr_covers_the_whole_disk() {
        let mut dev = sample_disk();
        write_sample(&mut dev);
        let mbr = read_sector(&mut dev, 0);

        assert_eq!(mbr[510], 0x55);
        assert_eq!(mbr[511], 0xAA);
        assert_eq!(mbr[446 + 4], 0xEE);
        assert_eq!(u32::from_le_bytes(mbr[446 + 8..446 + 12].try_into().unwrap()), 1);
        assert_eq!(
            u32::from_le_bytes(mbr[446 + 12..446 + 16].try_into().unwrap()) as u64,
            DISK_SECTORS - 1
        );
        // Остальные три записи обязаны остаться пустыми.
        assert!(mbr[462..510].iter().all(|&byte| byte == 0));
    }

    /// Заголовок должен сходиться по собственной CRC — именно так его
    /// проверяет прошивка, и именно здесь всплывает лишний байт в разметке
    /// полей.
    #[test]
    fn both_headers_verify() {
        let mut dev = sample_disk();
        write_sample(&mut dev);

        for lba in [1, DISK_SECTORS - 1] {
            let sector = read_sector(&mut dev, lba);
            assert_eq!(&sector[0..8], b"EFI PART", "подпись заголовка на LBA {lba}");

            let stored = u32::from_le_bytes(sector[16..20].try_into().unwrap());
            let mut check = sector;
            check[16..20].fill(0);
            let size = u32::from_le_bytes(sector[12..16].try_into().unwrap()) as usize;
            assert_eq!(size, 92);
            let mut crc = Crc32::new();
            crc.update(&check[..size]);
            assert_eq!(crc.finish(), stored, "CRC заголовка на LBA {lba}");

            assert_eq!(
                u64::from_le_bytes(sector[24..32].try_into().unwrap()),
                lba,
                "MyLBA обязан совпадать с собственным адресом"
            );
        }

        // Заголовки обязаны ссылаться друг на друга.
        let primary = read_sector(&mut dev, 1);
        let backup = read_sector(&mut dev, DISK_SECTORS - 1);
        assert_eq!(
            u64::from_le_bytes(primary[32..40].try_into().unwrap()),
            DISK_SECTORS - 1
        );
        assert_eq!(u64::from_le_bytes(backup[32..40].try_into().unwrap()), 1);
        // И на разные копии таблицы.
        assert_eq!(u64::from_le_bytes(primary[72..80].try_into().unwrap()), 2);
        assert_eq!(
            u64::from_le_bytes(backup[72..80].try_into().unwrap()),
            DISK_SECTORS - backup_sectors(SECTOR)
        );
    }

    #[test]
    fn entries_match_their_crc_and_content() {
        let mut dev = sample_disk();
        let layout = write_sample(&mut dev);

        let mut table = vec![0u8; (ENTRY_COUNT * ENTRY_SIZE) as usize];
        dev.read(2, &mut table).expect("таблица");

        let primary = read_sector(&mut dev, 1);
        let stored = u32::from_le_bytes(primary[88..92].try_into().unwrap());
        let mut crc = Crc32::new();
        crc.update(&table);
        assert_eq!(crc.finish(), stored);

        let first = &table[..ENTRY_SIZE as usize];
        assert_eq!(Guid::from_bytes(first[0..16].try_into().unwrap()), ESP_TYPE);
        assert_eq!(
            u64::from_le_bytes(first[32..40].try_into().unwrap()),
            layout.esp.first_lba
        );
        assert_eq!(
            u64::from_le_bytes(first[40..48].try_into().unwrap()),
            layout.esp.last_lba
        );
        let name: alloc::string::String = char::decode_utf16(
            first[56..128]
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|&unit| unit != 0),
        )
        .map(|ch| ch.unwrap_or('?'))
        .collect();
        assert_eq!(name, "FreeOS ESP");

        // Резервная копия обязана быть побайтово той же.
        let mut backup = vec![0u8; table.len()];
        dev.read(DISK_SECTORS - backup_sectors(SECTOR), &mut backup)
            .expect("резервная таблица");
        assert_eq!(backup, table);
    }

    /// Чужая разметка обязана исчезнуть целиком: и в начале носителя, и в
    /// хвосте, где прошивка ищет резервный заголовок.
    #[test]
    fn wipe_clears_stale_structures() {
        let mut dev = sample_disk();
        // До затирания хвост заведомо ненулевой — иначе тест ничего не значит.
        let tail_before = read_sector(&mut dev, DISK_SECTORS - 1);
        assert!(tail_before.iter().any(|&byte| byte != 0));

        wipe(&mut dev).expect("затирание");

        for lba in [0, 1, 2, alignment_sectors(SECTOR) - 1, DISK_SECTORS - backup_sectors(SECTOR), DISK_SECTORS - 1]
        {
            let sector = read_sector(&mut dev, lba);
            assert!(
                sector.iter().all(|&byte| byte == 0),
                "сектор {lba} остался ненулевым"
            );
        }
        // А данные между затираемыми областями трогать нельзя.
        let middle = read_sector(&mut dev, DISK_SECTORS / 2);
        assert!(middle.iter().any(|&byte| byte != 0));
    }

    /// Читатель обязан вернуть ровно то, что записал писатель: это
    /// единственная связь между разметкой, которую делает установщик, и
    /// разметкой, которую потом разбирает ядро.
    #[test]
    fn the_reader_recovers_what_the_writer_wrote() {
        let mut dev = sample_disk();
        let layout = write_sample(&mut dev);
        let root = layout.root.expect("корневой раздел");

        let table = read(&mut dev).expect("таблица читается");
        assert_eq!(table.disk_guid, Guid::from_entropy([1; 16]));
        assert_eq!(table.first_usable_lba, first_usable_lba(SECTOR));
        assert_eq!(table.last_usable_lba, DISK_SECTORS - 1 - backup_sectors(SECTOR));
        assert_eq!(table.partitions.len(), 2);

        let esp = &table.partitions[0];
        assert_eq!(esp.index, 0);
        assert_eq!(esp.type_guid, ESP_TYPE);
        assert_eq!(esp.range(), layout.esp);
        assert_eq!(esp.name_string(), "FreeOS ESP");

        let found = table.find(FREEOS_ROOT_TYPE).expect("корневой раздел найден");
        assert_eq!(found.range(), root);
        assert_eq!(found.name_string(), "FreeOS root");
        assert!(table.find(Guid::from_entropy([9; 16])).is_none());
    }

    /// Порча заголовка обязана приводить к отказу, а не к чтению по мусорным
    /// адресам: контрольная сумма для того и считается.
    #[test]
    fn a_damaged_header_is_refused() {
        let mut dev = sample_disk();
        write_sample(&mut dev);

        let mut header = read_sector(&mut dev, 1);
        // Правим адрес таблицы записей, не трогая контрольную сумму.
        header[72..80].copy_from_slice(&999_999u64.to_le_bytes());
        dev.write(1, &header).expect("запись");
        assert_eq!(read(&mut dev), Err(Error::NotPartitioned));
    }

    /// Порча самой таблицы записей ловится её собственной суммой.
    #[test]
    fn a_damaged_entry_table_is_refused() {
        let mut dev = sample_disk();
        write_sample(&mut dev);

        let mut sector = read_sector(&mut dev, 2);
        sector[40] ^= 0xFF;
        dev.write(2, &sector).expect("запись");
        assert_eq!(read(&mut dev), Err(Error::NotPartitioned));
    }

    #[test]
    fn an_unpartitioned_disk_is_reported_as_such() {
        let mut dev = sample_disk();
        wipe(&mut dev).expect("затирание");
        assert_eq!(read(&mut dev), Err(Error::NotPartitioned));
    }

    #[test]
    fn overlapping_partitions_are_rejected() {
        let mut dev = sample_disk();
        let result = write(
            &mut dev,
            Guid::from_entropy([1; 16]),
            &[
                PartitionSpec {
                    type_guid: ESP_TYPE,
                    unique_guid: Guid::from_entropy([2; 16]),
                    first_lba: 2048,
                    last_lba: 4096,
                    attributes: 0,
                    name: "a",
                },
                PartitionSpec {
                    type_guid: ESP_TYPE,
                    unique_guid: Guid::from_entropy([3; 16]),
                    first_lba: 4096,
                    last_lba: 8192,
                    attributes: 0,
                    name: "b",
                },
            ],
        );
        assert_eq!(result, Err(Error::OutOfRange));
    }

    #[test]
    fn partition_past_the_backup_area_is_rejected() {
        let mut dev = sample_disk();
        let result = write(
            &mut dev,
            Guid::from_entropy([1; 16]),
            &[PartitionSpec {
                type_guid: ESP_TYPE,
                unique_guid: Guid::from_entropy([2; 16]),
                first_lba: 2048,
                last_lba: DISK_SECTORS - 1,
                attributes: 0,
                name: "too big",
            }],
        );
        assert_eq!(result, Err(Error::OutOfRange));
    }
}
