//! `cargo xtask clr-check` — наш разбор сборок .NET против чужого (фаза N1).
//!
//! # Что сверяется
//!
//! Одна и та же сборка печатается дважды: крейтом `clr-meta` и программой
//! `tools/dotnet/clrdump` поверх `System.Reflection.Metadata` от Microsoft.
//! Текст — число строк, размер строки и смещение каждой таблицы, все типы,
//! поля, методы с сигнатурами, байтами IL и обработчиками исключений, ссылки на
//! члены и сборки, атрибуты, строки программы. Совпасть обязано всё до символа.
//!
//! Размер строки и смещение таблицы — самая строгая часть: ширина столбца
//! зависит от размеров чужих таблиц и куч, и ошибка в одном столбце одной
//! таблицы сдвигает все таблицы после неё. Здесь она видна сразу, а не через
//! фазу, когда интерпретатор возьмёт не тот метод.
//!
//! # На чём
//!
//! Две пробные программы (`tools/dotnet/samples`) и настоящие сборки
//! установленного .NET: `System.Private.CoreLib` (десятки тысяч методов, образ
//! ReadyToRun), `System.Runtime`, `System.Console` и, если стоит Windows
//! Desktop Runtime, `System.Windows.Forms` и `System.Drawing.Common` — ради них
//! веха и затеяна.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clr_meta::body::{CLAUSE_CATCH, CLAUSE_FILTER};
use clr_meta::tables::{TABLE_COUNT, id};
use clr_meta::{Assembly, Coded, Token};

/// Пробные программы, собираемые перед сверкой, и выполняет ли их своя среда.
///
/// `features` нужен ради таблиц метаданных, а не запуска: в нём P/Invoke,
/// которого у среды ещё нет.
const SAMPLES: [(&str, bool); 25] =
    [("hello", true), ("arith", true), ("objects", true), ("exceptions", true), ("generics", true), ("gc", true), ("text", true), ("collections", true), ("floats", true), ("enums", true), ("linq", true), ("files", true), ("time", true), ("form", true), ("winforms", true), ("controls", true), ("lists", true), ("dialogs", true), ("layout", true), ("choices", true), ("tabs", true), ("numbers", true), ("keys", true), ("pqueue", true), ("features", false)];

/// Аргументы командной строки образца. `time` печатает их и
/// `Environment.GetCommandLineArgs()` (фаза N5b); кириллица проверяет, что
/// аргумент доходит до программы строкой UTF-16 без потерь.
fn sample_args(sample: &str) -> &'static [&'static str] {
    match sample {
        "time" => &["alpha", "два"],
        // Форма N6a сама перерисовывается и закрывается: на машине разработчика
        // dotnet на миг показывает настоящее окно, песочнице щёлкать некому.
        "form" | "winforms" | "controls" | "lists" | "dialogs" | "layout" | "choices" | "tabs" | "numbers" | "keys" => &["self-test"],
        _ => &[],
    }
}

/// Имя сборки базовой библиотеки своей среды (`tools/dotnet/corelib`).
const CORELIB: &str = "FreeOs.CoreLib.dll";

