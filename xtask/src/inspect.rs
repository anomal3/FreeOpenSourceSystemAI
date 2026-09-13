//! Разбор готового образа: что на нём за разделы и что лежит на корневом.
//!
//! Существует ради одного вопроса, на который иначе нечем ответить: **то ли
//! записал установщик, что собирался?** Установка идёт внутри виртуальной
//! машины, её результат — файл образа, и посмотреть внутрь него на Windows
//! нечем: ext2 система не монтирует.
//!
//! # Кто что читает
//!
//! Таблицу разделов разбирает наш собственный код (`disk::gpt::read`) — тот
//! самый, который в Phase 9b будет разбирать её в ядре. А файловую систему
//! читает **чужая** реализация, крейт `ext4-view`. Разделение намеренное:
//! если бы обе половины были нашими, совпадение доказывало бы лишь то, что
//! писатель и читатель одинаково понимают формат, а нужно знать, что образ
//! понимает кто-то посторонний.
//!
//! # С btrfs так не выйдет, и это сказано прямо
//!
//! Чужого читателя btrfs на Rust нет, а `btrfs check` живёт в Linux. Том
//! данных здесь читает **наш** крейт `btrfs` — то есть эта половина отчёта
//! доказывает лишь внутреннюю согласованность. Настоящая проверка формата не
//! здесь, а в `cargo test -p btrfs`, где образ сделан `mkfs.btrfs` и проверен
//! `btrfs check`. Написано это затем, чтобы никто не принял вывод ниже за
//! подтверждение со стороны.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use disk::gpt;
use ext4_view::Ext4;

/// Разобрать образ и напечатать, что в нём.
pub fn image(path: &Path) -> Result<()> {
    let data = fs::read(path)
        .with_context(|| format!("не удалось прочитать образ {}", path.display()))?;
    let sectors = data.len() as u64 / disk::DEFAULT_SECTOR_SIZE as u64;
    let mut dev = disk::MemDisk::from_vec(data)
        .ok_or_else(|| anyhow::anyhow!("длина образа не кратна сектору"))?;

    say!();
    say!("образ: {} ({} МиБ)", path.display(), sectors / 2048);

    let table = gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("таблица разделов не читается: {err}"))?;
    say!("  GPT   : {}", table.disk_guid);
    say!(
        "  диапазон: LBA {}..{}",
        table.first_usable_lba, table.last_usable_lba
    );

    for partition in &table.partitions {
        let size = partition.range().bytes(disk::DEFAULT_SECTOR_SIZE);
        let kind = partition_kind(partition.type_guid);
        say!();
        say!(
            "  раздел {}: {kind}, {} МиБ, LBA {}..{}",
            partition.index + 1,
            size / (1024 * 1024),
            partition.first_lba,
            partition.last_lba,
        );
        say!("    имя  : {}", partition.name_string());
        say!("    тип  : {}", partition.type_guid);
        say!("    GUID : {}", partition.unique_guid);
    }

    // Раздел данных показывается до корневого: он есть далеко не на всяком
    // образе, а когда есть — обычно ради него `inspect` и зовут.
    if let Some(data) = table.find(gpt::FREEOS_DATA_TYPE) {
        let first_lba = data.first_lba;
        if btrfs::detect(&mut dev, first_lba) {
            print_btrfs(&mut dev, first_lba)?;
        } else {
            say!();
            say!("раздел данных есть, но это не btrfs — показать его нечем");
        }
    }

    let Some(root) = table.find(gpt::FREEOS_ROOT_TYPE) else {
        say!();
        say!("корневого раздела FreeOS на образе нет");
        return Ok(());
    };

    let first = root.first_lba as usize * disk::DEFAULT_SECTOR_SIZE;
    let last = (root.last_lba as usize + 1) * disk::DEFAULT_SECTOR_SIZE;
    let bytes = dev.as_bytes();
    if last > bytes.len() {
        bail!("корневой раздел выходит за пределы образа");
    }
    print_root(&bytes[first..last])
}

/// Как называется раздел с таким типом.
///
/// Список полный не для красоты: до этой фазы всё, кроме ESP и корня,
/// называлось «неизвестным» — включая второй слот и раздел состояния, которые
/// система создаёт сама. Отчёт, называющий свои же разделы чужими, сбивает с
/// толку ровно там, где на него смотрят.
fn partition_kind(type_guid: disk::guid::Guid) -> &'static str {
    if type_guid == gpt::ESP_TYPE {
        "ESP"
    } else if type_guid == gpt::FREEOS_ROOT_TYPE {
        "FreeOS root (слот A)"
    } else if type_guid == gpt::FREEOS_ROOT_B_TYPE {
        "FreeOS root (слот B)"
    } else if type_guid == gpt::FREEOS_STATE_TYPE {
        "FreeOS state"
    } else if type_guid == gpt::FREEOS_DATA_TYPE {
        "FreeOS data"
    } else {
        "неизвестный"
    }
}

