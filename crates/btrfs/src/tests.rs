//! Тесты читателя на образе, сделанном **чужой** реализацией.
//!
//! Ни один байт в `tests/fixture.img.gz` не написан этим крейтом: том создан
//! `mkfs.btrfs` v6.8.1, а наполнен ядром Linux через обычный `mount`. Это и
//! есть весь смысл тестов — читатель, сверенный со своим же писателем,
//! доказывает внутреннюю согласованность, а нужна согласованность с форматом.
//!
//! Что лежит в образе (кладёт `cargo xtask btrfs-fixture`, он же держит
//! эталон в одном месте с этим списком):
//!
//! | путь | что проверяет |
//! |---|---|
//! | `/hello.txt` | встроенный экстент: данные лежат в самом дереве |
//! | `/mixed.bin` | 3000 байт — больше предела встраивания, меньше сектора |
//! | `/holes.bin` | дыра посередине при `NO_HOLES`: экстента нет вовсе |
//! | `/dir/big.bin` | 4 МиБ: несколько экстентов, тысяча контрольных сумм |
//! | `/dir/sub/small.txt` | вложенность каталогов |
//! | `/many/f0000…f1999` | дерево высотой больше нуля: внутренние узлы |
//! | имя из 255 знаков | предел длины имени в записи каталога |
//!
//! Образ намеренно **не** пересоздаётся тестом: на машине без Linux
//! `mkfs.btrfs` взять негде, а проверять разбор формата надо и там.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::io::Read as _;

use disk::{BlockDevice, MemDisk};

use crate::layout::{u32_at, u64_at};
use crate::read::{Btrfs, FileType};
use crate::{Attributes, Error, FormatOptions, Key, Problem, Writer, format, item_type};

/// Развернуть образ из репозитория в память.
fn fixture() -> MemDisk {
    let packed = include_bytes!("../tests/fixture.img.gz");
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut raw)
        .expect("образец тома не разворачивается");
    MemDisk::from_vec(raw).expect("образец тома не кратен сектору")
}

/// Имя в 255 знаков — то же, что кладёт `xtask btrfs-fixture`.
fn long_name() -> String {
    "n".repeat(255)
}

#[test]
fn mounts_and_reports_what_mkfs_wrote() {
    let mut disk = fixture();
    let fs = Btrfs::mount(&mut disk, 0).expect("том не смонтировался");

    assert_eq!(fs.label(), "FREEOS-FIXTURE");
    assert_eq!(fs.sector_size(), 4096);
    assert_eq!(fs.node_size(), 16384);
    assert_eq!(fs.usage().0, 128 * 1024 * 1024);
    assert!(fs.was_clean(), "образ отмонтирован чисто, журнала быть не должно");
    // Куски: системный, метаданных и данных как минимум. Число не жёсткое —
    // важно, что карта собрана не из одного лишь суперблока.
    assert!(fs.chunks() >= 3, "в карте всего {} кусков", fs.chunks());
}

#[test]
fn reads_the_root_directory() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let root = fs.root(&mut disk).unwrap();
    assert_eq!(root.kind, FileType::Directory);

    let names: Vec<String> = fs
        .list(&mut disk, &root)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();

    for expected in ["hello.txt", "dir", "holes.bin", "mixed.bin", "many"] {
        assert!(names.contains(&expected.to_string()), "нет {expected} в {names:?}");
    }
    assert!(names.contains(&long_name()), "имя в 255 знаков потерялось");
    // Ни «.», ни «..» в btrfs в дереве нет вовсе — проверяем, что мы их не
    // выдумали.
    assert!(!names.iter().any(|name| name == "." || name == ".."));
}

#[test]
fn reads_an_inline_extent() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/hello.txt").unwrap();
    assert_eq!(node.kind, FileType::Regular);
    assert_eq!(node.size, 17);
    assert_eq!(fs.read_file(&mut disk, &node).unwrap(), b"hello from btrfs\n");
}

#[test]
fn reads_a_file_shorter_than_a_sector() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/mixed.bin").unwrap();
    assert_eq!(node.size, 3000);
    let data = fs.read_file(&mut disk, &node).unwrap();
    assert_eq!(data.len(), 3000);
    assert!(data.iter().all(|&byte| byte == b'M'));
}

#[test]
fn nested_directories_resolve() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/dir/sub/small.txt").unwrap();
    assert_eq!(fs.read_file(&mut disk, &node).unwrap(), b"small\n");

    let dir = fs.resolve(&mut disk, "/dir").unwrap();
    assert_eq!(dir.kind, FileType::Directory);
    assert_eq!(fs.resolve(&mut disk, "/dir/sub").unwrap().kind, FileType::Directory);
    // Каталог — не файл, и попытка прочитать его обязана отличаться от пустого
    // ответа: молчаливый ноль здесь выглядел бы как пустой файл.
    assert_eq!(
        fs.read_at(&mut disk, &dir, 0, &mut [0u8; 16]).unwrap_err(),
        Error::IsADirectory
    );
}

#[test]
fn reads_four_megabytes_and_checks_every_sector() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/dir/big.bin").unwrap();
    assert_eq!(node.size, 4 * 1024 * 1024);

    let data = fs.read_file(&mut disk, &node).unwrap();
    assert_eq!(data.len(), 4 * 1024 * 1024);
    // Узор задан генератором образа: шестнадцать знаков по кругу.
    let pattern = b"0123456789abcdef";
    for (index, byte) in data.iter().enumerate() {
        assert_eq!(*byte, pattern[index % 16], "разошлось на байте {index}");
    }
}

