//! Проверка тома: своим читателем и — что важнее — чужим.
//!
//! Крейт `ext4-view` здесь не вспомогательный инструмент, а смысл всей затеи с
//! ext2. Свой писатель, проверенный своим же читателем, доказывает лишь, что
//! обе половины одинаково понимают формат; расходятся ли они со
//! спецификацией — из такой проверки не видно. Ровно поэтому образ FAT32 в
//! крейте `disk` читается крейтом `fatfs`, и ровно поэтому собственный формат
//! файловой системы был отвергнут: у него такой проверки не было бы никогда.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use std::vec;

use disk::MemDisk;
use ext4_view::Ext4;

use crate::layout::BlockSize;
use crate::read::{Ext2, FileType};
use crate::write::{FormatOptions, Writer, format_with};
use crate::{Error, ROOT_INODE};

/// 512 МиБ — примерно тот корневой раздел, который создаёт установщик.
const HALF_GIB_SECTORS: u64 = 512 * 1024 * 1024 / 512;

fn options() -> FormatOptions<'static> {
    FormatOptions {
        label: "FreeOS",
        uuid: [
            0x4f, 0x72, 0x6f, 0x46, 0x53, 0x45, 0x4f, 0x52, 0x4f, 0x4f, 0x54, 0x01, 0x02, 0x03,
            0x04, 0x05,
        ],
        // Фиксированное время: образ обязан быть побайтово воспроизводимым.
        time: 1_700_000_000,
    }
}

/// Отформатировать образ в памяти и вернуть его вместе с писателем.
fn formatted(sectors: u64, block_size: BlockSize) -> (MemDisk, Writer) {
    let mut dev = MemDisk::new(sectors).expect("образ размещается");
    let writer = format_with(&mut dev, 0, sectors, block_size, &options())
        .expect("форматирование удаётся");
    (dev, writer)
}

/// Смонтировать образ посторонней реализацией ext2.
fn foreign(dev: &MemDisk) -> Ext4 {
    Ext4::load(std::boxed::Box::new(dev.as_bytes().to_vec()))
        .expect("чужой драйвер монтирует том")
}

#[test]
fn foreign_reader_mounts_a_freshly_formatted_volume() {
    for block_size in [BlockSize::B1024, BlockSize::B4096] {
        let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, block_size);
        writer.finish(&mut dev, &options()).expect("завершение");

        let fs = foreign(&dev);
        assert!(fs.exists("/").expect("корень существует"), "{block_size:?}");
        // Свежий том пуст: в корне нет ничего, кроме «.» и «..», которых
        // перечисление не показывает.
        let entries: Vec<_> = fs
            .read_dir("/")
            .expect("корень читается")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().as_str().unwrap_or("?").to_string())
            .filter(|name| name != "." && name != "..")
            .collect();
        assert!(entries.is_empty(), "{block_size:?}: {entries:?}");
    }
}

#[test]
fn files_and_directories_survive_a_round_trip_through_a_foreign_reader() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B4096);

    writer
        .create_dir_path(&mut dev, "etc", 0o755, 0, 0)
        .expect("каталог");
    writer
        .write_file_path(&mut dev, "etc/passwd", b"roman:1000:1000\n", 0o640, 0, 0)
        .expect("файл");
    writer
        .create_dir_path(&mut dev, "home/roman", 0o750, 1000, 1000)
        .expect("домашний каталог");
    writer
        .write_file_path(
            &mut dev,
            "home/roman/notes.txt",
            b"hello from the installer",
            0o644,
            1000,
            1000,
        )
        .expect("файл пользователя");
    writer.finish(&mut dev, &options()).expect("завершение");

    let fs = foreign(&dev);
    assert_eq!(
        fs.read("/etc/passwd").expect("passwd читается"),
        b"roman:1000:1000\n"
    );
    assert_eq!(
        fs.read_to_string("/home/roman/notes.txt")
            .expect("заметка читается"),
        "hello from the installer"
    );

    // Права и владение — то самое, ради чего FAT32 не подошёл под корень.
    let meta = fs.metadata("/etc/passwd").expect("метаданные");
    assert_eq!(meta.mode() & 0o7777, 0o640);
    assert_eq!(meta.uid(), 0);
    assert_eq!(meta.gid(), 0);

    let meta = fs.metadata("/home/roman").expect("метаданные каталога");
    assert!(meta.is_dir());
    assert_eq!(meta.mode() & 0o7777, 0o750);
    assert_eq!(meta.uid(), 1000);
    assert_eq!(meta.gid(), 1000);
}

