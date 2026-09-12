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
}

/// Программы на C, которые едут в `/bin`.
pub const C_PROGRAMS: [CProgram; 2] = [
    CProgram { name: "cdemo", needs: None, libs: &[] },
    // Чужая библиотека, собранная нашим набором. Она здесь не ради сжатия: это
    // единственная проверка, доказывающая, что код, вышедший из чужого
    // `configure`, **работает**, а не только собрался. См. `libc/examples/zdemo.c`.
    CProgram { name: "zdemo", needs: Some("libz.a"), libs: &["libz.a"] },
];

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

        let cross = write_cross_file(arch)?;
        let build = root.join(format!("pico-{}", arch.name()));
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
/// * `thread-local-storage=false`, `single-thread=true`, `newlib-global-errno=true`
///   — потоков в этой системе нет. **Это не экономия, а исправление
///   настоящего отказа**: с TLS по умолчанию первая же ошибка роняла программу
///   отказом страницы по адресу `-4` — `errno` лежит в блоке, которого без
///   `_init_tls` не существует, а наш компоновочный сценарий его и не заводит;
/// * `multilib=false`, `tests=false` — вариантов сборки у нас один, а тесты
///   picolibc требуют запускать программы на хосте.
const MESON_OPTIONS: [&str; 7] = [
    "-Dsemihost=false",
    "-Dposix-console=true",
    "-Dpicocrt=false",
    "-Dthread-local-storage=false",
    "-Dsingle-thread=true",
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
fn write_cross_file(arch: Arch) -> Result<PathBuf> {
    let root = toolchain_dir();
    let cross_dir = root.join("cross");
    std::fs::create_dir_all(&cross_dir)?;
    let path = cross_dir.join(format!("{}.txt", arch.name()));

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

    let includes = vec![sysroot.join("include"), libc_dir().join("freeos")];
    let work = paths::workspace_root()
        .join("build/toolchain/cbuild")
        .join(arch.name());

    // Слой ОС и стартовый код собираются один раз на архитектуру: они одни и те
    // же для всех программ.
    let mut common = Vec::new();
    for name in ["crt0", "syscalls"] {
        let source = libc_dir().join("freeos").join(format!("{name}.c"));
        let object = work.join(format!("{name}.o"));
        compile(arch, &source, &object, &includes)?;
        common.push(object);
    }

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
        compile(arch, &source, &object, &includes)?;

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
            let Ok(number) = value.parse::<i64>() else {
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
            "SYS_RENAME" => abi::SYS_RENAME as i64,
            "SYS_CREATE" => abi::SYS_CREATE as i64,
            "SYS_RANDOM" => abi::SYS_RANDOM as i64,
            "SYS_MMAP" => abi::SYS_MMAP as i64,
            "SYS_MUNMAP" => abi::SYS_MUNMAP as i64,
            "SYS_FSTAT" => abi::SYS_FSTAT as i64,
            "SYS_ISATTY" => abi::SYS_ISATTY as i64,
            "SYS_CLOCK" => abi::SYS_CLOCK as i64,
            "SYS_NANOSLEEP" => abi::SYS_NANOSLEEP as i64,
            "SYS_TIMES" => abi::SYS_TIMES as i64,

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
