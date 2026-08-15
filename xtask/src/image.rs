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
use disk::{DEFAULT_SECTOR_SIZE as SECTOR_SIZE, MemDisk, fat32, iso9660};

use crate::arch::{self, Arch, Component};
use crate::build::Built;
use crate::paths;
use crate::util;
use crate::version;

/// Что за образ собирается.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Готовая система: прошивка запускает загрузчик, тот поднимает ядро.
    System,
    /// Установочный носитель: прошивка запускает установщик, а система лежит
    /// рядом полезной нагрузкой.
    Installer,
}

impl Kind {
    /// Часть имени файла образа.
    const fn slug(self) -> &'static str {
        match self {
            Kind::System => "FreeOS",
            Kind::Installer => "FreeOS-Installer",
        }
    }

    /// Метка тома ESP.
    const fn label(self) -> &'static str {
        match self {
            Kind::System => "FREEOS ESP",
            Kind::Installer => "FREEOS INST",
        }
    }
}

/// Метка загрузочного тома ISO — то, чем носитель подписан для человека,
/// открывшего его файловым менеджером хоста.
///
/// Версия в ней ровно затем же, зачем и в имени файла: подключённый в
/// гипервизоре образ имени файла уже не показывает, а метку — показывает.
/// В одиннадцать байт метки FAT32 `FREEOS 0.1` помещается; более длинную
/// запись форматирование обрежет само, и это не потеря — рядом лежит
/// `README.TXT`, где версия написана целиком.
fn volume_label() -> String {
    format!("FREEOS {}", crate::version::VERSION)
}

/// Ревизия способа сборки образа; входит в слепок.
///
/// Слепок отслеживает содержимое, но не код этого модуля: правка геометрии
/// меняет байты образа, не меняя ни одного входного файла, и без явного
/// признака устаревший образ так и остался бы лежать в `build/`.
const FORMAT_REVISION: u32 = 1;

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
pub fn build(built: &Built, kind: Kind) -> Result<PathBuf> {
    let arch = built.arch;
    let payload = collect(built, kind)?;

    let stamp_path = paths::disk_image_stamp(kind.slug(), arch, built.release);
    let stamp = stamp_text(&payload, kind);
    let build = version::resolve_build(&stamp_path, &stamp)?;
    let image_path = paths::disk_image(kind.slug(), build.number, arch, built.release);

    // Пересборка только по изменению содержимого: образ — десятки мегабайт, и
    // формировать его на каждый запуск значит платить паузу за файлы, которые
    // меняются реже кода.
    if reuse(&build, &image_path, &stamp_path, &stamp, "образ", |number| {
        paths::disk_image(kind.slug(), number, arch, built.release)
    })? {
        return Ok(image_path);
    }

    let bytes = assemble(&payload, kind)?;

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
    fs::write(&stamp_path, version::stamp_with_build(build.number, &stamp))
        .with_context(|| format!("не удалось записать слепок {}", stamp_path.display()))?;

    println!(
        "образ: {} файл(ов) -> {} ({} МиБ, GPT + FAT32 ESP)",
        payload.len(),
        image_path.display(),
        bytes.len() / (1024 * 1024),
    );
    retire(build.previous, build.number, |number| {
        paths::disk_image(kind.slug(), number, arch, built.release)
    });
    for file in &payload {
        println!("  \\{:<24} {}", file.path.replace('/', "\\"), file.source.display());
    }

    Ok(image_path)
}

