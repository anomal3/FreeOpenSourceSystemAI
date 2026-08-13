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

    println!();
    println!("образ: {} ({} МиБ)", path.display(), sectors / 2048);

    let table = gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("таблица разделов не читается: {err}"))?;
    println!("  GPT   : {}", table.disk_guid);
    println!(
        "  диапазон: LBA {}..{}",
        table.first_usable_lba, table.last_usable_lba
    );

    for partition in &table.partitions {
        let size = partition.range().bytes(disk::DEFAULT_SECTOR_SIZE);
        let kind = if partition.type_guid == gpt::ESP_TYPE {
            "ESP"
        } else if partition.type_guid == gpt::FREEOS_ROOT_TYPE {
            "FreeOS root"
        } else {
            "неизвестный"
        };
        println!();
        println!(
            "  раздел {}: {kind}, {} МиБ, LBA {}..{}",
            partition.index + 1,
            size / (1024 * 1024),
            partition.first_lba,
            partition.last_lba,
        );
        println!("    имя  : {}", partition.name_string());
        println!("    тип  : {}", partition.type_guid);
        println!("    GUID : {}", partition.unique_guid);
    }

    let Some(root) = table.find(gpt::FREEOS_ROOT_TYPE) else {
        println!();
        println!("корневого раздела FreeOS на образе нет");
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

/// Показать содержимое корневой файловой системы чужой реализацией.
fn print_root(partition: &[u8]) -> Result<()> {
    let fs = Ext4::load(Box::new(partition.to_vec()))
        .map_err(|err| anyhow::anyhow!("сторонний читатель не смонтировал ext2: {err}"))?;

    println!();
    println!("корневая файловая система (читает крейт ext4-view):");
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
        println!(
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
                println!("  {indent}  | {line}");
            }
        }
    }
    Ok(())
}
