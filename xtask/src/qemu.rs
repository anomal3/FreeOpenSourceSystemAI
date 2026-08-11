//! Подготовка ESP и запуск QEMU.

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

    for component in Component::ALL {
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

pub fn run(opts: &RunOptions, built: &Built) -> Result<()> {
    let arch = built.arch;
    let qemu = find_qemu(arch)?;
    let esp = prepare_esp(built)?;

    let fw = firmware::resolve(arch, Some(qemu.as_path()))?;
    let fw = firmware::prepare(arch, &fw, opts.reset_nvram)?;
    println!("прошивка: {}", fw.description);

    let esp_arg = format!("fat:rw:{}", util::qemu_path(&esp)?);

    let mut cmd = Command::new(&qemu);
    cmd.current_dir(paths::workspace_root());

    match arch {
        Arch::X86_64 => {
            cmd.args(["-machine", "q35"]);
            // Без if= драйв уезжает на дефолтный интерфейс машины (для q35 это
            // AHCI) — классическая и хорошо проверенная связка OVMF + VVFAT.
            cmd.arg("-drive").arg(format!("format=raw,file={esp_arg}"));
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
            // «machine type does not support if=ide». Поэтому подключаем диск
            // явно через virtio-blk-pci (VirtioBlkDxe есть в ArmVirtQemu).
            cmd.arg("-drive")
                .arg(format!("if=none,id=esp,format=raw,file={esp_arg}"));
            cmd.args(["-device", "virtio-blk-pci,drive=esp"]);
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

    cmd.arg("-m").arg(&opts.memory);
    cmd.args(&fw.args);
    // Вывод ядра/загрузчика идёт в серийный порт: он одинаково работает на обеих
    // архитектурах и в headless-режиме CI.
    cmd.args(["-serial", "stdio"]);
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
