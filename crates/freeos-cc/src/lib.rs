//! Набор для сборки под FreeOS: правила компиляции C и обёртка `cc`.
//!
//! # Зачем обёртка, если есть clang
//!
//! Потому что чужой проект не спрашивает, чем его собирают. `./configure`
//! запускает `$CC` с флагами, которые придумал сам, `make` — тоже, и ни один из
//! них не подставит ни `--target`, ни `-nostdinc`, ни наш компоновочный
//! сценарий. Единственное место, куда можно положить знание о целевой
//! системе, — это сам `cc`. Поэтому набор состоит не из «списка флагов в
//! документации», а из программы `x86_64-freeos-cc`, которая ведёт себя как
//! обычный компилятор и знает про FreeOS всё.
//!
//! # Почему компоновка идёт мимо clang
//!
//! У x86-64 нет bare-metal драйвера (см. [`triple`]), и любой голый триплет
//! зовёт для компоновки `gcc`. Поэтому обёртка в режиме компоновки собирает
//! объектники clang'ом, а связывает их `ld.lld` напрямую — своим сценарием,
//! тем же, по которому живут программы на Rust.
//!
//! # Что обёртка обязана уметь, кроме сборки
//!
//! Отвечать на расспросы `configure`: `-dumpmachine`, `--version`, `-E`, `-c`,
//! `-print-file-name`. Это не украшение — на каждом из них чужой скрипт
//! принимает решение, и молчание в ответ выглядит как «компилятор сломан».

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Архитектура, под которую собирается C.
///
/// Своя, а не заимствованная у `xtask`: этот крейт зовут и оттуда, и из
/// обёртки, которая про `xtask` ничего не знает.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    X86_64,
    Aarch64,
}

impl Target {
    /// Все цели, которые набор умеет собирать.
    pub const ALL: [Target; 2] = [Target::X86_64, Target::Aarch64];

    /// Имя архитектуры так, как оно пишется в путях и в имени обёртки.
    pub fn name(self) -> &'static str {
        match self {
            Target::X86_64 => "x86_64",
            Target::Aarch64 => "aarch64",
        }
    }

    /// Разобрать имя архитектуры. `None` — имя чужое.
    pub fn from_name(name: &str) -> Option<Target> {
        match name {
            "x86_64" => Some(Target::X86_64),
            "aarch64" => Some(Target::Aarch64),
            _ => None,
        }
    }

    /// Триплет набора: то, что чужой проект увидит в `--host` и `-dumpmachine`.
    ///
    /// Он **не** равен триплету clang (см. [`triple`]) и равен не может: тот
    /// выбран из-за особенностей драйвера, а этот — имя нашей системы, по
    /// которому чужой `configure` её и опознает.
    pub fn sdk_triple(self) -> &'static str {
        match self {
            Target::X86_64 => "x86_64-freeos",
            Target::Aarch64 => "aarch64-freeos",
        }
    }
}

/// Триплет, который получает clang.
///
/// # Почему у двух архитектур он разной природы
///
/// У AArch64 всё как ожидается: `aarch64-unknown-none-elf` — «свободностоящая
/// ELF-мишень без операционной системы», и драйвер clang знает про неё всё.
///
/// У x86-64 такого драйвера **нет**. Любой голый триплет попадает в общий
/// GCC-совместимый драйвер, а тот зовёт для компоновки `gcc` и передаёт
/// `-fuse-ld=lld` ему же. На машине без gcc это «unable to execute command».
/// Поэтому у x86-64 стоит триплет Linux — единственный, чей драйвер умеет lld, —
/// а линуксовость снимается с препроцессора явно ([`compile_flags`]).
pub fn triple(target: Target) -> &'static str {
    match target {
        Target::X86_64 => "x86_64-unknown-linux-elf",
        Target::Aarch64 => "aarch64-unknown-none-elf",
    }
}

/// Макросы, которые триплет Linux принёс с собой и которые надо снять.
///
/// Снять обязательно: picolibc и всякая портируемая программа смотрят именно на
/// них, решая, есть ли под ними Linux. Под нами его нет, и программа, поверившая
/// в обратное, зовёт `syscall()` с линуксовыми номерами.
pub const LINUX_MACROS: [&str; 7] = [
    "__linux__",
    "__linux",
    "linux",
    "__gnu_linux__",
    "__unix__",
    "__unix",
    "unix",
];

