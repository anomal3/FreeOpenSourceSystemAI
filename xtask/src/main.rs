//! `cargo xtask` — host-side оркестратор сборки и запуска ОС.
//!
//! Живёт в том же workspace, что и ядерные крейты, но собирается под host-триплет.
//! Именно поэтому в `.cargo/config.toml` намеренно нет ни `[build] target`, ни
//! `[unstable] build-std`: обе настройки глобальны для workspace и сломали бы
//! сборку этого бинарника. Все `--target` передаются отсюда, явно и по одному.

mod arch;
mod build;
mod firmware;
mod paths;
mod qemu;
mod util;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::arch::Arch;

#[derive(Parser, Debug)]
#[command(
    name = "cargo-xtask",
    bin_name = "cargo xtask",
    // Явный env!, а не голый `version`: не зависим от feature `cargo` у clap.
    version = env!("CARGO_PKG_VERSION"),
    about = "Сборка и запуск ОС: одна команда — от исходников до окна QEMU.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Собрать UEFI-загрузчик под указанную архитектуру.
    Build(BuildArgs),
    /// Собрать загрузчик и запустить его в QEMU.
    Run(RunArgs),
    /// Собрать загрузочный образ диска (появится в Phase 8).
    Image(ImageArgs),
    /// Быстрая проверка компиляции (cargo check) без линковки.
    Check(CheckArgs),
    /// Удалить target/ и build/.
    Clean,
}

#[derive(Args, Debug)]
struct BuildArgs {
    /// Целевая архитектура.
    #[arg(long, short = 'a', value_enum, default_value = "x86_64")]
    arch: Arch,
    /// Собирать с профилем release.
    #[arg(long, short = 'r')]
    release: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Целевая архитектура.
    #[arg(long, short = 'a', value_enum, default_value = "x86_64")]
    arch: Arch,
    /// Собирать с профилем release.
    #[arg(long, short = 'r')]
    release: bool,
    /// Поднять gdbstub на порту 1234 и остановиться до первой инструкции.
    #[arg(long)]
    gdb: bool,
    /// Headless-режим: только серийная консоль, без окна QEMU (для CI).
    #[arg(long)]
    serial_only: bool,
    /// Пересоздать хранилище UEFI-переменных из шаблона прошивки.
    #[arg(long)]
    reset_nvram: bool,
    /// Объём памяти виртуальной машины.
    #[arg(long, default_value = "512M")]
    memory: String,
    /// Дополнительные аргументы, передаваемые QEMU как есть: `-- -d int -D qemu.log`.
    #[arg(last = true, allow_hyphen_values = true)]
    qemu_args: Vec<String>,
}

#[derive(Args, Debug)]
struct ImageArgs {
    /// Целевая архитектура.
    #[arg(long, short = 'a', value_enum, default_value = "x86_64")]
    arch: Arch,
}

#[derive(Args, Debug)]
struct CheckArgs {
    /// Проверить только одну архитектуру (по умолчанию — обе).
    #[arg(long, short = 'a', value_enum)]
    arch: Option<Arch>,
}

fn main() -> std::process::ExitCode {
    match real_main() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // {:#} у anyhow печатает всю цепочку контекстов в одну строку —
            // именно то, что нужно, чтобы пользователь увидел и «что делали»,
            // и «что именно сломалось».
            eprintln!("\nошибка: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Build(args) => {
            let efi = build::build_boot_uefi(args.arch, args.release)?;
            println!("готово: {}", efi.display());
        }

        Command::Run(args) => {
            let efi = build::build_boot_uefi(args.arch, args.release)?;
            let opts = qemu::RunOptions {
                arch: args.arch,
                gdb: args.gdb,
                serial_only: args.serial_only,
                reset_nvram: args.reset_nvram,
                memory: args.memory,
                extra: args.qemu_args,
            };
            qemu::run(&opts, &efi)?;
        }

        Command::Image(args) => print_image_stub(args.arch),

        Command::Check(args) => {
            let arches: Vec<Arch> = match args.arch {
                Some(arch) => vec![arch],
                None => Arch::ALL.to_vec(),
            };
            build::check(&arches)?;
            println!("проверка пройдена");
        }

        Command::Clean => build::clean()?,
    }

    Ok(())
}

/// Честная заглушка вместо полусобранного образа.
fn print_image_stub(arch: Arch) {
    println!(
        "\
`xtask image --arch {arch}` пока не реализована — и это осознанное решение.

Настоящий загрузочный образ (GPT-таблица разделов + FAT32 ESP + записанный в него
загрузчик) появится в Phase 8, вместе с графическим установщиком: раньше он просто
некому нужен. Планируемая реализация — крейты `gpt` (разметка) и `fatfs`
(файловая система) поверх обычного файла-образа.

Что делать сейчас:

    cargo xtask run --arch {arch}

`run` использует драйвер VVFAT в QEMU: каталог хоста build/esp/{arch} подключается
как FAT-раздел напрямую (`-drive format=raw,file=fat:rw:...`). Файлы туда просто
копируются, никакой пересборки образа между правками — dev-loop получается заметно
короче, а прошивка видит ровно ту же структуру \\EFI\\BOOT\\{boot_file}, что и на
настоящем диске.

Ограничение, о котором стоит помнить: VVFAT — это эмуляция, а не реальный носитель.
Проверить GPT-разметку, выравнивание разделов или работу самого установщика на нём
нельзя. Именно поэтому Phase 8 и нужна.",
        boot_file = arch.removable_media_file(),
    );
}
