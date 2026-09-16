//! Перепись устройств — то, что показывает диспетчер устройств (фаза С7).
//!
//! # Откуда берутся сведения
//!
//! Отдельного реестра устройств в ядре не было: каждый драйвер сам искал на
//! шине своё, поднимал и говорил об этом одной строкой журнала. Человеку с
//! Windows нужен ответ на другой вопрос — что стоит в машине, чем оно
//! обслуживается и у чего драйвера нет, — и этот модуль собирает его из
//! четырёх источников, ничего не храня сверх необходимого:
//!
//! * **PCI** обходится заново на каждый вопрос. Запомненный список устарел бы
//!   на первом же горячем подключении, а обход шины — это десятки чтений
//!   конфигурационного пространства, не работа.
//! * **Кто что поднял** — отметки драйверов ([`claim`]), по адресу функции.
//!   Отметка ставится там, где устройство действительно заработало, а не где
//!   найдено: «нашёл, но не поднял» — это ровно тот случай, ради которого окно
//!   открывают.
//! * **USB** — сводки драйверов xHCI и OHCI, те же, что печатает оболочка.
//! * **Диски** — сводка, запомненная при загрузке ([`remember_disk`]): после
//!   неё носители уходят в разбор разделов, и спросить их второй раз не у кого.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::pci;
use crate::sync::SpinLock;

/// Сколько отметок драйверов помещается. Массив, а не `Vec`: отметку ставят
/// из пути запуска драйвера, и выделять память под замком там незачем.
const CLAIMS_MAX: usize = 32;

/// Сколько дисков помнит сводка.
const DISKS_MAX: usize = 16;

static CLAIMS: SpinLock<[Option<(pci::Address, &'static str)>; CLAIMS_MAX]> =
    SpinLock::new([None; CLAIMS_MAX]);

#[derive(Clone, Copy)]
struct Disk {
    kind: &'static str,
    unit: usize,
    sectors: u64,
}

static DISKS: SpinLock<[Option<Disk>; DISKS_MAX]> = SpinLock::new([None; DISKS_MAX]);

/// Отметить устройство на шине PCI как поднятое драйвером `driver`.
///
/// Повторная отметка того же адреса ничего не меняет: драйвер, поднимающий
/// устройство второй раз после сброса, остаётся тем же драйвером.
pub fn claim(address: pci::Address, driver: &'static str) {
    let mut claims = CLAIMS.lock();
    if claims.iter().flatten().any(|(known, _)| *known == address) {
        return;
    }
    if let Some(slot) = claims.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some((address, driver));
    }
}

fn claimed(address: pci::Address) -> Option<&'static str> {
    CLAIMS
        .lock()
        .iter()
        .flatten()
        .find(|(known, _)| *known == address)
        .map(|(_, driver)| *driver)
}

/// Запомнить найденный при загрузке диск.
pub fn remember_disk(kind: &'static str, unit: usize, sectors: u64) {
    let mut disks = DISKS.lock();
    if let Some(slot) = disks.iter_mut().find(|slot| slot.is_none()) {
        *slot = Some(Disk { kind, unit, sectors });
    }
}

/// Что это за устройство — по классу. Слова английские: это договор с
/// программой, переводит она.
fn class_name(class: u8, subclass: u8, prog_if: u8) -> &'static str {
    match (class, subclass, prog_if) {
        (0x01, 0x00, _) => "scsi storage controller",
        (0x01, 0x01, _) => "ide controller",
        (0x01, 0x06, 0x01) => "sata controller (ahci)",
        (0x01, 0x06, _) => "sata controller",
        (0x01, 0x08, 0x02) => "nvme controller",
        (0x01, _, _) => "storage controller",
        (0x02, 0x00, _) => "ethernet controller",
        (0x02, _, _) => "network controller",
        (0x03, 0x00, _) => "vga display",
        (0x03, _, _) => "display controller",
        (0x04, _, _) => "multimedia controller",
        (0x05, _, _) => "memory controller",
        (0x06, 0x00, _) => "host bridge",
        (0x06, 0x01, _) => "isa bridge",
        (0x06, 0x04, _) => "pci bridge",
        (0x06, _, _) => "bridge",
        (0x07, _, _) => "communication controller",
        (0x08, _, _) => "system peripheral",
        (0x09, _, _) => "input controller",
        (0x0C, 0x03, 0x00) => "usb controller (uhci)",
        (0x0C, 0x03, 0x10) => "usb controller (ohci)",
        (0x0C, 0x03, 0x20) => "usb controller (ehci)",
        (0x0C, 0x03, 0x30) => "usb controller (xhci)",
        (0x0C, 0x03, _) => "usb controller",
        (0x0C, 0x05, _) => "smbus controller",
        (0x0C, _, _) => "serial bus controller",
        (0x00, _, _) => "unclassified device",
        _ => "other device",
    }
}

