//! `/dev/dri/renderD128` для Mesa RADV: uAPI DRM `amdgpu` (22.11.2026,
//! `docs/AMD_DRM_PLAN.md`, шаг «AMDGPU_INFO на программной карте»).
//!
//! # Зачем
//!
//! RADV (`libvulkan_radeon.so` из Debian) говорит с ядром только через узел
//! DRM: `drmGetDevices2` libdrm находит его по `/dev/dri` и
//! `/sys/dev/char/226:128/…`, `DRM_IOCTL_VERSION` должен назвать драйвер
//! «amdgpu» (major 3), а `AMDGPU_INFO` отдаёт описание карты
//! ([`DrmAmdgpuInfoDevice`] и др.; список запросов — `docs/AMD_A4_CALLS.md`).
//!
//! # Узел
//!
//! Узел `renderD128` один: пока включён AMD ([`set_soft`]), узел принадлежит
//! ему, иначе — NVIDIA (`drm::backend`). Файл открытого узла — [`AmdFile`], а
//! не [`super::DrmFile`]: у AMD нет ни каналов nouveau, ни NVIF.
//!
//! # Что есть
//!
//! * `VERSION`, `GET_UNIQUE`, `GET_CAP`, `SET_CLIENT_CAP`, права узла рендера;
//! * `AMDGPU_INFO`: `ACCEL_WORKING`, `DEV_INFO`, `READ_MMR_REG`, `HW_IP_INFO`,
//!   `HW_IP_COUNT`, `FW_VERSION`, `MEMORY`, `VRAM_GTT`, `TIMESTAMP` и счётчики;
//! * числа [`AmdInfo`] — Polaris10 (RX 590): часть сняты с настоящей карты
//!   (PCI `1002:6fdf` rev `e7`, 8 ГиБ, `GB_ADDR_CONFIG` `0x22011003`, версии
//!   прошивок), остальные — из драйвера Linux по умолчанию и **не проверены**
//!   RADV (помечены в [`AmdInfo::polaris10`]).
//!
//! GEM, `CS`, `GEM_VA`, контексты — следующие шаги плана.

use alloc::string::String;
use alloc::sync::Arc;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use nvgpu::uapi::{
    DRM_CAP_PRIME, DRM_CAP_SYNCOBJ, DRM_CAP_SYNCOBJ_TIMELINE, DRM_CAP_TIMESTAMP_MONOTONIC, DRM_IOCTL_AUTH_MAGIC as AUTH_MAGIC,
    DRM_IOCTL_BASE as DRM_TYPE, DRM_IOCTL_DROP_MASTER as DROP_MASTER, DRM_IOCTL_GET_CAP as GET_CAP,
    DRM_IOCTL_GET_MAGIC as GET_MAGIC, DRM_IOCTL_GET_UNIQUE as GET_UNIQUE, DRM_IOCTL_SET_CLIENT_CAP as SET_CLIENT_CAP,
    DRM_IOCTL_SET_MASTER as SET_MASTER, DRM_IOCTL_VERSION as VERSION, DRM_PRIME_CAP_EXPORT, DRM_PRIME_CAP_IMPORT, DrmGetCap,
    DrmVersion,
};

use super::{get, put, EOPNOTSUPP};
use crate::linux::e;
use crate::user::files::Object;

/// Версия uAPI: `3.59.0` — как у ядра 6.12. libdrm требует major 3, RADV — не
/// меньше 3.27; поля, появившиеся позже, при меньшей версии не читаются.
const DRIVER_MAJOR: u32 = 3;
const DRIVER_MINOR: u32 = 59;

/// `AMDGPU_FAMILY_VI` (Polaris, Fiji, Tonga).
const AMDGPU_FAMILY_VI: u32 = 130;
/// `AMDGPU_VRAM_TYPE_GDDR5`.
const AMDGPU_VRAM_TYPE_GDDR5: u32 = 5;

