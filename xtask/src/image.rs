//! Сборка настоящего загрузочного образа: GPT + FAT32 ESP.
//!
//! До Phase 8 запуск обходился драйвером VVFAT в QEMU: каталог хоста
//! подключался как FAT-раздел, и никакого образа не существовало вовсе. Для
//! цикла «поправил — запустил» это по-прежнему лучший вариант и остаётся
//! умолчанием, но проверить на нём нечего: ни таблицы разделов, ни
//! выравнивания, ни того, читает ли прошивка **нашу** файловую систему, — VVFAT
//! подставляет свою.
//!
//! Здесь образ собирается целиком в памяти теми же процедурами, которыми
//! установщик размечает настоящий диск (крейт `disk`), и сбрасывается на диск
//! одним куском. Это не просто удобство: любая ошибка в разметке всплывает
//! здесь, на хосте, где есть отладчик и `cargo test`, а не внутри UEFI-
//! приложения на живой машине.
//!
//! # Воспроизводимость
//!
//! Одно и то же содержимое даёт побайтово одинаковый образ. Для этого метки
//! времени фиксированы (эпоха FAT), а GUID диска и раздела выводятся из хеша
//! полезной нагрузки, а не из часов. Смысл тот же, что у слепка `initrd`:
//! «образ изменился» должно означать «изменилось содержимое», иначе признак
//! бесполезен.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use disk::gpt::{self, PartitionSpec};
use disk::guid::Guid;
use disk::{MemDisk, SECTOR_SIZE, fat32};

use crate::arch::{self, Arch, Component};
use crate::build::Built;
use crate::paths;
use crate::util;

/// Ревизия способа сборки образа; входит в слепок.
///
/// Слепок отслеживает содержимое, но не код этого модуля: правка геометрии
/// меняет байты образа, не меняя ни одного входного файла, и без явного
/// признака устаревший образ так и остался бы лежать в `build/`.
const FORMAT_REVISION: u32 = 1;

/// Метка тома ESP.
const VOLUME_LABEL: &str = "FREEOS ESP";

/// Запас на ESP сверх полезной нагрузки.
///
/// Четверть плюс 16 МиБ — не подобранное число, а ответ на два разных расхода.
/// Доля покрывает потери на хвостах кластеров и записи каталогов, слагаемое —
/// служебные структуры самого тома (две копии таблицы FAT на 64-мегабайтном
/// томе занимают около мегабайта) и место, чтобы на ESP можно было что-то
/// дописать, не пересобирая образ.
const SLACK_NUMERATOR: u64 = 1;
const SLACK_DENOMINATOR: u64 = 4;
const SLACK_FIXED: u64 = 16 * 1024 * 1024;

/// Наименьший разумный размер ESP.
///
/// Ниже 34 МиБ том вообще не может быть FAT32 (нужно 65525 кластеров), и
/// упираться в эту границу незачем: 64 МиБ дают запас и на кластер крупнее
/// сектора.
const MIN_ESP_BYTES: u64 = 64 * 1024 * 1024;

/// Свободное место в начале и в конце образа под GPT.
///
/// В начале мегабайт — это выравнивание первого раздела (см.
/// `gpt::ALIGNMENT_SECTORS`), в конце столько же — резервная копия таблицы плюс
/// запас, чтобы образ имел круглый размер.
const GPT_MARGIN_BYTES: u64 = 1024 * 1024;

/// Один файл, попадающий на ESP.
struct Payload {
    /// Путь внутри тома, разделитель — `/`.
    path: String,
    data: Vec<u8>,
    /// Откуда файл взят — только для сообщений пользователю.
    source: PathBuf,
}

