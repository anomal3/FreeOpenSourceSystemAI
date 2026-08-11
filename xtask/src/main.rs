//! `cargo xtask` — host-side оркестратор сборки и запуска ОС.
//!
//! Живёт в том же workspace, что и ядерные крейты, но собирается под host-триплет.
//! Именно поэтому в `.cargo/config.toml` намеренно нет ни `[build] target`, ни
//! `[unstable] build-std`: обе настройки глобальны для workspace и сломали бы
//! сборку этого бинарника. Все `--target` передаются отсюда, явно и по одному.

mod arch;
mod build;
mod firmware;
mod image;
mod initrd;
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
    /// Собрать загрузчик, ядро и образ initrd под указанную архитектуру.
    Build(BuildArgs),
    /// Собрать всё, разложить по ESP и запустить в QEMU.
    Run(RunArgs),
    /// Собрать загрузочный образ диска: GPT + FAT32 ESP.
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
    /// Не собирать ядро — только загрузчик.
    #[arg(long)]
    no_kernel: bool,
    /// Не собирать образ RAM-диска initrd.img.
    #[arg(long)]
    no_initrd: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Целевая архитектура.
    #[arg(long, short = 'a', value_enum, default_value = "x86_64")]
    arch: Arch,
    /// Собирать с профилем release.
    #[arg(long, short = 'r')]
    release: bool,
    /// Не собирать ядро и убрать kernel.elf из ESP: отладка самого загрузчика,
    /// в том числе его поведения, когда ядра на разделе нет.
    #[arg(long)]
    no_kernel: bool,
    /// Не собирать образ RAM-диска и убрать initrd.img из ESP: проверка того,
    /// что ядро поднимается и без файловой системы.
    #[arg(long)]
    no_initrd: bool,
    /// Поднять gdbstub на порту 1234 и остановиться до первой инструкции.
    #[arg(long)]
    gdb: bool,
    /// Headless-режим: только серийная консоль, без окна QEMU (для CI).
    #[arg(long)]
    serial_only: bool,
    /// Пересоздать хранилище UEFI-переменных из шаблона прошивки.
    #[arg(long)]
    reset_nvram: bool,
    /// Грузиться с настоящего образа диска (GPT + FAT32), а не с каталога
    /// хоста через VVFAT: медленнее на пересборке, но проверяет разметку.
    #[arg(long)]
    image: bool,
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
    /// Собирать с профилем release.
    #[arg(long, short = 'r')]
    release: bool,
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
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: !args.no_kernel,
                initrd: !args.no_initrd,
            })?;
            print_built(&built);
        }

        Command::Run(args) => {
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: !args.no_kernel,
                initrd: !args.no_initrd,
            })?;
            print_built(&built);
            let opts = qemu::RunOptions {
                gdb: args.gdb,
                serial_only: args.serial_only,
                reset_nvram: args.reset_nvram,
                image: args.image,
                memory: args.memory,
                extra: args.qemu_args,
            };
            qemu::run(&opts, &built)?;
        }

        Command::Image(args) => {
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: true,
                initrd: true,
            })?;
            print_built(&built);
            let path = image::build(&built)?;
            image::describe(args.arch, &path);
        }

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

/// Печатает, что собрано и где лежит.
///
/// Пути нужны не для красоты: их копируют в `add-symbol-file` при отладке и в
/// команды копирования на настоящую флешку.
fn print_built(built: &build::Built) {
    println!();
    println!("готово ({}, {}):", built.arch, built.profile());
    for (component, path) in built.iter() {
        println!("  {:<10} {}", component.title(), path.display());
    }
    if let Some(initrd) = built.initrd() {
        println!("  {:<10} {}", "initrd", initrd.display());
    }
    println!();
}

