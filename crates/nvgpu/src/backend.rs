// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Договор между слоем Linux (DRM nouveau, WP-C) и тем, кто на самом деле
//! двигает видеокарту: GSP-RM на RTX 3060 (WP-A, `nvidia_rm_chan.rs`) или
//! [`SoftGpu`](crate::softgpu::SoftGpu) в QEMU и в тестах на хосте (WP-B).
//!
//! Трейт [`GpuBackend`] — ровно раздел 3.5 спецификации `nvk-spec.md`; меняется
//! только с ведома ведущего и с сохранением совместимости. Всё, что трейт
//! называет (`GpuInfo`, `Place`, ручки, `Push`, `Errno`), определено здесь же.
//!
//! # Соглашения
//!
//! * Все размеры и адреса — байты; адреса VA, `bo_off` и длины кратны 4 КиБ
//!   (проверяет слой DRM, бэкенд отвечает `EINVAL` на невыровненное).
//! * `vm_map`/`vm_sparse`/`vm_unmap` только пишут таблицы страниц; TLB
//!   сбрасывает `vm_flush` (слой DRM зовёт его после каждой пачки VM_BIND).
//! * `chan_submit` возвращает номер забора (seq), который станет виден в
//!   `chan_seq_done`, когда GPU выполнит все переданные сегменты.
//! * Ручки — непрозрачные числа бэкенда, ноль допустим.

use alloc::string::String;
use alloc::vec::Vec;

/// Код ошибки Linux (положительный, как `errno`). Слой DRM возвращает
/// пользователю `-errno`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Errno(pub i32);

impl Errno {
    pub const EPERM: Errno = Errno(1);
    pub const ENOENT: Errno = Errno(2);
    pub const EINTR: Errno = Errno(4);
    pub const EIO: Errno = Errno(5);
    pub const E2BIG: Errno = Errno(7);
    pub const EBADF: Errno = Errno(9);
    pub const EAGAIN: Errno = Errno(11);
    pub const ENOMEM: Errno = Errno(12);
    pub const EFAULT: Errno = Errno(14);
    pub const EBUSY: Errno = Errno(16);
    pub const EEXIST: Errno = Errno(17);
    pub const ENODEV: Errno = Errno(19);
    pub const EINVAL: Errno = Errno(22);
    pub const ENOSPC: Errno = Errno(28);
    pub const ERANGE: Errno = Errno(34);
    pub const ENOSYS: Errno = Errno(38);
    pub const ENOTSUP: Errno = Errno(95);
    pub const ETIMEDOUT: Errno = Errno(110);
    pub const ECANCELED: Errno = Errno(125);
}

/// Где жить буферу.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    /// Видеопамять; процессору не видна (кроме служебной арены в окне BAR1).
    Vram,
    /// Обычные кадры памяти по 4 КиБ, когерентные (PTE SYS_COH + VOL).
    /// Всё, что пользователь отображает через mmap, живёт здесь.
    Sysmem,
}

/// Ручка буфера (BO) у бэкенда.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoHandle(pub u32);

/// Ручка адресного пространства GPU (одно на открытый файл DRM после VM_INIT).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VmHandle(pub u32);

/// Ручка канала (GPFIFO).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChanHandle(pub u32);

/// Ручка объекта класса движка в канале (3D, compute, copy, 2D, M2MF).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjHandle(pub u32);

/// Один сегмент пушбуфера из `drm_nouveau_exec_push`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Push {
    /// VA начала сегмента в пространстве канала, кратен 4.
    pub va: u64,
    /// Длина в байтах, кратна 4, меньше `1 << 23`.
    pub len: u32,
    /// [`Push::NO_PREFETCH`] и только он.
    pub flags: u32,
}

impl Push {
    /// `DRM_NOUVEAU_EXEC_PUSH_NO_PREFETCH`.
    pub const NO_PREFETCH: u32 = 1;
}

/// Номера классов (приложение B спецификации).
pub mod class {
    pub const NV01_ROOT: u32 = 0x0000;
    pub const NV01_DEVICE_0: u32 = 0x0080;
    pub const NV20_SUBDEVICE_0: u32 = 0x2080;
    pub const FERMI_VASPACE_A: u32 = 0x90f1;
    pub const AMPERE_CHANNEL_GPFIFO_A: u32 = 0xc56f;
    pub const AMPERE_USERMODE_A: u32 = 0xc561;
    pub const FERMI_TWOD_A: u32 = 0x902d;
    pub const KEPLER_INLINE_TO_MEMORY_B: u32 = 0xa140;
    pub const AMPERE_B: u32 = 0xc797;
    pub const AMPERE_COMPUTE_B: u32 = 0xc7c0;
    pub const AMPERE_DMA_COPY_A: u32 = 0xc6b5;
    pub const AMPERE_DMA_COPY_B: u32 = 0xc7b5;

    /// Ответ на NVIF SCLASS для канала GR у GA106 (раздел 1.3).
    pub const GA106_GR_CHANNEL: [u32; 6] = [
        FERMI_TWOD_A,
        KEPLER_INLINE_TO_MEMORY_B,
        AMPERE_B,
        AMPERE_COMPUTE_B,
        AMPERE_DMA_COPY_A,
        AMPERE_DMA_COPY_B,
    ];
}

/// Адрес устройства на шине PCI (`PCI_SLOT_NAME=dddd:bb:dd.f`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PciSlot {
    pub domain: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