// Запросы `AMDGPU_INFO` (`amdgpu_drm.h`).
const INFO_ACCEL_WORKING: u32 = 0x00;
const INFO_HW_IP_INFO: u32 = 0x02;
const INFO_HW_IP_COUNT: u32 = 0x03;
const INFO_TIMESTAMP: u32 = 0x05;
const INFO_FW_VERSION: u32 = 0x0e;
const INFO_NUM_BYTES_MOVED: u32 = 0x0f;
const INFO_VRAM_USAGE: u32 = 0x10;
const INFO_GTT_USAGE: u32 = 0x11;
const INFO_VRAM_GTT: u32 = 0x14;
const INFO_READ_MMR_REG: u32 = 0x15;
const INFO_DEV_INFO: u32 = 0x16;
const INFO_VIS_VRAM_USAGE: u32 = 0x17;
const INFO_NUM_EVICTIONS: u32 = 0x18;
const INFO_MEMORY: u32 = 0x19;
const INFO_NUM_VRAM_CPU_PAGE_FAULTS: u32 = 0x1e;
const INFO_VRAM_LOST_COUNTER: u32 = 0x1f;

// Типы прошивок `AMDGPU_INFO_FW_*`.
const FW_GFX_ME: u32 = 0x04;
const FW_GFX_PFP: u32 = 0x05;
const FW_GFX_CE: u32 = 0x06;
const FW_GFX_RLC: u32 = 0x07;
const FW_GFX_MEC: u32 = 0x08;
const FW_SMC: u32 = 0x0a;
const FW_SDMA: u32 = 0x0b;

// Типы IP `AMDGPU_HW_IP_*`.
const HW_IP_GFX: u32 = 0;
const HW_IP_COMPUTE: u32 = 1;
const HW_IP_DMA: u32 = 2;
const HW_IP_NUM: u32 = 10;

/// `DRM_COMMAND_BASE`: номера ioctl драйвера начинаются с него.
const DRM_COMMAND_BASE: u32 = 0x40;
const DRM_AMDGPU_INFO: u32 = 0x05;

/// `struct drm_amdgpu_info`: `query` и объединение из четырёх слов (разбор — по запросу).
#[repr(C)]
#[derive(Clone, Copy)]
struct DrmAmdgpuInfo {
    return_pointer: u64,
    return_size: u32,
    query: u32,
    word: [u32; 4],
}
const _: () = assert!(core::mem::size_of::<DrmAmdgpuInfo>() == 32);

/// `struct drm_amdgpu_heap_info` — 32 байта.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DrmAmdgpuHeapInfo {
    total_heap_size: u64,
    usable_heap_size: u64,
    heap_usage: u64,
    max_allocation: u64,
}

/// `struct drm_amdgpu_memory_info` — три кучи, 96 байт.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DrmAmdgpuMemoryInfo {
    vram: DrmAmdgpuHeapInfo,
    cpu_accessible_vram: DrmAmdgpuHeapInfo,
    gtt: DrmAmdgpuHeapInfo,
}
const _: () = assert!(core::mem::size_of::<DrmAmdgpuMemoryInfo>() == 96);

/// `struct drm_amdgpu_info_vram_gtt` — 24 байта.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DrmAmdgpuInfoVramGtt {
    vram_size: u64,
    vram_cpu_accessible_size: u64,
    gtt_size: u64,
}

/// `struct drm_amdgpu_info_hw_ip` — 32 байта.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DrmAmdgpuInfoHwIp {
    hw_ip_version_major: u32,
    hw_ip_version_minor: u32,
    capabilities_flags: u64,
    ib_start_alignment: u32,
    ib_size_alignment: u32,
    available_rings: u32,
    ip_discovery_version: u32,
}
const _: () = assert!(core::mem::size_of::<DrmAmdgpuInfoHwIp>() == 32);

/// `struct drm_amdgpu_info_firmware` — 8 байт.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DrmAmdgpuInfoFirmware {
    ver: u32,
    feature: u32,
}

