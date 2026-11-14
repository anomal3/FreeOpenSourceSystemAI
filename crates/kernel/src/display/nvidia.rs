//! Видеокарты NVIDIA: этапы 1–2 плана (13.11.2026, ROADMAP «Видеокарты
//! NVIDIA»): найти карту, отобразить её регистры (BAR0) и прочитать, что за
//! чип, сколько памяти и в каком состоянии защищённая область прошивки.
//!
//! # Только чтение
//!
//! Картинку на экране сейчас держит прошивка (GOP) — через эту же карту.
//! Здесь ничего не пишется ни в карту, ни в её конфигурационное пространство:
//! даже замер размеров BAR выключал бы на миг декодирование памяти, а с ним и
//! экран. Запуск карты (FWSEC, GSP-RM) — следующие этапы.
//!
//! # Источники
//!
//! Только открытое (решение Романа 13.11): раскладка `NV_PMC_BOOT_0`/`BOOT_42`
//! — `open-gpu-doc/manuals/.../dev_master.ref.txt`; адреса регистров памяти и
//! WPR2 — как их читает nouveau (`nvkm/subdev/fb`, `nvkm/subdev/gsp/ga102.c`)
//! и open-gpu-kernel-modules NVIDIA (MIT).

use crate::mm::PageFlags;
use crate::sync::SpinLock;

const VENDOR: u16 = 0x10de;
/// Окно регистров (BAR0) — 16 МиБ у всех карт со времён Fermi.
const BAR0_LEN: usize = 16 * 1024 * 1024;

const NV_PMC_BOOT_0: usize = 0x0000_0000;
const NV_PMC_BOOT_42: usize = 0x0000_0a00;
/// Размер видеопамяти (`NV_PFB_PRI_MMU_LOCAL_MEMORY_RANGE`, Turing и новее).
const NV_PFB_LOCAL_MEMORY_RANGE: usize = 0x0010_0ce0;
/// Границы WPR2 — защищённой области, которую строит FWSEC-FRTS (Ampere).
const NV_PFB_WPR2_ADDR_LO: usize = 0x001f_a824;
const NV_PFB_WPR2_ADDR_HI: usize = 0x001f_a828;

/// Что узнали о карте — для команды оболочки `gpu`.
#[derive(Clone, Copy)]
pub struct Found {
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
    pub device: u16,
    pub revision: u8,
    pub boot0: u32,
    pub boot42: u32,
    pub memory_range: u32,
    pub wpr2: (u32, u32),
    pub bar0: u64,
}

static FOUND: SpinLock<Option<Found>> = SpinLock::new(None);

/// Найденная карта, если есть.
#[must_use]
pub fn found() -> Option<Found> {
    *FOUND.lock()
}

/// Семейство по полю `architecture` (`BOOT_42` биты 28:24).
#[must_use]
pub fn family(arch: u32) -> &'static str {
    match arch {
        0x16 => "Turing",
        0x17 => "Ampere",
        0x19 => "Ada",
        0x1a => "Hopper",
        0x1b => "Blackwell",
        0x14 => "Volta",
        0x13 => "Pascal",
        0x11 | 0x12 => "Maxwell",
        0x0e | 0x0f => "Kepler",
        _ => "unknown",
    }
}

/// Имя чипа по `chip_id` (биты 28:20): то, что пишут на коробке.
#[must_use]
pub fn chip_name(chip: u32) -> &'static str {
    match chip {
        0x172 => "GA102",
        0x173 => "GA103",
        0x174 => "GA104",
        0x176 => "GA106",
        0x177 => "GA107",
        0x170 => "GA100",
        0x162 => "TU102",
        0x164 => "TU104",
        0x166 => "TU106",
        0x167 => "TU117",
        0x168 => "TU116",
        0x192 => "AD102",
        0x193 => "AD103",
        0x194 => "AD104",
        0x196 => "AD106",
        0x197 => "AD107",
        _ => "?",
    }
}

/// Разобрать `chip_id` и ревизию из `BOOT_0`/`BOOT_42`: у новых карт
/// `BOOT_0` может нести ноль в архитектуре — тогда правда в `BOOT_42`.
#[must_use]
pub fn chip_of(boot0: u32, boot42: u32) -> (u32, u32) {
    let word = if boot0 & 0x1f00_0000 != 0 { boot0 } else { boot42 };
    let chip = (word >> 20) & 0x1ff;
    let revision = word & 0xff;
    (chip, revision)
}

/// Найти карту NVIDIA и прочитать о ней главное — один раз, на загрузке.
///
/// # Safety
///
/// Ядро работает на своих таблицах страниц; `root` — живая шина PCI.
pub unsafe fn probe(root: &crate::pci::Root) {
    let mut found = None;
    // SAFETY: обход шины только читает конфигурационное пространство.
    unsafe {
        crate::pci::for_each(root, |device| {
            if device.vendor == VENDOR && device.class == 0x03 {
                found = Some(*device);
                return false;
            }
            true
        });
    }
    let Some(device) = found else { return };
    let address = device.address;
    crate::kprintln!(
        "  nvidia      : {:02x}:{:02x}.{} device {:04x} rev {:02x} (display class {:02x}{:02x})",
        address.bus,
        address.device,
        address.function,
        device.device,
        device.revision,
        device.class,
        device.subclass
    );
    let Some(phys) = device.memory_bar(0) else {
        crate::kprintln!("  nvidia      : no register window (BAR0) - nothing more to read");
        return;
    };
    let mmio = phys.to_direct_map();
    // SAFETY: окно регистров устройства (BAR0); ядро по нему не исполняется и
    // только читает.
    if unsafe { crate::arch::map_active(mmio, phys, BAR0_LEN, PageFlags::READ | PageFlags::DEVICE) }.is_err() {
        crate::kprintln!("  nvidia      : could not map BAR0 at {:#x}", phys.as_u64());
        return;
    }
    let read = |offset: usize| -> u32 {
        // SAFETY: смещение внутри отображённого окна BAR0, чтение 32-битное
        // и выровненное, как требует карта.
        unsafe { core::ptr::read_volatile((mmio.as_usize() + offset) as *const u32) }
    };
    let boot0 = read(NV_PMC_BOOT_0);
    let boot42 = read(NV_PMC_BOOT_42);
    if boot0 == u32::MAX {
        crate::kprintln!("  nvidia      : BAR0 answers all ones - the card is asleep or off the bus");
        return;
    }
    let (chip, revision) = chip_of(boot0, boot42);
    let arch = chip >> 4;
    let memory_range = read(NV_PFB_LOCAL_MEMORY_RANGE);
    let wpr2 = (read(NV_PFB_WPR2_ADDR_LO), read(NV_PFB_WPR2_ADDR_HI));
    crate::kprintln!(
        "  nvidia      : {} {} rev {:02x} (BOOT_0 {boot0:#010x}, BOOT_42 {boot42:#010x}), BAR0 {:#x}",
        family(arch),
        chip_name(chip),
        revision,
        phys.as_u64()
    );
    crate::kprintln!(
        "  nvidia      : local memory range {memory_range:#010x}, WPR2 {:#010x}..{:#010x} ({})",
        wpr2.0,
        wpr2.1,
        if wpr2.1 == 0 { "not set up - FWSEC-FRTS has not run" } else { "set" }
    );
    crate::devices::claim(address, "nvidia (read-only)");
    *FOUND.lock() = Some(Found {
        bus: address.bus,
        slot: address.device,
        function: address.function,
        device: device.device,
        revision: device.revision,
        boot0,
        boot42,
        memory_range,
        wpr2,
        bar0: phys.as_u64(),
    });
}
