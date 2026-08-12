//! Подготовка ESP и запуск QEMU.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::arch::{self, Arch, Component};
use crate::build::Built;
use crate::firmware;
use crate::paths;
use crate::util;

pub struct RunOptions {
    pub gdb: bool,
    pub serial_only: bool,
    pub reset_nvram: bool,
    pub memory: String,
    pub extra: Vec<String>,
    /// Носители машины в порядке подключения.
    pub drives: Vec<Drive>,
    /// Куда уходит серийная консоль.
    pub serial: Serial,
    /// Адрес, на котором стенд ждёт подключения монитора QEMU (HMP).
    ///
    /// `None` — монитора нет вовсе; так запускается обычный `run`, где им
    /// управляет человек через окно эмулятора.
    pub monitor: Option<SocketAddr>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            gdb: false,
            serial_only: false,
            reset_nvram: false,
            memory: "512M".to_string(),
            extra: Vec::new(),
            drives: Vec::new(),
            serial: Serial::Stdio,
            monitor: None,
        }
    }
}

/// Куда QEMU выводит серийную консоль.
pub enum Serial {
    /// В тот же терминал, из которого запущен xtask.
    Stdio,
    /// В сокет, к которому QEMU **подключается сам**.
    ///
    /// Клиентский режим, а не серверный, выбран сознательно: слушает стенд,
    /// поэтому порт можно занять заранее (`127.0.0.1:0`) и узнать его номер у
    /// ядра ОС. Серверный режим потребовал бы выбрать номер заранее и надеяться,
    /// что он свободен, — то есть гонку с любым другим процессом на машине.
    ///
    /// Сокет вместо канала (pipe) — тоже не деталь: канал на Windows съедает
    /// возврат каретки (0x0D), и путь «CR как Enter» через него не проверить
    /// вовсе. Через сокет байты доходят как есть.
    Socket(SocketAddr),
}

/// Носитель, подключаемый к машине.
pub enum Drive {
    /// Каталог хоста, выдаваемый за FAT-раздел драйвером VVFAT.
    ///
    /// Никакой таблицы разделов не существует — QEMU синтезирует её на лету.
    /// Для цикла «поправил — запустил» это лучший вариант: между правками
    /// ничего не пересобирается.
    HostDirectory(PathBuf),
    /// Настоящий образ: наша разметка, наша файловая система.
    Image(PathBuf),
}

/// Раскладывает собранные артефакты по структуре ESP и возвращает корень раздела.
///
/// Настоящий загрузочный образ (GPT + FAT32) пока не собирается: QEMU умеет
/// представлять обычный каталог хоста как FAT-раздел (драйвер VVFAT,
/// `file=fat:rw:<dir>`). Это убирает из dev-loop целый шаг — пересборку образа
/// после каждой правки — и заметно ускоряет цикл «поправил → запустил».
///
/// Раскладка: загрузчик уходит в `\EFI\BOOT\BOOT<MACHINE>.EFI` (путь диктует
/// прошивка), ядро — в корень под именем `kernel.elf`, образ RAM-диска — туда
/// же под именем `initrd.img` (оба имени диктует загрузчик).
pub fn prepare_esp(built: &Built) -> Result<PathBuf> {
    let arch = built.arch;
    let esp = paths::esp_dir(arch);

    std::fs::create_dir_all(&esp)
        .with_context(|| format!("не удалось создать каталог ESP {}", esp.display()))?;

    // Только компоненты системы: установщик кладётся по тому же пути, что и
    // загрузчик (`\EFI\BOOT\BOOT*.EFI`), и попал бы сюда как «его не собирали,
    // удалить устаревший» — то есть стёр бы только что скопированный загрузчик.
    for component in Component::SYSTEM {
        let dst = esp.join(component.esp_path(arch));
        match built.get(component) {
            Some(src) => {
                util::copy_file(src, &dst)?;
                println!("ESP: {} -> {}", src.display(), dst.display());
            }
            // Компонент не собирался (--no-kernel). Файл от прошлого запуска
            // надо убрать: иначе загрузчик подхватит устаревшее ядро, и то, что
            // мы собирались проверить без ядра, проверено не будет.
            None => {
                if dst.is_file() {
                    std::fs::remove_file(&dst)
                        .with_context(|| format!("не удалось удалить {}", dst.display()))?;
                    println!("ESP: удалён устаревший {}", dst.display());
                }
            }
        }
    }

    let initrd_dst = esp.join(arch::INITRD_ESP_FILE);
    match built.initrd() {
        Some(src) => {
            if util::copy_file_if_stale(src, &initrd_dst)? {
                println!("ESP: {} -> {}", src.display(), initrd_dst.display());
            } else {
                println!("ESP: {} уже актуален", initrd_dst.display());
            }
        }
        // Ровно та же логика, что и с ядром: `--no-initrd` бессмыслен, если
        // образ от прошлого запуска остаётся лежать на разделе.
        None => {
            if initrd_dst.is_file() {
                std::fs::remove_file(&initrd_dst)
                    .with_context(|| format!("не удалось удалить {}", initrd_dst.display()))?;
                println!("ESP: удалён устаревший {}", initrd_dst.display());
            }
        }
    }

    Ok(esp)
}

