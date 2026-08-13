//! Проверка и починка тома: образ ломается намеренно и чинится обратно.
//!
//! Здесь портит образ тот же крейт, который его пишет, — а потом чинит и
//! предъявляет **чужому** читателю. Обе половины утверждения проверяемы: что
//! поломка найдена и что после починки том остался томом, а не набором
//! согласованных между собой чисел.
//!
//! Настоящего `e2fsck` на машине разработки нет — она под Windows, — и
//! притворяться иначе нельзя. Чужой верификатор здесь `ext4-view`: он ловит
//! всё, что делает том нечитаемым, но про счётчики «clean» не скажет. Поэтому
//! счётчики сверяются повторным прогоном собственной проверки, и это сказано
//! вслух, а не подразумевается.

use alloc::format;
use alloc::vec::Vec;
use std::vec;

use disk::{BlockDevice, MemDisk};

use crate::check::{Fix, Problem, check};
use crate::layout::{BlockSize, GROUP_DESC_SIZE, put_u16, put_u32, u16_at, u32_at};
use crate::read::Ext2;
use crate::tests::{foreign, formatted};
use crate::ROOT_INODE;

/// Том с деревом файлов — то, на чём проверка вообще имеет смысл.
///
/// Блок 1024 байта выбран не случайно: при нём суперблок занимает блок 1
/// целиком, и все служебные блоки группы 0 идут подряд с предсказуемыми
/// номерами, которые тесту нужно портить.
fn populated() -> MemDisk {
    let (mut dev, mut writer) = formatted(64 * 1024 * 1024 / 512, BlockSize::B1024);
    writer
        .create_dir_path(&mut dev, "home/roman", 0o755, 1000, 1000)
        .expect("каталоги");
    writer
        .write_file_path(&mut dev, "home/roman/notes.txt", b"keep me", 0o644, 1000, 1000)
        .expect("файл");
    writer
        .write_file_path(&mut dev, "etc/passwd", b"roman:1000:1000\n", 0o640, 0, 0)
        .expect("файл");
    writer.flush(&mut dev).expect("сброс");
    writer.mark_clean(&mut dev).expect("закрытие");
    dev
}

/// Номера служебных блоков группы 0 при блоке в 1024 байта.
const SUPERBLOCK: u32 = 1;
const GROUP_DESCRIPTORS: u32 = 2;
const BLOCK_BITMAP: u32 = 3;
const INODE_BITMAP: u32 = 4;

fn read_block(dev: &mut MemDisk, block: u32) -> Vec<u8> {
    let mut buf = vec![0u8; 1024];
    BlockDevice::read(dev, u64::from(block) * 2, &mut buf).expect("чтение блока");
    buf
}

fn write_block(dev: &mut MemDisk, block: u32, data: &[u8]) {
    BlockDevice::write(dev, u64::from(block) * 2, data).expect("запись блока");
}

/// Найти первый свободный бит в битовой карте и занять его.
fn take_free_bit(map: &mut [u8]) -> u32 {
    let byte = map.iter().position(|byte| *byte != 0xFF).expect("свободные биты есть");
    let bit = (0..8).find(|bit| map[byte] & (1 << bit) == 0).expect("свободный бит");
    map[byte] |= 1 << bit;
    (byte * 8) as u32 + bit
}

#[test]
fn a_healthy_volume_passes_the_check() {
    let mut dev = populated();
    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка проходит");
    assert!(
        report.is_clean(),
        "исправный том не должен давать находок: {:?}",
        report.problems
    );
    assert!(report.inodes_used > 0 && report.blocks_used > 0);
}

#[test]
fn a_block_marked_used_by_nobody_is_given_back() {
    let mut dev = populated();
    // Ровно то, что остаётся после пропажи питания между «взял блок в карте» и
    // «привязал его к файлу»: бит стоит, а блок не принадлежит никому.
    let mut map = read_block(&mut dev, BLOCK_BITMAP);
    take_free_bit(&mut map);
    write_block(&mut dev, BLOCK_BITMAP, &map);

    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка");
    assert!(
        report.problems.iter().any(|problem| matches!(
            problem,
            Problem::BlockBitmap { leaked: 1, missing: 0, .. }
        )),
        "утёкший блок обязан быть найден: {:?}",
        report.problems
    );
    assert!(!report.needs_attention(), "это чинится однозначно");

    let report = check(&mut dev, 0, Fix::Safe).expect("починка");
    assert!(report.fixed > 0);
    let report = check(&mut dev, 0, Fix::Nothing).expect("повторная проверка");
    assert!(report.is_clean(), "после починки: {:?}", report.problems);

    // И главное: том остался томом.
    let fs = foreign(&dev);
    assert_eq!(fs.read("/home/roman/notes.txt").expect("файл цел"), b"keep me");
}

