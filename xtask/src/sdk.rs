//! Набор для сборки: то, чем чужой проект собирается под FreeOS.
//!
//! # Чем набор отличается от того, что было в фазе 45
//!
//! В фазе 45 сборка C существовала как **наша команда**: `cargo xtask cbuild`
//! знала, где лежит libc, какие флаги нужны и в каком порядке звать компоновщик.
//! Этого достаточно ровно для наших программ и ни для чего больше: чужой
//! `./configure` не станет читать наш `xtask`.
//!
//! Набор переносит то же знание в место, куда чужой проект смотрит сам, — в
//! программу по имени `x86_64-freeos-cc`. Отсюда всё остальное:
//!
//! * **sysroot самодостаточен.** До сих пор заголовки брались из двух мест: из
//!   `build/toolchain/sysroot/<арх>/include` (picolibc) и из `libc/freeos` в
//!   репозитории. Второе — наше дерево, и чужому проекту его знать неоткуда.
//!   Теперь всё, что нужно для сборки, лежит под sysroot: заголовки, `libc.a`,
//!   `libfreeos.a`, `crt0.o`, компоновочный сценарий.
//! * **Обёртки лежат в `bin` рядом.** Обёртка ищет sysroot **относительно
//!   себя** — поэтому набор можно перенести целиком, и он не сломается.
//! * **Результат кладётся в `.fpk`.** Тем же контейнером, что и всё остальное
//!   (фаза 31): набор — это такой же продукт сборки, как система, и раздаётся
//!   он так же.
//!
//! # Чего набор не делает и делать не будет
//!
//! Компилятор в него не входит. clang — чужая программа весом в гигабайт, она
//! ставится один раз своим установщиком, и складывать её копию в наш пакет
//! значило бы раздавать чужой бинарник под своим именем. Набор — это цель
//! сборки: заголовки, библиотеки, правила и обёртка, которая всё это знает.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::{cbuild, paths};

/// Корень набора: `bin` с обёртками и `sysroot` с целями.
pub fn root() -> PathBuf {
    paths::workspace_root().join("build/toolchain")
}

/// Каталог с обёртками.
pub fn bin_dir() -> PathBuf {
    root().join("bin")
}

/// Инструменты, которые набор выставляет наружу.
///
/// Список не произволен: это ровно те имена, которые чужой `configure`
/// подставляет к триплету, когда ему сказали `--host=x86_64-freeos`. Не найдя
/// `x86_64-freeos-ar`, autoconf молча берёт хостовый `ar` — и собирает архив
/// формата хоста, который наш компоновщик не прочтёт.
pub const TOOLS: [&str; 9] = [
    "cc", "gcc", "ar", "ranlib", "nm", "strip", "objcopy", "objdump", "ld",
];

/// Полный путь к обёртке набора для этой цели.
pub fn tool_path(arch: Arch, tool: &str) -> PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    bin_dir().join(format!(
        "{}-freeos-{tool}{suffix}",
        cbuild::c_target(arch).sdk_triple().trim_end_matches("-freeos")
    ))
}

/// Собрать и установить набор целиком.
///
/// Порядок шагов существенен только в одном месте: обёртка обязана появиться
/// **после** sysroot, иначе первая же попытка ею собрать упрётся в отсутствие
/// заголовков и объяснит это как поломку набора.
pub fn install() -> Result<()> {
    // picolibc и вспомогательные подпрограммы компилятора — основание, на
    // котором всё стоит. Если их нет, `toolchain` скажет об этом внятно и
    // назовёт команду.
    cbuild::toolchain()?;

    for arch in Arch::ALL {
        populate_sysroot(arch)?;
    }
    build_wrappers()?;
    say!("набор установлен: {}", root().display());
    say!("проверить: {} --version", tool_path(Arch::X86_64, "cc").display());
    Ok(())
}

