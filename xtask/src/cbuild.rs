//! Сборка того, что написано на C, и сверка его договора с `user-abi`.
//!
//! # Зачем здесь сверка, а не просто сборка
//!
//! Номера системных вызовов выписаны дважды: в `crates/user-abi/src/lib.rs` для
//! стороны Rust и в `libc/freeos/freeos-syscall.h` для стороны C. Одно из двух
//! пришлось бы порождать из другого, и порождать было бы хуже: заголовок читает
//! человек, и комментарии в нём объясняют не меньше, чем числа.
//!
//! Но дублирование опасное. Разъехавшись, две таблицы не дадут ни ошибки
//! сборки, ни отказа во время работы: программа просто позовёт не тот вызов, и
//! выглядеть это будет как испорченная файловая система или молча пропавший
//! вывод. Поэтому дублирование не запрещено, а **проверено**: тест ниже читает
//! заголовок, вынимает каждый `#define SYS_*` и `#define FREEOS_*` и сверяет с
//! константой того же имени из `user-abi`.
//!
//! Цена ошибки измерена на себе: первая версия заголовка была написана по
//! памяти, и четыре номера ошибок из пяти оказались чужими. Сборка прошла.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::paths;

/// Каталог с исходниками на C.
pub fn libc_dir() -> PathBuf {
    paths::workspace_root().join("libc")
}

/// Заголовок с номерами вызовов.
pub fn syscall_header() -> PathBuf {
    libc_dir().join("freeos/freeos-syscall.h")
}

/// Цель набора, соответствующая нашей архитектуре.
///
/// Перевод, а не второй перечислитель: правила сборки C живут в крейте
/// `freeos-cc`, потому что он же — обёртка `x86_64-freeos-cc`, которой собирают
/// чужие проекты. Два списка флагов — наш и её — разошлись бы молча.
pub fn c_target(arch: Arch) -> freeos_cc::Target {
    match arch {
        Arch::X86_64 => freeos_cc::Target::X86_64,
        Arch::Aarch64 => freeos_cc::Target::Aarch64,
    }
}

/// Найти инструмент набора: сначала на PATH, потом там, куда его кладёт winget.
pub fn llvm_tool(name: &str) -> Result<PathBuf> {
    freeos_cc::find_tool(name).map_err(anyhow::Error::msg)
}

/// Флаги компиляции для **нашего** кода: правила набора плюс строгость.
///
/// Строгость приписывается здесь, а не в наборе, и это существенно: `-Werror` на
/// своём коде — дисциплина, а на чужом — «сборка ломается от новой версии
/// компилятора». Обёртка, которой собирают zlib, этих трёх флагов не ставит.
pub fn c_flags(arch: Arch) -> Vec<String> {
    let mut flags = freeos_cc::compile_flags(c_target(arch));
    flags.extend(["-O2".to_string(), "-Wall".into(), "-Wextra".into(), "-Werror".into()]);
    flags
}

/// Скомпилировать один файл C в объектник.
///
/// `-nostdinc` обязателен: без него clang подставляет **свои** заголовки, и
/// программа собирается против чужого `stdio.h`, а линкуется с нашей libc.
/// Расхождение при этом не обязано быть ошибкой сборки — оно бывает разной
/// раскладкой `FILE`, то есть падением на первом же `printf`.
///
/// Исключение одно и названо: каталог самого clang (`-idirafter`) остаётся —
/// оттуда берутся `stddef.h`, `stdarg.h`, `stdint.h` и прочее, что обязан
/// поставлять **компилятор**, а не библиотека. picolibc их и не поставляет.
pub fn compile(arch: Arch, source: &Path, object: &Path, includes: &[PathBuf]) -> Result<()> {
    let clang = llvm_tool("clang")?;
    if let Some(parent) = object.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cmd = Command::new(&clang);
    cmd.args(c_flags(arch));
    cmd.arg("-nostdinc");
    for dir in includes {
        cmd.arg(format!("-I{}", dir.display()));
    }
    if let Some(builtin) = clang_builtin_includes(&clang) {
        cmd.arg(format!("-idirafter{}", builtin.display()));
    }
    cmd.arg("-c").arg(source).arg("-o").arg(object);
    run(cmd, &format!("clang {}", source.display()))
}

/// Каталог заголовков самого компилятора: `<корень LLVM>/lib/clang/<версия>/include`.
fn clang_builtin_includes(clang: &Path) -> Option<PathBuf> {
    freeos_cc::clang_builtin_includes(clang)
}

/// Слинковать программу нашим компоновочным сценарием.
///
/// Сценарий тот же, что у программ на Rust (`crates/user-progs/user.ld`), и это
/// не экономия: раскладка задана требованиями ядра — адрес 512 ГиБ, страницы,
/// разведённые по правам, — и второй сценарий разъехался бы с первым молча.
pub fn link(objects: &[PathBuf], libs: &[PathBuf], output: &Path) -> Result<()> {
    let lld = llvm_tool("ld.lld")?;
    let script = paths::workspace_root().join("crates/user-progs/user.ld");
    if !script.is_file() {
        bail!("нет компоновочного сценария программ: {}", script.display());
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cmd = Command::new(&lld);
    cmd.arg("-T")
        .arg(&script)
        .arg("-z")
        .arg("max-page-size=0x1000")
        .args(freeos_cc::LINK_PIE)
        // Отладочную информацию выбрасываем здесь же, а не отдельным шагом:
        // ядро читает файл программы целиком в кучу, прежде чем разобрать
        // заголовки, и сегмент, уехавший за предел чтения, выглядел бы как
        // испорченный файл.
        .arg("--strip-debug")
        .arg("-o")
        .arg(output);
    cmd.args(objects);
    cmd.args(libs);
    run(cmd, &format!("ld.lld -> {}", output.display()))
}

/// Собрать файл на ассемблере.
///
/// Отдельно от [`compile`]: флаги C здесь не к месту, а `-Werror` превращает
/// каждый неиспользованный (`-O2`, `-D…`, `-nostdinc`) в отказ сборки.
pub fn assemble(arch: Arch, source: &Path, object: &Path) -> Result<()> {
    let clang = llvm_tool("clang")?;
    if let Some(parent) = object.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cmd = Command::new(&clang);
    cmd.arg(format!("--target={}", freeos_cc::triple(c_target(arch))));
    cmd.arg("-c").arg(source).arg("-o").arg(object);
    run(cmd, &format!("clang {}", source.display()))
}

/// Стартовый код позиционно-независимой программы — общий с Rust (см.
/// `crates/user-progs/src/start.rs`).
pub fn start_source(arch: Arch) -> PathBuf {
    libc_dir().join("freeos").join(format!("start-{}.s", arch.name()))
}

/// Программа на C, которая едет в `/bin`.
pub struct CProgram {
    /// Имя файла в `/bin` и имя исходника в `libc/examples`.
    pub name: &'static str,
    /// Файл в `<sysroot>/lib`, без которого её незачем собирать.
    ///
    /// `None` — программе хватает libc. Иначе это чужая библиотека, и её
    /// отсутствие не ошибка: `cargo xtask thirdparty` ходит в сеть, и система
    /// обязана собираться на машине без интернета — просто без этой программы.
    pub needs: Option<&'static str>,
    /// Что дописать компоновщику после наших объектников.
    pub libs: &'static [&'static str],
    /// Свои каталоги заголовков под `<sysroot>/include` (фаза 61): cairo и
    /// freetype кладут их не в корень, а в `cairo/` и `freetype2/`.
    pub includes: &'static [&'static str],
}

