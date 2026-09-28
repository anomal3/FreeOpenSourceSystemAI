//! Чужой проект, собранный нашим набором. Это и есть проверка фазы 46.
//!
//! # Почему проверка именно такая
//!
//! Набор, проверенный своими же программами, не проверен ничем: мы писали и
//! программы, и набор, и подгоняли одно под другое, не замечая этого. Настоящий
//! вопрос — «соберётся ли то, что писали не мы и не про нас», — и ответ на него
//! даёт только чужой исходник, взятый **без единой правки**.
//!
//! Отсюда все решения ниже:
//!
//! * **Исходники приносятся с их сервера и проверяются по хешу**, а не лежат в
//!   нашем дереве. Копия в репозитории — это копия, которую однажды кто-нибудь
//!   поправит «на одну строчку», и проверка перестанет что-либо значить.
//! * **`CC` не задаётся.** Задаётся `CHOST=x86_64-freeos` и путь к каталогу
//!   обёрток — дальше zlib находит `x86_64-freeos-gcc`, `-ar`, `-ranlib` сам, по
//!   своему же соглашению. Это то же самое, что делает любой настоящий
//!   кросс-набор, и проверяет оно не «умеет ли наш компилятор», а «выглядим ли
//!   мы как набор, который чужой проект узнаёт».
//! * **Дерево исходников для каждой архитектуры своё, а исходное не трогается
//!   вовсе.** Так видно, что правок не было: распакованное дерево остаётся
//!   ровно таким, каким приехало.
//!
//! # Что этой проверке нужно на хосте, кроме LLVM
//!
//! `make` и `sh`. Первый ставится (`winget install --id ezwinports.make`),
//! второй приезжает с Git. Обойтись без них было бы можно — собрать зlib своим
//! списком файлов, — но это ровно та подгонка, против которой написана вся
//! проверка: чужой проект собирается **своей** сборочной системой или не
//! собирается вовсе.
//!
//! И `gperf` (фаза 61b, `winget install --id oss-winget.gperf`): им fontconfig
//! порождает при сборке таблицу имён своих свойств. Без него `configure`
//! проходит (его проверка `gperf` пустой ответ принимает), а падает `make` —
//! на `fcobjshash.h`. Вход `gperf` пишет препроцессор цели в конвейере, и его
//! ошибка теряется: `gperf` жалуется на пустой файл, а пустой файл остаётся и
//! валит все следующие `make` в том же дереве.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::{cbuild, sdk};

/// Куда складываются чужие исходники и их сборки.
fn dir() -> PathBuf {
    sdk::root().join("thirdparty")
}

/// Чем собирается чужой проект.
///
/// Вид, а не общий рецепт с флажками: у zlib есть свой `configure`, у Lua его
/// нет вовсе — и делать вид, что оба собираются одинаково, значило бы подгонять
/// их под нас. Каждый собирается **своим** способом, тем самым, что написан в
/// его собственной документации для кросс-сборки.
#[derive(Clone, Copy, PartialEq, Eq)]
enum How {
    /// `./configure` находит набор по `CHOST`, дальше `make` и `make install`.
    Configure,
    /// `make <платформа>` с указанным компилятором, дальше `make install`.
    ///
    /// Так кросс-собирают Lua: `configure` у него нет, платформа выбирается
    /// целью, а компилятор передаётся переменной — ровно это и написано в его
    /// `doc/readme.html`.
    MakePlatform(&'static str),
    /// Обычный GNU `configure`: `--host=<триплет>`, статическая сборка в
    /// sysroot и свои ключи проекта (фаза 61). Так собирается почти всё, что
    /// написано под autotools: инструменты с приставкой триплета, включая
    /// `pkg-config`, `configure` находит сам.
    Autotools(Autotools),
    /// Не сборка, а файлы: взять из архива и положить в набор одни на обе
    /// архитектуры (фаза 61, шрифт). Пары — путь в архиве и имя в
    /// [`cbuild::fonts_dir`].
    Files(&'static [(&'static str, &'static str)]),
}

/// Свои ключи проекта, собираемого GNU `configure`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Autotools {
    /// Ключи `configure` сверх общих.
    configure: &'static [&'static str],
    /// Переменные `make` — и сборке, и установке. Обычно `SUBDIRS=<библиотека>`,
    /// чтобы не собирать тесты и примеры, которым нужно то, чего в системе нет
    /// (`fork`). `{prefix}` заменяется на sysroot архитектуры.
    make: &'static [&'static str],
    /// Приставки аргументов, которые sh из Git не должен переписывать как пути
    /// (`MSYS2_ARG_CONV_EXCL`, через `;`). Пусто — переписывать как обычно.
    ///
    /// Путь цели, вшитый в библиотеку ключом `-DИМЯ="/etc/…"`, sh из Git Bash,
    /// запуская компилятор (программу Windows), молча превращает в
    /// «C:/Program Files/Git/etc/…» — путь своей установки. Собирается всё, а
    /// в системе библиотека ищет свои файлы там, где их нет (так было у Mono,
    /// фаза 60; fontconfig вшивает пути тем же способом).
    keep_paths: &'static str,
    /// Файлы установки, которые едут в образ, — одни на обе архитектуры: путь
    /// в sysroot и имя в [`cbuild::fonts_dir`]. Берутся из sysroot последней
    /// собранной архитектуры; пусто — ничего.
    shared: &'static [(&'static str, &'static str)],
}

