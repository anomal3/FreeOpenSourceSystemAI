//! Репозиторий обновлений: каталог, который выкладывают на сервер.
//!
//! # Что получается
//!
//! ```text
//!   <каталог>/index                    текст: что предлагается и с каким хешем
//!   <каталог>/index.sig                подпись индекса
//!   <каталог>/freeos-<версия>-<арх>.fpk  сами образы
//!   <каталог>/drivers, drivers.sig       каталог драйверов и его подпись (Д4)
//!   <каталог>/<пакет>-<версия>-<арх>.fpk пакеты-драйверы
//! ```
//!
//! Каталог драйверов лежит в том же каталоге, что индекс, а не подкаталогом:
//! у ассетов релиза на GitHub путей нет, только имена. Его можно собрать и
//! выложить отдельно от образов (`cargo xtask repo --drivers`): новый драйвер —
//! не повод предлагать всем машинам новую систему.
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
use osupdate::drivers;
use osupdate::index::build::{Offer, hash, render, render_signature};

use crate::arch::Arch;
use crate::package::Flavour;
use crate::{build, keys, package, paths};

/// Куда складывается готовый репозиторий, если не сказано иначе.
pub fn default_dir() -> PathBuf {
    paths::build_dir().join("repo")
}

/// Собрать репозиторий из уже собранных систем.
///
/// Возвращает путь к каталогу. `version` — версия, которую понесут образы: она
/// же попадает в имя файла, в `/os-release` внутри образа и в индекс.
///
/// Собранное передаётся готовым, а не собирается здесь, и это требование
/// параллельного стенда: репозиторий ему нужен посреди прогона, в потоке
/// воркера, а `build_all` в этот момент позвал бы cargo (замок на `target/`) и
/// потрогал бы образ initrd — общий файл, который в этот самый миг читает
/// соседний прогон.
pub fn build(builds: &[&build::Built], version: &str, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;

    let mut files: Vec<(String, String, u64, [u8; 32])> = Vec::new();
    for built in builds {
        let arch = built.arch;
        let release = built.release;
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
        say!("репозиторий: {} ({} МиБ)", target.display(), bytes.len() / (1024 * 1024));
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

    say!("репозиторий: {} и {}", index_path.display(), sig_path.display());
    build_drivers(builds, dir)?;
    Ok(dir.to_path_buf())
}

/// Собрать каталог драйверов: пакеты-драйверы всех собранных архитектур,
/// `drivers` и `drivers.sig`.
///
/// Запись каталога составляется из **манифеста** пакета, а не из второго
/// списка рядом: имя, версия и устройства берутся оттуда же, откуда их потом
/// прочитает `pkg` на машине. Разойдись каталог с пакетом, `drvd` скачал бы
/// драйвер, который после установки не назвал бы устройства.
pub fn build_drivers(builds: &[&build::Built], dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;

    struct Record {
        drives: String,
        arch: &'static str,
        package: String,
        version: String,
        file: String,
        size: u64,
        sha256: [u8; 32],
    }
    let mut records: Vec<Record> = Vec::new();
    for built in builds {
        let arch = built.arch.name();
        for bytes in package::drivers(built.arch, built.release)? {
            let header = fpk::Header::parse(&bytes)
                .map_err(|err| anyhow::anyhow!("пакет-драйвер не разбирается: {err:?}"))?;
            let start = header.manifest_offset() as usize;
            let manifest_bytes = &bytes[start..start + header.manifest_len as usize];
            let manifest = fpk::Manifest::parse(&header, manifest_bytes)
                .map_err(|err| anyhow::anyhow!("манифест пакета-драйвера не разбирается: {err:?}"))?;
            let name = manifest.name().map_err(|err| anyhow::anyhow!("{err:?}"))?;
            let version = manifest.version().map_err(|err| anyhow::anyhow!("{err:?}"))?;
            let Some(drives) = manifest.field("drives") else {
                anyhow::bail!("пакет {name} попал в каталог драйверов, но не называет устройств (drives=)");
            };
            if manifest.drives().any(|drive| drive.is_none()) {
                anyhow::bail!("пакет {name}: поле drives= не разбирается как vendor:device");
            }
            let file = format!("{name}-{version}-{arch}.fpk");
            let target = dir.join(&file);
            fs::write(&target, &bytes)
                .with_context(|| format!("не удалось записать {}", target.display()))?;
            say!("каталог драйверов: {} ({} байт)", target.display(), bytes.len());
            records.push(Record {
                drives: drives.to_string(),
                arch,
                package: name.to_string(),
                version: version.to_string(),
                file,
                size: bytes.len() as u64,
                sha256: hash(&bytes),
            });
        }
    }

    let offers: Vec<drivers::build::Offer<'_>> = records
        .iter()
        .map(|record| drivers::build::Offer {
            drives: &record.drives,
            arch: record.arch,
            package: &record.package,
            version: &record.version,
            file: &record.file,
            size: record.size,
            sha256: record.sha256,
        })
        .collect();
    let path = dir.join(drivers::FILE);
    fs::write(&path, drivers::build::render(&offers).as_bytes())
        .with_context(|| format!("не удалось записать {}", path.display()))?;
    // Подпись — по записанным байтам, как у индекса.
    let signature = keys::sign_catalogue(&fs::read(&path)?)?;
    let sig_path = dir.join(drivers::SIGNATURE_FILE);
    fs::write(&sig_path, drivers::build::render_signature(&signature).as_bytes())
        .with_context(|| format!("не удалось записать {}", sig_path.display()))?;
    say!("каталог драйверов: {} и {}", path.display(), sig_path.display());
    Ok(())
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

    // И каталог драйверов с той же чужой подписью (Д4): отказать обязан
    // `sysupdate driver`, до того как качать пакет. Пакеты копируются из
    // годного каталога — дело до них не дойдёт, но отказ должен случиться из-за
    // подписи, а не из-за «нет такого файла».
    let catalogue = fs::read(good.join(drivers::FILE)).with_context(|| {
        format!("годный каталог драйверов обязан быть собран раньше: нет {}", good.join(drivers::FILE).display())
    })?;
    let text = String::from_utf8_lossy(&catalogue).into_owned();
    for line in text.lines() {
        if let Some(file) = line.strip_prefix("file=") {
            fs::copy(good.join(file), dir.join(file))
                .with_context(|| format!("не удалось скопировать {file} в чужой репозиторий"))?;
        }
    }
    fs::write(dir.join(drivers::FILE), &catalogue)?;
    let signature = keys::sign_catalogue_with_stranger(&catalogue)?;
    fs::write(dir.join(drivers::SIGNATURE_FILE), drivers::build::render_signature(&signature).as_bytes())?;
    say!("стенд: рядом положен репозиторий с чужой подписью индекса ({})", dir.display());
    Ok(dir.to_path_buf())
}
