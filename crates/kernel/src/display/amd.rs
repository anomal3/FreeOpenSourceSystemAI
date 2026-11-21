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
    crate::kprintln!("  amdgpu     : auto start: marker found, starting the SMC");
    let _ = polaris_smc_start();
    for line in polaris_smc() {
        crate::kprintln!("  amdgpu     : auto start: smc: {line}");
    }
}
