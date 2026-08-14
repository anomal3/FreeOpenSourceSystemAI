//! Вызовы cargo для крейтов, которые нельзя собрать под host-триплет.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::{Arch, Component};
use crate::initrd;
use crate::paths;
use crate::util;

/// Что именно собирать.
///
/// Отдельная структура, а не четыре аргумента подряд: `build` и `run` задают
/// один и тот же набор, и при вызове было бы уже не разобрать, какой из голых
/// булевых флагов что означает.
pub struct BuildOptions {
    pub arch: Arch,
    pub release: bool,
    /// Собирать ядро (`--no-kernel` выключает).
    pub kernel: bool,
    /// Собирать образ RAM-диска (`--no-initrd` выключает).
    pub initrd: bool,
    /// Собирать установщик. Обычному запуску он не нужен, а сборка его стоит
    /// времени, поэтому по умолчанию выключен.
    pub installer: bool,
}

fn cargo() -> Command {
    // Cargo сообщает дочернему процессу путь к себе; так мы гарантированно
    // используем тот же toolchain, из которого запущен xtask.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    // Работаем из корня workspace, чтобы подхватились .cargo/config.toml
    // и rust-toolchain.toml независимо от того, откуда вызван xtask.
    cmd.current_dir(paths::workspace_root());
    cmd
}

/// Результат сборки: какие компоненты собраны и где лежат их артефакты.
///
/// Хранится списком, а не полями, чтобы раскладка ESP умела пройти по нему
/// циклом и не знала заранее, из скольких файлов состоит система.
pub struct Built {
    pub arch: Arch,
    pub release: bool,
    items: Vec<(Component, PathBuf)>,
    /// Образ RAM-диска. Не компонент: собирается не cargo и не зависит от
    /// архитектуры, поэтому в общий список не помещается.
    initrd: Option<PathBuf>,
    /// Пользовательские программы: имя и путь к собранному файлу.
    ///
    /// Нужны дважды и по-разному. В образ RAM-диска они попадают файлами
    /// `/bin/<имя>` — так их запускает система, загруженная с носителя. На
    /// установочный носитель они кладутся отдельными файлами, потому что
    /// установщик переносит их на **корневой раздел**, а читать их из образа
    /// RAM-диска он не умеет: это потребовало бы от него FAT-читалки, которой у
    /// него нет и заводить которую ради этого незачем.
    programs: Vec<(&'static str, PathBuf)>,
}

impl Built {
    pub fn iter(&self) -> impl Iterator<Item = (Component, &Path)> {
        self.items
            .iter()
            .map(|(component, path)| (*component, path.as_path()))
    }

    /// Имя профиля так, как его называет cargo.
    pub fn profile(&self) -> &'static str {
        paths::profile_dir_name(self.release)
    }

    /// Путь к артефакту компонента, если он собирался в этот заход.
    pub fn get(&self, component: Component) -> Option<&Path> {
        self.iter()
            .find(|(item, _)| *item == component)
            .map(|(_, path)| path)
    }

    /// Путь к образу RAM-диска, если он собирался в этот заход.
    pub fn initrd(&self) -> Option<&Path> {
        self.initrd.as_deref()
    }

    /// Пользовательские программы, собранные в этот заход.
    pub fn programs(&self) -> impl Iterator<Item = (&'static str, &Path)> {
        self.programs
            .iter()
            .map(|(name, path)| (*name, path.as_path()))
    }
}

