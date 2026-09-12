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
}

/// Чем проверяется набор.
///
/// zlib выбран не за размер, а за устройство: свой рукописный `configure`, своя
/// проверка компилятора, `Makefile`, работа с файлами и с памятью, и никакой
/// зависимости от Linux. Ровно тот класс проекта, ради которого набор и
/// существует.
const PROJECTS: [Project; 1] = [Project {
    name: "zlib",
    version: "1.3.1",
    url: "https://github.com/madler/zlib/releases/download/v1.3.1/zlib-1.3.1.tar.gz",
    sha256: "9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23",
    unpacked: "zlib-1.3.1",
}];

/// Собрать все чужие проекты под все указанные архитектуры.
pub fn build_all(arches: &[Arch], refresh: bool) -> Result<()> {
    for project in &PROJECTS {
        let source = fetch(project, refresh)?;
        for &arch in arches {
            say!("=== {} {} для {} ===", project.name, project.version, arch.name());
            build(project, &source, arch)?;
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

    let archive = root.join(format!("{}-{}.tar.gz", project.name, project.version));
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
        .arg("-xzf")
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

    // `--static`: разделяемых библиотек в этой системе нет вовсе — нет ни
    // динамического компоновщика, ни отображения чужого образа в чужое
    // адресное пространство. Просить их у zlib значило бы получить отказ на
    // шаге, к набору отношения не имеющем.
    run_shell(
        &work,
        &format!("CHOST={triple} ./configure --static --prefix={prefix}"),
        &format!("configure {} {}", project.name, arch.name()),
    )?;

    run_make(&work, &["libz.a"], &format!("make {}", arch.name()))?;
    run_make(&work, &["install"], &format!("make install {}", arch.name()))?;

    for expected in [sysroot.join("lib/libz.a"), sysroot.join("include/zlib.h")] {
        if !expected.is_file() {
            bail!(
                "{} собрался, но {} в наборе не появился",
                project.name,
                expected.display()
            );
        }
    }
    let size = fs::metadata(sysroot.join("lib/libz.a"))?.len();
    say!(
        "{} {} для {}: libz.a {size} байт, установлен в {}",
        project.name,
        project.version,
        arch.name(),
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
    let make = find_make()?;
    let mut cmd = Command::new(make);
    cmd.current_dir(dir).args(args);
    with_sdk_path(&mut cmd);
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
