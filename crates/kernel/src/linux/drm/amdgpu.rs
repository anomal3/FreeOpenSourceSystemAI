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
//! * буферы: `GEM_CREATE`, `GEM_MMAP` (+ `mmap` по поддельному смещению),
//!   `GEM_CLOSE`, `GEM_METADATA`, `GEM_OP`, `GEM_WAIT_IDLE`; все буферы — кадры
//!   системной памяти (у программной карты нет отдельной видеопамяти);
//! * адресное пространство: `GEM_VA` (`MAP`/`UNMAP`/`REPLACE`/`CLEAR`) — учёт
//!   отображений с проверками как у `amdgpu_gem_va_ioctl`;
//! * контексты: `CTX` (`ALLOC`/`FREE`/`QUERY_STATE`/`QUERY_STATE2`, стабильное
//!   p-state).
//!
//! `CS`, заборы, syncobj и PRIME — следующие шаги плана.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nvgpu::uapi::{
    DRM_IOCTL_GEM_CLOSE as GEM_CLOSE, DRM_CAP_PRIME, DRM_CAP_SYNCOBJ, DRM_CAP_SYNCOBJ_TIMELINE, DRM_CAP_TIMESTAMP_MONOTONIC, DRM_IOCTL_AUTH_MAGIC as AUTH_MAGIC,
    DRM_IOCTL_BASE as DRM_TYPE, DRM_IOCTL_DROP_MASTER as DROP_MASTER, DRM_IOCTL_GET_CAP as GET_CAP,
    DRM_IOCTL_GET_MAGIC as GET_MAGIC, DRM_IOCTL_GET_UNIQUE as GET_UNIQUE, DRM_IOCTL_SET_CLIENT_CAP as SET_CLIENT_CAP,
    DRM_IOCTL_SET_MASTER as SET_MASTER, DRM_IOCTL_VERSION as VERSION, DRM_PRIME_CAP_EXPORT, DRM_PRIME_CAP_IMPORT, DrmGetCap,
    DrmVersion,
};

use super::{get, put, EOPNOTSUPP};
use crate::linux::e;
use crate::mm::{FrameAllocator, PhysAddr};
use crate::sync::{Mutex, SpinLock};
use crate::user::files::Object;
use crate::user::shm::SharedMem;

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
const DRM_AMDGPU_GEM_CREATE: u32 = 0x00;
const DRM_AMDGPU_GEM_MMAP: u32 = 0x01;
const DRM_AMDGPU_CTX: u32 = 0x02;
const DRM_AMDGPU_INFO: u32 = 0x05;
const DRM_AMDGPU_GEM_METADATA: u32 = 0x06;
const DRM_AMDGPU_GEM_WAIT_IDLE: u32 = 0x07;
const DRM_AMDGPU_GEM_VA: u32 = 0x08;
const DRM_AMDGPU_GEM_OP: u32 = 0x10;

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
    st: Mutex<AmdState>,
}

/// Состояние файла; порядок полей — порядок разбора при закрытии: сперва
/// отображения (держат ссылки на буферы), потом ручки.
struct AmdState {
    va: BTreeMap<u64, Mapping>,
    ctxs: BTreeMap<u32, Ctx>,
    next_ctx: u32,
    bos: BTreeMap<u32, Arc<AmdBo>>,
    next_bo: u32,
}

impl AmdState {
    fn new() -> Self {
        Self { va: BTreeMap::new(), ctxs: BTreeMap::new(), next_ctx: 1, bos: BTreeMap::new(), next_bo: 1 }
    }
}

impl Drop for AmdFile {
    fn drop(&mut self) {
        let st = self.st.lock();
        crate::kprintln!("amdgpu: drm close pid {} (gem {} ctx {} va {})", self.pid, st.bos.len(), st.ctxs.len(), st.va.len());
    }
}

/// Контекст `AMDGPU_CTX`.
struct Ctx {
    #[allow(dead_code)]
    priority: i32,
    stable_pstate: u32,
}

/// Отображение `GEM_VA`: буфер, смещение в нём, длина, флаги PTE.
struct Mapping {
    #[allow(dead_code)]
    bo: Arc<AmdBo>,
    #[allow(dead_code)]
    offset: u64,
    size: u64,
    #[allow(dead_code)]
    flags: u32,
}

