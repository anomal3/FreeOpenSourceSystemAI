//! Пути внутри проекта.
//!
//! Всё считается от `CARGO_MANIFEST_DIR` самого xtask, а не от текущей рабочей
//! директории: `cargo xtask` можно запустить из любого подкаталога workspace.

use std::path::{Path, PathBuf};

use crate::arch::Arch;

/// Корень workspace: каталог на уровень выше `xtask/`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("у каталога xtask всегда есть родитель — корень workspace")
        .to_path_buf()
}

/// Каталог артефактов cargo с учётом возможного `CARGO_TARGET_DIR`.
pub fn target_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(value) if !value.is_empty() => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                workspace_root().join(path)
            }
        }
        _ => workspace_root().join("target"),
    }
}

/// Каталог артефактов xtask (ESP, копии прошивок, будущие образы дисков).
/// Он в .gitignore и полностью удаляется командой `clean`.
pub fn build_dir() -> PathBuf {
    workspace_root().join("build")
}

/// Каталог, который отдаётся QEMU как FAT-раздел ESP.
/// Разделён по архитектурам, чтобы `BOOTX64.EFI` от прошлой сборки не оставался
/// рядом с `BOOTAA64.EFI` и не путал ни прошивку, ни пользователя.
pub fn esp_dir(arch: Arch) -> PathBuf {
    build_dir().join("esp").join(arch.name())
}

/// Каталог для изменяемых копий прошивки (NVRAM, дополненные до 64 MiB образы).
pub fn firmware_dir(arch: Arch) -> PathBuf {
    build_dir().join("firmware").join(arch.name())
}

pub fn profile_dir_name(release: bool) -> &'static str {
    if release { "release" } else { "debug" }
}