/// Флаги компиляции, общие для всего, что собирается под FreeOS.
///
/// `-ffreestanding` — стандартной библиотеки под нами может не быть вовсе;
/// `-fno-stack-protector` — защита стека зовёт `__stack_chk_fail`, которого
/// неоткуда взять; `-mcmodel=large` на x86-64 — программа живёт по адресу
/// 512 ГиБ, а малая модель кода рассчитана на первые два гигабайта, и обращение
/// к своим же данным не дотягивается.
///
/// `__freeos__` объявляется здесь же и намеренно: чужому коду нужно чем-то нас
/// опознать, а `#ifdef __linux__` мы у него отняли.
pub fn compile_flags(target: Target) -> Vec<String> {
    let mut flags = vec![
        format!("--target={}", triple(target)),
        "-ffreestanding".into(),
        "-fno-stack-protector".into(),
        "-fno-pic".into(),
        "-fno-PIE".into(),
        "-D__freeos__=1".into(),
        "-D__FreeOS__=1".into(),
    ];
    if target == Target::X86_64 {
        flags.push("-mcmodel=large".into());
        for macro_name in LINUX_MACROS {
            flags.push(format!("-U{macro_name}"));
        }
    }
    flags
}

/// Где стоит LLVM, если его нет на PATH.
///
/// winget ставит LLVM машинно, а PATH пользователя обновляется только в новых
/// терминалах: без этого пути набор «пропадает» после установки до перезахода.
const LLVM_FALLBACK: &str = r"C:\Program Files\LLVM\bin";

/// Найти инструмент набора: сначала на PATH, потом там, куда его кладёт winget.
pub fn find_tool(name: &str) -> Result<PathBuf, String> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let fallback = Path::new(LLVM_FALLBACK).join(&exe);
    if fallback.is_file() {
        return Ok(fallback);
    }
    Err(format!(
        "не найден {name}: нет ни на PATH, ни в {LLVM_FALLBACK}.\n\
         Поставить набор: winget install --id LLVM.LLVM --exact"
    ))
}

/// Каталог заголовков самого компилятора: `<корень LLVM>/lib/clang/<версия>/include`.
///
/// Оттуда берутся `stddef.h`, `stdarg.h`, `stdint.h` — то, что обязан поставлять
/// **компилятор**, а не библиотека; picolibc их и не поставляет. Ищется по
/// дереву, а не собирается из номера версии: номер меняется с каждым
/// обновлением LLVM, и зашитый в код он сломал бы сборку ровно тогда, когда её
/// никто не трогал.
pub fn clang_builtin_includes(clang: &Path) -> Option<PathBuf> {
    let root = clang.parent()?.parent()?;
    let versions = std::fs::read_dir(root.join("lib/clang")).ok()?;
    let mut best: Option<PathBuf> = None;
    for entry in versions.flatten() {
        let candidate = entry.path().join("include");
        if candidate.join("stddef.h").is_file() {
            best = Some(candidate);
        }
    }
    best
}

/// Разбор имени, под которым обёртку позвали.
///
/// Имя — это `<арх>-freeos-<инструмент>`: `x86_64-freeos-cc`,
/// `aarch64-freeos-ar`. Архитектура берётся **из имени**, а не из флага, потому
/// что чужой `Makefile` подставляет `$(CC)` целиком и добавить к нему ничего не
/// может.
pub fn parse_program_name(argv0: &str) -> Option<(Target, String)> {
    let base = Path::new(argv0)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(argv0);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    let (arch, tool) = base.split_once("-freeos-")?;
    let target = Target::from_name(arch)?;
    Some((target, tool.to_string()))
}