const PAGE: u64 = 4096;
/// Нижний резерв VA (`AMDGPU_VA_RESERVED_BOTTOM`): UMD не отображает ниже.
const VA_RESERVED_BOTTOM: u64 = 1 << 16;

/// Следующее поддельное смещение `mmap` (как у Linux: с `0x1_0000_0000`).
static NEXT_MAP: AtomicU64 = AtomicU64::new(0x1_0000_0000);

/// Метаданные буфера (`GEM_METADATA`): тайлинг и непрозрачный блок UMD.
#[derive(Clone, Copy)]
struct BoMetadata {
    flags: u64,
    tiling_info: u64,
    size_bytes: u32,
    data: [u32; 64],
}

/// Буфер: кадры системной памяти.
struct AmdBo {
    size: u64,
    domains: u64,
    domain_flags: u64,
    map_handle: u64,
    frames: Vec<u64>,
    /// Память для `mmap`, пока её кто-то держит.
    mem: SpinLock<Weak<SharedMem>>,
    metadata: SpinLock<BoMetadata>,
}

impl Drop for AmdBo {
    fn drop(&mut self) {
        crate::mm::frame::with(|pool| {
            for &frame in &self.frames {
                // SAFETY: кадр выдан `AmdBo::new` и принадлежит только этому буферу;
                // отображения держат буфер живым (`keep` у `SharedMem`).
                unsafe { pool.free(PhysAddr(frame)) };
            }
        });
    }
}

impl AmdBo {
    fn new(size: u64, domains: u64, domain_flags: u64) -> Result<Arc<Self>, i64> {
        let pages = (size / PAGE) as usize;
        let mut frames: Vec<u64> = Vec::new();
        if frames.try_reserve_exact(pages).is_err() {
            return Err(-e::ENOMEM);
        }
        for _ in 0..pages {
            let frame = crate::mm::frame::with(|pool| {
                // Запас ядра не трогаем — как `SharedMem`.
                if pool.stats().free <= crate::user::RESERVE_FRAMES {
                    return None;
                }
                pool.allocate()
            })
            .flatten();
            let Some(frame) = frame else {
                crate::mm::frame::with(|pool| {
                    for &taken in &frames {
                        // SAFETY: кадры только что выданы этим вызовом.
                        unsafe { pool.free(PhysAddr(taken)) };
                    }
                });
                return Err(-e::ENOMEM);
            };
            // SAFETY: кадр только что выдан пулом и ничей; прямое отображение покрывает всю память.
            unsafe { core::ptr::write_bytes(frame.to_direct_map().as_mut_ptr::<u8>(), 0, PAGE as usize) };
            frames.push(frame.0);
        }
        // Смещения не пересекаются; между буферами — страница зазора.
        let map_handle = NEXT_MAP.fetch_add(size + PAGE, Ordering::Relaxed);
        Ok(Arc::new(Self {
            size,
            domains,
            domain_flags,
            map_handle,
            frames,
            mem: SpinLock::new(Weak::new()),
            metadata: SpinLock::new(BoMetadata { flags: 0, tiling_info: 0, size_bytes: 0, data: [0; 64] }),
        }))
    }