/// Показать том btrfs: что в суперблоке, что в дереве и что скажет проверка.
fn print_btrfs(dev: &mut disk::MemDisk, first_lba: u64) -> Result<()> {
    let mut fs = btrfs::Btrfs::mount(dev, first_lba)
        .map_err(|err| anyhow::anyhow!("том btrfs не читается: {err}"))?;
    let (total, used) = fs.usage();

    say!();
    say!("раздел данных: btrfs (читает НАШ крейт btrfs, см. заголовок файла)");
    say!("  метка      : {}", fs.label());
    say!("  поколение  : {}", fs.generation());
    say!(
        "  размер     : {} МиБ, занято {} МиБ, кусков {}",
        total / (1024 * 1024),
        used / (1024 * 1024),
        fs.chunks()
    );
    say!(
        "  геометрия  : сектор {} Б, узел {} Б",
        fs.sector_size(),
        fs.node_size()
    );
    if !fs.was_clean() {
        say!("  внимание   : том не был отмонтирован чисто");
    }

    say!();
    say!("содержимое:");
    walk_btrfs(&mut fs, dev, "/", 1)?;

    // Проверка тома — то же, что делает `fsck` в системе, только здесь её
    // видно целиком и без эмулятора.
    let report = fs
        .check(dev)
        .map_err(|err| anyhow::anyhow!("проверка тома не дошла до конца: {err}"))?;
    say!();
    say!(
        "проверка: {} inode, {} файл(ов), {} каталог(ов), {} байт, сверено {} сектор(ов)",
        report.inodes,
        report.files,
        report.directories,
        report.bytes,
        report.sectors
    );
    for problem in &report.problems {
        say!("  находка: {problem}");
    }
    if report.dropped > 0 {
        say!("  и ещё {} находок", report.dropped);
    }
    if report.is_clean() {
        say!("  том согласован");
    }
    Ok(())
}

/// Обойти дерево тома и напечатать его.
///
/// Каталог из тысяч файлов в отчёте не нужен — он скрывает всё остальное.
/// Показываются первые двенадцать, остальные считаются; так же поступает
/// проверка со своими находками.
fn walk_btrfs(
    fs: &mut btrfs::Btrfs,
    dev: &mut disk::MemDisk,
    path: &str,
    depth: usize,
) -> Result<()> {
    const SHOW: usize = 12;

    let node = fs
        .resolve(dev, path)
        .map_err(|err| anyhow::anyhow!("путь {path} не читается: {err}"))?;
    let mut entries = fs
        .list(dev, &node)
        .map_err(|err| anyhow::anyhow!("каталог {path} не читается: {err}"))?;
    entries.sort_by(|left, right| left.name.cmp(&right.name));

    let hidden = entries.len().saturating_sub(SHOW);
    let indent = "  ".repeat(depth);

    for entry in entries.iter().take(SHOW) {
        let child = if path == "/" {
            format!("/{}", entry.name)
        } else {
            format!("{path}/{}", entry.name)
        };
        let inode = fs
            .inode(dev, entry.inode)
            .map_err(|err| anyhow::anyhow!("inode {} не читается: {err}", entry.inode))?;
        let directory = inode.kind == btrfs::FileType::Directory;
        say!(
            "  {indent}{}{}  {:04o}  uid {} gid {}  {} байт",
            entry.name,
            if directory { "/" } else { "" },
            inode.mode,
            inode.uid,
            inode.gid,
            inode.size,
        );
        if directory {
            walk_btrfs(fs, dev, &child, depth + 1)?;
        } else if inode.size > 0 && inode.size < 512 {
            let data = fs
                .read_file(dev, &inode)
                .map_err(|err| anyhow::anyhow!("файл {child} не читается: {err}"))?;
            for line in String::from_utf8_lossy(&data).lines() {
                say!("  {indent}  | {line}");
            }
        }
    }
    if hidden > 0 {
        say!("  {indent}... и ещё {hidden} запис(ей)");
    }
    Ok(())
}

/// Показать содержимое корневой файловой системы чужой реализацией.
fn print_root(partition: &[u8]) -> Result<()> {
    let fs = Ext4::load(Box::new(partition.to_vec()))
        .map_err(|err| anyhow::anyhow!("сторонний читатель не смонтировал ext2: {err}"))?;

    say!();
    say!("корневая файловая система (читает крейт ext4-view):");
    walk(&fs, "/", 1)
}

fn walk(fs: &Ext4, path: &str, depth: usize) -> Result<()> {
    let mut entries: Vec<_> = fs
        .read_dir(path)
        .map_err(|err| anyhow::anyhow!("каталог {path} не читается: {err}"))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().as_str().ok().map(str::to_owned))
        .filter(|name| name != "." && name != "..")
        .collect();
    entries.sort();

    for name in entries {
        let child = if path == "/" {
            format!("/{name}")
        } else {
            format!("{path}/{name}")
        };
        let meta = fs
            .metadata(child.as_str())
            .map_err(|err| anyhow::anyhow!("метаданные {child} не читаются: {err}"))?;
        let indent = "  ".repeat(depth);
        // Права, владелец и группа печатаются всегда: они и есть причина, по
        // которой корневая ФС не FAT32, и их отсутствие обязано быть заметно.
        say!(
            "  {indent}{name}{}  {:04o}  uid {} gid {}  {} байт",
            if meta.is_dir() { "/" } else { "" },
            meta.mode() & 0o7777,
            meta.uid(),
            meta.gid(),
            meta.len(),
        );
        if meta.is_dir() {
            walk(fs, &child, depth + 1)?;
        } else if meta.len() > 0 && meta.len() < 4096 {
            // Мелкие файлы показываются целиком: на корневом разделе их
            // ровно два, и оба существуют, чтобы их прочли.
            let data = fs
                .read(child.as_str())
                .map_err(|err| anyhow::anyhow!("файл {child} не читается: {err}"))?;
            for line in String::from_utf8_lossy(&data).lines() {
                say!("  {indent}  | {line}");
            }
        }
    }
    Ok(())
}