/// Собирает образ, если он устарел, и возвращает путь к нему.
pub fn build(built: &Built) -> Result<PathBuf> {
    let arch = built.arch;
    let payload = collect(built)?;

    let image_path = paths::disk_image(arch, built.release);
    let stamp_path = paths::disk_image_stamp(arch, built.release);
    let stamp = stamp_text(&payload);

    // Пересборка только по изменению содержимого: образ — десятки мегабайт, и
    // формировать его на каждый запуск значит платить паузу за файлы, которые
    // меняются реже кода.
    if util::file_len(&image_path).is_some()
        && fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp.as_str())
    {
        println!("образ: актуален, пересборка не нужна ({})", image_path.display());
        return Ok(image_path);
    }

    let bytes = assemble(&payload)?;

    if let Some(parent) = image_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    // Слепок снимается перед записью образа: если запись оборвётся, следующий
    // запуск увидит отсутствующий слепок и пересоберёт образ, а не поверит
    // недописанному файлу.
    let _ = fs::remove_file(&stamp_path);
    fs::write(&image_path, &bytes)
        .with_context(|| format!("не удалось записать образ {}", image_path.display()))?;
    fs::write(&stamp_path, &stamp)
        .with_context(|| format!("не удалось записать слепок {}", stamp_path.display()))?;

    println!(
        "образ: {} файл(ов) -> {} ({} МиБ, GPT + FAT32 ESP)",
        payload.len(),
        image_path.display(),
        bytes.len() / (1024 * 1024),
    );
    for file in &payload {
        println!("  \\{:<24} {}", file.path.replace('/', "\\"), file.source.display());
    }

    Ok(image_path)
}

/// Читает всё, что должно попасть на ESP.
fn collect(built: &Built) -> Result<Vec<Payload>> {
    let arch = built.arch;
    let mut payload = Vec::new();

    for component in Component::ALL {
        let Some(source) = built.get(component) else {
            // Компонент не собирался (`--no-kernel`) — на образе его просто не
            // будет. Это законный сценарий: ядро обязано отсутствовать так же
            // осмысленно, как присутствовать.
            continue;
        };
        payload.push(read_payload(
            &esp_path_string(component.esp_path(arch)),
            source,
        )?);
    }

    if let Some(source) = built.initrd() {
        payload.push(read_payload(arch::INITRD_ESP_FILE, source)?);
    }

    if payload.is_empty() {
        bail!(
            "собирать образ не из чего: ни загрузчика, ни ядра, ни initrd.\n\
             Уберите --no-kernel/--no-initrd."
        );
    }

    Ok(payload)
}

fn read_payload(path: &str, source: &Path) -> Result<Payload> {
    let data =
        fs::read(source).with_context(|| format!("не удалось прочитать {}", source.display()))?;
    Ok(Payload {
        path: path.to_string(),
        data,
        source: source.to_path_buf(),
    })
}

