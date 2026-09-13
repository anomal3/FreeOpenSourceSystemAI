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

use disk::MemDisk;

use crate::read::{Btrfs, FileType};
use crate::{Error, Key, item_type};

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
