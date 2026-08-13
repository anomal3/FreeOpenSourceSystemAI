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
use crate::edit::Editor;
use crate::write::{FormatOptions, format_with};
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
pub(crate) fn formatted(sectors: u64, block_size: BlockSize) -> (MemDisk, Editor) {
    let mut dev = MemDisk::new(sectors).expect("образ размещается");
    let writer = format_with(&mut dev, 0, sectors, block_size, &options())
        .expect("форматирование удаётся");
    (dev, writer)
}

/// Смонтировать образ посторонней реализацией ext2.
pub(crate) fn foreign(dev: &MemDisk) -> Ext4 {
    Ext4::load(std::boxed::Box::new(dev.as_bytes().to_vec()))
        .expect("чужой драйвер монтирует том")
}

#[test]
fn foreign_reader_mounts_a_freshly_formatted_volume() {
    for block_size in [BlockSize::B1024, BlockSize::B4096] {
        let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, block_size);
        writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
        writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");

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
    writer.flush(&mut dev).expect("завершение");
    assert_eq!(Ext2::label(&mut dev, 0).as_deref(), Ok("FreeOS"));
    assert_eq!(Ext2::is_clean(&mut dev, 0), Ok(true));
}

// ---------------------------------------------------------------------------
// Правка живого тома
// ---------------------------------------------------------------------------
//
// Проверки ниже сначала **закрывают** том и открывают его заново
// ([`Editor::open`]), и это не формальность: ядро именно так с ним и работает —
// оно этот раздел не размечало и о его счётчиках знает только то, что прочитало
// с диска. Правка редактором, доставшимся от форматирования, проверяла бы
// совсем другой путь.

/// Открыть том заново — как это делает ядро при монтировании.
fn reopened(dev: &mut MemDisk) -> Editor {
    Editor::open(dev, 0).expect("том открывается на правку")
}

/// Признак чистого размонтирования ведёт себя так, как обещано: том, который
/// открыли и закрыли, чист; том, который открыли и бросили, — нет.
///
/// Проверка на том же уровне, на котором это работает у ядра: «бросили» здесь
/// — это просто потерянный редактор, ровно то, что остаётся от системы после
/// пропажи питания.
#[test]
fn a_volume_is_marked_used_while_open_and_clean_only_when_closed() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    writer.flush(&mut dev).expect("завершение");
    assert_eq!(Ext2::is_clean(&mut dev, 0), Ok(true), "свежий том чист");

    // Открытие помечает том используемым **сразу**, а не при первой записи.
    let mut editor = reopened(&mut dev);
    assert_eq!(
        Ext2::is_clean(&mut dev, 0),
        Ok(false),
        "открытый на правку том не может считаться закрытым"
    );
    editor.flush(&mut dev).expect("сброс");
    editor.mark_clean(&mut dev).expect("закрытие");
    assert_eq!(Ext2::is_clean(&mut dev, 0), Ok(true), "закрыли — стал чист");

    // А теперь то же самое, но том бросают на середине работы.
    let mut editor = reopened(&mut dev);
    editor
        .create(&mut dev, ROOT_INODE, "half-written.txt", 0o644, 0, 0)
        .expect("файл");
    drop(editor);
    assert_eq!(
        Ext2::is_clean(&mut dev, 0),
        Ok(false),
        "брошенный том обязан остаться помеченным"
    );

    // И следующее монтирование это видит — то самое знание, ради которого
    // признак и существует.
    let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
    assert!(!fs.was_clean(), "монтирующий узнаёт о прошлом сеансе");
}

#[test]
fn a_file_written_after_remount_is_visible_to_a_foreign_reader() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B4096);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    let dir = editor
        .mkdir(&mut dev, ROOT_INODE, "notes", 0o755, 1000, 1000)
        .expect("каталог");
    let file = editor
        .create(&mut dev, dir, "today.txt", 0o644, 1000, 1000)
        .expect("файл");
    editor
        .write_at(&mut dev, file, 0, b"written by the kernel")
        .expect("запись");
    editor.flush(&mut dev).expect("сброс");

    let fs = foreign(&dev);
    assert_eq!(
        fs.read_to_string("/notes/today.txt").expect("файл читается"),
        "written by the kernel"
    );
    let meta = fs.metadata("/notes/today.txt").expect("метаданные");
    assert_eq!(meta.mode() & 0o777, 0o644);
    assert_eq!(meta.uid(), 1000);
}

#[test]
fn writing_past_the_end_extends_the_file_and_the_gap_reads_as_zeroes() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    let file = editor
        .create(&mut dev, ROOT_INODE, "sparse.bin", 0o644, 0, 0)
        .expect("файл");
    editor.write_at(&mut dev, file, 0, b"head").expect("начало");
    // Смещение за концом: между «head» и «tail» остаётся дыра в несколько
    // блоков, которой на диске не существует.
    editor.write_at(&mut dev, file, 5000, b"tail").expect("хвост");
    editor.flush(&mut dev).expect("сброс");

    let fs = foreign(&dev);
    let data = fs.read("/sparse.bin").expect("файл читается");
    assert_eq!(data.len(), 5004);
    assert_eq!(&data[..4], b"head");
    assert!(data[4..5000].iter().all(|byte| *byte == 0), "дыра читается нулями");
    assert_eq!(&data[5000..], b"tail");
}

