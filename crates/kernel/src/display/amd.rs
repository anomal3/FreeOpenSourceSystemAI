//! Видеокарты AMD (Radeon): этап A1 плана `docs/AMD_PLAN.md` (17.11.2026) —
//! разведка. Найти карту, прочитать таблицу IP discovery (версии блоков GC,
//! SDMA, PSP, SMU, DCN, VCN…), VBIOS из ACPI-таблицы VFCT и выписать, какие
//! файлы прошивки понадобятся следующим этапам.
//!
//! # Только чтение
//!
//! Картинку на экране может держать прошивка (GOP) через эту же карту. Поэтому:
//! конфигурационное пространство PCI не пишется (`Command` не трогаем, размеры
//! BAR не замеряем — замер выключал бы на миг декодирование памяти), в карту
//! пишутся лишь регистры индексного доступа `MM_INDEX`/`MM_INDEX_HI`, которыми
//! драйвер Linux читает видеопамять (это чтение, а не запись в видеопамять).
//!
//! # Источники
//!
//! Только открытое: драйвер Linux `amdgpu` v6.12 (`amdgpu_discovery.c`,
//! `include/discovery.h`, `include/soc15_hw_ip.h`, `amdgpu_bios.c`,
//! `include/atombios.h`; почти всё под MIT). Числа ниже сверены с ними.

use crate::mm::PageFlags;
use crate::sync::SpinLock;
use alloc::{format, string::String, vec::Vec};

pub(super) const VENDOR: u16 = 0x1002;
/// Окно регистров (BAR5 у всех карт, начиная с Bonaire). Размер BAR замерять
/// нельзя (см. выше); 512 КиБ — обычный размер у RDNA, а драйвер Linux берёт
/// его из `pci_resource_len`. Отображение шире BAR безвредно: мы не читаем
/// за пределами регистров, которые перечислены ниже.
pub(super) const MMIO_LEN: usize = 512 * 1024;

/// Бэкенд DRM на железе: адресное пространство VMID 1 и отправка буферов команд.
pub mod hw;
/// Снимки экрана во время AMD-теста (что видно на мониторе).
pub mod shots;

/// Индексный доступ к памяти через окно регистров (байтовые смещения;
/// у драйвера это dword-номера 0, 1 и 6).
pub(super) const MM_INDEX: usize = 0x0;
pub(super) const MM_DATA: usize = 0x4;
pub(super) const MM_INDEX_HI: usize = 0x18;
/// `mmRCC_CONFIG_MEMSIZE` = dword 0xde3 (`amdgpu_discovery.c`): размер VRAM, МиБ.
pub(super) const RCC_CONFIG_MEMSIZE: usize = 0xde3 * 4;
/// `mmMP0_SMN_C2PMSG_33` = dword 0x16061: бит 31 — PSP закончил загрузку.
pub(super) const MP0_SMN_C2PMSG_33: usize = 0x16061 * 4;

/// Таблица discovery: 10 КиБ, в 64 КиБ от конца VRAM.
pub(super) const DISCOVERY_TMR_SIZE: usize = 10 * 1024;
pub(super) const DISCOVERY_TMR_OFFSET: u64 = 64 * 1024;

pub(super) const BINARY_SIGNATURE: u32 = 0x2821_1407;
pub(super) const DISCOVERY_TABLE_SIGNATURE: u32 = 0x5344_5049; // «IPDS»
pub(super) const MAX_IPS: usize = 512;

/// Один блок IP из таблицы discovery.
#[derive(Clone, Copy)]
pub struct IpBlock {
    pub die: u16,
    pub hw_id: u16,
    pub instance: u8,
    pub major: u8,
    pub minor: u8,
    pub revision: u8,
    pub variant: u8,
    pub sub_revision: u8,
    /// Первый базовый адрес блока (младшие 30 бит, как у драйвера), если есть.
    pub base0: Option<u32>,
}

pub(super) static REPORT: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

/// Отчёт разведки (строки для журнала и команды `amd`).
#[must_use]
pub fn report() -> Vec<String> {
    REPORT.lock().clone()
}

