//! NetSurf — лёгкий браузер (начато 03.11): свои HTML, CSS и картинки без
//! JavaScript, окно — поверхность libnsfb на окне стола, сеть — libcurl с
//! mbedTLS из набора. Цель — браузер, который влезает в десять мегабайт на
//! телефоне, где Chromium не поместится никогда.
//!
//! # Как собирается
//!
//! Архив `netsurf-all` — браузер и все его библиотеки одним деревом, и каждая
//! собирается **своей** сборочной системой (`buildsystem/makefiles`), так, как
//! NetSurf велит кросс-собирать себя: `HOST=<триплет>`, компилятор переменной,
//! установка в префикс. Нашего здесь — три вещи, все в `ports/netsurf`:
//!
//! * поверхность libnsfb на окне стола (`libnsfb/freeos.c`) и патч, который
//!   вписывает её в список поверхностей (`libnsfb/freeos.patch`);
//! * патч самого NetSurf (`freeos.patch`): у нас нет `scandir` и `fstatat`, а
//!   у NetSurf на этот случай есть свои пути — патч их включает; и компоновка
//!   через файл ответов (почему — ниже);
//! * заглушка POSIX для его инструментов сборки (`host/`).
//!
//! # Что здесь не как у всех
//!
//! * **Инструменты сборки.** NetSurf строит на машине сборки три программы:
//!   шрифт и значки он превращает в C, сообщения — в сжатый файл. Им нужны zlib
//!   и libpng **машины сборки** — они собираются здесь clang-ом под Windows из
//!   тех же исходников, что едут в набор (проекты `zlib` и `libpng`), а
//!   `<unistd.h>` с `getopt` даёт `ports/netsurf/host`.
//! * **Длинная команда компоновки.** `make` 4.4 под Windows обрезает рецепт,
//!   который идёт через оболочку, на восьми килобайтах, и компоновщик получает
//!   половину имени объектника. Команды без оболочки (`ar` libcss — 31 КБ)
//!   проходят целиком; у NetSurf же компоновка с перенаправлением вывода.
//!   Список объектников уходит в файл функцией `make` `$(file ...)`, а обёртка
//!   набора файлы ответов раскрывает.
//! * **Ссылки в архиве.** Ресурсы фронтендов — символические ссылки на
//!   `resources/`, и распакованные на Windows они то файлы, то ссылки, то
//!   битые. Дерево копируется с переходом по ссылкам, а ресурсы в установку
//!   берутся прямо из `resources/`.
//! * **`toolchain=clang`.** Свой компилятор buildsystem узнаёт по первому слову
//!   `--version`, а наша обёртка отвечает «x86_64-freeos (FossAi SDK …)».
//! * **Без `iconv`.** libparserutils собирается с `WITHOUT_ICONV_FILTER`: у
//!   picolibc `iconv_open` отказывает всему.
//! * **Без libnslog.** Его разбор фильтров собирается flex и bison, которых на
//!   машине сборки нет; NetSurf без него пишет журнал в stderr сам.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::{cbuild, thirdparty};

/// Библиотеки NetSurf в порядке зависимостей. libnsfb — последней: она
/// единственная с нашей правкой.
const LIBRARIES: [&str; 11] = [
    "libwapcaplet",
    "libparserutils",
    "libcss",
    "libhubbub",
    "libdom",
    "libnsbmp",
    "libnsgif",
    "libnsutils",
    "libutf8proc",
    "libnspsl",
    "libnsfb",
];

/// Ресурсы браузера: путь в дереве `netsurf` и имя в `share/netsurf` — тот же
/// список, что ставит его `install-framebuffer`, плюс лицензия.
const RESOURCES: [(&str, &str); 11] = [
    ("resources/adblock.css", "adblock.css"),
    ("resources/default.css", "default.css"),
    ("resources/internal.css", "internal.css"),
    ("resources/quirks.css", "quirks.css"),
    ("resources/netsurf.png", "netsurf.png"),
    ("resources/favicon.png", "favicon.png"),
    ("resources/en/credits.html", "credits.html"),
    ("resources/en/licence.html", "licence.html"),
    ("resources/en/welcome.html", "welcome.html"),
    // Сообщения интерфейса порождает сборка (`split-messages`, сжатые gzip).
    ("frontends/framebuffer/res/en/Messages", "Messages"),
    ("COPYING", "COPYING"),
];

/// Где в системе лежит браузер: пакет ставится в `/opt/netsurf`, и путь
/// ресурсов, вшитый в программу (`NETSURF_FB_RESPATH`), смотрит в
/// `/opt/netsurf/share/netsurf`.
const PREFIX: &str = "/opt/netsurf";

