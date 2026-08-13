//! Номер версии системы — одно место, откуда его берут имена образов, метки
//! томов и README внутри ISO.
//!
//! # Зачем номер понадобился
//!
//! Потому что образ, попавший к человеку, ничем не отличался от образа,
//! собранного двумя часами раньше: `freeos-aarch64-release.iso` — и всё. Так и
//! вышло, что система в VirtualBox сообщала о неопознанном контроллере
//! прерываний уже после того, как это было исправлено: в гипервизоре был
//! подключён прежний файл, и понять это по имени было нельзя.
//!
//! # Откуда берётся значение
//!
//! Из `version` в `[workspace.package]`, то есть оттуда же, откуда его берёт
//! ядро для своего баннера. Второй список версий в проекте разошёлся бы с
//! первым, и разошёлся бы молча.
//!
//! Патч-версия в имя не входит: `0.1.0` и `0.1` для человека, выбирающего файл
//! в списке, — одно и то же, а третье число заставляло бы поднимать его при
//! каждой правке, иначе оно врёт.

/// Версия для человека: мажор и минор из `Cargo.toml` workspace.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
);

/// Строка, которой ядро подписывается в баннере (`crates/kernel/src/main.rs`).
///
/// Собирается здесь, а не пишется в сценариях стенда буквой, ровно потому, что
/// версия в ней меняется. Раньше сценарии ждали `"FreeOS kernel v"` — подстроку
/// без версии, — и это не просто хрупко: стенд, ждущий строку без номера,
/// одинаково рад и свежему ядру, и тому, что осталось от прошлой сборки. Теперь
/// совпадение версии в баннере с версией сборки проверяется каждым прогоном.
pub const KERNEL_BANNER: &str = concat!(
    "FreeOS ",
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR"),
    " kernel"
);

// ---------------------------------------------------------------------------
// Номер сборки
// ---------------------------------------------------------------------------
//
// Версии `0.1` мало: она стоит на месте месяцами, а образ за один вечер
// пересобирается пять раз, и все пять называются одинаково. Именно так и вышло:
// на машине проверялся файл, собранный до исправления, а отличить его от файла,
// собранного после, было нечем — ни по имени, ни по экрану.
//
// # Что именно номер обозначает
//
// **Состояние исходников, а не отдельный файл.** Debug и release — это одна и
// та же сборка, показанная двумя способами, и номер у них обязан быть один.
// Точно так же образ для ARM64 и образ для x86-64, собранные из одного кода, —
// одна сборка. Номер меняется, когда меняется код: поправили ARM64 — новый
// номер у всех четырёх образов. Появился новый функционал — поднимается и
// версия в `[workspace.package]`, руками, потому что «новый функционал» решает
// человек, а не счётчик.
//
// Первая попытка считала номер по слепку каждого образа отдельно, и четыре
// файла из одного кода получили четыре разных номера. Это отвечало на вопрос
// «которое по счёту изменение этого файла» — не тот вопрос.
//
// # Почему счётчик, а не время сборки
//
// Потому что время идёт и тогда, когда ничего не менялось. Номер поднимается
// на изменение отпечатка исходников; собрали дважды подряд без правок — номер
// один и тот же, и это верно: код тот же самый.
//
// # Почему номер только в имени файла
//
// Внутрь образа он не попадает намеренно. Счётчик — свойство истории **этого**
// каталога сборки, а не содержимого: у человека, склонировавшего репозиторий,
// та же система получит другой номер. Положить его в метку тома или в README
// значило бы, что одинаковое содержимое даёт разные байты, — а
// воспроизводимость образа [`crate::image`] держит сознательно.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};

use crate::paths;

/// Какой номер достался этой сборке.
pub struct Build {
    /// Номер сборки — общий для всех образов из этого состояния кода.
    pub number: u32,
    /// Содержимое совпало со слепком: пересобирать нечего. Номер при этом
    /// вполне может быть новым — тогда файл достаточно переименовать.
    pub unchanged: bool,
    /// Номер, под которым лежит образ от прошлого раза. `None`, если слепка не
    /// было вовсе.
    pub previous: Option<u32>,
}

/// Файл со счётчиком: номер и отпечаток кода, которому он выдан.
fn counter() -> PathBuf {
    paths::build_dir().join("build-number")
}

/// Что входит в отпечаток исходников.
///
/// Всё, что способно изменить собранный образ, и ничего сверх того. `target/` и
/// `build/` исключены как производные, документация — как не влияющая на байты:
/// номер, меняющийся от правки README, перестал бы означать «другой код».
const SOURCES: &[&str] = &[
    "crates",
    "xtask/src",
    "initrd",
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    ".cargo/config.toml",
];

