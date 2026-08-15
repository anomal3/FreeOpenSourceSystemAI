//! Репозиторий обновлений: каталог, который выкладывают на сервер.
//!
//! # Что получается
//!
//! ```text
//!   <каталог>/index                    текст: что предлагается и с каким хешем
//!   <каталог>/index.sig                подпись индекса
//!   <каталог>/freeos-<версия>-<арх>.fpk  сами образы
//! ```
//!
//! Всё, что нужно серверу, — раздавать этот каталог по HTTP как обычные файлы.
//! Ни базы, ни скриптов: система читает три файла и проверяет подписи сама.
//!
//! # Почему индекс подписывается здесь, а не на сервере
//!
//! Потому что закрытый ключ на сервере, раздающем файлы в интернет, — это
//! закрытый ключ, которого больше нет. Подписывает машина сборки, сервер
//! раздаёт готовое; взломанный сервер может отдать старое или ничего, но не
//! может подписать своё.
//!
//! # Один индекс на обе архитектуры
//!
//! Записей в нём столько, сколько архитектур собрали, и машина берёт свою.
//! Второй индекс рядом означал бы, что подписей две, а выложены они порознь — то
//! есть что однажды одна из них устареет молча.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use osupdate::index::build::{Offer, hash, render, render_signature};

use crate::arch::Arch;
use crate::package::Flavour;
use crate::{build, keys, package, paths};

/// Куда складывается готовый репозиторий, если не сказано иначе.
pub fn default_dir() -> PathBuf {
    paths::build_dir().join("repo")
}

/// Собрать репозиторий из перечисленных архитектур.
///
/// Возвращает путь к каталогу. `version` — версия, которую понесут образы: она
/// же попадает в имя файла, в `/os-release` внутри образа и в индекс.
pub fn build(arches: &[Arch], release: bool, version: &str, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;

    let mut files: Vec<(String, String, u64, [u8; 32])> = Vec::new();
    for &arch in arches {
        let built = build::build_all(&build::BuildOptions {
            arch,
            release,
            kernel: true,
            initrd: true,
            installer: false,
        })?;
        let (Some(kernel), Some(initrd)) = (built.get(crate::arch::Component::Kernel), built.initrd())
        else {
            anyhow::bail!("для репозитория нужны собранные ядро и initrd ({arch})");
        };
        let programs: Vec<(&'static str, PathBuf)> = built
            .programs()
            .map(|(name, path)| (name, path.to_path_buf()))
            .collect();

        let package = package::build_system(
            arch,
            release,
            version,
            kernel,
            initrd,
            &programs,
            Flavour::Good,
        )?;
        // Имя в репозитории **всегда** несёт архитектуру, даже если собрана
        // одна: два файла с одним именем на одном сервере — это вопрос времени,
        // а не возможности.
        let name = format!("freeos-{version}-{}.fpk", arch.name());
        let bytes = fs::read(&package.path)
            .with_context(|| format!("не удалось прочитать {}", package.path.display()))?;
        let target = dir.join(&name);
        fs::write(&target, &bytes)
            .with_context(|| format!("не удалось записать {}", target.display()))?;
        files.push((
            String::from(arch.name()),
            name,
            bytes.len() as u64,
            hash(&bytes),
        ));
        println!("репозиторий: {} ({} МиБ)", target.display(), bytes.len() / (1024 * 1024));
    }

    let offers: Vec<Offer<'_>> = files
        .iter()
        .map(|(arch, file, size, sha256)| Offer {
            version,
            arch,
            file,
            size: *size,
            sha256: *sha256,
        })
        .collect();
    let index = render(&offers);
    let index_path = dir.join("index");
    fs::write(&index_path, index.as_bytes())
        .with_context(|| format!("не удалось записать {}", index_path.display()))?;

    // Подпись считается по тем самым байтам, которые записаны, а не по тексту в
    // памяти: между ними разницы быть не должно, и единственный способ этого не
    // проверять — не иметь двух источников.
    let written = fs::read(&index_path)?;
    let signature = keys::sign_index(&written)?;
    let sig_path = dir.join("index.sig");
    fs::write(&sig_path, render_signature(&signature).as_bytes())
        .with_context(|| format!("не удалось записать {}", sig_path.display()))?;

    println!("репозиторий: {} и {}", index_path.display(), sig_path.display());
    Ok(dir.to_path_buf())
}

/// Сделать рядом репозиторий, индекс которого подписан **чужим** ключом.
///
/// Нужен стенду и только ему. Без него проверка «обновление скачалось и встало»
/// ничего не доказывает: машина, верящая любому индексу, проходит её ровно так
/// же успешно — а верить индексу означает верить тому, кто сказал, какой файл и
/// какого размера качать.
///
/// Отвергнуть его обязана **программа**, до единого скачанного байта: подпись
/// индекса проверяется первой. Тем и отличается от проверки в сценарии
/// `update`, где чужим ключом подписан контейнер и отвергает его ядро.
///
/// Образ сюда просто копируется из годного репозитория: до него дело всё равно
/// не дойдёт, а собирать второй такой же ради этого — минуты на пустом месте.
/// Существовать он обязан: отказ должен случиться из-за подписи, а не из-за
/// того, что сервер ответил «нет такого файла».
pub fn build_untrusted(good: &Path, version: &str, arch: Arch, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;

    let name = format!("freeos-{version}-{}.fpk", arch.name());
    let bytes = fs::read(good.join(&name)).with_context(|| {
        format!("годный репозиторий обязан быть собран раньше: нет {}", good.join(&name).display())
    })?;
    fs::write(dir.join(&name), &bytes)?;

    let index = render(&[Offer {
        version,
        arch: arch.name(),
        file: &name,
        size: bytes.len() as u64,
        sha256: hash(&bytes),
    }]);
    let index_path = dir.join("index");
    fs::write(&index_path, index.as_bytes())?;
    let signature = keys::sign_index_with_stranger(&fs::read(&index_path)?)?;
    fs::write(dir.join("index.sig"), render_signature(&signature).as_bytes())?;
    println!("стенд: рядом положен репозиторий с чужой подписью индекса ({})", dir.display());
    Ok(dir.to_path_buf())
}