/// Имя блока по `hw_id` (`include/soc15_hw_ip.h`).
#[must_use]
pub fn hw_name(id: u16) -> &'static str {
    match id {
        1 => "MP1/SMU",
        2 => "MP2",
        3 => "THM",
        4 => "SMUIO",
        5 => "FUSE",
        6 => "CLKA",
        10 => "PWR",
        11 => "GC",
        12 => "VCN",
        13 => "AUDIO_AZ",
        14 => "ACP",
        15 => "DCI",
        16 => "DCO",
        21 => "VPE",
        24 => "IOHC",
        28 => "L2IMU",
        34 => "MMHUB",
        35 => "ATHUB",
        40 => "OSSSYS/IH",
        41 => "HDP",
        42 => "SDMA0",
        43 => "SDMA1",
        44 => "ISP",
        46 => "DF",
        47 => "CLKB",
        48 => "FCH",
        68 => "SDMA2",
        69 => "SDMA3",
        70 => "PCIE",
        80 => "PCS",
        108 => "NBIF",
        124 => "IOAPIC",
        128 => "SYSTEMHUB",
        150 => "UMC",
        255 => "MP0/PSP",
        271 => "DMU/DCN",
        272 => "DIO",
        274 => "DAZ",
        _ => "?",
    }
}

pub(super) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

pub(super) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Сумма байтов по модулю 2^16 — контрольная сумма таблиц discovery.
pub(super) fn byte_sum(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |sum, &byte| sum.wrapping_add(u16::from(byte)))
}

/// Разобрать двоичную таблицу IP discovery (`discovery.h`, `#pragma pack(1)`).
///
/// Чистая функция над байтами: проверяется подписью `0x28211407`, суммой
/// байт, подписью `IPDS` и границами каждой записи.
pub fn parse_discovery(bin: &[u8]) -> Result<Vec<IpBlock>, String> {
    // binary_header: сигнатура u32, версия 2×u16, контрольная сумма u16,
    // размер u16, затем table_list[6] по 8 байт (offset, checksum, size, pad).
    let signature = u32_at(bin, 0).ok_or("table too short")?;
    if signature != BINARY_SIGNATURE {
        return Err(format!("binary signature {signature:#010x}, expected {BINARY_SIGNATURE:#010x}"));
    }
    let checksum = u16_at(bin, 8).ok_or("table too short")?;
    let size = usize::from(u16_at(bin, 10).ok_or("table too short")?);
    if size < 12 + 48 || size > bin.len() {
        return Err(format!("binary size {size} outside 60..={}", bin.len()));
    }
    // Сумма — от поля binary_size (смещение 10) до конца.
    let sum = byte_sum(&bin[10..size]);
    if sum != checksum {
        return Err(format!("binary checksum {sum:#06x}, table says {checksum:#06x}"));
    }
    let ip_offset = usize::from(u16_at(bin, 12).ok_or("no IP_DISCOVERY entry")?);
    let ip_checksum = u16_at(bin, 14).ok_or("no IP_DISCOVERY entry")?;
    // ip_discovery_header: signature u32, version u16, size u16, id u32,
    // num_dies u16, die_info[16] (die_id u16, die_offset u16), затем байт с
    // флагом `base_addr_64_bit` (v4) по смещению 78.
    let header = bin.get(ip_offset..ip_offset + 80).ok_or("IP_DISCOVERY header outside the table")?;
    let ip_signature = u32_at(header, 0).ok_or("short")?;
    if ip_signature != DISCOVERY_TABLE_SIGNATURE {
        return Err(format!("IPDS signature {ip_signature:#010x}"));
    }
    let version = u16_at(header, 4).ok_or("short")?;
    let ip_size = usize::from(u16_at(header, 6).ok_or("short")?);
    let ip_bytes = bin.get(ip_offset..ip_offset + ip_size).ok_or("IP_DISCOVERY size outside the table")?;
    if byte_sum(ip_bytes) != ip_checksum {
        return Err(String::from("IP_DISCOVERY checksum mismatch"));
    }
    let dies = usize::from(u16_at(header, 12).ok_or("short")?).min(16);
    let wide = version >= 4 && header[78] & 1 != 0;
    let base_size = if wide { 8 } else { 4 };
    let mut blocks = Vec::new();
    for die in 0..dies {
        let die_offset = usize::from(u16_at(header, 14 + die * 4 + 2).ok_or("short")?);
        // die_header: die_id u16, num_ips u16; записи идут сразу за ним.
        let die_id = u16_at(bin, die_offset).ok_or("die header outside the table")?;
        let count = usize::from(u16_at(bin, die_offset + 2).ok_or("die header outside the table")?);
        if usize::from(die_id) != die {
            return Err(format!("die {die} header says id {die_id}"));
        }
        let mut at = die_offset + 4;
        for _ in 0..count {
            // ip: hw_id u16, number_instance u8, num_base_address u8,
            // major, minor, revision u8, затем байт: harvest (v1-2) или
            // sub_revision:4 + variant:4 (v3+); далее base_address[].
            let entry = bin.get(at..at + 8).ok_or("IP entry outside the table")?;
            let bases = usize::from(entry[3]);
            let length = 8 + bases * base_size;
            let tail = bin.get(at + 8..at + length).ok_or("IP base addresses outside the table")?;
            let base0 = (bases > 0).then(|| u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) & 0x3FFF_FFFF);
            let (sub_revision, variant) = if version >= 3 { (entry[7] & 0x0F, entry[7] >> 4) } else { (0, 0) };
            if blocks.len() < MAX_IPS {
                blocks.push(IpBlock {
                    die: die as u16,
                    hw_id: u16::from_le_bytes([entry[0], entry[1]]),
                    instance: entry[2],
                    major: entry[4],
                    minor: entry[5],
                    revision: entry[6],
                    variant,
                    sub_revision,
                    base0,
                });
            }
            at += length;
        }
    }
    Ok(blocks)
}

