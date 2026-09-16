//! `ramfb` на QEMU `virt`: экран, память которого — обычная память гостя.
//!
//! Устройство не имеет регистров вовсе. Режим и адрес буфера задаются записью
//! файла `etc/ramfb` через fw_cfg — структурой `RAMFBCfg` (все поля
//! big-endian, упакована):
//!
//! ```text
//! u64 addr; u32 fourcc; u32 flags; u32 width; u32 height; u32 stride;
//! ```
//!
//! Буфер выделяет гость. Прошивка при загрузке выделила свой и больше его не
//! трогает; мы выделяем новый на каждую смену и отдаём прежний, если он был
//! нашим.
//!
//! fw_cfg в MMIO-варианте: данные `+0`, селектор `+8` (16 бит), адрес запроса
//! DMA `+16` (64 бита); все значения big-endian, операцию запускает запись
//! младшей половины адреса (`+20`). Читать и писать сам файл `etc/ramfb`
//! можно только через DMA: традиционная запись в регистр данных в современных
//! QEMU отключена.

use boot_info::Framebuffer;

use super::DisplayError;
use crate::mm::{PAGE_SIZE, PageFlags, PhysAddr};

/// ACPI ID устройства fw_cfg в DSDT.
const FW_CFG_HID: &[u8] = b"QEMU0002";

const KEY_SIGNATURE: u16 = 0x0000;
const KEY_ID: u16 = 0x0001;
const KEY_FILE_DIR: u16 = 0x0019;
/// Признак интерфейса DMA в `FW_CFG_ID`.
const ID_DMA: u32 = 1 << 1;

const REG_DATA: usize = 0;
const REG_SELECTOR: usize = 8;
const REG_DMA_HIGH: usize = 16;
const REG_DMA_LOW: usize = 20;
/// Меньше этого окно регистров без DMA, и тогда `etc/ramfb` не записать.
const WINDOW_MIN: u32 = 0x18;

const DMA_ERROR: u32 = 0x01;
const DMA_SELECT: u32 = 0x08;
const DMA_WRITE: u32 = 0x10;

/// `DRM_FORMAT_XRGB8888`: байты в памяти B, G, R, X — тот же порядок, что у
/// буфера от прошивки.
const FOURCC_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

/// Размер `RAMFBCfg`.
const CFG_LEN: usize = 28;

/// Смещения полей FADT: 32- и 64-разрядный адрес DSDT.
const FADT_DSDT32: usize = 40;
const FADT_DSDT64: usize = 140;

/// Сколько записей каталога fw_cfg читать самое большее. У QEMU их десятки;
/// предел — от испорченного счётчика, который иначе увёл бы чтение в миллионы
/// обращений к регистру.
const MAX_FILES: u32 = 512;

/// Буфер, выделенный нами под прошлый режим, — чтобы вернуть его в пул.
static OURS: crate::sync::SpinLock<Option<(PhysAddr, usize)>> = crate::sync::SpinLock::new(None);

/// Вся смена режима идёт под одним замком: селектор fw_cfg — общий регистр.
static FW_CFG: crate::sync::Mutex<()> = crate::sync::Mutex::new(());