/// Выдать номер этой сборке образа.
///
/// `stamp` — текст слепка содержимого; по нему решается, нужна ли пересборка.
/// Номер от слепка не зависит: он один на всё состояние кода.
pub fn resolve_build(stamp_path: &Path, stamp: &str) -> Result<Build> {
    let number = build_number()?;
    let recorded = fs::read_to_string(stamp_path).ok();
    let parsed = recorded.as_deref().and_then(split_stamp);

    Ok(Build {
        number,
        unchanged: parsed.is_some_and(|(_, text)| text == stamp),
        previous: parsed.map(|(number, _)| number),
    })
}

/// Номер сборки для текущего состояния исходников.
///
/// Считается один раз за запуск: обход дерева стоит недорого (два с половиной
/// мегабайта), но делать его на каждый образ незачем — а главное, номер обязан
/// быть одним и тем же для всех образов одного запуска.
pub fn build_number() -> Result<u32> {
    static CACHE: OnceLock<u32> = OnceLock::new();
    if let Some(number) = CACHE.get() {
        return Ok(*number);
    }

    let fingerprint = fingerprint()?;
    let path = counter();
    let recorded = fs::read_to_string(&path).unwrap_or_default();
    let (last, last_fingerprint) = parse_counter(&recorded);

    let number = if last_fingerprint == Some(fingerprint) && last != 0 {
        last
    } else {
        let next = last.saturating_add(1);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
        }
        fs::write(&path, format!("build={next}\nsource={fingerprint:016x}\n"))
            .with_context(|| format!("не удалось записать счётчик сборок {}", path.display()))?;
        next
    };

    // Гонки здесь нет: xtask однопоточен, а `set` при занятой ячейке просто
    // вернёт ошибку, и мы возьмём то, что уже лежит.
    let _ = CACHE.set(number);
    Ok(number)
}

/// Разобрать файл счётчика. Испорченный или отсутствующий даёт `(0, None)` —
/// нумерация начнётся заново, и это дешевле, чем останавливать сборку.
fn parse_counter(text: &str) -> (u32, Option<u64>) {
    let mut number = 0;
    let mut fingerprint = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("build=") {
            number = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("source=") {
            fingerprint = u64::from_str_radix(value.trim(), 16).ok();
        }
    }
    (number, fingerprint)
}

/// Отпечаток состояния исходников: одно число на всё дерево.
fn fingerprint() -> Result<u64> {
    let root = paths::workspace_root();
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for entry in SOURCES {
        absorb(&mut hash, &root.join(entry), entry)?;
    }
    Ok(hash)
}

/// Подмешать в отпечаток файл или каталог целиком.
///
/// Каталог обходится в порядке имён, а не в том, в каком его отдаёт
/// файловая система: иначе один и тот же код давал бы разный отпечаток на
/// разных машинах — и номер сборки менялся бы сам собой.
fn absorb(hash: &mut u64, path: &Path, name: &str) -> Result<()> {
    let Ok(meta) = fs::metadata(path) else {
        // Отсутствующий путь — не ошибка: `Cargo.lock` может быть не создан, а
        // `.cargo/config.toml` не существовать вовсе.
        return Ok(());
    };

    if meta.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(path)
            .with_context(|| format!("не удалось прочитать каталог {}", path.display()))?
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("не удалось перечислить {}", path.display()))?
            .into_iter()
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for entry in entries {
            let child = entry.file_name().unwrap_or_default().to_string_lossy().into_owned();
            absorb(hash, &entry, &format!("{name}/{child}"))?;
        }
        return Ok(());
    }

    let data = fs::read(path)
        .with_context(|| format!("не удалось прочитать исходник {}", path.display()))?;
    mix(hash, name.as_bytes());
    mix(hash, &data);
    Ok(())
}

/// FNV-1a: не криптография, а способ заметить, что байты стали другими.
fn mix(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    *hash = hash.rotate_left(17);
}

/// Слепок с номером сборки в первой строке — то, что лежит в файле слепка.
#[must_use]
pub fn stamp_with_build(number: u32, stamp: &str) -> String {
    format!("build={number}\n{stamp}")
}

/// Разобрать файл слепка обратно на номер и содержимое.
///
/// Слепок без строки `build=` — от прежней версии xtask; такой считается
/// несовпавшим, и образ пересоберётся под новым номером. Это дешевле, чем
/// придумывать, каким номером задним числом подписать то, что собрано без него.
fn split_stamp(text: &str) -> Option<(u32, &str)> {
    let rest = text.strip_prefix("build=")?;
    let end = rest.find('\n')?;
    let number = rest[..end].parse().ok()?;
    Some((number, &rest[end + 1..]))
}
