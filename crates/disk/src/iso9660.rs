//! Загрузочный образ ISO 9660 с El Torito: то, что можно отдать человеку.
//!
//! # Зачем он, когда есть образ диска
//!
//! Затем, что образ диска нужно куда-то записать, а ISO — подключить. Гипервизор
//! (VirtualBox, VMware, Hyper-V) принимает его как привод одним пунктом меню, и
//! система стартует, ничего на диск не записывая. Это единственный способ
//! показать её тому, у кого нет ни QEMU, ни свободной машины, ни желания
//! разбираться ни в том, ни в другом.
//!
//! # Как это грузится
//!
//! Не через ISO 9660. Прошивка UEFI читает **El Torito** — таблицу загрузочных
//! записей, придуманную для CD, — находит там запись с платформой `0xEF` («EFI»)
//! и берёт из неё образ, который монтирует как обычный раздел FAT. Дальше всё
//! как с флешки: `\EFI\BOOT\BOOTX64.EFI` (или `BOOTAA64.EFI`) и наш загрузчик.
//!
//! Отсюда устройство образа: внутри ISO лежит целиком готовый том FAT32 — тот
//! же самый, который [`crate::fat32`] пишет на ESP, собранный тем же кодом. ISO
//! 9660 вокруг него нужен ровно для того, чтобы носитель считался носителем:
//! чтобы у него были дескриптор тома, таблица путей и корневой каталог.
//!
//! # Чего здесь нет намеренно
//!
//! * **Rock Ridge и Joliet.** Это расширения для длинных имён и прав в самом ISO
//!   9660. Файлы, которые важны, лежат внутри загрузочного образа FAT, а не в
//!   ISO, и единственный файл снаружи — текстовый.
//! * **BIOS-загрузки (`isohybrid`, `isolinux`).** Проект грузится только через
//!   UEFI, и вторая загрузочная запись означала бы вторую цепочку загрузки,
//!   которую нечем проверить.
//! * **Каталогов.** Корень плоский. Дерево в ISO потребовало бы полноценной
//!   таблицы путей с несколькими записями, а класть туда нечего.

use alloc::vec;
use alloc::vec::Vec;

/// Размер логического сектора ISO 9660. Не 512: у оптического носителя блок
/// всегда 2048 байт, и все поля дескрипторов считают именно в них.
pub const SECTOR_SIZE: usize = 2048;

/// Первые шестнадцать секторов — «системная область»: место под загрузочный код
/// тех платформ, что грузятся не через El Torito. У нас она нулевая.
const SYSTEM_AREA_SECTORS: usize = 16;

/// Сектор с основным дескриптором тома.
const PVD_SECTOR: usize = SYSTEM_AREA_SECTORS;
/// Сектор с загрузочной записью El Torito.
const BOOT_RECORD_SECTOR: usize = PVD_SECTOR + 1;
/// Сектор с признаком конца набора дескрипторов.
const TERMINATOR_SECTOR: usize = BOOT_RECORD_SECTOR + 1;
/// Сектор с каталогом загрузочных записей El Torito.
const BOOT_CATALOG_SECTOR: usize = TERMINATOR_SECTOR + 1;
/// Сектор с таблицей путей (обе, L и M, помещаются в один каждая).
const PATH_TABLE_L_SECTOR: usize = BOOT_CATALOG_SECTOR + 1;
const PATH_TABLE_M_SECTOR: usize = PATH_TABLE_L_SECTOR + 1;
/// Сектор с записями корневого каталога.
const ROOT_DIRECTORY_SECTOR: usize = PATH_TABLE_M_SECTOR + 1;
/// Первый сектор, доступный под содержимое.
const FIRST_DATA_SECTOR: usize = ROOT_DIRECTORY_SECTOR + 1;

/// Платформа `0xEF` в El Torito — это UEFI. Значение назначено спецификацией
/// UEFI (приложение о загрузке с оптического носителя), а не самим El Torito,
/// который знал только x86, PowerPC и Mac.
const PLATFORM_EFI: u8 = 0xEF;

/// Платформа `0x00` — x86. Стоит в проверочной записи, которая описывает
/// каталог целиком, а не конкретную загрузку: она досталась El Torito от BIOS и
/// к выбору загружаемого образа отношения не имеет.
const PLATFORM_X86: u8 = 0x00;