/// Инструменты, которые обёртка просто переименовывает.
///
/// Чужой `configure` зовёт `$AR`, `$RANLIB`, `$STRIP` по имени с приставкой
/// триплета и обижается, если их нет. Ничего своего им передавать не надо —
/// архив есть архив, — поэтому здесь только таблица имён.
fn plain_tool(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "ar" => "llvm-ar",
        "ranlib" => "llvm-ranlib",
        "nm" => "llvm-nm",
        "strip" => "llvm-strip",
        "objcopy" => "llvm-objcopy",
        "objdump" => "llvm-objdump",
        "readelf" => "llvm-readelf",
        "size" => "llvm-size",
        "ld" => "ld.lld",
        _ => return None,
    })
}

/// Корень набора: каталог, в котором лежат `include` и `lib` для этой цели.
///
/// Ищется **относительно самой обёртки** (`bin/../sysroot/<арх>`), а не по
/// зашитому пути: набор — это каталог, который можно перенести, и путь,
/// записанный при сборке, верен на одной машине. Переменная `FREEOS_SYSROOT`
/// перебивает поиск — ею пользуется `xtask`, когда собирает из дерева.
pub fn sysroot(target: Target) -> Result<PathBuf, String> {
    if let Some(from_env) = std::env::var_os("FREEOS_SYSROOT") {
        return Ok(PathBuf::from(from_env).join(target.name()));
    }
    let exe = std::env::current_exe().map_err(|err| format!("не найден путь обёртки: {err}"))?;
    let bin = exe
        .parent()
        .ok_or_else(|| String::from("у обёртки нет каталога"))?;
    let root = bin
        .parent()
        .ok_or_else(|| String::from("обёртка лежит в корне тома"))?;
    Ok(root.join("sysroot").join(target.name()))
}

/// Что обёртку попросили сделать.
///
/// Различать обязательно: в режиме компиляции нельзя добавлять компоновочные
/// флаги (clang ругнётся на неиспользованный аргумент, а с `-Werror` у чужого
/// проекта это провалит **каждую** проверку `configure`), а в режиме компоновки
/// нельзя отдавать управление драйверу clang.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// `-c`, `-S`, `-E`, `-M` — clang зовётся напрямую, компоновки нет.
    CompileOnly,
    /// Ни одного из них: надо собрать и связать.
    Link,
}

/// Флаги, после которых идёт отдельным словом их значение.
///
/// Список нужен, чтобы не принять значение за имя входного файла: `-o hello`
/// это вывод, а не исходник, и `-include foo.h` — не то, что надо компилировать.
const FLAGS_WITH_VALUE: [&str; 12] = [
    "-o",
    "-I",
    "-L",
    "-D",
    "-U",
    "-include",
    "-isystem",
    "-idirafter",
    "-iquote",
    "-MF",
    "-MT",
    "-MQ",
];

/// Разобранная командная строка.
#[derive(Debug, Default)]
pub struct Invocation {
    /// Аргументы, которые уйдут clang'у при компиляции.
    pub compile: Vec<String>,
    /// Входные файлы и библиотеки — **в исходном порядке**.
    ///
    /// Порядок не переставляется намеренно: компоновщик идёт по архивам слева
    /// направо и берёт из каждого только то, чего ещё не хватает. Переставленный
    /// `-lz` не даст ошибки разбора — он даст «неизвестный символ `deflate`».
    pub inputs: Vec<Input>,
    /// Что просили на выходе.
    pub output: Option<String>,
    /// Аргументы, адресованные компоновщику (`-Wl,...` и `-L`).
    pub link: Vec<String>,
}

/// Одна позиция во входной части командной строки.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Input {
    /// Исходник на C или на ассемблере: его надо сначала скомпилировать.
    Source(String),
    /// Готовый объектник или архив: уходит компоновщику как есть.
    Object(String),
    /// `-lname`: уходит компоновщику как есть.
    Library(String),
}