/// Драйвер устройства и его состояние.
///
/// Состояния: `active` — драйвер поднял устройство; `idle` — драйвер для
/// такого устройства в ядре есть, но этим устройством он не занят (второй
/// контроллер, адаптер экрана до первой смены режима, устройство, которое не
/// поднялось); `none` — драйвера нет; `not-needed` — мосты: их настраивает
/// прошивка, и «нет драйвера» у них означало бы тревогу на ровном месте.
fn driver_for(device: &pci::Device) -> (&'static str, &'static str) {
    if let Some(driver) = claimed(device.address) {
        return (driver, "active");
    }
    let known = match (device.vendor, device.device, device.class, device.subclass, device.prog_if) {
        (pci::VENDOR_VIRTIO, pci::DEVICE_VIRTIO_BLK_LEGACY | pci::DEVICE_VIRTIO_BLK_MODERN, ..) => {
            Some("virtio-blk")
        }
        (pci::VENDOR_VIRTIO, pci::DEVICE_VIRTIO_NET_LEGACY | pci::DEVICE_VIRTIO_NET_MODERN, ..) => {
            Some("virtio-net")
        }
        (vendor, model, ..) if crate::net::e1000::supports(vendor, model) => Some("e1000"),
        (vendor, model, ..) if crate::net::atl1c::supports(vendor, model) => Some("atl1c"),
        (0x1234, 0x1111, ..) => Some("bochs vbe"),
        (_, _, 0x01, 0x06, 0x01) => Some("ahci"),
        (_, _, 0x01, 0x08, 0x02) => Some("nvme"),
        (_, _, 0x0C, 0x03, 0x30) => Some("xhci"),
        (_, _, 0x0C, 0x03, 0x20) => Some("ehci"),
        (_, _, 0x0C, 0x03, 0x10) => Some("ohci"),
        _ => None,
    };
    match known {
        // Драйверы USB отметок не ставят: у них своя сводка, и она же отвечает,
        // работает ли контроллер. Контроллер у машины один — на двух оба
        // окажутся «active», и это названный предел, а не догадка.
        Some("xhci") if crate::usb::xhci::summary().is_some() => ("xhci", "active"),
        Some("ohci") if crate::usb::ohci::summary().is_some() => ("ohci", "active"),
        Some("ehci") if crate::usb::ehci::summary().is_some() => ("ehci", "active"),
        Some(driver) => (driver, "idle"),
        None if device.class == 0x06 => ("-", "not-needed"),
        None => ("-", "none"),
    }
}