#[test]
fn reads_from_the_middle_of_a_big_file() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/dir/big.bin").unwrap();

    // Чтение не с начала — отдельная проверка: экстент, покрывающий смещение,
    // находится шагом **назад** от результата поиска, и ошибка здесь видна
    // только на середине файла.
    let pattern = b"0123456789abcdef";
    for start in [1u64, 4095, 4096, 100_000, 1_048_576, 4 * 1024 * 1024 - 10] {
        let mut buf = [0u8; 64];
        let read = fs.read_at(&mut disk, &node, start, &mut buf).unwrap();
        assert!(read > 0, "нулевое чтение со смещения {start}");
        for (index, byte) in buf[..read].iter().enumerate() {
            let at = start as usize + index;
            assert_eq!(*byte, pattern[at % 16], "разошлось на байте {at}");
        }
    }
    // За концом файла читать нечего, и это не ошибка.
    assert_eq!(fs.read_at(&mut disk, &node, node.size, &mut [0u8; 8]).unwrap(), 0);
}

#[test]
fn a_hole_reads_as_zeros() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/holes.bin").unwrap();
    assert_eq!(node.size, 1024 * 1024);

    let data = fs.read_file(&mut disk, &node).unwrap();
    assert_eq!(data.len(), 1024 * 1024);
    assert!(data[..4096].iter().all(|&byte| byte == b'A'), "первый сектор не тот");
    assert!(
        data[4096..1024 * 1024 - 4096].iter().all(|&byte| byte == 0),
        "дыра прочиталась не нулями"
    );
    assert!(
        data[1024 * 1024 - 4096..].iter().all(|&byte| byte == b'Z'),
        "последний сектор не тот"
    );
}

#[test]
fn walks_a_directory_of_two_thousand_files() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let many = fs.resolve(&mut disk, "/many").unwrap();

    let entries = fs.list(&mut disk, &many).unwrap();
    assert_eq!(entries.len(), 2000, "каталог обошёлся не целиком");

    // Обход через несколько листьев дерева проверяется именно так: у каталога
    // из двух тысяч записей элементы не помещаются в один узел, и переход
    // между листьями — самое хрупкое место обхода.
    let mut names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
    names.sort();
    assert_eq!(names[0], "f0000");
    assert_eq!(names[1999], "f1999");

    // Поиск по имени идёт по хешу, а не перебором — и обязан находить то же
    // самое.
    for probe in ["f0000", "f0777", "f1999"] {
        let node = fs
            .lookup(&mut disk, &many, probe)
            .unwrap()
            .unwrap_or_else(|| panic!("{probe} не нашёлся"));
        let inode = fs.inode(&mut disk, node.inode).unwrap();
        let text = fs.read_file(&mut disk, &inode).unwrap();
        let number: u32 = probe[1..].parse().unwrap();
        assert_eq!(text, format!("file {number:04}\n").as_bytes());
    }
    assert!(fs.lookup(&mut disk, &many, "f2000").unwrap().is_none());
}

#[test]
fn a_name_of_two_hundred_fifty_five_characters_survives() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let path = format!("/{}", long_name());
    let node = fs.resolve(&mut disk, &path).unwrap();
    assert_eq!(fs.read_file(&mut disk, &node).unwrap(), b"long name\n");
}

#[test]
fn missing_names_are_not_found() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    assert_eq!(fs.resolve(&mut disk, "/nothing").unwrap_err(), Error::NotFound);
    assert_eq!(
        fs.resolve(&mut disk, "/hello.txt/deeper").unwrap_err(),
        Error::NotADirectory
    );
}

#[test]
fn ownership_and_mode_come_from_the_inode() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    // Образ наполнялся от root — то есть 0:0, и файлы 0644, каталоги 0755.
    let node = fs.resolve(&mut disk, "/hello.txt").unwrap();
    assert_eq!((node.uid, node.gid), (0, 0));
    assert_eq!(node.mode, 0o644);
    assert_eq!(node.links, 1);
    assert!(node.mtime > 1_700_000_000, "время правки не похоже на настоящее");

    let dir = fs.resolve(&mut disk, "/dir").unwrap();
    assert_eq!(dir.mode, 0o755);
}

#[test]
fn a_broken_checksum_is_reported_and_not_swallowed() {
    // Портим один байт данных `/dir/big.bin` и требуем, чтобы чтение отказало.
    // Это единственная проверка, ради которой btrfs здесь вообще появился:
    // ext2 отдал бы испорченный байт молча.
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/dir/big.bin").unwrap();
    assert!(fs.read_file(&mut disk, &node).is_ok());

    let spoiled = {
        let mut raw = disk.into_vec();
        // Ищем узор в первом мегабайте данных и портим его. Точный адрес
        // экстента знать не нужно — достаточно того, что этот кусок данных
        // принадлежит файлу и накрыт суммой.
        let at = find_pattern(&raw, b"0123456789abcdef0123456789abcdef")
            .expect("узор файла не нашёлся в образе");
        raw[at] ^= 0xFF;
        raw
    };
    let mut disk = MemDisk::from_vec(spoiled).unwrap();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/dir/big.bin").unwrap();
    assert_eq!(
        fs.read_file(&mut disk, &node).unwrap_err(),
        Error::BadChecksum,
        "порча данных прошла незамеченной"
    );
}

#[test]
fn a_volume_without_the_signature_is_refused() {
    let mut raw = {
        let disk = fixture();
        disk.into_vec()
    };
    // Портим подпись во всех копиях суперблока, какие есть в образе.
    for offset in [0x1_0000usize, 0x400_0000] {
        if offset + 72 <= raw.len() {
            raw[offset + 64..offset + 72].fill(0);
        }
    }
    let mut disk = MemDisk::from_vec(raw).unwrap();
    assert!(matches!(
        Btrfs::mount(&mut disk, 0),
        Err(Error::Corrupt | Error::TooSmall)
    ));
}

