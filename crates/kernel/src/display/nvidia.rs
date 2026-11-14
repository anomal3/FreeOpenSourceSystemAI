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
    if let Some(bytes) = vram_bytes(memory_range) {
        crate::kprintln!("  nvidia      : video memory {} MiB", bytes >> 20);
    }
    let mut rom = Rom::new(mmio.as_usize());
    let vbios = rom.parse();
    report(&vbios);
    crate::devices::claim(address, "nvidia (read-only)");
    *VBIOS.lock() = Some(vbios);
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

// --- Этап 2: прошивка карты (VBIOS) -----------------------------------------
//
// Источники (открытые): nouveau `nvkm/subdev/bios/{image,pcir,npde,bit}.c`
// (MIT) и nova-core `vbios.rs`. Прошивка читается через окно PROM в BAR0
// (0x300000, до 1 МиБ) 32-битными словами — только чтение, как у nova-core.

/// Окно PROM в BAR0.
const PROM: usize = 0x0030_0000;
/// Больше окна PROM не бывает.
const PROM_LEN: usize = 0x0010_0000;
/// Приложение FWSEC (боевое) в таблице микрокодов Falcon.
const APPID_FWSEC_PROD: u8 = 0x85;

/// Строки отчёта о прошивке — для журнала и команды `gpu`.
static VBIOS: SpinLock<Option<alloc::vec::Vec<alloc::string::String>>> = SpinLock::new(None);

/// Отчёт о прошивке карты, если её читали.
#[must_use]
pub fn vbios_report() -> alloc::vec::Vec<alloc::string::String> {
    VBIOS.lock().clone().unwrap_or_default()
}

/// Размер видеопамяти по `NV_PFB_PRI_MMU_LOCAL_MEMORY_RANGE` (как nouveau
/// `gp100`/`tu102`): величина в битах 9:4, сдвиг в битах 3:0 (в МиБ), бит 30 —
/// ECC отнимает шестнадцатую часть.
#[must_use]
pub fn vram_bytes(range: u32) -> Option<u64> {
    if range == 0 || range == u32::MAX {
        return None;
    }
    let magnitude = u64::from((range >> 4) & 0x3f);
    let scale = range & 0xf;
    let mut size = magnitude << (scale + 20);
    if range & 0x4000_0000 != 0 {
        size = size / 16 * 15;
    }
    Some(size)
}

/// Прошивка, читаемая из окна PROM по мере надобности.
struct Rom {
    mmio: usize,
    /// Где в окне начинаются образы (заголовок IFR «NVGI» сдвигает их).
    start: usize,
    data: alloc::vec::Vec<u8>,
}

/// Один образ прошивки (PCI expansion ROM image).
#[derive(Clone, Copy)]
struct Image {
    base: usize,
    size: usize,
    kind: u8,
    vendor: u16,
    device: u16,
}

impl Rom {
    fn new(mmio: usize) -> Self {
        Self { mmio, start: 0, data: alloc::vec::Vec::new() }
    }

    fn word_raw(&self, offset: usize) -> u32 {
        // SAFETY: смещение внутри окна PROM, которое лежит внутри
        // отображённого BAR0; чтение 32-битное и выровненное.
        unsafe { core::ptr::read_volatile((self.mmio + PROM + offset) as *const u32) }
    }

    /// Дочитать прошивку до `end` байт от начала образов.
    fn ensure(&mut self, end: usize) -> bool {
        if self.start + end > PROM_LEN {
            return false;
        }
        while self.data.len() < end {
            let word = self.word_raw(self.start + self.data.len());
            self.data.extend_from_slice(&word.to_le_bytes());
        }
        true
    }

    fn u8_at(&mut self, offset: usize) -> Option<u8> {
        self.ensure(offset + 1).then(|| self.data[offset])
    }

    fn u16_at(&mut self, offset: usize) -> Option<u16> {
        self.ensure(offset + 2).then(|| u16::from_le_bytes([self.data[offset], self.data[offset + 1]]))
    }

    fn u32_at(&mut self, offset: usize) -> Option<u32> {
        self.ensure(offset + 4).then(|| {
            u32::from_le_bytes([self.data[offset], self.data[offset + 1], self.data[offset + 2], self.data[offset + 3]])
        })
    }