/// Разобрать аргументы `cc`.
///
/// Разбор нарочно консервативный: всё незнакомое считается флагом компиляции и
/// уезжает clang'у. Ошибиться в другую сторону дороже — незнакомый флаг,
/// принятый за имя файла, превращается в «нет такого файла» посреди чужой
/// сборки, и виноватым выглядит проект, а не набор.
pub fn parse_arguments(args: &[String]) -> (Mode, Invocation) {
    let mut mode = Mode::Link;
    let mut inv = Invocation::default();
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].clone();
        index += 1;

        if matches!(arg.as_str(), "-c" | "-S" | "-E" | "-M" | "-MM") {
            mode = Mode::CompileOnly;
            inv.compile.push(arg);
            continue;
        }
        if arg == "-o" {
            if let Some(value) = args.get(index) {
                inv.output = Some(value.clone());
                index += 1;
            }
            continue;
        }
        if let Some(value) = arg.strip_prefix("-o") {
            if !value.is_empty() {
                inv.output = Some(value.to_string());
                continue;
            }
        }
        if let Some(rest) = arg.strip_prefix("-Wl,") {
            for piece in rest.split(',') {
                inv.link.push(piece.to_string());
            }
            continue;
        }
        if arg.starts_with("-l") && arg.len() > 2 {
            inv.inputs.push(Input::Library(arg));
            continue;
        }
        if arg == "-l" {
            if let Some(value) = args.get(index) {
                inv.inputs.push(Input::Library(format!("-l{value}")));
                index += 1;
            }
            continue;
        }
        if let Some(dir) = arg.strip_prefix("-L") {
            if dir.is_empty() {
                if let Some(value) = args.get(index) {
                    inv.link.push(format!("-L{value}"));
                    index += 1;
                }
            } else {
                inv.link.push(arg.clone());
            }
            continue;
        }
        if FLAGS_WITH_VALUE.contains(&arg.as_str()) {
            inv.compile.push(arg);
            if let Some(value) = args.get(index) {
                inv.compile.push(value.clone());
                index += 1;
            }
            continue;
        }
        if arg.starts_with('-') {
            inv.compile.push(arg);
            continue;
        }
        match classify_file(&arg) {
            Some(input) => inv.inputs.push(input),
            // Файл неизвестного расширения. Компоновщику он всё равно понятнее,
            // чем компилятору: чаще всего это `.lo`, `.obj` или архив с чужим
            // именем.
            None => inv.inputs.push(Input::Object(arg)),
        }
    }
    (mode, inv)
}

/// Что это за файл по его имени.
fn classify_file(name: &str) -> Option<Input> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".c") || lower.ends_with(".s") {
        return Some(Input::Source(name.to_string()));
    }
    if lower.ends_with(".o") || lower.ends_with(".a") || lower.ends_with(".obj") {
        return Some(Input::Object(name.to_string()));
    }
    None
}

/// Чем именно интересуется чужой скрипт, если собирать нечего.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Query {
    /// Готовый ответ и код возврата.
    Answer(String, i32),
    /// Спросить у clang и отдать его ответ как есть.
    ///
    /// Так отвечает `-v`, и это не лень. `configure` смотрит на вывод `-v`,
    /// чтобы понять, **какой** это компилятор, и от ответа зависит весь
    /// дальнейший набор флагов. Придуманная нами строка была бы враньём в обе
    /// стороны: скажи «gcc» — получишь флаги, которых у нас нет; промолчи —
    /// получишь «неизвестный компилятор», а это, как выяснилось на zlib, ветка,
    /// в которой `configure` теряет имя компилятора и зовёт голый `cc`.
    /// Настоящий ответ настоящего clang честен и работает.
    AskClang,
}

/// Ответы на расспросы `configure`.
///
/// Считается расспросом только вызов **без единого входного файла**: `-v` рядом
/// с исходником означает «собери и покажи подробности», и подменять этот случай
/// печатью версии значило бы молча ничего не собрать.
///
/// Почему это вообще нужно: `configure` часто зовёт компилятор только ради
/// `-dumpmachine`, `--version` или `-v`. Обёртка, попытавшаяся на это
/// что-нибудь собрать, вернула бы ошибку, и чужой скрипт решил бы, что
/// компилятора нет.
pub fn answer_query(target: Target, args: &[String]) -> Option<Query> {
    let (_, inv) = parse_arguments(args);
    if !inv.inputs.is_empty() {
        return None;
    }
    for arg in args {
        match arg.as_str() {
            "-dumpmachine" => {
                return Some(Query::Answer(format!("{}\n", target.sdk_triple()), 0));
            }
            "-dumpversion" => {
                return Some(Query::Answer(format!("{}\n", env!("CARGO_PKG_VERSION")), 0));
            }
            "--version" | "-version" => {
                return Some(Query::Answer(
                    format!(
                        "{} (FreeOS SDK {}) clang-based\n",
                        target.sdk_triple(),
                        env!("CARGO_PKG_VERSION")
                    ),
                    0,
                ));
            }
            "-v" | "-###" | "--help" => return Some(Query::AskClang),
            _ => {}
        }
    }
    None
}

