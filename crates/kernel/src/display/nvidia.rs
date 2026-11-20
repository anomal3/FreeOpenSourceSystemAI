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

/// Карта для этапа 4: устройство PCI и адрес окна BAR0.
struct Card {
    device: crate::pci::Device,
    mmio: usize,
}

// SAFETY: `Device` — описание устройства на шине (адрес и идентификаторы),
// `mmio` — адрес окна регистров; ни то, ни другое не привязано к потоку.
unsafe impl Send for Card {}

static CARD: SpinLock<Option<Card>> = SpinLock::new(None);

/// Карта для следующих этапов (`nvidia_gsp`): устройство и окно регистров.
pub(crate) fn card() -> Option<(crate::pci::Device, Bar0)> {
    CARD.lock().as_ref().map(|card| (card.device, Bar0(card.mmio)))
}

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
    // SAFETY: окно регистров устройства (BAR0); ядро по нему не исполняется.
    // Отображено и на запись — ради этапа 4 (`gpu boot`), но на загрузке в
    // карту не пишется ничего.
    if unsafe { crate::arch::map_active(mmio, phys, BAR0_LEN, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
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
    *CARD.lock() = Some(Card { device, mmio: mmio.as_usize() });
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
    /// Где начинается дескриптор FWSEC (боевого), если нашёлся.
    fwsec_ucode: Option<usize>,
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
        Self { mmio, start: 0, data: alloc::vec::Vec::new(), fwsec_ucode: None }
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
                self.fwsec_ucode = Some(ucode);
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

// --- Этап 4: FWSEC-FRTS -------------------------------------------------------
//
// Запуск FWSEC из прошивки карты на её процессоре GSP (Falcon) с командой FRTS:
// микрокод NVIDIA, подписанный ими же, отводит вверху видеопамяти защищённую
// область WPR2 — без неё не стартует GSP-RM (этап 5). Порядок, регистры и
// раскладки — по nova-core (`firmware/fwsec.rs`, `falcon.rs`, `falcon/hal/
// ga102.rs`, `fb.rs`, `gsp/hal/tu102.rs`) и nouveau (`nvkm/falcon/ga102.c`,
// `subdev/gsp/fwsec.c`, `tu102.c`); разбор — в ROADMAP «Видеокарты NVIDIA».
//
// Запускается только командой `gpu boot`: это первая запись в карту. Прошивка
// карты (VBIOS) не меняется; худшее, что может случиться, — зависший GSP или
// погасший экран до выключения питания.

/// Регистры процессора GSP (Falcon) и его второй страницы.
const GSP: usize = 0x0011_0000;
const GSP2: usize = 0x0011_1000;
/// Размер одной пересылки DMA Falcon.
const DMA_CHUNK: usize = 256;
/// Размер подписи RSA-3K.
const SIGNATURE: usize = 384;

/// Окно регистров карты.
#[derive(Clone, Copy)]
pub(crate) struct Bar0(pub(crate) usize);

impl Bar0 {
    pub(crate) fn rd(self, offset: usize) -> u32 {
        // SAFETY: смещение внутри отображённого окна BAR0, 32 бита, выровнено.
        unsafe { core::ptr::read_volatile((self.0 + offset) as *const u32) }
    }

    pub(crate) fn wr(self, offset: usize, value: u32) {
        // SAFETY: см. `rd`; окно отображено на запись (`probe`).
        unsafe { core::ptr::write_volatile((self.0 + offset) as *mut u32, value) }
    }

    /// Ждать, пока `ready` не станет истиной, не дольше `us` микросекунд.
    pub(crate) fn wait(self, us: u64, ready: impl Fn(Self) -> bool) -> bool {
        let until = crate::time::uptime_ns().saturating_add(us * 1000);
        loop {
            if ready(self) {
                return true;
            }
            if crate::time::uptime_ns() >= until {
                return ready(self);
            }
            core::hint::spin_loop();
        }
    }
}

pub(crate) fn spin_us(us: u64) {
    let until = crate::time::uptime_ns().saturating_add(us * 1000);
    while crate::time::uptime_ns() < until {
        core::hint::spin_loop();
    }
}

fn get32(buf: &[u8], at: usize) -> Option<u32> {
    buf.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn put32(buf: &mut [u8], at: usize, value: u32) -> Option<()> {
    buf.get_mut(at..at + 4)?.copy_from_slice(&value.to_le_bytes());
    Some(())
}

/// Этап 4: запустить FWSEC-FRTS. Возвращает строки отчёта (они же — в журнал).
pub fn boot_fwsec() -> alloc::vec::Vec<alloc::string::String> {
    let mut log = alloc::vec::Vec::new();
    let result = run_fwsec(&mut log);
    match result {
        Ok(()) => log.push("FWSEC-FRTS: done, WPR2 is set - stage 5 (GSP-RM) can start".into()),
        Err(why) => log.push(alloc::format!("FWSEC-FRTS: stopped - {why}")),
    }
    for line in &log {
        crate::kprintln!("  nvidia      : {line}");
    }
    log
}

/// Команды FWSEC — `init_cmd` приложения DMEMMAPPER (nouveau `nvfw/fw.h`).
const FWSEC_CMD_FRTS: u32 = 0x15;
const FWSEC_CMD_SB: u32 = 0x19;

#[allow(clippy::too_many_lines)]
fn run_fwsec(log: &mut alloc::vec::Vec<alloc::string::String>) -> Result<(), alloc::string::String> {
    use alloc::format;
    let (device, bar) = {
        let card = CARD.lock();
        let card = card.as_ref().ok_or("no NVIDIA card")?;
        (card.device, Bar0(card.mmio))
    };
    let (chip, _) = chip_of(bar.rd(NV_PMC_BOOT_0), bar.rd(NV_PMC_BOOT_42));
    if chip >> 4 != 0x17 || chip == 0x170 {
        return Err(format!("chip {chip:#x} is not GA102-GA107 - only Ampere consumer cards are done"));
    }

    // 1. Прошивка карты закончила свой запуск (GFW boot).
    wait_gfw_boot(bar)?;
    let (lo, hi) = (bar.rd(NV_PFB_WPR2_ADDR_LO), bar.rd(NV_PFB_WPR2_ADDR_HI));
    if hi >> 4 != 0 {
        return Err(format!("WPR2 already set ({lo:#010x}..{hi:#010x}) - needs a GPU reset (power off)"));
    }

    // 2. Где ляжет FRTS: под рабочей областью VGA вверху видеопамяти.
    let usable_mib = bar.rd(0x0011_83a4);
    let by_range = vram_bytes(bar.rd(NV_PFB_LOCAL_MEMORY_RANGE)).unwrap_or(0);
    let fb_end = if usable_mib != 0 && usable_mib != u32::MAX { u64::from(usable_mib) << 20 } else { by_range };
    if fb_end < 0x0100_0000 {
        return Err(format!("video memory size unknown (0x1183a4 {usable_mib:#x})"));
    }
    let base_nopdisp = fb_end - 0x10_0000;
    let display = bar.rd(0x0082_0c04) & 1 == 0;
    let vga = bar.rd(0x0062_5f04);
    let vga_base = if display && vga & 0x8 != 0 {
        let at = u64::from(vga >> 8) << 16;
        if at < base_nopdisp { fb_end - 0x2_0000 } else { at }
    } else {
        base_nopdisp
    };
    let frts_size: u64 = 0x10_0000;
    let frts_base = (vga_base & !0x1_ffff) - frts_size;
    log.push(format!(
        "VRAM end {fb_end:#x} ({} MiB by 0x1183a4, {} MiB by 0x100ce0), VGA workspace {vga:#x}, FRTS at {frts_base:#x}",
        usable_mib,
        by_range >> 20
    ));
    if (frts_base >> 12) > u64::from(u32::MAX) {
        return Err("FRTS address does not fit the command".into());
    }

    // 3–4. Микрокод FWSEC из прошивки карты: копия, подпись и команда FRTS.
    let image = prepare_fwsec(log, bar, FWSEC_CMD_FRTS, frts_base, frts_size)?;

    // Шина: память и управление шиной (DMA с карты в память машины).
    // SAFETY: регистр команд конфигурационного пространства этой карты.
    unsafe { device.write_config8(0x04, device.config8(0x04) | 0x06) };

    // Страница сброса кешей системной памяти — до сброса Falcon.
    let flush = crate::mm::dma::alloc(4096).map_err(|_| "no DMA memory for the flush page")?;
    let flush_phys = flush.phys().as_u64();
    bar.wr(0x0010_0c40, ((flush_phys >> 40) & 0xff_ffff) as u32);
    bar.wr(0x0010_0c10, (flush_phys >> 8) as u32);

    let outcome = start_fwsec(log, bar, &image).and_then(|(halted, mbox0, _)| {
        let scratch = bar.rd(0x0000_1438);
        let (lo, hi) = (bar.rd(NV_PFB_WPR2_ADDR_LO), bar.rd(NV_PFB_WPR2_ADDR_HI));
        log.push(format!("FRTS error {:#x}, WPR2 {lo:#010x}..{hi:#010x}", scratch >> 16));
        if !halted {
            return Err("FWSEC did not finish (signature refused or hang)".into());
        }
        if mbox0 != 0 {
            return Err(format!("FWSEC returned {mbox0:#x}"));
        }
        if scratch >> 16 != 0 {
            return Err(format!("FRTS error {:#x}", scratch >> 16));
        }
        if hi >> 4 == 0 {
            return Err("WPR2 still not set".into());
        }
        let lower = u64::from(lo >> 4) << 12;
        if lower != frts_base {
            return Err(format!("WPR2 starts at {lower:#x}, expected {frts_base:#x}"));
        }
        Ok(())
    });
    // SAFETY: Falcon остановлен (или не стартовал), буфер больше не читается.
    unsafe { crate::mm::dma::free(&image.buffer) };
    outcome
}

/// Прошивка карты закончила свой запуск (GFW boot) — до 4 с.
fn wait_gfw_boot(bar: Bar0) -> Result<(), alloc::string::String> {
    if !bar.wait(4_000_000, |b| b.rd(0x0011_8128) & 1 != 0 && b.rd(0x0011_8234) & 0xff == 0xff) {
        return Err(alloc::format!(
            "GFW boot not complete (0x118128 {:#x}, 0x118234 {:#x})",
            bar.rd(0x0011_8128),
            bar.rd(0x0011_8234)
        ));
    }
    Ok(())
}

/// FWSEC, готовый к запуску: копия микрокода из VBIOS в памяти DMA с уже
/// вписанными подписью и командой. Для FWSEC-SB его готовят заранее — при
/// запуске GSP-RM, — а запускают при выгрузке (как nouveau: `fwsec_sb.ctor`
/// в `r535_gsp_rm_boot_ctor`, запуск в `tu102_gsp_fini`).
pub(crate) struct FwsecImage {
    buffer: crate::mm::dma::DmaBuffer,
    f: Fwsec,
    command: u32,
}

// SAFETY: буфер DMA — память машины, принадлежащая драйверу; владелец один.
unsafe impl Send for FwsecImage {}

/// Прочитать FWSEC из прошивки карты и вписать подпись и команду `command`
/// (FRTS — с областью `frts_base`/`frts_size`; SB — только `read_vbios`).
fn prepare_fwsec(
    log: &mut alloc::vec::Vec<alloc::string::String>,
    bar: Bar0,
    command: u32,
    frts_base: u64,
    frts_size: u64,
) -> Result<FwsecImage, alloc::string::String> {
    use alloc::format;
    let mut rom = Rom::new(bar.0);
    let _ = rom.parse();
    let desc = rom.fwsec_ucode.ok_or("no FWSEC (prod) ucode in the VBIOS")?;
    let field = |rom: &mut Rom, at: usize| rom.u32_at(desc + at).ok_or("VBIOS read past the PROM window");
    let hdr = field(&mut rom, 0x00)?;
    if hdr & 1 == 0 || (hdr >> 8) & 0xff != 3 {
        return Err(format!("FWSEC descriptor header {hdr:#x} is not a valid v3"));
    }
    let pkc_offset = field(&mut rom, 0x08)? as usize;
    let interface_offset = field(&mut rom, 0x0c)? as usize;
    let imem_base = field(&mut rom, 0x10)?;
    let imem_len = field(&mut rom, 0x14)? as usize;
    let dmem_base = field(&mut rom, 0x1c)?;
    let dmem_len = field(&mut rom, 0x20)? as usize;
    let word = field(&mut rom, 0x24)?;
    let engine_mask = word & 0xffff;
    let ucode_id = (word >> 16) & 0xff;
    let signature_count = (word >> 24) as usize;
    let signature_versions = field(&mut rom, 0x28)? & 0xffff;
    let blob_at = desc + (hdr >> 16) as usize;
    let blob_len = imem_len + dmem_len;
    log.push(format!(
        "FWSEC v3 (cmd {command:#x}): IMEM {imem_len:#x} at {imem_base:#x}, DMEM {dmem_len:#x} at {dmem_base:#x}, \
         engines {engine_mask:#x}, ucode {ucode_id}, {signature_count} signatures (versions {signature_versions:#x})"
    ));
    if imem_len % DMA_CHUNK != 0 || blob_len == 0 || blob_len > 0x8_0000 {
        return Err("unexpected FWSEC sizes".into());
    }
    if !rom.ensure(blob_at + blob_len) || !rom.ensure(desc + 0x2c + signature_count * SIGNATURE) {
        return Err("FWSEC image runs past the PROM window".into());
    }
    let padded = blob_len.div_ceil(DMA_CHUNK) * DMA_CHUNK;
    let buffer = crate::mm::dma::alloc(padded).map_err(|_| "no DMA memory for the ucode")?;
    // SAFETY: буфер только что выделен, `padded` байт, наш до `free`.
    let image = unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr::<u8>(), padded) };
    image[..blob_len].copy_from_slice(&rom.data[blob_at..blob_at + blob_len]);
    let f = Fwsec {
        pkc_offset,
        interface_offset,
        imem_base,
        imem_len,
        dmem_base,
        dmem_len,
        engine_mask,
        ucode_id,
        signature_count,
        signature_versions,
    };
    if let Err(why) = patch(log, bar, image, &rom.data, desc, &f, command, frts_base, frts_size) {
        // SAFETY: карта этот буфер ещё не видела.
        unsafe { crate::mm::dma::free(&buffer) };
        return Err(why);
    }
    Ok(FwsecImage { buffer, f, command })
}

/// Поля дескриптора, нужные загрузке.
#[derive(Clone, Copy)]
struct Fwsec {
    pkc_offset: usize,
    interface_offset: usize,
    imem_base: u32,
    imem_len: usize,
    dmem_base: u32,
    dmem_len: usize,
    engine_mask: u32,
    ucode_id: u32,
    signature_count: usize,
    signature_versions: u32,
}

/// Вписать подпись по фьюзу и команду в DMEMMAPPER (nouveau
/// `nvkm_gsp_fwsec_patch`: `read_vbios` всегда, область FRTS — только для FRTS).
#[allow(clippy::too_many_arguments)]
fn patch(
    log: &mut alloc::vec::Vec<alloc::string::String>,
    bar: Bar0,
    image: &mut [u8],
    rom: &[u8],
    desc: usize,
    f: &Fwsec,
    init_cmd: u32,
    frts_base: u64,
    frts_size: u64,
) -> Result<(), alloc::string::String> {
    use alloc::format;
    let dmem = f.imem_len;

    // Подпись по версии из фьюзов (nova: старший бит фьюза -> номер версии,
    // индекс — сколько версий ниже неё есть в наборе).
    if f.signature_count != 0 {
        if f.engine_mask & 0x400 == 0 || !(1..=16).contains(&f.ucode_id) {
            return Err(format!("FWSEC is not for GSP (engines {:#x}, ucode {})", f.engine_mask, f.ucode_id));
        }
        let fuse = bar.rd(0x0082_41c0 + (f.ucode_id as usize - 1) * 4) & 0xffff;
        let version = 32 - fuse.leading_zeros();
        let bit = 1u32 << version;
        if f.signature_versions & bit == 0 {
            return Err(format!("no signature for fuse {fuse:#x} (version {version})"));
        }
        let index = (f.signature_versions & (bit - 1)).count_ones() as usize;
        if index >= f.signature_count {
            return Err(format!("signature index {index} out of {}", f.signature_count));
        }
        let from = desc + 0x2c + index * SIGNATURE;
        let to = dmem + f.pkc_offset;
        image
            .get_mut(to..to + SIGNATURE)
            .ok_or("signature place is outside DMEM")?
            .copy_from_slice(&rom[from..from + SIGNATURE]);
        log.push(format!("signature {index} for fuse {fuse:#x} -> DMEM {:#x}", f.pkc_offset));
    }

    // Команда: таблица интерфейсов приложения -> DMEMMAPPER (id 4).
    let table = dmem + f.interface_offset;
    let head = image.get(table..table + 4).ok_or("app interface table is outside DMEM")?;
    let (version, header, entry_len, entries) = (head[0], usize::from(head[1]), usize::from(head[2]), usize::from(head[3]));
    if version != 1 || entry_len < 8 {
        return Err(format!("app interface table v{version}, entry {entry_len} - unknown"));
    }
    let mut mapper = None;
    for index in 0..entries {
        let entry = table + header + index * entry_len;
        if get32(image, entry) == Some(4) {
            mapper = get32(image, entry + 4).map(|at| dmem + at as usize);
            break;
        }
    }
    let mapper = mapper.ok_or("no DMEMMAPPER in the app interface table")?;
    let command = dmem + get32(image, mapper + 0x08).ok_or("DMEMMAPPER is outside DMEM")? as usize;
    let command_len = get32(image, mapper + 0x0c).unwrap_or(0);
    // read_vbios: ver, hdr, addr (u64), size, flags.
    let mut words: alloc::vec::Vec<u32> = alloc::vec![1, 24, 0, 0, 0, 2];
    if init_cmd == FWSEC_CMD_FRTS {
        // frts_region: ver, hdr, addr, size (4 KiB), type FB.
        words.extend_from_slice(&[1, 20, (frts_base >> 12) as u32, (frts_size >> 12) as u32, 2]);
    }
    if (command_len as usize) < words.len() * 4 {
        return Err(format!("FWSEC command buffer too small ({command_len})"));
    }
    for (index, value) in words.iter().enumerate() {
        put32(image, command + index * 4, *value).ok_or("FWSEC command is outside DMEM")?;
    }
    put32(image, mapper + 0x2c, init_cmd).ok_or("DMEMMAPPER is outside DMEM")?;
    log.push(format!("DMEMMAPPER at DMEM {:#x}, command at {:#x}, init_cmd {init_cmd:#x}", mapper - dmem, command - dmem));
    Ok(())
}

/// Сбросить GSP Falcon, загрузить в него FWSEC и запустить; ждать остановки
/// до 2 с. Возвращает (остановился, MAILBOX0, MAILBOX1).
fn start_fwsec(
    log: &mut alloc::vec::Vec<alloc::string::String>,
    bar: Bar0,
    fw: &FwsecImage,
) -> Result<(bool, u32, u32), alloc::string::String> {
    use alloc::format;
    let f = fw.f;
    // Сброс GSP Falcon.
    let _ = bar.rd(GSP + 0xf4);
    let _ = bar.wait(150, |b| b.rd(GSP + 0xf4) & 0x8000_0000 != 0);
    bar.wr(GSP + 0x3c0, bar.rd(GSP + 0x3c0) | 1);
    spin_us(10);
    bar.wr(GSP + 0x3c0, bar.rd(GSP + 0x3c0) & !1);
    if !bar.wait(20_000, |b| b.rd(GSP + 0xf4) & 0x1000 == 0) {
        return Err(format!("GSP memory scrubbing did not finish (HWCFG2 {:#x})", bar.rd(GSP + 0xf4)));
    }
    if bar.rd(GSP2 + 0x668) & 0x10 != 0 {
        bar.wr(GSP2 + 0x668, 0);
        if !bar.wait(10_000, |b| b.rd(GSP2 + 0x668) & 1 != 0) {
            return Err("GSP did not switch to the Falcon core".into());
        }
    }
    if !bar.wait(20_000, |b| b.rd(GSP + 0xf4) & 0x1000 == 0) {
        return Err("GSP memory scrubbing did not finish after core select".into());
    }
    bar.wr(GSP + 0x84, bar.rd(NV_PMC_BOOT_0));

    // DMA из системной памяти, физические адреса.
    bar.wr(GSP + 0x624, bar.rd(GSP + 0x624) | 0x80);
    bar.wr(GSP + 0x10c, 0);
    bar.wr(GSP + 0x600, (bar.rd(GSP + 0x600) & !7) | 5);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let phys = fw.buffer.phys().as_u64();
    let section = |base: u64, offset: usize, target: u32, len: usize, command: u32| -> Result<(), alloc::string::String> {
        bar.wr(GSP + 0x110, (base >> 8) as u32);
        bar.wr(GSP + 0x128, ((base >> 40) & 0x1ff) as u32);
        let mut pos = 0;
        while pos < len.div_ceil(DMA_CHUNK) * DMA_CHUNK {
            bar.wr(GSP + 0x114, target + pos as u32);
            bar.wr(GSP + 0x11c, (offset + pos) as u32);
            bar.wr(GSP + 0x118, command);
            if !bar.wait(2_000_000, |b| b.rd(GSP + 0x118) & 2 != 0) {
                return Err(format!("DMA chunk at {pos:#x} did not finish (DMATRFCMD {:#x})", bar.rd(GSP + 0x118)));
            }
            pos += DMA_CHUNK;
        }
        Ok(())
    };
    section(phys, 0, f.imem_base, f.imem_len, 0x614)?;
    section(phys + f.imem_len as u64, 0, f.dmem_base, f.dmem_len, 0x600)?;
    log.push(format!("FWSEC cmd {:#x} loaded by DMA from {phys:#x}", fw.command));

    // Загрузчик в ПЗУ (BROM) проверит подпись и запустит микрокод.
    bar.wr(GSP2 + 0x210, f.pkc_offset as u32);
    bar.wr(GSP2 + 0x19c, f.engine_mask);
    bar.wr(GSP2 + 0x198, f.ucode_id);
    bar.wr(GSP2 + 0x180, 1);
    bar.wr(GSP + 0x104, 0);
    bar.wr(GSP + 0x40, 0);
    if bar.rd(GSP + 0x100) & 0x40 != 0 {
        bar.wr(GSP + 0x130, 2);
    } else {
        bar.wr(GSP + 0x100, 2);
    }
    let halted = bar.wait(2_000_000, |b| b.rd(GSP + 0x100) & 0x10 != 0);
    let (mbox0, mbox1) = (bar.rd(GSP + 0x40), bar.rd(GSP + 0x44));
    log.push(format!(
        "FWSEC cmd {:#x} {}: CPUCTL {:#x}, MAILBOX0 {mbox0:#x}, MAILBOX1 {mbox1:#x}",
        fw.command,
        if halted { "halted" } else { "did NOT halt in 2 s" },
        bar.rd(GSP + 0x100)
    ));
    Ok((halted, mbox0, mbox1))
}

/// Приготовить FWSEC-SB заранее (пока окно PROM читается и память есть).
pub(crate) fn prepare_fwsec_sb(log: &mut alloc::vec::Vec<alloc::string::String>) -> Result<FwsecImage, alloc::string::String> {
    let bar = card().ok_or("no NVIDIA card")?.1;
    prepare_fwsec(log, bar, FWSEC_CMD_SB, 0, 0)
}

/// FWSEC-SB: вернуть процессор GSP в состояние «до LIBOS» перед
/// `booter_unload` (nouveau `nvkm_gsp_fwsec_sb`, nova `Sec2UnloadBundle::run`).
/// Ошибка SB — в младших 16 битах 0x1454 (`0x001400 + 0x15 * 4`).
pub(crate) fn run_fwsec_sb(log: &mut alloc::vec::Vec<alloc::string::String>, fw: &FwsecImage) -> Result<(), alloc::string::String> {
    use alloc::format;
    let (device, bar) = card().ok_or("no NVIDIA card")?;
    // SAFETY: регистр команд конфигурационного пространства этой карты:
    // память и управление шиной (DMA микрокода из памяти машины).
    unsafe { device.write_config8(0x04, device.config8(0x04) | 0x06) };
    let (halted, mbox0, _) = start_fwsec(log, bar, fw)?;
    let error = bar.rd(0x0000_1454) & 0xffff;
    log.push(format!("FWSEC-SB: SB error {error:#x} (0x1454 {:#x})", bar.rd(0x0000_1454)));
    if !halted {
        return Err("FWSEC-SB did not finish".into());
    }
    if mbox0 != 0 {
        return Err(format!("FWSEC-SB returned {mbox0:#x}"));
    }
    if error != 0 {
        return Err(format!("FWSEC-SB error {error:#x}"));
    }
    Ok(())
}

/// Границы WPR2 сейчас: (LO, HI) — регистры 0x1fa824/0x1fa828.
#[must_use]
pub fn wpr2_now() -> Option<(u32, u32)> {
    card().map(|(_, bar)| (bar.rd(NV_PFB_WPR2_ADDR_LO), bar.rd(NV_PFB_WPR2_ADDR_HI)))
}