#[test]
fn rewriting_the_middle_of_a_file_leaves_the_rest_alone() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    let file = editor
        .create_file(&mut dev, ROOT_INODE, "log.txt", b"aaaabbbbcccc", 0o644, 0, 0)
        .expect("файл");
    editor.write_at(&mut dev, file, 4, b"BBBB").expect("правка");
    editor.flush(&mut dev).expect("сброс");

    assert_eq!(
        foreign(&dev).read_to_string("/log.txt").expect("читается"),
        "aaaaBBBBcccc"
    );
}

/// Файл длиннее двенадцати прямых блоков — то есть с косвенностью, — дописанный
/// по кускам. Именно здесь ошибка в таблицах указателей и проявляется.
#[test]
fn appending_across_indirect_blocks_produces_a_readable_file() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    let file = editor
        .create(&mut dev, ROOT_INODE, "big.bin", 0o644, 0, 0)
        .expect("файл");

    // 400 КиБ при блоке в килобайт: двенадцать прямых, дальше одинарная
    // косвенность целиком (256 указателей) и начало двойной.
    let chunk: Vec<u8> = (0..1000u32).map(|value| value as u8).collect();
    for round in 0..400u64 {
        let at = round * chunk.len() as u64;
        editor.write_at(&mut dev, file, at, &chunk).expect("дозапись");
    }
    editor.flush(&mut dev).expect("сброс");

    let data = foreign(&dev).read("/big.bin").expect("файл читается");
    assert_eq!(data.len(), 400 * chunk.len());
    for round in 0..400 {
        let at = round * chunk.len();
        assert_eq!(&data[at..at + chunk.len()], &chunk[..], "кусок {round}");
    }
}

#[test]
fn truncating_returns_the_blocks_and_clears_the_tail() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let before = reopened(&mut dev).free_bytes();

    let mut editor = reopened(&mut dev);
    let file = editor
        .create(&mut dev, ROOT_INODE, "shrink.bin", 0o644, 0, 0)
        .expect("файл");
    let data = vec![0xAB; 100 * 1024];
    editor.write_at(&mut dev, file, 0, &data).expect("запись");
    assert!(editor.free_bytes() < before, "запись занимает место");

    editor.truncate(&mut dev, file, 10).expect("усечение");
    editor.flush(&mut dev).expect("сброс");

    let fs = foreign(&dev);
    assert_eq!(fs.read("/shrink.bin").expect("читается").len(), 10);

    // Место вернулось: под десять байт остался ровно один блок.
    let mut editor = reopened(&mut dev);
    assert_eq!(before - editor.free_bytes(), 1024, "занят ровно один блок");

    // А выросший обратно файл не показывает прежнее содержимое.
    editor.write_at(&mut dev, file, 20, b"x").expect("дозапись");
    editor.flush(&mut dev).expect("сброс");
    let data = foreign(&dev).read("/shrink.bin").expect("читается");
    assert!(data[10..20].iter().all(|byte| *byte == 0), "хвост обнулён");
}

#[test]
fn deleting_a_file_frees_everything_it_held() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let free_at_start = reopened(&mut dev).free_bytes();

    let mut editor = reopened(&mut dev);
    editor
        .create_file(&mut dev, ROOT_INODE, "temp.bin", &vec![7u8; 60 * 1024], 0o644, 0, 0)
        .expect("файл");
    editor.flush(&mut dev).expect("сброс");
    assert!(reopened(&mut dev).free_bytes() < free_at_start);

    let mut editor = reopened(&mut dev);
    editor.unlink(&mut dev, ROOT_INODE, "temp.bin").expect("удаление");
    editor.flush(&mut dev).expect("сброс");

    // Место вернулось всё, до байта: и данные, и блок косвенности.
    assert_eq!(reopened(&mut dev).free_bytes(), free_at_start);

    let fs = foreign(&dev);
    assert!(!fs.exists("/temp.bin").expect("вопрос допустим"));
}