    /// Память для `mmap` (одна на буфер).
    fn shared(self: &Arc<Self>) -> Arc<SharedMem> {
        let mut slot = self.mem.lock();
        if let Some(mem) = slot.upgrade() {
            return mem;
        }
        let phys: Vec<PhysAddr> = self.frames.iter().map(|&addr| PhysAddr(addr)).collect();
        let keep: Arc<dyn Any + Send + Sync> = Arc::clone(self) as Arc<dyn Any + Send + Sync>;
        let mem = SharedMem::from_frames(&phys, self.size, keep);
        *slot = Arc::downgrade(&mem);
        mem
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
    let file: Arc<dyn Object> = Arc::new(AmdFile { info: AmdInfo::polaris10(), pid, st: Mutex::new(AmdState::new()) });
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

// ---- структуры ioctl буферов, адресов и контекстов (`amdgpu_drm.h`) -------------

/// `AMDGPU_GEM_DOMAIN_*`.
const DOMAIN_CPU: u64 = 0x1;
const DOMAIN_GTT: u64 = 0x2;
const DOMAIN_VRAM: u64 = 0x4;
const DOMAIN_MASK: u64 = DOMAIN_CPU | DOMAIN_GTT | DOMAIN_VRAM | 0x8 | 0x10 | 0x20 | 0x40;
/// Домены, которых у программной карты нет (GDS/GWS/OA/doorbell).
const DOMAIN_UNSUPPORTED: u64 = 0x8 | 0x10 | 0x20 | 0x40;

/// `union drm_amdgpu_gem_create`: вход — 4 × u64, выход — `handle` в первых 4 байтах.
#[repr(C)]
#[derive(Clone, Copy)]
struct GemCreate {
    bo_size: u64,
    alignment: u64,
    domains: u64,
    domain_flags: u64,
}
const _: () = assert!(core::mem::size_of::<GemCreate>() == 32);

/// `struct drm_amdgpu_gem_va`.
#[repr(C)]
#[derive(Clone, Copy)]
struct GemVa {
    handle: u32,
    pad: u32,
    operation: u32,
    flags: u32,
    va_address: u64,
    offset_in_bo: u64,
    map_size: u64,
}
const _: () = assert!(core::mem::size_of::<GemVa>() == 40);

const VA_OP_MAP: u32 = 1;
const VA_OP_UNMAP: u32 = 2;
const VA_OP_CLEAR: u32 = 3;
const VA_OP_REPLACE: u32 = 4;
/// `AMDGPU_VM_PAGE_{READABLE,WRITEABLE,EXECUTABLE,PRT}` и `DELAY_UPDATE`, `NOALLOC`.
const VM_FLAGS_KNOWN: u32 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 9);
const VM_PAGE_PRT: u32 = 1 << 4;

/// `struct drm_amdgpu_ctx_in` (вход) и `union drm_amdgpu_ctx_out` (выход) — 16 байт.
#[repr(C)]
#[derive(Clone, Copy)]
struct CtxArgs {
    op: u32,
    flags: u32,
    ctx_id: u32,
    priority: i32,
}
const _: () = assert!(core::mem::size_of::<CtxArgs>() == 16);

const CTX_OP_ALLOC: u32 = 1;
const CTX_OP_FREE: u32 = 2;
const CTX_OP_QUERY_STATE: u32 = 3;
const CTX_OP_QUERY_STATE2: u32 = 4;
const CTX_OP_GET_STABLE_PSTATE: u32 = 5;
const CTX_OP_SET_STABLE_PSTATE: u32 = 6;

/// `struct drm_amdgpu_gem_metadata` — 288 байт.
#[repr(C)]
#[derive(Clone, Copy)]
struct GemMetadata {
    handle: u32,
    op: u32,
    flags: u64,
    tiling_info: u64,
    data_size_bytes: u32,
    data: [u32; 64],
}
const _: () = assert!(core::mem::size_of::<GemMetadata>() == 288);

/// `struct drm_amdgpu_gem_op`.
#[repr(C)]
#[derive(Clone, Copy)]
struct GemOp {
    handle: u32,
    op: u32,
    value: u64,
}

/// `union drm_amdgpu_gem_wait_idle`: вход `{handle, flags, timeout}`, выход `{status, domain}`.
#[repr(C)]
#[derive(Clone, Copy)]
struct GemWaitIdle {
    handle: u32,
    flags: u32,
    timeout: u64,
}

impl AmdState {
    /// Новая ручка BO (ненулевая, не занятая).
    fn add_bo(&mut self, bo: Arc<AmdBo>) -> u32 {
        while self.next_bo == 0 || self.bos.contains_key(&self.next_bo) {
            self.next_bo = self.next_bo.wrapping_add(1);
        }
        let handle = self.next_bo;
        self.next_bo = self.next_bo.wrapping_add(1);
        self.bos.insert(handle, bo);
        handle
    }