/// Перепись текстом — ответ `SYS_DEVICES`. Договор строк — у
/// `user_abi::SYS_DEVICES`.
pub fn report() -> String {
    let mut out = String::new();

    let rsdp = crate::acpi::rsdp();
    if rsdp != 0 {
        // SAFETY: RSDP из хэндоффа, прямое отображение активно; обход шины
        // только читает конфигурационное пространство.
        if let Ok(root) = unsafe { pci::Root::discover(rsdp) } {
            // Сначала собрать, потом спрашивать драйверы: сводка USB берёт
            // замок контроллера, и держать под ним обход шины незачем.
            let mut found: Vec<pci::Device> = Vec::new();
            // SAFETY: см. выше.
            unsafe {
                pci::for_each(&root, |device| {
                    found.push(*device);
                    true
                });
            }
            for device in &found {
                let (driver, state) = driver_for(device);
                let _ = writeln!(
                    out,
                    "pci\t{}\t{:04x}:{:04x}\t{}\t{driver}\t{state}",
                    device.address,
                    device.vendor,
                    device.device,
                    class_name(device.class, device.subclass, device.prog_if),
                );
            }
        }
    }

    if let Some(summary) = crate::usb::xhci::summary() {
        usb_lines(&mut out, "xhci", &summary.attached);
    }
    if let Some(summary) = crate::usb::ohci::summary() {
        usb_lines(&mut out, "ohci", &summary.attached);
    }
    if let Some(summary) = crate::usb::ehci::summary() {
        usb_lines(&mut out, "ehci", &summary.attached);
    }

    for disk in DISKS.lock().iter().flatten() {
        let _ = writeln!(
            out,
            "disk\t{} #{}\t{} MiB\tdisk\t{}\tactive",
            disk.kind,
            disk.unit,
            disk.sectors / 2048,
            disk.kind
        );
    }
    out
}

/// Перепись шины словами — для журнала загрузки и для команды `pci`.
///
/// # Почему один текст на двоих
///
/// Потому что на чужой машине свидетельство одно — фотография экрана. Строка в
/// журнале загрузки и строка, которую печатает оболочка, обязаны совпадать
/// дословно: иначе человек фотографирует одно, а я ищу в коде другое. Ноутбук
/// ASUS K53SD стоил двух ночей ровно потому, что список устройств в системе был,
/// а идентификаторов в нём не было: класс «сетевой адаптер» не говорит, какой
/// драйвер писать, а `1969:1083` говорит.
#[must_use]
pub fn census_text() -> String {
    let mut out = String::new();
    let rsdp = crate::acpi::rsdp();
    if rsdp == 0 {
        let _ = writeln!(out, "no ACPI tables, so no PCI bus here");
        return out;
    }
    // SAFETY: RSDP из хэндоффа, прямое отображение активно; обход шины только
    // читает конфигурационное пространство.
    let Ok(root) = (unsafe { pci::Root::discover(rsdp) }) else {
        let _ = writeln!(out, "no PCI bus on this machine");
        return out;
    };

    let mut found: Vec<pci::Device> = Vec::new();
    // SAFETY: см. выше.
    unsafe {
        pci::for_each(&root, |device| {
            found.push(*device);
            true
        });
    }

    let mut without = 0usize;
    for device in &found {
        let (driver, state) = driver_for(device);
        let verdict = match state {
            "active" => alloc::format!("{driver}, working"),
            "idle" => alloc::format!("{driver} is in this kernel, but not on this device"),
            "not-needed" => String::from("set up by the firmware, no driver needed"),
            _ => {
                without += 1;
                String::from("NO DRIVER IN THIS KERNEL")
            }
        };
        let _ = writeln!(
            out,
            "{} {:04x}:{:04x} class {:02x}:{:02x}:{:02x} {:<24} -- {verdict}",
            device.address,
            device.vendor,
            device.device,
            device.class,
            device.subclass,
            device.prog_if,
            class_name(device.class, device.subclass, device.prog_if),
        );
    }
    let _ = writeln!(out, "{} function(s) on the bus, {without} of them without a driver", found.len());
    out
}

/// Напечатать перепись в журнал загрузки.
///
/// Зовётся поздно — когда драйверы уже отметились: перепись, снятая до них,
/// объявила бы «драйвера нет» у всего сразу и этим соврала бы.
pub fn log_census() {
    crate::kprintln!();
    crate::kprintln!("---- devices ----------------------------------------------------");
    for line in census_text().lines() {
        crate::kprintln!("  bus         : {line}");
    }
}

fn usb_lines(out: &mut String, driver: &str, attached: &[crate::usb::Attached]) {
    for device in attached.iter().filter(|device| device.port != 0) {
        let _ = writeln!(
            out,
            "usb\tport {}\t{:04x}:{:04x}\t{}\t{driver}\tactive",
            device.port, device.vendor, device.product, device.kind
        );
    }
}