#[test]
fn directories_are_created_and_removed_only_when_empty() {
    let (mut dev, mut writer) = formatted(HALF_GIB_SECTORS, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    let dir = editor
        .mkdir(&mut dev, ROOT_INODE, "work", 0o755, 0, 0)
        .expect("каталог");
    editor
        .create_file(&mut dev, dir, "draft.txt", b"...", 0o644, 0, 0)
        .expect("файл внутри");

    assert_eq!(editor.rmdir(&mut dev, ROOT_INODE, "work"), Err(Error::NotEmpty));
    // И наоборот: удалять каталог как файл нельзя.
    assert_eq!(
        editor.unlink(&mut dev, ROOT_INODE, "work"),
        Err(Error::IsADirectory)
    );

    editor.unlink(&mut dev, dir, "draft.txt").expect("файл удаляется");
    editor
        .rmdir(&mut dev, ROOT_INODE, "work")
        .expect("пустой каталог удаляется");
    editor.flush(&mut dev).expect("сброс");

    let fs = foreign(&dev);
    assert!(!fs.exists("/work").expect("вопрос допустим"));
    // Ссылка «..» удалённого каталога снята с корня: остались «.» и «..».
    let root = Ext2::mount(&mut dev, 0)
        .expect("монтирование")
        .root(&mut dev)
        .expect("корень");
    assert_eq!(root.links, 2);
}

/// Место, освобождённое удалением, достаётся следующему файлу. Без второго
/// прохода поиска свободного блока том «кончился» бы, имея половину пустоты.
#[test]
fn freed_space_is_handed_out_again() {
    let (mut dev, mut writer) = formatted(8 * 1024 * 1024 / 512, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    let big = vec![1u8; 3 * 1024 * 1024];
    editor
        .create_file(&mut dev, ROOT_INODE, "first.bin", &big, 0o644, 0, 0)
        .expect("первый файл");
    editor
        .create_file(&mut dev, ROOT_INODE, "second.bin", &big, 0o644, 0, 0)
        .expect("второй файл");
    editor
        .unlink(&mut dev, ROOT_INODE, "first.bin")
        .expect("удаление");
    // Третий помещается только в место, освободившееся от первого.
    editor
        .create_file(&mut dev, ROOT_INODE, "third.bin", &big, 0o644, 0, 0)
        .expect("третий файл");
    editor.flush(&mut dev).expect("сброс");

    let fs = foreign(&dev);
    assert_eq!(fs.read("/third.bin").expect("читается").len(), big.len());
    assert_eq!(fs.read("/second.bin").expect("читается").len(), big.len());
}

/// Занятое имя занято — и файлом, и каталогом.
#[test]
fn names_are_not_reused_silently() {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    writer.flush(&mut dev).expect("сброс");

    let mut editor = reopened(&mut dev);
    editor
        .create(&mut dev, ROOT_INODE, "same", 0o644, 0, 0)
        .expect("файл");
    assert_eq!(
        editor.create(&mut dev, ROOT_INODE, "same", 0o644, 0, 0),
        Err(Error::Exists)
    );
    assert_eq!(
        editor.mkdir(&mut dev, ROOT_INODE, "same", 0o755, 0, 0),
        Err(Error::Exists)
    );
    assert_eq!(
        editor.unlink(&mut dev, ROOT_INODE, "missing"),
        Err(Error::NotFound)
    );
}

/// Том на носителе с сектором 4096 — и его читает чужая реализация.
///
/// Проверка нужна из-за одного места, которое до Phase 26c было верным по
/// совпадению: суперблок ext2 лежит по **байтовому** смещению 1024 от начала
/// тома, и на 512-байтном диске это ровно два сектора. На 4Kn-диске то же
/// смещение попадает внутрь первого сектора — формула «смещение делить на 512»
/// прочитала бы восьмой сектор, то есть чужие данные.
///
/// Судья здесь тот же, что и во всех остальных проверках этого файла, и в этом
/// весь смысл: `ext4-view` ничего не знает про наши сектора и читает том по
/// спецификации.
#[test]
fn a_volume_on_a_4kn_medium_is_read_by_the_foreign_driver() {
    /// 512 МиБ, выраженные в секторах по 4096 байт.
    const SECTORS_4K: u64 = 512 * 1024 * 1024 / 4096;

    let mut dev = MemDisk::with_sector_size(SECTORS_4K, 4096).expect("образ 4Kn размещается");
    let mut writer = format_with(&mut dev, 0, SECTORS_4K, BlockSize::B4096, &options())
        .expect("форматирование 4Kn удаётся");

    let inode = writer
        .create(&mut dev, ROOT_INODE, "on-4kn.txt", 0o644, 1000, 1000)
        .expect("файл создаётся");
    writer
        .write_at(&mut dev, inode, 0, b"written on a 4Kn medium")
        .expect("запись удаётся");
    writer.flush(&mut dev).expect("завершение");

    // Свой читатель: том монтируется, блок ext2 равен четырём секторам.
    let fs = Ext2::mount(&mut dev, 0).expect("свой читатель монтирует том");
    assert_eq!(fs.geometry().sector_size, 4096);
    assert_eq!(fs.geometry().sectors_per_block(), 1);

    // Чужой: он про наши сектора не знает вовсе и читает по спецификации.
    let foreign = foreign(&dev);
    assert!(foreign.exists("/on-4kn.txt").expect("файл виден снаружи"));
    let content = foreign.read("/on-4kn.txt").expect("файл читается снаружи");
    assert_eq!(content, b"written on a 4Kn medium");
}