/// `struct drm_amdgpu_info_device` (ядро 6.12) — 440 байт.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DrmAmdgpuInfoDevice {
    device_id: u32,
    chip_rev: u32,
    external_rev: u32,
    pci_rev: u32,
    family: u32,
    num_shader_engines: u32,
    num_shader_arrays_per_engine: u32,
    gpu_counter_freq: u32,
    max_engine_clock: u64,
    max_memory_clock: u64,
    cu_active_number: u32,
    cu_ao_mask: u32,
    cu_bitmap: [[u32; 4]; 4],
    enabled_rb_pipes_mask: u32,
    num_rb_pipes: u32,
    num_hw_gfx_contexts: u32,
    pcie_gen: u32,
    ids_flags: u64,
    virtual_address_offset: u64,
    virtual_address_max: u64,
    virtual_address_alignment: u32,
    pte_fragment_size: u32,
    gart_page_size: u32,
    ce_ram_size: u32,
    vram_type: u32,
    vram_bit_width: u32,
    vce_harvest_config: u32,
    gc_double_offchip_lds_buf: u32,
    prim_buf_gpu_addr: u64,
    pos_buf_gpu_addr: u64,
    cntl_sb_buf_gpu_addr: u64,
    param_buf_gpu_addr: u64,
    prim_buf_size: u32,
    pos_buf_size: u32,
    cntl_sb_buf_size: u32,
    param_buf_size: u32,
    wave_front_size: u32,
    num_shader_visible_vgprs: u32,
    num_cu_per_sh: u32,
    num_tcc_blocks: u32,
    gs_vgt_table_depth: u32,
    gs_prim_buffer_depth: u32,
    max_gs_waves_per_vgt: u32,
    pcie_num_lanes: u32,
    cu_ao_bitmap: [[u32; 4]; 4],
    high_va_offset: u64,
    high_va_max: u64,
    pa_sc_tile_steering_override: u32,
    tcc_disabled_mask: u64,
    min_engine_clock: u64,
    min_memory_clock: u64,
    tcp_cache_size: u32,
    num_sqc_per_wgp: u32,
    sqc_data_cache_size: u32,
    sqc_inst_cache_size: u32,
    gl1c_cache_size: u32,
    gl2c_cache_size: u32,
    mall_size: u64,
    enabled_rb_pipes_mask_hi: u32,
    shadow_size: u32,
    shadow_alignment: u32,
    csa_size: u32,
    csa_alignment: u32,
}

/// Описание карты для `AMDGPU_INFO` и sysfs.
pub struct AmdInfo {
    pub pci_vendor: u16,
    pub pci_device: u16,
    pub pci_revision: u8,
    pub subsys_vendor: u16,
    pub subsys_device: u16,
    /// `(домен, шина, устройство, функция)`.
    pub pci_slot: (u16, u8, u8, u8),
    pub vram_bytes: u64,
    /// Видимая процессору часть VRAM (окно BAR0).
    pub vram_visible_bytes: u64,
    /// Сколько системной памяти можно отдать под GTT.
    pub gtt_bytes: u64,
    /// `GB_ADDR_CONFIG` (его читает RADV через `READ_MMR_REG` `0x263e`).
    pub gb_addr_config: u32,
    /// Версии прошивок `(ver, feature)`: ME, PFP, CE, RLC, MEC, SMC, SDMA.
    pub fw: [(u32, u32); 7],
}

impl AmdInfo {
    /// Polaris10, RX 590 GME. **Сняты с карты** (журнал `LOG.TXT`, ROADMAP P1–P6):
    /// PCI `1002:6fdf` rev `e7`, шина `03:00.0`, подсистема `1002:0b31`, 8192 МиБ,
    /// окно BAR0 256 МиБ, `GB_ADDR_CONFIG` `0x22011003`, версии микрокода из
    /// `polaris10_*_2.bin`. **Не проверены RADV** (значения драйвера Linux для
    /// RX 580/590): число SE/CU/RB, частоты, `virtual_address_*`, кэши.
    #[must_use]
    pub fn polaris10() -> Self {
        Self {
            pci_vendor: 0x1002,
            pci_device: 0x6fdf,
            pci_revision: 0xe7,
            subsys_vendor: 0x1002,
            subsys_device: 0x0b31,
            pci_slot: (0, 3, 0, 0),
            vram_bytes: 8 << 30,
            vram_visible_bytes: 256 << 20,
            gtt_bytes: 4 << 30,
            gb_addr_config: 0x2201_1003,
            // ucode_version из заголовков прошивок: me_2 0xa7, pfp_2 0xfe, ce 0x?,
            // rlc 0x11e, mec_2 0x2da, smc_k 0x171100(?), sdma 0x3a.
            fw: [(0xa7, 0), (0xfe, 0), (0x91, 0), (0x11e, 0), (0x2da, 0), (0x17_1100, 0), (0x3a, 0)],
        }
    }