/// Всё, что слою DRM нужно знать о карте: GETPARAM, NVIF `NV_DEVICE_V0_INFO`,
/// SCLASS и тексты sysfs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuInfo {
    /// 0x176 у GA106.
    pub chipset: u16,
    /// Короткое имя чипа для `nv_device_info_v0.chip` ("GA106").
    pub chip: String,
    /// Имя для `nv_device_info_v0.name` ("NVIDIA GeForce RTX 3060").
    pub name: String,
    pub pci_vendor: u16,
    pub pci_device: u16,
    pub pci_revision: u8,
    pub subsys_vendor: u16,
    pub subsys_device: u16,
    pub pci_slot: PciSlot,
    /// Видеопамять, доступная пользователю (GETPARAM FB_SIZE, `ram_user`).
    pub vram_bytes: u64,
    /// Размер окна BAR1 (GETPARAM VRAM_BAR_SIZE).
    pub bar_bytes: u64,
    /// Сколько системной памяти можно отдать под буферы (GETPARAM AGP_SIZE).
    pub gart_bytes: u64,
    pub gpcs: u32,
    pub tpcs: u32,
    /// Классы движков канала GR в порядке ответа SCLASS.
    pub classes: Vec<u32>,
}

impl GpuInfo {
    /// GETPARAM GRAPH_UNITS: `(tpcs << 8) | gpcs`.
    pub fn graph_units(&self) -> u64 {
        ((self.tpcs as u64) << 8) | (self.gpcs as u64 & 0xff)
    }

    /// RTX 3060 (GA106) с запасными значениями из спецификации: 3 GPC,
    /// 14 TPC (раздел 2.1), BAR1 256 МиБ. Реальный бэкенд подставляет то, что
    /// сказал RM; `SoftGpu` берёт это как есть.
    pub fn ga106(vram_bytes: u64, gart_bytes: u64) -> GpuInfo {
        GpuInfo {
            chipset: 0x176,
            chip: String::from("GA106"),
            name: String::from("NVIDIA GeForce RTX 3060"),
            pci_vendor: 0x10de,
            pci_device: 0x2503,
            pci_revision: 0xa1,
            subsys_vendor: 0x10de,
            subsys_device: 0x0000,
            pci_slot: PciSlot { domain: 0, bus: 1, dev: 0, func: 0 },
            vram_bytes,
            bar_bytes: 0x1000_0000,
            gart_bytes,
            gpcs: 3,
            tpcs: 14,
            classes: class::GA106_GR_CHANNEL.to_vec(),
        }
    }
}

/// Бэкенд GPU (раздел 3.5 спецификации). Реальный — `nvidia_rm_chan.rs`
/// (WP-A), программный — [`SoftGpu`](crate::softgpu::SoftGpu); слой DRM
/// видит только этот трейт.
pub trait GpuBackend: Send + Sync {
    /// Чипсет 0x176, PCI, видеопамять, BAR, GPC/TPC, имя, классы.
    fn info(&self) -> &GpuInfo;
    /// Выделить буфер. Видеопамять — непрерывная, системная — кадры по 4 КиБ,
    /// обнулённые.
    fn bo_alloc(&self, size: u64, place: Place) -> Result<BoHandle, Errno>;
    fn bo_free(&self, bo: BoHandle);
    /// Физические адреса кадров (для CPU, по 4 КиБ) — только у системного
    /// буфера; у видеопамяти `None`.
    fn bo_frames(&self, bo: BoHandle) -> Option<Vec<u64 /*cpu phys, 4K*/>>;
    /// client+device+vaspace+PD3+зарезервированный для RM диапазон.
    /// `kernel_va` = (`kernel_managed_addr`, `kernel_managed_size`) из VM_INIT.
    fn vm_create(&self, kernel_va: (u64, u64)) -> Result<VmHandle, Errno>;
    fn vm_destroy(&self, vm: VmHandle);
    /// Отобразить `[bo_off, bo_off+len)` буфера на `va` страницами по 4 КиБ;
    /// `kind` — PTE kind из флагов VM_BIND (проверяет [`crate::gmmu::pte_kind`]).
    fn vm_map(&self, vm: VmHandle, va: u64, bo: BoHandle, bo_off: u64, len: u64, kind: u8) -> Result<(), Errno>;
    /// Включить (`on`) или снять разреженность диапазона.
    fn vm_sparse(&self, vm: VmHandle, va: u64, len: u64, on: bool) -> Result<(), Errno>;
    /// Снять отображение; `restore_sparse` — диапазон внутри разреженной
    /// области, PTE возвращаются в «разреженное» состояние, а не в ноль.
    fn vm_unmap(&self, vm: VmHandle, va: u64, len: u64, restore_sparse: bool) -> Result<(), Errno>;
    /// Сбросить TLB после пачки изменений.
    fn vm_flush(&self, vm: VmHandle);
    /// inst/userd/ring/pushbuf/fence/канал RM/bind/schedule.
    fn chan_create(&self, vm: VmHandle) -> Result<ChanHandle, Errno>;
    fn chan_obj(&self, ch: ChanHandle, class: u32) -> Result<ObjHandle, Errno>;
    fn chan_obj_free(&self, ch: ChanHandle, o: ObjHandle);
    fn chan_destroy(&self, ch: ChanHandle);
    /// Поставить сегменты в GPFIFO, дописать сегмент ядра с выпуском забора и
    /// позвонить в дверной звонок. Возвращает номер забора.
    fn chan_submit(&self, ch: ChanHandle, pushes: &[Push]) -> Result<u64 /*fence seq*/, Errno>;
    /// Последний выполненный номер забора; `Err(ENODEV)`, если канал убит.
    fn chan_seq_done(&self, ch: ChanHandle) -> Result<u64, Errno>;
    /// PTIMER, наносекунды.
    fn time_ns(&self) -> u64;
    /// Сколько видеопамяти занято (GETPARAM VRAM_USED, должно быть > 0).
    fn vram_used(&self) -> u64;
}
