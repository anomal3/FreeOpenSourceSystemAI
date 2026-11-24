//! 7-Zip (24.11, Роман: «7-Zip как стандартная программа»): консольный `7zz`
//! из исходников 7-zip.org, собранный нашим набором, — `/bin/7zz` образа на
//! обеих архитектурах. Окно архиватора — своя программа `/bin/archiver`
//! (`user-progs`), она зовёт `7zz`.
//!
//! # Как собирается
//!
//! Своим `makefile.gcc` 7-Zip (`CPP/7zip/Bundles/Alone2` — «7zz»: все форматы
//! внутри, без подгружаемых модулей), тем же `make`, что и остальные чужие
//! проекты (`thirdparty`), с компилятором набора переменной. Ключи сверху —
//! только то, что `makefile.gcc` сам предлагает менять:
//!
//! * `SystemDrive=` и `SYSTEMDRIVE=` — по этим переменным окружения он
//!   решает, что собирается под Windows (MinGW): на машине сборки они есть
//!   всегда, а цель у нас — не Windows;
//! * `USE_ASM=`, `IS_X64=`, `IS_ARM64=`, `MY_ARCH=` — без ассемблера (asmc
//!   в набор не входит), C-путь кодеков;
//! * `CFLAGS_WARN_WALL=` — без `-Werror`: предупреждения clang 22, которых
//!   автор под своим компилятором не видел, сборку не останавливают;
//! * `LIB2=-lpthread` — без `-ldl`: внешних кодеков у `7zz` нет;
//! * `CXXFLAGS_EXTRA`/`CFLAGS_BASE2` — каталог `ports/7zip/include`: три
//!   заголовка, которых нет у picolibc (`sys/sysmacros.h`, `sys/sysinfo.h`
//!   через `sysconf`, `asm/hwcap.h` для AArch64) и `struct winsize` в
//!   `sys/ioctl.h`. Исходники 7-Zip не правятся.
//!
//! Чего не хватало в libc (`libc/freeos/files.c`, 24.11): `fstatat`,
//! `utimensat`/`futimens` (`ENOSYS`: ядро времён файлу не ставит —
//! распакованный файл получает время распаковки), `umask`, `getppid`.
//!
//! Архив исходников плоский (`C/`, `CPP/`, `DOC/` в корне), поэтому
//! распаковывается в свой каталог; хеш — `digest` выпуска на GitHub
//! (ip7z/7zip 26.06), сверен со скачанным 24.11.2026. Лицензия — GNU LGPL с
//! ограничением unRAR (`DOC/License.txt`, едет в образ рядом).
//!
//! Зовётся сборкой образа до программ на C ([`ensure`]): готовый `7zz` не
//! пересобирается; нет сети или набора — строка в журнале, образ без него.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::{cbuild, paths, sdk, thirdparty};

/// Версия 7-Zip.
pub const VERSION: &str = "26.06";
const URL: &str = "https://github.com/ip7z/7zip/releases/download/26.06/7z2606-src.tar.xz";
const SHA256: &str = "a4e374c8f864f51df48c2cfd0e1413ec3b2ea030deae852d6a655402ebb61ac4";
const ARCHIVE: &str = "7z2606-src.tar.xz";

/// Имя программы в `/bin` и в sysroot (`bin/7zz`).
pub const PROGRAM: &str = "7zz";

fn source_dir() -> PathBuf {
    sdk::root().join("thirdparty").join(format!("7zip-{VERSION}"))
}

fn ports_dir() -> PathBuf {
    paths::workspace_root().join("ports/7zip")
}

/// Скачать (один раз), сверить и распаковать исходники.
fn fetch() -> Result<PathBuf> {
    let root = sdk::root().join("thirdparty");
    fs::create_dir_all(&root)?;
    let source = source_dir();
    if source.join("CPP/7zip/Bundles/Alone2/makefile.gcc").is_file() {
        return Ok(source);
    }
    let file = root.join(ARCHIVE);
    if !file.is_file() {
        say!("> curl {URL}");
        let status = std::process::Command::new("curl")
            .args(["--location", "--fail", "--silent", "--show-error", "--output"])
            .arg(&file)
            .arg(URL)
            .status()
            .context("curl не запустился")?;
        if !status.success() {
            bail!("не скачался {URL} ({status})");
        }
    }
    let bytes = fs::read(&file)?;
    let mut hasher = fpk::Hasher::new();
    hasher.update(&bytes);
    let got: String = hasher.finish().iter().map(|b| format!("{b:02x}")).collect();
    if got != SHA256 {
        bail!("{}: SHA-256 {got}, ожидался {SHA256} (файл оставлен)", file.display());
    }
    fs::create_dir_all(&source)?;
    let status = std::process::Command::new("tar")
        .current_dir(&root)
        .arg("-xf")
        .arg(ARCHIVE)
        .arg("-C")
        .arg(source.file_name().unwrap())
        .status()
        .context("tar не запустился")?;
    if !status.success() || !source.join("CPP/7zip/Bundles/Alone2/makefile.gcc").is_file() {
        bail!("{} не распаковался в {}", file.display(), source.display());
    }
    Ok(source)
}