    fn device_info(&self) -> DrmAmdgpuInfoDevice {
        let mut cu_bitmap = [[0u32; 4]; 4];
        for engine in &mut cu_bitmap {
            engine[0] = 0x1ff; // 9 CU на SE (4 SE × 9 = 36 CU)
        }
        DrmAmdgpuInfoDevice {
            device_id: u32::from(self.pci_device),
            chip_rev: 0,
            external_rev: 0x50, // POLARIS10: `rev_id + 0x50`, диапазон 0x50..0x5a
            pci_rev: u32::from(self.pci_revision),
            family: AMDGPU_FAMILY_VI,
            num_shader_engines: 4,
            num_shader_arrays_per_engine: 1,
            gpu_counter_freq: 27_000, // кГц
            max_engine_clock: 1_500_000,
            max_memory_clock: 2_000_000,
            cu_active_number: 36,
            cu_ao_mask: 0,
            cu_bitmap,
            enabled_rb_pipes_mask: 0xff,
            num_rb_pipes: 8,
            num_hw_gfx_contexts: 8,
            pcie_gen: 3,
            ids_flags: 0, // дискретная: без FUSION
            virtual_address_offset: 1 << 16,
            virtual_address_max: (1 << 30) - 0x40_4000, // 1 ГиБ VA (наш PD) без верхнего резерва
            virtual_address_alignment: 4096,
            pte_fragment_size: 512 * 4096, // фрагмент 9 бит
            gart_page_size: 4096,
            ce_ram_size: 32 * 1024,
            vram_type: AMDGPU_VRAM_TYPE_GDDR5,
            vram_bit_width: 256,
            gc_double_offchip_lds_buf: 1,
            wave_front_size: 64,
            num_shader_visible_vgprs: 256,
            num_cu_per_sh: 9,
            num_tcc_blocks: 16,
            gs_vgt_table_depth: 32,
            gs_prim_buffer_depth: 1792,
            max_gs_waves_per_vgt: 32,
            pcie_num_lanes: 16,
            min_engine_clock: 300_000,
            min_memory_clock: 300_000,
            ..DrmAmdgpuInfoDevice::default()
        }
    }
}
const _: () = assert!(core::mem::size_of::<DrmAmdgpuInfoDevice>() == 440);

/// Включён ли узел AMD (`amdnode soft on`).
static SOFT: AtomicBool = AtomicBool::new(false);

/// Узел AMD сейчас есть.
#[must_use]
pub fn active() -> bool {
    SOFT.load(Ordering::Relaxed)
}

/// `amdnode soft on|off`: программный узел AMD (QEMU, пробы).
pub fn set_soft(on: bool) {
    if on {
        crate::kprintln!("amdgpu: drm node soft on ({})", "Polaris10 RX 590");
    }
    SOFT.store(on, Ordering::Relaxed);
}

/// Что про узел знает sysfs.
#[must_use]
pub fn ident() -> Option<super::Ident> {
    if !active() {
        return None;
    }
    let info = AmdInfo::polaris10();
    let (domain, bus, dev, func) = info.pci_slot;
    Some(super::Ident {
        driver: "amdgpu",
        vendor: info.pci_vendor,
        device: info.pci_device,
        revision: info.pci_revision,
        subsys_vendor: info.subsys_vendor,
        subsys_device: info.subsys_device,
        slot_name: alloc::format!("{domain:04x}:{bus:02x}:{dev:02x}.{func:x}"),
    })
}

/// Открытый узел AMD.
pub struct AmdFile {
    info: AmdInfo,
    pid: u32,
}

impl Drop for AmdFile {
    fn drop(&mut self) {
        crate::kprintln!("amdgpu: drm close pid {}", self.pid);
    }
}