    /// Заголовок IFR («NVGI») в начале окна: образы лежат дальше
    /// (nova-core `rom_offset`, версии 1–3).
    fn find_start(&mut self) -> Result<usize, &'static str> {
        if self.word_raw(0) != u32::from_le_bytes(*b"NVGI") {
            return Ok(0);
        }
        let fixed1 = self.word_raw(4);
        let fixed_size = ((fixed1 >> 16) & 0x7fff) as usize;
        match (fixed1 >> 8) & 0xff {
            1 | 2 => Ok(self.word_raw(fixed_size + 4) as usize),
            3 => {
                let total = (self.word_raw(8) & 0xf_ffff) as usize;
                let status = self.word_raw(total) as usize;
                let directory = status + 0x1000;
                if directory + 12 > PROM_LEN || self.word_raw(directory) != u32::from_le_bytes(*b"RFRD") {
                    return Err("IFR v3 without an RFRD directory");
                }
                Ok(self.word_raw(directory + 8) as usize)
            }
            _ => Err("unknown IFR version"),
        }
    }

    /// Следующий образ на `base` (nouveau `nvbios_imagen`): `(образ, последний)`.
    fn image(&mut self, base: usize) -> Option<(Image, bool)> {
        match self.u16_at(base)? {
            0xaa55 | 0xbb77 | 0x4e56 => {}
            _ => return None,
        }
        let pcir = base + usize::from(self.u16_at(base + 0x18)?);
        match &self.u32_at(pcir)?.to_le_bytes() {
            b"PCIR" | b"RGIS" | b"NPDS" => {}
            _ => return None,
        }
        let header = usize::from(self.u16_at(pcir + 0x0a)?);
        let vendor = self.u16_at(pcir + 0x04)?;
        let device = self.u16_at(pcir + 0x06)?;
        let mut size = usize::from(self.u16_at(pcir + 0x10)?) * 512;
        let kind = self.u8_at(pcir + 0x14)?;
        let mut last = self.u8_at(pcir + 0x15)? & 0x80 != 0;
        if kind == 0x70 {
            last = true;
        } else {
            let npde = (pcir + header + 0x0f) & !0x0f;
            if self.u32_at(npde) == Some(u32::from_le_bytes(*b"NPDE")) {
                let sub = usize::from(self.u16_at(npde + 0x08)?) * 512;
                if sub != 0 {
                    size = sub;
                }
                last = self.u8_at(npde + 0x0a)? & 0x80 != 0;
            }
        }
        (size != 0).then_some((Image { base, size, kind, vendor, device }, last))
    }

    /// Разобрать прошивку: образы, таблица BIT, где лежит FWSEC.
    fn parse(&mut self) -> alloc::vec::Vec<alloc::string::String> {
        use alloc::format;
        let mut lines = alloc::vec::Vec::new();
        match self.find_start() {
            Ok(start) if start < PROM_LEN => self.start = start,
            Ok(start) => {
                lines.push(format!("VBIOS: IFR points past the PROM window ({start:#x})"));
                return lines;
            }
            Err(why) => {
                lines.push(format!("VBIOS: {why}"));
                return lines;
            }
        }
        let mut images = alloc::vec::Vec::new();
        let mut base = 0;
        while images.len() < 16 {
            let Some((image, last)) = self.image(base) else { break };
            images.push(image);
            base = (image.base + image.size + 511) & !511;
            if last || base >= PROM_LEN - self.start {
                break;
            }
        }
        if images.is_empty() {
            let first = self.word_raw(self.start);
            lines.push(format!("VBIOS: no ROM image at PROM+{:#x} (first word {first:#010x})", self.start));
            return lines;
        }
        let names: alloc::vec::Vec<_> = images
            .iter()
            .map(|image| {
                let kind = match image.kind {
                    0x00 => "PCI-AT",
                    0x03 => "EFI",
                    0x70 => "NBSI",
                    0xe0 => "FwSec",
                    _ => "other",
                };
                format!("{kind} {}K", image.size / 1024)
            })
            .collect();
        lines.push(format!(
            "VBIOS: {} images at PROM+{:#x} ({}), ROM ids {:04x}:{:04x}",
            images.len(),
            self.start,
            names.join(", "),
            images[0].vendor,
            images[0].device
        ));
        let Some(pci_at) = images.iter().copied().find(|image| image.kind == 0x00) else {
            lines.push("VBIOS: no PCI-AT image - no BIT table".into());
            return lines;
        };
        let Some(bit) = self.find_bit(pci_at) else {
            lines.push("VBIOS: no BIT table in the PCI-AT image".into());
            return lines;
        };
        let version = self.u16_at(bit + 6).unwrap_or(0);
        let header = usize::from(self.u8_at(bit + 8).unwrap_or(0));
        let token_size = usize::from(self.u8_at(bit + 9).unwrap_or(0));
        let tokens = usize::from(self.u8_at(bit + 10).unwrap_or(0));
        let mut ids = alloc::string::String::new();
        let mut falcon = None;
        for index in 0..tokens {
            let token = bit + header + index * token_size;
            let (Some(id), Some(data_version), Some(offset)) =
                (self.u8_at(token), self.u8_at(token + 1), self.u16_at(token + 4))
            else {
                break;
            };
            let shown = if id.is_ascii_graphic() { id as char } else { '?' };
            ids.push(shown);
            if id == 0x70 {
                falcon = Some((data_version, pci_at.base + usize::from(offset)));
            }
        }
        lines.push(format!("VBIOS: BIT {}.{} at {bit:#x}, {tokens} tokens \"{ids}\"", version >> 8, version & 0xff));
        let Some((_, falcon_data)) = falcon else {
            lines.push("VBIOS: no falcon data token ('p')".into());
            return lines;
        };
        // Указатель на таблицу микрокодов считается от начала PCI-AT, а сами
        // микрокоды лежат за ним — в образах FwSec (nova-core: указатель минус
        // длина PCI-AT, отсчёт от начала образа FwSec).
        let Some(table_pointer) = self.u32_at(falcon_data) else { return lines };
        let Some(fwsec) = images.iter().copied().find(|image| image.kind == 0xe0) else {
            lines.push(format!("VBIOS: falcon table pointer {table_pointer:#x}, but no FwSec image"));
            return lines;
        };
        let rebase = |pointer: u32| (pointer as usize).checked_sub(pci_at.size).map(|offset| fwsec.base + offset);
        let Some(table) = rebase(table_pointer) else {
            lines.push(format!("VBIOS: falcon table pointer {table_pointer:#x} inside PCI-AT"));
            return lines;
        };
        let (Some(table_version), Some(table_header), Some(entry_len), Some(entries)) =
            (self.u8_at(table), self.u8_at(table + 1), self.u8_at(table + 2), self.u8_at(table + 3))
        else {
            return lines;
        };
        let mut apps = alloc::string::String::new();
        let mut fwsec_ucode = None;
        for index in 0..usize::from(entries) {
            let entry = table + usize::from(table_header) + index * usize::from(entry_len);
            let (Some(app), Some(pointer)) = (self.u8_at(entry), self.u32_at(entry + 2)) else { break };
            apps.push_str(&format!(" {app:02x}"));
            if app == APPID_FWSEC_PROD {
                fwsec_ucode = rebase(pointer);
            }
        }
        lines.push(format!("VBIOS: falcon ucode table v{table_version}, {entries} entries:{apps}"));
        match fwsec_ucode {
            Some(ucode) => {
                let descriptor_version = self.u8_at(ucode + 1).unwrap_or(0);
                lines.push(format!(
                    "VBIOS: FWSEC (prod) ucode at {ucode:#x}, descriptor v{descriptor_version} - stage 3 can load it"
                ));
            }
            None => lines.push("VBIOS: no FWSEC (prod) entry".into()),
        }
        lines
    }

    /// Найти заголовок BIT (`FF B8 'B' 'I' 'T' 00`) в образе PCI-AT.
    fn find_bit(&mut self, image: Image) -> Option<usize> {
        let end = image.base + image.size.min(0x10000);
        if !self.ensure(end) {
            return None;
        }
        self.data[image.base..end]
            .windows(6)
            .position(|window| window == [0xff, 0xb8, b'B', b'I', b'T', 0])
            .map(|position| image.base + position)
    }
}

fn report(lines: &[alloc::string::String]) {
    for line in lines {
        crate::kprintln!("  nvidia      : {line}");
    }
}