#[test]
fn keys_order_the_way_the_format_says() {
    // Дерево сумм живёт под объектом −10, то есть 0xFFFF…F6, и при знаковом
    // сравнении оказалось бы **перед** корнем тома. Ошибка выглядела бы как
    // «сумм нет», то есть как порча данных.
    let csums = Key::new(u64::MAX - 9, item_type::EXTENT_CSUM, 0);
    let fs_tree = Key::new(5, item_type::ROOT_ITEM, 0);
    assert!(fs_tree < csums);

    // Переход к следующему ключу обязан переносить разряд, а не застревать.
    let last = Key::new(7, item_type::EXTENT_CSUM, u64::MAX);
    assert_eq!(last.next(), Key::new(7, item_type::EXTENT_CSUM + 1, 0));
    let very_last = Key::new(7, u8::MAX, u64::MAX);
    assert_eq!(very_last.next(), Key::new(8, 0, 0));
}

/// Первое вхождение образца в буфер.
fn find_pattern(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn a_clean_volume_checks_out_clean() {
    let mut disk = fixture();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let report = fs.check(&mut disk).expect("проверка не дошла до конца");

    assert!(report.is_clean(), "на чистом томе нашлось: {:?}", report.problems);
    // Две тысячи мелких файлов плюс шесть в корне и один в `dir/sub`. Точное
    // число здесь уместно: образец собирается рецептом, а не наугад, и
    // расхождение означало бы, что обход дерева не дошёл до конца.
    assert_eq!(report.files, 2000 + 5 + 1, "файлов найдено {}", report.files);
    // Корень, `dir`, `dir/sub`, `many`.
    assert_eq!(report.directories, 4, "каталогов найдено {}", report.directories);
    // Четыре мегабайта, мегабайт с дырой, три килобайта и мелочь.
    assert!(report.bytes > 5 * 1024 * 1024, "суммарный размер {}", report.bytes);
    // Сверены настоящие сектора, а не ноль: проверка, которая ничего не
    // прочитала, отчиталась бы точно так же.
    assert!(report.sectors > 1000, "сверено секторов {}", report.sectors);
}

#[test]
fn the_check_finds_broken_data_and_keeps_going() {
    let spoiled = {
        let disk = fixture();
        let mut raw = disk.into_vec();
        let at = find_pattern(&raw, b"0123456789abcdef0123456789abcdef")
            .expect("узор файла не нашёлся в образе");
        raw[at] ^= 0xFF;
        raw
    };
    let mut disk = MemDisk::from_vec(spoiled).unwrap();
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let report = fs.check(&mut disk).expect("проверка обязана дойти до конца");

    assert!(!report.is_clean(), "порча прошла незамеченной");
    assert!(
        report.problems.iter().any(|problem| matches!(problem, Problem::BadData { .. })),
        "нашлось не то: {:?}",
        report.problems
    );
    // Главное во всей проверке: найдя порчу, она не бросает том. Остальные
    // файлы обязаны быть просмотрены — иначе один испорченный сектор скрывал
    // бы все следующие.
    assert_eq!(report.files, 2006, "обход оборвался: файлов {}", report.files);
    assert!(!crate::describe(&report.problems[0]).unwrap().is_empty());
}

// --- свой mkfs ---------------------------------------------------------------
//
// Здесь том создаёт этот же крейт, и сам по себе такой тест доказывает только
// согласованность с собой — общая ошибка в понимании формата прошла бы и тут.
// Чужой взгляд на те же байты — `cargo xtask btrfs-linux-check` (`btrfs check`
// и ядро Linux). В `cargo test` остаётся то, что можно привязать к чужому без
// Linux: числа из дампа тома от `mkfs.btrfs` и поведение поверх такого тома.

const MIB: u64 = 1024 * 1024;

fn options() -> FormatOptions<'static> {
    FormatOptions {
        label: "FREEOS-STATE",
        uuid: [
            0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x33, 0x33, 0x44, 0x44, 0x55, 0x55, 0x55, 0x55,
            0x55, 0x55,
        ],
        time: 1_789_289_131,
    }
}

#[test]
fn our_mkfs_mounts_with_our_reader() {
    let sectors = 128 * MIB / 512;
    let mut disk = MemDisk::new(sectors).unwrap();
    let mut fs = format(&mut disk, 0, sectors, &options()).expect("том не создался");

    assert_eq!(fs.label(), "FREEOS-STATE");
    assert_eq!((fs.sector_size(), fs.node_size()), (4096, 16384));
    assert_eq!(fs.usage(), (128 * MIB, 9 * 16384));
    assert!(fs.was_clean());
    assert_eq!(fs.chunks(), 3, "куски: системный, метаданных, данных");

    let root = fs.root(&mut disk).unwrap();
    assert_eq!(root.kind, FileType::Directory);
    assert_eq!((root.mode, root.uid, root.gid), (0o755, 0, 0));
    assert_eq!(root.mtime, 1_789_289_131);
    assert!(fs.list(&mut disk, &root).unwrap().is_empty(), "свежий том не пуст");

    let report = fs.check(&mut disk).expect("проверка не дошла до конца");
    assert!(report.is_clean(), "на свежем томе нашлось: {:?}", report.problems);
    assert_eq!((report.inodes, report.files, report.directories), (1, 0, 1));
}

#[test]
fn the_superblock_says_what_mkfs_says() {
    // Числа — из `btrfs inspect-internal dump-super` и `dump-tree` тома, который
    // `mkfs.btrfs` 6.8.1 создал с теми же параметрами (`-m single -d single`,
    // 128 МиБ). Совпадение с ними — единственное в этом разделе, что привязывает
    // наш mkfs к чужому без Linux.
    let sectors = 128 * MIB / 512;
    let mut disk = MemDisk::new(sectors).unwrap();
    format(&mut disk, 0, sectors, &options()).unwrap();
    let raw = disk.as_bytes();

    let sb = &raw[0x1_0000..0x1_0000 + 4096];
    assert_eq!(u64_at(sb, 120), 147_456, "bytes_used: девять узлов");
    assert_eq!(u32_at(sb, 160), 97, "sys_array_size: ключ и кусок с одной полосой");
    assert_eq!(u64_at(sb, 128), 6, "root_dir");
    assert_eq!(u64_at(sb, 180), 0x3, "compat_ro: дерево свободного места");
    assert_eq!(u64_at(sb, 188), 0x341, "incompat");
    assert_eq!(u32_at(sb, 156), 4096, "stripesize");
    assert_eq!(u64_at(sb, 201 + 16), 20_971_520, "dev_item.bytes_used: 4 + 8 + 8 МиБ");
    assert_eq!(&sb[299..299 + 12], b"FREEOS-STATE");

    // Копия на 64 МиБ знает своё место — и потому сумма у неё своя.
    let at = (64 * MIB) as usize;
    let mirror = &raw[at..at + 4096];
    assert_eq!(&mirror[64..72], b"_BHRfS_M");
    assert_eq!(u64_at(mirror, 48), 64 * MIB);
    assert_ne!(mirror[..4], sb[..4]);

    // Хеш имени `default` в каталоге дерева корней — из того же дампа.
    assert_eq!(crate::crc32c::name_hash(b"default"), 2_378_154_706);
}

