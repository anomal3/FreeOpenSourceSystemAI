//! `std` Rust для FreeOS — цель `<арх>-unknown-freeos` (N2 браузера на Servo, 07.11.2026).
//!
//! Обычные крейты (Servo, winit, egui, tokio…) требуют `std`. Своя `std`
//! строится по схеме Redox и Managarm: ОС объявлена семейством `unix` поверх
//! **нашей** libc (picolibc + `libc/freeos`, потоки и futex с фаз 55–57), так
//! что работает готовая ветка `std::sys::pal::unix`, а не свой `pal` с нуля.
//! picolibc — потомок newlib, поэтому цель — `env = "newlib"`, и `std` идёт
//! путями соседних newlib-систем (Horizon, Vita).
//!
//! Что лежит в `ports/rust-std`:
//! - `targets/<арх>-unknown-freeos.json` — спецификация цели (компоновка как
//!   у C++-программ набора: `freeos-cxx.ld`, `crt0.o`, `-lfreeos -lc
//!   -lunwind`);
//! - `libc/freeos.rs` — модуль крейта `libc`: типы и раскладки сняты с
//!   заголовков набора пробой `sizeof`/`offsetof` на обеих архитектурах,
//!   константы (≈950) — оттуда же;
//! - `std-os-freeos/` — модуль `std::os::freeos`;
//! - `check/` — программа `rstd`, сценарий `rust-std`.
//!
//! Правки чужих исходников (`library/` из `rust-src` тулчейна и `libc` с
//! crates.io) делаются здесь, по якорям; при смене тулчейна якорь, которого
//! нет, останавливает сборку с именем файла, а не правит не ту строку.
//!
//! Тонкость cargo: зависимости `-Zbuild-std` он считает «не своими» и по
//! правке исходника не пересобирает. Поэтому при любой смене подготовленных
//! исходников каталог сборки стирается целиком (`prepare`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::arch::Arch;
use crate::{cbuild, java, paths, util};

/// Программы на `std`, которые едут в `/bin`: `rstd` — сама `std`,
/// `rtokio` — асинхронная экосистема (tokio/mio/socket2, N3).
pub const PROGRAMS: [&str; 2] = ["rstd", "rtokio"];

/// Крейты экосистемы, которым нужна ветка под FreeOS (N3). Правка одна и та
/// же: рядом с каждым `target_os = "redox"` встаёт `"freeos"` — Redox тоже
/// минимальный unix на своей libc, и его ветки (pipe вместо eventfd, без
/// SO_* сверх POSIX) подходят нам почти дословно. Исключение — selector
/// `mio`: у Redox epoll, у нас его нет, поэтому `mio` собирается в
/// официальном режиме `poll()` (`--cfg mio_unsupported_force_poll_poll`).
/// Версии закреплены, хеши — `cksum` с index.crates.io (07.11.2026).
const LIKE_REDOX: &[(&str, &str, &str)] = &[
    ("mio", "1.2.4", "1788edb87fdc09c7e26304471e2f5be8cdefb1b6930d6e3985fc02ff53bf86ee"),
    ("socket2", "0.6.5", "c3d1e2c7f27f8d4cb10542a02c49005dbd6e93095799d6f3be745fae9f8fedd4"),
    ("tokio", "1.54.1", "b52fd3255258d26fac7264ab5a76b53c13367136dc011d3d6ad4f8ed070396a3"),
];

/// Версия `libc`, которой собирается `std` этого тулчейна (`library/Cargo.lock`).
const LIBC_VERSION: &str = "0.2.189";
/// SHA-256 `libc-0.2.189.crate` (index.crates.io, `cksum`, сверено 07.11.2026).
const LIBC_SHA256: &str = "3eaf3ede3fee6db1a4c2ee091bf8a8b4dccdc6d17f656fb07896ee72867612f2";

fn root() -> PathBuf {
    paths::build_dir().join("rust-std")
}

fn ports() -> PathBuf {
    paths::workspace_root().join("ports/rust-std")
}

/// Спецификация цели.
pub fn target_spec(arch: Arch) -> PathBuf {
    ports().join("targets").join(format!("{}-unknown-freeos.json", arch.name()))
}