/// Файл длиннее двенадцати прямых блоков — единственный путь, на котором
/// работает косвенность первого уровня, а за ней и второго.
#[test]
fn large_files_use_indirect_blocks() {
    // Блок в 1024 байта выбран намеренно: при нём двойная косвенность
    // начинается уже после 268 КиБ, и её можно проверить файлом разумного
    // размера. При 4 КиБ для этого понадобился бы файл в четыре мегабайта.
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);

    // 12 прямых + 256 через один уровень + начало второго уровня.
    let size = (12 + 256 + 300) * 1024 + 123;
    let payload: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    writer
        .write_file_path(&mut dev, "big.bin", &payload, 0o644, 0, 0)
        .expect("большой файл");
    writer.finish(&mut dev, &options()).expect("завершение");

    let fs = foreign(&dev);
    let read = fs.read("/big.bin").expect("большой файл читается");
    assert_eq!(read.len(), payload.len());
    assert_eq!(read, payload);
}

/// Каталог, не помещающийся в один блок: записи должны продолжиться в
/// следующем, а не затереть друг друга.
#[test]
fn directories_grow_past_one_block() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    let count = 200;
    for index in 0..count {
        let name = format!("file-with-a-longish-name-{index:04}.txt");
        writer
            .write_file_path(&mut dev, &format!("many/{name}"), b"x", 0o644, 0, 0)
            .expect("файл");
    }
    writer.finish(&mut dev, &options()).expect("завершение");

    let fs = foreign(&dev);
    let listed: Vec<String> = fs
        .read_dir("/many")
        .expect("каталог читается")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().as_str().unwrap_or("?").to_string())
        .filter(|name| name != "." && name != "..")
        .collect();
    assert_eq!(listed.len(), count);
    assert!(listed.contains(&"file-with-a-longish-name-0199.txt".to_string()));
}

/// Наш читатель обязан видеть ровно то же, что чужой. Расхождение здесь
/// означает ошибку ровно в одном из двух — и покажет, в каком.
#[test]
fn our_reader_agrees_with_the_foreign_one() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B4096);
    writer
        .write_file_path(&mut dev, "etc/system.cfg", b"language=ru\n", 0o644, 0, 0)
        .expect("файл");
    writer
        .create_dir_path(&mut dev, "home/roman", 0o750, 1000, 1000)
        .expect("каталог");
    let payload: Vec<u8> = (0..200_000u32).map(|index| (index % 253) as u8).collect();
    writer
        .write_file_path(&mut dev, "home/roman/data.bin", &payload, 0o600, 1000, 1000)
        .expect("файл");
    writer.finish(&mut dev, &options()).expect("завершение");

    let ours = Ext2::mount(&mut dev, 0).expect("свой драйвер монтирует том");
    let theirs = foreign(&dev);

    for path in ["/etc/system.cfg", "/home/roman/data.bin"] {
        let inode = ours.resolve(&mut dev, path).expect("путь разрешается");
        let mine = ours.read_file(&mut dev, &inode).expect("файл читается");
        let foreign_bytes = theirs.read(path).expect("чужой драйвер читает");
        assert_eq!(mine, foreign_bytes, "{path}");

        let meta = theirs.metadata(path).expect("метаданные");
        assert_eq!(u64::from(inode.mode), u64::from(meta.mode() & 0o7777), "{path}");
        assert_eq!(inode.uid, meta.uid(), "{path}");
        assert_eq!(inode.gid, meta.gid(), "{path}");
        assert_eq!(inode.size, mine.len() as u64, "{path}");
    }

    // Перечисление каталога тоже обязано совпасть.
    let dir = ours.resolve(&mut dev, "/home/roman").expect("каталог");
    let mut mine: Vec<String> = ours
        .list(&mut dev, &dir)
        .expect("перечисление")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    let mut theirs: Vec<String> = theirs
        .read_dir("/home/roman")
        .expect("перечисление")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().as_str().unwrap_or("?").to_string())
        .filter(|name| name != "." && name != "..")
        .collect();
    mine.sort();
    theirs.sort();
    assert_eq!(mine, theirs);
}