/// Читает всё, что должно попасть на ESP.
fn collect(built: &Built, kind: Kind) -> Result<Vec<Payload>> {
    let arch = built.arch;
    let mut payload = Vec::new();

    // Разница между образами вся здесь, в путях. У системного образа загрузчик
    // лежит по стандартному пути removable media, и прошивка запускает его. У
    // установочного этот путь занимает установщик, а система уезжает в каталог
    // полезной нагрузки — откуда установщик её и читает.
    match kind {
        Kind::System => {
            for component in Component::SYSTEM {
                let Some(source) = built.get(component) else {
                    // Компонент не собирался (`--no-kernel`) — на образе его
                    // просто не будет. Это законный сценарий: ядро обязано
                    // отсутствовать так же осмысленно, как присутствовать.
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
        }
        Kind::Installer => {
            let installer = built.get(Component::Installer).ok_or_else(|| {
                anyhow::anyhow!("установщик не собран, а без него установочный носитель бессмыслен")
            })?;
            payload.push(read_payload(
                &esp_path_string(Component::Installer.esp_path(arch)),
                installer,
            )?);

            // Установщик открывает ровно эти пути и отказывается работать,
            // если хоть одного нет, — поэтому здесь они обязательны, в отличие
            // от системного образа.
            for component in Component::SYSTEM {
                let path = component
                    .payload_path(arch)
                    .expect("у компонентов системы путь полезной нагрузки есть всегда");
                let source = built.get(component).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{} не собран, а установочный носитель без него неполон",
                        component.title()
                    )
                })?;
                payload.push(read_payload(&path, source)?);
            }
            let initrd = built.initrd().ok_or_else(|| {
                anyhow::anyhow!("initrd не собран, а установочный носитель без него неполон")
            })?;
            payload.push(read_payload(arch::PAYLOAD_INITRD, initrd)?);

            // Доверенные ключи обновления. Едут на носитель отдельным файлом и
            // ложатся установщиком в корень как `/os-keys`: система, у которой
            // их нет, обновиться не сможет вовсе — и это правильный отказ по
            // умолчанию, потому что принимать неподписанное хуже.
            payload.push(Payload {
                path: arch::PAYLOAD_KEYS.to_string(),
                data: crate::keys::trusted_text()?.into_bytes(),
                // Источника-файла у них нет: текст собирается на месте из
                // открытой половины ключа. Слепку пересборки этого достаточно —
                // он считает содержимое, а не пути.
                source: paths::build_dir().join("keys"),
            });

            // Пользовательские программы — их установщик кладёт не на ESP, а на
            // корневой раздел, в `/bin`. Список имён здесь и в
            // `crates/installer/src/payload.rs` обязан совпадать; расходится он
            // не молча — установленная система без `/bin/perms` валит сценарий
            // `installed`.
            for (name, path) in built.programs() {
                let target = format!("{}/{}", arch::PAYLOAD_BIN_DIR, medium_name(name));
                payload.push(read_payload(&target, path)?);
            }

            // Эталонные настройки. Единственный экземпляр этих файлов лежит в
            // репозитории (`initrd/usr/share/defaults/etc/`), и на установленную
            // систему они попадают отсюда — установщик кладёт их в корневой
            // образ, а не на раздел состояния. Почему именно так, сказано у
            // `arch::PAYLOAD_DEFAULTS_DIR`.
            for (name, medium) in arch::PAYLOAD_DEFAULTS {
                let target = format!("{}/{medium}", arch::PAYLOAD_DEFAULTS_DIR);
                payload.push(read_payload(&target, &paths::defaults_dir().join(name))?);
            }

            // Образцовые пакеты. Имена на носителе — короткие 8.3, потому что
            // это FAT; настоящие имена (`hello-1.0.fpk`) знает установщик и
            // ставит файлы под ними в `/media`.
            for package in crate::package::build_samples(arch, built.release)? {
                let short = package
                    .file_name
                    .split('-')
                    .next()
                    .unwrap_or("PKG")
                    .to_uppercase();
                let target = format!("{}/{short}.FPK", arch::PAYLOAD_PKG_DIR);
                payload.push(read_payload(&target, &package.path)?);
            }
        }
    }

    if payload.is_empty() {
        bail!(
            "собирать образ не из чего: ни загрузчика, ни ядра, ни initrd.\n\
             Уберите --no-kernel/--no-initrd."
        );
    }

    Ok(payload)
}