/// Путь внутри ESP в виде, который понимает крейт `disk`.
///
/// `Component::esp_path` отдаёт `PathBuf`, а тот на Windows склеивается
/// обратными слэшами; в томе FAT разделитель один — `/`.
fn esp_path_string(path: PathBuf) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Формирует образ в памяти.
fn assemble(payload: &[Payload]) -> Result<Vec<u8>> {
    let content: u64 = payload.iter().map(|file| file.data.len() as u64).sum();
    let esp_bytes = esp_size(content);
    let disk_bytes = 2 * GPT_MARGIN_BYTES + esp_bytes;
    let sectors = disk_bytes / SECTOR_SIZE as u64;

    let mut dev = MemDisk::new(sectors).with_context(|| {
        format!(
            "не удалось разместить в памяти образ на {} МиБ",
            disk_bytes / (1024 * 1024)
        )
    })?;

    let layout = gpt::plan(sectors, esp_bytes, false)
        .map_err(|err| anyhow::anyhow!("не удалось спланировать разметку образа: {err}"))?;

    // Хеш содержимого служит источником «случайности» для GUID: настоящая
    // случайность сделала бы образ невоспроизводимым, а уникальность в пределах
    // одного образа от этого не страдает — соли у идентификаторов разные.
    let seed = content_hash(payload);

    gpt::wipe(&mut dev).map_err(|err| anyhow::anyhow!("не удалось очистить образ: {err}"))?;
    gpt::write(
        &mut dev,
        Guid::from_entropy(expand(seed, b"freeos-disk-guid")),
        &[PartitionSpec {
            type_guid: gpt::ESP_TYPE,
            unique_guid: Guid::from_entropy(expand(seed, b"freeos-esp-guid")),
            first_lba: layout.esp.first_lba,
            last_lba: layout.esp.last_lba,
            attributes: 0,
            name: "FreeOS ESP",
        }],
    )
    .map_err(|err| anyhow::anyhow!("не удалось записать таблицу разделов: {err}"))?;

    let mut volume = fat32::format(
        &mut dev,
        layout.esp,
        &fat32::FormatOptions {
            label: VOLUME_LABEL,
            volume_id: (seed >> 32) as u32 ^ seed as u32,
            // Фиксированная метка времени — та же причина, что и у initrd:
            // одно и то же содержимое обязано давать один и тот же образ.
            timestamp: fat32::Timestamp::EPOCH,
        },
    )
    .map_err(|err| anyhow::anyhow!("не удалось отформатировать ESP: {err}"))?;

    // Проверка до первой записи: «не поместилось» на середине раскладки
    // оставило бы образ, который выглядит собранным.
    let free = volume.free_bytes();
    if free < content {
        bail!(
            "содержимое не помещается на ESP: нужно {} КиБ, свободно {} КиБ.\n\
             Увеличьте запас (SLACK_* в xtask/src/image.rs).",
            content / 1024,
            free / 1024,
        );
    }

    for file in payload {
        volume
            .write_file_path(&mut dev, &file.path, &file.data)
            .map_err(|err| {
                anyhow::anyhow!("не удалось записать в образ \\{}: {err}", file.path)
            })?;
    }

    volume
        .finish(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось завершить том FAT32: {err}"))?;

    Ok(dev.into_vec())
}

/// Размер ESP под заданный объём полезной нагрузки.
fn esp_size(content: u64) -> u64 {
    let wanted = content + content * SLACK_NUMERATOR / SLACK_DENOMINATOR + SLACK_FIXED;
    let bytes = wanted.max(MIN_ESP_BYTES);
    // Выравнивание вверх до мегабайта: раздел всё равно начинается и
    // заканчивается на границе мегабайта, и дробный остаток просто пропал бы.
    bytes.div_ceil(1024 * 1024) * (1024 * 1024)
}

/// FNV-1a, 64 бита — тот же выбор и по той же причине, что в сборщике initrd:
/// вопрос стоит «изменилось содержимое или нет», а не «подделали ли его».
fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Хеш всего содержимого образа: и путей, и данных.
fn content_hash(payload: &[Payload]) -> u64 {
    let mut hash = fnv1a64(&FORMAT_REVISION.to_le_bytes());
    for file in payload {
        hash ^= fnv1a64(file.path.as_bytes());
        hash = hash.rotate_left(17) ^ fnv1a64(&file.data);
    }
    hash
}

/// Растягивает 64-битный хеш в 16 байт под GUID, подмешивая назначение.
///
/// Соль нужна, чтобы идентификатор диска и идентификатор раздела при одном и
/// том же содержимом не совпали: совпадающие GUID — законный повод для утилит
/// счесть разметку испорченной.
fn expand(seed: u64, salt: &[u8]) -> [u8; 16] {
    let mut low = fnv1a64(salt) ^ seed;
    let high = fnv1a64(&seed.to_le_bytes()).rotate_left(31) ^ fnv1a64(salt);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&high.to_be_bytes());
    low = low.rotate_left(13);
    out[8..].copy_from_slice(&low.to_be_bytes());
    out
}

/// Текст слепка: по строке на файл плюс геометрия.
///
/// Хранится текстом, а не одним числом, намеренно: когда образ вдруг
/// пересобирается (или, наоборот, не пересобирается), файл слепка можно просто
/// открыть и увидеть, что именно разошлось.
fn stamp_text(payload: &[Payload]) -> String {
    let content: u64 = payload.iter().map(|file| file.data.len() as u64).sum();
    let mut text = format!(
        "rev={FORMAT_REVISION} esp={} label={VOLUME_LABEL}\n",
        esp_size(content)
    );
    for file in payload {
        text.push_str(&format!(
            "f {:>10} {:016x} {}\n",
            file.data.len(),
            fnv1a64(&file.data),
            file.path
        ));
    }
    text
}

/// Печатает, что лежит в образе и как его записать на настоящий носитель.
pub fn describe(arch: Arch, path: &Path) {
    println!();
    println!("образ готов: {}", path.display());
    println!();
    println!("Запустить в эмуляторе:");
    println!("    cargo xtask run --arch {arch} --image");
    println!();
    println!("Записать на USB-носитель (всё, что на нём было, будет уничтожено):");
    if cfg!(windows) {
        println!("    любым записывателем образов в режиме посекторной записи —");
        println!("    Rufus (режим DD), balenaEtcher, USBImager.");
    } else {
        println!(
            "    sudo dd if={} of=/dev/sdX bs=4M status=progress conv=fsync",
            path.display()
        );
    }
    println!();
    println!(
        "Образ содержит одну таблицу GPT и один раздел ESP; корневого раздела в нём нет —\n\
         корневой ФС у системы пока тоже нет. Разметку с корневым разделом делает установщик."
    );
}