/// Что положить в образ.
pub struct Options<'a> {
    /// Метка тома: до 32 знаков, заглавные латинские буквы, цифры и `_`.
    pub label: &'a str,
    /// Готовый том FAT32 — тот, что прошивка смонтирует как ESP.
    pub boot_image: &'a [u8],
    /// Файлы, видимые в самом ISO. Имя — в формате ISO 9660 (8.3, заглавные).
    pub files: &'a [(&'a str, &'a [u8])],
}

/// Собрать образ целиком.
///
/// Возвращает готовые байты: писать их в файл или отдавать гипервизору — дело
/// вызывающего.
#[must_use]
pub fn build(options: &Options<'_>) -> Vec<u8> {
    // Раскладка считается заранее, до единой записи: сектор загрузочного образа
    // и его размер попадают в каталог El Torito, а тот лежит **раньше** самого
    // образа. Писать «вперёд по потоку» здесь нельзя.
    let boot_sectors = sectors_for(options.boot_image.len());
    let boot_start = FIRST_DATA_SECTOR;

    let mut file_layout = Vec::new();
    let mut next = boot_start + boot_sectors;
    for (name, data) in options.files {
        file_layout.push((*name, *data, next));
        next += sectors_for(data.len());
    }
    let total_sectors = next.max(FIRST_DATA_SECTOR + 1);

    let mut image = vec![0u8; total_sectors * SECTOR_SIZE];

    write_primary_descriptor(&mut image, options, total_sectors, &file_layout);
    write_boot_record(&mut image);
    write_terminator(&mut image);
    write_boot_catalog(&mut image, boot_start, options.boot_image.len());
    write_path_tables(&mut image);
    write_root_directory(&mut image, &file_layout);

    put_bytes(&mut image, boot_start * SECTOR_SIZE, options.boot_image);
    for (_, data, sector) in &file_layout {
        put_bytes(&mut image, sector * SECTOR_SIZE, data);
    }

    image
}

/// Сколько секторов занимает столько байт.
fn sectors_for(bytes: usize) -> usize {
    bytes.div_ceil(SECTOR_SIZE)
}

/// Основной дескриптор тома: то, по чему носитель опознаётся как ISO 9660.
fn write_primary_descriptor(
    image: &mut [u8],
    options: &Options<'_>,
    total_sectors: usize,
    files: &[(&str, &[u8], usize)],
) {
    let at = PVD_SECTOR * SECTOR_SIZE;
    image[at] = 1; // тип: основной дескриптор
    put_bytes(image, at + 1, b"CD001");
    image[at + 6] = 1; // версия

    // Идентификаторы системы и тома. Пробелы, а не нули: спецификация требует
    // строк, дополненных пробелами, и часть читателей на нулях спотыкается.
    put_padded(image, at + 8, 32, "");
    put_padded(image, at + 40, 32, options.label);

    put_both_u32(image, at + 80, total_sectors as u32); // размер тома в секторах
    put_both_u16(image, at + 120, 1); // число томов в наборе
    put_both_u16(image, at + 124, 1); // номер этого тома
    put_both_u16(image, at + 128, SECTOR_SIZE as u16);

    let path_table_size = path_table_bytes() as u32;
    put_both_u32(image, at + 132, path_table_size);
    put_u32_le(image, at + 140, PATH_TABLE_L_SECTOR as u32);
    put_u32_be(image, at + 148, PATH_TABLE_M_SECTOR as u32);

    // Запись корневого каталога лежит внутри дескриптора — 34 байта на месте.
    let root = directory_record(0, ROOT_DIRECTORY_SECTOR as u32, root_directory_bytes(files) as u32, true);
    put_bytes(image, at + 156, &root);

    // Прочие идентификаторы: пробелы. Заполнять их выдуманными названиями
    // издателя и подготовителя незачем — поля необязательные.
    for (offset, len) in [(190, 128), (318, 128), (446, 128), (574, 37), (702, 37), (739, 37)] {
        put_padded(image, at + offset, len, "");
    }

    // Даты тома: спецификация допускает «не задано» — все цифры нулевые.
    for offset in [813, 830, 847, 864] {
        put_bytes(image, at + offset, b"0000000000000000");
    }
    image[at + 881] = 1; // версия структуры файлов
}