#[test]
fn counters_that_drifted_are_recomputed() {
    let mut dev = populated();
    // Счётчик свободных блоков в суперблоке — то, что разъезжается первым,
    // если машина выключилась до сброса редактора.
    let mut sb = read_block(&mut dev, SUPERBLOCK);
    let was = u32_at(&sb, 12);
    put_u32(&mut sb, 12, was - 17);
    write_block(&mut dev, SUPERBLOCK, &sb);

    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка");
    assert!(
        report.problems.iter().any(|problem| matches!(
            problem,
            Problem::FreeBlocks { correct, .. } if *correct == was
        )),
        "разъехавшийся счётчик обязан быть найден: {:?}",
        report.problems
    );

    check(&mut dev, 0, Fix::Safe).expect("починка");
    let sb = read_block(&mut dev, SUPERBLOCK);
    assert_eq!(u32_at(&sb, 12), was, "счётчик пересчитан");
    let report = check(&mut dev, 0, Fix::Nothing).expect("повторная проверка");
    assert!(report.is_clean(), "{:?}", report.problems);
}

#[test]
fn an_interrupted_create_is_collected() {
    let mut dev = populated();
    // Занять inode в карте, не создав ни записи каталога, ни ссылок: так
    // выглядит том, у которого питание пропало между этими двумя шагами.
    let mut map = read_block(&mut dev, INODE_BITMAP);
    let number = take_free_bit(&mut map) + 1;
    write_block(&mut dev, INODE_BITMAP, &map);

    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| matches!(problem, Problem::Abandoned { inode } if *inode == number)),
        "остаток создания обязан быть найден: {:?}",
        report.problems
    );

    check(&mut dev, 0, Fix::Safe).expect("починка");
    let report = check(&mut dev, 0, Fix::Nothing).expect("повторная проверка");
    assert!(report.is_clean(), "{:?}", report.problems);
}

#[test]
fn a_file_that_lost_its_name_moves_to_lost_found() {
    let mut dev = populated();
    let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
    let file = fs
        .resolve(&mut dev, "/home/roman/notes.txt")
        .expect("файл на месте")
        .number;
    let dir = fs.resolve(&mut dev, "/home/roman").expect("каталог");
    let block = dir.first_block();

    // Обнуляем номер inode в записи каталога — ровно так помечают запись
    // удалённой. Данные файла целы, имени больше нет.
    let mut buf = read_block(&mut dev, block);
    let mut at = 0usize;
    loop {
        let entry = u32_at(&buf, at);
        let rec_len = u16_at(&buf, at + 4) as usize;
        let name_len = buf[at + 6] as usize;
        if entry == file && &buf[at + 8..at + 8 + name_len] == b"notes.txt" {
            put_u32(&mut buf, at, 0);
            break;
        }
        at += rec_len;
        assert!(at + 8 <= buf.len(), "запись обязана найтись");
    }
    write_block(&mut dev, block, &buf);

    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| matches!(problem, Problem::Lost { inode, .. } if *inode == file)),
        "потерянный файл обязан быть найден: {:?}",
        report.problems
    );

    let report = check(&mut dev, 0, Fix::Safe).expect("починка");
    assert_eq!(report.rescued, 1, "файл обязан переехать");

    // Доказательство, ради которого тест и написан: файл читается **чужим**
    // драйвером по новому имени, и содержимое то же самое.
    let fs = foreign(&dev);
    let name = format!("/lost+found/#{file}");
    assert_eq!(fs.read(&name).expect("спасённый файл читается"), b"keep me");

    let report = check(&mut dev, 0, Fix::Nothing).expect("повторная проверка");
    assert!(report.is_clean(), "{:?}", report.problems);
}