impl Autotools {
    const fn plain(configure: &'static [&'static str], make: &'static [&'static str]) -> How {
        How::Autotools(Autotools { configure, make, keep_paths: "", shared: &[] })
    }
}

/// Чужой проект: как его зовут, откуда брать и чем проверить.
struct Project {
    name: &'static str,
    version: &'static str,
    url: &'static str,
    /// SHA-256 архива. Не удобство, а условие: исходник, приехавший по сети,
    /// проверяется до того, как его начнут собирать нашим компилятором.
    sha256: &'static str,
    /// Каталог, который появляется после распаковки.
    unpacked: &'static str,
    how: How,
    /// Что обязано появиться в наборе после установки. Пусто — не проверять.
    ///
    /// Проверка не формальность: `make install`, не нашедший чего-нибудь,
    /// охотно заканчивается успехом, и тогда «проект собран» значит «проект
    /// собран неизвестно куда».
    installs: &'static [&'static str],
}

/// Чем проверяется набор.
///
/// zlib выбран не за размер, а за устройство: свой рукописный `configure`, своя
/// проверка компилятора, `Makefile`, работа с файлами и с памятью, и никакой
/// зависимости от Linux. Ровно тот класс проекта, ради которого набор и
/// существует.
const PROJECTS: [Project; 9] = [
    Project {
        name: "zlib",
        version: "1.3.1",
        url: "https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
        sha256: "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23",
        unpacked: "zlib-1.3.1",
        how: How::Configure,
        installs: &["lib/libz.a", "include/zlib.h"],
    },
    // Lua — второй чужой проект и первый, который едет в систему **программой**,
    // а не библиотекой. Выбран он не за размер: это законченное приложение на
    // чистом C89 без единой строки под конкретную систему, со своим сборщиком
    // байткода, своей виртуальной машиной и своим сборщиком мусора. Из системных
    // услуг ему нужно ровно то, чего у нас до сих пор не было: `rename` и
    // `system` (см. `libc/freeos/syscalls.c`).
    //
    // Хеш — тот, что опубликован на lua.org рядом с архивом, а не тот, что
    // приехал: сверять скачанное с ним же значит не сверять ничего.
    Project {
        name: "lua",
        version: "5.4.9",
        url: "https://www.lua.org/ftp/lua-5.4.9.tar.gz",
        sha256: "2335b6c582a52654f94612bf10d2f4672805d05329aa6568b1d8cd9e5c6fb8e6",
        unpacked: "lua-5.4.9",
        // `generic` — сборка без единого предположения о системе: ни POSIX, ни
        // Linux, ни readline. Ровно то, чем мы являемся.
        how: How::MakePlatform("generic"),
        installs: &["bin/lua", "lib/liblua.a", "include/lua.h"],
    },
    // Фаза 61 — рисование для `System.Drawing` Mono: libgdiplus стоит на cairo,
    // cairo — на pixman (растеризация), libpng (PNG, поверх zlib) и freetype
    // (шрифты). Порядок в списке — порядок зависимостей. Хеши — опубликованные
    // рядом с архивами.
    Project {
        name: "libpng",
        version: "1.6.43",
        url: "https://download.sourceforge.net/libpng/libpng-1.6.43.tar.xz",
        sha256: "6a5ca0652392a2d7c9db2ae5b40210843c0bbc081cbd410825ab00cc59f14a6c",
        unpacked: "libpng-1.6.43",
        // Утилиты `pngfix` и соседи — программы для машины, которой у нас нет
        // в `/bin`; библиотеке они не нужны.
        how: Autotools::plain(&["--disable-tools"], &[]),
        installs: &["lib/libpng16.a", "include/png.h", "lib/pkgconfig/libpng.pc"],
    },
    Project {
        name: "pixman",
        version: "0.42.2",
        url: "https://cairographics.org/releases/pixman-0.42.2.tar.gz",
        sha256: "ea1480efada2fd948bc75366f7c349e1c96d3297d09a3fe62626e38e234a625e",
        unpacked: "pixman-0.42.2",
        // Последняя версия с autotools (дальше — только meson). GTK и libpng
        // нужны лишь её тестам.
        // Тесты — `fork` и `waitpid`, которых нет; собирается только сама
        // библиотека.
        how: Autotools::plain(&["--disable-gtk", "--disable-libpng", "--disable-openmp"], &["SUBDIRS=pixman"]),
        installs: &["lib/libpixman-1.a", "include/pixman-1/pixman.h", "lib/pkgconfig/pixman-1.pc"],
    },
    Project {
        name: "freetype",
        version: "2.13.2",
        url: "https://download.savannah.gnu.org/releases/freetype/freetype-2.13.2.tar.xz",
        sha256: "12991c4e55c506dd7f9b765933e62fd2be2e06d421505d7950a132e4f1bb484d",
        unpacked: "freetype-2.13.2",
        // Сжатые шрифты — через zlib, PNG-глифы — через libpng; HarfBuzz,
        // Brotli и bzip2 не нужны.
        how: Autotools::plain(&[
            "--with-zlib=yes",
            "--with-png=yes",
            "--with-harfbuzz=no",
            "--with-brotli=no",
            "--with-bzip2=no",
            // Компилятор машины сборки: им freetype собирает свою утилиту
            // `apinames`, которая исполняется здесь же, при сборке. clang от
            // LLVM на Windows сам находит MSVC и собирает под хост.
            "CC_BUILD=clang",
        ], &[
            // Свой Makefile freetype делает путь к себе абсолютным через `pwd`,
            // а `pwd` у sh из Git — это `/e/…`, которого `make` для Windows не
            // понимает. Переменная командной строки сильнее присваивания в
            // Makefile: путь остаётся относительным, а собирается она из корня.
            "TOP_DIR=.",
        ]),
        installs: &["lib/libfreetype.a", "include/freetype2/ft2build.h", "lib/pkgconfig/freetype2.pc"],
    },
    // Фаза 61b — выбор шрифта по имени. libgdiplus ищет шрифты только через
    // fontconfig («Arial» → лучший из того, что есть), а fontconfig читает
    // свои настройки XML-разборщиком — expat. Хеши — те, что опубликованы
    // рядом с архивами в Homebrew (сверено 2026-09-28).
    Project {
        name: "expat",
        version: "2.8.5",
        url: "https://github.com/libexpat/libexpat/releases/download/R_2_8_5/expat-2.8.5.tar.xz",
        sha256: "1e727b8933ec51a77a9a9d9afcf8e688bce45d907c13e36ab7393fe36e703182",
        unpacked: "expat-2.8.5",
        // Только библиотека. Соль хеш-таблиц — `arc4random_buf` из libc, его
        // `configure` находит сам.
        how: Autotools::plain(&["--without-xmlwf", "--without-examples", "--without-tests", "--without-docbook"], &[]),
        installs: &["lib/libexpat.a", "include/expat.h", "lib/pkgconfig/expat.pc"],
    },
    Project {
        name: "fontconfig",
        version: "2.15.0",
        url: "https://www.freedesktop.org/software/fontconfig/release/fontconfig-2.15.0.tar.xz",
        sha256: "63a0658d0e06e0fa886106452b58ef04f21f58202ea02a94c39de0d3335d7c0e",
        unpacked: "fontconfig-2.15.0",
        how: How::Autotools(Autotools {
            // Пути **цели**: fontconfig вшивает их в библиотеку и в свой
            // `fonts.conf` — там, в системе, она и ищет настройки, шрифты и
            // кеш. `prefix` при этом — sysroot, куда ставятся библиотека и
            // заголовки.
            //
            // Основной `fonts.conf` — не в `/etc/fonts`, как у Linux, а в
            // `/usr/share/fontconfig`, внутри образа системы: `/etc` у
            // установленной системы лежит на разделе состояния и обновлением
            // не заменяется (фаза 48), и настройка, приехавшая с одной версией
            // fontconfig, осталась бы там при следующей. Файл этот и сам
            // говорит «не править, заменяется при обновлении». А место для
            // своих дополнений человека — `/etc/fonts/conf.d`: его основной
            // файл включает.
            //
            // Документация, переводы и iconv не нужны; XML — через expat, не
            // libxml2. Кеш шрифтов при сборке не строится: его строит
            // `fc-cache` цели, а не машины сборки.
            //
            // Хеш-таблицу имён своих свойств (`fcobjshash.h`) fontconfig
            // порождает при сборке утилитой машины сборки `gperf`: готовой в
            // архиве нет (см. шапку файла).
            configure: &[
                "--sysconfdir=/etc",
                "--localstatedir=/var",
                "--with-baseconfigdir=/usr/share/fontconfig",
                "--with-configdir=/etc/fonts/conf.d",
                "--with-default-fonts=/usr/share/fonts",
                "--with-cache-dir=/var/cache/fontconfig",
                "--with-templatedir=/usr/share/fontconfig/conf.avail",
                "--with-xmldir=/usr/share/xml/fontconfig",
                "--disable-docs",
                "--disable-nls",
                "--disable-iconv",
                "--disable-libxml2",
                "--disable-cache-build",
                // Случайные числа (имена временных файлов кеша): найдя
                // `random`, fontconfig берёт и `initstate`/`setstate`, а у
                // picolibc их нет — и видно это только на компоновке
                // программы. `lrand48` есть, следующим она берёт его.
                "ac_cv_func_random=no",
            ],
            // Библиотека без утилит `fc-*`. Каталоги **установки** её
            // настроек — в sysroot: `make install` иначе полез бы в
            // `/usr/share` и `/var` машины сборки. Вшитые пути это не трогает —
            // они у неё в других, заглавных переменных.
            make: &[
                "SUBDIRS=fontconfig fc-case fc-lang src",
                "baseconfigdir={prefix}/share/fontconfig",
                "fc_cachedir={prefix}/var/cache/fontconfig",
                "xmldir={prefix}/share/xml/fontconfig",
            ],
            keep_paths: "-DFC_;-DFONTCONFIG_;-DCONFIGDIR",
            shared: &[("share/fontconfig/fonts.conf", "fonts.conf")],
        }),
        installs: &[
            "lib/libfontconfig.a",
            "include/fontconfig/fontconfig.h",
            "lib/pkgconfig/fontconfig.pc",
            "share/fontconfig/fonts.conf",
        ],
    },
    Project {
        name: "cairo",
        version: "1.16.0",
        url: "https://cairographics.org/releases/cairo-1.16.0.tar.xz",
        sha256: "5e7b29b3f113ef870d1e3ecf8adf21f923396401604bda16d44be45e66052331",
        unpacked: "cairo-1.16.0",
        // Последняя версия с autotools. Поверхности — картинка в памяти, PNG,
        // PDF/SVG/PS (им хватает zlib); шрифты — freetype, выбор шрифта по
        // имени — fontconfig (61b: libgdiplus создаёт шрифт cairo из шаблона
        // fontconfig). Оконных систем чужих ОС нет.
        how: Autotools::plain(&[
            "--enable-ft=yes",
            "--enable-fc=yes",
            "--enable-png=yes",
            "--enable-xlib=no",
            "--enable-xcb=no",
            "--enable-quartz=no",
            "--enable-win32=no",
            "--enable-gobject=no",
            "--enable-trace=no",
            "--enable-interpreter=no",
            "--enable-symbol-lookup=no",
            "--disable-valgrind",
            // Порядок слов в `double` configure узнаёт, запустив программу, —
            // а при кросс-сборке запускать нечем. Обе наши архитектуры —
            // little-endian.
            "ax_cv_c_float_words_bigendian=no",
            // `locale_t` у picolibc — число, а cairo держит его указателем в
            // атомарной ячейке. Локаль ей нужна одна — «C», для чисел в PDF и
            // SVG, — и на этот случай у неё есть путь через `localeconv`.
            "ac_cv_func_newlocale=no",
            "ac_cv_func_strtod_l=no",
        ], &["SUBDIRS=src"]),
        installs: &["lib/libcairo.a", "include/cairo/cairo.h", "lib/pkgconfig/cairo.pc"],
    },
    // Шрифт для текста: DejaVu Sans. Не собирается — берётся готовым; рядом
    // едет его лицензия, этого она и требует.
    Project {
        name: "dejavu",
        version: "2.37",
        url: "https://github.com/dejavu-fonts/dejavu-fonts/releases/download/version_2_37/dejavu-fonts-ttf-2.37.tar.bz2",
        sha256: "fa9ca4d13871dd122f61258a80d01751d603b4d3ee14095d65453b4e846e17d7",
        unpacked: "dejavu-fonts-ttf-2.37",
        how: How::Files(&[("ttf/DejaVuSans.ttf", "DejaVuSans.ttf"), ("LICENSE", "DejaVu-LICENSE")]),
        installs: &[],
    },
];