/// Версия первого блока с таким `hw_id` в виде «мажор_минор_ревизия».
pub(super) fn version_of(blocks: &[IpBlock], hw_id: u16) -> Option<String> {
    blocks
        .iter()
        .find(|block| block.hw_id == hw_id)
        .map(|block| format!("{}_{}_{}", block.major, block.minor, block.revision))
}

/// Имена файлов прошивки, которые, вероятно, понадобятся следующим этапам.
///
/// **Кандидаты**: имена строятся по шаблонам из драйвера Linux
/// (`MODULE_FIRMWARE` в `gfx_v10_0.c`, `sdma_v5_2.c`, `psp_v13_0.c`,
/// `amdgpu_dm.c`, `amdgpu_vcn.c`), но у ряда блоков версия в имени файла
/// не совпадает с версией IP — сверять с `linux-firmware/amdgpu/`.
#[must_use]
pub fn firmware_candidates(blocks: &[IpBlock]) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(v) = version_of(blocks, 11) {
        for part in ["pfp", "me", "ce", "mec", "mec2", "rlc"] {
            names.push(format!("gc_{v}_{part}.bin"));
        }
    }
    if let Some(v) = version_of(blocks, 42) {
        names.push(format!("sdma_{v}.bin"));
    }
    if let Some(v) = version_of(blocks, 255) {
        for part in ["toc", "ta", "sos"] {
            names.push(format!("psp_{v}_{part}.bin"));
        }
    }
    if let Some(v) = version_of(blocks, 1) {
        names.push(format!("smu_{v}.bin"));
    }
    if let Some(v) = version_of(blocks, 271) {
        names.push(format!("dcn_{v}_dmcub.bin"));
    }
    if let Some(v) = version_of(blocks, 12) {
        names.push(format!("vcn_{v}.bin"));
    }
    names
}

// --- VBIOS из ACPI VFCT -----------------------------------------------------

/// Прочитать образ VBIOS карты из ACPI-таблицы VFCT (`amdgpu_bios.c`,
/// `amdgpu_acpi_vfct_bios`): у встроенной графики своего ПЗУ нет.
pub(super) fn vfct_image(bus: u8, slot: u8, function: u8, device: u16) -> Result<&'static [u8], String> {
    // SAFETY: `rsdp` — адрес, проверенный при загрузке (ноль — таблиц нет,
    // и `find_table` это обрабатывает).
    let table = unsafe { crate::acpi::find_table(crate::acpi::rsdp(), b"VFCT") }
        .map_err(|error| format!("no VFCT table ({error})"))?;
    // UEFI_ACPI_VFCT: заголовок ACPI 36 байт, UUID 16, VBIOSImageOffset u32 на 0x34.
    let mut offset = u32_at(table, 0x34).ok_or("VFCT too short")? as usize;
    while offset + 28 <= table.len() {
        // VFCT_IMAGE_HEADER: PCIBus, PCIDevice, PCIFunction u32; VendorID,
        // DeviceID, SSVID, SSID u16; Revision u32; ImageLength u32.
        let head = &table[offset..offset + 28];
        let image_bus = u32_at(head, 0).unwrap_or(u32::MAX);
        let image_slot = u32_at(head, 4).unwrap_or(u32::MAX);
        let image_function = u32_at(head, 8).unwrap_or(u32::MAX);
        let vendor = u16_at(head, 12).unwrap_or(0);
        let image_device = u16_at(head, 14).unwrap_or(0);
        let length = u32_at(head, 24).unwrap_or(0) as usize;
        let start = offset + 28;
        let end = start.checked_add(length).filter(|&end| end <= table.len()).ok_or("VFCT image runs past the table")?;
        if length != 0
            && image_bus == u32::from(bus)
            && image_slot == u32::from(slot)
            && image_function == u32::from(function)
            && vendor == VENDOR
            && image_device == device
        {
            return Ok(&table[start..end]);
        }
        offset = end;
    }
    Err(String::from("VFCT has no image for this card"))
}