/// Собрать библиотеки и браузер для одной архитектуры: библиотеки — в sysroot,
/// браузер — `bin/netsurf`, ресурсы — `share/netsurf` там же.
pub fn build(source: &Path, work: &Path, arch: Arch) -> Result<()> {
    let sysroot = cbuild::sysroot(arch);
    let prefix = thirdparty::to_posix(&sysroot);
    let triple = cbuild::c_target(arch).sdk_triple();
    let ports = crate::paths::workspace_root().join("ports/netsurf");

    copy_following(source, work)?;
    let host = host_libraries(work)?;

    let nsfb = work.join("libnsfb");
    fs::copy(ports.join("libnsfb/freeos.c"), nsfb.join("src/surface/freeos.c"))?;
    thirdparty::run_shell(
        &nsfb,
        &format!("patch -p1 -i {}", thirdparty::to_posix(&ports.join("libnsfb/freeos.patch"))),
        &format!("patch libnsfb {}", arch.name()),
    )?;
    thirdparty::run_shell(
        &work.join("netsurf"),
        &format!("patch -p1 -i {}", thirdparty::to_posix(&ports.join("freeos.patch"))),
        &format!("patch netsurf {}", arch.name()),
    )?;

    let shell = format!("SHELL={}", thirdparty::make_shell()?);
    let host_vars = [
        format!("HOST={triple}"),
        format!("CC={triple}-cc"),
        format!("AR={triple}-ar"),
    ];
    let buildsystem = format!("NSSHARED={}", thirdparty::to_posix(&work.join("buildsystem")));
    let install_prefix = format!("PREFIX={prefix}");
    let pkgconfig = format!("PKGCONFIG={triple}-pkg-config");
    for library in LIBRARIES {
        let mut args: Vec<&str> = vec!["install", &shell];
        args.extend(host_vars.iter().map(String::as_str));
        args.extend([
            "BUILD_CC=clang",
            "toolchain=clang",
            &install_prefix,
            &buildsystem,
            &pkgconfig,
            "WARNFLAGS=-Wall -W -Wno-error",
            "INSTALL=install",
            "DESTDIR=",
            "Q=",
        ]);
        // libparserutils перекодирует вход через `iconv`, а `iconv_open`
        // picolibc не знает ни одной кодировки, даже UTF-8 в UTF-8: парсер не
        // создаётся, и любая страница — «An unexpected error occurred». Без
        // `iconv` она берёт свои кодеки (UTF-8, UTF-16, 8859-*, Windows-125x)
        // — их браузеру хватает. `CFLAGS` окружения её `Makefile` дописывает к
        // своим.
        let env: &[(&str, &str)] =
            if library == "libparserutils" { &[("CFLAGS", "-DWITHOUT_ICONV_FILTER")] } else { &[] };
        thirdparty::run_make_with(&work.join(library), &args, None, env, &format!("make {library} {}", arch.name()))?;
    }

    let host_cc = format!(
        "BUILD_CC=clang -D_CRT_SECURE_NO_WARNINGS -D_CRT_NONSTDC_NO_DEPRECATE -I{} -I{}",
        thirdparty::to_posix(&ports.join("host")),
        thirdparty::to_posix(&host.join("include"))
    );
    let host_ld = format!("BUILD_LDFLAGS=-L{}", thirdparty::to_posix(&host.join("lib")));
    let pkg_config = format!("PKG_CONFIG={triple}-pkg-config");
    let netsurf_prefix = format!("PREFIX={PREFIX}");
    let mut args: Vec<&str> = vec!["-j6", &shell, "TARGET=framebuffer"];
    args.extend(host_vars.iter().map(String::as_str));
    args.extend([
        &pkg_config,
        &host_cc,
        &host_ld,
        "BUILD_LIBPNG_LDFLAGS=-lpng -lz",
        &netsurf_prefix,
        // Без JavaScript — так и задумано: Duktape и его привязки (nsgenbind)
        // не собираются вовсе. Без OpenSSL: TLS у libcurl свой (mbedTLS), а
        // OpenSSL NetSurf нужен только для показа цепочки сертификатов.
        "NETSURF_USE_DUKTAPE=NO",
        "NETSURF_USE_OPENSSL=NO",
        "NETSURF_USE_NSLOG=NO",
        // Чего в наборе нет: SVG (libsvgtiny тянет свой gperf-разбор), спрайты
        // RISC OS, JPEG, WebP, JPEG XL, PDF.
        "NETSURF_USE_NSSVG=NO",
        "NETSURF_USE_ROSPRITE=NO",
        "NETSURF_USE_JPEG=NO",
        "NETSURF_USE_WEBP=NO",
        "NETSURF_USE_JPEGXL=NO",
        "NETSURF_USE_HARU_PDF=NO",
        "NETSURF_USE_VIDEO=NO",
        // Шрифт — растровый, вшитый в программу; FreeType — потом.
        "NETSURF_FB_FONTLIB=internal",
        "Q=",
    ]);
    // Пути цели (`/opt/netsurf/...` в `-DNETSURF_FB_RESPATH=`) sh из Git,
    // запуская компилятор, переписал бы в пути своей установки; все наши пути —
    // уже в написании Windows, переписывать нечего.
    thirdparty::run_make_keeping(&work.join("netsurf"), &args, Some("*"), &format!("make netsurf {}", arch.name()))?;

    let bin = sysroot.join("bin");
    fs::create_dir_all(&bin)?;
    fs::copy(work.join("netsurf/nsfb"), bin.join("netsurf")).context("NetSurf собрался, но программы nsfb нет")?;
    let share = sysroot.join("share/netsurf");
    let _ = fs::remove_dir_all(&share);
    fs::create_dir_all(&share)?;
    for (from, to) in RESOURCES {
        fs::copy(work.join("netsurf").join(from), share.join(to))
            .with_context(|| format!("нет ресурса NetSurf {from}"))?;
    }
    Ok(())
}

