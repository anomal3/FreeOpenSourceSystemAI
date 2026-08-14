//! `cargo xtask` — host-side оркестратор сборки и запуска ОС.
//!
//! Живёт в том же workspace, что и ядерные крейты, но собирается под host-триплет.
//! Именно поэтому в `.cargo/config.toml` намеренно нет ни `[build] target`, ни
//! `[unstable] build-std`: обе настройки глобальны для workspace и сломали бы
//! сборку этого бинарника. Все `--target` передаются отсюда, явно и по одному.

mod arch;
mod build;
mod diskfile;
mod firmware;
mod harness;
mod image;
mod initrd;
mod inspect;
mod keys;
mod package;
mod paths;
mod qemu;
mod util;
mod version;

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
    /// Запустить установщик в QEMU: установочный носитель плюс чистый диск.
    Install(InstallArgs),
    /// Собрать загрузочный ISO — то, что можно отдать человеку или подключить
    /// к виртуальной машине.
    Iso(ImageArgs),
    /// Разобрать образ диска: разделы и содержимое корневой ФС.
    Inspect(InspectArgs),
    /// Прогнать систему в QEMU по сценариям стенда — без человека за клавиатурой.
    Test(TestArgs),
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
    /// Собрать ещё и установщик.
    #[arg(long)]
    installer: bool,
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
    /// Грузиться с диска, на который ставил установщик, — то есть проверить
    /// результат его работы.
    #[arg(long, conflicts_with = "image")]
    installed: bool,
    /// Подключить планшет вместо мыши — так, как это делает VirtualBox.
    ///
    /// Планшет не объявляет boot-протокола и сообщает координаты, а не
    /// приращения; ядро читает его, разобрав дескриптор отчётов. Флаг нужен,
    /// чтобы посмотреть на это глазами, не заводя виртуальную машину в другом
    /// гипервизоре.
    #[arg(long)]
    tablet: bool,
    /// Подключить сетевую карту virtio-net в пользовательскую сеть QEMU.
    ///
    /// Гость получает адрес `10.0.2.15`, шлюз и ответчик на `ping` —
    /// `10.0.2.2`, сервер имён — `10.0.2.3`. Адрес система пока не запрашивает
    /// сама: задайте его командой `ip 10.0.2.15/24 10.0.2.2`.
    #[arg(long)]
    net: bool,
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
    /// Собрать установочный носитель, а не образ готовой системы.
    #[arg(long)]
    installer: bool,
}

#[derive(Args, Debug)]
struct InspectArgs {
    /// Целевая архитектура — по ней выбирается образ по умолчанию.
    #[arg(long, short = 'a', value_enum, default_value = "x86_64")]
    arch: Arch,
    /// Разобрать конкретный файл вместо диска, на который ставил установщик.
    #[arg(long)]
    path: Option<std::path::PathBuf>,
}

#[derive(Args, Debug)]
struct InstallArgs {
    /// Целевая архитектура.
    #[arg(long, short = 'a', value_enum, default_value = "x86_64")]
    arch: Arch,
    /// Собирать с профилем release.
    #[arg(long, short = 'r')]
    release: bool,
    /// Headless-режим: только серийная консоль, без окна QEMU (для CI).
    #[arg(long)]
    serial_only: bool,
    /// Пересоздать целевой диск, стерев результат прошлой установки.
    #[arg(long)]
    fresh: bool,
    /// Пересоздать хранилище UEFI-переменных из шаблона прошивки.
    ///
    /// Нужно, когда меняется способ подключения носителя: в NVRAM остаётся
    /// запись загрузки со старым путём устройства, прошивка пытается пойти по
    /// ней и, не найдя, уходит в свою оболочку вместо установщика.
    #[arg(long)]
    reset_nvram: bool,
    /// Размер целевого диска в мегабайтах.
    #[arg(long, default_value_t = 1024)]
    target_size: u64,
    /// Объём памяти виртуальной машины.
    #[arg(long, default_value = "512M")]
    memory: String,
    /// Дополнительные аргументы для QEMU.
    #[arg(last = true, allow_hyphen_values = true)]
    qemu_args: Vec<String>,
}