pub fn set_mode(current: &Framebuffer, width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
    let _guard = FW_CFG.lock();
    let base = fw_cfg_window()?;
    let port = FwCfg::map(base)?;

    if port.read_bytes::<4>(KEY_SIGNATURE) != *b"QEMU" {
        return Err(DisplayError::NoDriver("fw_cfg signature is not QEMU"));
    }
    if u32::from_le_bytes(port.read_bytes::<4>(KEY_ID)) & ID_DMA == 0 {
        return Err(DisplayError::Refused("fw_cfg without the DMA interface"));
    }
    let select = port
        .find_file(b"etc/ramfb")
        .ok_or(DisplayError::NoDriver("no ramfb on this machine"))?;

    let stride_bytes = u64::from(width) * 4;
    let needed = stride_bytes * u64::from(height);
    let pages = (needed as usize).div_ceil(PAGE_SIZE);
    let buffer = crate::mm::frame::with(|frames| frames.allocate_contiguous(pages))
        .flatten()
        .ok_or(DisplayError::OutOfMemory)?;

    let mut cfg = [0u8; CFG_LEN];
    cfg[0..8].copy_from_slice(&buffer.as_u64().to_be_bytes());
    cfg[8..12].copy_from_slice(&FOURCC_XRGB8888.to_be_bytes());
    cfg[12..16].copy_from_slice(&0u32.to_be_bytes());
    cfg[16..20].copy_from_slice(&width.to_be_bytes());
    cfg[20..24].copy_from_slice(&height.to_be_bytes());
    cfg[24..28].copy_from_slice(&(stride_bytes as u32).to_be_bytes());

    if let Err(err) = port.dma_write(select, &cfg) {
        // Устройство буфер не взяло — он наш, и вернуть его надо сразу.
        // SAFETY: кадры выделены только что и устройству не отданы.
        crate::mm::frame::with(|frames| unsafe { frames.free_contiguous(buffer, pages) });
        return Err(err);
    }

    // Прежний буфер отдаётся только после того, как устройство взяло новый:
    // до этого QEMU читал картинку из него. Буфер прошивки не наш — он в
    // `OURS` не попадает и остаётся зарезервированным, как его оставила она.
    if let Some((old, old_pages)) = OURS.lock().replace((buffer, pages)) {
        // SAFETY: устройство уже читает новый буфер; к старому никто больше не
        // обращается — стол перейдёт на новый раньше, чем что-то нарисует.
        crate::mm::frame::with(|frames| unsafe { frames.free_contiguous(old, old_pages) });
    }

    Ok(Framebuffer {
        base: buffer.to_direct_map().as_usize() as u64,
        size: (pages * PAGE_SIZE) as u64,
        width,
        height,
        stride: width,
        format: current.format,
    })
}

/// Адрес окна регистров fw_cfg — из DSDT, по ACPI ID.
///
/// AML не исполняется: ищется строка `QEMU0002`, а за ней, в пределах самого
/// описания устройства, дескриптор `Memory32Fixed` (метка `0x86`, длина 9).
/// Так описывает fw_cfg таблица QEMU `virt`; описание другого вида — это не
/// повод угадывать, а ответ «не нашли».
fn fw_cfg_window() -> Result<u64, DisplayError> {
    let rsdp = crate::acpi::rsdp();
    if rsdp == 0 {
        return Err(DisplayError::NoDriver("no ACPI tables"));
    }
    // SAFETY: RSDP из хэндоффа, прямое отображение активно, таблицы ACPI
    // лежат в памяти, которую ядро не переиспользует.
    let fadt = unsafe { crate::acpi::find_table(rsdp, b"FACP") }
        .map_err(|_| DisplayError::NoDriver("no FADT"))?;
    let address = if fadt.len() >= FADT_DSDT64 + 8 && crate::acpi::read_u64(fadt, FADT_DSDT64) != 0 {
        crate::acpi::read_u64(fadt, FADT_DSDT64)
    } else if fadt.len() >= FADT_DSDT32 + 4 {
        u64::from(crate::acpi::read_u32(fadt, FADT_DSDT32))
    } else {
        0
    };
    if address == 0 {
        return Err(DisplayError::NoDriver("FADT names no DSDT"));
    }
    // SAFETY: см. выше; подпись и сумма проверяются внутри.
    let dsdt = unsafe { crate::acpi::table_at(address, b"DSDT") }
        .map_err(|_| DisplayError::NoDriver("DSDT unreadable"))?;

    let at = dsdt
        .windows(FW_CFG_HID.len())
        .position(|window| window == FW_CFG_HID)
        .ok_or(DisplayError::NoDriver("DSDT describes no fw_cfg"))?;
    // Описание устройства короткое: `_HID`, `_STA`, `_CCA`, `_CRS`. Сотни
    // байт с запасом, но не весь остаток таблицы — там дескрипторы чужих
    // устройств.
    let tail = &dsdt[at..dsdt.len().min(at + 160)];
    for index in 0..tail.len().saturating_sub(12) {
        if tail[index] == 0x86 && tail[index + 1] == 0x09 && tail[index + 2] == 0x00 {
            let base = u32::from_le_bytes(tail[index + 4..index + 8].try_into().unwrap_or_default());
            let len = u32::from_le_bytes(tail[index + 8..index + 12].try_into().unwrap_or_default());
            if base == 0 || len < WINDOW_MIN {
                return Err(DisplayError::Refused("fw_cfg window too small for DMA"));
            }
            return Ok(u64::from(base));
        }
    }
    Err(DisplayError::NoDriver("fw_cfg has no memory window in DSDT"))
}