#[test]
fn formats_a_partition_on_a_4k_disk_without_touching_its_neighbours() {
    let first_lba = 2048u64; // 8 МиБ при секторе 4096
    let sectors = 128 * MIB / 4096;
    let mut disk = MemDisk::with_sector_size(first_lba + sectors + 16, 4096).unwrap();
    let marker = [0xA5u8; 4096];
    disk.write(first_lba - 1, &marker).unwrap();
    disk.write(first_lba + sectors, &marker).unwrap();

    let mut fs = format(&mut disk, first_lba, sectors, &options()).expect("том не создался");
    let root = fs.root(&mut disk).unwrap();
    assert!(fs.list(&mut disk, &root).unwrap().is_empty());
    assert!(crate::detect(&mut disk, first_lba));

    let mut back = [0u8; 4096];
    disk.read(first_lba - 1, &mut back).unwrap();
    assert_eq!(back, marker, "затёрт сектор перед разделом");
    disk.read(first_lba + sectors, &mut back).unwrap();
    assert_eq!(back, marker, "затёрт сектор после раздела");
}

#[test]
fn a_volume_too_small_or_a_label_too_long_is_refused_before_any_write() {
    let sectors = 64 * MIB / 512;
    let mut disk = MemDisk::new(sectors).unwrap();
    assert_eq!(format(&mut disk, 0, sectors, &options()).err(), Some(Error::TooSmall));

    let sectors = 128 * MIB / 512;
    let mut disk = MemDisk::new(sectors).unwrap();
    let long = "x".repeat(256);
    let refused = format(&mut disk, 0, sectors, &FormatOptions { label: &long, ..options() });
    assert_eq!(refused.err(), Some(Error::BadName));
    // Раздел, выходящий за конец диска.
    assert_eq!(format(&mut disk, 1, sectors, &options()).err(), Some(Error::TooSmall));
    assert!(disk.as_bytes().iter().all(|&byte| byte == 0), "отказ после записи");
}

/// Носитель, который отказывает после заданного числа записей.
struct Failing {
    inner: MemDisk,
    writes_left: usize,
}

impl BlockDevice for Failing {
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.inner.sector_count()
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> core::result::Result<(), disk::Error> {
        self.inner.read(lba, buf)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> core::result::Result<(), disk::Error> {
        if self.writes_left == 0 {
            // Вид отказа неважен: форматирование обязано остановиться на любом.
            return Err(disk::Error::OutOfRange);
        }
        self.writes_left -= 1;
        self.inner.write(lba, buf)
    }

    fn flush(&mut self) -> core::result::Result<(), disk::Error> {
        self.inner.flush()
    }
}

#[test]
fn an_interrupted_format_never_mounts_a_mixture() {
    // Форматируем поверх тома от `mkfs.btrfs` и обрываем запись на каждом шаге
    // по очереди. Старые деревья лежат по тем же адресам, что и новые, а копия
    // старого суперблока — на 64 МиБ. Допустимых исходов три: старый том
    // целиком, никакого тома или новый пустой. Недопустим четвёртый — старый
    // суперблок поверх наполовину новых деревьев.
    let pristine = fixture().into_vec();
    let mut completed = false;
    for limit in 0..=32 {
        let inner = MemDisk::from_vec(pristine.clone()).unwrap();
        let sectors = inner.sector_count();
        let mut dev = Failing { inner, writes_left: limit };
        let finished = format(&mut dev, 0, sectors, &options()).is_ok();
        completed |= finished;

        let mut disk = dev.inner;
        let Ok(mut fs) = Btrfs::mount(&mut disk, 0) else {
            assert!(!finished, "форматирование закончилось, а том не монтируется");
            continue;
        };
        let root = fs.root(&mut disk).unwrap_or_else(|err| panic!("обрыв после {limit}: {err}"));
        let names = fs
            .list(&mut disk, &root)
            .unwrap_or_else(|err| panic!("обрыв после {limit}: каталог не читается: {err}"));
        if names.is_empty() {
            assert_eq!(fs.label(), "FREEOS-STATE", "обрыв после {limit}: пустой, но не новый");
        } else {
            assert!(!finished, "форматирование закончилось, а том старый");
            let hello = fs.resolve(&mut disk, "/hello.txt").unwrap();
            assert_eq!(
                fs.read_file(&mut disk, &hello).unwrap(),
                b"hello from btrfs\n",
                "обрыв после {limit}: старый том смонтировался испорченным"
            );
        }
    }
    assert!(completed, "ни одно форматирование не дошло до конца — предел мал");
}

// --- запись в существующий том ------------------------------------------------
//
// Писатель проверяется нашим же читателем, и это опять согласованность с собой.
// Чужой взгляд — `cargo xtask btrfs-linux-check`, где записанное читает Linux.
// Здесь — то, что можно без Linux: запись поверх тома от `mkfs.btrfs`,
// наполненного ядром, десятки транзакций подряд, новый кусок, обрыв фиксации на
// каждой записи и нехватка места.

fn attrs() -> Attributes {
    Attributes { mode: 0o644, uid: 1000, gid: 1000, time: 1_789_300_000 }
}

fn dir_attrs() -> Attributes {
    Attributes { mode: 0o755, ..attrs() }
}

/// Узор, у которого соседние сектора различаются: сдвинутый на сектор экстент
/// прочитался бы не тем, а не тем же самым.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| ((at / 4096) as u8).wrapping_mul(97) ^ (at as u8).wrapping_mul(31) ^ seed)
        .collect()
}

