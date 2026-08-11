//! Вызовы cargo для крейтов, которые нельзя собрать под host-триплет.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::paths;
use crate::util;

pub const BOOT_PACKAGE: &str = "boot-uefi";

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

/// Собирает UEFI-загрузчик и возвращает путь к готовому `.efi`.
pub fn build_boot_uefi(arch: Arch, release: bool) -> Result<PathBuf> {
    let triple = arch.uefi_triple();

    let mut cmd = cargo();
    cmd.arg("build")
        .arg("--package")
        .arg(BOOT_PACKAGE)
        // --target обязателен здесь, а не в .cargo/config.toml: там [build] target
        // подействовал бы на весь workspace и сломал бы сборку самого xtask.
        .arg("--target")
        .arg(triple);
    if release {
        cmd.arg("--release");
    }

    util::run(&mut cmd, &format!("cargo build ({BOOT_PACKAGE}, {triple})")).with_context(|| {
        format!(
            "не удалось собрать {BOOT_PACKAGE} под {triple}.\n\
             Если cargo жалуется на отсутствующий таргет, выполните:\n    \
             rustup target add {triple}"
        )
    })?;

    let artifact = paths::target_dir()
        .join(triple)
        .join(paths::profile_dir_name(release))
        .join(format!("{BOOT_PACKAGE}.efi"));

    if !artifact.is_file() {
        bail!(
            "cargo отработал успешно, но артефакт не найден: {}\n\
             Ожидалось, что крейт {BOOT_PACKAGE} собирается в бинарный таргет \
             с тем же именем (для *-unknown-uefi cargo даёт расширение .efi).",
            artifact.display()
        );
    }

    Ok(artifact)
}

/// `cargo check` для указанных архитектур — быстрая проверка без линковки.
pub fn check(arches: &[Arch]) -> Result<()> {
    for &arch in arches {
        let triple = arch.uefi_triple();
        let mut cmd = cargo();
        cmd.arg("check")
            .arg("--package")
            .arg(BOOT_PACKAGE)
            .arg("--target")
            .arg(triple);
        util::run(&mut cmd, &format!("cargo check ({BOOT_PACKAGE}, {triple})"))?;
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