/// Аргумент `file=` для носителя.
fn drive_file(drive: &Drive) -> Result<String> {
    match drive {
        Drive::HostDirectory(path) => Ok(format!("fat:rw:{}", util::qemu_path(path)?)),
        Drive::Image(path) => util::qemu_path(path),
    }
}

/// Ищет бинарник qemu-system-* и объясняет, что делать, если его нет.
fn find_qemu(arch: Arch) -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(arch.qemu_env()) {
        let path = PathBuf::from(explicit);
        if !path.is_file() {
            bail!(
                "{} указывает на несуществующий файл: {}",
                arch.qemu_env(),
                path.display()
            );
        }
        return Ok(path);
    }

    let stem = arch.qemu_binary();
    if let Some(found) = util::which(stem) {
        return Ok(found);
    }

    // QEMU на Windows часто ставится без правки PATH.
    let mut probed: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
            if let Some(base) = std::env::var_os(var) {
                probed.push(PathBuf::from(base).join("qemu"));
            }
        }
        probed.push(PathBuf::from(r"C:\Program Files\qemu"));
        probed.push(PathBuf::from(r"C:\qemu"));
        probed.push(PathBuf::from(r"C:\msys64\mingw64\bin"));
    } else {
        probed.push(PathBuf::from("/usr/bin"));
        probed.push(PathBuf::from("/usr/local/bin"));
        probed.push(PathBuf::from("/opt/homebrew/bin"));
    }

    let file_name = util::exe_name(stem);
    for dir in &probed {
        let candidate = dir.join(&file_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let mut msg = format!("не найден {file_name}.\n\nИскал в PATH и в каталогах:\n");
    for dir in &probed {
        msg.push_str(&format!("  - {}\n", dir.display()));
    }
    msg.push_str(&format!(
        "\nЧто делать:\n\
         1) Установите QEMU (Windows: https://qemu.weilnetz.de/w64/) и добавьте \
            каталог установки в PATH.\n\
         2) Либо укажите бинарник явно:\n\
            PowerShell:  $env:{env} = \"C:\\Program Files\\qemu\\{file_name}\"\n\
            bash:        export {env}=/usr/bin/{stem}\n",
        env = arch.qemu_env(),
    ));
    bail!("{msg}");
}

/// Собрать командную строку QEMU, но не запускать.
///
/// Отделено от [`run`] ради стенда ([`crate::harness`]): ему нужен тот же
/// процесс, но с перехваченными потоками и подключённым монитором. Две
/// независимые сборки командной строки означали бы, что стенд проверяет не ту
/// машину, которую видит человек, — и расхождение обнаружилось бы в тот день,
/// когда тесты зелёные, а система не грузится.
pub fn command(opts: &RunOptions, built: &Built) -> Result<Command> {
    let arch = built.arch;
    let qemu = find_qemu(arch)?;

    let fw = firmware::resolve(arch, Some(qemu.as_path()))?;
    let fw = firmware::prepare(arch, &fw, opts.reset_nvram)?;
    println!("прошивка: {}", fw.description);

    let mut cmd = Command::new(&qemu);
    cmd.current_dir(paths::workspace_root());

    match arch {
        Arch::X86_64 => {
            cmd.args(["-machine", "q35"]);
            // Носители подключаются через virtio-blk, а не через штатный для
            // q35 контроллер AHCI, и это ради ядра, а не ради прошивки.
            // Драйвер диска в FreeOS один на обе архитектуры (см.
            // `kernel::virtio`), а AHCI на машине `virt` не существует вовсе;
            // оставить здесь AHCI значило бы, что на x86-64 ядро своего диска
            // не видит. Прошивка при этом ничего не теряет: VirtioBlkDxe есть
            // и в OVMF, и в ArmVirtQemu.
            for (index, drive) in opts.drives.iter().enumerate() {
                cmd.arg("-drive").arg(format!(
                    "if=none,id=disk{index},format=raw,file={}",
                    drive_file(drive)?
                ));
                cmd.args(["-device", &format!("virtio-blk-pci,drive=disk{index}")]);
            }
            // Видео на q35 есть по умолчанию (stdvga), а QemuVideoDxe в OVMF
            // отдаёт по нему GOP с честным линейным framebuffer'ом — именно то,
            // что загрузчик кладёт в boot-info. Отдельное -device не нужно.
        }
        Arch::Aarch64 => {
            cmd.args(["-machine", "virt"]);
            // virt по умолчанию поднимает cortex-a15 (32-битное ядро ARMv7),
            // на котором aarch64-прошивка просто не стартует.
            cmd.args(["-cpu", "cortex-a72"]);
            // У virt нет ни IDE, ни AHCI: block_default_type у машины остаётся
            // IF_IDE, и голый `-drive format=raw,...` завершился бы ошибкой
            // «machine type does not support if=ide». Поэтому подключаем диски
            // явно через virtio-blk-pci (VirtioBlkDxe есть в ArmVirtQemu).
            for (index, drive) in opts.drives.iter().enumerate() {
                cmd.arg("-drive").arg(format!(
                    "if=none,id=disk{index},format=raw,file={}",
                    drive_file(drive)?
                ));
                cmd.args(["-device", &format!("virtio-blk-pci,drive=disk{index}")]);
            }
            // Графика: на virt по умолчанию НЕТ видеоустройства вообще.
            //
            // Выбран ramfb, а не virtio-gpu-pci, и вот почему. В edk2 обе
            // железки поддержаны (ArmVirtQemu.dsc содержит и QemuRamfbDxe, и
            // VirtioGpuDxe), но GOP у них принципиально разный:
            //   * VirtioGpuDxe отдаёт Blt()-only GOP — Mode->FrameBufferBase
            //     остаётся нулевым, линейного буфера в адресном пространстве
            //     гостя не существует (это сделано сознательно, чтобы обойти
            //     проблемы когерентности кэша на aarch64/KVM);
            //   * QemuRamfbDxe выделяет reserved-страницы и кладёт их адрес в
            //     Mode->FrameBufferBase, формат PixelBlueGreenRedReserved8BitPerColor.
            // Нашему загрузчику нужен именно физический адрес framebuffer'а,
            // чтобы передать его ядру в boot-info и рисовать после ExitBootServices,
            // когда GOP->Blt() уже вызывать нельзя. Значит — ramfb.
            cmd.args(["-device", "ramfb"]);
        }
    }

    // USB-контроллер с клавиатурой — одинаково на обеих архитектурах, и это
    // главное. `qemu-xhci` — это NEC/Renesas-совместимый xHCI на PCIe, тот же
    // класс железа, что VL805 на Raspberry Pi 4, поэтому драйвер отлаживается
    // здесь и уезжает на плату без переписывания.
    //
    // На aarch64 это единственный способ вообще что-нибудь набрать: у машины
    // `virt` нет ни PS/2, ни любого другого legacy-ввода. На x86-64 контроллер
    // добавляется тоже, и не ради симметрии, а чтобы драйвер проверялся на обеих
    // архитектурах: PS/2 там продолжает работать рядом, и оба источника событий
    // складывают их в одну очередь.
    cmd.args(["-device", "qemu-xhci,id=xhci"]);
    cmd.args(["-device", "usb-kbd,bus=xhci.0"]);
    // Мышь — второе устройство на том же контроллере, и именно поэтому она
    // здесь: `usb-mouse` говорит на том же boot-протоколе HID, что и клавиатура,
    // но занимает **свой** слот и своё кольцо. Одно устройство на контроллер
    // драйвер обслуживал бы вдвое меньшим кодом — и молча отдавал бы отчёты
    // мыши разборщику клавиатуры.
    //
    // Не `usb-tablet`: планшет отдаёт абсолютные координаты и требует разбора
    // HID Report Descriptor, которого драйвер не делает. Обычная мышь шлёт три
    // байта фиксированного вида — ровно то, что описано в приложении B.2.
    cmd.args(["-device", "usb-mouse,bus=xhci.0"]);

    cmd.arg("-m").arg(&opts.memory);
    cmd.args(&fw.args);
    // Вывод ядра/загрузчика идёт в серийный порт: он одинаково работает на обеих
    // архитектурах и в headless-режиме CI.
    match &opts.serial {
        Serial::Stdio => cmd.args(["-serial", "stdio"]),
        Serial::Socket(addr) => cmd.args(["-serial", &format!("tcp:{addr}")]),
    };
    if let Some(addr) = opts.monitor {
        cmd.args(["-monitor", &format!("tcp:{addr}")]);
    }
    // Тройная ошибка не должна уводить VM в бесконечный цикл перезагрузок.
    cmd.arg("-no-reboot");
    // Сеть на Phase 0 не нужна, а её отсутствие ещё и экономит время на попытках
    // PXE-загрузки в UEFI.
    cmd.args(["-net", "none"]);

    if opts.serial_only {
        cmd.args(["-display", "none"]);
    }

    if opts.gdb {
        // -s: gdbstub на tcp::1234, -S: не запускать CPU до команды отладчика.
        cmd.args(["-s", "-S"]);
    }

    cmd.args(&opts.extra);
    Ok(cmd)
}

pub fn run(opts: &RunOptions, built: &Built) -> Result<()> {
    let arch = built.arch;
    let mut cmd = command(opts, built)?;

    if opts.gdb {
        print_gdb_hint(arch);
    }

    println!();
    util::run(&mut cmd, &format!("QEMU ({arch})"))?;
    Ok(())
}

fn print_gdb_hint(arch: Arch) {
    let gdb = match arch {
        Arch::X86_64 => "gdb",
        Arch::Aarch64 => "gdb-multiarch (или aarch64-none-elf-gdb)",
    };
    let arch_cmd = match arch {
        Arch::X86_64 => "set architecture i386:x86-64:intel",
        Arch::Aarch64 => "set architecture aarch64",
    };

    println!();
    println!("--- отладка ---");
    println!("QEMU остановлен до первой инструкции, gdbstub слушает tcp::1234.");
    println!("В другом терминале:");
    println!("    {gdb}");
    println!("    (gdb) {arch_cmd}");
    println!("    (gdb) target remote localhost:1234");
    println!("    (gdb) continue");
    println!();
    println!(
        "Символы: .efi — это PE, и прошивка перемещает его по произвольному адресу.\n\
         Загрузчик печатает свой image base в серийную консоль; когда увидите адрес:\n\
             (gdb) add-symbol-file target/<triple>/<profile>/boot-uefi.efi <base + .text RVA>"
    );
    println!();
    println!(
        "Ядро — ELF (и файл без расширения: так устроены таргеты *-unknown-none),\n\
         но собрано как PIE, поэтому адрес тоже берётся из вывода загрузчика:\n\
             (gdb) add-symbol-file target/<triple>/<profile>/kernel <load addr>"
    );
    println!("---------------");
}
