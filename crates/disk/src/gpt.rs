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
use crate::{BlockDevice, Error, Result, SECTOR_SIZE, check_device, put_u32, put_u64, zero_sectors};

/// Число записей в таблице.
///
/// 128 — минимум, который спецификация требует уметь читать, и ровно то, что
/// пишут все существующие инструменты. Меньше делать нельзя: часть прошивок
/// просто не рассматривает таблицы другого размера.
pub const ENTRY_COUNT: u32 = 128;

/// Размер одной записи. Спецификация допускает больше (кратно 128), но не
/// меньше.
pub const ENTRY_SIZE: u32 = 128;

/// Сколько секторов занимает массив записей: 128 * 128 / 512 = 32.
pub const ENTRY_SECTORS: u64 = (ENTRY_COUNT as u64 * ENTRY_SIZE as u64) / SECTOR_SIZE as u64;

/// Первый сектор, доступный разделам: MBR + заголовок + таблица.
pub const FIRST_USABLE_LBA: u64 = 2 + ENTRY_SECTORS;

/// Сколько секторов в хвосте носителя занимает резервная копия.
pub const BACKUP_SECTORS: u64 = ENTRY_SECTORS + 1;

/// Выравнивание начала разделов — 1 МиБ.
///
/// Это не эстетика. Накопитель с физическим сектором 4 КиБ (а таковы все
/// современные) на невыровненном разделе выполняет каждую запись как
/// «прочитать-изменить-записать», теряя кратно в скорости; у SSD то же
/// касается страницы стирания. 1 МиБ кратен любому реально существующему
/// физическому сектору и странице, поэтому его и выбрали стандартом де-факто.
pub const ALIGNMENT_SECTORS: u64 = (1024 * 1024) / SECTOR_SIZE as u64;

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

    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.sectors() * SECTOR_SIZE as u64
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
pub fn plan(sector_count: u64, esp_bytes: u64, want_root: bool) -> Result<Layout> {
    if sector_count <= FIRST_USABLE_LBA + BACKUP_SECTORS + ALIGNMENT_SECTORS {
        return Err(Error::TooSmall);
    }
    let last_usable = sector_count - 1 - BACKUP_SECTORS;

    // Начало первого раздела выравнивается вверх, а не просто ставится в
    // FIRST_USABLE_LBA: см. ALIGNMENT_SECTORS.
    let esp_first = align_up(FIRST_USABLE_LBA, ALIGNMENT_SECTORS);
    let esp_sectors = align_up(esp_bytes.div_ceil(SECTOR_SIZE as u64), ALIGNMENT_SECTORS);
    let esp_last = esp_first
        .checked_add(esp_sectors)
        .ok_or(Error::TooSmall)?
        .checked_sub(1)
        .ok_or(Error::TooSmall)?;
    if esp_last > last_usable {
        return Err(Error::TooSmall);
    }

    let root = if want_root {
        let root_first = align_up(esp_last + 1, ALIGNMENT_SECTORS);
        // Раздел меньше выравнивания смысла не имеет: под ФС там всё равно
        // ничего не разместить, а запись в таблице появится и будет вводить в
        // заблуждение.
        if root_first + ALIGNMENT_SECTORS <= last_usable {
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
    let sectors = dev.sector_count();

    let head = ALIGNMENT_SECTORS.min(sectors);
    zero_sectors(dev, 0, head)?;

    let tail = BACKUP_SECTORS.min(sectors);
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
    let sectors = dev.sector_count();
    if sectors <= FIRST_USABLE_LBA + BACKUP_SECTORS {
        return Err(Error::TooSmall);
    }
    if parts.len() > ENTRY_COUNT as usize {
        return Err(Error::NoSpace);
    }

    let first_usable = FIRST_USABLE_LBA;
    let last_usable = sectors - 1 - BACKUP_SECTORS;

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

    let backup_entries_lba = sectors - BACKUP_SECTORS;
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
    );
    let backup = header(
        disk_guid,
        backup_header_lba,
        1,
        backup_entries_lba,
        first_usable,
        last_usable,
        entries_crc,
    );

    // Резервный заголовок пишется раньше основного. Если запись оборвётся
    // между ними, диск останется без действующей разметки — и это лучше, чем
    // основной заголовок, ссылающийся на несуществующую копию: такой диск
    // прошивка считает размеченным и пытается им пользоваться.
    dev.write(backup_header_lba, &backup)?;
    dev.write(0, &protective_mbr(sectors))?;
    dev.write(1, &primary)?;

    dev.flush()
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
) -> [u8; SECTOR_SIZE] {
    /// Длина заголовка по спецификации. Именно это число, а не 512: CRC
    /// считается по первым 92 байтам, и хвост сектора в неё не входит.
    const HEADER_SIZE: u32 = 92;

    let mut sector = [0u8; SECTOR_SIZE];
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
fn protective_mbr(sectors: u64) -> [u8; SECTOR_SIZE] {
    let mut sector = [0u8; SECTOR_SIZE];
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

    fn sample_disk() -> MemDisk {
        junk_disk(DISK_SECTORS).expect("образ на 64 МиБ должен размещаться")
    }

    fn read_sector(dev: &mut MemDisk, lba: u64) -> [u8; SECTOR_SIZE] {
        let mut sector = [0u8; SECTOR_SIZE];
        dev.read(lba, &mut sector).expect("сектор в пределах образа");
        sector
    }

    fn write_sample(dev: &mut MemDisk) -> Layout {
        let layout = plan(dev.sector_count(), 16 * 1024 * 1024, true).expect("раскладка");
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
        let layout = plan(DISK_SECTORS, 16 * 1024 * 1024, true).expect("раскладка");
        let root = layout.root.expect("корневой раздел");
        assert_eq!(layout.esp.first_lba % ALIGNMENT_SECTORS, 0);
        assert_eq!(root.first_lba % ALIGNMENT_SECTORS, 0);
        assert!(layout.esp.last_lba < root.first_lba);
        assert_eq!(root.last_lba, DISK_SECTORS - 1 - BACKUP_SECTORS);
        assert_eq!(layout.esp.bytes(), 16 * 1024 * 1024);
    }

    #[test]
    fn single_partition_layout_has_no_root() {
        let layout = plan(DISK_SECTORS, 16 * 1024 * 1024, false).expect("раскладка");
        assert!(layout.root.is_none());
    }

    #[test]
    fn tiny_disk_is_rejected() {
        assert_eq!(plan(64, 1024 * 1024, false), Err(Error::TooSmall));
        // Носитель есть, но ESP запрошен больше него.
        assert_eq!(
            plan(DISK_SECTORS, 512 * 1024 * 1024, false),
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
            DISK_SECTORS - BACKUP_SECTORS
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
        dev.read(DISK_SECTORS - BACKUP_SECTORS, &mut backup)
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

        for lba in [0, 1, 2, ALIGNMENT_SECTORS - 1, DISK_SECTORS - BACKUP_SECTORS, DISK_SECTORS - 1]
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