/// Программы на C, которые едут в `/bin`.
pub const C_PROGRAMS: [CProgram; 8] = [
    CProgram { name: "cdemo", needs: None, libs: &[], includes: &[] },
    // Переполняет свой стек нарочно: доказывает, что канарейка стека есть и у
    // программ на C. См. `libc/examples/csmash.c` и `__stack_chk_fail` в `crt0.c`.
    CProgram { name: "csmash", needs: None, libs: &[], includes: &[] },
    // Память так, как её просит чужая среда исполнения: резерв, куски,
    // права страниц (фаза 56). См. `libc/examples/cmem.c`.
    CProgram { name: "cmem", needs: None, libs: &[], includes: &[] },
    // Потоки POSIX: мьютексы, условные переменные со сроком, `errno` и
    // `__thread` у каждого свои (фаза 57). См. `libc/examples/cthreads.c`.
    CProgram { name: "cthreads", needs: None, libs: &[], includes: &[] },
    // Слой POSIX под чужую среду: текущий каталог, каталоги, канал с `poll`,
    // пределы, таблица сигналов (фаза 58b). См. `libc/examples/cposix.c`.
    CProgram { name: "cposix", needs: None, libs: &[], includes: &[] },
    // Память кода: функция, записанная в память и вызванная, пока второй
    // поток её исполняет, — W^X по обращению (фаза 59). См. `libc/examples/cjit.c`.
    CProgram { name: "cjit", needs: None, libs: &[], includes: &[] },
    // Чужая библиотека, собранная нашим набором. Она здесь не ради сжатия: это
    // единственная проверка, доказывающая, что код, вышедший из чужого
    // `configure`, **работает**, а не только собрался. См. `libc/examples/zdemo.c`.
    CProgram { name: "zdemo", needs: Some("libz.a"), libs: &["libz.a"], includes: &[] },
    // Рисование чужой графической стопкой — cairo поверх pixman, libpng,
    // freetype и zlib (фаза 61a): на ней стоит `System.Drawing` Mono. Точки
    // картинки, PNG туда и обратно, глиф шрифта. См. `libc/examples/cdraw.c`.
    CProgram {
        name: "cdraw",
        needs: Some("libcairo.a"),
        libs: &["libcairo.a", "libpixman-1.a", "libfreetype.a", "libpng16.a", "libz.a"],
        includes: &["cairo", "freetype2", "pixman-1"],
    },
];

/// Программы, которые собрал **не наш** компилятор и не наш рецепт.
///
/// Появляются в `sysroot/<арх>/bin/` после `cargo xtask thirdparty` и едут в
/// образ как есть — их уже собрала своя сборочная система. Пересобирать их
/// здесь было бы ровно той подгонкой, против которой написана вся проверка
/// чужого кода: наш список файлов вместо их `Makefile`.
///
/// `luac` (сборщик байткода Lua) в этот список не входит: он собирается вместе
/// с `lua` и лежит в наборе, но в образ не едет. Скрипты у нас исполняются
/// исходниками, а четверть мегабайта в `/bin` ради возможности заранее собрать
/// их в байткод — цена без спроса.
///
/// `mono` (фаза 58b) — рантайм Mono 6.14.1, собранный своим `configure` по
/// рецепту `ports/mono/configure.sh`. Библиотеки классов к нему — [`MONO_BCL`]
/// (фаза 60).
pub const FOREIGN_PROGRAMS: [&str; 2] = ["lua", "mono"];

/// Библиотеки классов Mono, которые едут в образ рядом с `/bin/mono` (фаза 60):
/// имя сборки и каталог в образе.
///
/// Сборки IL, одни на обе архитектуры, — начальный набор `monolite` из архива
/// исходников 6.14.1, ровно под наш рантайм (почему не из пакета — в
/// `ports/mono/bcl.sh`, он их и кладёт в [`mono_bcl_dir`]). Три сборки — то, без
/// чего не работает консольная программа на C#.
///
/// Раскладка — та же, что у установленной Mono на Linux, и она не выбор, а
/// требование рантайма. `mscorlib` он ищет в `<prefix>/lib/mono/4.5`, prefix —
/// `/usr` (`ports/mono/configure.sh`). Все остальные сборки со строгим именем —
/// **только** в GAC: `lib/mono/gac/<имя>/<версия>__<токен>/`. Положенная рядом
/// с mscorlib `System.dll` не находится вовсе: у настоящей установки файлы в
/// `4.5` — ссылки в GAC, а не сами сборки.
///
/// Третье поле — имя 8.3 на установочном носителе (FAT), каталог там —
/// [`crate::arch::PAYLOAD_MONO_LIB_DIR`]. Список обязан совпадать с `MONO_LIB`
/// в `crates/installer/src/payload.rs`.
pub const MONO_BCL: [(&str, &str, &str); 3] = [
    ("mscorlib.dll", "usr/lib/mono/4.5", "MSCORLIB.DLL"),
    ("System.dll", "usr/lib/mono/gac/System/4.0.0.0__b77a5c561934e089", "SYSTEM.DLL"),
    ("System.Core.dll", "usr/lib/mono/gac/System.Core/4.0.0.0__b77a5c561934e089", "SYSCORE.DLL"),
];