/// Загрузочная запись El Torito: указывает, где лежит каталог записей.
fn write_boot_record(image: &mut [u8]) {
    let at = BOOT_RECORD_SECTOR * SECTOR_SIZE;
    image[at] = 0; // тип: загрузочная запись
    put_bytes(image, at + 1, b"CD001");
    image[at + 6] = 1;
    // Строка опознаётся прошивкой буквально: любое отличие — и El Torito на
    // носителе просто не будет найден.
    put_bytes(image, at + 7, b"EL TORITO SPECIFICATION");
    put_u32_le(image, at + 71, BOOT_CATALOG_SECTOR as u32);
}

/// Конец набора дескрипторов.
fn write_terminator(image: &mut [u8]) {
    let at = TERMINATOR_SECTOR * SECTOR_SIZE;
    image[at] = 0xFF;
    put_bytes(image, at + 1, b"CD001");
    image[at + 6] = 1;
}

/// Каталог загрузочных записей El Torito.
///
/// # Почему четыре записи, а не две
///
/// Потому что двух прошивке не хватает, и выяснилось это ровно так, как такие
/// вещи и выясняются: OVMF **увидел** носитель (в оболочке он числится как
/// `FS0: CDROM`, то есть ISO 9660 разобран), но грузиться с него не стал и ушёл
/// в UEFI Shell. Ни ошибки, ни строчки в журнале.
///
/// Причина в том, что каталог El Torito родом из мира BIOS, и EFI туда
/// добавлен **секцией**, а не заменой первых записей. Раскладка обязана быть
/// такой:
///
/// 1. проверочная запись — общая шапка каталога, платформа в ней `0x00`;
/// 2. запись по умолчанию — та, что грузил бы BIOS; у нас она помечена
///    незагрузочной, потому что BIOS-пути в проекте нет;
/// 3. заголовок секции с платформой `0xEF` — вот его и ищет прошивка UEFI;
/// 4. запись секции — где лежит образ и сколько его.
///
/// Соблазн выкинуть вторую запись велик (она ничего не грузит), но место под
/// неё занято по спецификации: заголовок секции обязан идти после записи по
/// умолчанию, а не вместо неё.
fn write_boot_catalog(image: &mut [u8], boot_start: usize, boot_bytes: usize) {
    let at = BOOT_CATALOG_SECTOR * SECTOR_SIZE;

    // 1. Проверочная запись. Содержательны в ней платформа и контрольная сумма:
    // сумма всех 16-битных слов записи обязана быть нулём, и по ней прошивка
    // отличает каталог от случайных байтов.
    image[at] = 0x01;
    image[at + 1] = PLATFORM_X86;
    put_padded(image, at + 4, 24, "FreeOS");
    image[at + 30] = 0x55;
    image[at + 31] = 0xAA;
    let checksum = catalog_checksum(&image[at..at + 32]);
    put_u16_le(image, at + 28, checksum);

    // 2. Запись по умолчанию: «не загружать». Ноль в первом байте — это и есть
    // «незагрузочная», и именно так помечают пустое место те, у кого нет
    // BIOS-пути.
    let default = at + 32;
    image[default] = 0x00;
    image[default + 1] = 0; // без эмуляции

    // 3. Заголовок секции EFI. `0x91` — «последний заголовок»: секций больше не
    // будет, и прошивке не нужно искать дальше.
    let header = at + 64;
    image[header] = 0x91;
    image[header + 1] = PLATFORM_EFI;
    put_u16_le(image, header + 2, 1); // записей в секции — одна
    put_padded(image, header + 4, 28, "UEFI");

    // 4. Запись секции: где образ и сколько его.
    let entry = at + 96;
    image[entry] = 0x88; // загрузочная
    image[entry + 1] = 0; // без эмуляции: образ монтируется как есть
    // Размер в «виртуальных секторах» по 512 байт. Поле шестнадцатибитное и при
    // большом образе переполняется; прошивки UEFI берут настоящий размер из
    // самой файловой системы, но ноль здесь часть из них считает ошибкой.
    let sectors_512 = boot_bytes.div_ceil(512).min(u16::MAX as usize) as u16;
    put_u16_le(image, entry + 6, sectors_512);
    put_u32_le(image, entry + 8, boot_start as u32);
}

