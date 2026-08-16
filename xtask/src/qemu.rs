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
    /// Адрес для QMP. Нужен только там, где указатель абсолютный: послать
    /// абсолютное событие через HMP нечем (см. [`crate::harness`]).
    pub qmp: Option<SocketAddr>,
    /// Какой манипулятор подключён к машине.
    pub pointer: Pointer,
    /// Каким контроллером подключены клавиатура и манипулятор.
    pub usb: UsbController,
    /// Чем подключены диски.
    pub disk_bus: DiskBus,
    /// Подключена ли к машине сетевая карта.
    pub network: bool,
    /// Проброс порта: `(порт на хосте, порт в госте)`.
    ///
    /// Единственный способ достучаться до гостя снаружи через SLIRP: сеть за
    /// трансляцией адресов, и входящих соединений в ней не бывает — кроме тех,
    /// о которых попросили заранее.
    pub hostfwd: Option<(u16, u16)>,
    /// Разрешить машине перезагружаться.
    ///
    /// По умолчанию QEMU запускается с `-no-reboot`, и это защита: перезагрузка,
    /// которую никто не заказывал, — это тройная ошибка, и без флага она
    /// превратилась бы в бесконечный цикл загрузок вместо внятного отказа.
    /// Сценарию, который проверяет саму перезагрузку, флаг мешает: с ним QEMU
    /// завершается ровно там, где машина должна подняться заново.
    pub allow_reboot: bool,
}

/// Каким контроллером подключены носители.
///
/// Выбор проверяет ядро, а не QEMU. До Phase 26a дисковый драйвер в FreeOS был
/// один — virtio-blk, — и стенд подключал диски только им; это означало, что
/// путь «система находит свой корень» проверялся ровно на том контроллере,
/// которого нет ни в VirtualBox по умолчанию, ни в реальном компьютере. AHCI
/// здесь — не другая настройка эмулятора, а другой драйвер в ядре, и сценарии с
/// ним проверяют именно его.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiskBus {
    Virtio,
    Ahci,
    Nvme,
    /// NVMe с сектором 4096 байт — «4Kn».
    ///
    /// Не настройка эмулятора ради разнообразия: такой диск ломает всё, где
    /// размер сектора был вписан числом. Заголовок GPT лежит по другому
    /// адресу, суперблок ext2 попадает внутрь первого сектора, а FAT обязан
    /// объявить `BytsPerSec = 4096`. Ровно поэтому ограничение «только 512»
    /// держалось до Phase 26c с пометкой «проверить нечем» — а проверить, как
    /// выяснилось, есть чем.
    Nvme4k,
}

/// Манипулятор виртуальной машины.
///
/// Выбор не косметический: это два разных класса устройств, и ядро читает их
/// по-разному. Мышь объявляет boot-протокол и шлёт приращения; планшет не
/// объявляет ничего и шлёт координаты, поэтому понять его можно только разобрав
/// дескриптор отчётов. VirtualBox предлагает по умолчанию именно планшет — и
/// это причина, по которой он здесь появился.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    Mouse,
    Tablet,
}

/// Каким контроллером подключены устройства ввода.
///
/// Выбор проверяет ядро, а не эмулятор: это два разных драйвера. Появился он не
/// ради полноты — у VirtualBox по умолчанию включён **только** OHCI, и система,
/// умеющая один xHCI, на машине читателя не слушается ни клавиатуры, ни мыши.
/// `pci-ohci` в QEMU — тот же класс контроллера, что там эмулируется, поэтому
/// драйвер отлаживается здесь, а не на чужой машине без журнала.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsbController {
    Xhci,
    Ohci,
}