/// Короткое описание образа ATOM: размер, имя, дата. Любое поле — `None`,
/// если не сошлось (всё — по `atom.c`/`atombios.h`).
pub(super) fn atom_summary(bios: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    if bios.len() < 0x49 || bios[0] != 0x55 || bios[1] != 0xAA {
        lines.push(String::from("VBIOS image has no 55AA signature"));
        return lines;
    }
    let header = usize::from(u16_at(bios, 0x48).unwrap_or(0));
    let atom = header != 0 && matches!(bios.get(header + 4..header + 8), Some(b"ATOM" | b"MOTA"));
    lines.push(format!("VBIOS {} bytes, ATOM header at {header:#x}: {}", bios.len(), if atom { "ok" } else { "missing" }));
    if !atom {
        return lines;
    }
    // Имя: после `str_num` строк, начиная с указателя в 0x6e, ещё 2 байта.
    let strings = usize::from(bios.get(0x2f).copied().unwrap_or(0));
    if strings > 0 {
        let mut at = usize::from(u16_at(bios, 0x6e).unwrap_or(0));
        for _ in 0..strings {
            while bios.get(at).is_some_and(|&b| b != 0) {
                at += 1;
            }
            at += 1;
        }
        at += 2;
        let name: String = bios
            .get(at..)
            .unwrap_or(&[])
            .iter()
            .take(63)
            .take_while(|&&b| (0x20..0x7f).contains(&b))
            .map(|&b| char::from(b))
            .collect();
        if !name.is_empty() {
            lines.push(format!("VBIOS name: {}", name.trim_end()));
        }
    }
    if let Some(date) = bios.get(0x50..0x50 + 14) {
        if date.iter().all(|b| (0x20..0x7f).contains(b)) {
            lines.push(format!("VBIOS date: {}", String::from_utf8_lossy(date)));
        }
    }
    lines
}

// --- Разведка ---------------------------------------------------------------

pub(super) struct Window(usize);

impl Window {
    pub(super) fn rd(&self, offset: usize) -> u32 {
        // SAFETY: смещение внутри отображённого окна регистров, 32-битное
        // выровненное чтение.
        unsafe { core::ptr::read_volatile((self.0 + offset) as *const u32) }
    }

    pub(super) fn wr(&self, offset: usize, value: u32) {
        // SAFETY: то же; пишутся только регистры индексного доступа.
        unsafe { core::ptr::write_volatile((self.0 + offset) as *mut u32, value) }
    }