/// Доложить в sysroot всё, чего picolibc туда не кладёт.
///
/// Копии, а не ссылки: sysroot обязан пережить перенос на другую машину, а
/// ссылка на `libc/freeos/freeos-syscall.h` в нашем дереве верна ровно до
/// первого `git clone` в другой каталог.
fn populate_sysroot(arch: Arch) -> Result<()> {
    let sysroot = cbuild::sysroot(arch);
    let include = sysroot.join("include");
    let lib = sysroot.join("lib");
    if !include.join("stdio.h").is_file() {
        bail!(
            "picolibc не установлена в {}\nСобрать: cargo xtask toolchain",
            sysroot.display()
        );
    }
    fs::create_dir_all(&lib)?;

    // Заголовок с номерами вызовов. Он нужен не для того, чтобы чужой проект им
    // пользовался, — почти никто не будет, — а для того, чтобы им пользовался
    // **наш слой ОС** и всякий, кому понадобится позвать систему напрямую.
    copy(&cbuild::syscall_header(), &include.join("freeos-syscall.h"))?;

    // Компоновочный сценарий. Тот же, что у программ на Rust, и это не
    // экономия: раскладка задана требованиями ядра — адрес 512 ГиБ, страницы,
    // разведённые по правам, — и второй сценарий разъехался бы с первым молча.
    copy(
        &paths::workspace_root().join("crates/user-progs/user.ld"),
        &lib.join("freeos.ld"),
    )?;

    // Вспомогательные подпрограммы компилятора: деление `__int128`, программная
    // плавающая точка, `memcpy` там, где компилятор решил позвать его сам.
    copy(&cbuild::builtins(arch), &lib.join("libclang_rt.builtins.a"))?;

    // Стартовый код и слой ОС. Собираются здесь, а не берутся из `cbuild`,
    // потому что попадают они в **разные** места: `crt0.o` компоновщик обязан
    // получить отдельным файлом и первым, а `syscalls.o` живёт в архиве, откуда
    // берётся только то, что понадобилось.
    let work = root().join("sdk").join(arch.name());
    fs::create_dir_all(&work)?;
    let includes = vec![include.clone(), cbuild::libc_dir().join("freeos")];

    let crt0 = work.join("crt0.o");
    cbuild::compile(
        arch,
        &cbuild::libc_dir().join("freeos/crt0.c"),
        &crt0,
        &includes,
    )?;
    copy(&crt0, &lib.join("crt0.o"))?;

    let syscalls = work.join("syscalls.o");
    cbuild::compile(
        arch,
        &cbuild::libc_dir().join("freeos/syscalls.c"),
        &syscalls,
        &includes,
    )?;
    let archive = lib.join("libfreeos.a");
    // Архив пересоздаётся, а не дополняется: `llvm-ar r` в существующий файл
    // оставил бы там объектник от прошлой архитектуры, если каталог когда-то
    // был переиспользован. Ошибка выглядела бы как «неверный формат файла» на
    // компоновке.
    let _ = fs::remove_file(&archive);
    let ar = cbuild::llvm_tool("llvm-ar")?;
    let status = Command::new(&ar)
        .arg("rcs")
        .arg(&archive)
        .arg(&syscalls)
        .status()
        .with_context(|| format!("не удалось запустить {}", ar.display()))?;
    if !status.success() {
        bail!("llvm-ar не собрал {} ({status})", archive.display());
    }

    say!("sysroot {}: {}", arch.name(), sysroot.display());
    Ok(())
}

/// Собрать обёртку и разложить её под всеми именами, которых ждёт `configure`.
///
/// Копии, а не символические ссылки, и это осознанно: на Windows ссылка требует
/// прав администратора или режима разработчика, и набор, который у половины
/// машин «не ставится», хуже набора, который весит на пару мегабайт больше.
/// Обёртка узнаёт себя по имени файла, поэтому копии ведут себя по-разному.
fn build_wrappers() -> Result<()> {
    let status = Command::new(cargo())
        .current_dir(paths::workspace_root())
        .args(["build", "--release", "-p", "freeos-cc", "--bin", "freeos-cc"])
        .status()
        .context("не удалось запустить cargo для сборки обёртки")?;
    if !status.success() {
        bail!("обёртка компилятора не собралась ({status})");
    }
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let built = paths::target_dir()
        .join("release")
        .join(format!("freeos-cc{suffix}"));
    if !built.is_file() {
        bail!("cargo собрал обёртку, но её нет по пути {}", built.display());
    }

    let bin = bin_dir();
    fs::create_dir_all(&bin)?;
    let mut count = 0;
    for arch in Arch::ALL {
        for tool in TOOLS {
            copy(&built, &tool_path(arch, tool))?;
            count += 1;
        }
    }
    say!("обёртки: {count} шт. в {}", bin.display());
    Ok(())
}