impl Object for AmdFile {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        // События DRM (vblank, flip) у узла рендера не приходят.
        Err(-e::EAGAIN)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (false, true, false)
    }
    fn mode(&self) -> u32 {
        0o020_000 // S_IFCHR
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `open("/dev/dri/renderD128")` при включённом узле AMD.
pub fn open(flags: usize) -> i64 {
    const O_NONBLOCK: usize = 0o4000;
    const O_CLOEXEC: usize = 0o2_000_000;
    let pid = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
    crate::kprintln!("amdgpu: drm open pid {pid}");
    let file: Arc<dyn Object> = Arc::new(AmdFile { info: AmdInfo::polaris10(), pid });
    super::install(file, flags & O_NONBLOCK != 0, flags & O_CLOEXEC != 0)
}

/// Байты значения `T` (POD из uAPI).
fn bytes_of<T: Copy>(value: &T) -> &[u8] {
    // SAFETY: `T` — `repr(C)` без указателей; читаем её собственную память.
    unsafe { core::slice::from_raw_parts((value as *const T).cast::<u8>(), core::mem::size_of::<T>()) }
}

/// Записать ответ в `return_pointer` (не больше `return_size` байт, как `copy_to_user` ядра).
fn reply(request: &DrmAmdgpuInfo, bytes: &[u8]) -> i64 {
    let take = (request.return_size as usize).min(bytes.len());
    if take == 0 || request.return_pointer == 0 {
        return -e::EINVAL;
    }
    match crate::linux::ipc::write_user(request.return_pointer as usize, &bytes[..take]) {
        Ok(()) => 0,
        Err(_) => -e::EFAULT,
    }
}

/// `DRM_IOCTL_AMDGPU_INFO`.
fn info_ioctl(file: &AmdFile, arg: usize) -> i64 {
    let Ok(request) = get::<DrmAmdgpuInfo>(arg) else { return -e::EFAULT };
    let info = &file.info;
    match request.query {
        INFO_ACCEL_WORKING => reply(&request, &1u32.to_le_bytes()),
        INFO_DEV_INFO => reply(&request, bytes_of(&info.device_info())),
        INFO_HW_IP_COUNT => {
            let count = match request.word[0] {
                HW_IP_GFX => 1u32, // кольцо GFX; compute (MEC) и SDMA — следующие шаги
                HW_IP_COMPUTE | HW_IP_DMA => 0,
                t if t < HW_IP_NUM => 0,
                _ => return -e::EINVAL,
            };
            reply(&request, &count.to_le_bytes())
        }
        INFO_HW_IP_INFO => {
            let mut hw = DrmAmdgpuInfoHwIp::default();
            match request.word[0] {
                HW_IP_GFX => {
                    hw.hw_ip_version_major = 8;
                    hw.hw_ip_version_minor = 0;
                    hw.ib_start_alignment = 32;
                    hw.ib_size_alignment = 32;
                    hw.available_rings = 1;
                }
                t if t < HW_IP_NUM => {}
                _ => return -e::EINVAL,
            }
            reply(&request, bytes_of(&hw))
        }
        INFO_FW_VERSION => {
            // `query_fw`: fw_type, ip_instance, index, pad.
            let (ver, feature) = match request.word[0] {
                FW_GFX_ME => info.fw[0],
                FW_GFX_PFP => info.fw[1],
                FW_GFX_CE => info.fw[2],
                FW_GFX_RLC => info.fw[3],
                FW_GFX_MEC => info.fw[4],
                FW_SMC => info.fw[5],
                FW_SDMA => info.fw[6],
                _ => return -e::EINVAL,
            };
            reply(&request, bytes_of(&DrmAmdgpuInfoFirmware { ver, feature }))
        }
        INFO_READ_MMR_REG => {
            // Для VI RADV читает только `GB_ADDR_CONFIG` (dword 0x263e); остальное — 0.
            let value = if request.word[0] == 0x263e { info.gb_addr_config } else { 0 };
            reply(&request, &value.to_le_bytes())
        }
        INFO_MEMORY => {
            let heap = |total: u64| DrmAmdgpuHeapInfo { total_heap_size: total, usable_heap_size: total, heap_usage: 0, max_allocation: total * 3 / 4 };
            let memory = DrmAmdgpuMemoryInfo {
                vram: heap(info.vram_bytes),
                cpu_accessible_vram: heap(info.vram_visible_bytes),
                gtt: heap(info.gtt_bytes),
            };
            reply(&request, bytes_of(&memory))
        }
        INFO_VRAM_GTT => reply(
            &request,
            bytes_of(&DrmAmdgpuInfoVramGtt {
                vram_size: info.vram_bytes,
                vram_cpu_accessible_size: info.vram_visible_bytes,
                gtt_size: info.gtt_bytes,
            }),
        ),
        INFO_TIMESTAMP => reply(&request, &(crate::time::uptime_ms().saturating_mul(1_000_000) / 10).to_le_bytes()),
        INFO_NUM_BYTES_MOVED | INFO_VRAM_USAGE | INFO_GTT_USAGE | INFO_VIS_VRAM_USAGE | INFO_NUM_EVICTIONS
        | INFO_NUM_VRAM_CPU_PAGE_FAULTS => reply(&request, &0u64.to_le_bytes()),
        INFO_VRAM_LOST_COUNTER => reply(&request, &0u32.to_le_bytes()),
        // Остальное (датчики, VBIOS, видео-возможности, MAX_IBS…) RADV терпит как ошибку.
        _ => -e::EINVAL,
    }
}

/// `DRM_IOCTL_VERSION`: имя «amdgpu», версия 3.59.0.
fn version(arg: usize) -> i64 {
    const NAME: &[u8] = b"amdgpu";
    const DATE: &[u8] = b"20150101";
    const DESC: &[u8] = b"AMD GPU";
    let Ok(mut v) = get::<DrmVersion>(arg) else { return -e::EFAULT };
    v.version_major = DRIVER_MAJOR as i32;
    v.version_minor = DRIVER_MINOR as i32;
    v.version_patchlevel = 0;
    for (len, ptr, text) in [(&mut v.name_len, v.name, NAME), (&mut v.date_len, v.date, DATE), (&mut v.desc_len, v.desc, DESC)] {
        let take = (*len as usize).min(text.len());
        if take != 0 && ptr != 0 && crate::linux::ipc::write_user(ptr as usize, &text[..take]).is_err() {
            return -e::EFAULT;
        }
        *len = text.len() as u64;
    }
    if put(arg, v).is_err() { -e::EFAULT } else { 0 }
}

/// `DRM_IOCTL_GET_CAP`: то же, что у узла NVIDIA (RADV просит syncobj и timeline).
fn get_cap(arg: usize) -> i64 {
    let Ok(mut cap) = get::<DrmGetCap>(arg) else { return -e::EFAULT };
    cap.value = match cap.capability {
        DRM_CAP_PRIME => DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT,
        DRM_CAP_TIMESTAMP_MONOTONIC | DRM_CAP_SYNCOBJ | DRM_CAP_SYNCOBJ_TIMELINE => 1,
        _ => return -e::EINVAL,
    };
    if put(arg, cap).is_err() { -e::EFAULT } else { 0 }
}

/// ioctl на открытом узле AMD (`request` уже приведён `canonical`).
pub fn ioctl(file: &AmdFile, request: u32, arg: usize) -> i64 {
    match request {
        VERSION => version(arg),
        GET_UNIQUE => {
            let Ok([_len, _ptr]) = get::<[u64; 2]>(arg) else { return -e::EFAULT };
            if put::<u64>(arg, 0).is_err() { -e::EFAULT } else { 0 }
        }
        GET_CAP => get_cap(arg),
        SET_CLIENT_CAP => {
            if get::<[u64; 2]>(arg).is_err() { -e::EFAULT } else { -EOPNOTSUPP }
        }
        GET_MAGIC | AUTH_MAGIC | SET_MASTER | DROP_MASTER => -e::EACCES,
        _ if (request >> 8) & 0xff != DRM_TYPE => -e::ENOTTY,
        // Драйверные ioctl: номер от `DRM_COMMAND_BASE`.
        _ if request & 0xff == DRM_COMMAND_BASE + DRM_AMDGPU_INFO => info_ioctl(file, arg),
        _ => -e::ENOTTY,
    }
}

/// Имя драйвера для строки состояния оболочки.
#[must_use]
pub fn status() -> String {
    alloc::format!("amdgpu node {}", if active() { "on (soft)" } else { "off" })
}