/// Чтение с произвольного смещения: путь, которым пойдёт `cat` в оболочке.
#[test]
fn partial_reads_land_where_they_should() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    let payload: Vec<u8> = (0..10_000u32).map(|index| (index % 255) as u8).collect();
    writer
        .write_file_path(&mut dev, "data.bin", &payload, 0o644, 0, 0)
        .expect("файл");
    writer.finish(&mut dev, &options()).expect("завершение");

    let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
    let inode = fs.resolve(&mut dev, "/data.bin").expect("файл");

    // Смещения подобраны так, чтобы попасть и внутрь блока, и на его границу,
    // и на переход между блоками.
    for (offset, len) in [(0usize, 10), (1023, 2), (1024, 1024), (5000, 3000), (9990, 100)] {
        let mut buf = vec![0u8; len];
        let read = fs
            .read_at(&mut dev, &inode, offset as u64, &mut buf)
            .expect("чтение");
        let expected = &payload[offset..(offset + len).min(payload.len())];
        assert_eq!(read, expected.len(), "смещение {offset}");
        assert_eq!(&buf[..read], expected, "смещение {offset}");
    }

    // За концом файла читается ноль байт, и это не ошибка.
    let mut buf = [0u8; 16];
    assert_eq!(fs.read_at(&mut dev, &inode, 10_000, &mut buf), Ok(0));
}

/// Том, лежащий не в начале носителя, — самое подходящее место, чтобы
/// ошибиться на смещение раздела.
#[test]
fn a_volume_at_an_offset_is_self_consistent() {
    const OFFSET: u64 = 2048;
    let sectors = 64 * 1024 * 1024 / 512;
    let mut dev = MemDisk::new(OFFSET + sectors).expect("образ");
    let mut writer = format_with(&mut dev, OFFSET, sectors, BlockSize::B1024, &options())
        .expect("форматирование");
    writer
        .write_file_path(&mut dev, "etc/passwd", b"roman", 0o640, 0, 0)
        .expect("файл");
    writer.finish(&mut dev, &options()).expect("завершение");

    // Сектор перед разделом обязан остаться нетронутым.
    let mut before = [0u8; 512];
    disk::BlockDevice::read(&mut dev, OFFSET - 1, &mut before).expect("сектор");
    assert!(before.iter().all(|&byte| byte == 0));

    let fs = Ext2::mount(&mut dev, OFFSET).expect("монтирование со смещением");
    let inode = fs.resolve(&mut dev, "/etc/passwd").expect("файл");
    assert_eq!(fs.read_file(&mut dev, &inode).expect("чтение"), b"roman");

    // И чужой читатель обязан согласиться, если вырезать раздел из носителя.
    let partition = dev.as_bytes()[(OFFSET as usize * 512)..].to_vec();
    let theirs = Ext4::load(std::boxed::Box::new(partition)).expect("том со смещением");
    assert_eq!(theirs.read("/etc/passwd").expect("чтение"), b"roman");
}

#[test]
fn metadata_of_the_root_directory_is_sane() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B4096);
    writer
        .create_dir_path(&mut dev, "etc", 0o755, 0, 0)
        .expect("каталог");
    writer
        .create_dir_path(&mut dev, "home", 0o755, 0, 0)
        .expect("каталог");
    writer.finish(&mut dev, &options()).expect("завершение");

    let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
    let root = fs.root(&mut dev).expect("корень");
    assert_eq!(root.number, ROOT_INODE);
    assert_eq!(root.kind, FileType::Directory);
    // Ссылок на корень: «.», «..» и по одной записи «..» из каждого
    // подкаталога. Забыть их увеличить — та ошибка, которую `e2fsck` чинит при
    // первом же запуске.
    assert_eq!(root.links, 4);
}