/// Куда `ports/mono/bcl.sh` кладёт библиотеки классов на машине сборки.
pub fn mono_bcl_dir() -> PathBuf {
    toolchain_dir().join("mono-bcl/4.5")
}

/// Шрифты, которые едут в образ (фаза 61): имя, каталог в образе, имя 8.3 на
/// носителе.
///
/// Рисованию текста — cairo через freetype, дальше `System.Drawing` — нужен
/// настоящий векторный шрифт, а своего TrueType у системы нет (её собственный
/// шрифт — растровый, рисуется ядром). DejaVu Sans — свободный (лицензия
/// Bitstream Vera с дополнениями), широкий по охвату и есть почти в любом
/// Linux. Кладёт его `cargo xtask thirdparty` в [`fonts_dir`]. Список обязан
/// совпадать с `FONTS` в `crates/installer/src/payload.rs`.
pub const FONTS: [(&str, &str, &str); 2] = [
    ("DejaVuSans.ttf", "usr/share/fonts/dejavu", "DEJAVU.TTF"),
    // Лицензия шрифта едет рядом с ним: этого она и требует.
    ("DejaVu-LICENSE", "usr/share/fonts/dejavu", "DEJAVU.TXT"),
];

/// Куда `cargo xtask thirdparty` кладёт шрифты: одни на обе архитектуры.
pub fn fonts_dir() -> PathBuf {
    toolchain_dir().join("fonts")
}

/// Чужой файл образа — не программа: библиотека классов Mono, шрифт.
pub struct ForeignFile {
    /// Путь в образе, без ведущей косой.
    pub image: String,
    /// Где он лежит на машине сборки.
    pub host: PathBuf,
    /// Путь на установочном носителе (FAT, имена 8.3).
    pub medium: String,
}

/// Чужие файлы образа: библиотеки классов Mono — только вместе с самой
/// `mono` (без рантайма они мёртвый груз в девять мегабайт), и шрифты.
///
/// Каждая группа — все файлы или ни одного: без mscorlib Mono не запускает
/// ничего, а половина набора падала бы на первой же ссылке на недостающую
/// сборку — посреди работы, а не при запуске. Не подготовлены — пусто, как у
/// чужих программ: система обязана собираться и без них. Один список на всех,
/// кто собирает образ (RAM-диск, установочный носитель, образ обновления), —
/// по той же причине, по которой есть `arch::IMAGE_SHARE`.
pub fn foreign_files(with_mono: bool) -> Vec<ForeignFile> {
    let mut out = Vec::new();
    if with_mono {
        out.extend(foreign_group(
            &MONO_BCL,
            &mono_bcl_dir(),
            crate::arch::PAYLOAD_MONO_LIB_DIR,
            "библиотеки классов Mono",
            "sh ports/mono/bcl.sh",
        ));
    }
    out.extend(foreign_group(&FONTS, &fonts_dir(), crate::arch::PAYLOAD_FONTS_DIR, "шрифты", "cargo xtask thirdparty"));
    out
}

fn foreign_group(
    list: &[(&str, &str, &str)],
    dir: &Path,
    medium_dir: &str,
    what: &str,
    how: &str,
) -> Vec<ForeignFile> {
    let files: Vec<ForeignFile> = list
        .iter()
        .map(|(name, image_dir, medium)| ForeignFile {
            image: format!("{image_dir}/{name}"),
            host: dir.join(name),
            medium: format!("{medium_dir}/{medium}"),
        })
        .collect();
    if files.iter().all(|file| file.host.is_file()) {
        files
    } else {
        say!("{what} пропущены: нет {} — положить: {how}", dir.display());
        Vec::new()
    }
}

/// Корень всего, что собрано из чужих исходников.
///
/// Под `build/`, то есть вне репозитория: это результат сборки, а не наш код.
/// Отсюда же и правило — **ничего незаменимого здесь лежать не должно**.
/// Рецепт живёт в репозитории ([`toolchain`] и `tools/build-builtins.ps1`),
/// кросс-файлы порождаются, исходники клонируются. `cargo xtask clean` сносит
/// этот каталог, и после него всё восстанавливается одной командой.
fn toolchain_dir() -> PathBuf {
    paths::workspace_root().join("build/toolchain")
}