/// Контрольная сумма проверочной записи: сумма 16-битных слов до нуля.
fn catalog_checksum(entry: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for word in entry.chunks_exact(2) {
        sum = sum.wrapping_add(u16::from_le_bytes([word[0], word[1]]));
    }
    // Дополнение до нуля: сумма всей записи вместе с этим полем даст ноль.
    sum.wrapping_neg()
}

/// Сколько байт занимает таблица путей с одним корнем.
fn path_table_bytes() -> usize {
    // Запись: длина имени (1), длина расширенной записи (1), сектор (4),
    // родитель (2), имя (1 байт — нулевой знак корня). Итого десять.
    10
}

/// Таблицы путей: одна с порядком байт от младшего, вторая от старшего.
///
/// Обе обязательны по спецификации, и обе описывают одно и то же дерево — здесь
/// состоящее из одного корня.
fn write_path_tables(image: &mut [u8]) {
    let l = PATH_TABLE_L_SECTOR * SECTOR_SIZE;
    image[l] = 1; // длина имени: один байт
    image[l + 1] = 0; // расширенной записи нет
    put_u32_le(image, l + 2, ROOT_DIRECTORY_SECTOR as u32);
    put_u16_le(image, l + 6, 1); // номер родителя: сам корень
    image[l + 8] = 0; // имя корня — один нулевой байт

    let m = PATH_TABLE_M_SECTOR * SECTOR_SIZE;
    image[m] = 1;
    image[m + 1] = 0;
    put_u32_be(image, m + 2, ROOT_DIRECTORY_SECTOR as u32);
    put_u16_be(image, m + 6, 1);
    image[m + 8] = 0;
}

/// Сколько байт занимают записи корневого каталога.
fn root_directory_bytes(files: &[(&str, &[u8], usize)]) -> usize {
    let mut bytes = 34 * 2; // «.» и «..»
    for (name, _, _) in files {
        bytes += record_len(name);
    }
    bytes.max(SECTOR_SIZE)
}

/// Длина записи каталога с таким именем: чётная, как требует спецификация.
fn record_len(name: &str) -> usize {
    let name_len = iso_name(name).len();
    let len = 33 + name_len;
    len + (len & 1)
}

/// Записи корневого каталога: «.», «..» и файлы.
fn write_root_directory(image: &mut [u8], files: &[(&str, &[u8], usize)]) {
    let at = ROOT_DIRECTORY_SECTOR * SECTOR_SIZE;
    let size = root_directory_bytes(files) as u32;

    let dot = directory_record(0, ROOT_DIRECTORY_SECTOR as u32, size, true);
    put_bytes(image, at, &dot);
    let dotdot = directory_record(1, ROOT_DIRECTORY_SECTOR as u32, size, true);
    put_bytes(image, at + 34, &dotdot);

    let mut offset = at + 68;
    for (name, data, sector) in files {
        let record = file_record(name, *sector as u32, data.len() as u32);
        // Запись не имеет права пересекать границу сектора: читатель берёт
        // каталог посекторно, и разорванная запись для него — конец каталога.
        if (offset % SECTOR_SIZE) + record.len() > SECTOR_SIZE {
            offset = (offset / SECTOR_SIZE + 1) * SECTOR_SIZE;
        }
        put_bytes(image, offset, &record);
        offset += record.len();
    }
}

/// Запись каталога для «.» или «..».
fn directory_record(name: u8, sector: u32, size: u32, directory: bool) -> [u8; 34] {
    let mut record = [0u8; 34];
    record[0] = 34;
    put_u32_le(&mut record, 2, sector);
    put_u32_be(&mut record, 6, sector);
    put_u32_le(&mut record, 10, size);
    put_u32_be(&mut record, 14, size);
    record[18..25].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0]); // дата не задана
    record[25] = if directory { 0x02 } else { 0 };
    put_u16_le(&mut record, 28, 1); // номер тома
    put_u16_be(&mut record, 30, 1);
    record[32] = 1; // длина имени
    record[33] = name; // 0 — «.», 1 — «..»
    record
}

