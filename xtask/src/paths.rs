//! Пути внутри проекта.
//!
//! Всё считается от `CARGO_MANIFEST_DIR` самого xtask, а не от текущей рабочей
//! директории: `cargo xtask` можно запустить из любого подкаталога workspace.

use std::path::{Path, PathBuf};

use crate::arch::Arch;

/// Корень workspace: каталог на уровень выше `xtask/`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("у каталога xtask всегда есть родитель — корень workspace")
        .to_path_buf()
}

/// Каталог артефактов cargo с учётом возможного `CARGO_TARGET_DIR`.
pub fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                workspace_root().join(path)
            }
        }
        _ => workspace_root().join("target"),
    }
}

/// Каталог, куда cargo кладёт артефакты конкретной пары (триплет, профиль).
pub fn artifact_dir(triple: &str, release: bool) -> PathBuf {
    target_dir().join(triple).join(profile_dir_name(release))
}

/// Манифест крейта из `crates/`. Нужен только для диагностики: по его наличию
/// отличаем «крейт ещё не написан» от «крейт есть, но не собрался».
pub fn crate_manifest(package: &str) -> PathBuf {
    workspace_root()
        .join("crates")
        .join(package)
        .join("Cargo.toml")
}

/// Упомянут ли `crates/<package>` в корневом Cargo.toml.
///
/// Тоже чистая диагностика, поэтому хватает поиска подстроки вместо разбора
/// TOML: цена ошибки — неточная подсказка, а не неверная сборка. При любых
/// сомнениях (файл не читается) отвечаем «да», чтобы не сбивать с толку.
pub fn is_workspace_member(package: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(workspace_root().join("Cargo.toml")) else {
        return true;
    };
    text.contains(&format!("crates/{package}"))
}

/// Каталог артефактов xtask (ESP, копии прошивок, будущие образы дисков).
/// Он в .gitignore и полностью удаляется командой `clean`.
pub fn build_dir() -> PathBuf {
    workspace_root().join("build")
}

/// Каталог с результатами прогонов стенда: журналы и снимки экрана.
///
/// Внутри `build/`, то есть в .gitignore. Журнал прогона — не артефакт проекта,
/// а свидетельство одного запуска; хранить их в истории значит хранить мегабайты
/// снимков, которые перестают что-либо значить на следующем коммите.
pub fn test_dir() -> PathBuf {
    build_dir().join("test")
}

/// Каталог, который отдаётся QEMU как FAT-раздел ESP.
/// Разделён по архитектурам, чтобы `BOOTX64.EFI` от прошлой сборки не оставался
/// рядом с `BOOTAA64.EFI` и не путал ни прошивку, ни пользователя.
pub fn esp_dir(arch: Arch) -> PathBuf {
    build_dir().join("esp").join(arch.name())
}

/// Каталог для изменяемых копий прошивки (NVRAM, дополненные до 64 MiB образы).
pub fn firmware_dir(arch: Arch) -> PathBuf {
    build_dir().join("firmware").join(arch.name())
}

/// Каталог с исходным содержимым RAM-диска.
///
/// Всё, что здесь лежит, попадает в `initrd.img` рекурсивно и как есть: чтобы
/// добавить файл в демонстрацию, достаточно положить его сюда, править xtask
/// не требуется.
pub fn initrd_source_dir() -> PathBuf {
    workspace_root().join("initrd")
}

/// Готовый образ RAM-диска.
///
/// Лежит в корне `build/`, а не в `build/esp/<arch>/`: содержимое образа от
/// архитектуры не зависит, и собирать его дважды смысла нет.
pub fn initrd_image() -> PathBuf {
    build_dir().join("initrd.img")
}