#[derive(Args, Debug)]
struct TestArgs {
    /// Прогнать только одну архитектуру (по умолчанию — обе).
    #[arg(long, short = 'a', value_enum)]
    arch: Option<Arch>,
    /// Прогнать в release вместо debug.
    #[arg(long, short = 'r', conflicts_with = "full")]
    release: bool,
    /// Оба профиля на обеих архитектурах — та самая планка, которую фаза обязана
    /// взять перед коммитом.
    #[arg(long)]
    full: bool,
    /// Прогнать один сценарий по имени.
    #[arg(long, short = 's')]
    scenario: Option<String>,
    /// Показать список сценариев и выйти.
    #[arg(long)]
    list: bool,
    /// Показывать окно QEMU. Снимки экрана делаются и без него.
    #[arg(long)]
    windowed: bool,
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
                installer: args.installer,
            })?;
            print_built(&built);
        }

        Command::Run(args) => {
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: !args.no_kernel,
                initrd: !args.no_initrd,
                installer: false,
            })?;
            print_built(&built);

            // Три источника загрузки, и разница между ними принципиальна.
            // VVFAT — эмуляция: таблицы разделов не существует, проверить на
            // ней нечего, кроме самой системы. Образ проходит через нашу
            // разметку. Установленный диск не собирается вовсе — это результат
            // работы установщика, и его загрузка проверяет именно её.
            let drive = if args.installed {
                let target = paths::target_disk(args.arch);
                if !target.is_file() {
                    anyhow::bail!(
                        "установленного диска нет: {}\n\
                         Сначала выполните установку:\n    \
                         cargo xtask install --arch {}",
                        target.display(),
                        args.arch,
                    );
                }
                qemu::Drive::Image(target)
            } else if args.image {
                qemu::Drive::Image(image::build(&built, image::Kind::System)?)
            } else {
                qemu::Drive::HostDirectory(qemu::prepare_esp(&built)?)
            };

            let opts = qemu::RunOptions {
                gdb: args.gdb,
                serial_only: args.serial_only,
                reset_nvram: args.reset_nvram,
                memory: args.memory,
                extra: args.qemu_args,
                drives: vec![drive],
                pointer: if args.tablet { qemu::Pointer::Tablet } else { qemu::Pointer::Mouse },
                network: args.net,
                ..qemu::RunOptions::default()
            };
            qemu::run(&opts, &built)?;
        }

        Command::Image(args) => {
            let kind = if args.installer {
                image::Kind::Installer
            } else {
                image::Kind::System
            };
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: true,
                initrd: true,
                installer: args.installer,
            })?;
            print_built(&built);
            let path = image::build(&built, kind)?;
            image::describe(args.arch, &path, kind);
        }

        Command::Iso(args) => {
            let kind = if args.installer {
                image::Kind::Installer
            } else {
                image::Kind::System
            };
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: true,
                initrd: true,
                installer: args.installer,
            })?;
            print_built(&built);
            let path = image::build_iso(&built, kind)?;
            println!();
            println!("Подключите этот файл приводом к виртуальной машине с включённым EFI:");
            println!("  {}", path.display());
        }

        Command::Install(args) => {
            let built = build::build_all(&build::BuildOptions {
                arch: args.arch,
                release: args.release,
                kernel: true,
                initrd: true,
                installer: true,
            })?;
            print_built(&built);

            let media = image::build(&built, image::Kind::Installer)?;
            let target = image::prepare_target(args.arch, args.target_size, args.fresh)?;

            let opts = qemu::RunOptions {
                gdb: false,
                serial_only: args.serial_only,
                reset_nvram: args.reset_nvram,
                memory: args.memory,
                extra: args.qemu_args,
                // Порядок важен: прошивка перебирает носители в порядке
                // подключения, и загрузочный раздел есть только у первого —
                // целевой диск на этот момент пуст.
                drives: vec![qemu::Drive::Image(media), qemu::Drive::Image(target)],
                ..qemu::RunOptions::default()
            };
            qemu::run(&opts, &built)?;

            println!();
            println!("Проверить результат установки:");
            println!("    cargo xtask inspect --arch {}   # что записано на диск", args.arch);
            println!("    cargo xtask run --arch {} --installed   # загрузиться с него", args.arch);
        }

        Command::Inspect(args) => {
            let path = args.path.unwrap_or_else(|| paths::target_disk(args.arch));
            if !path.is_file() {
                anyhow::bail!(
                    "образа нет: {}\n\
                     Сначала выполните установку:\n    \
                     cargo xtask install --arch {}",
                    path.display(),
                    args.arch,
                );
            }
            inspect::image(&path)?;
        }

        Command::Test(args) => {
            if args.list {
                harness::list();
                return Ok(());
            }
            let arches = match args.arch {
                Some(arch) => vec![arch],
                None => Arch::ALL.to_vec(),
            };
            let profiles = if args.full {
                vec![false, true]
            } else {
                vec![args.release]
            };
            harness::run(&harness::TestOptions {
                arches,
                profiles,
                only: args.scenario,
                windowed: args.windowed,
            })?;
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