    /// Отображения, пересекающие `[start, end)`: их начала.
    fn overlapping(&self, start: u64, end: u64) -> Vec<u64> {
        self.va.iter().filter(|&(&from, map)| from < end && from + map.size > start).map(|(&from, _)| from).collect()
    }
}

/// `DRM_IOCTL_AMDGPU_GEM_CREATE`.
fn gem_create(file: &AmdFile, arg: usize) -> i64 {
    let Ok(args) = get::<GemCreate>(arg) else { return -e::EFAULT };
    if args.bo_size == 0 || args.domains == 0 || args.domains & !DOMAIN_MASK != 0 {
        return -e::EINVAL;
    }
    // Как `amdgpu_gem_create_ioctl`: выравнивание — степень двойки, не меньше страницы.
    let alignment = args.alignment.max(PAGE).next_power_of_two();
    let Some(size) = args.bo_size.checked_next_multiple_of(alignment.max(PAGE)) else { return -e::EINVAL };
    if args.domains & DOMAIN_UNSUPPORTED != 0 {
        return -e::EINVAL;
    }
    if size > file.info.gtt_bytes.max(file.info.vram_bytes) {
        return -e::ENOMEM;
    }
    let bo = match AmdBo::new(size, args.domains, args.domain_flags) {
        Ok(bo) => bo,
        Err(err) => return err,
    };
    let handle = file.st.lock().add_bo(bo);
    let mut out = [0u8; 32];
    out[..4].copy_from_slice(&handle.to_le_bytes());
    if crate::linux::ipc::write_user(arg, &out).is_err() {
        file.st.lock().bos.remove(&handle);
        return -e::EFAULT;
    }
    0
}

/// `DRM_IOCTL_AMDGPU_GEM_MMAP`: поддельное смещение для `mmap` на файле.
fn gem_mmap(file: &AmdFile, arg: usize) -> i64 {
    let Ok([handle, _pad]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
    let Some(bo) = file.st.lock().bos.get(&handle).cloned() else { return -e::ENOENT };
    if put::<u64>(arg, bo.map_handle).is_err() { -e::EFAULT } else { 0 }
}

/// `mmap(drm_fd, offset)`: буфер файла, у которого `map_handle ≤ offset`.
pub fn mmap_file(file: &AmdFile, offset: u64, len: u64) -> Result<(Arc<SharedMem>, u64), i64> {
    let bo = file.st.lock().bos.values().find(|bo| offset >= bo.map_handle && offset < bo.map_handle + bo.size).cloned();
    let Some(bo) = bo else { return Err(-e::EINVAL) };
    let inside = offset - bo.map_handle;
    if inside % PAGE != 0 || inside.checked_add(len).is_none_or(|end| end > bo.size) {
        return Err(-e::EINVAL);
    }
    Ok((bo.shared(), inside))
}

/// `DRM_IOCTL_GEM_CLOSE`: уходит ручка; отображения в VA и память программы держат буфер.
fn gem_close(file: &AmdFile, arg: usize) -> i64 {
    let Ok([handle, _pad]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
    if file.st.lock().bos.remove(&handle).is_some() { 0 } else { -e::EINVAL }
}

/// `DRM_IOCTL_AMDGPU_GEM_VA`: учёт отображений, проверки как у Linux.
fn gem_va(file: &AmdFile, arg: usize) -> i64 {
    let Ok(args) = get::<GemVa>(arg) else { return -e::EFAULT };
    if args.pad != 0 || args.flags & !VM_FLAGS_KNOWN != 0 {
        return -e::EINVAL;
    }
    if !matches!(args.operation, VA_OP_MAP | VA_OP_UNMAP | VA_OP_CLEAR | VA_OP_REPLACE) {
        return -e::EINVAL;
    }
    // Верхняя граница — наш PD на 1 ГиБ (`AmdInfo::device_info`).
    let va_max = file.info.device_info().virtual_address_max;
    if args.va_address < VA_RESERVED_BOTTOM
        || args.va_address.checked_add(args.map_size).is_none_or(|end| end > va_max)
        || args.va_address % PAGE != 0
        || args.map_size % PAGE != 0
        || args.offset_in_bo % PAGE != 0
    {
        return -e::EINVAL;
    }
    let mut st = file.st.lock();
    let start = args.va_address;
    let end = start + args.map_size;
    match args.operation {
        VA_OP_MAP | VA_OP_REPLACE => {
            if args.map_size == 0 {
                return -e::EINVAL;
            }
            // PRT: отображение без буфера (handle 0) — разреженная область; пока нет.
            if args.flags & VM_PAGE_PRT != 0 && args.handle == 0 {
                return -e::EINVAL;
            }
            let Some(bo) = st.bos.get(&args.handle).cloned() else { return -e::ENOENT };
            if args.offset_in_bo.checked_add(args.map_size).is_none_or(|top| top > bo.size) {
                return -e::EINVAL;
            }
            let clash = st.overlapping(start, end);
            if args.operation == VA_OP_MAP {
                // `amdgpu_vm_bo_map`: пересечение с уже отображённым — EINVAL.
                if !clash.is_empty() {
                    return -e::EINVAL;
                }
            } else {
                // REPLACE: пересечённые отображения заменяются (целиком; обрезка краёв — позже).
                for from in clash {
                    st.va.remove(&from);
                }
            }
            st.va.insert(start, Mapping { bo, offset: args.offset_in_bo, size: args.map_size, flags: args.flags });
            0
        }
        VA_OP_UNMAP => match st.va.get(&start) {
            // Снимается ровно то отображение, что начинается здесь (как `amdgpu_vm_bo_unmap`).
            Some(map) if map.size == args.map_size || args.map_size == 0 => {
                st.va.remove(&start);
                0
            }
            _ => -e::EINVAL,
        },
        _ => {
            // CLEAR: снять всё, что пересекает диапазон.
            for from in st.overlapping(start, end) {
                st.va.remove(&from);
            }
            0
        }
    }
}

/// `DRM_IOCTL_AMDGPU_CTX`.
fn ctx_ioctl(file: &AmdFile, arg: usize) -> i64 {
    let Ok(args) = get::<CtxArgs>(arg) else { return -e::EFAULT };
    let mut st = file.st.lock();
    // Результат — в тех же 16 байтах (union `out`).
    let mut out = [0u8; 16];
    match args.op {
        CTX_OP_ALLOC => {
            // Приоритеты `AMDGPU_CTX_PRIORITY_*`: от -1023 до 1023.
            if !(-1023..=1023).contains(&args.priority) || args.flags != 0 {
                return -e::EINVAL;
            }
            while st.next_ctx == 0 || st.ctxs.contains_key(&st.next_ctx) {
                st.next_ctx = st.next_ctx.wrapping_add(1);
            }
            let id = st.next_ctx;
            st.next_ctx = st.next_ctx.wrapping_add(1);
            st.ctxs.insert(id, Ctx { priority: args.priority, stable_pstate: 0 });
            out[..4].copy_from_slice(&id.to_le_bytes());
        }
        CTX_OP_FREE => {
            if st.ctxs.remove(&args.ctx_id).is_none() {
                return -e::EINVAL;
            }
        }
        // `state.flags`, `state.hangs`, `state.reset_status` — нули: сбросов не было.
        CTX_OP_QUERY_STATE | CTX_OP_QUERY_STATE2 => {
            if !st.ctxs.contains_key(&args.ctx_id) {
                return -e::EINVAL;
            }
        }
        CTX_OP_GET_STABLE_PSTATE => {
            let Some(ctx) = st.ctxs.get(&args.ctx_id) else { return -e::EINVAL };
            out[..4].copy_from_slice(&ctx.stable_pstate.to_le_bytes());
        }
        CTX_OP_SET_STABLE_PSTATE => {
            let Some(ctx) = st.ctxs.get_mut(&args.ctx_id) else { return -e::EINVAL };
            ctx.stable_pstate = args.flags;
        }
        _ => return -e::EINVAL,
    }
    drop(st);
    if crate::linux::ipc::write_user(arg, &out).is_err() { -e::EFAULT } else { 0 }
}

/// `DRM_IOCTL_AMDGPU_GEM_METADATA`: `SET` запоминает, `GET` отдаёт.
fn gem_metadata(file: &AmdFile, arg: usize) -> i64 {
    const OP_SET: u32 = 1;
    const OP_GET: u32 = 2;
    let Ok(mut args) = get::<GemMetadata>(arg) else { return -e::EFAULT };
    let Some(bo) = file.st.lock().bos.get(&args.handle).cloned() else { return -e::ENOENT };
    match args.op {
        OP_SET => {
            // Как Linux: не больше 256 байт непрозрачных данных.
            if args.data_size_bytes as usize > core::mem::size_of_val(&args.data) {
                return -e::EINVAL;
            }
            *bo.metadata.lock() =
                BoMetadata { flags: args.flags, tiling_info: args.tiling_info, size_bytes: args.data_size_bytes, data: args.data };
            0
        }
        OP_GET => {
            let m = *bo.metadata.lock();
            args.flags = m.flags;
            args.tiling_info = m.tiling_info;
            args.data_size_bytes = m.size_bytes;
            args.data = m.data;
            if put(arg, args).is_err() { -e::EFAULT } else { 0 }
        }
        _ => -e::EINVAL,
    }
}

/// `DRM_IOCTL_AMDGPU_GEM_OP`: `GET_GEM_CREATE_INFO` (0) и `SET_PLACEMENT` (1).
fn gem_op(file: &AmdFile, arg: usize) -> i64 {
    let Ok(args) = get::<GemOp>(arg) else { return -e::EFAULT };
    let Some(bo) = file.st.lock().bos.get(&args.handle).cloned() else { return -e::ENOENT };
    match args.op {
        0 => {
            let info = GemCreate { bo_size: bo.size, alignment: PAGE, domains: bo.domains, domain_flags: bo.domain_flags };
            match crate::linux::ipc::write_user(args.value as usize, bytes_of(&info)) {
                Ok(()) => 0,
                Err(_) => -e::EFAULT,
            }
        }
        1 => {
            // Перенос между доменами: всё и так в системной памяти.
            if args.value == 0 || args.value & !DOMAIN_MASK != 0 || args.value & DOMAIN_UNSUPPORTED != 0 {
                return -e::EINVAL;
            }
            0
        }
        _ => -e::EINVAL,
    }
}

/// `DRM_IOCTL_AMDGPU_GEM_WAIT_IDLE`: работы на GPU нет — буфер всегда свободен (`status` 0).
fn gem_wait_idle(file: &AmdFile, arg: usize) -> i64 {
    let Ok(args) = get::<GemWaitIdle>(arg) else { return -e::EFAULT };
    let Some(bo) = file.st.lock().bos.get(&args.handle).cloned() else { return -e::ENOENT };
    let domain = if bo.domains & DOMAIN_VRAM != 0 { DOMAIN_VRAM } else { DOMAIN_GTT };
    let mut out = [0u8; 16];
    out[4..8].copy_from_slice(&(domain as u32).to_le_bytes());
    if crate::linux::ipc::write_user(arg, &out).is_err() { -e::EFAULT } else { 0 }
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
        GEM_CLOSE => gem_close(file, arg),
        _ if (request >> 8) & 0xff != DRM_TYPE => -e::ENOTTY,
        // Драйверные ioctl: номер от `DRM_COMMAND_BASE`.
        _ => match (request & 0xff).wrapping_sub(DRM_COMMAND_BASE) {
            DRM_AMDGPU_GEM_CREATE => gem_create(file, arg),
            DRM_AMDGPU_GEM_MMAP => gem_mmap(file, arg),
            DRM_AMDGPU_CTX => ctx_ioctl(file, arg),
            DRM_AMDGPU_INFO => info_ioctl(file, arg),
            DRM_AMDGPU_GEM_METADATA => gem_metadata(file, arg),
            DRM_AMDGPU_GEM_WAIT_IDLE => gem_wait_idle(file, arg),
            DRM_AMDGPU_GEM_VA => gem_va(file, arg),
            DRM_AMDGPU_GEM_OP => gem_op(file, arg),
            _ => -e::ENOTTY,
        },
    }
}

/// Имя драйвера для строки состояния оболочки.
#[must_use]
pub fn status() -> String {
    alloc::format!("amdgpu node {}", if active() { "on (soft)" } else { "off" })
}