pub fn check() -> Result<()> {
    let root = repo_root();
    let out = root.join("build").join("clr");
    fs::create_dir_all(&out).context("create build/clr")?;

    let dumper_dir = out.join("clrdump");
    dotnet_build(&root.join("tools/dotnet/clrdump"), &dumper_dir)?;
    let dumper = dumper_dir.join("clrdump.dll");

    let mut assemblies: Vec<(String, PathBuf)> = Vec::new();
    for (sample, _) in SAMPLES {
        let dir = out.join("samples").join(sample);
        dotnet_build(&root.join("tools/dotnet/samples").join(sample), &dir)?;
        assemblies.push((sample.to_string(), dir.join(format!("{sample}.dll"))));
    }
    // Своя базовая библиотека тоже сверяется: это сборка без стандартной
    // библиотеки (NoStdLib), и нашему разбору такие ещё не попадались.
    let corelib_dir = out.join("corelib");
    dotnet_build(&root.join("tools/dotnet/corelib"), &corelib_dir)?;
    let corelib_path = corelib_dir.join(CORELIB);
    assemblies.push((CORELIB.to_string(), corelib_path.clone()));
    let runtimes = installed_runtimes()?;
    if let Some(core) = runtimes.iter().find(|r| r.name == "Microsoft.NETCore.App") {
        for file in ["System.Private.CoreLib.dll", "System.Runtime.dll", "System.Console.dll"] {
            assemblies.push((file.to_string(), core.dir.join(file)));
        }
    }
    match runtimes.iter().find(|r| r.name == "Microsoft.WindowsDesktop.App") {
        Some(desktop) => {
            for file in ["System.Windows.Forms.dll", "System.Drawing.Common.dll"] {
                assemblies.push((file.to_string(), desktop.dir.join(file)));
            }
        }
        None => println!("clr-check: Windows Desktop Runtime is not installed, WinForms is not checked"),
    }

    let mut failed = 0;
    for (name, path) in &assemblies {
        let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let ours = dump(&data).with_context(|| format!("our reader on {name}"))?;
        let theirs = run_dumper(&dumper, path)?;
        match first_difference(&ours, &theirs) {
            None => println!("clr-check: {name}: {} lines match", ours.lines().count()),
            Some((line, our_line, their_line)) => {
                failed += 1;
                let base = out.join(name.replace(".dll", ""));
                fs::write(base.with_extension("ours.txt"), &ours)?;
                fs::write(base.with_extension("theirs.txt"), &theirs)?;
                println!("clr-check: {name}: DIFFERS at line {line}");
                println!("  ours  : {our_line}");
                println!("  theirs: {their_line}");
                println!("  full dumps: {}.{{ours,theirs}}.txt", base.display());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} assemblies differ from System.Reflection.Metadata", assemblies.len());
    }
    println!("clr-check: all {} assemblies match System.Reflection.Metadata", assemblies.len());

    // Фаза N4c: печать и разбор чисел — десятки тысяч случаев за секунды, без
    // интерпретатора (см. `numcheck`).
    crate::numcheck::check(&root, &out)?;

    // Фаза N2: те же программы — своей средой и настоящим dotnet. Совпасть
    // обязаны вывод до байта и код возврата.
    let corelib = fs::read(&corelib_path).with_context(|| format!("read {}", corelib_path.display()))?;
    refresh_initrd(&root, CORELIB, &corelib)?;
    let mut run_failed = 0;
    for (sample, run) in SAMPLES {
        if !run {
            continue;
        }
        let dll = out.join("samples").join(sample).join(format!("{sample}.dll"));
        let data = fs::read(&dll).with_context(|| format!("read {}", dll.display()))?;
        // Фаза N5a: каждая среда начинает в своём пустом каталоге, и сравнивается
        // не только вывод, но и файлы, которые программа после себя оставила.
        let sandbox = out.join("run").join(sample);
        let (our_dir, their_dir) = (sandbox.join("ours"), sandbox.join("theirs"));
        fresh_dir(&our_dir)?;
        fresh_dir(&their_dir)?;
        let args = sample_args(sample);
        let (their_code, theirs) = run_dotnet(&dll, &their_dir, args)?;
        let (our_code, ours) = run_ours(&data, &corelib, &our_dir, &format!("{sample}.dll"), args);
        let (our_files, their_files) = (tree(&our_dir)?, tree(&their_dir)?);
        if ours == theirs && our_code == Some(their_code) && our_files == their_files {
            let files = if our_files.is_empty() { String::new() } else { format!(", {} files and directories as dotnet left them", our_files.len()) };
            println!(
                "clr-check: run {sample}: output matches dotnet ({} lines), exit code {their_code}{files}",
                ours.lines().count()
            );
        } else {
            run_failed += 1;
            let base = out.join(format!("{sample}.run"));
            fs::write(base.with_extension("ours.txt"), &ours)?;
            fs::write(base.with_extension("theirs.txt"), &theirs)?;
            println!("clr-check: run {sample}: DIFFERS (exit code ours {our_code:?}, dotnet {their_code})");
            if let Some((line, our_line, their_line)) = first_difference(&ours, &theirs) {
                println!("  line {line}");
                println!("  ours  : {our_line}");
                println!("  theirs: {their_line}");
            }
            if our_files != their_files {
                let names = |files: &[(String, Vec<u8>)]| files.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(" ");
                println!("  files ours  : {}", names(&our_files));
                println!("  files theirs: {}", names(&their_files));
                println!("  (a name listed in both differs in content; trees are in {})", sandbox.display());
            }
        }
        refresh_initrd(&root, &format!("samples/{sample}.dll"), &data)?;
        // Рядом со сборкой — её `.runtimeconfig.json` (фаза N8): по нему «Файлы»
        // узнают программу и запускают её через `/bin/dotnet`, и его же везёт
        // пакет `winforms`.
        let config = out.join("samples").join(sample).join(format!("{sample}.runtimeconfig.json"));
        if let Ok(text) = fs::read(&config) {
            // SDK пишет его с CRLF, а репозиторий хранит текст с LF
            // (`.gitattributes`): без этой замены на свежем клоне `clr-check`
            // «обновлял» бы все эти файлы при каждом запуске.
            let text = String::from_utf8_lossy(&text).replace("\r\n", "\n");
            refresh_initrd(&root, &format!("samples/{sample}.runtimeconfig.json"), text.as_bytes())?;
        }
    }
    if run_failed > 0 {
        bail!("{run_failed} sample program(s) behave differently under the own runtime");
    }
    Ok(())
}

fn fresh_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir).with_context(|| format!("clear {}", dir.display()))?;
    }
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))
}