/// Собрать чужие проекты под все указанные архитектуры: все или только
/// названные в `only`.
///
/// `only` — для пересборки одного проекта после правки набора или его ключей:
/// всё целиком — это полчаса, из них двадцать минут — cairo. Зависимости он
/// не пересобирает: названный проект строится поверх того, что уже в sysroot.
pub fn build_all(arches: &[Arch], refresh: bool, only: &[String]) -> Result<()> {
    if let Some(unknown) = only.iter().find(|name| !PROJECTS.iter().any(|project| project.name == name.as_str())) {
        let known: Vec<&str> = PROJECTS.iter().map(|project| project.name).collect();
        bail!("нет чужого проекта «{unknown}»; есть: {}", known.join(", "));
    }
    for project in PROJECTS.iter().filter(|project| only.is_empty() || only.iter().any(|name| name == project.name)) {
        let source = fetch(project, refresh)?;
        if let How::Files(files) = project.how {
            let dir = cbuild::fonts_dir();
            fs::create_dir_all(&dir)?;
            for (from, to) in files {
                fs::copy(source.join(from), dir.join(to))
                    .with_context(|| format!("не удалось положить {from} из {}", project.name))?;
            }
            say!("{} {}: {} файл(а) в {}", project.name, project.version, files.len(), dir.display());
            continue;
        }
        for &arch in arches {
            say!("=== {} {} для {} ===", project.name, project.version, arch.name());
            build(project, &source, arch)?;
        }
        if let (How::Autotools(own), Some(&last)) = (project.how, arches.last()) {
            let dir = cbuild::fonts_dir();
            for (from, to) in own.shared {
                fs::create_dir_all(&dir)?;
                fs::copy(cbuild::sysroot(last).join(from), dir.join(to))
                    .with_context(|| format!("не удалось положить {from} из {}", project.name))?;
                say!("{} {}: {from} -> {}", project.name, project.version, dir.join(to).display());
            }
        }
    }
    Ok(())
}