/// Собирает всё, что нужно для запуска: загрузчик, ядро и образ RAM-диска.
pub fn build_all(opts: &BuildOptions) -> Result<Built> {
    let mut items = Vec::with_capacity(Component::ALL.len());

    for component in Component::ALL {
        if component == Component::Kernel && !opts.kernel {
            println!("ядро пропущено (--no-kernel)");
            continue;
        }
        if component == Component::Installer && !opts.installer {
            continue;
        }
        items.push((
            component,
            build_component(component, opts.arch, opts.release)?,
        ));
    }

    // Программы собираются вместе с ядром: программа и ядро связаны номерами
    // системных вызовов, и собранная порознь пара разъезжается молча.
    let programs: Vec<(&'static str, PathBuf)> = if opts.kernel || opts.initrd {
        USER_PROGRAMS
            .iter()
            .copied()
            .zip(build_user_programs(opts.arch, opts.release)?)
            .collect()
    } else {
        Vec::new()
    };

    let initrd = if opts.initrd {
        // В образ они попадают как `/bin/<имя>` — оттуда их запускает система,
        // загруженная с носителя.
        let mut extra: Vec<(String, PathBuf)> = programs
            .iter()
            .map(|(name, path)| (format!("bin/{name}"), path.clone()))
            .collect();
        // Образцовые пакеты — в `/media`, туда же, куда их кладёт установщик.
        // Живая система обязана уметь ставить пакеты так же, как установленная:
        // иначе проверить `pkg` было бы можно только после установки.
        if !programs.is_empty() {
            for package in crate::package::build_samples(opts.arch, opts.release)? {
                extra.push((format!("media/{}", package.file_name), package.path));
            }
        }
        Some(initrd::build(&extra)?)
    } else {
        println!("initrd пропущен (--no-initrd)");
        None
    };

    Ok(Built {
        arch: opts.arch,
        release: opts.release,
        items,
        initrd,
        programs,
    })
}

/// Собирает один компонент и возвращает путь к готовому артефакту.
pub fn build_component(component: Component, arch: Arch, release: bool) -> Result<PathBuf> {
    let package = component.package();
    let triple = component.triple(arch);

    let mut cmd = cargo();
    cmd.arg("build")
        .arg("--package")
        .arg(package)
        // --target обязателен здесь, а не в .cargo/config.toml: там [build] target
        // подействовал бы на весь workspace и сломал бы сборку самого xtask.
        .arg("--target")
        .arg(triple);
    apply_build_std(&mut cmd, component);
    if release {
        cmd.arg("--release");
    }

    run_cargo(&mut cmd, "build", component, triple)?;

    let dir = paths::artifact_dir(triple, release);
    locate_artifact(&dir, component)
}

/// Триплет пользовательских программ.
///
/// Отличается от ядерного на AArch64, и отличается намеренно. Ядро собрано под
/// `softfloat`: обработчик прерывания там не имеет права трогать векторные
/// регистры, потому что сохранять их посреди прерывания дорого и незачем.
/// Программа — наоборот: с Phase 29a её векторное состояние принадлежит задаче
/// и переживает переключение, поэтому запрещать компилятору NEON стало нечем.
///
/// На x86-64 таргет тот же, что у ядра, но векторы включаются флагом (см.
/// [`build_user_programs`]): отдельного «none-hardfloat» варианта у этой
/// архитектуры не существует.
fn user_triple(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64-unknown-none",
        Arch::Aarch64 => "aarch64-unknown-none",
    }
}

/// Имена пользовательских программ. Они же — имена файлов в `/bin`.
pub const USER_PROGRAMS: [&str; 21] = [
    "hello", "crash", "peek", "perms", "count", "spin", "forever", "nap", "save", "wc", "ls",
    "ask", "vec", "mc", "pkg", "init", "svclog", "svcbad", "dhcp", "echod", "echoc",
];

/// Программы, которые в `/bin` **не** едут.
///
/// Они попадают в систему единственным способом — внутри пакета, — и в этом всё
/// их назначение: «пакет положил программу, и она работает» невозможно доказать
/// программой, которая и так лежит на диске. Собираются они вместе со всеми
/// (`cargo build --bins` строит их в любом случае), а вот в initrd и на
/// установочный носитель не попадают.
pub const PACKAGED_PROGRAMS: [&str; 1] = ["greet"];

