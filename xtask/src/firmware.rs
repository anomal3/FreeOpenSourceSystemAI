//! Поиск и подготовка UEFI-прошивки (edk2 / OVMF / AAVMF) для QEMU.
//!
//! Наш загрузчик — UEFI-приложение, поэтому QEMU обязан стартовать с edk2, а не
//! с SeaBIOS. Официальные сборки QEMU (в том числе Windows-инсталлятор) кладут
//! прошивки в подкаталог `share/` рядом с бинарником; дистрибутивы Linux
//! раскладывают их по /usr/share/{qemu,OVMF,AAVMF,edk2}.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::arch::Arch;
use crate::paths;
use crate::util;

/// Как именно подключать прошивку.
pub enum Firmware {
    /// Раздельные code+vars через `-drive if=pflash`.
    ///
    /// Это рекомендованный edk2 способ: код монтируется read-only, а
    /// хранилище переменных — отдельным изменяемым банком. Только так UEFI
    /// действительно умеет сохранять BootOrder/BootXXXX между запусками.
    Split { code: PathBuf, vars: PathBuf },
    /// Единый образ через `-bios`.
    ///
    /// Фолбэк для сборок, где рядом нет varstore. Переменные при этом
    /// эмулируются в RAM и не переживают перезапуск, но для dev-loop это
    /// несущественно: загрузчик находится по removable-media пути
    /// \EFI\BOOT\BOOT*.EFI, а он не требует записей в NVRAM.
    Unified { code: PathBuf },
}

/// Готовые аргументы QEMU + человекочитаемое описание найденной прошивки.
pub struct PreparedFirmware {
    pub args: Vec<String>,
    pub description: String,
}

/// Каталоги, в которых имеет смысл искать прошивки.
fn search_dirs(qemu_exe: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    if let Some(dir) = std::env::var_os("FREEOS_FIRMWARE_DIR") {
        push(PathBuf::from(dir));
    }

    // Относительно бинарника QEMU. Windows-инсталлятор: <qemu>\share.
    // MSYS2/Linux-префикс: <prefix>/bin/qemu-... -> <prefix>/share/qemu.
    if let Some(bin) = qemu_exe.and_then(Path::parent) {
        push(bin.join("share"));
        push(bin.to_path_buf());
        if let Some(prefix) = bin.parent() {
            push(prefix.join("share").join("qemu"));
            push(prefix.join("share").join("edk2"));
            push(prefix.join("share"));
        }
    }

    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base).join("qemu");
                push(base.join("share"));
                push(base);
            }
        }
        push(PathBuf::from(r"C:\Program Files\qemu\share"));
        push(PathBuf::from(r"C:\Program Files\qemu"));
        push(PathBuf::from(r"C:\qemu\share"));
        push(PathBuf::from(r"C:\qemu"));
        push(PathBuf::from(r"C:\msys64\mingw64\share\qemu"));
    } else {
        for dir in [
            "/usr/share/qemu",
            "/usr/share/edk2/x64",
            "/usr/share/edk2/aarch64",
            "/usr/share/edk2/ovmf",
            "/usr/share/OVMF",
            "/usr/share/AAVMF",
            "/usr/share/ovmf",
            "/usr/share/ovmf/x64",
            "/usr/share/qemu-efi-aarch64",
            "/usr/share/edk2-ovmf",
            "/usr/local/share/qemu",
            "/opt/homebrew/share/qemu",
        ] {
            push(PathBuf::from(dir));
        }
    }

    dirs
}

/// Находит прошивку для архитектуры.
pub fn resolve(arch: Arch, qemu_exe: Option<&Path>) -> Result<Firmware> {
    // 1. Явное переопределение переменными окружения имеет высший приоритет.
    if let Some(code) = std::env::var_os(arch.firmware_env()) {
        let code = PathBuf::from(code);
        if !code.is_file() {
            bail!(
                "{} указывает на несуществующий файл: {}",
                arch.firmware_env(),
                code.display()
            );
        }

        if let Some(vars) = std::env::var_os(arch.nvram_env()) {
            let vars = PathBuf::from(vars);
            if !vars.is_file() {
                bail!(
                    "{} указывает на несуществующий файл: {}",
                    arch.nvram_env(),
                    vars.display()
                );
            }
            return Ok(Firmware::Split { code, vars });
        }

        // Есть только code — попробуем угадать varstore рядом по известным парам.
        let guessed_vars = code
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|file_name| {
                arch.split_firmware_names()
                    .iter()
                    .find(|(code_name, _)| *code_name == file_name)
            })
            .and_then(|(_, vars_name)| code.parent().map(|dir| dir.join(vars_name)))
            .filter(|vars| vars.is_file());

        if let Some(vars) = guessed_vars {
            return Ok(Firmware::Split { code, vars });
        }

        return Ok(Firmware::Unified { code });
    }

    let dirs = search_dirs(qemu_exe);

    // 2. Раздельная прошивка (предпочтительно).
    for dir in &dirs {
        for (code_name, vars_name) in arch.split_firmware_names() {
            let code = dir.join(code_name);
            let vars = dir.join(vars_name);
            if code.is_file() && vars.is_file() {
                return Ok(Firmware::Split { code, vars });
            }
        }
    }

    // 3. Единый образ.
    for dir in &dirs {
        for name in arch.unified_firmware_names() {
            let code = dir.join(name);
            if code.is_file() {
                return Ok(Firmware::Unified { code });
            }
        }
    }

    bail!("{}", not_found_message(arch, &dirs));
}