fn fresh_volume(mib: u64) -> MemDisk {
    let sectors = mib * MIB / 512;
    let mut disk = MemDisk::new(sectors).unwrap();
    format(&mut disk, 0, sectors, &options()).expect("том не создался");
    disk
}

fn read_path(disk: &mut MemDisk, path: &str) -> Vec<u8> {
    let mut fs = Btrfs::mount(disk, 0).expect("том не монтируется");
    let node = fs.resolve(disk, path).unwrap_or_else(|err| panic!("{path}: {err}"));
    fs.read_file(disk, &node).unwrap_or_else(|err| panic!("{path}: {err}"))
}

fn assert_clean(disk: &mut MemDisk) -> crate::Report {
    let mut fs = Btrfs::mount(disk, 0).expect("том не монтируется");
    let report = fs.check(disk).expect("проверка не дошла до конца");
    assert!(report.is_clean(), "нашлось: {:?}", report.problems);
    report
}

#[test]
fn writes_files_and_directories_that_our_reader_reads_back() {
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).expect("том не открылся на запись");
    let etc = writer.create_directory(256, "etc", &dir_attrs()).unwrap();
    let home = writer.create_directory(256, "home", &dir_attrs()).unwrap();
    let roman = writer.create_directory(home, "roman", &dir_attrs()).unwrap();
    // Три вида файла: встроенный, пустой (экстента нет вовсе) и обычный.
    writer.create_file(&mut disk, etc, "hostname", b"freeos\n", &attrs()).unwrap();
    writer.create_file(&mut disk, etc, "empty", b"", &attrs()).unwrap();
    let mixed = pattern(3000, 1);
    writer.create_file(&mut disk, etc, "mixed.bin", &mixed, &attrs()).unwrap();
    let big = pattern(1024 * 1024 + 123, 2);
    writer.create_file(&mut disk, roman, "big.bin", &big, &attrs()).unwrap();
    assert_eq!(writer.resolve("/home/roman").unwrap(), roman, "писатель видит несохранённое");

    // До фиксации том на диске прежний: данные уже записаны, но на них не
    // ссылается ни один суперблок.
    {
        let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
        let root = fs.root(&mut disk).unwrap();
        assert!(fs.list(&mut disk, &root).unwrap().is_empty(), "видно до фиксации");
    }

    writer.commit(&mut disk).expect("фиксация не удалась");
    assert_eq!(writer.generation(), 2);

    assert_eq!(read_path(&mut disk, "/etc/hostname"), b"freeos\n");
    assert_eq!(read_path(&mut disk, "/etc/empty"), b"");
    assert_eq!(read_path(&mut disk, "/etc/mixed.bin"), mixed);
    assert_eq!(read_path(&mut disk, "/home/roman/big.bin"), big);

    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    assert_eq!(fs.generation(), 2);
    let node = fs.resolve(&mut disk, "/home/roman/big.bin").unwrap();
    assert_eq!((node.mode, node.uid, node.gid, node.mtime), (0o644, 1000, 1000, 1_789_300_000));
    let etc_node = fs.resolve(&mut disk, "/etc").unwrap();
    let names: Vec<String> = fs
        .list(&mut disk, &etc_node)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["hostname", "empty", "mixed.bin"], "порядок перечисления — порядок создания");
    drop(fs);

    let report = assert_clean(&mut disk);
    assert_eq!((report.files, report.directories), (4, 4));
    // Один сектор у `mixed.bin` и 257 у `big.bin`: хвост сверен вместе с нулями.
    assert_eq!(report.sectors, 1 + 257);
}

#[test]
fn many_transactions_reuse_the_space_they_free() {
    // Каждая фиксация перестраивает дерево ФС на новом месте. Без повторного
    // использования освобождённых узлов сорок транзакций исписали бы
    // восьмимегабайтный кусок метаданных и потребовали бы нового — поэтому
    // проверяется число кусков, а не объём.
    let mut disk = fresh_volume(128);
    for round in 0..40u32 {
        let mut writer =
            Writer::open(&mut disk, 0).unwrap_or_else(|err| panic!("круг {round}: {err}"));
        let dir = writer.create_directory(256, &format!("round{round:02}"), &dir_attrs()).unwrap();
        for n in 0..60u32 {
            let text = format!("round {round} file {n}\n");
            writer.create_file(&mut disk, dir, &format!("f{n:03}"), text.as_bytes(), &attrs()).unwrap();
        }
        writer.commit(&mut disk).unwrap_or_else(|err| panic!("круг {round}: {err}"));
    }

    let fs = Btrfs::mount(&mut disk, 0).unwrap();
    assert_eq!(fs.generation(), 41);
    assert_eq!(fs.chunks(), 3, "освобождённые узлы не пошли в дело");
    drop(fs);
    assert_eq!(read_path(&mut disk, "/round07/f042"), b"round 7 file 42\n");
    assert_eq!(read_path(&mut disk, "/round39/f059"), b"round 39 file 59\n");
    let report = assert_clean(&mut disk);
    assert_eq!((report.files, report.directories), (2400, 41));
}