/// Собрать строку вызова clang для компиляции.
///
/// `-nostdinc` обязателен: без него clang подставляет **свои** заголовки, и
/// программа собирается против чужого `stdio.h`, а линкуется с нашей libc.
/// Расхождение при этом не обязано быть ошибкой сборки — оно бывает разной
/// раскладкой `FILE`, то есть падением на первом же `printf`.
pub fn compile_command(target: Target, sysroot: &Path, clang: &Path) -> Command {
    let mut cmd = Command::new(clang);
    cmd.args(compile_flags(target));
    cmd.arg("-nostdinc");
    cmd.arg(format!("-isystem{}", sysroot.join("include").display()));
    if let Some(builtin) = clang_builtin_includes(clang) {
        cmd.arg(format!("-idirafter{}", builtin.display()));
    }
    cmd
}

/// Библиотеки набора, которые дописываются к любой компоновке.
///
/// Порядок не произволен: сначала наш слой ОС (он зовёт libc), потом libc (она
/// зовёт вспомогательные подпрограммы компилятора), потом они. Компоновщик идёт
/// слева направо, и переставленный список кончается «неизвестным символом» из
/// чужого архива.
const SDK_LIBRARIES: [&str; 3] = ["-lfreeos", "-lc", "-lclang_rt.builtins"];

/// Точка входа обёртки. Возвращает код, с которым надо выйти.
pub fn run(argv: Vec<String>) -> i32 {
    let argv0 = argv.first().cloned().unwrap_or_default();
    let args: Vec<String> = argv.into_iter().skip(1).collect();

    let Some((target, tool)) = parse_program_name(&argv0) else {
        eprintln!(
            "freeos-cc: непонятно, чем меня позвали ({argv0}).\n\
             Имя обязано быть вида <арх>-freeos-<инструмент>, например x86_64-freeos-cc."
        );
        return 2;
    };

    if let Some(real) = plain_tool(&tool) {
        return match find_tool(real) {
            Ok(path) => forward(&path, &args),
            Err(err) => {
                eprintln!("{argv0}: {err}");
                2
            }
        };
    }
    if tool != "cc" && tool != "gcc" && tool != "clang" {
        eprintln!("{argv0}: инструмент `{tool}` набор не поставляет");
        return 2;
    }

    match answer_query(target, &args) {
        Some(Query::Answer(text, code)) => {
            print!("{text}");
            return code;
        }
        Some(Query::AskClang) => {
            return match find_tool("clang") {
                Ok(clang) => {
                    let mut all = compile_flags(target);
                    all.extend(args);
                    forward(&clang, &all)
                }
                Err(err) => {
                    eprintln!("{argv0}: {err}");
                    2
                }
            };
        }
        None => {}
    }

    match compile_and_link(target, &args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{argv0}: {err}");
            1
        }
    }
}

/// Позвать чужой инструмент, ничего не меняя.
fn forward(path: &Path, args: &[String]) -> i32 {
    match Command::new(path).args(args).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            eprintln!("не удалось запустить {}: {err}", path.display());
            2
        }
    }
}