/// Собрать программы на `std` под `arch`. Ошибка — у вызывающего строкой в
/// журнале, как у программ на C: без `rust-src` или набора система обязана
/// собираться, только без этих программ.
pub fn build_programs(arch: Arch) -> Result<Vec<(&'static str, PathBuf)>> {
    let lib = cbuild::sysroot(arch).join("lib");
    for need in ["libc.a", "libfreeos.a", "libunwind.a", "crt0.o", "freeos-cxx.ld"] {
        if !lib.join(need).is_file() {
            bail!(
                "нет {} — собрать набор: cargo xtask toolchain, cargo xtask thirdparty --only libcxx, cargo xtask sdk",
                lib.join(need).display()
            );
        }
    }
    warn_stale_libfreeos(&lib);

    let library = prepare()?;
    let target_dir = root().join("target");
    let triple = format!("{}-unknown-freeos", arch.name());

    // Библиотеки набора cargo не отслеживает: новая `libfreeos.a` (правка
    // `libc/freeos` + `cargo xtask sdk`) не перекомпоновала бы программы, чей
    // исходник не менялся. Готовые программы старше набора — стираются.
    let newest_lib = ["libfreeos.a", "libc.a", "libunwind.a", "crt0.o"]
        .iter()
        .filter_map(|name| fs::metadata(lib.join(name)).and_then(|m| m.modified()).ok())
        .max();
    let out = target_dir.join(&triple).join("release");
    for name in PROGRAMS {
        let program = out.join(name);
        let built = fs::metadata(&program).and_then(|m| m.modified()).ok();
        if let (Some(built), Some(newest)) = (built, newest_lib) {
            if newest > built {
                let _ = fs::remove_file(&program);
            }
        }
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(ports().join("check"))
        .arg("build")
        .arg("--release")
        .arg("-Zbuild-std=std,panic_abort")
        .arg("-Zjson-target-spec")
        .arg("--target")
        .arg(target_spec(arch))
        .arg("--target-dir")
        .arg(&target_dir)
        // Подмена libc и крейтов экосистемы на поправленные копии.
        .arg("--config")
        .arg(root().join("src/patches.toml"));
    // Пути — с прямой косой: RUSTFLAGS режется по пробелам, а `\` в пути
    // Windows lld понимает и так и так.
    let slash = |p: &Path| p.display().to_string().replace('\\', "/");
    let lib_s = slash(&lib);
    cmd.env(
        "RUSTFLAGS",
        format!(
            "-L {lib_s} -C link-arg=-T{lib_s}/freeos-cxx.ld -C link-arg={lib_s}/crt0.o -C strip=debuginfo \
             --cfg mio_unsupported_force_poll_poll"
        ),
    )
    .env_remove("CARGO_ENCODED_RUSTFLAGS")
    .env_remove("CARGO_BUILD_TARGET")
    .env_remove("CARGO_TARGET_DIR")
    // Свой `library/` вместо `rust-src` тулчейна.
    .env("__CARGO_TESTS_ONLY_SRC_ROOT", &library);
    util::run(&mut cmd, &format!("std Rust и программы на std ({triple})"))?;

    let out = target_dir.join(&triple).join("release");
    Ok(PROGRAMS.iter().map(|name| (*name, out.join(name))).collect())
}

/// `libfreeos.a` кладёт в набор `cargo xtask sdk`; правка `libc/freeos` без
/// него до программ на `std` не доедет (программы на C берут объектники
/// напрямую и этого не замечают).
fn warn_stale_libfreeos(lib: &Path) {
    let archive = fs::metadata(lib.join("libfreeos.a")).and_then(|m| m.modified()).ok();
    let newest = fs::read_dir(paths::workspace_root().join("libc/freeos"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "c"))
        .filter_map(|e| e.metadata().and_then(|m| m.modified()).ok())
        .max();
    if let (Some(archive), Some(newest)) = (archive, newest) {
        if newest > archive {
            say!("внимание: libc/freeos новее {} — пересобрать: cargo xtask sdk", lib.join("libfreeos.a").display());
        }
    }
}

/// Подготовить `build/rust-std/src/{library,libc}` и вернуть путь к `library`.
fn prepare() -> Result<PathBuf> {
    let src = root().join("src");
    let library = src.join("library");
    let stamp = root().join("src.stamp");
    let want = stamp_text()?;
    if fs::read_to_string(&stamp).ok().as_deref() == Some(want.as_str()) && library.is_dir() {
        return Ok(library);
    }
    say!("готовлю исходники std для freeos: {}", src.display());
    if src.exists() {
        fs::remove_dir_all(&src).with_context(|| format!("не стёрся {}", src.display()))?;
    }
    // Собранное из старых исходников cargo сам не пересоберёт — см. начало файла.
    let target = root().join("target");
    if target.exists() {
        fs::remove_dir_all(&target).with_context(|| format!("не стёрся {}", target.display()))?;
    }

    let rust_src = toolchain_sysroot()?.join("lib/rustlib/src/rust/library");
    if !rust_src.join("std").is_dir() {
        bail!("нет rust-src тулчейна: {} (rustup component add rust-src)", rust_src.display());
    }
    copy_dir(&rust_src, &library)?;
    patch_std(&library)?;

    let libc = src.join("libc");
    unpack_crate("libc", LIBC_VERSION, LIBC_SHA256, &libc)?;
    patch_libc(&libc)?;

    let slash = |p: &Path| p.display().to_string().replace('\\', "/");
    let mut patches = format!("[patch.crates-io]\nlibc = {{ path = \"{}\" }}\n", slash(&libc));
    for (name, version, sha) in LIKE_REDOX {
        let dir = src.join(format!("{name}-{version}"));
        unpack_crate(name, version, sha, &dir)?;
        let sites = next_to_redox(&dir)?;
        say!("{name} {version}: freeos рядом с redox в {sites} местах");
        if *name == "mio" {
            patch_mio(&dir)?;
        }
        patches += &format!("{name} = {{ path = \"{}\" }}\n", slash(&dir));
    }
    fs::write(src.join("patches.toml"), patches)?;

    fs::write(&stamp, want)?;
    Ok(library)
}

/// Отпечаток входа: тулчейн и все файлы `ports/rust-std` (кроме `check`).
fn stamp_text() -> Result<String> {
    let mut hasher = fpk::Hasher::new();
    let version = Command::new("rustc")
        .current_dir(paths::workspace_root())
        .arg("-vV")
        .output()
        .context("не запустился rustc")?;
    hasher.update(&version.stdout);
    let mut files = Vec::new();
    collect_files(&ports(), &mut files)?;
    files.retain(|p| !p.starts_with(ports().join("check")));
    files.sort();
    for file in files {
        hasher.update(file.strip_prefix(ports()).unwrap_or(&file).to_string_lossy().as_bytes());
        hasher.update(&fs::read(&file)?);
    }
    hasher.update(file!().as_bytes());
    hasher.update(include_bytes!("ruststd.rs"));
    Ok(hasher.finish().iter().map(|b| format!("{b:02x}")).collect())
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn toolchain_sysroot() -> Result<PathBuf> {
    let out = Command::new("rustc")
        .current_dir(paths::workspace_root())
        .args(["--print", "sysroot"])
        .output()
        .context("не запустился rustc")?;
    if !out.status.success() {
        bail!("rustc --print sysroot: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dst = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst)?;
        } else {
            fs::copy(entry.path(), &dst).with_context(|| format!("не скопировался {}", entry.path().display()))?;
        }
    }
    Ok(())
}

// ── Правки std ──────────────────────────────────────────────────────────────

const HORIZON: &str = "target_os = \"horizon\"";

/// Где рядом с Horizon (3DS, тоже newlib) ставится `freeos`: файл и номера
/// строк `rust-src` закреплённого тулчейна. Каждая строка проверяется.
const NEXT_TO_HORIZON: &[(&str, &[usize])] = &[
    // memalign: posix_memalign у нас нет.
    ("sys/alloc/unix.rs", &[80]),
    // argc/argv сохраняются при старте программы.
    ("sys/args/unix.rs", &[81]),
    // readv/writev — по одному буферу.
    ("sys/fd/unix.rs", &[151, 168, 180, 387, 404, 416]),
    // Сигналов нет: SIGPIPE, проверка дескрипторов 0–2 при старте.
    ("sys/pal/unix/mod.rs", &[83, 128, 145, 193, 204, 369, 380]),
    // Запуск процессов из std — пока нет (у нас spawn, а не fork/exec).
    ("sys/process/mod.rs", &[46, 85]),
    ("sys/process/unix/mod.rs", &[2, 16]),
    // current_exe и домашний каталог через getpwuid_r — нет.
    ("sys/paths/unix.rs", &[386, 442, 456]),
    // stat со st_atim/st_mtim, без openat/unlinkat/futimens.
    ("sys/fs/unix.rs", &[271, 572, 600, 612, 640, 985, 1560, 1627, 1948, 2024, 2314, 2330]),
];

fn patch_std(library: &Path) -> Result<()> {
    let std = library.join("std/src");
    for (rel, lines) in NEXT_TO_HORIZON {
        edit(&std.join(rel), |text| {
            let mut ls: Vec<String> = text.split('\n').map(str::to_string).collect();
            for &n in *lines {
                let line = ls.get_mut(n - 1).with_context(|| format!("{rel}:{n}: нет такой строки"))?;
                if !line.contains(HORIZON) {
                    bail!("{rel}:{n}: ожидался {HORIZON}, а там {line:?} — тулчейн сменился, сверить якоря");
                }
                *line = line.replacen(HORIZON, "target_os = \"horizon\", target_os = \"freeos\"", 1);
            }
            Ok(ls.join("\n"))
        })?;
    }

    // Известная ОС, а не `restricted_std`.
    replace(&library.join("std/build.rs"), "        || target_os == \"vita\"\n", "        || target_os == \"vita\"\n        || target_os == \"freeos\"\n")?;
    // Случайные байты — getentropy() поверх SYS_RANDOM.
    replace(&std.join("sys/random/mod.rs"), "    target_os = \"emscripten\" => {", "    any(target_os = \"emscripten\", target_os = \"freeos\") => {")?;
    // std::os::freeos.
    replace(
        &std.join("os/mod.rs"),
        "#[cfg(target_os = \"horizon\")]\npub mod horizon;",
        "#[cfg(target_os = \"horizon\")]\npub mod horizon;\n#[cfg(target_os = \"freeos\")]\npub mod freeos;",
    )?;
    replace(
        &std.join("os/unix/mod.rs"),
        "    #[cfg(target_os = \"horizon\")]\n    pub use crate::os::horizon::*;",
        "    #[cfg(target_os = \"horizon\")]\n    pub use crate::os::horizon::*;\n    #[cfg(target_os = \"freeos\")]\n    pub use crate::os::freeos::*;",
    )?;
    replace(
        &std.join("sys/env_consts.rs"),
        "#[cfg(target_os = \"horizon\")]\npub mod os {",
        "#[cfg(target_os = \"freeos\")]\npub mod os {\n    pub const FAMILY: &str = \"unix\";\n    pub const OS: &str = \"freeos\";\n    \
         pub const DLL_PREFIX: &str = \"lib\";\n    pub const DLL_SUFFIX: &str = \".so\";\n    pub const DLL_EXTENSION: &str = \"so\";\n    \
         pub const EXE_SUFFIX: &str = \"\";\n    pub const EXE_EXTENSION: &str = \"\";\n}\n\n#[cfg(target_os = \"horizon\")]\npub mod os {",
    )?;
    copy_dir(&ports().join("std-os-freeos"), &std.join("os/freeos"))?;

    // `libc` — наш, из `build/rust-std/src/libc`.
    replace(
        &library.join("Cargo.toml"),
        "windows-sys = { path = 'windows-sys' }",
        "windows-sys = { path = 'windows-sys' }\nlibc = { path = '../libc' }",
    )?;
    Ok(())
}

// ── libc ────────────────────────────────────────────────────────────────────

/// `mio` у Redox оставляет сокеты **блокирующими**: Redox нет ни в списке
/// `socket(SOCK_NONBLOCK)`, ни в списке `accept4(SOCK_NONBLOCK)`. С таким
/// сокетом рантайм tokio засыпает в первом же `read`/`accept` (N3,
/// 07.11.2026, разобрано сценарием `rust-std`: все `F_GETFL` без
/// `O_NONBLOCK`). У нас есть и `SOCK_NONBLOCK`, и `accept4` — freeos
/// ставится в оба списка рядом с Linux.
fn patch_mio(dir: &Path) -> Result<()> {
    let unix = dir.join("src/sys/unix");
    replace(
        &unix.join("net.rs"),
        "        target_os = \"cygwin\",\n    ))]\n    let socket_type = socket_type | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC;",
        "        target_os = \"cygwin\",\n        target_os = \"freeos\",\n    ))]\n    let socket_type = socket_type | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC;",
    )?;
    replace(
        &unix.join("tcp.rs"),
        "        target_os = \"cygwin\",\n        target_os = \"nuttx\",\n    ))]\n    let stream = {\n        syscall!(accept4(",
        "        target_os = \"cygwin\",\n        target_os = \"nuttx\",\n        target_os = \"freeos\",\n    ))]\n    let stream = {\n        syscall!(accept4(",
    )?;
    // …и убрать из второй ветки (BSD: accept без флагов, «режим наследуется
    // от слушающего»), куда freeos попал рядом с Redox: она стоит ниже и
    // затеняла бы `accept4`.
    replace(
        &unix.join("tcp.rs"),
        "        target_os = \"macos\",\n        target_os = \"redox\", target_os = \"freeos\",",
        "        target_os = \"macos\",\n        target_os = \"redox\",",
    )?;
    Ok(())
}

/// Поставить `target_os = "freeos"` рядом с каждым `target_os = "redox"`.
fn next_to_redox(dir: &Path) -> Result<usize> {
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    let mut sites = 0;
    for file in files.into_iter().filter(|f| f.extension().is_some_and(|x| x == "rs")) {
        let text = fs::read_to_string(&file)?;
        let n = text.matches("target_os = \"redox\"").count();
        if n > 0 {
            sites += n;
            fs::write(&file, text.replace("target_os = \"redox\"", "target_os = \"redox\", target_os = \"freeos\""))?;
        }
    }
    if sites == 0 {
        bail!("{}: ни одного target_os = \"redox\" — версия сменилась, сверить", dir.display());
    }
    Ok(sites)
}

/// Скачать крейт с crates.io (кеш — `build/toolchain/thirdparty/rust-std`),
/// сверить SHA-256 и развернуть в `dest`.
fn unpack_crate(name: &str, version: &str, sha: &str, dest: &Path) -> Result<()> {
    let cache = crate::sdk::root().join("thirdparty/rust-std");
    fs::create_dir_all(&cache)?;
    let file = cache.join(format!("{name}-{version}.crate"));
    if !file.is_file() || sha256(&file)? != sha {
        let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
        say!("> curl {url}");
        let status = Command::new("curl")
            .args(["--location", "--fail", "--silent", "--show-error", "--output"])
            .arg(&file)
            .arg(&url)
            .status()
            .context("не удалось запустить curl")?;
        if !status.success() {
            bail!("не скачался {url} ({status})");
        }
        let got = sha256(&file)?;
        if got != sha {
            bail!("хеш {name} {version} не тот: {got}, ожидался {sha}");
        }
    }
    let prefix = format!("{name}-{version}/");
    let mut err = None;
    java::stream_tar(&file, |_| true, |name, item| {
        if err.is_some() {
            return;
        }
        let Some(rel) = name.strip_prefix(&prefix) else { return };
        if let java::Item::File { data, .. } = item {
            let path = dest.join(rel);
            let res = path.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path, data));
            if let Err(e) = res {
                err = Some(anyhow::anyhow!("{}: {e}", path.display()));
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    Ok(())
}

fn patch_libc(libc: &Path) -> Result<()> {
    let unix = libc.join("src/unix");
    fs::create_dir_all(unix.join("freeos"))?;
    fs::copy(ports().join("libc/freeos.rs"), unix.join("freeos/mod.rs"))?;
    let m = unix.join("mod.rs");
    replace(
        &m,
        "cfg_if! {\n    if #[cfg(target_env = \"newlib\")] {\n        mod newlib;",
        "cfg_if! {\n    if #[cfg(target_os = \"freeos\")] {\n        mod freeos;\n        pub use self::freeos::*;\n    } \
         else if #[cfg(target_env = \"newlib\")] {\n        mod newlib;",
    )?;
    // libfreeos.a подменяет заглушки блокировок picolibc — стоит первой.
    replace(
        &m,
        "    } else if #[cfg(target_env = \"newlib\")] {\n        #[link(name = \"c\")]",
        "    } else if #[cfg(target_os = \"freeos\")] {\n        #[link(name = \"freeos\")]\n        #[link(name = \"c\")]\n        \
         #[link(name = \"m\")]\n        extern \"C\" {}\n    } else if #[cfg(target_env = \"newlib\")] {\n        #[link(name = \"c\")]",
    )?;
    // Общие для unix структуры, у которых наша раскладка другая (свои — в freeos.rs).
    for item in ["    pub struct group {", "    pub struct rusage {", "    pub struct winsize {"] {
        replace(&m, item, &format!("    #[cfg(not(target_os = \"freeos\"))]\n{item}"))?;
    }
    Ok(())
}

// ── Мелочи ──────────────────────────────────────────────────────────────────

fn edit(path: &Path, f: impl FnOnce(&str) -> Result<String>) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("не читается {}", path.display()))?;
    let new = f(&text).with_context(|| path.display().to_string())?;
    fs::write(path, new)?;
    Ok(())
}

fn replace(path: &Path, old: &str, new: &str) -> Result<()> {
    edit(path, |text| {
        if !text.contains(old) {
            bail!("нет якоря {:?} — тулчейн или libc сменились, сверить", old.lines().next().unwrap_or(old));
        }
        Ok(text.replacen(old, new, 1))
    })
}

fn sha256(path: &Path) -> Result<String> {
    let mut hasher = fpk::Hasher::new();
    hasher.update(&fs::read(path)?);
    Ok(hasher.finish().iter().map(|b| format!("{b:02x}")).collect())
}