/// Собрать программы, исполняющиеся вне ядра.
///
/// Отдельная функция, а не ещё один [`Component`]: компонент — это один
/// артефакт, попадающий на ESP по своему пути, а здесь несколько бинарников,
/// которые уезжают в файловую систему как обычные файлы.
///
/// Компоновочный сценарий подаётся через `RUSTFLAGS` этого запуска, а не через
/// `.cargo/config.toml`: там он подействовал бы на весь workspace, включая
/// ядро, у которого раскладка своя.
pub fn build_user_programs(arch: Arch, release: bool) -> Result<Vec<PathBuf>> {
    let triple = user_triple(arch);
    let script = paths::workspace_root().join("crates/user-progs/user.ld");
    if !script.is_file() {
        bail!("нет компоновочного сценария программ: {}", script.display());
    }

    let mut cmd = cargo();
    cmd.arg("build")
        .arg("--package")
        .arg("user-progs")
        .arg("--bins")
        .arg("--target")
        .arg(triple);
    if release {
        cmd.arg("--release");
    }
    // Стандартные крейты пересобираются теми же флагами, что и программа.
    // Иначе не сходится модель кода: rustup поставляет `core`, собранный под
    // малую модель (адреса влезают в 32 бита со знаком), а программа живёт по
    // адресу 512 ГиБ. Ошибка выглядит как «relocation R_X86_64_32S out of
    // range» в чужом объектнике и никак не связана с нашим кодом.
    cmd.arg("-Zbuild-std=core,compiler_builtins")
        .arg("-Zbuild-std-features=compiler-builtins-mem");

    // `-C link-arg=-T<файл>` доходит до компоновщика как есть. Максимальный
    // размер страницы задаётся явно: без него lld выравнивает сегменты по 2 МиБ
    // и раздувает крошечную программу до мегабайтов нулей в файле.
    // Отладочная информация из образа выбрасывается, и это не про размер.
    // Ядро читает файл программы целиком в кучу, прежде чем разобрать его
    // заголовки; с отладочными секциями крошечная программа весит два с
    // половиной мегабайта, и сегмент, уехавший за предел чтения, выглядел бы
    // как испорченный файл.
    let mut flags = format!(
        "-C link-arg=-T{} -C link-arg=-z -C link-arg=max-page-size=0x1000 \
         -C relocation-model=static -C strip=debuginfo",
        script.display()
    );
    if arch == Arch::X86_64 {
        // Большая модель кода: обращения к своим же данным по абсолютному
        // 64-битному адресу. На AArch64 этого не нужно — там адрес собирается
        // парой `adrp`/`add` относительно счётчика команд, и абсолютное
        // положение программы значения не имеет.
        flags.push_str(" -C code-model=large");
        // Векторные регистры программе разрешены — с Phase 29a ядро сохраняет
        // их при переключении задач. Таргет `x86_64-unknown-none` объявляет
        // `-sse,+soft-float`, то есть запрещает компилятору их использовать
        // вовсе; без этой строки проверка «две программы не видят чисел друг
        // друга» проверяла бы код, в котором векторов нет ни одного.
        flags.push_str(" -C target-feature=+sse,+sse2,-soft-float");
    }
    cmd.env("RUSTFLAGS", flags);

    run_cargo(&mut cmd, "build", Component::Kernel, triple)?;

    let dir = paths::artifact_dir(triple, release);
    let mut built = Vec::new();
    for name in USER_PROGRAMS.iter().chain(PACKAGED_PROGRAMS.iter()) {
        let path = dir.join(name);
        if !path.is_file() {
            bail!(
                "программа {name} не собралась: нет {}\n\
                 Ожидались бинарники: {}",
                path.display(),
                USER_PROGRAMS.join(", ")
            );
        }
        if USER_PROGRAMS.contains(name) {
            built.push(path);
        }
    }
    Ok(built)
}

/// Путь к собранной программе, которая едет только внутри пакета.
///
/// Отдельная функция, а не ещё один элемент возвращаемого списка: список
/// [`USER_PROGRAMS`] совпадает с содержимым `/bin` один к одному, и подмешивать
/// в него то, чего в `/bin` нет, значило бы завести исключение в трёх местах
/// сразу — в initrd, в установщике и в сценариях стенда.
pub fn packaged_program(arch: Arch, release: bool, name: &str) -> Result<PathBuf> {
    let path = paths::artifact_dir(user_triple(arch), release).join(name);
    if !path.is_file() {
        bail!("программа {name} не собрана: нет {}", path.display());
    }
    Ok(path)
}

/// Досыпает `-Z build-std` при сборке ядра.
///
/// Ядро линкуется как PIE, а `core`/`alloc`, которые rustup поставляет уже
/// собранными, собраны со `static` relocation-model. На aarch64 это упирается в
/// отказ линкера: `relocation R_AARCH64_ABS64 cannot be used against local
/// symbol` — абсолютные релокации из чужого объектника невозможно уложить в
/// позиционно-независимый образ. Пересборка стандартных крейтов теми же
/// rustflags, что и само ядро, снимает расхождение.
///
/// Флаг живёт здесь, а не в `.cargo/config.toml`, по той же причине, что и
/// `--target`: секция `[unstable]` глобальна для workspace и применилась бы к
/// хостовой сборке самого xtask.
fn apply_build_std(cmd: &mut Command, component: Component) {
    if component != Component::Kernel {
        return;
    }
    cmd.arg("-Zbuild-std=core,alloc,compiler_builtins")
        // memcpy и соседи в freestanding-окружении взять неоткуда: их даёт
        // compiler_builtins, но только с этой фичей.
        .arg("-Zbuild-std-features=compiler-builtins-mem");
}