/// Собрать набор для C: picolibc под обе архитектуры.
///
/// Предполагает, что уже есть: LLVM (`winget install --id LLVM.LLVM`), meson и
/// ninja (`pip install --user meson ninja`), исходники picolibc и compiler-rt в
/// `build/toolchain`. Чего нет — о том говорится внятно, с командой, которой это
/// ставится.
pub fn toolchain() -> Result<()> {
    let root = toolchain_dir();
    let picolibc = root.join("picolibc");
    if !picolibc.join("meson.build").is_file() {
        bail!(
            "нет исходников picolibc: {}\n\
             Принести:\n  git clone https://github.com/picolibc/picolibc {}",
            picolibc.display(),
            picolibc.display()
        );
    }

    for arch in Arch::ALL {
        let builtins = builtins(arch);
        if !builtins.is_file() {
            bail!(
                "нет вспомогательных подпрограмм компилятора: {}\n\
                 Собрать: powershell -File tools/build-builtins.ps1 {}-none-elf {}\n\
                 (нужны исходники llvm-project в build/toolchain — см. рецепт в самом файле)",
                builtins.display(),
                arch.name(),
                arch.name()
            );
        }

        let build = root.join(format!("pico-{}", arch.name()));
        // Кросс-файл meson читает один раз, при `setup`: `configure` его не
        // перечитывает, и изменившиеся флаги компилятора (так в `c_flags`
        // появилась канарейка стека) молча остались бы без действия — libc
        // собиралась бы старыми флагами, а `cargo xtask toolchain` говорил бы,
        // что всё сделано. Поэтому изменившийся кросс-файл сносит каталог
        // сборки: полная пересборка picolibc — минуты, а расхождение между
        // тем, что записано, и тем, что собрано, не находится никак.
        let cross_before = std::fs::read(cross_file_path(arch)).ok();
        let cross = write_cross_file(arch)?;
        if cross_before.is_some_and(|old| std::fs::read(&cross).ok().as_deref() != Some(old.as_slice()))
            && build.is_dir()
        {
            say!("кросс-файл {} изменился, каталог сборки пересоздаётся", arch.name());
            std::fs::remove_dir_all(&build)?;
        }
        let meson = tool("meson")?;
        let ninja = tool("ninja")?;

        if !build.join("build.ninja").is_file() {
            let mut cmd = Command::new(&meson);
            cmd.current_dir(&picolibc)
                .arg("setup")
                .arg(&build)
                .arg("--cross-file")
                .arg(&cross)
                // Префикс задаётся в стиле целевой системы, а не хоста: meson
                // при кросс-сборке считает `E:\...` **не** абсолютным путём и
                // отказывается. Раскладывает установленное `--destdir` ниже.
                .arg("--prefix=/")
                .args(MESON_OPTIONS);
            run(cmd, &format!("meson setup {}", arch.name()))?;
        } else {
            // Уже настроено — только сверяем параметры: они меняются чаще, чем
            // сам каталог сборки, и пересоздавать его ради этого незачем.
            let mut cmd = Command::new(&meson);
            cmd.arg("configure").arg(&build).args(MESON_OPTIONS);
            run(cmd, &format!("meson configure {}", arch.name()))?;
        }

        let mut cmd = Command::new(&ninja);
        cmd.arg("-C").arg(&build);
        run(cmd, &format!("ninja {}", arch.name()))?;

        let mut cmd = Command::new(&meson);
        cmd.arg("install")
            .arg("-C")
            .arg(&build)
            .arg("--destdir")
            .arg(sysroot(arch));
        run(cmd, &format!("meson install {}", arch.name()))?;

        // Канарейка стека у нас своя: эталон лежит в странице процесса (символ
        // ставит `user.ld`), падение ловит `crt0.c`. Объектник picolibc с тем же
        // назначением вреден дважды. Его конструктор `__stack_chk_init` пишет в
        // эталон — то есть в страницу только на чтение, и программа снималась
        // бы до `main`. А в позиционно-независимой программе он и не
        // компонуется: обращается к эталону относительно счётчика команд, до
        // абсолютного адреса страницы процесса не дотягивается. Вытаскивает
        // его любая ссылка на `__stack_chk_guard` — определение в сценарии
        // компоновки извлечения из архива не останавливает (так нашлось
        // 26.09.2026, при переходе на PIE). Поэтому его в архиве нет.
        let archive = sysroot(arch).join("lib").join("libc.a");
        let mut cmd = Command::new(llvm_tool("llvm-ar")?);
        cmd.arg("d").arg(&archive).arg("libc_ssp_stack_protector.c.o");
        run(cmd, &format!("llvm-ar d {}", archive.display()))?;
    }
    Ok(())
}

/// Как именно собирается picolibc.
///
/// Каждый ключ здесь отвечает на вопрос «чего в этой системе нет»:
///
/// * `semihost=false` — полухостинг это отладочный канал через отладчик, у нас
///   его нет; по умолчанию он **включён**, и без этой строки libc зовёт чужие
///   ловушки;
/// * `posix-console=true` — `stdin`/`stdout`/`stderr` идут через дескрипторы
///   0/1/2, то есть через наши `read`/`write`. По умолчанию picolibc ждёт, что
///   их устройство напишет порт сам;
/// * `picocrt=false` — стартовый код у нас свой (`libc/freeos/crt0.c`): ядро
///   передаёт аргументы регистрами, а не через стек, как принято у прошивок;
/// * `thread-local-storage=true`, `single-thread=false` — с фазы 57 потоки в
///   системе есть. До неё здесь стояло обратное, и это было исправлением
///   настоящего отказа: `errno` в TLS ронял программу отказом страницы по
///   адресу `-4`, потому что блока TLS никто не заводил. Теперь его заводит
///   `libc/freeos/threads.c` — по шаблону, который ядро кладёт в страницу
///   процесса, — до конструкторов и до `main`. Там же восемь функций
///   блокировки, которых многопоточная picolibc ждёт от системы;
/// * `multilib=false`, `tests=false` — вариантов сборки у нас один, а тесты
///   picolibc требуют запускать программы на хосте.
const MESON_OPTIONS: [&str; 8] = [
    "-Dsemihost=false",
    "-Dposix-console=true",
    "-Dpicocrt=false",
    "-Dthread-local-storage=true",
    "-Dsingle-thread=false",
    // Канарейка стека у нас глобальная — в странице процесса (`user.ld`).
    // С TLS picolibc по умолчанию («auto») выбрала бы канарейку в блоке
    // потока, по смещению, которого наш TCB не держит.
    "-Dstack-protector-guard=global",
    "-Dmultilib=false",
    "-Dtests=false",
];