#[test]
fn a_dangling_entry_is_named_but_not_touched() {
    let mut dev = populated();
    let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
    let dir = fs.resolve(&mut dev, "/etc").expect("каталог");
    let block = dir.first_block();

    // Переставим запись на заведомо свободный номер inode. Удалить такую
    // запись — значит потерять имя, поэтому проверка обязана её назвать и не
    // трогать.
    let mut buf = read_block(&mut dev, block);
    let mut at = 0usize;
    loop {
        let entry = u32_at(&buf, at);
        let rec_len = u16_at(&buf, at + 4) as usize;
        let name_len = buf[at + 6] as usize;
        if entry != 0 && name_len == 6 && &buf[at + 8..at + 14] == b"passwd" {
            put_u32(&mut buf, at, 4321);
            break;
        }
        at += rec_len;
        assert!(at + 8 <= buf.len(), "запись обязана найтись");
    }
    write_block(&mut dev, block, &buf);

    let report = check(&mut dev, 0, Fix::Safe).expect("проверка с починкой");
    assert!(
        report
            .problems
            .iter()
            .any(|problem| matches!(problem, Problem::Dangling { inode: 4321, .. })),
        "битая ссылка обязана быть названа: {:?}",
        report.problems
    );
    assert!(report.needs_attention(), "это не чинится само");

    // Запись на месте: проверка её не удаляла.
    let buf = read_block(&mut dev, block);
    assert!(
        buf.windows(6).any(|window| window == b"passwd"),
        "имя не должно исчезнуть"
    );
}

#[test]
fn a_wrong_link_count_is_corrected() {
    let mut dev = populated();
    let fs = Ext2::mount(&mut dev, 0).expect("монтирование");
    let number = fs.resolve(&mut dev, "/etc/passwd").expect("файл").number;
    let geometry = fs.geometry();

    // Испортить счётчик ссылок в самом inode: семь вместо единственной записи.
    let (group, index) = geometry.locate_inode(number).expect("номер в пределах тома");
    let byte_offset = index as usize * 128;
    let block = geometry.inode_table_block(group) + (byte_offset / 1024) as u32;
    let within = byte_offset % 1024;
    let mut buf = read_block(&mut dev, block);
    put_u16(&mut buf, within + 26, 7);
    write_block(&mut dev, block, &buf);

    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка");
    assert!(
        report.problems.iter().any(|problem| matches!(
            problem,
            Problem::Links { inode, was: 7, correct: 1 } if *inode == number
        )),
        "неверное число ссылок обязано быть найдено: {:?}",
        report.problems
    );

    check(&mut dev, 0, Fix::Safe).expect("починка");
    let report = check(&mut dev, 0, Fix::Nothing).expect("повторная проверка");
    assert!(report.is_clean(), "{:?}", report.problems);
    let fs = foreign(&dev);
    assert_eq!(fs.read("/etc/passwd").expect("файл цел"), b"roman:1000:1000\n");
}

/// На исправном томе починка обязана быть **ничем**: ни одного изменённого
/// байта в дескрипторах групп. Иначе «починка» была бы способом испортить том,
/// проверив его.
#[test]
fn repairing_a_healthy_volume_changes_nothing() {
    let mut dev = populated();
    let before = read_block(&mut dev, GROUP_DESCRIPTORS)[..GROUP_DESC_SIZE].to_vec();
    let report = check(&mut dev, 0, Fix::Safe).expect("починка исправного тома");
    assert!(report.is_clean());
    assert_eq!(report.fixed, 0);
    let after = read_block(&mut dev, GROUP_DESCRIPTORS)[..GROUP_DESC_SIZE].to_vec();
    assert_eq!(before, after);
}

/// Корень обязан оставаться достижимым: если проверка теряет дорогу к нему,
/// потерянным окажется весь том разом, и «починка» перенесёт в `/lost+found`
/// всё дерево.
#[test]
fn everything_reachable_from_the_root_stays_reachable() {
    let mut dev = populated();
    let report = check(&mut dev, 0, Fix::Nothing).expect("проверка");
    assert!(
        !report.problems.iter().any(|problem| matches!(problem, Problem::Lost { .. })),
        "на исправном томе потерянных быть не может: {:?}",
        report.problems
    );
    let _ = ROOT_INODE;
}