/// Запускает cargo и, если он упал, дописывает к его ошибке разбор причины.
///
/// Подсказка идёт отдельным абзацем, а не через `with_context`: anyhow
/// склеивает цепочку контекстов через «: », и многострочный совет в такой
/// строке читается плохо.
fn run_cargo(cmd: &mut Command, verb: &str, component: Component, triple: &str) -> Result<()> {
    let package = component.package();
    match util::run(cmd, &format!("cargo {verb} ({package}, {triple})")) {
        Ok(()) => Ok(()),
        Err(err) => bail!("{err:#}\n\n{}", failure_hint(component, triple)),
    }
}

/// Разбор типовых причин, по которым cargo не собрал компонент.
fn failure_hint(component: Component, triple: &str) -> String {
    let package = component.package();
    let manifest = paths::crate_manifest(package);

    if !manifest.is_file() {
        let mut msg = format!("крейта {package} нет: не найден {}", manifest.display());
        if component == Component::Kernel {
            msg.push_str(
                "\nПока ядро не написано, загрузчик собирается и запускается отдельно:\n    \
                 cargo xtask run --no-kernel",
            );
        }
        return msg;
    }

    if !paths::is_workspace_member(package) {
        return format!(
            "крейт {package} существует, но не подключён к workspace — отсюда и \
             «did not match any packages».\n\
             Добавьте \"crates/{package}\" в members корневого Cargo.toml."
        );
    }

    format!(
        "не удалось собрать {package} ({}) под {triple}.\n\
         Если cargo жалуется на отсутствующий таргет, выполните:\n    \
         rustup target add {triple}",
        component.title(),
    )
}

/// Ищет артефакт компонента в `target/<triple>/<profile>/`.
///
/// Имя проверяется фактическое, а не предполагаемое: расширение задаёт
/// спецификация таргета (`exe_suffix`), и оно разное — `boot-uefi.efi` у
/// UEFI-таргетов против `kernel` без расширения у `*-unknown-none`. Если
/// ожидаемого файла нет, перебираем каталог по имени пакета, отбрасывая
/// побочные продукты сборки (`.d`, `.pdb`, ...), чтобы вместо «файл не найден»
/// дать либо настоящий артефакт, либо внятный список того, что там лежит.
fn locate_artifact(dir: &Path, component: Component) -> Result<PathBuf> {
    let expected = dir.join(component.artifact_file());
    if expected.is_file() {
        return Ok(expected);
    }

    // cargo нормализует дефисы в имени пакета в подчёркивания для файлов,
    // но не для бинарных таргетов; проверяем оба варианта.
    let stems = [
        component.package().to_string(),
        component.package().replace('-', "_"),
    ];
    // Всё, что cargo кладёт рядом с бинарником и что бинарником не является.
    const SIDE_PRODUCTS: [&str; 6] = ["d", "pdb", "rlib", "rmeta", "dwp", "o"];

    let mut found: Vec<PathBuf> = Vec::new();
    let mut listing: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let stem_matches = path
                .file_stem()
                .is_some_and(|stem| stems.iter().any(|want| stem == want.as_str()));
            if !stem_matches {
                continue;
            }
            let is_side_product = path.extension().is_some_and(|ext| {
                SIDE_PRODUCTS
                    .iter()
                    .any(|bad| ext.eq_ignore_ascii_case(bad))
            });
            listing.push(entry.file_name().to_string_lossy().into_owned());
            if !is_side_product {
                found.push(path);
            }
        }
    }

    if let Some(actual) = found.first() {
        println!(
            "внимание: ожидался артефакт {}, найден {} — использую его",
            component.artifact_file(),
            actual.display()
        );
        return Ok(actual.clone());
    }

    let seen = if listing.is_empty() {
        "ничего похожего в каталоге нет".to_string()
    } else {
        format!("рядом лежат: {}", listing.join(", "))
    };
    bail!(
        "cargo отработал успешно, но артефакт не найден: {}\n\
         Ожидалось, что крейт {} собирается в бинарный таргет с тем же именем; {seen}.",
        expected.display(),
        component.package(),
    );
}