/// Принести архив и распаковать его. Возвращает каталог с исходниками.
///
/// Распакованное дерево считается **эталонным и неприкосновенным**: собирается
/// не оно, а его копия. Так «правок не было» — не обещание, а наблюдаемое
/// свойство.
fn fetch(project: &Project, refresh: bool) -> Result<PathBuf> {
    let root = dir();
    fs::create_dir_all(&root)?;
    let source = root.join(project.unpacked);
    if source.is_dir() && !refresh {
        say!("исходники на месте: {}", source.display());
        return Ok(source);
    }
    if refresh {
        let _ = fs::remove_dir_all(&source);
    }

    // Имя — из адреса: архивы бывают и `.tar.gz`, и `.tar.xz`.
    let archive = root.join(project.url.rsplit('/').next().unwrap_or(project.name));
    if !archive.is_file() || refresh {
        say!("> curl {}", project.url);
        let status = Command::new("curl")
            .arg("--location")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            .arg("--output")
            .arg(&archive)
            .arg(project.url)
            .status()
            .context("не удалось запустить curl — нет доступа в сеть?")?;
        if !status.success() {
            bail!("не скачался {} ({status})", project.url);
        }
    }

    let bytes = fs::read(&archive)?;
    let mut hasher = fpk::Hasher::new();
    hasher.update(&bytes);
    let got = hex(&hasher.finish());
    if got != project.sha256 {
        // Файл не удаляется намеренно: чтобы человек мог посмотреть, что именно
        // приехало. Удалённый «неправильный» файл — это отладка вслепую.
        bail!(
            "хеш {} не тот, что ожидался:\n  приехало {got}\n  ожидалось {}\n\
             Файл оставлен: {}",
            archive.display(),
            project.sha256,
            archive.display()
        );
    }
    say!("хеш архива совпал: {got}");

    let status = Command::new("tar")
        .current_dir(&root)
        // `-xf` без буквы сжатия: tar узнаёт gzip и xz по содержимому.
        .arg("-xf")
        .arg(archive.file_name().unwrap())
        .status()
        .context("не удалось запустить tar")?;
    if !status.success() {
        bail!("не распаковался {} ({status})", archive.display());
    }
    if !source.is_dir() {
        bail!(
            "архив распакован, но каталога {} нет — имя внутри архива другое",
            source.display()
        );
    }
    Ok(source)
}