#[test]
fn a_file_larger_than_the_data_chunk_gets_a_new_chunk() {
    let mut disk = fresh_volume(128);
    let big = pattern(20 * MIB as usize, 3);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    writer.create_file(&mut disk, 256, "twenty.bin", &big, &attrs()).unwrap();
    writer.commit(&mut disk).unwrap();

    let fs = Btrfs::mount(&mut disk, 0).unwrap();
    assert!(fs.chunks() >= 4, "кусков {} — новый не заведён", fs.chunks());
    drop(fs);
    assert_eq!(read_path(&mut disk, "/twenty.bin"), big);
    let report = assert_clean(&mut disk);
    assert_eq!(report.sectors, 20 * MIB / 4096);
}

#[test]
fn writes_into_a_volume_that_mkfs_and_linux_made() {
    // Дерево ФС здесь многоуровневое, дерево свободного места построено ядром
    // Linux, и ни одного байта не писал этот крейт.
    let mut disk = fixture();
    let mut writer = Writer::open(&mut disk, 0).expect("том от mkfs.btrfs не открылся на запись");
    let dir = writer.resolve("/dir").unwrap();
    writer.create_file(&mut disk, dir, "ours.txt", b"written by freeos\n", &attrs()).unwrap();
    let blob = pattern(300_000, 4);
    writer.create_file(&mut disk, 256, "ours.bin", &blob, &attrs()).unwrap();
    assert_eq!(
        writer.create_file(&mut disk, 256, "hello.txt", b"x", &attrs()).err(),
        Some(Error::Exists)
    );
    writer.commit(&mut disk).expect("фиксация поверх тома от Linux не удалась");

    assert_eq!(read_path(&mut disk, "/dir/ours.txt"), b"written by freeos\n");
    assert_eq!(read_path(&mut disk, "/ours.bin"), blob);
    assert_eq!(read_path(&mut disk, "/hello.txt"), b"hello from btrfs\n");
    assert_eq!(read_path(&mut disk, "/many/f1999"), b"file 1999\n");
    let big = read_path(&mut disk, "/dir/big.bin");
    let expected = b"0123456789abcdef";
    assert!(big.iter().enumerate().all(|(at, byte)| *byte == expected[at % 16]), "чужой файл испорчен");
    let report = assert_clean(&mut disk);
    assert_eq!(report.files, 2006 + 2);
}

#[test]
fn an_interrupted_commit_leaves_the_old_volume_or_the_new_one() {
    let mut base = fresh_volume(128);
    {
        let mut writer = Writer::open(&mut base, 0).unwrap();
        writer.create_file(&mut base, 256, "before.txt", b"committed\n", &attrs()).unwrap();
        writer.commit(&mut base).unwrap();
    }
    let pristine = base.into_vec();
    let payload = pattern(200_000, 5);

    let (mut saw_old, mut saw_new) = (false, false);
    for limit in 0..80 {
        let inner = MemDisk::from_vec(pristine.clone()).unwrap();
        let mut dev = Failing { inner, writes_left: limit };
        let finished = {
            let mut attempt = || -> crate::Result<()> {
                let mut writer = Writer::open(&mut dev, 0)?;
                let dir = writer.create_directory(256, "after", &dir_attrs())?;
                writer.create_file(&mut dev, dir, "payload.bin", &payload, &attrs())?;
                writer.commit(&mut dev)
            };
            attempt().is_ok()
        };

        let mut disk = dev.inner;
        let generation = Btrfs::mount(&mut disk, 0)
            .unwrap_or_else(|err| panic!("обрыв после {limit}: том не монтируется: {err}"))
            .generation();
        assert_eq!(read_path(&mut disk, "/before.txt"), b"committed\n", "обрыв после {limit}");
        match generation {
            2 => {
                assert!(!finished, "фиксация закончилась, а поколение старое");
                let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
                assert_eq!(fs.resolve(&mut disk, "/after").err(), Some(Error::NotFound));
                saw_old = true;
            }
            3 => {
                assert_eq!(read_path(&mut disk, "/after/payload.bin"), payload, "обрыв после {limit}");
                saw_new = true;
            }
            other => panic!("обрыв после {limit}: поколение {other}"),
        }
        assert_clean(&mut disk);
    }
    assert!(saw_old && saw_new, "обрывы не накрыли обе стороны фиксации");
}

#[test]
fn the_writer_refuses_what_it_must() {
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let file = writer.create_file(&mut disk, 256, "file", b"x", &attrs()).unwrap();
    assert_eq!(writer.create_file(&mut disk, 256, "file", b"y", &attrs()).err(), Some(Error::Exists));
    assert_eq!(writer.create_directory(file, "sub", &dir_attrs()).err(), Some(Error::NotADirectory));
    assert_eq!(writer.create_directory(256, "a/b", &dir_attrs()).err(), Some(Error::BadName));
    assert_eq!(writer.create_directory(256, "", &dir_attrs()).err(), Some(Error::BadName));
    assert_eq!(writer.create_directory(4242, "x", &dir_attrs()).err(), Some(Error::NotFound));
    // Отказы до изменений транзакцию не портят.
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/file"), b"x");
    assert_clean(&mut disk);
}

#[test]
fn running_out_of_space_is_an_error_not_a_broken_volume() {
    let mut disk = fresh_volume(128);
    let chunk = pattern(16 * MIB as usize, 6);
    let mut stored = 0u32;
    loop {
        assert!(stored < 16, "128 МиБ не могут вместить {stored} файлов по 16 МиБ");
        let mut writer = Writer::open(&mut disk, 0).unwrap();
        match writer.create_file(&mut disk, 256, &format!("fill{stored:02}"), &chunk, &attrs()) {
            Ok(_) => match writer.commit(&mut disk) {
                Ok(()) => stored += 1,
                Err(err) => {
                    assert_eq!(err, Error::NoSpace);
                    break;
                }
            },
            Err(err) => {
                assert_eq!(err, Error::NoSpace);
                // Сорвавшаяся на середине операция не доедет до диска.
                assert_eq!(writer.commit(&mut disk).err(), Some(Error::Aborted));
                break;
            }
        }
    }
    assert!(stored >= 4, "поместилось всего {stored} файлов по 16 МиБ");
    let last = format!("/fill{:02}", stored - 1);
    assert_eq!(read_path(&mut disk, &last), chunk);
    let report = assert_clean(&mut disk);
    assert_eq!(report.files, u64::from(stored));
}