/// `cargo check` для указанных архитектур — быстрая проверка без линковки.
///
/// Целей четыре (два компонента на две архитектуры) плюс host-крейт xtask:
/// одна ошибка в общем коде должна вылезать здесь, а не при сборке конкретного
/// таргета.
pub fn check(arches: &[Arch]) -> Result<()> {
    for &arch in arches {
        for component in Component::ALL {
            let package = component.package();
            let triple = component.triple(arch);
            let mut cmd = cargo();
            cmd.arg("check")
                .arg("--package")
                .arg(package)
                .arg("--target")
                .arg(triple);
            apply_build_std(&mut cmd, component);
            run_cargo(&mut cmd, "check", component, triple)?;
        }

        // `disk` собирается и хостом (сборщик образа), и UEFI-приложением
        // установщика. Хостовую сборку проверяет `cargo check` ниже, а вот
        // ошибку, которая проявляется только под `*-unknown-uefi` (скажем,
        // случайную зависимость от `std`), надо ловить здесь: иначе она
        // всплывёт при сборке установщика, то есть далеко от причины.
        let triple = Component::BootUefi.triple(arch);
        let mut cmd = cargo();
        cmd.arg("check")
            .arg("--package")
            .arg("disk")
            .arg("--target")
            .arg(triple);
        util::run(&mut cmd, &format!("cargo check (disk, {triple})"))?;
    }

    // Хост-часть workspace (сам xtask) проверяется без --target.
    let mut cmd = cargo();
    cmd.arg("check").arg("--package").arg("xtask");
    util::run(&mut cmd, "cargo check (xtask)")?;

    // Разбор дескрипторов HID — единственная часть USB-стека, которую можно
    // проверить, не заводя ни устройства, ни эмулятора: на входе байты, на
    // выходе битовые смещения. Ошибка в них выглядит как «курсор ездит
    // наискось», то есть как неисправное железо, и ловить её прогоном стенда
    // дороже на порядок. Здесь же она стоит секунды.
    let mut cmd = cargo();
    cmd.arg("test").arg("--package").arg("usb-hid");
    util::run(&mut cmd, "cargo test (usb-hid)")?;

    // Формат пакета и запись о слотах — то же самое рассуждение, что у HID.
    // Откат по счётчику попыток и разбор порванной записи проверяются здесь за
    // секунды; в эмуляторе то же утверждение стоило бы четырёх загрузок и
    // получаса, а поймать в нём арифметику счётчика всё равно нечем.
    for package in ["fpk", "slots"] {
        let mut cmd = cargo();
        cmd.arg("test").arg("--package").arg(package);
        util::run(&mut cmd, &format!("cargo test ({package})"))?;
    }

    // Пользовательские программы собираются под обе архитектуры вместе с ядром
    // (см. `build_user_programs`), но `check` их до сих пор не трогал: ошибка в
    // программе всплывала только при полной сборке образа. С появлением `pkg`,
    // `init` и служб программ стало восемнадцать, и это перестало быть мелочью.
    for &arch in arches {
        let mut cmd = cargo();
        cmd.arg("check")
            .arg("--package")
            .arg("user-progs")
            .arg("--bins")
            .arg("--target")
            .arg(user_triple(arch))
            .arg("-Zbuild-std=core,compiler_builtins")
            .arg("-Zbuild-std-features=compiler-builtins-mem");
        util::run(&mut cmd, &format!("cargo check (user-progs, {})", user_triple(arch)))?;
    }

    Ok(())
}

/// `cargo clean` плюс удаление каталога build/.
pub fn clean() -> Result<()> {
    let build = paths::build_dir();
    if build.exists() {
        std::fs::remove_dir_all(&build)
            .with_context(|| format!("не удалось удалить {}", build.display()))?;
        println!("удалён {}", build.display());
    }

    let mut cmd = cargo();
    cmd.arg("clean");
    util::run(&mut cmd, "cargo clean")?;

    Ok(())
}