/// Собрать один проект под одну архитектуру и установить его в sysroot.
fn build(project: &Project, source: &Path, arch: Arch) -> Result<()> {
    let sysroot = cbuild::sysroot(arch);
    if !sysroot.join("lib/crt0.o").is_file() {
        bail!("набор не установлен для {}\nСобрать: cargo xtask sdk", arch.name());
    }
    let work = dir().join(format!("{}-{}", project.name, arch.name()));
    // Каждый раз с чистого дерева: `configure` кеширует результаты проверок в
    // самом `Makefile`, и второй прогон после правки набора собрал бы старыми.
    let _ = fs::remove_dir_all(&work);
    copy_tree(source, &work)?;

    let triple = cbuild::c_target(arch).sdk_triple();
    let prefix = to_posix(&sysroot);

    match project.how {
        How::Configure => {
            // `--static`: разделяемых библиотек в этой системе нет вовсе — нет
            // ни динамического компоновщика, ни отображения чужого образа в
            // чужое адресное пространство. Просить их у zlib значило бы
            // получить отказ на шаге, к набору отношения не имеющем.
            run_shell(
                &work,
                &format!("CHOST={triple} ./configure --static --prefix={prefix}"),
                &format!("configure {} {}", project.name, arch.name()),
            )?;
            run_make(&work, &["libz.a"], &format!("make {}", arch.name()))?;
            run_make(&work, &["install"], &format!("make install {}", arch.name()))?;
        }
        How::Autotools(own) => {
            replace_config_sub(&work)?;
            let keep = (!own.keep_paths.is_empty()).then_some(own.keep_paths);
            // `--disable-shared`: разделяемых библиотек в системе нет (см. zlib
            // выше). Инструменты `configure` находит по приставке триплета —
            // и `pkg-config` тоже: у набора он свой и смотрит только в sysroot.
            run_shell(
                &work,
                &format!(
                    // `LD` — явно: libtool ищет компоновщик, спрашивая компилятор
                    // `-print-prog-name=ld`, и нашего по такому вопросу не находит
                    // (так было и с Mono, фаза 58b).
                    // `CXX`/`CXXCPP` — по той же причине: libtool проверяет C++ у
                    // всякого проекта, а `g++` с приставкой в наборе нет.
                    "./configure --host={triple} --prefix={prefix} --disable-shared --enable-static \
                     LD={triple}-ld CXX={triple}-cc CXXCPP=\"{triple}-cc -E\" {}",
                    own.configure.join(" ")
                ),
                &format!("configure {} {}", project.name, arch.name()),
            )?;
            // `SHELL` — путь к sh без пробела: `make` из winget подставляет его
            // в рецепты libtool без кавычек, и «C:/Program Files/…» ломает
            // первую же команду (так было и с Mono, фаза 58b).
            let shell = format!("SHELL={}", make_shell()?);
            let vars: Vec<String> = own.make.iter().map(|var| var.replace("{prefix}", &prefix)).collect();
            let mut build: Vec<&str> = vec!["-j6", &shell];
            build.extend(vars.iter().map(String::as_str));
            let mut install: Vec<&str> = vec!["install", &shell];
            install.extend(vars.iter().map(String::as_str));
            run_make_keeping(&work, &build, keep, &format!("make {} {}", project.name, arch.name()))?;
            run_make_keeping(&work, &install, keep, &format!("make install {} {}", project.name, arch.name()))?;
        }
        // Файлы не собираются: их кладёт `build_all`, один раз на обе
        // архитектуры.
        How::Files(_) => return Ok(()),
        How::MakePlatform(platform) => {
            // Компилятор передаётся переменной, и это не наша выдумка: именно
            // так кросс-собирают Lua, и написано это в его `doc/readme.html`.
            // Имена инструментов — с приставкой триплета, как у всякого
            // кросс-набора; находит их `make` по PATH.
            //
            // `INSTALL="cp -p"` — вариант, предложенный самим его `Makefile`
            // строкой ниже настроек. `install` на этой машине есть, но он от
            // Git, и права `-m 0755` на файловой системе Windows означают не то
            // же, что в Unix; `cp` честнее.
            let recipe = format!(
                "make -C src {platform} \
                     CC=\"{triple}-cc -std=gnu99\" \
                     AR=\"{triple}-ar rcu\" \
                     RANLIB=\"{triple}-ranlib\""
            );
            run_shell(&work, &recipe, &format!("make {platform} {}", arch.name()))?;
            run_shell(
                &work,
                &format!("make install INSTALL_TOP={prefix} INSTALL=\"cp -p\" INSTALL_EXEC=\"cp -p\" INSTALL_DATA=\"cp -p\""),
                &format!("make install {}", arch.name()),
            )?;
        }
    }

    for name in project.installs {
        let expected = sysroot.join(name);
        if !expected.is_file() {
            bail!(
                "{} собрался, но {} в наборе не появился",
                project.name,
                expected.display()
            );
        }
    }
    let sizes: Vec<String> = project
        .installs
        .iter()
        .map(|name| {
            let size = fs::metadata(sysroot.join(name)).map(|meta| meta.len()).unwrap_or(0);
            format!("{name} {size} байт")
        })
        .collect();
    say!(
        "{} {} для {}: {}, установлен в {}",
        project.name,
        project.version,
        arch.name(),
        sizes.join(", "),
        sysroot.display()
    );
    Ok(())
}