    /// Прочитать `out.len()` байт VRAM с позиции `pos` через MM_INDEX/MM_DATA
    /// (`amdgpu_device_mm_access`: позиция и длина кратны 4).
    pub(super) fn read_vram(&self, pos: u64, out: &mut [u8]) {
        self.wr(MM_INDEX_HI, (pos >> 31) as u32);
        for (index, chunk) in out.chunks_mut(4).enumerate() {
            let at = pos + (index as u64) * 4;
            self.wr(MM_INDEX, (at as u32) | 0x8000_0000);
            let word = self.rd(MM_DATA).to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

pub(super) fn say(lines: &mut Vec<String>, text: String) {
    crate::kprintln!("  amdgpu      : {text}");
    lines.push(text);
}

/// Найти карту AMD и прочитать о ней главное — один раз, на загрузке.
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
    let mut lines = Vec::new();
    let address = device.address;
    say(
        &mut lines,
        format!(
            "{:02x}:{:02x}.{} device {:04x} rev {:02x} class {:02x}{:02x}, command {:#06x}",
            address.bus,
            address.device,
            address.function,
            device.device,
            device.revision,
            device.class,
            device.subclass,
            device.command()
        ),
    );
    for index in [0, 2, 5] {
        match device.memory_bar(index) {
            Some(bar) => say(&mut lines, format!("BAR{index} at {:#x}", bar.as_u64())),
            None => say(&mut lines, format!("BAR{index}: none (or I/O)")),
        }
    }
    // VBIOS — из таблицы ACPI, карту для этого трогать не надо.
    match vfct_image(address.bus, address.device, address.function, device.device) {
        Ok(bios) => {
            for line in atom_summary(bios) {
                say(&mut lines, line);
            }
        }
        Err(why) => say(&mut lines, why),
    }

    // У Polaris таблицы IP discovery нет: версии блоков зашиты в драйвер Linux
    // по PCI-идентификатору, а состояние читается напрямую из регистров.
    if is_polaris10(device.device) {
        polaris(&device, &mut lines);
    } else {
        discovery(&device, &mut lines);
    }
    crate::devices::claim(address, "amdgpu (read-only)");
    *REPORT.lock() = lines;
}

/// Таблица IP discovery через окно регистров BAR5.
pub(super) fn discovery(device: &crate::pci::Device, lines: &mut Vec<String>) {
    let Some(phys) = device.memory_bar(5) else {
        say(lines, String::from("no register window (BAR5) - discovery skipped"));
        return;
    };
    let mmio = phys.to_direct_map();
    // SAFETY: окно регистров устройства (BAR5); ядро по нему не исполняется.
    if unsafe { crate::arch::map_active(mmio, phys, MMIO_LEN, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        say(lines, format!("could not map BAR5 at {:#x}", phys.as_u64()));
        return;
    }
    let window = Window(mmio.as_usize());
    let memsize = window.rd(RCC_CONFIG_MEMSIZE);
    if memsize == u32::MAX {
        say(lines, String::from("BAR5 answers all ones - memory decoding is off or the card sleeps"));
        return;
    }
    // Драйвер ждёт PSP (бит 31) до секунды и всё равно идёт дальше.
    let mut ready = false;
    for _ in 0..200 {
        if window.rd(MP0_SMN_C2PMSG_33) & 0x8000_0000 != 0 {
            ready = true;
            break;
        }
        super::nvidia::spin_us(1000);
    }
    say(
        lines,
        format!(
            "CONFIG_MEMSIZE {memsize} MiB, PSP bootloader {}",
            if ready { "ready (C2PMSG_33 bit 31)" } else { "did not report ready" }
        ),
    );
    if memsize == 0 {
        say(lines, String::from("CONFIG_MEMSIZE is 0: the table is in system memory (ACPI TMR path) - not done yet"));
        return;
    }
    let vram = u64::from(memsize) << 20;
    let mut table = alloc::vec![0u8; DISCOVERY_TMR_SIZE];
    window.read_vram(vram - DISCOVERY_TMR_OFFSET, &mut table);
    match parse_discovery(&table) {
        Ok(blocks) => {
            say(lines, format!("IP discovery: {} blocks", blocks.len()));
            for block in &blocks {
                let base = block.base0.map_or(String::new(), |base| format!(" base {base:#x}"));
                say(
                    lines,
                    format!(
                        "  die {} {:<10} #{} v{}.{}.{} (sub {}, variant {}){base}",
                        block.die,
                        hw_name(block.hw_id),
                        block.instance,
                        block.major,
                        block.minor,
                        block.revision,
                        block.sub_revision,
                        block.variant
                    ),
                );
            }
            let names = firmware_candidates(&blocks);
            if !names.is_empty() {
                say(lines, format!("firmware candidates (verify against linux-firmware): {}", names.join(" ")));
            }
        }
        Err(why) => {
            say(lines, format!("IP discovery table not usable: {why}"));
            let head: Vec<String> = table.iter().take(16).map(|b| format!("{b:02x}")).collect();
            say(lines, format!("  first bytes at VRAM-64K: {}", head.join(" ")));
        }
    }
}

mod polaris;
use polaris::*;
mod smc;
use smc::*;
mod auto;
mod autotest;
use autotest::*;
mod ucode;
use ucode::*;
mod ring;
use ring::*;
mod gart;
use gart::*;
mod gfx;
use gfx::*;

pub use polaris::{polaris_smc, polaris_vbios};
pub use smc::polaris_smc_start;
pub use auto::{auto_task, probed};
pub use ucode::polaris_ucode;
pub use ring::polaris_ring;
pub use gart::polaris_gart;