// --- изменение существующего --------------------------------------------------
//
// Запись поверх, укорачивание, удаление и переименование. Как и выше, наш
// читатель согласен с нашим писателем — это не доказательство формата, его даёт
// `cargo xtask btrfs-linux-check`. Здесь то, что без Linux: содержимое до байта,
// освобождение места, отказы и обрыв фиксации посреди правки.

const LATER: u64 = 1_789_400_000;

#[test]
fn overwrites_the_middle_of_a_file_and_writes_past_its_end() {
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let mut expected = pattern(200_000, 8);
    let file = writer.create_file(&mut disk, 256, "data.bin", &expected, &attrs()).unwrap();
    writer.commit(&mut disk).unwrap();

    // Первый и последний сектора тронуты не целиком: их прежние байты обязаны
    // уцелеть, а их суммы — сверены при чтении.
    let patch = pattern(10_000, 9);
    writer.write_at(&mut disk, file, 5_000, &patch, LATER).unwrap();
    expected[5_000..15_000].copy_from_slice(&patch);
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/data.bin"), expected);

    // Запись за концом оставляет дыру, которая читается нулями.
    writer.write_at(&mut disk, file, 300_000, b"tail", LATER).unwrap();
    expected.resize(300_000, 0);
    expected.extend_from_slice(b"tail");
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/data.bin"), expected);

    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let node = fs.resolve(&mut disk, "/data.bin").unwrap();
    assert_eq!((node.size, node.mtime), (300_004, LATER as i64));
    drop(fs);
    assert_clean(&mut disk);
}

#[test]
fn small_files_grow_out_of_the_leaf_and_shrink_back() {
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let file = writer.create_file(&mut disk, 256, "note.txt", b"hello\n", &attrs()).unwrap();
    writer.write_at(&mut disk, file, 6, b"world\n", LATER).unwrap();
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/note.txt"), b"hello\nworld\n");

    // Больше листа — встроенное содержимое переезжает в экстент.
    let big = pattern(5000, 10);
    writer.write_at(&mut disk, file, 0, &big, LATER).unwrap();
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/note.txt"), big);

    // Укоротили и снова продлили: хвост последнего сектора обязан быть нулями,
    // а не тем, что там лежало до укорачивания.
    writer.truncate(&mut disk, file, 3000, LATER).unwrap();
    writer.truncate(&mut disk, file, 5000, LATER).unwrap();
    writer.commit(&mut disk).unwrap();
    let mut expected = big[..3000].to_vec();
    expected.resize(5000, 0);
    assert_eq!(read_path(&mut disk, "/note.txt"), expected);
    assert_clean(&mut disk);

    writer.truncate(&mut disk, file, 0, LATER).unwrap();
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/note.txt"), b"");
    let report = assert_clean(&mut disk);
    assert_eq!(report.sectors, 0, "суммы укороченного до нуля файла остались");

    // Пустой файл снова становится встроенным.
    writer.write_at(&mut disk, file, 0, b"x", LATER).unwrap();
    writer.commit(&mut disk).unwrap();
    assert_eq!(read_path(&mut disk, "/note.txt"), b"x");
    assert_clean(&mut disk);
}

#[test]
fn deleting_frees_data_and_checksums() {
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let big = pattern(4 * MIB as usize, 11);
    writer.create_file(&mut disk, 256, "big.bin", &big, &attrs()).unwrap();
    let dir = writer.create_directory(256, "dir", &dir_attrs()).unwrap();
    writer.create_file(&mut disk, dir, "inside.txt", b"inside\n", &attrs()).unwrap();
    writer.commit(&mut disk).unwrap();
    let before = Btrfs::mount(&mut disk, 0).unwrap().usage().1;

    assert_eq!(writer.unlink(256, "dir", LATER).err(), Some(Error::IsADirectory));
    assert_eq!(writer.remove_directory(256, "big.bin", LATER).err(), Some(Error::NotADirectory));
    assert_eq!(writer.remove_directory(256, "dir", LATER).err(), Some(Error::NotEmpty));
    assert_eq!(writer.unlink(256, "nothing", LATER).err(), Some(Error::NotFound));

    writer.unlink(256, "big.bin", LATER).unwrap();
    writer.unlink(dir, "inside.txt", LATER).unwrap();
    writer.remove_directory(256, "dir", LATER).unwrap();
    writer.commit(&mut disk).unwrap();

    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    let after = fs.usage().1;
    assert!(after + 4 * MIB <= before, "место не освободилось: было {before}, стало {after}");
    assert_eq!(fs.resolve(&mut disk, "/dir").err(), Some(Error::NotFound));
    let root = fs.root(&mut disk).unwrap();
    assert_eq!(root.size, 0, "размер корня не вернулся к нулю");
    drop(fs);
    let report = assert_clean(&mut disk);
    assert_eq!((report.files, report.directories, report.sectors), (0, 1, 0));
}