/// Запустить строку в `sh` внутри каталога сборки.
///
/// Через `sh -c`, а не разбором на аргументы: `./configure` — это скрипт, и
/// запустить его напрямую на Windows нечем. Строка при этом наша, а не чужая:
/// подстановок в неё не приходит.
fn run_shell(dir: &Path, script: &str, what: &str) -> Result<()> {
    let sh = find_sh()?;
    let mut cmd = Command::new(sh);
    cmd.current_dir(dir).arg("-c").arg(script);
    with_sdk_path(&mut cmd);
    run(cmd, what)
}

fn run_make(dir: &Path, args: &[&str], what: &str) -> Result<()> {
    run_make_keeping(dir, args, None, what)
}

/// `make`, у которого sh из Git не переписывает аргументы с приставками из
/// `keep` (см. [`Autotools::keep_paths`]).
fn run_make_keeping(dir: &Path, args: &[&str], keep: Option<&str>, what: &str) -> Result<()> {
    let make = find_make()?;
    let mut cmd = Command::new(make);
    cmd.current_dir(dir).args(args);
    with_sdk_path(&mut cmd);
    if let Some(keep) = keep {
        cmd.env("MSYS2_ARG_CONV_EXCL", keep);
    }
    run(cmd, what)
}

/// Дополнить PATH дочернего процесса тем, что ему понадобится.
///
/// Каталог обёрток — **в начало**, и это главное: по нему `configure` найдёт
/// `x86_64-freeos-gcc`. LLVM — потому что обёртка зовёт clang, а он у winget
/// стоит машинно и на PATH пользователя может не появиться до перезахода. `sh`
/// от Git — потому что рецепты `Makefile` это команды оболочки, и `make` без
/// неё падает на первой же.
fn with_sdk_path(cmd: &mut Command) {
    let mut dirs: Vec<PathBuf> = vec![sdk::bin_dir()];
    if let Ok(clang) = cbuild::llvm_tool("clang") {
        if let Some(parent) = clang.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    if let Ok(sh) = find_sh() {
        if let Some(parent) = sh.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    if let Ok(existing) = std::env::var("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        cmd.env("PATH", joined);
    }
    // Набор ищет sysroot относительно себя, и относительный поиск здесь верен.
    // Переменная всё равно ставится: она делает выбор явным в журнале сборки, а
    // при переносе обёрток отдельно от sysroot — единственным работающим.
    cmd.env("FREEOS_SYSROOT", cbuild::sysroot_root());
}

/// Найти `sh`: он приезжает с Git и на PATH у Windows обычно не стоит.
fn find_sh() -> Result<PathBuf> {
    if let Ok(found) = cbuild::llvm_tool("sh") {
        return Ok(found);
    }
    for candidate in [r"C:\Program Files\Git\usr\bin", r"C:\Program Files\Git\bin"] {
        let path = Path::new(candidate).join("sh.exe");
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!(
        "не найден sh: чужой `configure` — это сценарий оболочки.\n\
         Поставить: winget install --id Git.Git --exact"
    )
}

/// Подменить в дереве проекта все `config.sub` нашим (фаза 61).
///
/// `config.sub` — сценарий, который по `--host` называет систему и отказывает
/// незнакомой: `x86_64-freeos` для него «OS 'freeos' not recognized». Файл
/// самостоятельный и обратно совместимый — заменить старый новым было обычным
/// делом, когда появлялась новая архитектура, — поэтому здесь он один на все
/// проекты: `ports/autotools/config.sub`, свежий GNU (из libpng 1.6.43) с
/// единственной дописанной системой, `freeos`. Mono носит свой — в своём
/// патче.
fn replace_config_sub(work: &Path) -> Result<()> {
    let ours = crate::paths::workspace_root().join("ports/autotools/config.sub");
    let mut stack = vec![work.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|name| name == "config.sub") {
                fs::copy(&ours, &path).with_context(|| format!("не удалось заменить {}", path.display()))?;
            }
        }
    }
    Ok(())
}

/// Путь к `sh`, пригодный для `SHELL=` у `make`: без пробелов.
///
/// Git стоит в `C:\Program Files`, и короткое имя каталога (`PROGRA~1`) — это
/// тот же путь без пробела; косые — прямые, как их пишет `make`.
fn make_shell() -> Result<String> {
    let sh = find_sh()?;
    let text = sh.to_string_lossy().replace('\\', "/");
    Ok(text.replace("Program Files", "PROGRA~1"))
}

/// Найти `make`.
fn find_make() -> Result<PathBuf> {
    for name in ["make", "mingw32-make", "gmake"] {
        if let Ok(found) = cbuild::llvm_tool(name) {
            return Ok(found);
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let path = Path::new(&local).join(r"Microsoft\WinGet\Links\make.exe");
        if path.is_file() {
            return Ok(path);
        }
    }
    bail!(
        "не найден make: чужой проект собирается своей сборочной системой.\n\
         Поставить: winget install --id ezwinports.make --exact"
    )
}

fn run(mut cmd: Command, what: &str) -> Result<()> {
    say!("> {what}");
    let status = cmd
        .status()
        .with_context(|| format!("не удалось запустить: {what}"))?;
    if !status.success() {
        bail!("{what} завершился с ошибкой ({status})");
    }
    Ok(())
}

/// Путь в написании, которое понимает `sh`: с прямыми косыми.
///
/// Обратная косая внутри строки для оболочки — знак экранирования, и
/// `--prefix=E:\build\toolchain` доехал бы до `Makefile` как `E:buildtoolchain`.
/// Ошибка при этом не немедленная: она проявляется на `make install`, который
/// бодро создаёт каталог с этим именем.
fn to_posix(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Скопировать дерево целиком.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).with_context(|| {
                format!("не скопировать {} -> {}", entry.path().display(), target.display())
            })?;
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Путь для оболочки — с прямыми косыми, и это не косметика: см. [`to_posix`].
    #[test]
    fn shell_paths_use_forward_slashes() {
        assert_eq!(to_posix(Path::new(r"E:\a\b")), "E:/a/b");
    }

    /// Хеш архива выписан полностью, а не приставкой: сравнение по приставке
    /// проверяет ровно столько знаков, сколько написано.
    #[test]
    fn digests_are_full_length() {
        for project in &PROJECTS {
            assert_eq!(
                project.sha256.len(),
                64,
                "у {} хеш длиной {} знаков",
                project.name,
                project.sha256.len()
            );
        }
    }
}