/// Как программа называется на установочном носителе.
///
/// Том носителя пишется без длинных имён (см. заголовок `disk::fat32`), то есть
/// имена там только 8.3. Почти все программы в восемь знаков укладываются;
/// `sysupdate` — нет, и укоротить его молча нельзя: имя обязано совпадать с тем,
/// что открывает установщик (`PROGRAMS` в `crates/installer/src/payload.rs`).
/// Поэтому исключение записано явно, а не выводится усечением.
fn medium_name(name: &str) -> String {
    match name {
        "sysupdate" => String::from("SYSUPD"),
        other => other.to_uppercase(),
    }
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
fn assemble(payload: &[Payload], kind: Kind) -> Result<Vec<u8>> {
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

    let layout = gpt::plan(sectors, SECTOR_SIZE, esp_bytes, gpt::Scheme::EspOnly)
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
            label: kind.label(),
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
fn stamp_text(payload: &[Payload], kind: Kind) -> String {
    let content: u64 = payload.iter().map(|file| file.data.len() as u64).sum();
    let mut text = format!(
        "rev={FORMAT_REVISION} version={} esp={} label={}\n",
        crate::version::VERSION,
        esp_size(content),
        kind.label(),
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

/// Готовит чистый диск, на который будет ставить установщик.
///
/// Файл создаётся разрежённым (`set_len` не пишет ни байта) — гигабайт нулей
/// на диске хоста ради того, чтобы установщик их перезаписал, никому не нужен.
pub fn prepare_target(arch: Arch, size_mib: u64, fresh: bool) -> Result<PathBuf> {
    let path = paths::target_disk(arch);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }

    let size = size_mib * 1024 * 1024;
    let exists = util::file_len(&path).is_some();
    if exists && !fresh {
        println!("целевой диск: {} (как есть)", path.display());
        return Ok(path);
    }

    // Пересоздание — единственная операция во всём xtask, которая уничтожает
    // данные, поэтому она делается только по явному `--fresh` (или когда
    // диска ещё нет) и обязательно сообщает о себе.
    if exists {
        println!("целевой диск: {} пересоздаётся (--fresh)", path.display());
    }
    let file = fs::File::create(&path)
        .with_context(|| format!("не удалось создать целевой диск {}", path.display()))?;
    file.set_len(size)
        .with_context(|| format!("не удалось задать размер {size} байт для {}", path.display()))?;
    println!("целевой диск: {} ({size_mib} МиБ, пустой)", path.display());
    Ok(path)
}

/// Собрать загрузочный ISO и вернуть путь к нему.
///
/// Отличается от [`build`] не содержимым, а упаковкой: те же файлы, тот же том
/// FAT32, но вместо таблицы разделов вокруг него — ISO 9660 с записью El Torito.
/// Разница в том, что делает с этим человек: образ диска надо записать на
/// носитель целиком, а ISO — подключить одним пунктом меню гипервизора.
pub fn build_iso(built: &Built, kind: Kind) -> Result<PathBuf> {
    let arch = built.arch;
    let payload = collect(built, kind)?;

    let stamp_path = paths::iso_stamp(kind.slug(), arch, built.release);
    // Ревизия формата входит в слепок, и это не перестраховка: правка самого
    // генератора ISO содержимого не меняет, поэтому образ считался бы
    // актуальным и не пересобирался. Ровно так и вышло при переходе каталога
    // El Torito на секционную раскладку — исправленный код собрал тот же файл,
    // и загрузка «по-прежнему» не работала.
    const ISO_FORMAT_REVISION: u32 = 2;
    let stamp = format!("iso-format={ISO_FORMAT_REVISION}
{}", stamp_text(&payload, kind));
    let build = version::resolve_build(&stamp_path, &stamp)?;
    let path = paths::iso_image(kind.slug(), build.number, arch, built.release);

    if reuse(&build, &path, &stamp_path, &stamp, "iso", |number| {
        paths::iso_image(kind.slug(), number, arch, built.release)
    })? {
        return Ok(path);
    }

    // Том FAT32 собирается без разметки вокруг: внутри ISO он не раздел, а
    // просто образ, который прошивка монтирует целиком. Ровно то же делает
    // «суперфлоппи» — носитель без таблицы разделов.
    let boot = assemble_fat(&payload)?;

    let readme = iso_readme(arch, kind);
    let label = volume_label();
    let bytes = iso9660::build(&iso9660::Options {
        label: &label,
        boot_image: &boot,
        files: &[("README.TXT", readme.as_bytes())],
    });

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    let _ = fs::remove_file(&stamp_path);
    fs::write(&path, &bytes)
        .with_context(|| format!("не удалось записать образ {}", path.display()))?;
    fs::write(&stamp_path, version::stamp_with_build(build.number, &stamp))
        .with_context(|| format!("не удалось записать слепок {}", stamp_path.display()))?;

    println!(
        "iso: {} файл(ов) -> {} ({} МиБ, ISO 9660 + El Torito/EFI)",
        payload.len(),
        path.display(),
        bytes.len() / (1024 * 1024),
    );
    retire(build.previous, build.number, |number| {
        paths::iso_image(kind.slug(), number, arch, built.release)
    });
    Ok(path)
}

/// Можно ли обойтись без пересборки — и если да, довести дело до конца.
///
/// Два разных «не надо пересобирать». Первое очевидно: файл уже лежит там, где
/// нужно. Второе тоньше и появилось вместе с общим номером сборки: содержимое
/// то же, а номер новый — потому что правка была в чужой архитектуре или в
/// другом профиле, и состояние кода изменилось, хотя байты этого образа нет.
/// Тогда файл достаточно **переименовать**: собирать заново семьдесят
/// мегабайт, чтобы получить те же семьдесят мегабайт, незачем.
///
/// Возвращает `true`, если образ уже готов и лежит по нужному пути.
fn reuse(
    build: &version::Build,
    path: &Path,
    stamp_path: &Path,
    stamp: &str,
    what: &str,
    path_of: impl Fn(u32) -> PathBuf,
) -> Result<bool> {
    if !build.unchanged {
        return Ok(false);
    }
    if util::file_len(path).is_some() {
        println!("{what}: актуален, пересборка не нужна ({})", path.display());
        return Ok(true);
    }

    let Some(previous) = build.previous.filter(|number| *number != build.number) else {
        return Ok(false);
    };
    let old_path = path_of(previous);
    if util::file_len(&old_path).is_none() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    // Неудача переименования — не отказ: вызывающий просто соберёт образ
    // заново, и результат будет тот же самый, только дороже.
    if fs::rename(&old_path, path).is_err() {
        return Ok(false);
    }
    fs::write(stamp_path, version::stamp_with_build(build.number, stamp))
        .with_context(|| format!("не удалось записать слепок {}", stamp_path.display()))?;
    println!(
        "{what}: содержимое то же, сборка {previous} -> {} ({})",
        build.number,
        path.display()
    );
    Ok(true)
}

/// Убрать образ предыдущей сборки — тот, который только что заменили.
///
/// Иначе каталог сборки набирает по семьдесят мегабайт на каждую правку, а в
/// нём копятся загрузочные образы, отличающиеся только номером, и подключить
/// вчерашний становится так же легко, как сегодняшний. Устаревший образ — это
/// не история, это ловушка: история хранится в git, а образ, собранный из кода,
/// которого больше нет, не подтверждает ничего.
///
/// Занятый файл (например, подключённый к работающей машине) — не отказ сборки:
/// новый образ уже записан, а о лишнем сказано вслух.
fn retire(previous: Option<u32>, current: u32, path_of: impl Fn(u32) -> PathBuf) {
    let Some(previous) = previous.filter(|number| *number != current) else {
        return;
    };
    let path = path_of(previous);
    if util::file_len(&path).is_none() {
        return;
    }
    match fs::remove_file(&path) {
        Ok(()) => println!("  заменяет сборку {previous} ({})", path.display()),
        Err(err) => println!("  сборка {previous} осталась лежать: {err}"),
    }
}

/// Том FAT32 целиком, без таблицы разделов: то, что уедет внутрь ISO.
fn assemble_fat(payload: &[Payload]) -> Result<Vec<u8>> {
    let content: u64 = payload.iter().map(|file| file.data.len() as u64).sum();
    let bytes = esp_size(content);
    let sectors = bytes / SECTOR_SIZE as u64;

    let mut dev = MemDisk::new(sectors)
        .with_context(|| format!("не удалось разместить том на {} МиБ", bytes / (1024 * 1024)))?;

    let seed = content_hash(payload);
    let range = gpt::Range { first_lba: 0, last_lba: sectors - 1 };
    let label = volume_label();
    let mut volume = fat32::format(
        &mut dev,
        range,
        &fat32::FormatOptions {
            label: &label,
            volume_id: (seed >> 32) as u32 ^ seed as u32,
            timestamp: fat32::Timestamp::EPOCH,
        },
    )
    .map_err(|err| anyhow::anyhow!("не удалось отформатировать загрузочный том: {err}"))?;

    for file in payload {
        volume
            .write_file_path(&mut dev, &file.path, &file.data)
            .map_err(|err| anyhow::anyhow!("не удалось записать в том {}: {err}", file.path))?;
    }
    volume
        .finish(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось завершить том FAT32: {err}"))?;

    Ok(dev.into_vec())
}

/// Текст, который человек увидит, открыв образ файловым менеджером.
///
/// Существует потому, что ISO попадает к людям, а не только на стенд: носитель
/// без единого читаемого файла выглядит пустым, и первое, что делает
/// получивший его, — проверяет, не скачался ли он битым.
fn iso_readme(arch: Arch, kind: Kind) -> String {
    let what = match kind {
        Kind::System => "the live system: it boots and runs from this medium, writing nothing",
        Kind::Installer => "the installer: it partitions a disk and installs the system onto it",
    };
    format!(
        "FreeOS {version} bootable image ({arch})



         This is {what}.

         

         Boot it in a virtual machine with EFI enabled, or write it to a USB stick.

         The bootloader lives inside an EFI System Partition image referenced by the

         El Torito boot catalog; there is no BIOS boot path, by design.

         

         https://github.com/anomal3/FreeOpenSourseSystemAI

",
        version = crate::version::VERSION,
        arch = arch.name(),
    )
}

/// Печатает, что лежит в образе и как его записать на настоящий носитель.
pub fn describe(arch: Arch, path: &Path, kind: Kind) {
    println!();
    println!("образ готов: {}", path.display());
    println!();
    println!("Запустить в эмуляторе:");
    match kind {
        Kind::System => println!("    cargo xtask run --arch {arch} --image"),
        Kind::Installer => println!("    cargo xtask install --arch {arch}"),
    }
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
    match kind {
        Kind::System => println!(
            "Образ содержит одну таблицу GPT и один раздел ESP; корневого раздела в нём нет —\n\
             корневой ФС у системы пока тоже нет. Разметку с корневым разделом делает установщик."
        ),
        Kind::Installer => println!(
            "На образе лежит установщик (по стандартному пути \\EFI\\BOOT) и переносимая им\n\
             система (в каталоге \\FREEOS). Прошивка запускает установщик, он размечает\n\
             выбранный диск и переносит систему туда."
        ),
    }
}