/// Всё, что лежит в каталоге: пути от него через `/` по порядку и содержимое
/// файлов (у каталога — пусто, а имя кончается на `/`).
fn tree(dir: &Path) -> Result<Vec<(String, Vec<u8>)>> {
    let mut entries = Vec::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = pending.pop() {
        for entry in fs::read_dir(&path).with_context(|| format!("list {}", path.display()))? {
            let entry = entry?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type()?.is_dir() {
                entries.push((format!("{name}/"), Vec::new()));
                pending.push((entry.path(), format!("{name}/")));
            } else {
                entries.push((name, fs::read(entry.path())?));
            }
        }
    }
    entries.sort();
    Ok(entries)
}

/// Выполнить сборку настоящим dotnet в каталоге `dir`.
fn run_dotnet(dll: &Path, dir: &Path, args: &[&str]) -> Result<(i32, String)> {
    // Режим инвариантной глобализации: у FreeOS культур нет, и сравнивать надо
    // с тем, что печатает .NET без них. Иначе на русской Windows эталон
    // получал бы запятую в дробных числах, неразрывные пробелы в разрядах и
    // культурное сравнение строк (фаза N4a).
    let output = Command::new("dotnet")
        .arg(dll)
        .args(args)
        .current_dir(dir)
        .env("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1")
        .output()
        .context("run dotnet")?;
    let text = String::from_utf8(output.stdout).context("dotnet output is not UTF-8")?;
    // .NET на Windows переводит строку парой CR LF, своя среда — одним LF, как
    // принято на FreeOS. Это разница платформ, а не поведения программы.
    Ok((output.status.code().unwrap_or(-1), text.replace("\r\n", "\n")))
}

/// Выполнить сборку своей средой с файлами в песочнице `dir`. Ошибка среды
/// попадает в вывод строкой — так её видно в сравнении рядом с тем, что успело
/// напечататься.
///
/// `program` — путь к сборке для `Environment.GetCommandLineArgs()`: у dotnet
/// там полный путь, образец печатает от него только имя файла.
fn run_ours(data: &[u8], corelib: &[u8], dir: &Path, program: &str, args: &[&str]) -> (Option<i32>, String) {
    let mut vm = match clr_vm::Vm::new(data, corelib, clr_vm::sandbox::Sandbox::new(dir)) {
        Ok(vm) => vm,
        Err(error) => return (None, format!("<load error: {error}>\n")),
    };
    vm.set_program_path(program);
    let result = vm.run_main(args);
    let mut output = vm.into_host().output;
    match result {
        Ok(code) => (Some(code), output),
        Err(error) => {
            output.push_str(&format!("<error: {error}>\n"));
            (None, output)
        }
    }
}