/// Окно регистров fw_cfg, отображённое в ядро.
struct FwCfg {
    base: usize,
}

impl FwCfg {
    fn map(phys: u64) -> Result<Self, DisplayError> {
        let page = phys & !(PAGE_SIZE as u64 - 1);
        let offset = (phys - page) as usize;
        let virt = PhysAddr::new(page).to_direct_map();
        // SAFETY: собственные таблицы ядра активны; страница — регистры
        // устройства, найденные в DSDT, и получает семантику Device.
        unsafe {
            crate::arch::map_active(
                virt,
                PhysAddr::new(page),
                PAGE_SIZE,
                // Экран — не регистры, записи в него объединять можно и нужно
                // (см. [`PageFlags::WRITE_COMBINE`]).
                PageFlags::READ | PageFlags::WRITE | PageFlags::WRITE_COMBINE,
            )
        }
        .map_err(|_| DisplayError::OutOfMemory)?;
        Ok(Self { base: virt.as_usize() + offset })
    }

    fn select(&self, key: u16) {
        // SAFETY: окно отображено в `map`; селектор — 16-битный регистр.
        unsafe { ((self.base + REG_SELECTOR) as *mut u16).write_volatile(key.to_be()) };
    }

    fn read_byte(&self) -> u8 {
        // SAFETY: см. `select`; регистр данных читается побайтно.
        unsafe { ((self.base + REG_DATA) as *const u8).read_volatile() }
    }

    fn read_bytes<const N: usize>(&self, key: u16) -> [u8; N] {
        self.select(key);
        let mut out = [0u8; N];
        for byte in &mut out {
            *byte = self.read_byte();
        }
        out
    }

    /// Номер селектора файла по имени.
    fn find_file(&self, name: &[u8]) -> Option<u16> {
        let count = u32::from_be_bytes(self.read_bytes::<4>(KEY_FILE_DIR));
        // Селектор уже стоит на каталоге, и чтение продолжается с пятого байта.
        for _ in 0..count.min(MAX_FILES) {
            let mut entry = [0u8; 64];
            for byte in &mut entry {
                *byte = self.read_byte();
            }
            let select = u16::from_be_bytes([entry[4], entry[5]]);
            let raw = &entry[8..];
            let len = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            if &raw[..len] == name {
                return Some(select);
            }
        }
        None
    }

    /// Записать файл целиком одним запросом DMA.
    fn dma_write(&self, select: u16, bytes: &[u8]) -> Result<(), DisplayError> {
        let request = crate::mm::dma::alloc(PAGE_SIZE).map_err(|_| DisplayError::OutOfMemory)?;
        let control = (u32::from(select) << 16) | DMA_SELECT | DMA_WRITE;
        let data_phys = request.phys().as_u64() + 16;
        // SAFETY: буфер на страницу, записи в первые 16 + len байт; содержимое
        // читает устройство, поэтому `volatile`.
        unsafe {
            let ptr = request.as_ptr::<u8>();
            let mut header = [0u8; 16];
            header[0..4].copy_from_slice(&control.to_be_bytes());
            header[4..8].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
            header[8..16].copy_from_slice(&data_phys.to_be_bytes());
            for (index, byte) in header.iter().chain(bytes.iter()).enumerate() {
                ptr.add(index).write_volatile(*byte);
            }
        }

        let address = request.phys().as_u64();
        // SAFETY: окно отображено; старшая половина запоминается, младшая
        // запускает операцию — QEMU выполняет её до возврата из записи.
        unsafe {
            ((self.base + REG_DMA_HIGH) as *mut u32).write_volatile(((address >> 32) as u32).to_be());
            ((self.base + REG_DMA_LOW) as *mut u32).write_volatile((address as u32).to_be());
        }

        // SAFETY: чтение слова управления из того же буфера.
        let done = unsafe { u32::from_be((request.as_ptr::<u32>()).read_volatile()) };
        // SAFETY: устройство запрос закончило — буфер больше никому не нужен.
        unsafe { crate::mm::dma::free(&request) };
        if done & DMA_ERROR != 0 {
            return Err(DisplayError::Refused("fw_cfg reported a DMA error"));
        }
        if done != 0 {
            return Err(DisplayError::Refused("fw_cfg did not finish the DMA write"));
        }
        Ok(())
    }
}
