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

const VENDOR: u16 = 0x1002;
/// Окно регистров (BAR5 у всех карт, начиная с Bonaire). Размер BAR замерять
/// нельзя (см. выше); 512 КиБ — обычный размер у RDNA, а драйвер Linux берёт
/// его из `pci_resource_len`. Отображение шире BAR безвредно: мы не читаем
/// за пределами регистров, которые перечислены ниже.
const MMIO_LEN: usize = 512 * 1024;

/// Бэкенд DRM на железе: адресное пространство VMID 1 и отправка буферов команд.
pub mod hw;

/// Индексный доступ к памяти через окно регистров (байтовые смещения;
/// у драйвера это dword-номера 0, 1 и 6).
const MM_INDEX: usize = 0x0;
const MM_DATA: usize = 0x4;
const MM_INDEX_HI: usize = 0x18;
/// `mmRCC_CONFIG_MEMSIZE` = dword 0xde3 (`amdgpu_discovery.c`): размер VRAM, МиБ.
const RCC_CONFIG_MEMSIZE: usize = 0xde3 * 4;
/// `mmMP0_SMN_C2PMSG_33` = dword 0x16061: бит 31 — PSP закончил загрузку.
const MP0_SMN_C2PMSG_33: usize = 0x16061 * 4;

/// Таблица discovery: 10 КиБ, в 64 КиБ от конца VRAM.
const DISCOVERY_TMR_SIZE: usize = 10 * 1024;
const DISCOVERY_TMR_OFFSET: u64 = 64 * 1024;

const BINARY_SIGNATURE: u32 = 0x2821_1407;
const DISCOVERY_TABLE_SIGNATURE: u32 = 0x5344_5049; // «IPDS»
const MAX_IPS: usize = 512;

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

