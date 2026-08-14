//! Перепись контроллеров USB: что на шине есть и чем из этого мы умеем править.
//!
//! # Зачем это отдельно от драйвера
//!
//! Потому что дороже всего обходится не отсутствующий драйвер, а **молчание**.
//! Релиз `v0.1.57` на VirtualBox под macOS не слушался ни клавиатуры, ни мыши, и
//! система об этом не сказала ничего: драйвер у неё ровно один — xHCI, — а
//! контроллер там был другой. Со стороны это выглядело как «система зависла»,
//! и разбирательство заняло больше времени, чем занял бы сам драйвер.
//!
//! Перепись печатается **до** попытки поднять хоть что-нибудь и называет каждый
//! найденный контроллер вместе с приговором: умеем или нет. Человек за такой
//! машиной получает ответ на главный вопрос («почему не работает ввод?») из
//! первых десяти строк журнала, а не из отладчика.
//!
//! # Почему по `prog-if`, а не по изготовителю
//!
//! Потому что так это и задумано в PCI: контроллеры USB все до одного лежат в
//! классе `0x0C` и подклассе `0x03`, а различает их **программный интерфейс** —
//! `0x00` UHCI, `0x10` OHCI, `0x20` EHCI, `0x30` xHCI. Число это не про
//! изготовителя и не про поколение шины, а ровно про то, какими регистрами
//! контроллер управляется, — то есть про то единственное, что интересует
//! драйвер.

use crate::kprintln;
use crate::pci;

/// Program Interface: UHCI — USB 1.1 от Intel.
const PROG_IF_UHCI: u8 = 0x00;
/// Program Interface: OHCI — USB 1.1, всё остальное железо.
const PROG_IF_OHCI: u8 = 0x10;
/// Program Interface: EHCI — USB 2.0.
const PROG_IF_EHCI: u8 = 0x20;
/// Program Interface: устройство USB, а не хост-контроллер.
const PROG_IF_DEVICE: u8 = 0xFE;

/// Сколько контроллеров нашлось и сколько из них мы умеем поднять.
pub struct Census {
    pub found: usize,
    pub drivable: usize,
}

/// Пересчитать контроллеры USB и сказать про каждый, умеем мы его или нет.
///
/// # Safety
///
/// Та же, что у [`pci::for_each`]: таблицы ACPI обязаны быть целы, а окно ECAM —
/// отображено.
pub unsafe fn take(root: &pci::Root) -> Census {
    let mut census = Census { found: 0, drivable: 0 };

    // SAFETY: контракт функции.
    unsafe {
        pci::for_each(root, |device| {
            if device.class != pci::CLASS_SERIAL_BUS || device.subclass != pci::SUBCLASS_USB {
                return true;
            }
            census.found += 1;
            let (name, driven) = describe(device.prog_if);
            if driven {
                census.drivable += 1;
            }
            kprintln!(
                "  usb         : {} {name} (prog-if {:#04x}) vendor {:#06x} device {:#06x} -- {}",
                device.address,
                device.prog_if,
                device.vendor,
                device.device,
                if driven { "driven" } else { "no driver here" },
            );
            true
        });
    }

    if census.found == 0 {
        // Не ошибка: на машине может не быть шины PCI вовсе, а ввод прийти с
        // PS/2 или с серийной линии. Но сказать об этом надо — «контроллеров
        // нет» и «контроллер есть, а драйвера нет» лечатся по-разному.
        kprintln!("  usb         : no USB controller on the PCI bus");
    } else if census.drivable == 0 {
        kprintln!(
            "  usb         : {} controller(s), none of them driven by this kernel",
            census.found
        );
    }
    census
}

/// Имя программного интерфейса и умеем ли мы его.
const fn describe(prog_if: u8) -> (&'static str, bool) {
    match prog_if {
        PROG_IF_UHCI => ("uhci", false),
        PROG_IF_OHCI => ("ohci", false),
        PROG_IF_EHCI => ("ehci", false),
        pci::PROG_IF_XHCI => ("xhci", true),
        // `0xFE` — не хост-контроллер, а само устройство USB, подключённое к
        // шине PCI. Драйвера ему не бывает по определению: он не управляет
        // ничем, он и есть то, чем управляют.
        PROG_IF_DEVICE => ("usb device, not a controller", false),
        _ => ("unknown", false),
    }
}