fn compile_and_link(target: Target, args: &[String]) -> Result<i32, String> {
    let clang = find_tool("clang")?;
    let sysroot = sysroot(target)?;
    if !sysroot.join("include/stdio.h").is_file() {
        return Err(format!(
            "набор не установлен: нет {}\n\
             Собрать: cargo xtask sdk",
            sysroot.join("include/stdio.h").display()
        ));
    }
    let (mode, inv) = parse_arguments(args);

    if mode == Mode::CompileOnly {
        let mut cmd = compile_command(target, &sysroot, &clang);
        cmd.args(&inv.compile);
        for input in &inv.inputs {
            match input {
                Input::Source(path) | Input::Object(path) => cmd.arg(path),
                Input::Library(flag) => cmd.arg(flag),
            };
        }
        if let Some(output) = &inv.output {
            cmd.arg("-o").arg(output);
        }
        return Ok(status_of(cmd));
    }

    // Компоновка. Каждый исходник сначала становится объектником во временном
    // каталоге: имя ему даётся по порядковому номеру, а не по исходному, потому
    // что два файла с одинаковым именем в разных каталогах — обычное дело в
    // чужом проекте, и совпадение стёрло бы один другим.
    let work = temp_dir()?;
    let mut objects: Vec<OsString> = Vec::new();
    let mut number = 0usize;
    for input in &inv.inputs {
        match input {
            Input::Source(path) => {
                number += 1;
                let object = work.join(format!("in{number}.o"));
                let mut cmd = compile_command(target, &sysroot, &clang);
                cmd.args(&inv.compile);
                cmd.arg("-c").arg(path).arg("-o").arg(&object);
                let code = status_of(cmd);
                if code != 0 {
                    let _ = std::fs::remove_dir_all(&work);
                    return Ok(code);
                }
                objects.push(object.into_os_string());
            }
            Input::Object(path) => objects.push(OsString::from(path)),
            Input::Library(flag) => objects.push(OsString::from(flag)),
        }
    }

    let lld = find_tool("ld.lld")?;
    let lib = sysroot.join("lib");
    let script = lib.join("freeos.ld");
    if !script.is_file() {
        let _ = std::fs::remove_dir_all(&work);
        return Err(format!(
            "нет компоновочного сценария: {}\nСобрать: cargo xtask sdk",
            script.display()
        ));
    }
    let output = inv.output.clone().unwrap_or_else(|| String::from("a.out"));

    let mut cmd = Command::new(&lld);
    cmd.arg("-T").arg(&script);
    // Страница у нас четыре килобайта; lld по умолчанию выравнивает сегменты на
    // 64 КиБ и раздувает образ до восьми раз.
    cmd.arg("-z").arg("max-page-size=0x1000");
    // Отладочную информацию выбрасываем здесь же, а не отдельным шагом: ядро
    // читает файл программы целиком в кучу, прежде чем разобрать заголовки, и
    // сегмент, уехавший за предел чтения, выглядел бы как испорченный файл.
    cmd.arg("--strip-debug");
    cmd.arg("-o").arg(&output);
    // Стартовый код — первым и всегда: точка входа программы живёт в нём.
    cmd.arg(lib.join("crt0.o"));
    cmd.args(&objects);
    cmd.args(&inv.link);
    cmd.arg(format!("-L{}", lib.display()));
    cmd.args(SDK_LIBRARIES);
    let code = status_of(cmd);
    let _ = std::fs::remove_dir_all(&work);
    Ok(code)
}

fn status_of(mut cmd: Command) -> i32 {
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(err) => {
            eprintln!("не удалось запустить инструмент набора: {err}");
            2
        }
    }
}