/// Положить свежую сборку (образец или базовую библиотеку) в initrd, если там
/// лежит другая. `relative` — путь внутри `/usr/share/dotnet`.
///
/// Образ системы собирается без .NET SDK, поэтому сборки лежат в репозитории
/// готовыми. Сборка у SDK детерминирована, и расхождение означает, что
/// поменялся исходник или сам SDK, — тогда образ обязан везти то, что сейчас
/// проверено.
fn refresh_initrd(root: &Path, relative: &str, data: &[u8]) -> Result<()> {
    let path = root.join("initrd/usr/share/dotnet").join(relative);
    if fs::read(&path).ok().as_deref() == Some(data) {
        return Ok(());
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    fs::write(&path, data).with_context(|| format!("write {}", path.display()))?;
    println!("clr-check: updated {} - the image carries this build now, commit it", path.display());
    Ok(())
}

fn repo_root() -> PathBuf {
    // `xtask` лежит в корне репозитория — его каталог и есть точка отсчёта.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}

pub(crate) fn dotnet_build(project: &Path, out: &Path) -> Result<()> {
    let status = Command::new("dotnet")
        .args(["build", "-c", "Release", "--nologo", "-v", "quiet", "-o"])
        .arg(out)
        .arg(project)
        .status()
        .context("run dotnet (is the .NET SDK installed?)")?;
    if !status.success() {
        bail!("dotnet build {} failed", project.display());
    }
    Ok(())
}

fn run_dumper(dumper: &Path, assembly: &Path) -> Result<String> {
    let output = Command::new("dotnet").arg(dumper).arg(assembly).output().context("run clrdump")?;
    if !output.status.success() {
        bail!(
            "clrdump {} failed: {}",
            assembly.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("clrdump output is not UTF-8")
}

struct Runtime {
    name: String,
    version: Vec<u64>,
    dir: PathBuf,
}

/// Установленные среды, самая новая версия каждой — из `dotnet --list-runtimes`.
fn installed_runtimes() -> Result<Vec<Runtime>> {
    let output = Command::new("dotnet").arg("--list-runtimes").output().context("dotnet --list-runtimes")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut best: Vec<Runtime> = Vec::new();
    for line in text.lines() {
        // `Microsoft.NETCore.App 10.0.1 [C:\Program Files\dotnet\shared\Microsoft.NETCore.App]`
        let Some((head, dir)) = line.split_once(" [") else { continue };
        let Some((name, version)) = head.split_once(' ') else { continue };
        // Предварительные выпуски (`-rc.1…`) не берутся: сверять с ними — значит
        // сверять с тем, что ещё поменяется.
        if version.contains('-') {
            continue;
        }
        let numbers: Vec<u64> = version.split('.').filter_map(|part| part.parse().ok()).collect();
        let dir = PathBuf::from(dir.trim_end_matches(']')).join(version);
        match best.iter_mut().find(|r| r.name == name) {
            Some(existing) if existing.version >= numbers => {}
            Some(existing) => {
                existing.version = numbers;
                existing.dir = dir;
            }
            None => best.push(Runtime { name: name.to_string(), version: numbers, dir }),
        }
    }
    Ok(best)
}

fn first_difference<'a>(ours: &'a str, theirs: &'a str) -> Option<(usize, &'a str, &'a str)> {
    let mut a = ours.lines();
    let mut b = theirs.lines();
    let mut number = 0;
    loop {
        number += 1;
        match (a.next(), b.next()) {
            (None, None) => return None,
            (x, y) if x == y => {}
            (x, y) => return Some((number, x.unwrap_or("<end>"), y.unwrap_or("<end>"))),
        }
    }
}

fn meta<T>(result: Result<T, clr_meta::Error>) -> Result<T> {
    result.map_err(|error| anyhow!("{error:?}"))
}

/// Тот же текст, что печатает `tools/dotnet/clrdump`, — нашим разбором.
fn dump(data: &[u8]) -> Result<String> {
    let asm = meta(Assembly::parse(data))?;
    let t = &asm.tables;
    let strings = asm.root.strings;
    let blobs = asm.root.blobs;
    let text = |index: u32| meta(strings.get(index)).map(escape);
    let blob = |index: u32| meta(blobs.get(index)).map(hex);
    let mut out = String::new();

    writeln!(
        out,
        "runtime {}.{} flags 0x{:08x} entry 0x{:08x}",
        asm.cli.runtime_major, asm.cli.runtime_minor, asm.cli.flags, asm.cli.entry_point
    )?;
    writeln!(out, "metadata {}", escape(asm.root.version))?;
    for table in 0..TABLE_COUNT as u8 {
        let rows = t.rows(table);
        if rows > 0 {
            writeln!(
                out,
                "table {table:02x} rows {rows} size {} offset {}",
                t.row_size(table),
                t.metadata_offset(table)
            )?;
        }
    }

    for row in 1..=t.rows(id::TYPE_REF) {
        let scope = meta(t.coded_column(id::TYPE_REF, row, 0, Coded::ResolutionScope))?;
        writeln!(
            out,
            "typeref {row} scope={} ns={} name={}",
            tok(scope),
            text(meta(t.column(id::TYPE_REF, row, 2))?)?,
            text(meta(t.column(id::TYPE_REF, row, 1))?)?
        )?;
    }

    for row in 1..=t.rows(id::TYPE_DEF) {
        let extends = meta(t.coded_column(id::TYPE_DEF, row, 3, Coded::TypeDefOrRef))?;
        let fields = span(meta(t.list(id::TYPE_DEF, row, 4, id::FIELD, id::FIELD_PTR))?)?;
        let methods = span(meta(t.list(id::TYPE_DEF, row, 5, id::METHOD_DEF, id::METHOD_PTR))?)?;
        writeln!(
            out,
            "typedef {row} flags=0x{:08x} ns={} name={} extends={} fields={fields} methods={methods}",
            meta(t.column(id::TYPE_DEF, row, 0))?,
            text(meta(t.column(id::TYPE_DEF, row, 2))?)?,
            text(meta(t.column(id::TYPE_DEF, row, 1))?)?,
            tok(extends)
        )?;
    }

    for row in 1..=t.rows(id::FIELD) {
        writeln!(
            out,
            "field {row} flags=0x{:04x} name={} sig={}",
            meta(t.column(id::FIELD, row, 0))?,
            text(meta(t.column(id::FIELD, row, 1))?)?,
            blob(meta(t.column(id::FIELD, row, 2))?)?
        )?;
    }

    for row in 1..=t.rows(id::METHOD_DEF) {
        let rva = meta(t.column(id::METHOD_DEF, row, 0))?;
        let params = meta(t.list(id::METHOD_DEF, row, 5, id::PARAM, id::PARAM_PTR))?.len();
        writeln!(
            out,
            "method {row} rva=0x{rva:08x} impl=0x{:04x} flags=0x{:04x} name={} sig={} params={params}",
            meta(t.column(id::METHOD_DEF, row, 1))?,
            meta(t.column(id::METHOD_DEF, row, 2))?,
            text(meta(t.column(id::METHOD_DEF, row, 3))?)?,
            blob(meta(t.column(id::METHOD_DEF, row, 4))?)?
        )?;
        let Some(body) = meta(asm.method_body(rva))? else { continue };
        writeln!(
            out,
            "body {row} maxstack={} init={} locals=0x{:08x} il={}",
            body.max_stack,
            u8::from(body.init_locals),
            body.local_signature,
            hex(body.code)
        )?;
        for clause in body.clauses() {
            let clause = meta(clause)?;
            let kind = clause.flags & 0x7;
            let extra = if kind == CLAUSE_CATCH || kind == CLAUSE_FILTER { clause.class_or_filter } else { 0 };
            writeln!(
                out,
                "eh {row} kind={kind} try={}+{} handler={}+{} extra=0x{extra:08x}",
                clause.try_offset, clause.try_length, clause.handler_offset, clause.handler_length
            )?;
        }
    }

    for row in 1..=t.rows(id::MEMBER_REF) {
        let parent = meta(t.coded_column(id::MEMBER_REF, row, 0, Coded::MemberRefParent))?;
        writeln!(
            out,
            "memberref {row} parent={} name={} sig={}",
            tok(parent),
            text(meta(t.column(id::MEMBER_REF, row, 1))?)?,
            blob(meta(t.column(id::MEMBER_REF, row, 2))?)?
        )?;
    }

    for row in 1..=t.rows(id::ASSEMBLY_REF) {
        let col = |c| meta(t.column(id::ASSEMBLY_REF, row, c));
        writeln!(
            out,
            "asmref {row} name={} version={}.{}.{}.{} flags=0x{:08x} key={} culture={}",
            text(col(6)?)?,
            col(0)?,
            col(1)?,
            col(2)?,
            col(3)?,
            col(4)?,
            blob(col(5)?)?,
            text(col(7)?)?
        )?;
    }

    for row in 1..=t.rows(id::CUSTOM_ATTRIBUTE) {
        let parent = meta(t.coded_column(id::CUSTOM_ATTRIBUTE, row, 0, Coded::HasCustomAttribute))?;
        let ctor = meta(t.coded_column(id::CUSTOM_ATTRIBUTE, row, 1, Coded::CustomAttributeType))?;
        writeln!(
            out,
            "attr {row} parent={} ctor={} value={}",
            tok(parent),
            tok(ctor),
            blob(meta(t.column(id::CUSTOM_ATTRIBUTE, row, 2))?)?
        )?;
    }

    for (offset, string) in asm.root.user_strings.iter() {
        if !string.is_empty() {
            writeln!(out, "us {offset} {}", escape_units(string.units()))?;
        }
    }
    Ok(out)
}

fn span(list: clr_meta::tables::ListRows<'_, '_>) -> Result<String> {
    let count = list.len();
    let mut first = 0;
    for (index, row) in list.enumerate() {
        let row = meta(row)?;
        if index == 0 {
            first = row;
        }
    }
    Ok(format!("{count}@{first}"))
}

fn tok(token: Token) -> String {
    if token.is_nil() { "00:0".to_string() } else { format!("{:02x}:{}", token.table, token.row) }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn escape(text: &str) -> String {
    escape_units(text.encode_utf16())
}

fn escape_units(units: impl Iterator<Item = u16>) -> String {
    let mut out = String::new();
    for unit in units {
        if (0x20..0x7F).contains(&unit) && unit != u16::from(b'\\') {
            out.push(char::from(unit as u8));
        } else {
            let _ = write!(out, "\\u{unit:04x}");
        }
    }
    out
}