/// Каталог эталонных настроек — единственный экземпляр в дереве.
///
/// Лежит внутри `initrd/`, потому что в живую систему эти файлы попадают вместе
/// с ней; в установленную их переносит установщик, в обновлённую — контейнер.
/// Три дороги, один комплект файлов: копия текста в исходниках установщика (так
/// было до фазы 39) означала бы, что список служб правится в двух местах, а
/// расходится молча.
pub fn defaults_dir() -> PathBuf {
    initrd_source_dir().join("usr/share/defaults/etc")
}

/// Слепок содержимого `initrd/`, по которому решается, нужна ли пересборка.
pub fn initrd_stamp() -> PathBuf {
    build_dir().join("initrd.stamp")
}

/// Имя файла образа: `FreeOS_0.1.4_aarch64_release.iso` и подобные.
///
/// Все поля обязательны, и каждое отвечает за свой способ перепутать образы.
/// Версия — потому что файл уезжает к человеку и живёт у него дольше одной
/// сборки. Номер сборки — потому что версия за вечер не меняется, а образ
/// пересобирается пятикратно (см. [`crate::version`]). Архитектура — потому что
/// `BOOTAA64.EFI` и `BOOTX64.EFI` в одинаково названных файлах не различить
/// ничем, кроме попытки загрузиться. Профиль — потому что образ содержит
/// собранные бинарники, и debug рядом с release под одним именем означал бы, что
/// запуск с `-r` и без него молча подсовывают друг другу чужое ядро.
fn image_name(slug: &str, build: u32, arch: Arch, release: bool, extension: &str) -> String {
    format!(
        "{slug}_{}.{build}_{}_{}.{extension}",
        crate::version::VERSION,
        arch.name(),
        profile_dir_name(release),
    )
}

/// Готовый загрузочный образ диска (GPT + FAT32 ESP).
pub fn disk_image(slug: &str, build: u32, arch: Arch, release: bool) -> PathBuf {
    build_dir().join(image_name(slug, build, arch, release, "img"))
}

/// Каталог с загрузочными ISO — и только с ними.
///
/// Отдельный каталог потому, что ISO — единственное, что уезжает с этой машины
/// к человеку. Всё остальное в `build/` (образы дисков, целевой диск, прошивки,
/// журналы стенда, слепки) существует ради сборки и проверки, и искать среди
/// этого один нужный файл — лишняя возможность взять соседний.
pub fn iso_dir() -> PathBuf {
    build_dir().join("ISO")
}

/// Загрузочный образ ISO — то, что отдают человеку.
pub fn iso_image(slug: &str, build: u32, arch: Arch, release: bool) -> PathBuf {
    iso_dir().join(image_name(slug, build, arch, release, "iso"))
}

/// Слепок содержимого образа диска, по которому решается, нужна ли пересборка.
///
/// Номера сборки в имени слепка нет намеренно: слепок обязан пережить смену
/// номера, иначе сравнивать будет не с чем и каждая сборка окажется первой.
/// Версии тоже нет — по той же причине.
pub fn disk_image_stamp(slug: &str, arch: Arch, release: bool) -> PathBuf {
    build_dir().join(stamp_name(slug, arch, release, "img"))
}

/// Слепок содержимого ISO. Лежит рядом со своим образом, в [`iso_dir`].
pub fn iso_stamp(slug: &str, arch: Arch, release: bool) -> PathBuf {
    iso_dir().join(stamp_name(slug, arch, release, "iso"))
}

fn stamp_name(slug: &str, arch: Arch, release: bool, extension: &str) -> String {
    format!(
        "{slug}_{}_{}.{extension}.stamp",
        arch.name(),
        profile_dir_name(release),
    )
}

/// Чистый диск, на который ставит установщик.
///
/// В слепок не входит и не пересобирается: это носитель, а не артефакт
/// сборки. Стереть его — отдельное осознанное действие (`--fresh`), потому что
/// именно на нём остаётся результат прошлой установки.
pub fn target_disk(arch: Arch) -> PathBuf {
    build_dir().join(format!("target-{}.img", arch.name()))
}

pub fn profile_dir_name(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}