#[test]
fn bad_names_are_refused() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    assert_eq!(
        writer.create_file(&mut dev, ROOT_INODE, "", b"", 0o644, 0, 0),
        Err(Error::BadName)
    );
    assert_eq!(
        writer.create_file(&mut dev, ROOT_INODE, "a/b", b"", 0o644, 0, 0),
        Err(Error::BadName)
    );
    assert_eq!(
        writer.create_file(&mut dev, ROOT_INODE, "..", b"", 0o644, 0, 0),
        Err(Error::BadName)
    );

    writer
        .create_file(&mut dev, ROOT_INODE, "one", b"", 0o644, 0, 0)
        .expect("файл");
    assert_eq!(
        writer.create_file(&mut dev, ROOT_INODE, "one", b"", 0o644, 0, 0),
        Err(Error::Exists)
    );
}

/// Свободного места должно хватать ровно настолько, насколько обещано, а
/// переполнение обязано быть ошибкой, а не паникой.
#[test]
fn running_out_of_space_is_an_error() {
    let (mut dev, mut writer) = formatted(40 * 1024 * 1024 / 512, BlockSize::B1024);
    let free = writer.free_bytes();
    let payload = vec![0u8; free as usize + 1];
    assert_eq!(
        writer.write_file_path(&mut dev, "toobig.bin", &payload, 0o644, 0, 0),
        Err(Error::NoSpace)
    );
}

/// Разбор суперблока обязан возвращать ту же геометрию, что была при
/// форматировании: это единственная связь между писателем и читателем.
#[test]
fn geometry_survives_a_round_trip_through_the_superblock() {
    for (sectors, block_size) in [
        (HALF_GIB_SECTORS, BlockSize::B4096),
        (64 * 1024 * 1024 / 512, BlockSize::B1024),
        (100 * 2048, BlockSize::B2048),
    ] {
        let (mut dev, mut writer) = formatted(sectors, block_size);
        let written = writer.geometry();
        writer.finish(&mut dev, &options()).expect("завершение");

        let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
        let read = fs.geometry();
        assert_eq!(read.block_size, written.block_size, "{block_size:?}");
        assert_eq!(read.blocks, written.blocks, "{block_size:?}");
        assert_eq!(read.groups, written.groups, "{block_size:?}");
        assert_eq!(
            read.inodes_per_group, written.inodes_per_group,
            "{block_size:?}"
        );
        assert_eq!(read.first_data_block, written.first_data_block, "{block_size:?}");
    }
}

/// Том, размеченный не нами, обязан быть отвергнут, а не прочитан наугад: у
/// ext4 та же подпись в суперблоке.
#[test]
fn a_volume_with_unknown_features_is_refused() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    writer.finish(&mut dev, &options()).expect("завершение");

    // Выставляем несуществующую у нас возможность прямо в суперблоке.
    let mut sb = [0u8; 1024];
    disk::BlockDevice::read(&mut dev, 2, &mut sb).expect("суперблок");
    sb[96..100].copy_from_slice(&0x0040u32.to_le_bytes());
    disk::BlockDevice::write(&mut dev, 2, &sb).expect("запись");

    assert!(matches!(Ext2::mount(&mut dev, 0), Err(Error::Unsupported)));
}

/// Метка тома и признак чистоты читаются без монтирования — установщику это
/// нужно, чтобы показать человеку, что за раздел он нашёл.
#[test]
fn label_and_state_are_readable() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    writer.finish(&mut dev, &options()).expect("завершение");
    assert_eq!(Ext2::label(&mut dev, 0).as_deref(), Ok("FreeOS"));
    assert_eq!(Ext2::is_clean(&mut dev, 0), Ok(true));
}