impl UsbController {
    /// Имя шины QEMU, к которой подключаются устройства.
    const fn bus(self) -> &'static str {
        match self {
            Self::Xhci => "xhci.0",
            Self::Ohci => "ohci.0",
        }
    }

    /// Само устройство контроллера.
    const fn device(self) -> &'static str {
        match self {
            Self::Xhci => "qemu-xhci,id=xhci",
            Self::Ohci => "pci-ohci,id=ohci",
        }
    }
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
            qmp: None,
            pointer: Pointer::Mouse,
            usb: UsbController::Xhci,
            disk_bus: DiskBus::Virtio,
            network: false,
            hostfwd: None,
            allow_reboot: false,
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
    /// Загрузочный ISO, подключаемый приводом.
    ///
    /// Отдельно от [`Drive::Image`], потому что подключается иначе: не
    /// virtio-blk, а привод — тот же способ, каким его подключит человек в
    /// VirtualBox. Смысл проверки в этом и состоит: прошивка обязана найти на
    /// нём загрузочную запись El Torito, а не файловую систему.
    Cdrom(PathBuf),
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
                say!("ESP: {} -> {}", src.display(), dst.display());
            }
            // Компонент не собирался (--no-kernel). Файл от прошлого запуска
            // надо убрать: иначе загрузчик подхватит устаревшее ядро, и то, что
            // мы собирались проверить без ядра, проверено не будет.
            None => {
                if dst.is_file() {
                    std::fs::remove_file(&dst)
                        .with_context(|| format!("не удалось удалить {}", dst.display()))?;
                    say!("ESP: удалён устаревший {}", dst.display());
                }
            }
        }
    }

    let initrd_dst = esp.join(arch::INITRD_ESP_FILE);
    match built.initrd() {
        Some(src) => {
            if util::copy_file_if_stale(src, &initrd_dst)? {
                say!("ESP: {} -> {}", src.display(), initrd_dst.display());
            } else {
                say!("ESP: {} уже актуален", initrd_dst.display());
            }
        }
        // Ровно та же логика, что и с ядром: `--no-initrd` бессмыслен, если
        // образ от прошлого запуска остаётся лежать на разделе.
        None => {
            if initrd_dst.is_file() {
                std::fs::remove_file(&initrd_dst)
                    .with_context(|| format!("не удалось удалить {}", initrd_dst.display()))?;
                say!("ESP: удалён устаревший {}", initrd_dst.display());
            }
        }
    }

    // Метка «за машиной никого нет». Лежит только на ESP, который собирает
    // стенд, и никогда — на носителе, который получает человек: по ней ядро
    // отличает прогон от работы и закрывает сеанс по простою.
    let autorun = esp.join("FREEOS").join("AUTORUN.CFG");
    if let Some(parent) = autorun.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    std::fs::write(&autorun, b"unattended
")
        .with_context(|| format!("не удалось записать {}", autorun.display()))?;

    Ok(esp)
}