fn not_found_message(arch: Arch, dirs: &[PathBuf]) -> String {
    let mut msg = format!(
        "не найдена UEFI-прошивка edk2 для {arch}.\n\
         Без неё QEMU стартует с SeaBIOS и наш .efi-загрузчик просто не будет запущен.\n\n\
         Искал каталоги:\n"
    );
    for dir in dirs {
        msg.push_str(&format!("  - {}\n", dir.display()));
    }

    msg.push_str("\nИскал пары code+vars (через -drive if=pflash):\n");
    for (code, vars) in arch.split_firmware_names() {
        msg.push_str(&format!("  - {code} + {vars}\n"));
    }

    msg.push_str("\nИскал единые образы (через -bios):\n");
    for name in arch.unified_firmware_names() {
        msg.push_str(&format!("  - {name}\n"));
    }

    msg.push_str(&format!(
        "\nЧто делать:\n\
         1) Windows: официальный инсталлятор QEMU кладёт прошивки в \
            C:\\Program Files\\qemu\\share (edk2-x86_64-code.fd, edk2-aarch64-code.fd).\n\
            Убедитесь, что установка завершилась и файлы на месте.\n\
         2) Linux: поставьте пакет ovmf / qemu-efi-aarch64 / edk2-ovmf.\n\
         3) Либо укажите путь вручную:\n\
            PowerShell:  $env:{code_env} = \"C:\\path\\to\\code.fd\"\n\
            (опционально) $env:{vars_env} = \"C:\\path\\to\\vars.fd\"\n\
            bash:        export {code_env}=/path/to/code.fd\n\
         4) Либо добавьте каталог с прошивками в FREEOS_FIRMWARE_DIR.\n",
        code_env = arch.firmware_env(),
        vars_env = arch.nvram_env(),
    ));

    msg
}

/// Готовит найденную прошивку к запуску и возвращает аргументы QEMU.
///
/// Для aarch64 здесь же решается вопрос дополнения образов: машина `virt`
/// имеет два жёстко заданных flash-банка по 64 MiB, и `-drive if=pflash`
/// требует, чтобы файл был ровно такого размера. QEMU свои `edk2-aarch64-code.fd`
/// и `edk2-arm-vars.fd` уже дополняет (`truncate -s 64m`), а вот Debian'овский
/// `QEMU_EFI.fd` — нет, поэтому короткие образы копируются в build/ с добивкой
/// нулями. Через `-bios` этого ограничения нет (QEMU просто грузит файл в
/// начало flash0 через load_image_mr, без проверки размера), поэтому фолбэк
/// на `-bios` остаётся рабочим при любом размере образа.
pub fn prepare(arch: Arch, firmware: &Firmware, reset_nvram: bool) -> Result<PreparedFirmware> {
    match firmware {
        Firmware::Split { code, vars } => {
            let dir = paths::firmware_dir(arch);
            let required = arch.pflash_size();

            // Код прошивки: подключается read-only, копия нужна только если
            // требуется добивка до размера банка.
            let code_path = match required {
                Some(size) if util::file_len(code) != Some(size) => {
                    let dst = dir.join("code.fd");
                    if util::file_len(&dst) != Some(size) {
                        util::write_padded(code, &dst, size)?;
                        say!(
                            "прошивка: {} дополнена нулями до {} MiB -> {}",
                            code.display(),
                            size / (1024 * 1024),
                            dst.display()
                        );
                    }
                    dst
                }
                _ => code.clone(),
            };

            // NVRAM обязана быть изменяемой и своей: писать в файл из
            // Program Files нельзя, да и портить системный шаблон не хочется.
            let vars_size = required.or_else(|| util::file_len(vars));
            let vars_path = dir.join("vars.fd");
            let vars_ok = !reset_nvram && util::file_len(&vars_path) == vars_size;
            if !vars_ok {
                match vars_size {
                    Some(size) => util::write_padded(vars, &vars_path, size)?,
                    None => util::copy_file(vars, &vars_path)?,
                }
                say!(
                    "прошивка: NVRAM инициализирована из {} -> {}",
                    vars.display(),
                    vars_path.display()
                );
            }

            let args = vec![
                "-drive".to_string(),
                format!(
                    "if=pflash,unit=0,format=raw,readonly=on,file={}",
                    util::qemu_path(&code_path)?
                ),
                "-drive".to_string(),
                format!(
                    "if=pflash,unit=1,format=raw,file={}",
                    util::qemu_path(&vars_path)?
                ),
            ];

            Ok(PreparedFirmware {
                args,
                description: format!(
                    "pflash: код {} + NVRAM {}",
                    code.display(),
                    vars_path.display()
                ),
            })
        }

        Firmware::Unified { code } => {
            if arch == Arch::X86_64 && is_code_only_x86(code) {
                eprintln!(
                    "ВНИМАНИЕ: рядом с {} не найден файл хранилища переменных \
                     (edk2-i386-vars.fd / OVMF_VARS*.fd).\n\
                     Запуск раздельного OVMF_CODE через -bios официально не поддерживается \
                     и может закончиться чёрным экраном.\n\
                     Укажите varstore через {} или установите полный комплект прошивки.",
                    code.display(),
                    arch.nvram_env()
                );
            }

            Ok(PreparedFirmware {
                args: vec!["-bios".to_string(), util::qemu_path(code)?],
                description: format!("-bios: {}", code.display()),
            })
        }
    }
}

fn is_code_only_x86(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            let lower = name.to_ascii_lowercase();
            lower.contains("code")
        })
        .unwrap_or(false)
}
