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
}

/// Собирает всё, что нужно для запуска: загрузчик, ядро и образ RAM-диска.
pub fn build_all(opts: &BuildOptions) -> Result<Built> {
    let mut items = Vec::with_capacity(Component::ALL.len());

    for component in Component::ALL {
        if component == Component::Kernel && !opts.kernel {
            println!("ядро пропущено (--no-kernel)");
            continue;
        }
        items.push((
            component,
            build_component(component, opts.arch, opts.release)?,
        ));
    }

    let initrd = if opts.initrd {
        Some(initrd::build()?)
    } else {
        println!("initrd пропущен (--no-initrd)");
        None
    };

    Ok(Built {
        arch: opts.arch,
        release: opts.release,
        items,
        initrd,
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