/// Самое свежее время изменения среди файлов порта: заголовки поменялись —
/// `7zz` пересобирается.
fn ports_stamp() -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![ports_dir()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(time) = entry.metadata().and_then(|meta| meta.modified()) {
                newest = Some(newest.map_or(time, |old: std::time::SystemTime| old.max(time)));
            }
        }
    }
    newest
}

/// Собрать `7zz` для `arch` (если он не собран или порт новее) и положить в
/// sysroot (`bin/7zz`) — откуда его берёт образ ([`cbuild::FOREIGN_PROGRAMS`]).
pub fn ensure(arch: Arch) -> Result<PathBuf> {
    let sysroot = cbuild::sysroot(arch);
    let target = sysroot.join("bin").join(PROGRAM);
    // Новее порта и слоя ОС libc (`libfreeos.a`: 7-Zip зовёт `open`/`chmod`
    // оттуда) — иначе пересобрать.
    let libc = fs::metadata(sysroot.join("lib/libfreeos.a")).and_then(|meta| meta.modified()).ok();
    let newest = ports_stamp().map(|ports| libc.map_or(ports, |libc| ports.max(libc)));
    let fresh = fs::metadata(&target).and_then(|meta| meta.modified()).ok().zip(newest).is_some_and(|(built, newest)| built >= newest);
    if fresh {
        return Ok(target);
    }
    build(arch)
}

/// Собрать `7zz` для `arch` всегда (`cargo xtask thirdparty --only 7zip`).
pub fn build(arch: Arch) -> Result<PathBuf> {
    let sysroot = cbuild::sysroot(arch);
    if !sysroot.join("lib/crt0.o").is_file() || !sysroot.join("lib/libc++.a").is_file() {
        bail!("нет набора с libc++ для {} (cargo xtask sdk, cargo xtask thirdparty --only libcxx)", arch.name());
    }
    let source = fetch()?;
    // Своё дерево на архитектуру: объектники `make` кладёт рядом с исходниками
    // (`b/freeos`), и две архитектуры в одном дереве смешались бы.
    let work = sdk::root().join("thirdparty").join(format!("7zip-{}", arch.name()));
    if !work.join("CPP/7zip/Bundles/Alone2/makefile.gcc").is_file() {
        copy_tree(&source, &work)?;
    }
    let triple = cbuild::c_target(arch).sdk_triple();
    let include = thirdparty::to_posix(&ports_dir().join("include"));
    let out = "b/freeos";
    // Порт поменялся — объектники старые: make о заголовках вне дерева не знает.
    let _ = fs::remove_dir_all(work.join("CPP/7zip/Bundles/Alone2").join(out));
    let shell = format!("SHELL={}", thirdparty::make_shell()?);
    let args = [
        "-f".to_string(),
        "makefile.gcc".to_string(),
        "-j8".to_string(),
        format!("CC={triple}-cc"),
        format!("CXX={triple}-c++"),
        "SystemDrive=".to_string(),
        "SYSTEMDRIVE=".to_string(),
        format!("O={out}"),
        "MY_ARCH=".to_string(),
        "USE_ASM=".to_string(),
        "IS_X64=".to_string(),
        "IS_ARM64=".to_string(),
        "CFLAGS_WARN_WALL=".to_string(),
        "LIB2=-lpthread".to_string(),
        "LFLAGS_NOEXECSTACK=".to_string(),
        format!("CXXFLAGS_EXTRA=-I{include}"),
        format!("CFLAGS_BASE2=-I{include}"),
        shell,
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    thirdparty::run_make_keeping(&work.join("CPP/7zip/Bundles/Alone2"), &args, None, &format!("make 7zz {}", arch.name()))?;
    let built = work.join("CPP/7zip/Bundles/Alone2").join(out).join(PROGRAM);
    if !built.is_file() {
        bail!("7-Zip собрался, но {} нет", built.display());
    }
    let target = sysroot.join("bin").join(PROGRAM);
    fs::create_dir_all(target.parent().unwrap())?;
    fs::copy(&built, &target).with_context(|| format!("не скопировать в {}", target.display()))?;
    say!("7-Zip {VERSION} для {}: {} ({} байт)", arch.name(), target.display(), fs::metadata(&target)?.len());
    Ok(target)
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| format!("не скопировать {}", entry.path().display()))?;
        }
    }
    Ok(())
}