static REPORT: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

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

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// Сумма байтов по модулю 2^16 — контрольная сумма таблиц discovery.
fn byte_sum(bytes: &[u8]) -> u16 {
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
fn version_of(blocks: &[IpBlock], hw_id: u16) -> Option<String> {
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
fn vfct_image(bus: u8, slot: u8, function: u8, device: u16) -> Result<&'static [u8], String> {
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
fn atom_summary(bios: &[u8]) -> Vec<String> {
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

struct Window(usize);

impl Window {
    fn rd(&self, offset: usize) -> u32 {
        // SAFETY: смещение внутри отображённого окна регистров, 32-битное
        // выровненное чтение.
        unsafe { core::ptr::read_volatile((self.0 + offset) as *const u32) }
    }

    fn wr(&self, offset: usize, value: u32) {
        // SAFETY: то же; пишутся только регистры индексного доступа.
        unsafe { core::ptr::write_volatile((self.0 + offset) as *mut u32, value) }
    }

    /// Прочитать `out.len()` байт VRAM с позиции `pos` через MM_INDEX/MM_DATA
    /// (`amdgpu_device_mm_access`: позиция и длина кратны 4).
    fn read_vram(&self, pos: u64, out: &mut [u8]) {
        self.wr(MM_INDEX_HI, (pos >> 31) as u32);
        for (index, chunk) in out.chunks_mut(4).enumerate() {
            let at = pos + (index as u64) * 4;
            self.wr(MM_INDEX, (at as u32) | 0x8000_0000);
            let word = self.rd(MM_DATA).to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

fn say(lines: &mut Vec<String>, text: String) {
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
fn discovery(device: &crate::pci::Device, lines: &mut Vec<String>) {
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

// --- Polaris 10 (RX 470/480/570/580/590), семейство VI --------------------------
//
// Этап A1 для карт без IP discovery. Источники — драйвер Linux `amdgpu` v6.12:
// `amdgpu_drv.c` (список PCI-идентификаторов), `vi.c`, `gmc_v8_0.c`,
// `include/asic_reg/{bif/bif_5_0_d.h, gca/gfx_8_0_d.h, gmc/gmc_8_1_d.h,
// oss/oss_3_0_d.h, smu/smu_7_1_1_d.h}`, `amdgpu.h` (`ASICID_IS_P20/P30`).
// Индексы регистров ниже — dword-номера в BAR5, как в заголовках драйвера;
// байтовое смещение = номер × 4.

/// PCI-идентификаторы Polaris 10 из `pciidlist` (`CHIP_POLARIS10`).
const POLARIS10_IDS: [u16; 13] =
    [0x67C0, 0x67C1, 0x67C2, 0x67C4, 0x67C7, 0x67D0, 0x67DF, 0x67C8, 0x67C9, 0x67CA, 0x67CC, 0x67CF, 0x6FDF];

/// Окно регистров (BAR5) у Polaris — 256 КиБ (типовое значение; размер BAR не
/// замеряем, см. начало файла).
const MMIO_LEN_VI: usize = 256 * 1024;

const MM_CONFIG_MEMSIZE: usize = 0x150a;
const MM_CONFIG_APER_SIZE: usize = 0x150c;
const MM_BIF_FB_EN: usize = 0x1524;
const MM_BIOS_SCRATCH_7: usize = 0x5d0;
const MM_SRBM_STATUS: usize = 0x394;
const MM_SRBM_STATUS2: usize = 0x393;
const MM_MC_VM_FB_LOCATION: usize = 0x809;
const MM_VM_L2_CNTL: usize = 0x500;
const MM_VM_CONTEXT0_CNTL: usize = 0x504;
const MM_GRBM_STATUS: usize = 0x2004;
const MM_GRBM_STATUS2: usize = 0x2002;
const MM_CC_RB_BACKEND_DISABLE: usize = 0x263d;
const MM_GB_ADDR_CONFIG: usize = 0x263e;
const MM_CC_GC_SHADER_ARRAY_CONFIG: usize = 0x226f;
const MM_RLC_CNTL: usize = 0xec00;
const MM_CP_ME_CNTL: usize = 0x21b6;
const MM_SDMA0_F32_CNTL: usize = 0x3412;
const MM_SDMA1_F32_CNTL: usize = 0x3612;
const MM_IH_RB_CNTL: usize = 0xe30;
/// Косвенный доступ к регистрам SMC (`SMC_IND_INDEX_11`/`DATA_11`) и адреса
/// ПЗУ видеокарты внутри SMC (`ixROM_INDEX`/`ixROM_DATA`) — так `vi.c`
/// (`vi_read_bios_from_rom`) читает VBIOS, не трогая PCI config.
const MM_SMC_IND_INDEX_11: usize = 0x1ac;
const MM_SMC_IND_DATA_11: usize = 0x1ad;
const IX_ROM_INDEX: u32 = 0xc060_0010;
const IX_ROM_DATA: u32 = 0xc060_0014;

/// `ATOM_S7_ASIC_INIT_COMPLETE_MASK`: прошивка карты (VBIOS) уже выполнила POST.
const ATOM_S7_ASIC_INIT_COMPLETE: u32 = 0x0000_0200;

/// Окно регистров Polaris после разведки — для `amd vbios`.
static POLARIS_MMIO: SpinLock<Option<(usize, u16, u8)>> = SpinLock::new(None);

#[must_use]
fn is_polaris10(device: u16) -> bool {
    POLARIS10_IDS.contains(&device)
}

/// `ASICID_IS_P20` (`amdgpu.h`): определяет «kicker»-прошивку SMC.
fn is_p20(device: u16, revision: u8) -> bool {
    (device == 0x67DF && matches!(revision, 0xE3 | 0xE4 | 0xE5 | 0xE7 | 0xEF))
        || (device == 0x6FDF && matches!(revision, 0xE7 | 0xEF | 0xFF))
}

/// `ASICID_IS_P30` (`amdgpu.h`): `polaris10_k_mc` и `polaris10_k2_smc`.
fn is_p30(device: u16, revision: u8) -> bool {
    device == 0x67DF && matches!(revision, 0xE1 | 0xF7)
}

/// Прошивки, которые запросил бы драйвер Linux для этой карты (v6.12).
///
/// GFX берёт сначала `*_2.bin` и откатывается на `*.bin` (какой из них есть в
/// наборе `linux-firmware`, не проверено); микрокод CP/RLC/SDMA на VI грузит
/// SMC сообщением `LoadUcodes` (`AMDGPU_FW_LOAD_SMU`), поэтому ей нужен и свой
/// файл. UVD/VCE нужны только для видео.
#[must_use]
pub fn polaris_firmware(device: u16, revision: u8) -> Vec<String> {
    let mut names = Vec::new();
    for part in ["ce", "pfp", "me", "mec", "mec2"] {
        names.push(format!("polaris10_{part}_2.bin (or polaris10_{part}.bin)"));
    }
    names.push(String::from("polaris10_rlc.bin"));
    names.push(String::from("polaris10_sdma.bin"));
    names.push(String::from("polaris10_sdma1.bin"));
    names.push(String::from(if is_p30(device, revision) { "polaris10_k_mc.bin" } else { "polaris10_mc.bin" }));
    names.push(String::from(if is_p20(device, revision) {
        "polaris10_k_smc.bin"
    } else if is_p30(device, revision) {
        "polaris10_k2_smc.bin"
    } else {
        "polaris10_smc.bin"
    }));
    names.push(String::from("polaris10_uvd.bin (video only)"));
    names.push(String::from("polaris10_vce.bin (video only)"));
    names
}

/// Читает регистры Polaris, ничего не записывая.
fn polaris(device: &crate::pci::Device, lines: &mut Vec<String>) {
    let Some(phys) = device.memory_bar(5) else {
        say(lines, String::from("no register window (BAR5) - nothing to read"));
        return;
    };
    let mmio = phys.to_direct_map();
    // SAFETY: окно регистров устройства (BAR5); ядро по нему не исполняется.
    // Отображение на запись нужно только для `amd vbios` (индексные регистры).
    if unsafe { crate::arch::map_active(mmio, phys, MMIO_LEN_VI, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        say(lines, format!("could not map BAR5 at {:#x}", phys.as_u64()));
        return;
    }
    let window = Window(mmio.as_usize());
    let reg = |index: usize| window.rd(index * 4);
    let memsize_raw = reg(MM_CONFIG_MEMSIZE);
    if memsize_raw == u32::MAX {
        say(lines, String::from("BAR5 answers all ones - memory decoding is off or the card sleeps"));
        return;
    }
    // `gmc_v8_0_mc_init`: значение — МиБ; при мусоре в старших 16 разрядах
    // остаются младшие.
    let memsize = if memsize_raw & 0xffff_0000 != 0 && memsize_raw & 0xffff != 0 { memsize_raw & 0xffff } else { memsize_raw };
    say(
        lines,
        format!(
            "Polaris 10 (VI, GFX8) {:04x} rev {:02x}: CONFIG_MEMSIZE {memsize} MiB, CONFIG_APER_SIZE {:#x}, BIF_FB_EN {:#x}",
            device.device,
            device.revision,
            reg(MM_CONFIG_APER_SIZE),
            reg(MM_BIF_FB_EN)
        ),
    );
    let scratch = reg(MM_BIOS_SCRATCH_7);
    say(
        lines,
        format!(
            "BIOS_SCRATCH_7 {scratch:#010x}: card {} by firmware (Linux {} POST)",
            if scratch & ATOM_S7_ASIC_INIT_COMPLETE != 0 { "initialised" } else { "NOT initialised" },
            if scratch & ATOM_S7_ASIC_INIT_COMPLETE != 0 { "skips" } else { "would run" }
        ),
    );
    let fb = reg(MM_MC_VM_FB_LOCATION);
    say(
        lines,
        format!(
            "MC_VM_FB_LOCATION {fb:#010x}: VRAM in GPU space {:#x}..={:#x}",
            u64::from(fb & 0xffff) << 24,
            (u64::from(fb >> 16) << 24) | 0xff_ffff
        ),
    );
    say(
        lines,
        format!(
            "VM_L2_CNTL {:#010x}, VM_CONTEXT0_CNTL {:#010x}, IH_RB_CNTL {:#010x}",
            reg(MM_VM_L2_CNTL),
            reg(MM_VM_CONTEXT0_CNTL),
            reg(MM_IH_RB_CNTL)
        ),
    );
    let grbm = reg(MM_GRBM_STATUS);
    say(
        lines,
        format!(
            "GRBM_STATUS {grbm:#010x} ({}), GRBM_STATUS2 {:#010x}, SRBM_STATUS {:#010x}, SRBM_STATUS2 {:#010x}",
            // GUI_ACTIVE — бит 31 (`gfx_8_0_sh_mask.h`).
            if grbm >> 31 == 0 { "gfx idle" } else { "gfx busy" },
            reg(MM_GRBM_STATUS2),
            reg(MM_SRBM_STATUS),
            reg(MM_SRBM_STATUS2)
        ),
    );
    let rlc = reg(MM_RLC_CNTL);
    let me = reg(MM_CP_ME_CNTL);
    say(
        lines,
        format!(
            "RLC_CNTL {rlc:#010x} (RLC {}), CP_ME_CNTL {me:#010x} (CE {}, PFP {}, ME {})",
            if rlc & 1 != 0 { "running" } else { "stopped" },
            if me & (1 << 24) != 0 { "halted" } else { "running" },
            if me & (1 << 26) != 0 { "halted" } else { "running" },
            if me & (1 << 28) != 0 { "halted" } else { "running" }
        ),
    );
    say(
        lines,
        format!(
            "SDMA0_F32_CNTL {:#010x}, SDMA1_F32_CNTL {:#010x} (bit 0 = halted)",
            reg(MM_SDMA0_F32_CNTL),
            reg(MM_SDMA1_F32_CNTL)
        ),
    );
    say(
        lines,
        format!(
            "GB_ADDR_CONFIG {:#010x}, CC_RB_BACKEND_DISABLE {:#010x}, CC_GC_SHADER_ARRAY_CONFIG {:#010x}",
            reg(MM_GB_ADDR_CONFIG),
            reg(MM_CC_RB_BACKEND_DISABLE),
            reg(MM_CC_GC_SHADER_ARRAY_CONFIG)
        ),
    );
    say(lines, format!("firmware Linux would load: {}", polaris_firmware(device.device, device.revision).join(", ")));
    *POLARIS_MMIO.lock() = Some((mmio.as_usize(), device.device, device.revision));
    *POLARIS_BAR0.lock() = device.memory_bar(0).map(|bar| bar.as_u64());
    // Состояние SMC — тоже на загрузке (19.11: клавиатура на живой флешке не
    // всегда опознаётся, а команду `amd smc` без неё не набрать). Только
    // чтение, плюс индекс косвенного доступа, как у `amd smc`.
    for line in polaris_smc() {
        say(lines, format!("smc: {line}"));
    }
}

/// `amd vbios`: прочитать VBIOS Polaris из ПЗУ карты через SMC, как
/// `vi_read_bios_from_rom`. Пишет только индексные регистры SMC (BAR5), в PCI
/// config не пишет; выполняется по команде, а не на загрузке.
#[must_use]
pub fn polaris_vbios() -> Vec<String> {
    let mut lines = Vec::new();
    let Some((base, _, _)) = *POLARIS_MMIO.lock() else {
        lines.push(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // Каждое чтение начинается с нулевого индекса ПЗУ; ROM_DATA сам
    // увеличивает его после каждого dword.
    let read = |bytes: usize| -> Vec<u8> {
        window.wr(MM_SMC_IND_INDEX_11 * 4, IX_ROM_INDEX);
        window.wr(MM_SMC_IND_DATA_11 * 4, 0);
        window.wr(MM_SMC_IND_INDEX_11 * 4, IX_ROM_DATA);
        let mut out = Vec::with_capacity(bytes);
        for _ in 0..bytes.div_ceil(4) {
            out.extend_from_slice(&window.rd(MM_SMC_IND_DATA_11 * 4).to_le_bytes());
        }
        out.truncate(bytes);
        out
    };
    let head = read(64);
    if head.first() != Some(&0x55) || head.get(1) != Some(&0xAA) {
        lines.push(format!("ROM has no 55AA signature (first bytes {:02x?})", &head[..8.min(head.len())]));
        return lines;
    }
    // Длина образа — в 512-байтных блоках (байт 2), как у `amdgpu_read_bios_from_rom`.
    let length = (usize::from(head[2]) << 9).clamp(512, 512 * 1024);
    let bios = read(length);
    lines.extend(atom_summary(&bios));
    lines
}

// --- SMC (микроконтроллер управления питанием), только чтение ---------------
//
// Этап P2, шаг 1. Источники — `smu7_smumgr.c` (`smu7_is_smc_ram_running`,
// `smu7_send_msg_to_smc`), `polaris10_smumgr.c` (`polaris10_start_smu`),
// `smu_7_1_3_d.h`/`smu_7_1_3_sh_mask.h`, `smu7_ppsmc.h` (Linux 6.12).
//
// Драйвер Linux запускает SMC (заливает образ в её SRAM) только если она ещё
// **не** работает: ck_disable == 0 и счётчик команд >= 0x20100. После POST
// прошивкой видеокарты (VBIOS `ASIC_Init`) она обычно уже запущена, и тогда
// остаётся лишь слать ей сообщения (`LoadUcodes` и т.д.). Этот шаг как раз
// выясняет, так ли это на живой карте, и ничего не пишет, кроме индекса
// косвенного доступа `SMC_IND_INDEX_11`.

const MM_SMC_MESSAGE_0: usize = 0x94;
const MM_SMC_RESP_0: usize = 0x95;
const MM_SMC_MSG_ARG_0: usize = 0xa4;
const IX_SMC_SYSCON_RESET_CNTL: u32 = 0x8000_0000;
const IX_SMC_SYSCON_CLOCK_CNTL_0: u32 = 0x8000_0004;
const IX_SMC_SYSCON_MISC_CNTL: u32 = 0x8000_0010;
const IX_SMC_SYSCON_MSG_ARG_0: u32 = 0x8000_0068;
const IX_SMC_PC_C: u32 = 0x8000_0370;
const IX_RCU_UC_EVENTS: u32 = 0xc000_0004;
const IX_SMU_STATUS: u32 = 0xe000_3088;
const IX_SMU_FIRMWARE: u32 = 0xe000_30a4;
/// Слово в SRAM SMC, которое Linux ждёт после старта (`FIRMWARE_FLAGS`, бит 0
/// `INTERRUPTS_ENABLED`).
const IX_FIRMWARE_FLAGS: u32 = 0x3f000;
/// Порог счётчика команд, с которого драйвер считает SMC запущенной.
const SMC_PC_RUNNING: u32 = 0x20100;

impl Window {
    /// Прочитать регистр SMC по косвенному адресу `ix` — как
    /// `cgs_read_ind_register(CGS_IND_REG__SMC, ix)`.
    fn smc_read(&self, ix: u32) -> u32 {
        self.wr(MM_SMC_IND_INDEX_11 * 4, ix);
        self.rd(MM_SMC_IND_DATA_11 * 4)
    }
}

/// `amd smc`: состояние SMC (микроконтроллера питания) Polaris. Только чтение.
#[must_use]
pub fn polaris_smc() -> Vec<String> {
    let mut lines = Vec::new();
    let Some((base, _, _)) = *POLARIS_MMIO.lock() else {
        lines.push(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    let reset = window.smc_read(IX_SMC_SYSCON_RESET_CNTL);
    let clock = window.smc_read(IX_SMC_SYSCON_CLOCK_CNTL_0);
    let misc = window.smc_read(IX_SMC_SYSCON_MISC_CNTL);
    let pc = window.smc_read(IX_SMC_PC_C);
    let events = window.smc_read(IX_RCU_UC_EVENTS);
    let status = window.smc_read(IX_SMU_STATUS);
    let firmware = window.smc_read(IX_SMU_FIRMWARE);
    let flags = window.smc_read(IX_FIRMWARE_FLAGS);
    let msg_arg = window.smc_read(IX_SMC_SYSCON_MSG_ARG_0);
    lines.push(format!(
        "SMC_SYSCON_RESET_CNTL {reset:#010x} (rst_reg {}), CLOCK_CNTL_0 {clock:#010x} (ck_disable {}), MISC_CNTL {misc:#010x}",
        reset & 1,
        clock & 1
    ));
    lines.push(format!("SMC_PC_C {pc:#010x} (running threshold {SMC_PC_RUNNING:#x})"));
    // `smu7_is_smc_ram_running`.
    let running = clock & 1 == 0 && pc >= SMC_PC_RUNNING;
    lines.push(format!(
        "SMC {}: Linux would {} the SMC firmware image",
        if running { "is RUNNING" } else { "is NOT running" },
        if running { "NOT upload" } else { "upload" }
    ));
    lines.push(format!(
        "RCU_UC_EVENTS {events:#010x} (boot_seq_done {}, interrupts_enabled {}), SMU_STATUS {status:#010x} (done {}, pass {})",
        (events >> 7) & 1,
        (events >> 16) & 1,
        status & 1,
        (status >> 1) & 1
    ));
    lines.push(format!(
        "SMU_FIRMWARE {firmware:#010x} (smu_mode {} = {}, smu_sel {}), FIRMWARE_FLAGS(SRAM 0x3f000) {flags:#010x} (interrupts_enabled {})",
        (firmware >> 16) & 1,
        if (firmware >> 16) & 1 == 0 { "non-protected" } else { "protected" },
        (firmware >> 17) & 1,
        flags & 1
    ));
    // Почтовый ящик: ответ 1 = OK, 0xFE = неизвестная команда, 0xFF = сбой.
    lines.push(format!(
        "mailbox: SMC_MESSAGE_0 {:#x}, SMC_RESP_0 {:#x}, SMC_MSG_ARG_0 {:#x}, SMC_SYSCON_MSG_ARG_0 {msg_arg:#x}",
        window.rd(MM_SMC_MESSAGE_0 * 4),
        window.rd(MM_SMC_RESP_0 * 4),
        window.rd(MM_SMC_MSG_ARG_0 * 4)
    ));
    lines
}

// --- Запуск SMC (`amd smc start`) -------------------------------------------
//
// Этап P2, шаг 2 — первый, который **пишет** в карту. Повторяет защищённый
// путь `polaris10_start_smu_in_protection_mode` (`polaris10_smumgr.c`,
// Linux 6.12) и `smu7_upload_smu_firmware_image`/`smu7_upload_smc_firmware_data`
// (`smu7_smumgr.c`). Выполняется только по команде, не на загрузке.
//
// Отказывается, если: SMC уже работает (как `polaris10_start_smu`), режим не
// защищённый или ключ `smu_sel` не 1 (для нас лежит только `polaris10_k_smc.bin`
// — образ для `smu_sel` = 1), образа нет на флешке (`FREEOS/AMD/SMC.BIN`) или
// он не прошёл проверку заголовка. При сбое пытается вернуть SMC в прежнее
// состояние — снять сброс (`rst_reg` = 0): тогда она снова исполняет ПЗУ, как
// до команды.

const MM_SMC_IND_ACCESS_CNTL: usize = 0x92;
/// `AUTO_INCREMENT_IND_11` — бит 11 `SMC_IND_ACCESS_CNTL`.
const AUTO_INCREMENT_IND_11: u32 = 1 << 11;
/// SRAM SMC, куда кладётся образ (`smu7_upload_smc_firmware_data`).
const SMC_IMAGE_ADDRESS: u32 = 0x20000;
/// Предел образа (`SMU7_SMC_SIZE`).
const SMC_IMAGE_LIMIT: usize = 0x20000;
/// `PPSMC_MSG_Test` — сообщение, запускающее проверку образа (`smu7_ppsmc.h`).
const PPSMC_MSG_TEST: u32 = 0x100;
const FIRMWARE_DIR: [&str; 4] = ["/usb", "/usb1", "/usb2", "/usb3"];

impl Window {
    fn smc_write(&self, ix: u32, value: u32) {
        self.wr(MM_SMC_IND_INDEX_11 * 4, ix);
        self.wr(MM_SMC_IND_DATA_11 * 4, value);
    }

    /// Прочитать-изменить-записать регистр SMC (`PHM_WRITE_VFPF_INDIRECT_FIELD`).
    fn smc_modify(&self, ix: u32, clear: u32, set: u32) {
        let value = self.smc_read(ix);
        self.smc_write(ix, (value & !clear) | set);
    }

    /// Ждать, пока `ready(значение регистра SMC)` не станет истинным, до `ms`.
    fn smc_wait(&self, ix: u32, ms: u32, ready: impl Fn(u32) -> bool) -> bool {
        for _ in 0..ms * 10 {
            if ready(self.smc_read(ix)) {
                return true;
            }
            super::nvidia::spin_us(100);
        }
        ready(self.smc_read(ix))
    }

    /// Сообщение SMC с параметром (`smu7_send_msg_to_smc_with_parameter`).
    /// Возвращает ответ из `SMC_RESP_0` (1 — выполнено).
    fn smc_message(&self, message: u32, argument: u32) -> Result<u32, String> {
        let wait_response = |what: &str| -> Result<u32, String> {
            for _ in 0..10_000 {
                let response = self.rd(MM_SMC_RESP_0 * 4) & 0xffff;
                if response != 0 {
                    return Ok(response);
                }
                super::nvidia::spin_us(100);
            }
            Err(format!("SMC did not answer ({what})"))
        };
        wait_response("before the message")?;
        self.wr(MM_SMC_MSG_ARG_0 * 4, argument);
        self.wr(MM_SMC_RESP_0 * 4, 0);
        self.wr(MM_SMC_MESSAGE_0 * 4, message);
        wait_response("after the message")
    }
}

/// Файл прошивки со флешки (`/usb*/FREEOS/AMD/<имя>`).
fn firmware_file(name: &str) -> Result<Vec<u8>, String> {
    for root in FIRMWARE_DIR {
        let path = format!("{root}/FREEOS/AMD/{name}");
        let Some(Ok(node)) =
            crate::fs::resolve_as(crate::vfs::perm::Credentials::ROOT, &path, crate::vfs::perm::Access::NONE)
        else {
            continue;
        };
        let size = node.metadata().size as usize;
        let mut data = alloc::vec![0u8; size];
        let mut done = 0;
        while done < size {
            let n = node.read_at(done as u64, &mut data[done..]).map_err(|error| format!("read {path}: {error:?}"))?;
            if n == 0 {
                return Err(format!("{path} ended after {done} of {size} bytes"));
            }
            done += n;
        }
        return Ok(data);
    }
    Err(format!("FREEOS/AMD/{name} not found on the stick (/usb*)"))
}

/// Проверить заголовок образа SMC и вернуть срез с кодом.
/// Заголовок — `smc_firmware_header_v1_0`: общая часть (32 байта) +
/// `ucode_start_addr`.
fn smc_image(file: &[u8]) -> Result<&[u8], String> {
    let size = u32_at(file, 0).ok_or("firmware too short")? as usize;
    if size != file.len() {
        return Err(format!("header says {size} bytes, file has {}", file.len()));
    }
    let (ip_major, ip_minor) = (u16_at(file, 12).ok_or("short")?, u16_at(file, 14).ok_or("short")?);
    if (ip_major, ip_minor) != (7, 2) {
        return Err(format!("not an SMC 7.2 image (ip version {ip_major}.{ip_minor})"));
    }
    let code_size = u32_at(file, 20).ok_or("short")? as usize;
    let code_offset = u32_at(file, 24).ok_or("short")? as usize;
    if code_size == 0 || code_size % 4 != 0 || code_size > SMC_IMAGE_LIMIT {
        return Err(format!("image size {code_size} is not a multiple of 4 within 1..={SMC_IMAGE_LIMIT}"));
    }
    file.get(code_offset..code_offset + code_size).ok_or_else(|| format!("image {code_offset}+{code_size} runs past the file"))
}

/// `amd smc start`: залить образ в SRAM SMC и запустить её.
///
/// Каждый шаг пишется строкой — и в результат, и в журнал ядра, чтобы при
/// зависании по `LOG.TXT` было видно, где остановились.
#[must_use]
pub fn polaris_smc_start() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : smc start: {text}");
        lines.push(text);
    };
    let Some((base, device, revision)) = *POLARIS_MMIO.lock() else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // Условия отказа — до первой записи.
    let clock = window.smc_read(IX_SMC_SYSCON_CLOCK_CNTL_0);
    let pc = window.smc_read(IX_SMC_PC_C);
    if clock & 1 == 0 && pc >= SMC_PC_RUNNING {
        note(format!("SMC already runs (PC {pc:#x}) - nothing to do"));
        return lines;
    }
    let firmware = window.smc_read(IX_SMU_FIRMWARE);
    let (protected, select) = ((firmware >> 16) & 1, (firmware >> 17) & 1);
    if protected != 1 || select != 1 {
        note(format!(
            "SMU_FIRMWARE {firmware:#010x}: smu_mode {protected}, smu_sel {select} - only protected mode with smu_sel 1 is implemented"
        ));
        return lines;
    }
    if !is_p20(device, revision) {
        note(format!("{device:04x} rev {revision:02x} is not a P20 card: polaris10_k_smc.bin is not its image"));
        return lines;
    }
    let file = match firmware_file("SMC.BIN") {
        Ok(file) => file,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    let image = match smc_image(&file) {
        Ok(image) => image,
        Err(why) => {
            note(format!("SMC.BIN rejected: {why}"));
            return lines;
        }
    };
    note(format!("image {} bytes from FREEOS/AMD/SMC.BIN, PC before {pc:#x}", image.len()));

    // Любой сбой дальше: вернуть SMC в состояние ПЗУ (снять сброс) и остановиться.
    let fail = |window: &Window, note: &mut dyn FnMut(String), why: String| {
        window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 1, 0);
        note(format!("FAILED: {why}; reset released, SMC left in ROM"));
    };

    // 1. Сброс SMC.
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 0, 1);
    note(String::from("1. rst_reg = 1 (SMC held in reset)"));
    // 2. Образ — в SRAM с адреса 0x20000, автоинкремент индекса.
    window.wr(MM_SMC_IND_INDEX_11 * 4, SMC_IMAGE_ADDRESS);
    let access = window.rd(MM_SMC_IND_ACCESS_CNTL * 4);
    window.wr(MM_SMC_IND_ACCESS_CNTL * 4, access | AUTO_INCREMENT_IND_11);
    for word in image.chunks_exact(4) {
        window.wr(MM_SMC_IND_DATA_11 * 4, u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
    }
    let access = window.rd(MM_SMC_IND_ACCESS_CNTL * 4);
    window.wr(MM_SMC_IND_ACCESS_CNTL * 4, access & !AUTO_INCREMENT_IND_11);
    note(format!("2. {} bytes written to SMC SRAM at {SMC_IMAGE_ADDRESS:#x}", image.len()));
    // 3-5. Сбросить статус, включить такт, снять сброс.
    window.smc_write(IX_SMU_STATUS, 0);
    window.smc_modify(IX_SMC_SYSCON_CLOCK_CNTL_0, 1, 0);
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 1, 0);
    note(String::from("3-5. SMU_STATUS = 0, ck_disable = 0, rst_reg = 0 (SMC released)"));
    // 6. Ждать флаг прерываний RCU.
    if !window.smc_wait(IX_RCU_UC_EVENTS, 500, |value| (value >> 16) & 1 == 1) {
        fail(&window, &mut note, String::from("RCU_UC_EVENTS.interrupts_enabled never set"));
        return lines;
    }
    note(String::from("6. RCU_UC_EVENTS.interrupts_enabled = 1"));
    // 7. Сообщение «Test» с параметром 0x20000 запускает проверку образа.
    match window.smc_message(PPSMC_MSG_TEST, SMC_IMAGE_ADDRESS) {
        Ok(response) => note(format!("7. message Test({SMC_IMAGE_ADDRESS:#x}) answered {response:#x}")),
        Err(why) => {
            fail(&window, &mut note, why);
            return lines;
        }
    }
    // 8. Ждать результат проверки: SMU_STATUS.done, затем .pass.
    if !window.smc_wait(IX_SMU_STATUS, 500, |value| value & 1 != 0) {
        fail(&window, &mut note, String::from("SMU_STATUS.done never set"));
        return lines;
    }
    let status = window.smc_read(IX_SMU_STATUS);
    if (status >> 1) & 1 != 1 {
        fail(&window, &mut note, format!("SMU_STATUS {status:#010x}: image check did not pass"));
        return lines;
    }
    note(format!("8. SMU_STATUS {status:#010x}: image check passed"));
    // 9-10. Сбросить флаги прошивки и перезапустить SMC.
    window.smc_write(IX_FIRMWARE_FLAGS, 0);
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 0, 1);
    window.smc_modify(IX_SMC_SYSCON_RESET_CNTL, 1, 0);
    note(String::from("9-10. FIRMWARE_FLAGS = 0, SMC reset pulse"));
    // 11. Ждать, пока прошивка скажет, что готова.
    if !window.smc_wait(IX_FIRMWARE_FLAGS, 1000, |value| value & 1 == 1) {
        fail(&window, &mut note, String::from("FIRMWARE_FLAGS.interrupts_enabled never set"));
        return lines;
    }
    let pc = window.smc_read(IX_SMC_PC_C);
    note(format!(
        "11. firmware is up: PC {pc:#x} ({})",
        if pc >= SMC_PC_RUNNING { "above the running threshold" } else { "BELOW the running threshold" }
    ));
    lines
}

// --- Auto start of the SMC (no keyboard needed) -----------------------------
//
// The USB keyboard of the live stick is not always recognised at boot (19-21.11),
// so `amd smc start` can not always be typed. A stick built with
// FOSSAI_AMD_SMC_START=1 carries the marker FREEOS/AMD/START.TXT: this task waits
// for the stick, and only if the marker is there runs the same start as the
// command. Without the marker nothing is written to the card.

/// True if a Polaris card was found and its register window is mapped.
#[must_use]
pub fn probed() -> bool {
    POLARIS_MMIO.lock().is_some()
}

/// Background task: start the SMC if the stick carries the marker.
pub fn auto_task() {
    if super::firmware_only() {
        crate::kprintln!("  amdgpu     : auto start skipped - firmware display asked in the boot menu");
        return;
    }
    // The stick appears some seconds after the kernel starts.
    let deadline = crate::time::uptime_ms() + 90_000;
    while firmware_file("SMC.BIN").is_err() {
        if crate::time::uptime_ms() > deadline {
            crate::kprintln!("  amdgpu      : auto start: no FREEOS/AMD/SMC.BIN found in 90 s");
            return;
        }
        crate::sched::sleep_ms(1000);
    }
    if firmware_file("START.TXT").is_err() {
        crate::kprintln!("  amdgpu     : auto start is off (no FREEOS/AMD/START.TXT on the stick); 'amd smc start' by hand");
        return;
    }
    crate::kprintln!("==== AMD TEST: started. Stages: SMC, microcode, ring, GART, CS, shader, hw backend, RADV (Proton from the stick). ====");
    crate::kprintln!("==== AMD TEST: watch the '==== AMD TEST' lines; the last one says when it is safe to power off. Whole run: about 5-15 min. ====");
    crate::kprintln!("  amdgpu     : auto start: marker found, starting the SMC");
    let _ = polaris_smc_start();
    for line in polaris_smc() {
        crate::kprintln!("  amdgpu     : auto start: smc: {line}");
    }
    // The second marker (FOSSAI_AMD_UCODE=1 at build time) asks for P3: the
    // CP/RLC/SDMA microcode through the running SMC.
    if firmware_file("UCODE.TXT").is_ok() {
        crate::kprintln!("  amdgpu     : auto start: UCODE.TXT found, loading the microcode");
        let _ = polaris_ucode();
        // The third marker (FOSSAI_AMD_RING=1) asks for P4: start the CP and test the GFX ring.
        if firmware_file("RING.TXT").is_ok() {
            crate::kprintln!("  amdgpu     : auto start: RING.TXT found, testing the GFX ring");
            let _ = polaris_ring();
            // The fourth marker (FOSSAI_AMD_GART=1) asks for P5: the GART of VMID0.
            if firmware_file("GART.TXT").is_ok() {
                crate::kprintln!("  amdgpu     : auto start: GART.TXT found, testing the GART");
                let _ = polaris_gart();
            }
            // The fifth marker (FOSSAI_AMD_CS=1) asks for P5 step 2: fence, IB, user VM (VMID 1).
            if firmware_file("CS.TXT").is_ok() {
                crate::kprintln!("  amdgpu     : auto start: CS.TXT found, testing fence, IB and the user VM");
                let _ = polaris_cs();
            }
            // The sixth marker (FOSSAI_AMD_SHADER=1) asks for P6: golden registers and the first compute shader.
            if firmware_file("SHADER.TXT").is_ok() {
                crate::kprintln!("  amdgpu     : auto start: SHADER.TXT found, running the first shader");
                let _ = polaris_shader();
            }
            // The seventh marker (FOSSAI_AMD_HW=1): the DRM backend on the card - VMID 1 over 64 GiB, an IB above 4 GiB.
            if firmware_file("HW.TXT").is_ok() {
                crate::kprintln!("  amdgpu     : auto start: HW.TXT found, testing the hardware DRM backend");
                let lines = hw::selftest();
                let verdict = |what: &str| lines.iter().find(|l| l.contains(what)).map_or("no result", |l| if l.contains("PASSED") { "PASSED" } else { "FAILED" });
                crate::kprintln!("==== AMD TEST: hw backend: {}, shader in VMID 1: {} ====", verdict("HW RESULT"), verdict("HW SHADER RESULT"));
                crate::usb::stick_log::flush_now();
            }
            // The eighth marker (FOSSAI_AMD_RADV=1): RADV on the card. The live stick installs Debian and Proton
            // into RAM (FOSSAI_WITH_PROTON=1); when that is done the whole vkprobe/vulkaninfo check runs on the node.
            if firmware_file("RADV.TXT").is_ok() {
                crate::kprintln!("  amdgpu     : auto start: RADV.TXT found, waiting for Proton and running RADV on the card");
                radv_auto();
            }
        }
    } else {
        crate::kprintln!("  amdgpu     : auto start: no FREEOS/AMD/UCODE.TXT, microcode not loaded ('amd ucode' by hand)");
    }
}

/// Ждать, пока флешка поставит Debian и Proton в память, и прогнать RADV на настоящей карте:
/// `gpu gametest hw -- vk` (vkprobe и vulkaninfo). Результат - строки `gametest|` в `LOG.TXT`.
fn radv_auto() {
    let present = |path: &str| matches!(crate::fs::list(path), Some(Ok(_)));
    crate::kprintln!("==== AMD TEST: hardware checks are DONE (see the amdgpu lines above). NEXT: RADV test. ====");
    crate::kprintln!("==== AMD TEST: Proton is being copied from the stick into RAM - WAIT, do not power off. ====");
    // До 25 минут: Proton с флешки - больше гигабайта. Строка состояния каждые 10 секунд.
    let started = crate::time::uptime_ms();
    let mut last_line = 0u64;
    loop {
        let waited = (crate::time::uptime_ms() - started) / 1000;
        let ready = present("/opt/proton/bin") && !crate::usb::storage::live_busy();
        if ready || waited > 1500 {
            break;
        }
        if waited >= last_line + 10 || last_line == 0 {
            last_line = waited.max(1);
            crate::kprintln!(
                "  amdgpu     : radv: waiting {waited} s of up to 1500: debian {}, proton {}, installer {}  (usually 2-8 min)",
                if present("/opt/debian") { "in RAM" } else { "not yet" },
                if present("/opt/proton/bin") { "in RAM" } else { "not yet" },
                if crate::usb::storage::live_busy() { "still running" } else { "idle" }
            );
        }
        crate::sched::sleep_ms(2000);
    }
    if !present("/opt/proton/bin") {
        crate::kprintln!("==== AMD TEST: STOPPED - Proton did not appear in /opt (image without FOSSAI_WITH_PROTON=1, or the stick is too slow). You can power off; the log is in FREEOS\\LOG.TXT. ====");
        crate::usb::stick_log::flush_now();
        return;
    }
    crate::kprintln!("==== AMD TEST: Proton is in RAM. Running RADV (vkprobe, vulkaninfo) on the card - about 1-3 min. ====");
    crate::kprintln!("  amdgpu     : radv: {}", super::nvidia_gametest::start("hw amd -- vk"));
    // Не уходить, пока проверка идёт: лог на флешку сбрасывает сама проверка.
    let mut ticks = 0u32;
    while super::nvidia_gametest::running() {
        crate::sched::sleep_ms(1000);
        ticks += 1;
        if ticks % 15 == 0 {
            crate::kprintln!("  amdgpu     : radv: the check is still running ({ticks} s)");
        }
    }
    crate::usb::stick_log::flush_now();
    // Маркер `radv d3d` (FOSSAI_AMD_RADV=2): ещё Direct3D 9 через DXVK на RADV (`d3dprobe`, 20 с кадров).
    // Префикс Wine создаётся при первом запуске - на настоящем процессоре это минуты.
    if firmware_file("RADV.TXT").is_ok_and(|text| text.windows(3).any(|w| w == b"d3d")) {
        crate::kprintln!("==== AMD TEST: DXVK (Direct3D 9) on RADV. The Wine prefix is created first - up to 10 min, WAIT. ====");
        crate::kprintln!("  amdgpu     : radv: {}", super::nvidia_gametest::start("hw amd -- d3d"));
        let mut ticks = 0u32;
        while super::nvidia_gametest::running() {
            crate::sched::sleep_ms(1000);
            ticks += 1;
            if ticks % 15 == 0 {
                crate::kprintln!("  amdgpu     : radv: DXVK check still running ({ticks} s of up to 900)");
            }
            if ticks > 900 {
                crate::kprintln!("==== AMD TEST: the DXVK check ran over 15 min - giving up waiting (the log keeps what it has). ====");
                break;
            }
        }
        crate::usb::stick_log::flush_now();
    }
    crate::kprintln!("==== AMD TEST: FINISHED. You can power off now. The result is in FREEOS\\LOG.TXT (gametest| lines). ====");
    crate::usb::stick_log::flush_now();
}

// --- Загрузка микрокода CP/RLC/SDMA через SMC (`amd ucode`) ----------------
//
// Этап P3. Источники — `smu7_smumgr.c` (`smu7_request_smu_load_fw`,
// `smu7_populate_single_firmware_entry`, `smu7_check_fw_load_finish`),
// `smu_ucode_xfer_vi.h`, `smu7_ppsmc.h`, `smu74.h`, `amdgpu_ucode.c`
// (`amdgpu_ucode_init_bo`), `amdgpu_cgs.c` (`amdgpu_cgs_get_firmware_info`)
// из Linux 6.12.
//
// Отличие от Linux: драйвер кладёт образы в GTT (через GART), мы — в
// видеопамять (так делает режим `debug_use_vram_fw_buf`): адреса VRAM попадают
// в системную апертуру MC, и таблица страниц GPU не нужна. Область — 2 МиБ со
// смещения `UCODE_AREA` окна VRAM (BAR0); GPU-адрес = `vram_start` + смещение.
// Раскладка: TOC (4 КиБ), буфер SMU (800 КиБ, содержимое не заполняется), затем
// образы по страницам: SDMA0, SDMA1, CE, PFP, ME, MEC (+ страница с JT), RLC.
//
// Пишет в видеопамять и просит SMC загрузить микрокод в RLC/CP/SDMA; MC,
// SMC и сами блоки — по их состоянию после `amd smc start`.

const MM_HDP_MEM_COHERENCY_FLUSH_CNTL: usize = 0x1520;
const MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR: usize = 0x80d;
const MM_MC_VM_SYSTEM_APERTURE_HIGH_ADDR: usize = 0x80e;
const MM_MC_VM_SYSTEM_APERTURE_DEFAULT_ADDR: usize = 0x80f;
const PPSMC_MSG_DRV_DRAM_ADDR_HI: u32 = 0x250;
const PPSMC_MSG_DRV_DRAM_ADDR_LO: u32 = 0x251;
const PPSMC_MSG_SMU_DRAM_ADDR_HI: u32 = 0x252;
const PPSMC_MSG_SMU_DRAM_ADDR_LO: u32 = 0x253;
const PPSMC_MSG_LOAD_UCODES: u32 = 0x254;
/// Указатель на мягкие регистры в заголовке прошивки SMC: `SMU7_FIRMWARE_HEADER_LOCATION`
/// (0x20000) + смещение `SoftRegisters` в `SMU74_Firmware_Header` (48).
const SMC_SOFT_REGISTERS_POINTER: u32 = 0x20000 + 48;
/// `UcodeLoadStatus` в `SMU74_SoftRegisters`.
const UCODE_LOAD_STATUS_OFFSET: u32 = 0x6c;
/// Что просим загрузить: RLC_G 0x400 + SDMA0 0x2 + SDMA1 0x4 + CE 0x8 + PFP 0x10 + ME 0x20 + MEC 0x40.
const FW_TO_LOAD: u32 = 0x47e;
/// Номера `UCODE_ID_*` для записей TOC.
const UCODE_ID_SDMA0: u16 = 1;
const UCODE_ID_SDMA1: u16 = 2;
const UCODE_ID_CP_CE: u16 = 3;
const UCODE_ID_CP_PFP: u16 = 4;
const UCODE_ID_CP_ME: u16 = 5;
const UCODE_ID_CP_MEC: u16 = 6;
const UCODE_ID_CP_MEC_JT1: u16 = 7;
const UCODE_ID_CP_MEC_JT2: u16 = 8;
const UCODE_ID_RLC_G: u16 = 10;
/// Смещение области в окне VRAM (BAR0): далеко от кадрового буфера прошивки
/// (он в начале видимой части) и внутри 256 МиБ видимого окна.
const UCODE_AREA: u64 = 0x0c00_0000;
const UCODE_AREA_LEN: usize = 2 * 1024 * 1024;
const TOC_SIZE: usize = 4096;
const SMU_BUFFER_SIZE: usize = 200 * 4096;
const PAGE: usize = 4096;

/// Состояние работающего кольца GFX — для следующих этапов (`amd gart` и далее).
struct RingState {
    /// Виртуальный адрес окна VRAM с кольцом.
    vram: usize,
    /// `vram_start`: GPU-адрес начала видеопамяти.
    vram_start: u64,
    /// Указатель записи в dword (0..RING_DWORDS).
    wptr: u32,
}

static RING_STATE: SpinLock<Option<RingState>> = SpinLock::new(None);

/// Окно VRAM (BAR0, физический адрес) — для `amd ucode`.
static POLARIS_BAR0: SpinLock<Option<u64>> = SpinLock::new(None);

const fn align_page(value: usize) -> usize {
    (value + PAGE - 1) & !(PAGE - 1)
}

/// Образ микрокода из файла `.bin`: сам код, версия и (для CP) таблица переходов.
struct Ucode {
    version: u32,
    /// `ucode_size_bytes` заголовка — размер кода (для MEC в нём и JT).
    code: Vec<u8>,
    /// `jt_offset` и `jt_size` в dword (у CE/PFP/ME/MEC/RLC).
    jt_offset: u32,
    jt_size: u32,
}

/// Разобрать `common_firmware_header` + поля `gfx_firmware_header_v1_0`.
fn parse_ucode(file: &[u8]) -> Result<Ucode, String> {
    let size = u32_at(file, 0).ok_or("short")? as usize;
    if size != file.len() {
        return Err(format!("header says {size} bytes, file has {}", file.len()));
    }
    let version = u32_at(file, 16).ok_or("short")?;
    let code_size = u32_at(file, 20).ok_or("short")? as usize;
    let code_offset = u32_at(file, 24).ok_or("short")? as usize;
    let code = file
        .get(code_offset..code_offset + code_size)
        .ok_or_else(|| format!("code {code_offset}+{code_size} runs past the file"))?
        .to_vec();
    Ok(Ucode { version, code, jt_offset: u32_at(file, 36).unwrap_or(0), jt_size: u32_at(file, 40).unwrap_or(0) })
}

/// Запись `SMU_Entry` (28 байт): id, version, адрес образа, метаданные (нули),
/// размер, флаги, число регистровых записей.
fn toc_entry(id: u16, version: u32, address: u64, size: usize, flags: u16) -> [u8; 28] {
    let mut entry = [0u8; 28];
    entry[0..2].copy_from_slice(&id.to_le_bytes());
    entry[2..4].copy_from_slice(&(version as u16).to_le_bytes());
    entry[4..8].copy_from_slice(&((address >> 32) as u32).to_le_bytes());
    entry[8..12].copy_from_slice(&(address as u32).to_le_bytes());
    entry[20..24].copy_from_slice(&(size as u32).to_le_bytes());
    entry[24..26].copy_from_slice(&flags.to_le_bytes());
    entry
}

/// Записать байты в видеопамять через окно BAR0 32-битными словами.
fn write_vram(base: usize, offset: usize, bytes: &[u8]) {
    for (index, chunk) in bytes.chunks(4).enumerate() {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        // SAFETY: адрес внутри отображённого окна VRAM (`UCODE_AREA_LEN`),
        // проверено вызывающим; запись 32-битная выровненная.
        unsafe { core::ptr::write_volatile((base + offset + index * 4) as *mut u32, u32::from_le_bytes(word)) };
    }
}

/// `amd ucode`: загрузить микрокод RLC/CP/SDMA через SMC (`LoadUcodes`).
///
/// Требует работающей SMC (`amd smc start`) и прошивок `FREEOS/AMD/{CE,PFP,ME,
/// MEC,RLC,SDMA0,SDMA1}.BIN` на флешке. Каждый шаг — строка и в результат, и в журнал.
#[must_use]
pub fn polaris_ucode() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : ucode: {text}");
        lines.push(text);
    };
    let (Some((base, _, _)), Some(bar0)) = (*POLARIS_MMIO.lock(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // 0. Условия: SMC работает, обращения хоста к VRAM разрешены.
    let clock = window.smc_read(IX_SMC_SYSCON_CLOCK_CNTL_0);
    let pc = window.smc_read(IX_SMC_PC_C);
    if clock & 1 != 0 || pc < SMC_PC_RUNNING {
        note(format!("SMC is not running (PC {pc:#x}) - run 'amd smc start' first"));
        return lines;
    }
    if window.rd(MM_BIF_FB_EN * 4) & 3 != 3 {
        note(String::from("BIF_FB_EN: host access to VRAM is off"));
        return lines;
    }
    let fb = window.rd(MM_MC_VM_FB_LOCATION * 4);
    let vram_start = u64::from(fb & 0xffff) << 24;
    let vram_end = (u64::from(fb >> 16) << 24) | 0xff_ffff;
    // 1. Образы с флешки.
    let load = |name: &str| -> Result<Ucode, String> { parse_ucode(&firmware_file(name)?).map_err(|why| format!("{name}: {why}")) };
    let images = (|| -> Result<[Ucode; 7], String> {
        Ok([load("SDMA0.BIN")?, load("SDMA1.BIN")?, load("CE.BIN")?, load("PFP.BIN")?, load("ME.BIN")?, load("MEC.BIN")?, load("RLC.BIN")?])
    })();
    let [sdma0, sdma1, ce, pfp, me, mec, rlc] = match images {
        Ok(images) => images,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    // MEC2 нужен только для версии записи JT2 (сам образ SMC не получает).
    let mec2_version = firmware_file("MEC2.BIN").ok().and_then(|file| parse_ucode(&file).ok()).map_or(mec.version, |u| u.version);
    // 2. Раскладка в области: TOC, буфер SMU, образы по страницам.
    let toc_offset = 0usize;
    let smu_offset = TOC_SIZE;
    let mut cursor = TOC_SIZE + SMU_BUFFER_SIZE;
    let mut place = |len: usize| {
        let at = cursor;
        cursor += align_page(len);
        at
    };
    let at_sdma0 = place(sdma0.code.len());
    let at_sdma1 = place(sdma1.code.len());
    let at_ce = place(ce.code.len());
    let at_pfp = place(pfp.code.len());
    let at_me = place(me.code.len());
    let at_mec = place(mec.code.len());
    let jt_bytes = (mec.jt_size as usize) * 4;
    let at_jt = place(jt_bytes.max(1));
    let at_rlc = place(rlc.code.len());
    let total = cursor;
    if total > UCODE_AREA_LEN {
        note(format!("images need {total} bytes, the area has {UCODE_AREA_LEN}"));
        return lines;
    }
    let jt_start = (mec.jt_offset as usize) * 4;
    let Some(jt) = mec.code.get(jt_start..jt_start + jt_bytes) else {
        note(format!("MEC jump table {jt_start}+{jt_bytes} runs past the code ({} bytes)", mec.code.len()));
        return lines;
    };
    let mc = |offset: usize| vram_start + UCODE_AREA + offset as u64;
    note(format!(
        "VRAM GPU {vram_start:#x}..={vram_end:#x}, area {:#x}.. ({} KiB): TOC {:#x}, SMU buffer {:#x}, images up to {:#x}",
        mc(0),
        total / 1024,
        mc(toc_offset),
        mc(smu_offset),
        mc(total)
    ));
    // 3. Системная апертура MC должна покрывать адреса VRAM (Linux задаёт её в `mc_program`).
    let low = window.rd(MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR * 4);
    let high = window.rd(MM_MC_VM_SYSTEM_APERTURE_HIGH_ADDR * 4);
    let default = window.rd(MM_MC_VM_SYSTEM_APERTURE_DEFAULT_ADDR * 4);
    let (first, last) = (vram_start >> 12, vram_end >> 12);
    let covered = u64::from(low) <= first && u64::from(high) >= last && low <= high;
    note(format!(
        "system aperture LOW {low:#x} HIGH {high:#x} DEFAULT {default:#x} (VRAM pages {first:#x}..={last:#x}): {}",
        if covered { "covers VRAM" } else { "does NOT cover VRAM - programming it like mc_program" }
    ));
    if !covered {
        window.wr(MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR * 4, first as u32);
        window.wr(MM_MC_VM_SYSTEM_APERTURE_HIGH_ADDR * 4, last as u32);
        window.wr(MM_MC_VM_SYSTEM_APERTURE_DEFAULT_ADDR * 4, (mc(UCODE_AREA_LEN - PAGE) >> 12) as u32);
    }
    // 4. Окно VRAM под область и запись образов.
    let phys = crate::mm::PhysAddr::new(bar0 + UCODE_AREA);
    let virt = phys.to_direct_map();
    // SAFETY: окно видеопамяти (BAR0) устройства; ядро по нему не исполняется.
    if unsafe { crate::arch::map_active(virt, phys, UCODE_AREA_LEN, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        note(format!("could not map the VRAM window at {:#x}", phys.as_u64()));
        return lines;
    }
    let vram = virt.as_usize();
    write_vram(vram, at_sdma0, &sdma0.code);
    write_vram(vram, at_sdma1, &sdma1.code);
    write_vram(vram, at_ce, &ce.code);
    write_vram(vram, at_pfp, &pfp.code);
    write_vram(vram, at_me, &me.code);
    write_vram(vram, at_mec, &mec.code);
    write_vram(vram, at_jt, jt);
    write_vram(vram, at_rlc, &rlc.code);
    // Проверка записи: первые слова каждого образа читаются обратно.
    let mut bad = Vec::new();
    for (name, at, code) in [("SDMA0", at_sdma0, &sdma0.code), ("CE", at_ce, &ce.code), ("PFP", at_pfp, &pfp.code), ("RLC", at_rlc, &rlc.code)] {
        let back = u32::from_le_bytes([code[0], code[1], code[2], code[3]]);
        // SAFETY: чтение из отображённой области.
        let got = unsafe { core::ptr::read_volatile((vram + at) as *const u32) };
        if got != back {
            bad.push(format!("{name}: wrote {back:#x}, read back {got:#x}"));
        }
    }
    if !bad.is_empty() {
        note(format!("VRAM write check failed: {}", bad.join("; ")));
        return lines;
    }
    note(format!("images written: SDMA0 {} B, SDMA1 {} B, CE {} B, PFP {} B, ME {} B, MEC {} B (+JT {} B), RLC {} B", sdma0.code.len(), sdma1.code.len(), ce.code.len(), pfp.code.len(), me.code.len(), mec.code.len(), jt_bytes, rlc.code.len()));
    // 5. TOC: порядок записей как в `smu7_request_smu_load_fw`.
    let entries = [
        toc_entry(UCODE_ID_RLC_G, rlc.version, mc(at_rlc), rlc.code.len(), 1),
        toc_entry(UCODE_ID_CP_CE, ce.version, mc(at_ce), ce.code.len(), 0),
        toc_entry(UCODE_ID_CP_PFP, pfp.version, mc(at_pfp), pfp.code.len(), 0),
        toc_entry(UCODE_ID_CP_ME, me.version, mc(at_me), me.code.len(), 0),
        toc_entry(UCODE_ID_CP_MEC, mec.version, mc(at_mec), (mec.jt_offset as usize) * 4, 1),
        toc_entry(UCODE_ID_CP_MEC_JT1, mec.version, mc(at_mec) + align_page(mec.code.len()) as u64, jt_bytes, 0),
        toc_entry(UCODE_ID_CP_MEC_JT2, mec2_version, mc(at_mec) + align_page(mec.code.len()) as u64, jt_bytes, 0),
        toc_entry(UCODE_ID_SDMA0, sdma0.version, mc(at_sdma0), sdma0.code.len(), 0),
        toc_entry(UCODE_ID_SDMA1, sdma1.version, mc(at_sdma1), sdma1.code.len(), 0),
    ];
    let mut toc = Vec::with_capacity(8 + entries.len() * 28);
    toc.extend_from_slice(&1u32.to_le_bytes()); // structure_version
    toc.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in &entries {
        toc.extend_from_slice(entry);
    }
    // 6. Сообщения SMC в порядке `smu7_request_smu_load_fw`.
    let soft = window.smc_read(SMC_SOFT_REGISTERS_POINTER);
    if !(0x20000..0x40000).contains(&soft) {
        note(format!("soft registers pointer {soft:#x} (SRAM {SMC_SOFT_REGISTERS_POINTER:#x}) is not in the SMC RAM"));
        return lines;
    }
    let status_ix = soft + UCODE_LOAD_STATUS_OFFSET;
    window.smc_write(status_ix, 0);
    note(format!("soft registers at SRAM {soft:#x}, UcodeLoadStatus at {status_ix:#x} cleared"));
    let send = |note: &mut dyn FnMut(String), message: u32, argument: u32, what: &str| -> bool {
        match window.smc_message(message, argument) {
            Ok(1) => true,
            Ok(response) => {
                note(format!("{what}: SMC answered {response:#x} (1 = ok, 0xfe = unknown message, 0xff = failed)"));
                false
            }
            Err(why) => {
                note(format!("{what}: {why}"));
                false
            }
        }
    };
    if !send(&mut note, PPSMC_MSG_SMU_DRAM_ADDR_HI, (mc(smu_offset) >> 32) as u32, "SMU_DRAM_ADDR_HI")
        || !send(&mut note, PPSMC_MSG_SMU_DRAM_ADDR_LO, mc(smu_offset) as u32, "SMU_DRAM_ADDR_LO")
    {
        return lines;
    }
    write_vram(vram, toc_offset, &toc);
    // Записи CPU в видеопамять через BAR должны дойти до MC раньше, чем SMC начнёт читать.
    window.wr(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4, 1);
    let _ = window.rd(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4);
    note(format!("TOC written ({} entries, {} B) at {:#x}, HDP flushed", entries.len(), toc.len(), mc(toc_offset)));
    if !send(&mut note, PPSMC_MSG_DRV_DRAM_ADDR_HI, (mc(toc_offset) >> 32) as u32, "DRV_DRAM_ADDR_HI")
        || !send(&mut note, PPSMC_MSG_DRV_DRAM_ADDR_LO, mc(toc_offset) as u32, "DRV_DRAM_ADDR_LO")
        || !send(&mut note, PPSMC_MSG_LOAD_UCODES, FW_TO_LOAD, "LoadUcodes")
    {
        return lines;
    }
    note(format!("LoadUcodes({FW_TO_LOAD:#x}) sent, waiting for UcodeLoadStatus"));
    // 7. Ждать, пока SMC отметит все блоки (до 3 с; Linux ждёт 1 с).
    let done = window.smc_wait(status_ix, 3000, |value| value & FW_TO_LOAD == FW_TO_LOAD);
    let status = window.smc_read(status_ix);
    note(format!(
        "UcodeLoadStatus {status:#x} of {FW_TO_LOAD:#x}: {}",
        if done { "ALL loaded" } else { "TIMEOUT - not everything was loaded" }
    ));
    // 8. Что стало с блоками.
    let rlc_state = window.rd(MM_RLC_CNTL * 4);
    let cp = window.rd(MM_CP_ME_CNTL * 4);
    note(format!(
        "after: RLC_CNTL {rlc_state:#010x}, CP_ME_CNTL {cp:#010x}, SDMA0_F32_CNTL {:#010x}, SDMA1_F32_CNTL {:#010x}, GRBM_STATUS {:#010x}",
        window.rd(MM_SDMA0_F32_CNTL * 4),
        window.rd(MM_SDMA1_F32_CNTL * 4),
        window.rd(MM_GRBM_STATUS * 4)
    ));
    lines
}

// --- Кольцо команд GFX (`amd ring`) ------------------------------------------
//
// Этап P4. Источники — `gfx_v8_0.c` (`gfx_v8_0_cp_gfx_enable`, `cp_gfx_resume`,
// `cp_gfx_start`, `ring_test_ring`, `ring_set_wptr_gfx`), `amdgpu_ring.c`
// (`amdgpu_ring_commit`), `vid.h` (PM4), `gfx_8_0_d.h` (Linux 6.12).
//
// Кольцо и writeback лежат в видеопамяти (область за образами микрокода), адреса —
// в системной апертуре MC (её задаёт `amd ucode`). Таблицы страниц GPU не
// используются: у VMID0 `VM_CONTEXT0` выключен. Тест 1 — пакет `SET_UCONFIG_REG`
// пишет `0xDEADBEEF` в `SCRATCH_REG0` (как `gfx_v8_0_ring_test_ring`). Тест 2 —
// пакет `WRITE_DATA` пишет слово в видеопамять, хост читает его через BAR0.
// Golden registers и `constants_init` (тайлинг, SH_MEM) пока не делаются: для
// кольца они не обязательны, для 3D понадобятся.
//
// При неудаче CP снова останавливается (`CP_ME_CNTL` = 0x15000000).

const MM_GRBM_GFX_INDEX: usize = 0xc200;
const MM_SRBM_GFX_CNTL: usize = 0x391;
const MM_GRBM_CNTL: usize = 0x2000;
const MM_CP_INT_CNTL_RING0: usize = 0x306a;
const MM_CP_RB_WPTR_DELAY: usize = 0x21c1;
const MM_CP_RB_VMID: usize = 0x3051;
const MM_CP_RB0_CNTL: usize = 0x3041;
const MM_CP_RB0_WPTR: usize = 0x3045;
const MM_CP_RB0_RPTR: usize = 0x21c0;
const MM_CP_RB0_RPTR_ADDR: usize = 0x3043;
const MM_CP_RB0_RPTR_ADDR_HI: usize = 0x3044;
const MM_CP_RB_WPTR_POLL_ADDR_LO: usize = 0x3046;
const MM_CP_RB_WPTR_POLL_ADDR_HI: usize = 0x3047;
const MM_CP_RB0_BASE: usize = 0x3040;
const MM_CP_RB0_BASE_HI: usize = 0x30b1;
const MM_CP_RB_DOORBELL_CONTROL: usize = 0x3059;
const MM_CP_MAX_CONTEXT: usize = 0x30ae;
const MM_CP_ENDIAN_SWAP: usize = 0x3050;
const MM_CP_DEVICE_ID: usize = 0x304b;
const MM_CP_STAT: usize = 0x21a0;
const MM_SCRATCH_REG0: usize = 0xc040;
const MM_HDP_DEBUG0: usize = 0xbcc;
/// CE_HALT | PFP_HALT | ME_HALT в `CP_ME_CNTL`.
const CP_HALT_BITS: u32 = 0x1500_0000;
/// `PACKET3(NOP, 0x3FFF)` — так Linux заливает кольцо и добивает до выравнивания.
const PM4_NOP: u32 = 0xffff_1000;
/// `PACKET3(SET_UCONFIG_REG, 1)`, `PACKET3(WRITE_DATA, 3)`.
const PM4_SET_UCONFIG_REG_1: u32 = 0xc001_7900;
const PM4_WRITE_DATA_3: u32 = 0xc003_3700;
/// `WRITE_DATA_DST_SEL(5) | WR_CONFIRM`: память, с подтверждением записи.
const WRITE_DATA_MEMORY_CONFIRM: u32 = 0x0010_0500;
/// Область под кольцо — сразу за областью микрокода (2 МиБ + 2 МиБ), 64 КиБ.
const RING_AREA: u64 = UCODE_AREA + UCODE_AREA_LEN as u64;
const RING_AREA_LEN: usize = 256 * 1024;
const RING_BYTES: usize = 8192;
const RING_DWORDS: u32 = (RING_BYTES / 4) as u32;
/// Writeback: слоты rptr и wptr-poll, затем слово для проверки записью в память.
const RPTR_SLOT: usize = RING_BYTES;
const WPTR_SLOT: usize = RING_BYTES + 64;
const TEST_SLOT: usize = RING_BYTES + 128;
/// Выравнивание, до которого Linux добивает кольцо пакетами NOP (`align_mask` + 1).
const RING_ALIGN: u32 = 256;

impl Window {
    /// Сброс буфера HDP: записи CPU в видеопамять через BAR0 должны дойти до GPU.
    fn hdp_flush(&self) {
        self.wr(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4, 1);
        let _ = self.rd(MM_HDP_MEM_COHERENCY_FLUSH_CNTL * 4);
    }

    /// Сброс кэша чтения HDP: хост должен увидеть то, что записал GPU (`vi_invalidate_hdp`).
    fn hdp_invalidate(&self) {
        self.wr(MM_HDP_DEBUG0 * 4, 1);
        let _ = self.rd(MM_HDP_DEBUG0 * 4);
    }
}

/// `amd ring`: запустить CP и проверить кольцо GFX двумя пакетами.
#[must_use]
pub fn polaris_ring() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : ring: {text}");
        lines.push(text);
    };
    let (Some((base, _, _)), Some(bar0)) = (*POLARIS_MMIO.lock(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    // 0. Условия: микрокод загружен (RLC работает), апертура MC задана.
    let rlc = window.rd(MM_RLC_CNTL * 4);
    let low = window.rd(MM_MC_VM_SYSTEM_APERTURE_LOW_ADDR * 4);
    if rlc & 1 == 0 || low == 0 {
        note(format!("RLC_CNTL {rlc:#x}, system aperture LOW {low:#x}: run 'amd smc start' and 'amd ucode' first"));
        return lines;
    }
    let fb = window.rd(MM_MC_VM_FB_LOCATION * 4);
    let vram_start = u64::from(fb & 0xffff) << 24;
    let gpu = |offset: usize| vram_start + RING_AREA + offset as u64;
    // Окно VRAM под кольцо.
    let phys = crate::mm::PhysAddr::new(bar0 + RING_AREA);
    let virt = phys.to_direct_map();
    // SAFETY: окно видеопамяти (BAR0) устройства; ядро по нему не исполняется.
    if unsafe { crate::arch::map_active(virt, phys, RING_AREA_LEN, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .is_err()
    {
        note(format!("could not map the VRAM window at {:#x}", phys.as_u64()));
        return lines;
    }
    let vram = virt.as_usize();
    let vram_read = |offset: usize| -> u32 {
        // SAFETY: чтение из отображённой области (`RING_AREA_LEN`), выровнено.
        unsafe { core::ptr::read_volatile((vram + offset) as *const u32) }
    };
    let vram_write = |offset: usize, value: u32| {
        // SAFETY: запись в отображённую область, выровнено.
        unsafe { core::ptr::write_volatile((vram + offset) as *mut u32, value) };
    };
    let stop_cp = |window: &Window| {
        let value = window.rd(MM_CP_ME_CNTL * 4);
        window.wr(MM_CP_ME_CNTL * 4, value | CP_HALT_BITS);
    };

    // 1. Общее: широковещание по SE/SH, обычный SRBM, таймаут чтения GRBM.
    window.wr(MM_GRBM_GFX_INDEX * 4, 0xe000_0000);
    window.wr(MM_SRBM_GFX_CNTL * 4, 0);
    let grbm_cntl = window.rd(MM_GRBM_CNTL * 4);
    window.wr(MM_GRBM_CNTL * 4, (grbm_cntl & !0xff) | 0xff);
    // Прерывания CP от занятости/простоя не нужны (Linux сбрасывает их в `rlc_stop`).
    let int_cntl = window.rd(MM_CP_INT_CNTL_RING0 * 4);
    window.wr(MM_CP_INT_CNTL_RING0 * 4, int_cntl & !(0x80000 | 0x100000 | 0x40000 | 0x200000));
    // 2. Кольцо: всё NOP, writeback-слоты нули, проверочное слово — маркер.
    for dword in 0..RING_DWORDS as usize {
        vram_write(dword * 4, PM4_NOP);
    }
    vram_write(RPTR_SLOT, 0);
    vram_write(WPTR_SLOT, 0);
    vram_write(TEST_SLOT, 0xcafe_dead);
    // 3. `gfx_v8_0_cp_gfx_resume`.
    window.wr(MM_CP_RB_WPTR_DELAY * 4, 0);
    window.wr(MM_CP_RB_VMID * 4, 0);
    // RB_BUFSZ = order_base_2(ring_size / 8), RB_BLKSZ = BUFSZ - 2, MTYPE = 3, MIN_IB_AVAILSZ = 1.
    let bufsz = (RING_BYTES / 8).trailing_zeros();
    let cntl = bufsz | ((bufsz - 2) << 8) | (3 << 15) | (1 << 22);
    window.wr(MM_CP_RB0_CNTL * 4, cntl);
    window.wr(MM_CP_RB0_CNTL * 4, cntl | 0x8000_0000); // RB_RPTR_WR_ENA
    window.wr(MM_CP_RB0_WPTR * 4, 0);
    window.wr(MM_CP_RB0_RPTR_ADDR * 4, gpu(RPTR_SLOT) as u32);
    window.wr(MM_CP_RB0_RPTR_ADDR_HI * 4, ((gpu(RPTR_SLOT) >> 32) & 0xff) as u32);
    window.wr(MM_CP_RB_WPTR_POLL_ADDR_LO * 4, gpu(WPTR_SLOT) as u32);
    window.wr(MM_CP_RB_WPTR_POLL_ADDR_HI * 4, (gpu(WPTR_SLOT) >> 32) as u32);
    super::nvidia::spin_us(1000);
    window.wr(MM_CP_RB0_CNTL * 4, cntl); // WR_ENA снят
    let base_address = gpu(0) >> 8;
    window.wr(MM_CP_RB0_BASE * 4, base_address as u32);
    window.wr(MM_CP_RB0_BASE_HI * 4, (base_address >> 32) as u32);
    // Без doorbell: `CP_RB_DOORBELL_CONTROL.DOORBELL_EN` (бит 30) = 0.
    let doorbell = window.rd(MM_CP_RB_DOORBELL_CONTROL * 4);
    window.wr(MM_CP_RB_DOORBELL_CONTROL * 4, doorbell & !(1 << 30));
    // `gfx_v8_0_cp_gfx_start`: контекст и ID, затем снять останов CE/PFP/ME.
    window.wr(MM_CP_MAX_CONTEXT * 4, 7);
    window.wr(MM_CP_ENDIAN_SWAP * 4, 0);
    window.wr(MM_CP_DEVICE_ID * 4, 1);
    let me = window.rd(MM_CP_ME_CNTL * 4);
    window.wr(MM_CP_ME_CNTL * 4, me & !CP_HALT_BITS);
    super::nvidia::spin_us(50);
    note(format!(
        "CP started: ring {} B at GPU {:#x}, RB0_CNTL {cntl:#x}, CP_ME_CNTL {:#x} -> {:#x}",
        RING_BYTES,
        gpu(0),
        me,
        window.rd(MM_CP_ME_CNTL * 4)
    ));
    // Отправка: пакеты уже в кольце, добиваем NOP до границы (кольцо залито NOP).
    let submit = |window: &Window, wptr: u32| {
        window.hdp_flush();
        window.wr(MM_CP_RB0_WPTR * 4, wptr);
        let _ = window.rd(MM_CP_RB0_WPTR * 4);
    };
    // Тест 1: SET_UCONFIG_REG пишет 0xDEADBEEF в SCRATCH_REG0 (смещение 0x40 от 0xc000).
    window.wr(MM_SCRATCH_REG0 * 4, 0xcafe_dead);
    vram_write(0, PM4_SET_UCONFIG_REG_1);
    vram_write(4, (MM_SCRATCH_REG0 - 0xc000) as u32);
    vram_write(8, 0xdead_beef);
    submit(&window, RING_ALIGN);
    let mut value = 0;
    for _ in 0..100_000 {
        value = window.rd(MM_SCRATCH_REG0 * 4);
        if value == 0xdead_beef {
            break;
        }
        super::nvidia::spin_us(1);
    }
    let state = |window: &Window| {
        format!(
            "RB0_RPTR {:#x}, rptr slot {:#x}, GRBM_STATUS {:#010x}, CP_STAT {:#010x}",
            window.rd(MM_CP_RB0_RPTR * 4),
            vram_read(RPTR_SLOT),
            window.rd(MM_GRBM_STATUS * 4),
            window.rd(MM_CP_STAT * 4)
        )
    };
    if value != 0xdead_beef {
        note(format!("TEST 1 FAILED: SCRATCH_REG0 {value:#x} (expected 0xdeadbeef); {}", state(&window)));
        stop_cp(&window);
        note(String::from("CP halted again"));
        return lines;
    }
    note(format!("TEST 1 PASSED: SCRATCH_REG0 = 0xdeadbeef written by the CP from the ring; {}", state(&window)));
    // Тест 2: WRITE_DATA в видеопамять (DST_SEL 5, WR_CONFIRM), хост читает через BAR0.
    let at = RING_ALIGN as usize;
    vram_write(at * 4, PM4_WRITE_DATA_3);
    vram_write(at * 4 + 4, WRITE_DATA_MEMORY_CONFIRM);
    vram_write(at * 4 + 8, gpu(TEST_SLOT) as u32);
    vram_write(at * 4 + 12, (gpu(TEST_SLOT) >> 32) as u32);
    vram_write(at * 4 + 16, 0xbeef_cafe);
    submit(&window, 2 * RING_ALIGN);
    let mut word = 0;
    for _ in 0..100_000 {
        window.hdp_invalidate();
        word = vram_read(TEST_SLOT);
        if word == 0xbeef_cafe {
            break;
        }
        super::nvidia::spin_us(1);
    }
    if word == 0xbeef_cafe {
        note(format!("TEST 2 PASSED: the CP wrote 0xbeefcafe into VRAM at GPU {:#x}; {}", gpu(TEST_SLOT), state(&window)));
    } else {
        note(format!("TEST 2 FAILED: VRAM word {word:#x} (expected 0xbeefcafe); {}", state(&window)));
    }
    // Кольцо живо (тест 1 пройден): следующие этапы шлют пакеты тем же кольцом.
    *RING_STATE.lock() = Some(RingState { vram, vram_start, wptr: 2 * RING_ALIGN });
    lines
}

// --- GART для VMID0 (`amd gart`) ---------------------------------------------
//
// Этап P5, шаг 1. Источники — `gmc_v8_0.c` (`gmc_v8_0_gart_enable`,
// `gmc_v8_0_flush_gpu_tlb`), `amdgpu_gart.c` (`amdgpu_gart_map`),
// `amdgpu_gmc.c` (`amdgpu_gmc_set_pte_pde`), `amdgpu_vm.h` (`AMDGPU_PTE_*`),
// `gmc_8_1_d.h`/`gmc_8_1_sh_mask.h` (Linux 6.12).
//
// GART — таблица страниц контекста 0 (VMID0): она даёт GPU доступ к страницам
// **системной** памяти по GPU-адресам вне видеопамяти. Здесь — 16 МиБ GART
// по адресу `GART_START` (вне VRAM, выровнен на 4 ГиБ, как у Linux), таблица (4096
// записей по 8 байт) лежит в видеопамяти за кольцом. Проверка: CP пакетом
// `WRITE_DATA` пишет слово по GART-адресу, ядро читает системную страницу.
// Регистры L2/TLB меняются «прочитать-изменить-записать», как у Linux:
// значения после POST прошивкой неизвестны.
//
// Запись PTE: `phys & 0x0000_FFFF_FFFF_F000 | VALID | SYSTEM | SNOOPED | EXECUTABLE |
// READABLE | WRITEABLE` (= `0x77`; для кэшируемой страницы CPU).

const MM_MC_VM_MX_L1_TLB_CNTL: usize = 0x819;
const MM_VM_L2_CNTL2: usize = 0x501;
const MM_VM_L2_CNTL3: usize = 0x502;
const MM_VM_L2_CNTL4: usize = 0x578;
const MM_VM_CONTEXT0_CNTL2: usize = 0x50c;
const MM_VM_CONTEXT0_PAGE_TABLE_BASE_ADDR: usize = 0x54f;
const MM_VM_CONTEXT0_PAGE_TABLE_START_ADDR: usize = 0x557;
const MM_VM_CONTEXT0_PAGE_TABLE_END_ADDR: usize = 0x55f;
const MM_VM_CONTEXT0_PROTECTION_FAULT_DEFAULT_ADDR: usize = 0x546;
const MM_VM_CONTEXT0_PROTECTION_FAULT_STATUS: usize = 0x536;
const MM_VM_CONTEXT0_PROTECTION_FAULT_MCCLIENT: usize = 0x538;
const MM_VM_CONTEXT0_PROTECTION_FAULT_ADDR: usize = 0x53e;
const MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_LOW_ADDR: usize = 0x575;
const MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_HIGH_ADDR: usize = 0x576;
const MM_VM_L2_CONTEXT_IDENTITY_PHYSICAL_OFFSET: usize = 0x577;
const MM_VM_INVALIDATE_REQUEST: usize = 0x51e;
/// GPU-адрес начала GART (вне VRAM, выровнен на 4 ГиБ).
const GART_START: u64 = 0xff_0000_0000;
const GART_PAGES: usize = 4096;
/// Таблица GART в окне VRAM кольца: за кольцом и словами проверки.
const GART_TABLE_OFFSET: usize = 0x1_0000;
const PTE_VALID: u64 = 1;
const PTE_SYSTEM: u64 = 1 << 1;
const PTE_SNOOPED: u64 = 1 << 2;
const PTE_EXECUTABLE: u64 = 1 << 4;
const PTE_READABLE: u64 = 1 << 5;
const PTE_WRITEABLE: u64 = 1 << 6;

/// Записать пакеты в кольцо GFX и сдвинуть указатель записи: блок в
/// `RING_ALIGN` dword (как `amdgpu_ring_commit`, который добивает NOP до границы).
fn ring_submit(window: &Window, packets: &[u32]) -> Result<(), String> {
    let mut guard = RING_STATE.lock();
    let state = guard.as_mut().ok_or("the ring is not running: run 'amd ring' first")?;
    if packets.len() >= RING_ALIGN as usize {
        return Err(format!("{} dwords do not fit one {RING_ALIGN}-dword block", packets.len()));
    }
    let put = |index: u32, value: u32| {
        // SAFETY: слово внутри отображённого окна кольца (`RING_BYTES`).
        unsafe { core::ptr::write_volatile((state.vram + (index % RING_DWORDS) as usize * 4) as *mut u32, value) };
    };
    let start = state.wptr;
    for index in 0..RING_ALIGN {
        put(start + index, PM4_NOP);
    }
    for (index, packet) in packets.iter().enumerate() {
        put(start + index as u32, *packet);
    }
    state.wptr = (start + RING_ALIGN) % RING_DWORDS;
    window.hdp_flush();
    window.wr(MM_CP_RB0_WPTR * 4, state.wptr);
    let _ = window.rd(MM_CP_RB0_WPTR * 4);
    Ok(())
}

/// Страница системной памяти: физический адрес и адрес в прямом отображении.
fn system_page(fill: u32) -> Result<(u64, usize), String> {
    use crate::mm::FrameAllocator as _;
    let phys = crate::mm::frame::with(|frames| frames.allocate())
        .flatten()
        .map(crate::mm::PhysAddr::as_u64)
        .ok_or("out of memory pages")?;
    let virt = crate::mm::PhysAddr::new(phys).to_direct_map().as_usize();
    for index in 0..PAGE / 4 {
        // SAFETY: страница только что выделена и отображена прямым отображением.
        unsafe { core::ptr::write_volatile((virt + index * 4) as *mut u32, fill) };
    }
    Ok((phys, virt))
}

/// `amd gart`: включить GART для VMID0 и проверить доступ GPU к системной памяти.
#[must_use]
pub fn polaris_gart() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : gart: {text}");
        lines.push(text);
    };
    let (Some((base, _, _)), Some(bar0)) = (*POLARIS_MMIO.lock(), *POLARIS_BAR0.lock()) else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    let Some(vram) = RING_STATE.lock().as_ref().map(|state| (state.vram, state.vram_start)) else {
        note(String::from("the ring is not running: run 'amd ring' first"));
        return lines;
    };
    let (vram_virt, vram_start) = vram;
    let _ = bar0;
    // Страницы: пустая для ошибок доступа и страница проверки.
    let (dummy_phys, _) = match system_page(0) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    let (test_phys, test_virt) = match system_page(0x1111_1111) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    // Таблица GART в видеопамяти: все записи недействительны, кроме страницы 1.
    let table_mc = vram_start + RING_AREA + GART_TABLE_OFFSET as u64;
    for index in 0..GART_PAGES * 2 {
        // SAFETY: таблица внутри окна VRAM (`RING_AREA_LEN`); 32-битные записи.
        unsafe { core::ptr::write_volatile((vram_virt + GART_TABLE_OFFSET + index * 4) as *mut u32, 0) };
    }
    let pte = (test_phys & 0x0000_ffff_ffff_f000) | PTE_VALID | PTE_SYSTEM | PTE_SNOOPED | PTE_EXECUTABLE | PTE_READABLE | PTE_WRITEABLE;
    let gart_va = GART_START + PAGE as u64;
    for (half, value) in [(0usize, pte as u32), (1, (pte >> 32) as u32)] {
        // SAFETY: запись 1 таблицы (8 байт со смещения 8), внутри окна VRAM.
        unsafe { core::ptr::write_volatile((vram_virt + GART_TABLE_OFFSET + 8 + half * 4) as *mut u32, value) };
    }
    note(format!(
        "table at GPU {table_mc:#x} ({} KiB), GART {GART_START:#x}.. ({} MiB), test page phys {test_phys:#x} (PTE {pte:#x}), dummy {dummy_phys:#x}",
        GART_PAGES * 8 / 1024,
        GART_PAGES * PAGE / (1024 * 1024)
    ));
    let before = [
        window.rd(MM_MC_VM_MX_L1_TLB_CNTL * 4),
        window.rd(MM_VM_L2_CNTL * 4),
        window.rd(MM_VM_L2_CNTL3 * 4),
        window.rd(MM_VM_CONTEXT0_CNTL * 4),
    ];
    // `gmc_v8_0_gart_enable`: TLB, L2, контекст 0.
    let modify = |register: usize, clear: u32, set: u32| {
        let value = window.rd(register * 4);
        window.wr(register * 4, (value & !clear) | set);
    };
    // ENABLE_L1_TLB, ENABLE_L1_FRAGMENT_PROCESSING, SYSTEM_ACCESS_MODE = 3, ADVANCED_DRIVER_MODEL;
    // SYSTEM_APERTURE_UNMAPPED_ACCESS = 0.
    modify(MM_MC_VM_MX_L1_TLB_CNTL, 0x1 | 0x2 | 0x18 | 0x20 | 0x40, 0x1 | 0x2 | 0x18 | 0x40);
    // ENABLE_L2_CACHE, FRAGMENT_PROCESSING, PTE/PDE0 LRU_UPDATE_BY_WRITE, QUEUE_SIZE = 7,
    // CONTEXT1_IDENTITY_ACCESS_MODE = 1, ENABLE_DEFAULT_PAGE_OUT_TO_SYSTEM_MEMORY.
    modify(MM_VM_L2_CNTL, 0x1 | 0x2 | 0x200 | 0x400 | 0x3_8000 | 0x18_0000 | 0x800, 0x1 | 0x2 | 0x200 | 0x400 | 0x3_8000 | 0x8_0000 | 0x800);
    modify(MM_VM_L2_CNTL2, 0, 0x1 | 0x2); // INVALIDATE_ALL_L1_TLBS | INVALIDATE_L2_CACHE
    // BANK_SELECT = 9, L2_CACHE_BIGK_FRAGMENT_SIZE = 9, L2_CACHE_BIGK_ASSOCIATIVITY = 1 (fragment_size = 9).
    modify(MM_VM_L2_CNTL3, 0x3f | 0xf_8000 | 0x10_0000, 9 | (9 << 15) | 0x10_0000);
    modify(MM_VM_L2_CNTL4, 0x3_ffc0, 0); // запросы VMC_TAP — по умолчанию
    // Контекст 0: диапазон GART, таблица, страница для ошибок.
    window.wr(MM_VM_CONTEXT0_PAGE_TABLE_START_ADDR * 4, (GART_START >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_PAGE_TABLE_END_ADDR * 4, ((GART_START + (GART_PAGES * PAGE) as u64 - 1) >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_PAGE_TABLE_BASE_ADDR * 4, (table_mc >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_PROTECTION_FAULT_DEFAULT_ADDR * 4, (dummy_phys >> 12) as u32);
    window.wr(MM_VM_CONTEXT0_CNTL2 * 4, 0);
    // ENABLE_CONTEXT, PAGE_TABLE_DEPTH = 0, RANGE_PROTECTION_FAULT_ENABLE_DEFAULT.
    modify(MM_VM_CONTEXT0_CNTL, 0x1 | 0x6 | 0x10, 0x1 | 0x10);
    window.wr(MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_LOW_ADDR * 4, 0);
    window.wr(MM_VM_L2_CONTEXT1_IDENTITY_APERTURE_HIGH_ADDR * 4, 0);
    window.wr(MM_VM_L2_CONTEXT_IDENTITY_PHYSICAL_OFFSET * 4, 0);
    // Записи таблицы должны дойти до GPU раньше сброса TLB.
    window.hdp_flush();
    window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1);
    super::nvidia::spin_us(100);
    note(format!(
        "context 0 enabled: L1_TLB {:#x}->{:#x}, L2_CNTL {:#x}->{:#x}, L2_CNTL3 {:#x}->{:#x}, CONTEXT0_CNTL {:#x}->{:#x}",
        before[0],
        window.rd(MM_MC_VM_MX_L1_TLB_CNTL * 4),
        before[1],
        window.rd(MM_VM_L2_CNTL * 4),
        before[2],
        window.rd(MM_VM_L2_CNTL3 * 4),
        before[3],
        window.rd(MM_VM_CONTEXT0_CNTL * 4)
    ));
    // Проверка: CP пишет слово по GART-адресу страницы 1, ядро читает системную страницу.
    let packets = [PM4_WRITE_DATA_3, WRITE_DATA_MEMORY_CONFIRM, gart_va as u32, (gart_va >> 32) as u32, 0xface_feed];
    if let Err(why) = ring_submit(&window, &packets) {
        note(why);
        return lines;
    }
    let mut word = 0;
    for _ in 0..100_000 {
        // SAFETY: страница выделена выше и отображена прямым отображением.
        word = unsafe { core::ptr::read_volatile(test_virt as *const u32) };
        if word == 0xface_feed {
            break;
        }
        super::nvidia::spin_us(1);
    }
    if word == 0xface_feed {
        note(format!("TEST PASSED: the CP wrote 0xfacefeed through the GART at GPU {gart_va:#x} into the system page {test_phys:#x}"));
    } else {
        note(format!(
            "TEST FAILED: system page word {word:#x} (expected 0xfacefeed); fault status {:#x}, address {:#x}, client {:#x}",
            window.rd(MM_VM_CONTEXT0_PROTECTION_FAULT_STATUS * 4),
            window.rd(MM_VM_CONTEXT0_PROTECTION_FAULT_ADDR * 4),
            window.rd(MM_VM_CONTEXT0_PROTECTION_FAULT_MCCLIENT * 4)
        ));
        // Вернуть всё как было: контекст 0 выключить.
        modify(MM_VM_CONTEXT0_CNTL, 0x1, 0);
        window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1);
        note(String::from("context 0 disabled again"));
    }
    lines
}

// --- Fence, IB и пользовательское адресное пространство (`amd cs`) -----------
//
// Этап P5, шаг 2. Источники — `gfx_v8_0.c` (`ring_emit_fence_gfx`,
// `ring_emit_ib_gfx`, `ring_test_ib`), `gmc_v8_0.c` (`gmc_v8_0_gart_enable`:
// контексты 1..15, `emit_flush_gpu_tlb`), `amdgpu_vm.c`/`amdgpu_vm_pt.c`
// (раскладка PD/PT), `amdgpu_vm.h` (`AMDGPU_PTE_*`) (Linux 6.12).
//
// Три проверки на уже работающем кольце:
// 1. fence: `EVENT_WRITE_EOP` пишет число в видеопамять (так драйвер узнаёт, что
//    GPU закончил);
// 2. IB: пакет `INDIRECT_BUFFER` (VMID 0) гонит CP по буферу, где `WRITE_DATA`;
// 3. VMID 1: корень PD и таблица PT в видеопамяти, три страницы: системная и две
//    видеопамяти (в PTE адрес VRAM — смещением от начала VRAM и MC-адресом с
//    `vram_start`: в какую сторону трактует железо, по коду Linux однозначно не
//    выводится); IB исполняется с VMID 1 и пишет по пользовательским адресам.
// При неудаче контекст 1 снова выключается.

const MM_VM_CONTEXT1_CNTL: usize = 0x505;
const MM_VM_CONTEXT1_CNTL2: usize = 0x50d;
const MM_VM_CONTEXT1_PAGE_TABLE_BASE_ADDR: usize = 0x550;
const MM_VM_CONTEXT1_PAGE_TABLE_START_ADDR: usize = 0x558;
const MM_VM_CONTEXT1_PAGE_TABLE_END_ADDR: usize = 0x560;
const MM_VM_CONTEXT1_PROTECTION_FAULT_DEFAULT_ADDR: usize = 0x547;
const MM_VM_CONTEXT1_PROTECTION_FAULT_STATUS: usize = 0x537;
const MM_VM_CONTEXT1_PROTECTION_FAULT_MCCLIENT: usize = 0x539;
const MM_VM_CONTEXT1_PROTECTION_FAULT_ADDR: usize = 0x53f;
/// `PACKET3(EVENT_WRITE_EOP, 4)`, `PACKET3(INDIRECT_BUFFER, 2)`.
const PM4_EVENT_WRITE_EOP_4: u32 = 0xc004_4700;
const PM4_INDIRECT_BUFFER_2: u32 = 0xc002_3f00;
/// EOP_TCL1_ACTION_EN | EOP_TC_ACTION_EN | EOP_TC_WB_ACTION_EN | EVENT_TYPE(CACHE_FLUSH_AND_INV_TS_EVENT = 0x14) | EVENT_INDEX(5).
const EOP_CONTROL: u32 = (1 << 16) | (1 << 17) | (1 << 15) | 0x14 | (5 << 8);
/// DATA_SEL(1): записать младшие 32 бита значения; INT_SEL(0): без прерывания.
const EOP_DATA_SEL_LOW32: u32 = 1 << 29;
/// Размещение в окне VRAM кольца (после таблицы GART, 0x10000..0x18000).
const VM_PD_OFFSET: usize = 0x1_8000;
const VM_PT_OFFSET: usize = 0x1_9000;
const IB0_OFFSET: usize = 0x1_b000;
const VM_IB_OFFSET: usize = 0x1_c000;
const VM_PAGE_A_OFFSET: usize = 0x1_d000;
const VM_PAGE_B_OFFSET: usize = 0x1_e000;
const FENCE_SLOT: usize = RING_BYTES + 0x100;
const IB0_RESULT_SLOT: usize = RING_BYTES + 0x140;
/// Пользовательский VA: PDE 1, PTE 0..3 (0x400000 + n * 4096); 1 ГиБ (`max_pfn` = 2^18), блок 10 бит.
const VM_VA: u64 = 0x40_0000;
const VM_MAX_PFN: u32 = 1 << 18;
const PTE_VRAM_FLAGS: u64 = PTE_VALID | PTE_READABLE | PTE_WRITEABLE | PTE_EXECUTABLE;

/// Прочитать слово видеопамяти, сбросив перед этим кэш чтения HDP.
fn vram_word(window: &Window, vram: usize, offset: usize) -> u32 {
    window.hdp_invalidate();
    // SAFETY: слово внутри окна VRAM кольца, выровнено.
    unsafe { core::ptr::read_volatile((vram + offset) as *const u32) }
}

fn vram_put(vram: usize, offset: usize, value: u32) {
    // SAFETY: слово внутри окна VRAM кольца, выровнено.
    unsafe { core::ptr::write_volatile((vram + offset) as *mut u32, value) };
}

/// Ждать, пока `read()` вернёт `want` (до 100 мс).
fn wait_for(want: u32, mut read: impl FnMut() -> u32) -> u32 {
    let mut value = 0;
    for _ in 0..100_000 {
        value = read();
        if value == want {
            break;
        }
        super::nvidia::spin_us(1);
    }
    value
}

/// `amd cs`: fence, IB и пользовательское адресное пространство GPU (VMID 1).
#[must_use]
pub fn polaris_cs() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : cs: {text}");
        lines.push(text);
    };
    let Some((base, _, _)) = *POLARIS_MMIO.lock() else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    let Some((vram, vram_start)) = RING_STATE.lock().as_ref().map(|state| (state.vram, state.vram_start)) else {
        note(String::from("the ring is not running: run 'amd ring' first"));
        return lines;
    };
    let gpu = |offset: usize| vram_start + RING_AREA + offset as u64;
    let write_data = |address: u64, value: u32| [PM4_WRITE_DATA_3, WRITE_DATA_MEMORY_CONFIRM, address as u32, (address >> 32) as u32, value];

    // 1. fence: EVENT_WRITE_EOP пишет 0x1234abcd в видеопамять.
    vram_put(vram, FENCE_SLOT, 0);
    let fence_address = gpu(FENCE_SLOT);
    let packets = [
        PM4_EVENT_WRITE_EOP_4,
        EOP_CONTROL,
        fence_address as u32 & !3,
        ((fence_address >> 32) as u32 & 0xffff) | EOP_DATA_SEL_LOW32,
        0x1234_abcd,
        0,
    ];
    if let Err(why) = ring_submit(&window, &packets) {
        note(why);
        return lines;
    }
    let value = wait_for(0x1234_abcd, || vram_word(&window, vram, FENCE_SLOT));
    if value != 0x1234_abcd {
        note(format!("FENCE FAILED: VRAM word {value:#x} (expected 0x1234abcd), GRBM_STATUS {:#x}", window.rd(MM_GRBM_STATUS * 4)));
        return lines;
    }
    note(format!("FENCE PASSED: EVENT_WRITE_EOP wrote 0x1234abcd at GPU {fence_address:#x}"));

    // 2. IB (VMID 0): INDIRECT_BUFFER → буфер с WRITE_DATA.
    vram_put(vram, IB0_RESULT_SLOT, 0);
    let ib = write_data(gpu(IB0_RESULT_SLOT), 0xa1b2_c3d4);
    for (index, word) in ib.iter().enumerate() {
        vram_put(vram, IB0_OFFSET + index * 4, *word);
    }
    let ib_address = gpu(IB0_OFFSET);
    // control: длина в dword | VMID << 24 (VMID 0 — ядро).
    let packets = [PM4_INDIRECT_BUFFER_2, ib_address as u32 & !3, (ib_address >> 32) as u32 & 0xffff, ib.len() as u32];
    if let Err(why) = ring_submit(&window, &packets) {
        note(why);
        return lines;
    }
    let value = wait_for(0xa1b2_c3d4, || vram_word(&window, vram, IB0_RESULT_SLOT));
    if value != 0xa1b2_c3d4 {
        note(format!("IB FAILED: VRAM word {value:#x} (expected 0xa1b2c3d4), GRBM_STATUS {:#x}", window.rd(MM_GRBM_STATUS * 4)));
        return lines;
    }
    note(format!("IB PASSED: the CP ran an indirect buffer of {} dwords at GPU {ib_address:#x}", ib.len()));

    // 3. Пользовательское адресное пространство: VMID 1.
    let (dummy_phys, _) = match system_page(0) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    let (sys_phys, sys_virt) = match system_page(0x2222_2222) {
        Ok(page) => page,
        Err(why) => {
            note(why);
            return lines;
        }
    };
    // PD (4 КиБ) и PT (8 КиБ) нулями; PDE 1 → PT; PTE 0..3.
    for index in 0..(0x1000 + 0x2000) / 4 {
        vram_put(vram, VM_PD_OFFSET + index * 4, 0);
    }
    for (index, word) in [VM_PAGE_A_OFFSET, VM_PAGE_B_OFFSET].iter().enumerate() {
        vram_put(vram, *word, 0x3333_3333 + index as u32);
    }
    let put64 = |offset: usize, value: u64| {
        vram_put(vram, offset, value as u32);
        vram_put(vram, offset + 4, (value >> 32) as u32);
    };
    let pt_mc = gpu(VM_PT_OFFSET);
    let pd_mc = gpu(VM_PD_OFFSET);
    put64(VM_PD_OFFSET + 8, pt_mc | PTE_VALID); // PDE 1 (блок 10 бит: 4 МиБ на запись)
    let system_flags = PTE_VALID | PTE_SYSTEM | PTE_SNOOPED | PTE_READABLE | PTE_WRITEABLE | PTE_EXECUTABLE;
    put64(VM_PT_OFFSET, (sys_phys & 0x0000_ffff_ffff_f000) | system_flags); // VA +0x0000: системная страница
    put64(VM_PT_OFFSET + 8, (((RING_AREA + VM_PAGE_A_OFFSET as u64) & !0xfff) & 0x0000_ffff_ffff_f000) | PTE_VRAM_FLAGS); // +0x1000: VRAM, смещение от начала VRAM
    put64(VM_PT_OFFSET + 16, (gpu(VM_PAGE_B_OFFSET) & 0x0000_ffff_ffff_f000) | PTE_VRAM_FLAGS); // +0x2000: VRAM, MC-адрес с vram_start
    put64(VM_PT_OFFSET + 24, (gpu(VM_IB_OFFSET) & 0x0000_ffff_ffff_f000) | PTE_VRAM_FLAGS); // +0x3000: буфер команд (MC-адрес)
    // Буфер команд: три WRITE_DATA по пользовательским VA.
    let user_ib: Vec<u32> = [(VM_VA, 0x11aa_22bb), (VM_VA + 0x1000, 0x33cc_44dd), (VM_VA + 0x2000, 0x55ee_66ff)]
        .iter()
        .flat_map(|(address, value)| write_data(*address, *value))
        .collect();
    for (index, word) in user_ib.iter().enumerate() {
        vram_put(vram, VM_IB_OFFSET + index * 4, *word);
    }
    note(format!(
        "VM tables: PD at GPU {pd_mc:#x}, PT {pt_mc:#x}, VA {VM_VA:#x}: system page {sys_phys:#x}, VRAM A (offset form), VRAM B (MC form), IB page"
    ));
    // Контекст 1: диапазон, корень PD, страница для ошибок, управление (как `gmc_v8_0_gart_enable`).
    window.wr(MM_VM_CONTEXT1_PAGE_TABLE_START_ADDR * 4, 0);
    window.wr(MM_VM_CONTEXT1_PAGE_TABLE_END_ADDR * 4, VM_MAX_PFN - 1);
    window.wr(MM_VM_CONTEXT1_PAGE_TABLE_BASE_ADDR * 4, (pd_mc >> 12) as u32);
    window.wr(MM_VM_CONTEXT1_PROTECTION_FAULT_DEFAULT_ADDR * 4, (dummy_phys >> 12) as u32);
    window.wr(MM_VM_CONTEXT1_CNTL2 * 4, 4);
    // ENABLE_CONTEXT, PAGE_TABLE_DEPTH = 1, *_PROTECTION_FAULT_ENABLE_DEFAULT (диапазон, заглушка, PDE0, VALID,
    // READ, WRITE, EXECUTE), PAGE_TABLE_BLOCK_SIZE = 10 - 9.
    let fault_bits: u32 = 0x10 | 0x80 | 0x400 | 0x2000 | 0x1_0000 | 0x8_0000 | 0x40_0000;
    let control_clear: u32 = 0x1 | 0x6 | fault_bits | 0xf00_0000;
    let control = window.rd(MM_VM_CONTEXT1_CNTL * 4);
    window.wr(MM_VM_CONTEXT1_CNTL * 4, (control & !control_clear) | 0x1 | (1 << 1) | fault_bits | (1 << 24));
    window.hdp_flush();
    window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1 << 1);
    super::nvidia::spin_us(100);
    note(format!("context 1 enabled: CONTEXT1_CNTL {control:#x} -> {:#x}", window.rd(MM_VM_CONTEXT1_CNTL * 4)));
    // IB исполняется с VMID 1: адрес IB — пользовательский VA.
    let ib_va = VM_VA + 0x3000;
    let packets = [PM4_INDIRECT_BUFFER_2, ib_va as u32 & !3, (ib_va >> 32) as u32 & 0xffff, user_ib.len() as u32 | (1 << 24)];
    if let Err(why) = ring_submit(&window, &packets) {
        note(why);
        return lines;
    }
    let system = wait_for(0x11aa_22bb, || {
        // SAFETY: страница выделена выше, прямое отображение.
        unsafe { core::ptr::read_volatile(sys_virt as *const u32) }
    });
    let a = vram_word(&window, vram, VM_PAGE_A_OFFSET);
    let b = vram_word(&window, vram, VM_PAGE_B_OFFSET);
    note(format!(
        "VM RESULT: system page {} ({system:#x}), VRAM A (offset form) {} ({a:#x}), VRAM B (MC form) {} ({b:#x})",
        if system == 0x11aa_22bb { "PASSED" } else { "FAILED" },
        if a == 0x33cc_44dd { "PASSED" } else { "no" },
        if b == 0x55ee_66ff { "PASSED" } else { "no" }
    ));
    if system != 0x11aa_22bb && a != 0x33cc_44dd && b != 0x55ee_66ff {
        note(format!(
            "fault status {:#x}, address {:#x}, client {:#x}, GRBM_STATUS {:#x}",
            window.rd(MM_VM_CONTEXT1_PROTECTION_FAULT_STATUS * 4),
            window.rd(MM_VM_CONTEXT1_PROTECTION_FAULT_ADDR * 4),
            window.rd(MM_VM_CONTEXT1_PROTECTION_FAULT_MCCLIENT * 4),
            window.rd(MM_GRBM_STATUS * 4)
        ));
        window.wr(MM_VM_CONTEXT1_CNTL * 4, control & !0x1);
        window.wr(MM_VM_INVALIDATE_REQUEST * 4, 1 << 1);
        note(String::from("context 1 disabled again"));
    }
    lines
}

// --- Настройка GFX и первый compute-шейдер (`amd shader`) -------------------
//
// Этап P6. Источники — `gfx_v8_0.c` (`gfx_v8_0_init_golden_registers`,
// `gfx_v8_0_constants_init`), `amdgpu_device.c`
// (`amdgpu_device_program_register_sequence`), тест `amdgpu_sync_dependency_test`
// из libdrm 2.4.124 (`tests/amdgpu/basic_tests.c`, строки про `shader_bin` для
// GFX8) и `vid.h`/`gfx_8_0_d.h` (Linux 6.12).
//
// Шаг 1 — настройка, которую Linux делает до первых шейдеров: golden registers
// Polaris10 и `SH_MEM_*` VMID 0. Шаг 2 — dispatch на кольце GFX: шейдер из libdrm
// (проверен на GFX8) записывает 42 по адресу из `COMPUTE_USER_DATA_0/1` через
// `buffer_store_dword`, затем `WRITE_DATA` после `CS_PARTIAL_FLUSH` пишет флаг 99.
// Если шейдер не стартовал, а флаг пришёл, вторая попытка — с битом «шейдер
// compute» (`PACKET3_COMPUTE`) в заголовках `SET_SH_REG` и `DISPATCH_DIRECT`.

/// `golden_settings_polaris10_a11` и `polaris10_golden_common_all`: (регистр, маска, значение).
/// Правило `amdgpu_device_program_register_sequence`: при маске `0xffffffff` — просто значение,
/// иначе `(старое & !маска) | значение`.
const GOLDEN_POLARIS10: [(usize, u32, u32); 25] = [
    (0xcd4, 0x000c_0fc0, 0x000c_0200),  // ATC_MISC_CG
    (0x2684, 0x0001_f3cf, 0x0000_7208), // CB_HW_CONTROL
    (0x2686, 0x0f00_0000, 0x0f00_0000), // CB_HW_CONTROL_2
    (0x2683, 0x0000_01ff, 0x0000_0040), // CB_HW_CONTROL_3
    (0x260d, 0xf00f_ffff, 0x0000_0400), // DB_DEBUG2
    (0x22fc, 0xffff_ffff, 0x2000_0001), // PA_SC_ENHANCE
    (0xc281, 0x0000_ff0f, 0x0000_0000), // PA_SC_LINE_STIPPLE_STATE
    (0xa0d4, 0x3f3f_ffff, 0x1600_0012), // PA_SC_RASTER_CONFIG
    (0xa0d5, 0x0000_003f, 0x0000_002a), // PA_SC_RASTER_CONFIG_1
    (0xec49, 0x0000_0003, 0x0001_003c), // RLC_CGCG_CGLS_CTRL
    (0xec9d, 0xffff_ffff, 0x0001_003c), // RLC_CGCG_CGLS_CTRL_3D
    (0x2300, 0x07f8_0000, 0x0718_0000), // SQ_CONFIG
    (0x2542, 0x000f_000f, 0x000b_0000), // TA_CNTL_AUX
    (0x2b80, 0x0010_0000, 0xf31f_ff7f), // TCC_CTRL
    (0x2b05, 0x0000_03ff, 0x0000_00f7), // TCP_ADDR_CONFIG
    (0x2b04, 0xffff_ffff, 0x0000_0000), // TCP_CHAN_STEER_HI
    (0x2232, 0x0000_0004, 0x0000_0004), // VGT_RESET_DEBUG
    (0xc200, 0xffff_ffff, 0xe000_0000), // GRBM_GFX_INDEX (все SE/SH/экземпляры)
    (0xa0d4, 0xffff_ffff, 0x1600_0012), // PA_SC_RASTER_CONFIG
    (0xa0d5, 0xffff_ffff, 0x0000_002a), // PA_SC_RASTER_CONFIG_1
    (0x263e, 0xffff_ffff, 0x2201_1003), // GB_ADDR_CONFIG
    (0x31dc, 0xffff_ffff, 0x0000_0800), // SPI_RESOURCE_RESERVE_CU_0
    (0x31dd, 0xffff_ffff, 0x0000_0800), // SPI_RESOURCE_RESERVE_CU_1
    (0x31e6, 0xffff_ffff, 0x00ff_7fbf), // SPI_RESOURCE_RESERVE_EN_CU_0
    (0x31e7, 0xffff_ffff, 0x00ff_7faf), // SPI_RESOURCE_RESERVE_EN_CU_1
];
const MM_SH_MEM_BASES: usize = 0x230a;
const MM_SH_MEM_APE1_BASE: usize = 0x230b;
const MM_SH_MEM_APE1_LIMIT: usize = 0x230c;
const MM_SH_MEM_CONFIG: usize = 0x230d;
const MM_SPI_SLAVE_DEBUG_BUSY: usize = 0x24d3;
/// `SH_MEM_CONFIG` VMID 0: DEFAULT_MTYPE = UC (3), APE1_MTYPE = UC (3), ALIGNMENT_MODE = UNALIGNED (3).
const SH_MEM_CONFIG_VMID0: u32 = (3 << 5) | (3 << 8) | (3 << 3);
const PM4_SET_SH_REG_START: usize = 0x2c00;
const PM4_CONTEXT_CONTROL_1: u32 = 0xc001_2800;
const PM4_EVENT_WRITE_0: u32 = 0xc000_4600;
/// `PACKET3(SET_SH_REG, n)` и `PACKET3(DISPATCH_DIRECT, 3)`; бит 1 — «шейдер compute».
const fn set_sh_reg(count: u32, compute: bool) -> u32 {
    0xc000_7600 | (count << 16) | if compute { 2 } else { 0 }
}
const fn dispatch_direct(compute: bool) -> u32 {
    0xc003_1500 | if compute { 2 } else { 0 }
}
/// Шейдер из libdrm (`shader_bin` для GFX8): задержка циклом, затем `buffer_store_dword` значения 42
/// по V# `s[0:3]` (`s[0:1]` — адрес из `COMPUTE_USER_DATA_0/1`, `s2` = -1, `s3` = 0xf000), `s_endpgm`.
const SHADER_GFX8: [u32; 15] = [
    0xbe82_0080, // s_mov_b32 s2, 0
    0xbf08_ff02, 0x0098_967f, // s_cmp_gt_u32 s2, 9999999
    0xbf85_0004, // s_cbranch_scc1 +4
    0x8102_8102, // s_add_i32 s2, s2, 1
    0xbf08_ff02, 0x0098_967f, // s_cmp_gt_u32 s2, 9999999
    0xbf84_fffc, // s_cbranch_scc0 -4
    0xbe83_00ff, 0x0000_f000, // s_mov_b32 s3, 0xf000
    0xbe82_00c1, // s_mov_b32 s2, -1
    0x7e00_02aa, // v_mov_b32 v0, 42
    0xe070_0000, 0x8000_0000, // buffer_store_dword v0, off, s[0:3], 0
    0xbf81_0000, // s_endpgm
];
const SHADER_OFFSET: usize = 0x1_f000;
const SHADER_DATA_SLOT: usize = 0x1_f800;
const SHADER_FLAG_SLOT: usize = 0x1_f840;

/// Golden registers Polaris10 и `SH_MEM_*` VMID 0.
fn gfx_init(window: &Window) -> String {
    for (register, mask, value) in GOLDEN_POLARIS10 {
        let new = if mask == 0xffff_ffff { value } else { (window.rd(register * 4) & !mask) | value };
        window.wr(register * 4, new);
    }
    // VMID 0 в SRBM, как `vi_srbm_select(adev, 0, 0, 0, 0)`.
    window.wr(MM_SRBM_GFX_CNTL * 4, 0);
    window.wr(MM_SH_MEM_CONFIG * 4, SH_MEM_CONFIG_VMID0);
    window.wr(MM_SH_MEM_BASES * 4, 0);
    window.wr(MM_SH_MEM_APE1_BASE * 4, 1);
    window.wr(MM_SH_MEM_APE1_LIMIT * 4, 0);
    format!(
        "golden registers ({} entries) and SH_MEM (VMID 0) applied; SQ_CONFIG {:#x}, SPI_RESOURCE_RESERVE_EN_CU_0 {:#x}, SH_MEM_CONFIG {:#x}",
        GOLDEN_POLARIS10.len(),
        window.rd(0x2300 * 4),
        window.rd(0x31e6 * 4),
        window.rd(MM_SH_MEM_CONFIG * 4)
    )
}

/// `amd shader`: настройка GFX и запуск compute-шейдера на кольце GFX.
#[must_use]
pub fn polaris_shader() -> Vec<String> {
    let mut lines = Vec::new();
    let mut note = |text: String| {
        crate::kprintln!("  amdgpu      : shader: {text}");
        lines.push(text);
    };
    let Some((base, _, _)) = *POLARIS_MMIO.lock() else {
        note(String::from("no Polaris card was probed"));
        return lines;
    };
    let window = Window(base);
    let Some((vram, vram_start)) = RING_STATE.lock().as_ref().map(|state| (state.vram, state.vram_start)) else {
        note(String::from("the ring is not running: run 'amd ring' first"));
        return lines;
    };
    let gpu = |offset: usize| vram_start + RING_AREA + offset as u64;
    note(gfx_init(&window));
    // Шейдер в видеопамяти (адрес кратен 256: `COMPUTE_PGM` хранит адрес >> 8).
    for (index, word) in SHADER_GFX8.iter().enumerate() {
        vram_put(vram, SHADER_OFFSET + index * 4, *word);
    }
    for attempt in 0..2 {
        let compute = attempt == 1;
        vram_put(vram, SHADER_DATA_SLOT, 0);
        vram_put(vram, SHADER_FLAG_SLOT, 0);
        let program = gpu(SHADER_OFFSET) >> 8;
        let data = gpu(SHADER_DATA_SLOT);
        let flag = gpu(SHADER_FLAG_SLOT);
        let sh = |count: u32| set_sh_reg(count, compute);
        let reg = |register: usize| (register - PM4_SET_SH_REG_START) as u32;
        let ib: Vec<u32> = alloc::vec![
            PM4_CONTEXT_CONTROL_1, 0x8000_0000, 0x8000_0000,
            sh(2), reg(0x2e0c), program as u32, (program >> 32) as u32, // COMPUTE_PGM_LO/HI
            sh(2), reg(0x2e12), 0x002c_0040, 0x0000_0010, // COMPUTE_PGM_RSRC1/2
            sh(3), reg(0x2e16), 0xffff_ffff, 0xffff_ffff, 0x0000_0100, // STATIC_THREAD_MGMT_SE0/1, TMPRING_SIZE
            sh(2), reg(0x2e19), 0xffff_ffff, 0xffff_ffff, // STATIC_THREAD_MGMT_SE2/3
            sh(2), reg(0x2e40), data as u32, (data >> 32) as u32, // COMPUTE_USER_DATA_0/1
            sh(1), reg(0x2e15), 0, // COMPUTE_RESOURCE_LIMITS
            sh(3), reg(0x2e07), 1, 1, 1, // COMPUTE_NUM_THREAD_X/Y/Z
            dispatch_direct(compute), 1, 1, 1, 0x0000_0045, // DISPATCH_DIRECT 1x1x1
            PM4_EVENT_WRITE_0, 0x0000_0407, // CS_PARTIAL_FLUSH
            PM4_WRITE_DATA_3, WRITE_DATA_MEMORY_CONFIRM, flag as u32, (flag >> 32) as u32, 99,
        ];
        for (index, word) in ib.iter().enumerate() {
            vram_put(vram, IB0_OFFSET + index * 4, *word);
        }
        let ib_address = gpu(IB0_OFFSET);
        let packets = [PM4_INDIRECT_BUFFER_2, ib_address as u32 & !3, (ib_address >> 32) as u32 & 0xffff, ib.len() as u32];
        if let Err(why) = ring_submit(&window, &packets) {
            note(why);
            return lines;
        }
        // Шейдер крутит цикл на 10 млн шагов, поэтому ждём до 3 с.
        let mut data_word = 0;
        let mut flag_word = 0;
        for _ in 0..3000 {
            data_word = vram_word(&window, vram, SHADER_DATA_SLOT);
            flag_word = vram_word(&window, vram, SHADER_FLAG_SLOT);
            if data_word == 42 && flag_word == 99 {
                break;
            }
            super::nvidia::spin_us(1000);
        }
        let variant = if compute { "with the compute bit" } else { "plain headers" };
        if data_word == 42 && flag_word == 99 {
            note(format!("SHADER PASSED ({variant}): the shader wrote 42 and the CP wrote the flag 99 after CS_PARTIAL_FLUSH"));
            return lines;
        }
        note(format!(
            "attempt {} ({variant}): data {data_word:#x} (want 0x2a), flag {flag_word:#x} (want 0x63); GRBM_STATUS {:#010x}, GRBM_STATUS2 {:#010x}, CP_STAT {:#010x}, SPI_SLAVE_DEBUG_BUSY {:#x}, RB0_RPTR {:#x}",
            attempt + 1,
            window.rd(MM_GRBM_STATUS * 4),
            window.rd(0x2002 * 4),
            window.rd(MM_CP_STAT * 4),
            window.rd(MM_SPI_SLAVE_DEBUG_BUSY * 4),
            window.rd(MM_CP_RB0_RPTR * 4)
        ));
        // Флаг не пришёл — CP застрял на шейдере: вторая попытка бессмысленна.
        if flag_word != 99 {
            note(String::from("the flag did not arrive: the CP is stuck, no second attempt"));
            return lines;
        }
    }
    lines
}