/// Файлы ресурсов в sysroot: имя в `share/netsurf` — для пакета.
pub fn resources() -> impl Iterator<Item = &'static str> {
    RESOURCES.iter().map(|(_, name)| *name)
}

/// zlib и libpng машины сборки — для `split-messages` и `convert_image`.
///
/// Списки файлов — библиотеки целиком без примеров и тестов; настройки libpng —
/// готовые из её `scripts/pnglibconf.h.prebuilt`, как она и советует сборке без
/// своего `configure`. Имена архивов — `z.lib` и `png.lib`: clang под MSVC
/// ищет `-lz` именно так.
fn host_libraries(work: &Path) -> Result<PathBuf> {
    let zlib = thirdparty::source_dir("zlib")?;
    let png = thirdparty::source_dir("libpng")?;
    let host = work.join("host");
    let objects = host.join("obj");
    let include = host.join("include");
    let lib = host.join("lib");
    for dir in [&objects, &include, &lib] {
        fs::create_dir_all(dir)?;
    }
    for name in ["zlib.h", "zconf.h"] {
        fs::copy(zlib.join(name), include.join(name))?;
    }
    for name in ["png.h", "pngconf.h"] {
        fs::copy(png.join(name), include.join(name))?;
    }
    fs::copy(png.join("scripts/pnglibconf.h.prebuilt"), include.join("pnglibconf.h"))?;

    let clang = cbuild::llvm_tool("clang")?;
    let ar = cbuild::llvm_tool("llvm-ar")?;
    let parts: [(&str, &Path, &[&str]); 2] = [
        (
            "z",
            &zlib,
            &[
                "adler32", "compress", "crc32", "deflate", "gzclose", "gzlib", "gzread", "gzwrite", "infback",
                "inffast", "inflate", "inftrees", "trees", "uncompr", "zutil",
            ],
        ),
        (
            "png",
            &png,
            &[
                "png", "pngerror", "pngget", "pngmem", "pngpread", "pngread", "pngrio", "pngrtran", "pngrutil",
                "pngset", "pngtrans", "pngwio", "pngwrite", "pngwtran", "pngwutil",
            ],
        ),
    ];
    for (name, dir, files) in parts {
        let mut built = Vec::new();
        for file in files {
            let object = objects.join(format!("{name}_{file}.o"));
            let mut cmd = Command::new(&clang);
            cmd.args(["-O2", "-w", "-D_CRT_SECURE_NO_WARNINGS", "-D_CRT_NONSTDC_NO_DEPRECATE"])
                .arg(format!("-I{}", include.display()))
                .arg("-c")
                .arg(dir.join(format!("{file}.c")))
                .arg("-o")
                .arg(&object);
            let status = cmd.status().with_context(|| format!("не удалось запустить {}", clang.display()))?;
            if !status.success() {
                bail!("clang машины сборки не собрал {file}.c ({status})");
            }
            built.push(object);
        }
        let status = Command::new(&ar)
            .arg("rcs")
            .arg(lib.join(format!("{name}.lib")))
            .args(&built)
            .status()
            .with_context(|| format!("не удалось запустить {}", ar.display()))?;
        if !status.success() {
            bail!("llvm-ar не собрал {name}.lib ({status})");
        }
    }
    Ok(host)
}

/// Скопировать дерево, переходя по символическим ссылкам; битые пропускаются
/// (ссылка `res/Messages` указывает на файл, который порождает сборка).
///
/// Ссылку на каталог (`framebuffer/res/throbber -> ../../gtk/res/throbber`)
/// tar под Windows оставляет с прямыми косыми внутри, и Windows по ней не
/// переходит. Такая ссылка разрешается здесь: её текст приклеивается к
/// каталогу, где она лежит, а уж такой путь Windows понимает.
fn copy_following(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let path = entry?.path();
        let Some(name) = path.file_name() else { continue };
        let target = to.join(name);
        let real = match fs::metadata(&path) {
            Ok(_) => path.clone(),
            Err(_) => match (fs::read_link(&path), path.parent()) {
                (Ok(link), Some(parent)) => parent.join(link),
                _ => continue,
            },
        };
        match fs::metadata(&real) {
            Ok(meta) if meta.is_dir() => copy_following(&real, &target)?,
            Ok(meta) if meta.is_file() => {
                fs::copy(&real, &target)
                    .with_context(|| format!("не скопировать {} -> {}", real.display(), target.display()))?;
            }
            _ => {}
        }
    }
    Ok(())
}