/// Запись каталога для файла.
fn file_record(name: &str, sector: u32, size: u32) -> Vec<u8> {
    let name = iso_name(name);
    let mut record = vec![0u8; record_len(&name)];
    record[0] = record.len() as u8;
    put_u32_le(&mut record, 2, sector);
    put_u32_be(&mut record, 6, sector);
    put_u32_le(&mut record, 10, size);
    put_u32_be(&mut record, 14, size);
    record[25] = 0; // обычный файл
    put_u16_le(&mut record, 28, 1);
    put_u16_be(&mut record, 30, 1);
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name.as_bytes());
    record
}

/// Имя в том виде, в каком его принимает ISO 9660 без расширений: заглавные
/// буквы, цифры, подчёркивание, точка и обязательная версия `;1`.
///
/// Приводится, а не проверяется: имя приходит из кода этого репозитория, и
/// отказ здесь означал бы сломанную сборку вместо файла с непривычным именем.
fn iso_name(name: &str) -> alloc::string::String {
    let mut out = alloc::string::String::new();
    for ch in name.chars() {
        let ch = ch.to_ascii_uppercase();
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if !out.contains(';') {
        out.push_str(";1");
    }
    out
}

// ---------------------------------------------------------------------------
// Запись чисел
//
// ISO 9660 хранит числа **дважды**: сперва от младшего байта, следом от
// старшего. Так носитель читается процессором с любым порядком байт без
// перестановок — решение из времён, когда это было не праздным вопросом.
// ---------------------------------------------------------------------------

fn put_bytes(image: &mut [u8], at: usize, data: &[u8]) {
    image[at..at + data.len()].copy_from_slice(data);
}

/// Строка, дополненная пробелами до нужной длины и обрезанная по ней же.
fn put_padded(image: &mut [u8], at: usize, len: usize, text: &str) {
    for index in 0..len {
        image[at + index] = b' ';
    }
    let bytes = text.as_bytes();
    let copy = bytes.len().min(len);
    image[at..at + copy].copy_from_slice(&bytes[..copy]);
}

fn put_u16_le(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u16_be(image: &mut [u8], at: usize, value: u16) {
    image[at..at + 2].copy_from_slice(&value.to_be_bytes());
}

fn put_u32_le(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u32_be(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_be_bytes());
}

/// Число в обоих порядках подряд — так ISO 9660 хранит все размеры.
fn put_both_u16(image: &mut [u8], at: usize, value: u16) {
    put_u16_le(image, at, value);
    put_u16_be(image, at + 2, value);
}

fn put_both_u32(image: &mut [u8], at: usize, value: u32) {
    put_u32_le(image, at, value);
    put_u32_be(image, at + 4, value);
}

/// # Чем проверяется этот модуль
///
/// Не чужим читателем на хосте, в отличие от FAT32 и ext2, и исключение
/// объяснимо: единственная живая реализация ISO 9660 на Rust (`cdfs`) тянет
/// FUSE, который на Windows не собирается вовсе, а разработка идёт здесь.
///
/// Вместо него образ читает **настоящая прошивка UEFI**: сценарий стенда `iso`
/// грузит систему с этого носителя через OVMF — ту же edk2, что стоит в
/// VirtualBox и на реальных машинах. Проверка от этого не слабее, а сильнее:
/// посторонний крейт подтвердил бы согласие со спецификацией, а прошивка
/// подтверждает то, ради чего образ делается, — что с него грузятся.
///
/// Тесты ниже проверяют раскладку: поля, на которые прошивка смотрит, и сумму,
/// по которой она отличает каталог от мусора. Они ловят опечатку в смещении,
/// прогон в QEMU — ошибку в понимании формата.
#[cfg(test)]
mod tests {
    use super::*;

    /// Загрузочный образ в тестах — просто узнаваемые байты: ISO не обязан
    /// понимать, что внутри, и не должен их трогать.
    fn boot_image() -> Vec<u8> {
        let mut data = vec![0u8; SECTOR_SIZE * 3 + 17];
        data[0] = 0xAA;
        data[SECTOR_SIZE] = 0xBB;
        let last = data.len() - 1;
        data[last] = 0xCC;
        data
    }

    fn build_sample() -> Vec<u8> {
        let boot = boot_image();
        build(&Options {
            label: "FREEOS",
            boot_image: &boot,
            files: &[("README.TXT", b"boot me" as &[u8])],
        })
    }

    /// Носитель опознаётся как ISO 9660, а файл лежит там, куда указывает его
    /// запись в корневом каталоге.
    #[test]
    fn the_volume_looks_like_iso9660() {
        let image = build_sample();
        let pvd = PVD_SECTOR * SECTOR_SIZE;
        assert_eq!(image[pvd], 1, "не основной дескриптор");
        assert_eq!(&image[pvd + 1..pvd + 6], b"CD001", "нет сигнатуры ISO 9660");
        assert_eq!(&image[pvd + 40..pvd + 46], b"FREEOS", "метка тома потерялась");

        // Первая запись после «.» и «..» — наш файл.
        let record = ROOT_DIRECTORY_SECTOR * SECTOR_SIZE + 68;
        let name_len = image[record + 32] as usize;
        assert_eq!(
            &image[record + 33..record + 33 + name_len],
            b"README.TXT;1",
            "имя записано не по правилам ISO 9660"
        );

        let sector = u32::from_le_bytes([
            image[record + 2],
            image[record + 3],
            image[record + 4],
            image[record + 5],
        ]) as usize;
        let size = u32::from_le_bytes([
            image[record + 10],
            image[record + 11],
            image[record + 12],
            image[record + 13],
        ]) as usize;
        let at = sector * SECTOR_SIZE;
        assert_eq!(&image[at..at + size], b"boot me", "файл лежит не там, где сказано");
    }

    /// Загрузочный образ обязан лежать в секторе, указанном каталогом El Torito,
    /// и совпадать байт в байт: именно эти байты прошивка смонтирует как ESP.
    #[test]
    fn the_boot_image_is_where_the_catalog_says() {
        let boot = boot_image();
        let image = build(&Options { label: "FREEOS", boot_image: &boot, files: &[] });

        let catalog = BOOT_CATALOG_SECTOR * SECTOR_SIZE;
        assert_eq!(image[catalog], 0x01, "нет проверочной записи");
        assert_eq!(image[catalog + 64], 0x91, "нет заголовка последней секции");
        assert_eq!(image[catalog + 65], PLATFORM_EFI, "секция не для EFI");
        assert_eq!(image[catalog + 30], 0x55);
        assert_eq!(image[catalog + 31], 0xAA);
        assert_eq!(image[catalog + 96], 0x88, "запись секции не загрузочная");

        let start = u32::from_le_bytes([
            image[catalog + 104],
            image[catalog + 105],
            image[catalog + 106],
            image[catalog + 107],
        ]) as usize;
        let at = start * SECTOR_SIZE;
        assert_eq!(&image[at..at + boot.len()], &boot[..], "образ лежит не там");
    }

    /// Сумма всех слов проверочной записи обязана быть нулём — так прошивка
    /// отличает каталог El Torito от случайных байтов.
    #[test]
    fn the_validation_entry_sums_to_zero() {
        let image = build_sample();
        let at = BOOT_CATALOG_SECTOR * SECTOR_SIZE;
        let sum = image[at..at + 32]
            .chunks_exact(2)
            .fold(0u16, |acc, word| acc.wrapping_add(u16::from_le_bytes([word[0], word[1]])));
        assert_eq!(sum, 0, "контрольная сумма записи не сходится");
    }

    /// Загрузочная запись тома указывает на каталог, а не куда-нибудь.
    #[test]
    fn the_boot_record_points_at_the_catalog() {
        let image = build_sample();
        let at = BOOT_RECORD_SECTOR * SECTOR_SIZE;
        assert_eq!(&image[at + 1..at + 6], b"CD001");
        assert_eq!(&image[at + 7..at + 30], b"EL TORITO SPECIFICATION");
        let pointer = u32::from_le_bytes([
            image[at + 71],
            image[at + 72],
            image[at + 73],
            image[at + 74],
        ]);
        assert_eq!(pointer as usize, BOOT_CATALOG_SECTOR);
    }

    /// Размер тома в дескрипторе обязан совпадать с длиной файла: иначе читатель
    /// либо не найдёт хвост, либо уйдёт за конец.
    #[test]
    fn the_declared_size_matches_the_image() {
        let image = build_sample();
        let at = PVD_SECTOR * SECTOR_SIZE;
        let sectors = u32::from_le_bytes([image[at + 80], image[at + 81], image[at + 82], image[at + 83]]);
        assert_eq!(sectors as usize * SECTOR_SIZE, image.len());
    }
}