#[test]
fn renames_within_and_across_directories() {
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let a = writer.create_directory(256, "a", &dir_attrs()).unwrap();
    let b = writer.create_directory(256, "b", &dir_attrs()).unwrap();
    writer.create_file(&mut disk, a, "x", b"moved\n", &attrs()).unwrap();
    writer.create_file(&mut disk, b, "taken", b"taken\n", &attrs()).unwrap();
    writer.create_directory(a, "sub", &dir_attrs()).unwrap();
    writer.commit(&mut disk).unwrap();

    writer.rename(a, "x", a, "y", LATER).unwrap();
    writer.rename(a, "y", b, "z", LATER).unwrap();
    assert_eq!(writer.rename(b, "z", b, "taken", LATER).err(), Some(Error::Exists));
    assert_eq!(writer.rename(a, "sub", b, "sub", LATER).err(), Some(Error::Unsupported));
    writer.rename(a, "sub", a, "renamed", LATER).unwrap();
    assert_eq!(writer.rename(a, "nothing", a, "other", LATER).err(), Some(Error::NotFound));
    writer.commit(&mut disk).unwrap();

    assert_eq!(read_path(&mut disk, "/b/z"), b"moved\n");
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    assert_eq!(fs.resolve(&mut disk, "/a/x").err(), Some(Error::NotFound));
    assert_eq!(fs.resolve(&mut disk, "/a/y").err(), Some(Error::NotFound));
    let a_node = fs.resolve(&mut disk, "/a").unwrap();
    let names: Vec<String> = fs.list(&mut disk, &a_node).unwrap().into_iter().map(|entry| entry.name).collect();
    assert_eq!(names, ["renamed"]);
    drop(fs);
    let report = assert_clean(&mut disk);
    assert_eq!((report.files, report.directories), (2, 4));
}

#[test]
fn rewriting_a_file_again_and_again_reuses_its_space() {
    // Шестьдесят перезаписей по два мегабайта — это 120 МиБ на томе в 128.
    // Пройти это можно, только освобождая прежние экстенты.
    let mut disk = fresh_volume(128);
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let file = writer.create_file(&mut disk, 256, "config.bin", &pattern(2 * MIB as usize, 0), &attrs()).unwrap();
    writer.commit(&mut disk).unwrap();
    for round in 1..=60u8 {
        writer
            .write_at(&mut disk, file, 0, &pattern(2 * MIB as usize, round), LATER)
            .unwrap_or_else(|err| panic!("круг {round}: {err}"));
        writer.commit(&mut disk).unwrap_or_else(|err| panic!("круг {round}: {err}"));
    }
    assert_eq!(read_path(&mut disk, "/config.bin"), pattern(2 * MIB as usize, 60));
    let report = assert_clean(&mut disk);
    assert_eq!(report.sectors, 2 * MIB / 4096);
}

#[test]
fn edits_on_a_volume_that_mkfs_and_linux_made() {
    let mut disk = fixture();
    let mut writer = Writer::open(&mut disk, 0).unwrap();
    let big = writer.resolve("/dir/big.bin").unwrap();
    // Экстенты этого файла сделало ядро Linux.
    writer.write_at(&mut disk, big, 1_000_000, b"FREEOS", LATER).unwrap();
    let mixed = writer.resolve("/mixed.bin").unwrap();
    writer.truncate(&mut disk, mixed, 1000, LATER).unwrap();
    writer.unlink(256, "holes.bin", LATER).unwrap();
    let dir = writer.resolve("/dir").unwrap();
    writer.rename(256, "hello.txt", dir, "hello2.txt", LATER).unwrap();
    writer.commit(&mut disk).unwrap();

    let data = read_path(&mut disk, "/dir/big.bin");
    let expected = b"0123456789abcdef";
    for (at, byte) in data.iter().enumerate() {
        let want = if (1_000_000..1_000_006).contains(&at) { b"FREEOS"[at - 1_000_000] } else { expected[at % 16] };
        assert_eq!(*byte, want, "разошлось на байте {at}");
    }
    assert_eq!(read_path(&mut disk, "/mixed.bin"), [b'M'; 1000]);
    assert_eq!(read_path(&mut disk, "/dir/hello2.txt"), b"hello from btrfs\n");
    let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
    assert_eq!(fs.resolve(&mut disk, "/holes.bin").err(), Some(Error::NotFound));
    drop(fs);
    let report = assert_clean(&mut disk);
    assert_eq!(report.files, 2006 - 1);
}

#[test]
fn an_interrupted_edit_leaves_the_old_volume_or_the_new_one() {
    let mut base = fresh_volume(128);
    let original = pattern(300_000, 12);
    {
        let mut writer = Writer::open(&mut base, 0).unwrap();
        writer.create_file(&mut base, 256, "data.bin", &original, &attrs()).unwrap();
        writer.create_file(&mut base, 256, "other.txt", b"other\n", &attrs()).unwrap();
        writer.commit(&mut base).unwrap();
    }
    let pristine = base.into_vec();
    let patch = pattern(50_000, 13);
    let mut edited = original.clone();
    edited[100_000..150_000].copy_from_slice(&patch);

    let (mut saw_old, mut saw_new) = (false, false);
    for limit in 0..60 {
        let inner = MemDisk::from_vec(pristine.clone()).unwrap();
        let mut dev = Failing { inner, writes_left: limit };
        let finished = {
            let mut attempt = || -> crate::Result<()> {
                let mut writer = Writer::open(&mut dev, 0)?;
                let file = writer.resolve("/data.bin")?;
                writer.write_at(&mut dev, file, 100_000, &patch, LATER)?;
                writer.unlink(256, "other.txt", LATER)?;
                writer.commit(&mut dev)
            };
            attempt().is_ok()
        };
        let mut disk = dev.inner;
        let generation = Btrfs::mount(&mut disk, 0)
            .unwrap_or_else(|err| panic!("обрыв после {limit}: {err}"))
            .generation();
        match generation {
            2 => {
                assert!(!finished);
                assert_eq!(read_path(&mut disk, "/data.bin"), original, "обрыв после {limit}");
                assert_eq!(read_path(&mut disk, "/other.txt"), b"other\n");
                saw_old = true;
            }
            3 => {
                assert_eq!(read_path(&mut disk, "/data.bin"), edited, "обрыв после {limit}");
                let mut fs = Btrfs::mount(&mut disk, 0).unwrap();
                assert_eq!(fs.resolve(&mut disk, "/other.txt").err(), Some(Error::NotFound));
                saw_new = true;
            }
            other => panic!("обрыв после {limit}: поколение {other}"),
        }
        assert_clean(&mut disk);
    }
    assert!(saw_old && saw_new);
}