/// Дополнить PATH дочернего процесса тем, что ему понадобится.
///
/// meson ищет `clang` и `llvm-ar` **сам**, по PATH, — имена в кросс-файле это
/// имена, а не пути. А `sh` нужен сборочному шагу самой picolibc: один из её
/// шагов это сценарий с `#!/bin/sh`, и без интерпретатора ninja падает стеком
/// Python со словами «не удаётся найти указанный файл», не называя, какой.
///
/// Каталоги добавляются **в начало**: у пользователя на PATH может стоять
/// другой clang, и собирать библиотеку одним компилятором, а программы другим —
/// это расхождение, которое не обязано быть ошибкой сборки.
fn with_tool_path(cmd: &mut Command) {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(clang) = llvm_tool("clang") {
        if let Some(dir) = clang.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(meson) = tool("meson") {
        if let Some(dir) = meson.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    for candidate in [r"C:\Program Files\Git\usr\bin"] {
        let dir = Path::new(candidate);
        if dir.join("sh.exe").is_file() {
            dirs.push(dir.to_path_buf());
        }
    }
    if let Ok(existing) = std::env::var("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        cmd.env("PATH", joined);
    }
}

/// Найти meson или ninja: на PATH либо там, куда их кладёт `pip install --user`.
fn tool(name: &str) -> Result<PathBuf> {
    if let Ok(found) = llvm_tool(name) {
        return Ok(found);
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let candidate = Path::new(&appdata)
            .join("Python/Python311/Scripts")
            .join(format!("{name}.exe"));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "не найден {name}.\n\
         Поставить: pip install --user meson ninja"
    )
}

/// Написать кросс-файл meson под эту архитектуру.
///
/// Порождается, а не лежит в репозитории, ровно по одной причине: в нём есть
/// **абсолютные пути** — к вспомогательным подпрограммам компилятора. Файл с
/// абсолютным путём, положенный в репозиторий, верен на одной машине.
///
/// # Три места, каждое из которых ломало сборку
///
/// 1. **`-L` нельзя писать в строке компилятора.** meson проверяет флаги
///    компиляцией с `-Werror=unused-command-line-argument`, и путь к
///    библиотекам, не нужный при компиляции, валит **каждую** такую проверку.
///    Выглядит это как «компилятор не поддерживает `-ffunction-sections`» и
///    кончается отказом на `-ftls-model`. Поэтому `-L` живёт в `c_link_args`.
/// 2. **У x86-64 триплет линуксовый.** Почему — написано у [`c_triple`].
/// 3. **Файл обязан быть без BOM.** meson читает его разбором ini и на BOM
///    отвечает «File contains no section headers» — про первую же строку,
///    которая на вид совершенно правильная.
/// Где лежит порождённый кросс-файл этой архитектуры.
fn cross_file_path(arch: Arch) -> PathBuf {
    toolchain_dir().join("cross").join(format!("{}.txt", arch.name()))
}

fn write_cross_file(arch: Arch) -> Result<PathBuf> {
    let path = cross_file_path(arch);
    if let Some(cross_dir) = path.parent() {
        std::fs::create_dir_all(cross_dir)?;
    }

    let rt_dir = builtins(arch)
        .parent()
        .map(|dir| dir.display().to_string().replace('\\', "/"))
        .unwrap_or_default();

    let compiler = c_flags(arch)
        .into_iter()
        // Предупреждения и оптимизация — дело наших программ, а не чужой
        // библиотеки: её собирают её же флагами, а `-Werror` на чужом коде
        // означает «сборка ломается от новой версии компилятора».
        .filter(|flag| !flag.starts_with("-W") && !flag.starts_with("-O"))
        .chain(["-static".to_string(), "-nostdlib".to_string()])
        .map(|flag| format!("'{flag}'"))
        .collect::<Vec<_>>()
        .join(", ");

    let text = format!(
        "# Порождён `cargo xtask toolchain`. Править здесь бесполезно — см.\n\
         # `write_cross_file` в `xtask/src/cbuild.rs`.\n\
         [binaries]\n\
         c = ['clang', {compiler}]\n\
         c_ld = 'lld'\n\
         ar = 'llvm-ar'\n\
         as = 'clang'\n\
         nm = 'llvm-nm'\n\
         strip = 'llvm-strip'\n\
         objcopy = 'llvm-objcopy'\n\
         \n\
         [built-in options]\n\
         c_link_args = ['-L{rt_dir}']\n\
         \n\
         [host_machine]\n\
         system = 'none'\n\
         cpu_family = '{cpu}'\n\
         cpu = '{cpu}'\n\
         endian = 'little'\n\
         \n\
         [properties]\n\
         skip_sanity_check = true\n\
         needs_exe_wrapper = true\n\
         librt = '-lclang_rt.builtins'\n",
        cpu = arch.name(),
    );
    // `write` кладёт ровно байты строки, без метки порядка байт: см. третью
    // ловушку в заголовке функции.
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Корень всех sysroot'ов: в нём по каталогу на архитектуру.
///
/// Именно его понимает переменная `FREEOS_SYSROOT`: обёртка дописывает к нему
/// имя своей цели сама, потому что цель она знает из своего имени, а вызвавший
/// её `make` — нет.
pub fn sysroot_root() -> PathBuf {
    paths::workspace_root().join("build/toolchain/sysroot")
}

/// Где лежит собранная picolibc для этой архитектуры.
///
/// Каталог под `build/`, то есть **не** в репозитории: это результат сборки
/// чужих исходников, а не наш код. Собирает его `cargo xtask toolchain`; нет
/// каталога — нет и libc, и сказать об этом надо внятно, а не падением clang.
pub fn sysroot(arch: Arch) -> PathBuf {
    sysroot_root().join(arch.name())
}

/// Библиотека вспомогательных подпрограмм компилятора.
///
/// Деление `__int128`, программная плавающая точка, `memcpy` там, где компилятор
/// решил позвать его сам. Без неё программа собирается ровно до первого такого
/// места, и ошибка выглядит как «неизвестный символ `__divti3`» в чужом коде.
pub fn builtins(arch: Arch) -> PathBuf {
    paths::workspace_root()
        .join("build/toolchain/compiler-rt")
        .join(format!("{}-none-elf", arch.name()))
        .join("libclang_rt.builtins.a")
}

/// Собрать программы на C. Возвращает пути к готовым файлам.
///
/// Порядок библиотек при компоновке не произволен: сначала наши объектники,
/// потом libc, потом вспомогательные подпрограммы компилятора. Компоновщик
/// берёт из архива только то, чего ещё не хватает, и идёт слева направо —
/// поставь `libc.a` первой, и она не вытянет ничего, потому что на этот момент
/// не нужно ещё ничего.
pub fn build_c_programs(arch: Arch) -> Result<Vec<(&'static str, PathBuf)>> {
    let sysroot = sysroot(arch);
    let libc = sysroot.join("lib/libc.a");
    if !libc.is_file() {
        bail!(
            "нет собранной libc: {}\n\
             Собрать набор: cargo xtask toolchain",
            libc.display()
        );
    }
    let builtins = builtins(arch);
    if !builtins.is_file() {
        bail!(
            "нет вспомогательных подпрограмм компилятора: {}\n\
             Собрать набор: cargo xtask toolchain",
            builtins.display()
        );
    }

    // Заголовок договора — из репозитория, а не его копия в sysroot: копию
    // кладёт `cargo xtask sdk`, и после правки договора она устаревает, а
    // стоя первой, заслоняет свежий (фаза 56 наткнулась: новый номер вызова
    // «не объявлен»). Других заголовков в `libc/freeos` нет, заслонять
    // ему нечего.
    // `libc/freeos/include` — заголовки POSIX, которых нет у picolibc
    // (`semaphore.h`, `sys/socket.h`, …, фаза 58).
    let includes = vec![libc_dir().join("freeos"), libc_dir().join("freeos/include"), sysroot.join("include")];
    let work = paths::workspace_root()
        .join("build/toolchain/cbuild")
        .join(arch.name());

    // Слой ОС и стартовый код собираются один раз на архитектуру: они одни и те
    // же для всех программ.
    let mut common = Vec::new();
    for name in ["crt0", "syscalls", "threads", "posix", "files"] {
        let source = libc_dir().join("freeos").join(format!("{name}.c"));
        let object = work.join(format!("{name}.o"));
        compile(arch, &source, &object, &includes)?;
        common.push(object);
    }
    // Вход позиционно-независимой программы: перемещения до `_start`.
    let start = work.join("start.o");
    assemble(arch, &start_source(arch), &start)?;
    common.push(start);

    let mut built = Vec::new();
    for program in &C_PROGRAMS {
        if let Some(needed) = program.needs {
            if !sysroot.join("lib").join(needed).is_file() {
                say!(
                    "{} пропущена: нет {} — собрать: cargo xtask thirdparty",
                    program.name,
                    sysroot.join("lib").join(needed).display()
                );
                continue;
            }
        }
        let source = libc_dir()
            .join("examples")
            .join(format!("{}.c", program.name));
        let object = work.join(format!("{}.o", program.name));
        let mut own = includes.clone();
        own.extend(program.includes.iter().map(|dir| sysroot.join("include").join(dir)));
        compile(arch, &source, &object, &own)?;

        let mut objects = common.clone();
        objects.push(object);
        // Чужие библиотеки идут **перед** libc: компоновщик берёт из архива
        // только недостающее и идёт слева направо, а zlib зовёт `memcpy` и
        // `malloc`. Поставленная после libc, она не вытянула бы ничего.
        let mut libs: Vec<PathBuf> = program
            .libs
            .iter()
            .map(|name| sysroot.join("lib").join(name))
            .collect();
        libs.push(libc.clone());
        libs.push(builtins.clone());
        let output = work.join(program.name);
        link(&objects, &libs, &output)?;
        built.push((program.name, output));
    }

    // Чужие программы берутся готовыми. Нет — значит `cargo xtask thirdparty`
    // на этой машине не выполнялась, и это не ошибка: система обязана
    // собираться и без неё, только без этих программ.
    for name in FOREIGN_PROGRAMS {
        let ready = sysroot.join("bin").join(name);
        if ready.is_file() {
            built.push((name, ready));
        } else {
            say!(
                "{name} пропущена: нет {} — собрать: cargo xtask thirdparty",
                ready.display()
            );
        }
    }
    Ok(built)
}

fn run(mut cmd: Command, what: &str) -> Result<()> {
    with_tool_path(&mut cmd);
    say!("> {what}");
    let status = cmd
        .status()
        .with_context(|| format!("не удалось запустить: {what}"))?;
    if !status.success() {
        bail!("{what} завершился с ошибкой ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Вынуть из заголовка все `#define ИМЯ ЧИСЛО`.
    ///
    /// Разбор нарочно тупой: строка, начинающаяся с `#define`, два слова после
    /// него, число в скобках или без. Умнее не нужно — заголовок наш, и правило
    /// «одно определение в строке» соблюдать дешевле, чем писать препроцессор.
    fn defines(text: &str) -> Vec<(String, i64)> {
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("#define ") else {
                continue;
            };
            let mut parts = rest.split_whitespace();
            let (Some(name), Some(value)) = (parts.next(), parts.next()) else {
                continue;
            };
            let value = value.trim_start_matches('(').trim_end_matches(')');
            // Суффиксы C (`UL`) и шестнадцатеричная запись: адрес страницы
            // процесса (фаза 57) записан так, и тупой разбор пропустил бы его
            // молча — то есть не сверил бы вовсе.
            let value = value.trim_end_matches(['U', 'L', 'u', 'l']);
            let parsed = match value.strip_prefix("0x") {
                Some(hex) => i64::from_str_radix(hex, 16),
                None => value.parse::<i64>(),
            };
            let Ok(number) = parsed else {
                continue;
            };
            out.push((name.to_string(), number));
        }
        out
    }

    /// Та же программа на C, собранная компилятором **хоста**, проходит те же
    /// проверки.
    ///
    /// Смысл не в дублировании, а в том, что проверяется разное. Прогон в QEMU
    /// отвечает на вопрос «работает ли наша libc»; этот — на вопрос «обычная ли
    /// это программа». Программа, написанная под наши особенности, прошла бы
    /// первый и провалила второй, и разницу между «переносимо» и «работает у
    /// нас» иначе не увидеть.
    ///
    /// Он уже окупился: в текстовом режиме Windows переводит `\n` в `\r\n`, и
    /// файл, записанный `fopen(..., "w")`, оказывался на два байта длиннее.
    /// В FreeOS текстового режима нет вовсе — там эта ошибка невидима.
    ///
    /// **Пропускается, если хостовым компилятором собрать нечем**, и говорит об
    /// этом вслух. Молчаливый пропуск был бы хуже отсутствия проверки: он
    /// выглядит как пройденная.
    #[test]
    fn cdemo_behaves_the_same_on_the_host() {
        let Ok(clang) = llvm_tool("clang") else {
            eprintln!("ПРОПУЩЕНО: нет clang — проверить переносимость нечем");
            return;
        };
        let work = std::env::temp_dir().join("freeos-cdemo-host");
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).expect("каталог для сборки");

        let exe = work.join(if cfg!(windows) { "cdemo.exe" } else { "cdemo" });
        let built = Command::new(&clang)
            .arg("-O1")
            // Windows объявляет `strerror` устаревшей. Это её мнение о своей
            // библиотеке, а не о нашей программе.
            .arg("-D_CRT_SECURE_NO_WARNINGS")
            .arg("-o")
            .arg(&exe)
            .arg(libc_dir().join("examples/cdemo.c"))
            .status();
        match built {
            Ok(status) if status.success() => {}
            _ => {
                eprintln!(
                    "ПРОПУЩЕНО: хостовый компилятор не собрал cdemo \
                     (нет библиотеки C для этой платформы)"
                );
                return;
            }
        }

        let output = Command::new(&exe)
            .arg(&work)
            .output()
            .expect("собранная программа запускается");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("cdemo: done, 0 check(s) failed"),
            "на хосте программа не прошла свои же проверки:\n{text}"
        );
        assert!(output.status.success(), "код возврата не нулевой:\n{text}");
    }

    /// Каждое число в заголовке C равно одноимённой константе договора.
    ///
    /// Сверяется **в обе стороны** по именам, которые заголовок объявил: он
    /// вправе не объявлять вызов, который слою ОС не нужен, но не вправе
    /// объявить его с другим номером.
    #[test]
    fn c_header_matches_the_abi() {
        let text = std::fs::read_to_string(syscall_header()).expect("заголовок на месте");
        let found = defines(&text);
        assert!(
            found.len() > 30,
            "в заголовке нашлось всего {} определений — разбор сломался",
            found.len()
        );

        for (name, value) in found {
            // `FREEOS_` — наша приставка, чтобы имена не сталкивались с чужими
            // в программе, которая подключит и нас, и что-нибудь ещё. В
            // договоре её нет.
            let abi_name = name.strip_prefix("FREEOS_").unwrap_or(&name);
            let Some(expected) = abi_constant(abi_name) else {
                // Имя, которого в договоре нет вовсе, — либо опечатка, либо
                // выдумка. И то и другое надо заметить.
                panic!("`{name}` в заголовке C есть, а в `user-abi` такого имени нет");
            };
            assert_eq!(
                value, expected,
                "`{name}`: в заголовке C {value}, а в договоре {expected}"
            );
        }
    }

    /// Раскладка структур, которые пересекают границу вместе с C.
    ///
    /// Проверяется здесь же, потому что ошибка в ней выглядит точно так же, как
    /// ошибка в номере: программа получает числа не из тех полей и ведёт себя
    /// осмысленно ровно до первой проверки.
    #[test]
    fn c_structures_match_the_abi() {
        let text = std::fs::read_to_string(syscall_header()).expect("заголовок на месте");
        // Порядок полей в `struct freeos_stat` обязан повторять `user_abi::Stat`.
        let stat = text
            .split("struct freeos_stat {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("в заголовке есть struct freeos_stat");
        let fields: Vec<&str> = stat
            .lines()
            .filter_map(|line| line.trim().strip_suffix(';'))
            .filter_map(|line| line.split_whitespace().nth(1))
            .collect();
        assert_eq!(
            fields,
            vec!["size", "mode", "uid", "gid", "kind"],
            "поля `freeos_stat` разъехались с `user_abi::Stat`"
        );

        assert_eq!(size_of::<user_abi::Stat>(), 24);
        assert_eq!(core::mem::offset_of!(user_abi::Stat, size), 0);
        assert_eq!(core::mem::offset_of!(user_abi::Stat, mode), 8);
        assert_eq!(core::mem::offset_of!(user_abi::Stat, kind), 20);

        // Фаза 58b: каталоги, `poll` и сведения о потоке. Имена полей — те же,
        // что в договоре, по порядку; массив имени сверяется по имени до `[`.
        let fields_of = |name: &str| -> Vec<String> {
            let body = text
                .split(&format!("struct {name} {{"))
                .nth(1)
                .and_then(|rest| rest.split('}').next())
                .unwrap_or_else(|| panic!("в заголовке есть struct {name}"));
            body.lines()
                .filter_map(|line| line.trim().strip_suffix(';'))
                .filter_map(|line| line.split_whitespace().nth(1))
                .map(|field| field.split('[').next().unwrap_or(field).to_string())
                .collect()
        };
        assert_eq!(
            fields_of("freeos_dirent"),
            ["size", "mtime", "mode", "uid", "gid", "kind", "name_len", "name"],
            "поля `freeos_dirent` разъехались с `user_abi::Dirent`"
        );
        assert_eq!(core::mem::offset_of!(user_abi::Dirent, name_len), 28);
        assert_eq!(core::mem::offset_of!(user_abi::Dirent, name), 32);
        assert_eq!(fields_of("freeos_pollfd"), ["fd", "wanted", "ready"]);
        assert_eq!(size_of::<user_abi::PollFd>(), 16);
        assert_eq!(fields_of("freeos_thread_info"), ["id", "stack_low", "stack_high"]);
        assert_eq!(size_of::<user_abi::ThreadInfo>(), 24);
        // Фаза 60: `sysconf` берёт отсюда память и число процессоров.
        assert_eq!(
            fields_of("freeos_sysinfo"),
            [
                "uptime_ms", "ticks", "frames_total", "frames_free", "heap_size", "heap_free", "dma_total",
                "dma_used", "keys_posted", "keys_dropped", "pointer_moves", "pointer_merged",
                "frames_composed", "rects", "windows", "tasks_alive", "flags", "pixel_format", "screen_w",
                "screen_h", "cpus",
            ],
            "поля `freeos_sysinfo` разъехались с `user_abi::SysInfo`"
        );
        assert_eq!(size_of::<user_abi::SysInfo>(), 144);
    }

    /// Константа договора по её имени.
    ///
    /// Список, а не отражение: отражения в Rust нет, а макрос, порождающий этот
    /// список, скрыл бы ровно то, ради чего он существует, — что здесь
    /// перечислено **всё**, что заголовок вправе объявить.
    fn abi_constant(name: &str) -> Option<i64> {
        use user_abi as abi;
        let value: i64 = match name {
            "SYS_WRITE" => abi::SYS_WRITE as i64,
            "SYS_EXIT" => abi::SYS_EXIT as i64,
            "SYS_YIELD" => abi::SYS_YIELD as i64,
            "SYS_UPTIME" => abi::SYS_UPTIME as i64,
            "SYS_OPEN" => abi::SYS_OPEN as i64,
            "SYS_READ" => abi::SYS_READ as i64,
            "SYS_CLOSE" => abi::SYS_CLOSE as i64,
            "SYS_STAT" => abi::SYS_STAT as i64,
            "SYS_GETUID" => abi::SYS_GETUID as i64,
            "SYS_GETGID" => abi::SYS_GETGID as i64,
            "SYS_GETPID" => abi::SYS_GETPID as i64,
            "SYS_MKDIR" => abi::SYS_MKDIR as i64,
            "SYS_REMOVE" => abi::SYS_REMOVE as i64,
            "SYS_SEEK" => abi::SYS_SEEK as i64,
            "SYS_TIME" => abi::SYS_TIME as i64,
            "SYS_READDIR" => abi::SYS_READDIR as i64,
            "SYS_TTYMODE" => abi::SYS_TTYMODE as i64,
            "SYS_PIPE" => abi::SYS_PIPE as i64,
            "SYS_DUP" => abi::SYS_DUP as i64,
            "SYS_POLL" => abi::SYS_POLL as i64,
            "SYS_RENAME" => abi::SYS_RENAME as i64,
            "SYS_SPAWN" => abi::SYS_SPAWN as i64,
            "SYS_WAIT" => abi::SYS_WAIT as i64,
            "SYS_CREATE" => abi::SYS_CREATE as i64,
            "SYS_RANDOM" => abi::SYS_RANDOM as i64,
            "SYS_MMAP" => abi::SYS_MMAP as i64,
            "SYS_MUNMAP" => abi::SYS_MUNMAP as i64,
            "SYS_MMAP_FILE" => abi::SYS_MMAP_FILE as i64,
            "SYS_FSTAT" => abi::SYS_FSTAT as i64,
            "SYS_ISATTY" => abi::SYS_ISATTY as i64,
            "SYS_CLOCK" => abi::SYS_CLOCK as i64,
            "SYS_NANOSLEEP" => abi::SYS_NANOSLEEP as i64,
            "SYS_TIMES" => abi::SYS_TIMES as i64,
            "SYS_SYSINFO" => abi::SYS_SYSINFO as i64,
            "SYS_THREAD_CREATE" => abi::SYS_THREAD_CREATE as i64,
            "SYS_SET_TLS" => abi::SYS_SET_TLS as i64,
            "SYS_THREAD_EXIT" => abi::SYS_THREAD_EXIT as i64,
            "SYS_FUTEX_WAIT" => abi::SYS_FUTEX_WAIT as i64,
            "SYS_FUTEX_WAKE" => abi::SYS_FUTEX_WAKE as i64,
            "SYS_MPROTECT" => abi::SYS_MPROTECT as i64,
            "SYS_THREAD_INFO" => abi::SYS_THREAD_INFO as i64,

            "MAP_LAZY" => abi::MAP_LAZY as i64,
            "MAP_JIT" => abi::MAP_JIT as i64,

            // `usize::MAX` в договоре — минус единица в регистре.
            "DUP_ANY" => abi::DUP_ANY as i64,
            "POLL_IN" => i64::from(abi::POLL_IN),
            "POLL_OUT" => i64::from(abi::POLL_OUT),
            "POLL_HUP" => i64::from(abi::POLL_HUP),
            "POLL_BAD" => i64::from(abi::POLL_BAD),
            "POLL_FOREVER" => abi::POLL_FOREVER,
            "TTY_LINE" => abi::TTY_LINE as i64,
            "TTY_RAW" => abi::TTY_RAW as i64,
            "MAX_OPEN_FILES" => abi::MAX_OPEN_FILES as i64,
            "MAX_NAME" => abi::MAX_NAME as i64,

            "FUTEX_CHANGED" => abi::FUTEX_CHANGED,
            "FUTEX_TIMED_OUT" => abi::FUTEX_TIMED_OUT,
            "PROCESS_PAGE" => abi::PROCESS_PAGE as i64,
            "PROCESS_PAGE_TLS" => abi::PROCESS_PAGE_TLS as i64,

            "FD_STDIN" => abi::FD_STDIN as i64,
            "FD_STDOUT" => abi::FD_STDOUT as i64,
            "FD_STDERR" => abi::FD_STDERR as i64,

            "O_WRITE" => abi::O_WRITE as i64,
            "O_CREATE" => abi::O_CREATE as i64,
            "O_TRUNC" => abi::O_TRUNC as i64,

            "SEEK_SET" => abi::SEEK_SET as i64,
            "SEEK_CUR" => abi::SEEK_CUR as i64,
            "SEEK_END" => abi::SEEK_END as i64,

            "KIND_FILE" => abi::KIND_FILE as i64,
            "KIND_DIRECTORY" => abi::KIND_DIRECTORY as i64,
            "KIND_PIPE" => abi::KIND_PIPE as i64,
            "KIND_TERMINAL" => abi::KIND_TERMINAL as i64,

            "SPAWN_INHERIT" => abi::SPAWN_INHERIT as i64,

            "CLOCK_REALTIME" => abi::CLOCK_REALTIME as i64,
            "CLOCK_MONOTONIC" => abi::CLOCK_MONOTONIC as i64,

            "ERR_NO_SYSCALL" => abi::ERR_NO_SYSCALL,
            "ERR_BAD_ADDRESS" => abi::ERR_BAD_ADDRESS,
            "ERR_NOT_FOUND" => abi::ERR_NOT_FOUND,
            "ERR_PERMISSION" => abi::ERR_PERMISSION,
            "ERR_BAD_FD" => abi::ERR_BAD_FD,
            "ERR_TOO_MANY_FILES" => abi::ERR_TOO_MANY_FILES,
            "ERR_IO" => abi::ERR_IO,
            "ERR_UNSUPPORTED" => abi::ERR_UNSUPPORTED,
            "ERR_NO_FILESYSTEM" => abi::ERR_NO_FILESYSTEM,
            "ERR_BAD_PATH" => abi::ERR_BAD_PATH,
            "ERR_NO_PROGRAM" => abi::ERR_NO_PROGRAM,
            "ERR_EXISTS" => abi::ERR_EXISTS,
            "ERR_NOT_EMPTY" => abi::ERR_NOT_EMPTY,
            "ERR_NO_SPACE" => abi::ERR_NO_SPACE,
            "ERR_AGAIN" => abi::ERR_AGAIN,
            "ERR_BROKEN_PIPE" => abi::ERR_BROKEN_PIPE,
            "ERR_LIMIT" => abi::ERR_LIMIT,

            _ => return None,
        };
        Some(value)
    }
}