/// Чем звать cargo. Переменная — потому что нас самих запустил он же.
fn cargo() -> PathBuf {
    std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(from, to)
        .with_context(|| format!("не удалось скопировать {} -> {}", from.display(), to.display()))?;
    Ok(())
}

/// Собрать пакет с набором для одной цели.
///
/// # Что в него входит и почему именно это
///
/// Заголовки и библиотеки — всё, чем собирается программа, и ничего сверх.
/// Компилятора нет ([см. заголовок модуля](self)); обёртки тоже нет, и это
/// отдельное решение: она — программа **хоста**, собранная под Windows или
/// Linux, а пакет ставится в FreeOS. Класть в пакет для целевой системы
/// исполняемый файл чужой платформы — самый дешёвый способ получить пакет,
/// который ставится и не работает.
///
/// Пакет отвечает на другой вопрос: «чем собирать программы **для** этой
/// системы, если набор надо перенести». Распакованный, он даёт готовый sysroot.
pub fn build_package(arch: Arch) -> Result<crate::package::Package> {
    let sysroot = cbuild::sysroot(arch);
    if !sysroot.join("lib/crt0.o").is_file() {
        bail!(
            "набор не установлен для {}\nСобрать: cargo xtask sdk",
            arch.name()
        );
    }

    let name = format!("freeos-sdk-{}", arch.name());
    let version = crate::version::VERSION;
    let mut builder = Builder::new(Kind::Package, &name, version);
    builder.field(
        "summary",
        "Headers, libraries and link script for building C programs for FreeOS",
    );

    let mut files = 0usize;
    let mut bytes = 0usize;
    for (dir, prefix, mode) in [
        (sysroot.join("include"), "usr/include", 0o644),
        (sysroot.join("lib"), "usr/lib", 0o644),
    ] {
        for (relative, data) in collect(&dir)? {
            bytes += data.len();
            files += 1;
            builder.file(&Entry {
                path: format!("{prefix}/{relative}"),
                mode,
                uid: 0,
                gid: 0,
                data,
            });
        }
    }

    let file_name = format!("{name}-{version}.fpk");
    let dir = paths::work_dir().join("pkg");
    fs::create_dir_all(&dir)?;
    let path = dir.join(&file_name);
    let packed = builder.finish();
    fs::write(&path, &packed)?;
    say!(
        "пакет набора: {} ({files} файлов, {bytes} байт исходно, {} в контейнере)",
        path.display(),
        packed.len()
    );
    Ok(crate::package::Package { file_name, path })
}

/// Прочитать дерево целиком: относительный путь и содержимое.
///
/// Пути внутри пакета всегда через `/`, независимо от того, на чём его собрали:
/// разделитель Windows внутри контейнера превратился бы в файл с обратной
/// косой в имени, и заметили бы это только в целевой системе.
fn collect(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((current, prefix)) = stack.pop() {
        let entries = fs::read_dir(&current)
            .with_context(|| format!("не прочитать каталог {}", current.display()))?;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push((entry.path(), relative));
            } else if kind.is_file() {
                out.push((relative, fs::read(entry.path())?));
            }
        }
    }
    // Порядок обхода каталога — дело файловой системы, а пакет обязан
    // собираться побайтно одинаково: иначе два прогона дают разные контрольные
    // суммы, и обновление видит изменение там, где его не было.
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Имя обёртки собирается из триплета набора, а не пишется руками.
    #[test]
    fn wrapper_names_follow_the_triple() {
        let path = tool_path(Arch::X86_64, "cc");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("x86_64-freeos-cc"),
            "обёртку назвали {name}, а `configure` ищет x86_64-freeos-cc"
        );
        let arm = tool_path(Arch::Aarch64, "ar");
        assert!(
            arm.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("aarch64-freeos-ar")
        );
    }

    /// Обёртка выставляется под всеми именами, которые autoconf ищет по
    /// триплету. Пропущенное имя означает молча взятый инструмент хоста.
    #[test]
    fn autoconf_tool_names_are_all_present() {
        for expected in ["cc", "ar", "ranlib", "nm", "strip", "ld"] {
            assert!(
                TOOLS.contains(&expected),
                "`{expected}` не выставлен: autoconf возьмёт хостовый"
            );
        }
    }
}