/// Временный каталог под объектники одной компоновки.
///
/// В имени — номер процесса: `make -j` запускает обёртку многократно
/// одновременно, и общий каталог означал бы, что две сборки затирают друг другу
/// `in1.o`. Отказ такой гонки выглядит как «повреждённый объектник» и
/// воспроизводится раз из десяти.
fn temp_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("freeos-cc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|err| format!("не создать {}: {err}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn program_name_carries_the_architecture() {
        assert_eq!(
            parse_program_name("x86_64-freeos-cc"),
            Some((Target::X86_64, String::from("cc")))
        );
        assert_eq!(
            parse_program_name(r"C:\sdk\bin\aarch64-freeos-ar.exe"),
            Some((Target::Aarch64, String::from("ar")))
        );
        assert_eq!(parse_program_name("clang"), None);
        assert_eq!(parse_program_name("riscv64-freeos-cc"), None);
    }

    /// `-c` переводит обёртку в режим компиляции, и компоновочные флаги в этот
    /// вызов попасть не должны.
    #[test]
    fn dash_c_means_compile_only() {
        let (mode, inv) = parse_arguments(&words("-O2 -c foo.c -o foo.o"));
        assert_eq!(mode, Mode::CompileOnly);
        assert_eq!(inv.output.as_deref(), Some("foo.o"));
        assert_eq!(inv.inputs, vec![Input::Source(String::from("foo.c"))]);
    }

    /// Порядок входов сохраняется: объектник, библиотека, снова объектник.
    ///
    /// Ровно это ломается молча — переставленный `-lz` даёт «неизвестный символ
    /// `deflate`», а не ошибку разбора.
    #[test]
    fn input_order_is_preserved() {
        let (mode, inv) = parse_arguments(&words("a.o -lz b.o -lm -o app"));
        assert_eq!(mode, Mode::Link);
        assert_eq!(inv.output.as_deref(), Some("app"));
        assert_eq!(
            inv.inputs,
            vec![
                Input::Object(String::from("a.o")),
                Input::Library(String::from("-lz")),
                Input::Object(String::from("b.o")),
                Input::Library(String::from("-lm")),
            ]
        );
    }

    /// `-Wl,` разбирается по запятым, а `-L` уезжает компоновщику, а не clang'у.
    ///
    /// Второе куплено фазой 45: `-L` в строке компилятора валит **каждую**
    /// проверку meson, потому что тот проверяет флаги компиляцией с
    /// `-Werror=unused-command-line-argument`.
    #[test]
    fn linker_arguments_are_separated() {
        let (_, inv) = parse_arguments(&words("-L/opt/lib -Wl,--gc-sections,-z,now foo.c"));
        assert_eq!(inv.link, vec![
            "-L/opt/lib",
            "--gc-sections",
            "-z",
            "now"
        ]);
        assert!(inv.compile.is_empty(), "лишнее уехало компилятору: {:?}", inv.compile);
    }

    /// Значение отдельным словом не принимается за имя исходника.
    #[test]
    fn flag_values_are_not_inputs() {
        let (_, inv) = parse_arguments(&words("-include config.h -isystem /a/b -D FOO=1 x.c"));
        assert_eq!(inv.inputs, vec![Input::Source(String::from("x.c"))]);
        assert!(inv.compile.contains(&String::from("config.h")));
        assert!(inv.compile.contains(&String::from("FOO=1")));
    }

    /// `-dumpmachine` отвечает именем нашей системы, а не триплетом clang.
    ///
    /// Разница существенная: по этой строке чужой `configure` решает, под что
    /// собирает, и `x86_64-unknown-linux-elf` научил бы его звать линуксовые
    /// системные вызовы.
    #[test]
    fn dumpmachine_names_freeos() {
        let answer = answer_query(Target::X86_64, &words("-dumpmachine")).expect("ответ");
        assert_eq!(answer, Query::Answer(String::from("x86_64-freeos\n"), 0));
        assert!(answer_query(Target::X86_64, &words("-c foo.c")).is_none());
    }

    /// `-v` в одиночку — вопрос, `-v` рядом с исходником — просьба собрать
    /// подробно. Купленo красной сборкой zlib: `configure` смотрит на вывод
    /// `-v`, чтобы опознать компилятор, и молчание в ответ уводит его в ветку,
    /// где имя компилятора теряется и зовётся голый `cc`.
    #[test]
    fn dash_v_alone_is_a_question() {
        assert_eq!(
            answer_query(Target::X86_64, &words("-v")),
            Some(Query::AskClang)
        );
        assert_eq!(answer_query(Target::X86_64, &words("-v -c foo.c")), None);
    }

    /// Триплет clang и триплет набора — разные строки, и это намеренно.
    #[test]
    fn clang_triple_differs_from_the_sdk_triple() {
        assert_ne!(triple(Target::X86_64), Target::X86_64.sdk_triple());
        assert!(compile_flags(Target::X86_64).contains(&String::from("-U__linux__")));
        assert!(!compile_flags(Target::Aarch64).contains(&String::from("-U__linux__")));
    }
}