/// Аргумент `file=` для носителя.
fn drive_file(drive: &Drive) -> Result<String> {
    match drive {
        Drive::HostDirectory(path) => Ok(format!("fat:rw:{}", util::qemu_path(path)?)),
        Drive::Image(path) | Drive::Cdrom(path) => util::qemu_path(path),
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
/// Добавить контроллер AHCI, если диски подключаются им.
///
/// Контроллер один на все диски: у него 32 порта, и второй экземпляр
/// понадобился бы только чтобы проверить, что мы умеем искать по двум
/// контроллерам, — а этого сегодня никто не обещает.
///
/// `ich9-ahci` — то же устройство, что стоит на настоящей материнской плате с
/// чипсетом ICH9, и то же, что VirtualBox показывает как контроллер SATA.
/// На `virt` его нет по умолчанию, но шина PCIe там есть, поэтому он
/// подключается одинаково на обеих архитектурах.
fn add_ahci_controller(cmd: &mut Command, opts: &RunOptions) {
    if opts.disk_bus != DiskBus::Ahci {
        return;
    }
    if opts.drives.iter().all(|drive| matches!(drive, Drive::Cdrom(_))) {
        return;
    }
    cmd.args(["-device", "ich9-ahci,id=ahci"]);
}

/// Подключить носитель выбранным контроллером.
///
/// Каталог хоста (VVFAT) подключается virtio всегда, каким бы ни был выбор.
/// Это не исключение ради удобства: с него грузится **прошивка**, а
/// `ArmVirtQemu` не умеет SATA вовсе — диск на AHCI она не видит и уходит в свою
/// оболочку с `map: No mapping found`. Драйвер же, который мы проверяем, живёт в
/// ядре, а не в прошивке, поэтому загрузка идёт тем путём, который работает
/// везде, а проверяемый диск подключается тем контроллером, который проверяется.
fn attach_disk(
    cmd: &mut Command,
    index: usize,
    drive: &Drive,
    bus: DiskBus,
    ahci_port: &mut usize,
    multiple: bool,
) -> Result<()> {
    let bus = match drive {
        // Каталог хоста подключается virtio всегда: с него грузится прошивка,
        // а `ArmVirtQemu` не умеет ни SATA, ни 4Kn-носитель.
        Drive::HostDirectory(_) => DiskBus::Virtio,
        // Первый носитель из нескольких — тоже загрузочный (установочный
        // образ), и его шину менять нельзя по той же причине. Проверяемый диск
        // в таких сценариях всегда последний.
        _ if index == 0 && multiple => DiskBus::Virtio,
        _ => bus,
    };
    cmd.arg("-drive").arg(format!(
        "if=none,id=disk{index},format=raw,file={}",
        drive_file(drive)?
    ));
    match bus {
        DiskBus::Virtio => {
            cmd.args(["-device", &format!("virtio-blk-pci,drive=disk{index}")]);
        }
        // `ide-hd` на шине AHCI — это диск SATA: контроллер тот же, что у
        // человека, и прошивка находит на нём ESP своим собственным драйвером,
        // а ядро — своим новым.
        //
        // Порт считается отдельно от номера носителя: занятыми должны быть
        // порты с нулевого подряд, иначе сценарий, который ждёт «port 0»,
        // зависел бы от того, сколько дисков подключено другим контроллером.
        DiskBus::Ahci => {
            cmd.args([
                "-device",
                &format!("ide-hd,drive=disk{index},bus=ahci.{ahci_port}"),
            ]);
            *ahci_port += 1;
        }
        // Серийный номер обязателен: без него QEMU предупреждает, а некоторые
        // прошивки отказываются перечислять устройство вовсе.
        DiskBus::Nvme => {
            cmd.args([
                "-device",
                &format!("nvme,drive=disk{index},serial=freeos{index}"),
            ]);
        }
        DiskBus::Nvme4k => {
            let device = format!(
                "nvme,drive=disk{index},serial=freeos{index},logical_block_size=4096,physical_block_size=4096"
            );
            cmd.args(["-device", &device]);
        }
    }
    Ok(())
}

pub fn command(opts: &RunOptions, built: &Built) -> Result<Command> {
    let arch = built.arch;
    let qemu = find_qemu(arch)?;

    let fw = firmware::resolve(arch, Some(qemu.as_path()))?;
    let fw = firmware::prepare(arch, &fw, opts.reset_nvram)?;
    say!("прошивка: {}", fw.description);

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
            add_ahci_controller(&mut cmd, opts);
            let mut ahci_port = 0usize;
            let multiple_disks = opts.drives.len() > 1;
            for (index, drive) in opts.drives.iter().enumerate() {
                if let Drive::Cdrom(path) = drive {
                    // Привод, а не virtio-blk: так его подключает человек, и так
                    // прошивка ищет на нём загрузочную запись El Torito.
                    cmd.args(["-cdrom", &util::qemu_path(path)?]);
                    continue;
                }
                attach_disk(&mut cmd, index, drive, opts.disk_bus, &mut ahci_port, multiple_disks)?;
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
            add_ahci_controller(&mut cmd, opts);
            let mut ahci_port = 0usize;
            let multiple_disks = opts.drives.len() > 1;
            for (index, drive) in opts.drives.iter().enumerate() {
                if let Drive::Cdrom(path) = drive {
                    // На `virt` привода нет вовсе, поэтому ISO подключается как
                    // устройство SCSI CD-ROM. Для прошивки разницы нет: она
                    // ищет El Torito на любом блочном устройстве, объявившем
                    // себя оптическим.
                    cmd.arg("-drive").arg(format!(
                        "if=none,id=cd{index},format=raw,media=cdrom,file={}",
                        util::qemu_path(path)?
                    ));
                    cmd.args(["-device", "virtio-scsi-pci,id=scsi"]);
                    cmd.args(["-device", &format!("scsi-cd,drive=cd{index}")]);
                    continue;
                }
                attach_disk(&mut cmd, index, drive, opts.disk_bus, &mut ahci_port, multiple_disks)?;
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
    cmd.args(["-device", opts.usb.device()]);
    cmd.args(["-device", &format!("usb-kbd,bus={}", opts.usb.bus())]);
    // Манипулятор — второе устройство на том же контроллере, и именно поэтому
    // он здесь: он занимает **свой** слот и своё кольцо. Одно устройство на
    // контроллер драйвер обслуживал бы вдвое меньшим кодом — и молча отдавал бы
    // отчёты мыши разборщику клавиатуры.
    //
    // По умолчанию мышь: она говорит на boot-протоколе, как и клавиатура.
    // Планшет отдаёт абсолютные координаты и boot-протокола не объявляет вовсе
    // — то есть проверяет совсем другой путь в драйвере, тот самый, которым
    // система живёт в VirtualBox.
    match opts.pointer {
        Pointer::Mouse => cmd.args(["-device", &format!("usb-mouse,bus={}", opts.usb.bus())]),
        Pointer::Tablet => cmd.args(["-device", &format!("usb-tablet,bus={}", opts.usb.bus())]),
    };

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
    if let Some(addr) = opts.qmp {
        cmd.args(["-qmp", &format!("tcp:{addr}")]);
    }
    // Тройная ошибка не должна уводить VM в бесконечный цикл перезагрузок.
    if !opts.allow_reboot {
        cmd.arg("-no-reboot");
    }
    if opts.network {
        // Пользовательская сеть (SLIRP): полноценный стек хоста, живущий внутри
        // процесса QEMU. Ценность его именно в том, что он **чужой** — ARP,
        // IPv4 и ICMP разбирает не наш код, и всё, в чём мы ошиблись, он молча
        // отбрасывает, а не прощает.
        //
        // Раскладка сети задана QEMU и постоянна: гость — `10.0.2.15`, шлюз и
        // он же ответчик на `ping` — `10.0.2.2`, сервер имён — `10.0.2.3`.
        // Именно эти адреса стоят в сценариях, и это не выдуманные числа, а
        // чужой договор, который мы обязаны соблюсти.
        //
        // Оговорка, которую стоит держать в голове: снаружи внутрь ICMP через
        // SLIRP не проходит вовсе, поэтому пропинговать гостя с хоста нельзя ни
        // при каких настройках. Проверяется поэтому обратное направление.
        let netdev = match opts.hostfwd {
            Some((host, guest)) => {
                format!("user,id=net0,hostfwd=tcp:127.0.0.1:{host}-:{guest}")
            }
            None => "user,id=net0".to_string(),
        };
        cmd.args(["-netdev", &netdev]);
        cmd.args(["-device", "virtio-net-pci,netdev=net0"]);
    } else {
        // Без сети машина не тратит время на попытки PXE-загрузки в UEFI.
        cmd.args(["-net", "none"]);
    }

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

    say!();
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

    say!();
    say!("--- отладка ---");
    say!("QEMU остановлен до первой инструкции, gdbstub слушает tcp::1234.");
    say!("В другом терминале:");
    say!("    {gdb}");
    say!("    (gdb) {arch_cmd}");
    say!("    (gdb) target remote localhost:1234");
    say!("    (gdb) continue");
    say!();
    say!(
        "Символы: .efi — это PE, и прошивка перемещает его по произвольному адресу.\n\
         Загрузчик печатает свой image base в серийную консоль; когда увидите адрес:\n\
             (gdb) add-symbol-file target/<triple>/<profile>/boot-uefi.efi <base + .text RVA>"
    );
    say!();
    say!(
        "Ядро — ELF (и файл без расширения: так устроены таргеты *-unknown-none),\n\
         но собрано как PIE, поэтому адрес тоже берётся из вывода загрузчика:\n\
             (gdb) add-symbol-file target/<triple>/<profile>/kernel <load addr>"
    );
    say!("---------------");
}
