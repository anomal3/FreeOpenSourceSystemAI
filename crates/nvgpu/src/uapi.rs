// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Двоичный интерфейс: структуры `drm.h`, `nouveau_drm.h` (Linux 6.12) и
//! кусочка NVIF, номера ioctl и их разбор.
//!
//! Раскладки сверены с заголовками, собранными gcc (раздел 3.4
//! спецификации): размеры и смещения проверяются при компиляции ниже, так что
//! разойтись с ядром Linux молча структура не может. Все структуры — только из
//! целых чисел без дыр-ловушек, их можно читать из байтов пользователя как есть
//! ([`Pod`]).

use core::mem::{offset_of, size_of};

// ---------------------------------------------------------------------------
// Номера ioctl

/// Тип ioctl у DRM (`'d'`).
pub const DRM_IOCTL_BASE: u32 = 0x64;
/// Первый номер команд драйвера (`DRM_COMMAND_BASE`).
pub const DRM_COMMAND_BASE: u32 = 0x40;

pub const IOC_NONE: u32 = 0;
pub const IOC_WRITE: u32 = 1;
pub const IOC_READ: u32 = 2;

/// `_IOC(dir, 'd', nr, size)` для x86-64 и aarch64.
pub const fn ioc(dir: u32, nr: u32, size: usize) -> u32 {
    (dir << 30) | ((size as u32 & 0x3fff) << 16) | (DRM_IOCTL_BASE << 8) | nr
}
pub const fn iow(nr: u32, size: usize) -> u32 {
    ioc(IOC_WRITE, nr, size)
}
pub const fn ior(nr: u32, size: usize) -> u32 {
    ioc(IOC_READ, nr, size)
}
pub const fn iowr(nr: u32, size: usize) -> u32 {
    ioc(IOC_READ | IOC_WRITE, nr, size)
}

/// Разобранный номер ioctl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ioctl {
    pub dir: u32,
    pub ty: u32,
    pub nr: u32,
    pub size: usize,
}

impl Ioctl {
    pub const fn decode(request: u32) -> Ioctl {
        Ioctl {
            dir: request >> 30,
            ty: (request >> 8) & 0xff,
            nr: request & 0xff,
            size: ((request >> 16) & 0x3fff) as usize,
        }
    }

    /// Это ioctl DRM (`'d'`)?
    pub const fn is_drm(&self) -> bool {
        self.ty == DRM_IOCTL_BASE
    }

    /// Команда драйвера nouveau (`nr >= 0x40`): номер без базы.
    pub const fn driver_nr(&self) -> Option<u32> {
        if self.is_drm() && self.nr >= DRM_COMMAND_BASE && self.nr < 0xa0 {
            Some(self.nr - DRM_COMMAND_BASE)
        } else {
            None
        }
    }
}

// Ядро DRM.
pub const DRM_IOCTL_VERSION: u32 = iowr(0x00, size_of::<DrmVersion>());
pub const DRM_IOCTL_GET_UNIQUE: u32 = iowr(0x01, size_of::<DrmUnique>());
pub const DRM_IOCTL_GET_MAGIC: u32 = ior(0x02, 4);
pub const DRM_IOCTL_GEM_CLOSE: u32 = iow(0x09, size_of::<DrmGemClose>());
pub const DRM_IOCTL_GET_CAP: u32 = iowr(0x0c, size_of::<DrmGetCap>());
pub const DRM_IOCTL_SET_CLIENT_CAP: u32 = iow(0x0d, size_of::<DrmSetClientCap>());
pub const DRM_IOCTL_AUTH_MAGIC: u32 = iow(0x11, 4);
pub const DRM_IOCTL_SET_MASTER: u32 = ioc(IOC_NONE, 0x1e, 0);
pub const DRM_IOCTL_DROP_MASTER: u32 = ioc(IOC_NONE, 0x1f, 0);
pub const DRM_IOCTL_PRIME_HANDLE_TO_FD: u32 = iowr(0x2d, size_of::<DrmPrimeHandle>());
pub const DRM_IOCTL_PRIME_FD_TO_HANDLE: u32 = iowr(0x2e, size_of::<DrmPrimeHandle>());
pub const DRM_IOCTL_SYNCOBJ_CREATE: u32 = iowr(0xbf, size_of::<DrmSyncobjCreate>());
pub const DRM_IOCTL_SYNCOBJ_DESTROY: u32 = iowr(0xc0, size_of::<DrmSyncobjDestroy>());
pub const DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD: u32 = iowr(0xc1, size_of::<DrmSyncobjHandle>());
pub const DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE: u32 = iowr(0xc2, size_of::<DrmSyncobjHandle>());
pub const DRM_IOCTL_SYNCOBJ_WAIT: u32 = iowr(0xc3, size_of::<DrmSyncobjWait>());
pub const DRM_IOCTL_SYNCOBJ_RESET: u32 = iowr(0xc4, size_of::<DrmSyncobjArray>());
pub const DRM_IOCTL_SYNCOBJ_SIGNAL: u32 = iowr(0xc5, size_of::<DrmSyncobjArray>());
pub const DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT: u32 = iowr(0xca, size_of::<DrmSyncobjTimelineWait>());
pub const DRM_IOCTL_SYNCOBJ_QUERY: u32 = iowr(0xcb, size_of::<DrmSyncobjTimelineArray>());
pub const DRM_IOCTL_SYNCOBJ_TRANSFER: u32 = iowr(0xcc, size_of::<DrmSyncobjTransfer>());
pub const DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL: u32 = iowr(0xcd, size_of::<DrmSyncobjTimelineArray>());
pub const DRM_IOCTL_SYNCOBJ_EVENTFD: u32 = iowr(0xcf, size_of::<DrmSyncobjEventfd>());

// nouveau: номера команд (без DRM_COMMAND_BASE).
pub const DRM_NOUVEAU_GETPARAM: u32 = 0x00;
pub const DRM_NOUVEAU_CHANNEL_ALLOC: u32 = 0x02;
pub const DRM_NOUVEAU_CHANNEL_FREE: u32 = 0x03;
/// NVIF: направление и размер у libdrm переменные (`drmCommandWrite`), сверять
/// только номер.
pub const DRM_NOUVEAU_NVIF: u32 = 0x07;
pub const DRM_NOUVEAU_VM_INIT: u32 = 0x10;
pub const DRM_NOUVEAU_VM_BIND: u32 = 0x11;
pub const DRM_NOUVEAU_EXEC: u32 = 0x12;
pub const DRM_NOUVEAU_GEM_NEW: u32 = 0x40;
pub const DRM_NOUVEAU_GEM_PUSHBUF: u32 = 0x41;
pub const DRM_NOUVEAU_GEM_CPU_PREP: u32 = 0x42;
pub const DRM_NOUVEAU_GEM_CPU_FINI: u32 = 0x43;
pub const DRM_NOUVEAU_GEM_INFO: u32 = 0x44;

const fn nv(nr: u32) -> u32 {
    DRM_COMMAND_BASE + nr
}
pub const DRM_IOCTL_NOUVEAU_GETPARAM: u32 = iowr(nv(DRM_NOUVEAU_GETPARAM), size_of::<NouveauGetparam>());
pub const DRM_IOCTL_NOUVEAU_CHANNEL_ALLOC: u32 = iowr(nv(DRM_NOUVEAU_CHANNEL_ALLOC), size_of::<NouveauChannelAlloc>());
pub const DRM_IOCTL_NOUVEAU_CHANNEL_FREE: u32 = iow(nv(DRM_NOUVEAU_CHANNEL_FREE), size_of::<NouveauChannelFree>());
pub const DRM_IOCTL_NOUVEAU_VM_INIT: u32 = iowr(nv(DRM_NOUVEAU_VM_INIT), size_of::<NouveauVmInit>());
pub const DRM_IOCTL_NOUVEAU_VM_BIND: u32 = iowr(nv(DRM_NOUVEAU_VM_BIND), size_of::<NouveauVmBind>());
pub const DRM_IOCTL_NOUVEAU_EXEC: u32 = iowr(nv(DRM_NOUVEAU_EXEC), size_of::<NouveauExec>());
pub const DRM_IOCTL_NOUVEAU_GEM_NEW: u32 = iowr(nv(DRM_NOUVEAU_GEM_NEW), size_of::<NouveauGemNew>());
pub const DRM_IOCTL_NOUVEAU_GEM_PUSHBUF: u32 = iowr(nv(DRM_NOUVEAU_GEM_PUSHBUF), size_of::<NouveauGemPushbuf>());
pub const DRM_IOCTL_NOUVEAU_GEM_CPU_PREP: u32 = iow(nv(DRM_NOUVEAU_GEM_CPU_PREP), size_of::<NouveauGemCpuPrep>());
pub const DRM_IOCTL_NOUVEAU_GEM_CPU_FINI: u32 = iow(nv(DRM_NOUVEAU_GEM_CPU_FINI), size_of::<NouveauGemCpuFini>());
pub const DRM_IOCTL_NOUVEAU_GEM_INFO: u32 = iowr(nv(DRM_NOUVEAU_GEM_INFO), size_of::<NouveauGemInfo>());

// Числа из раздела 3.4 (собраны gcc из настоящих заголовков) — сверка.
const _: () = {
    assert!(DRM_IOCTL_VERSION == 0xC0406400);
    assert!(DRM_IOCTL_GET_CAP == 0xC010640C);
    assert!(DRM_IOCTL_SET_CLIENT_CAP == 0x4010640D);
    assert!(DRM_IOCTL_GEM_CLOSE == 0x40086409);
    assert!(DRM_IOCTL_GET_UNIQUE == 0xC0106401);
    assert!(DRM_IOCTL_GET_MAGIC == 0x80046402);
    assert!(DRM_IOCTL_AUTH_MAGIC == 0x40046411);
    assert!(DRM_IOCTL_PRIME_HANDLE_TO_FD == 0xC00C642D);
    assert!(DRM_IOCTL_PRIME_FD_TO_HANDLE == 0xC00C642E);
    assert!(DRM_IOCTL_SET_MASTER == 0x641E);
    assert!(DRM_IOCTL_DROP_MASTER == 0x641F);
    assert!(DRM_IOCTL_SYNCOBJ_CREATE == 0xC00864BF);
    assert!(DRM_IOCTL_SYNCOBJ_DESTROY == 0xC00864C0);
    assert!(DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD == 0xC01064C1);
    assert!(DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE == 0xC01064C2);
    assert!(DRM_IOCTL_SYNCOBJ_WAIT == 0xC02864C3);
    assert!(DRM_IOCTL_SYNCOBJ_RESET == 0xC01064C4);
    assert!(DRM_IOCTL_SYNCOBJ_SIGNAL == 0xC01064C5);
    assert!(DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT == 0xC03064CA);
    assert!(DRM_IOCTL_SYNCOBJ_QUERY == 0xC01864CB);
    assert!(DRM_IOCTL_SYNCOBJ_TRANSFER == 0xC02064CC);
    assert!(DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL == 0xC01864CD);
    assert!(DRM_IOCTL_SYNCOBJ_EVENTFD == 0xC01864CF);
    assert!(DRM_IOCTL_NOUVEAU_GETPARAM == 0xC0106440);
    assert!(DRM_IOCTL_NOUVEAU_CHANNEL_ALLOC == 0xC0586442);
    assert!(DRM_IOCTL_NOUVEAU_CHANNEL_FREE == 0x40046443);
    assert!(DRM_IOCTL_NOUVEAU_VM_INIT == 0xC0106450);
    assert!(DRM_IOCTL_NOUVEAU_VM_BIND == 0xC0286451);
    assert!(DRM_IOCTL_NOUVEAU_EXEC == 0xC0286452);
    assert!(DRM_IOCTL_NOUVEAU_GEM_NEW == 0xC0306480);
    assert!(DRM_IOCTL_NOUVEAU_GEM_PUSHBUF == 0xC0406481);
    assert!(DRM_IOCTL_NOUVEAU_GEM_CPU_PREP == 0x40086482);
    assert!(DRM_IOCTL_NOUVEAU_GEM_CPU_FINI == 0x40046483);
    assert!(DRM_IOCTL_NOUVEAU_GEM_INFO == 0xC0286484);
};

// ---------------------------------------------------------------------------
// Константы

pub const DRM_CAP_DUMB_BUFFER: u64 = 0x1;
pub const DRM_CAP_PRIME: u64 = 0x5;
pub const DRM_PRIME_CAP_IMPORT: u64 = 0x1;
pub const DRM_PRIME_CAP_EXPORT: u64 = 0x2;
pub const DRM_CAP_TIMESTAMP_MONOTONIC: u64 = 0x6;
pub const DRM_CAP_SYNCOBJ: u64 = 0x13;
pub const DRM_CAP_SYNCOBJ_TIMELINE: u64 = 0x14;

pub const DRM_SYNCOBJ_CREATE_SIGNALED: u32 = 1 << 0;
pub const DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE: u32 = 1 << 0;
pub const DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE: u32 = 1 << 0;
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL: u32 = 1 << 0;
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT: u32 = 1 << 1;
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE: u32 = 1 << 2;
pub const DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE: u32 = 1 << 3;
pub const DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED: u32 = 1 << 0;

pub const NOUVEAU_GETPARAM_PCI_VENDOR: u64 = 3;
pub const NOUVEAU_GETPARAM_PCI_DEVICE: u64 = 4;
pub const NOUVEAU_GETPARAM_BUS_TYPE: u64 = 5;
pub const NOUVEAU_GETPARAM_FB_SIZE: u64 = 8;
pub const NOUVEAU_GETPARAM_AGP_SIZE: u64 = 9;
pub const NOUVEAU_GETPARAM_CHIPSET_ID: u64 = 11;
pub const NOUVEAU_GETPARAM_VM_VRAM_BASE: u64 = 12;
pub const NOUVEAU_GETPARAM_GRAPH_UNITS: u64 = 13;
pub const NOUVEAU_GETPARAM_PTIMER_TIME: u64 = 14;
pub const NOUVEAU_GETPARAM_HAS_BO_USAGE: u64 = 15;
pub const NOUVEAU_GETPARAM_HAS_PAGEFLIP: u64 = 16;
pub const NOUVEAU_GETPARAM_EXEC_PUSH_MAX: u64 = 17;
pub const NOUVEAU_GETPARAM_VRAM_BAR_SIZE: u64 = 18;
pub const NOUVEAU_GETPARAM_VRAM_USED: u64 = 19;
pub const NOUVEAU_GETPARAM_HAS_VMA_TILEMODE: u64 = 20;

/// GETPARAM BUS_TYPE: PCI Express.
pub const NV_BUS_TYPE_PCIE: u64 = 2;
/// GETPARAM EXEC_PUSH_MAX у nouveau (`ib_max 1023 / 2 - 1`).
pub const NOUVEAU_EXEC_PUSH_MAX: u32 = 510;

pub const NOUVEAU_GEM_DOMAIN_CPU: u32 = 1 << 0;
pub const NOUVEAU_GEM_DOMAIN_VRAM: u32 = 1 << 1;
pub const NOUVEAU_GEM_DOMAIN_GART: u32 = 1 << 2;
pub const NOUVEAU_GEM_DOMAIN_MAPPABLE: u32 = 1 << 3;
pub const NOUVEAU_GEM_DOMAIN_COHERENT: u32 = 1 << 4;
pub const NOUVEAU_GEM_DOMAIN_NO_SHARE: u32 = 1 << 5;

pub const NOUVEAU_GEM_CPU_PREP_NOWAIT: u32 = 0x1;
pub const NOUVEAU_GEM_CPU_PREP_WRITE: u32 = 0x4;

pub const DRM_NOUVEAU_SYNC_SYNCOBJ: u32 = 0x0;
pub const DRM_NOUVEAU_SYNC_TIMELINE_SYNCOBJ: u32 = 0x1;
pub const DRM_NOUVEAU_SYNC_TYPE_MASK: u32 = 0xf;

pub const DRM_NOUVEAU_VM_BIND_OP_MAP: u32 = 0x0;
pub const DRM_NOUVEAU_VM_BIND_OP_UNMAP: u32 = 0x1;
pub const DRM_NOUVEAU_VM_BIND_SPARSE: u32 = 1 << 8;
pub const DRM_NOUVEAU_VM_BIND_RUN_ASYNC: u32 = 0x1;
/// Сколько операций в одном VM_BIND принимает nouveau.
pub const NOUVEAU_VM_BIND_MAX_OPS: u32 = 4096;

pub const DRM_NOUVEAU_EXEC_PUSH_NO_PREFETCH: u32 = 0x1;
/// Предел длины сегмента EXEC (`va_len < 1 << 23`).
pub const NOUVEAU_EXEC_PUSH_MAX_LEN: u32 = 1 << 23;

// NVIF (Linux `nvif/ioctl.h`, `nvif/cl0080.h`; [I] — в локальных копиях
// заголовков этих определений нет, числа общеизвестные и сходятся со
// спецификацией: NEW = 2, MTHD = 4, PCIE = 3).
pub const NVIF_IOCTL_V0_NOP: u8 = 0x00;
pub const NVIF_IOCTL_V0_SCLASS: u8 = 0x01;
pub const NVIF_IOCTL_V0_NEW: u8 = 0x02;
pub const NVIF_IOCTL_V0_DEL: u8 = 0x03;
pub const NVIF_IOCTL_V0_MTHD: u8 = 0x04;
pub const NVIF_IOCTL_V0_OWNER_NVIF: u8 = 0x00;
pub const NVIF_IOCTL_V0_OWNER_ANY: u8 = 0xff;
pub const NVIF_IOCTL_V0_ROUTE_NVIF: u8 = 0x00;
pub const NVIF_IOCTL_V0_ROUTE_HIDDEN: u8 = 0xff;
/// Класс NVIF «устройство».
pub const NV_DEVICE: i32 = 0x0080;
/// Метод `NV_DEVICE_V0_INFO`.
pub const NV_DEVICE_V0_INFO: u8 = 0x00;
pub const NV_DEVICE_INFO_V0_PCIE: u8 = 0x03;
pub const NV_DEVICE_INFO_V0_AMPERE: u8 = 0x0d;

// ---------------------------------------------------------------------------
// Байты <-> структуры

/// Структура из одних целых: любые байты — допустимое значение, дыр нет.
///
/// # Safety
/// Реализовывать только для `repr(C)` без полей-ссылок и без внутренних дыр
/// (размер = сумма полей; это проверяют утверждения ниже).
pub unsafe trait Pod: Copy + Sized {
    /// Прочитать из начала `bytes`; `None`, если байтов меньше размера.
    fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < size_of::<Self>() {
            return None;
        }
        // SAFETY: длина проверена, тип Pod — любой набор байтов допустим.
        Some(unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const Self) })
    }

    /// Записать в начало `bytes`; `false`, если места мало.
    fn write(&self, bytes: &mut [u8]) -> bool {
        if bytes.len() < size_of::<Self>() {
            return false;
        }
        bytes[..size_of::<Self>()].copy_from_slice(self.as_bytes());
        true
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: Pod без дыр, все байты инициализированы.
        unsafe { core::slice::from_raw_parts(self as *const Self as *const u8, size_of::<Self>()) }
    }

    fn zeroed() -> Self {
        // SAFETY: ноль — допустимое значение любого Pod.
        unsafe { core::mem::zeroed() }
    }
}

macro_rules! pod {
    ($($t:ty = $size:expr),* $(,)?) => {
        $(
            // SAFETY: repr(C), только целые, размер сверен ниже.
            unsafe impl Pod for $t {}
            const _: () = assert!(size_of::<$t>() == $size);
        )*
    };
}

// ---------------------------------------------------------------------------
// Ядро DRM

/// `struct drm_version` (64 Б на 64-битных). Двухшаговый протокол: первый
/// вызов с нулевыми буферами узнаёт длины, второй копирует строки.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmVersion {
    pub version_major: i32,
    pub version_minor: i32,
    pub version_patchlevel: i32,
    pub _pad: u32,
    pub name_len: u64,
    pub name: u64,
    pub date_len: u64,
    pub date: u64,
    pub desc_len: u64,
    pub desc: u64,
}

/// Что отвечаем на VERSION (раздел 1.1).
pub const NOUVEAU_DRIVER_NAME: &str = "nouveau";
pub const NOUVEAU_DRIVER_DATE: &str = "20120801";
pub const NOUVEAU_DRIVER_DESC: &str = "nVidia Riva/TNT/GeForce/Quadro/Tesla/Tegra K1+";
pub const NOUVEAU_DRIVER_VERSION: (i32, i32, i32) = (1, 4, 0);

/// `struct drm_unique`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmUnique {
    pub unique_len: u64,
    pub unique: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmGemClose {
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmGetCap {
    pub capability: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSetClientCap {
    pub capability: u64,
    pub value: u64,
}

/// `struct drm_prime_handle` (12 Б); `flags` = `DRM_CLOEXEC | DRM_RDWR`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmPrimeHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjCreate {
    pub handle: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjDestroy {
    pub handle: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjHandle {
    pub handle: u32,
    pub flags: u32,
    pub fd: i32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjWait {
    pub handles: u64,
    /// Абсолютное время, CLOCK_MONOTONIC, нс.
    pub timeout_nsec: i64,
    pub count_handles: u32,
    pub flags: u32,
    pub first_signaled: u32,
    pub pad: u32,
    pub deadline_nsec: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjTimelineWait {
    pub handles: u64,
    pub points: u64,
    pub timeout_nsec: i64,
    pub count_handles: u32,
    pub flags: u32,
    pub first_signaled: u32,
    pub pad: u32,
    pub deadline_nsec: u64,
}

/// RESET / SIGNAL.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjArray {
    pub handles: u64,
    pub count_handles: u32,
    pub pad: u32,
}

/// QUERY / TIMELINE_SIGNAL.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjTimelineArray {
    pub handles: u64,
    pub points: u64,
    pub count_handles: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjTransfer {
    pub src_handle: u32,
    pub dst_handle: u32,
    pub src_point: u64,
    pub dst_point: u64,
    pub flags: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrmSyncobjEventfd {
    pub handle: u32,
    pub flags: u32,
    pub point: u64,
    pub fd: i32,
    pub pad: u32,
}

// ---------------------------------------------------------------------------
// nouveau

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauGetparam {
    pub param: u64,
    pub value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauSubchan {
    pub handle: u32,
    pub grclass: u32,
}

/// `struct drm_nouveau_channel_alloc` (88 Б). NVK присылает нули и читает
/// только `channel`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauChannelAlloc {
    pub fb_ctxdma_handle: u32,
    pub tt_ctxdma_handle: u32,
    pub channel: i32,
    pub pushbuf_domains: u32,
    pub notifier_handle: u32,
    pub subchan: [NouveauSubchan; 8],
    pub nr_subchan: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauChannelFree {
    pub channel: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauGemInfo {
    pub handle: u32,
    pub domain: u32,
    pub size: u64,
    /// GPU-адрес; в режиме uvmm всегда 0.
    pub offset: u64,
    /// Смещение для mmap() на файле DRM.
    pub map_handle: u64,
    pub tile_mode: u32,
    /// `pte_kind << 8` (в режиме uvmm только проверяется).
    pub tile_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauGemNew {
    pub info: NouveauGemInfo,
    pub channel_hint: u32,
    pub align: u32,
}

/// `struct drm_nouveau_gem_pushbuf` (64 Б) — в режиме uvmm nouveau отвечает
/// `-ENOSYS`; структура нужна только для размера.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauGemPushbuf {
    pub channel: u32,
    pub nr_buffers: u32,
    pub buffers: u64,
    pub nr_relocs: u32,
    pub nr_push: u32,
    pub relocs: u64,
    pub push: u64,
    pub suffix0: u32,
    pub suffix1: u32,
    pub vram_available: u64,
    pub gart_available: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauGemCpuPrep {
    pub handle: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauGemCpuFini {
    pub handle: u32,
}

/// `struct drm_nouveau_sync`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauSync {
    pub flags: u32,
    pub handle: u32,
    pub timeline_value: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauVmInit {
    pub kernel_managed_addr: u64,
    pub kernel_managed_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauVmBindOp {
    pub op: u32,
    /// Младшие 8 бит — PTE kind, бит 8 — SPARSE.
    pub flags: u32,
    pub handle: u32,
    pub pad: u32,
    pub addr: u64,
    pub bo_offset: u64,
    pub range: u64,
}

impl NouveauVmBindOp {
    pub fn kind(&self) -> u8 {
        self.flags as u8
    }
    pub fn sparse(&self) -> bool {
        self.flags & DRM_NOUVEAU_VM_BIND_SPARSE != 0
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauVmBind {
    pub op_count: u32,
    pub flags: u32,
    pub wait_count: u32,
    pub sig_count: u32,
    pub wait_ptr: u64,
    pub sig_ptr: u64,
    pub op_ptr: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauExecPush {
    pub va: u64,
    pub va_len: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NouveauExec {
    pub channel: u32,
    pub push_count: u32,
    pub wait_count: u32,
    pub sig_count: u32,
    pub wait_ptr: u64,
    pub sig_ptr: u64,
    pub push_ptr: u64,
}

// ---------------------------------------------------------------------------
// NVIF

/// Заголовок любого вызова NVIF (24 Б), дальше — тело по `ty`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvifIoctlV0 {
    pub version: u8,
    pub ty: u8,
    pub pad02: [u8; 4],
    pub owner: u8,
    pub route: u8,
    pub token: u64,
    pub object: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvifIoctlSclassV0 {
    pub version: u8,
    /// На входе — сколько мест под классы, на выходе — сколько классов есть.
    pub count: u8,
    pub pad02: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvifIoctlSclassOclassV0 {
    pub oclass: i32,
    pub minver: i16,
    pub maxver: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvifIoctlNewV0 {
    pub version: u8,
    pub pad01: [u8; 6],
    pub route: u8,
    pub token: u64,
    /// Печенье пользователя: по нему потом приходят MTHD/DEL.
    pub object: u64,
    pub handle: u32,
    pub oclass: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvifIoctlDel {
    pub version: u8,
    pub pad01: [u8; 7],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvifIoctlMthdV0 {
    pub version: u8,
    pub method: u8,
    pub pad02: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvDeviceV0 {
    pub version: u8,
    pub pad01: [u8; 7],
    pub device: u64,
}

/// Ответ `NV_DEVICE_V0_INFO` (104 Б).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NvDeviceInfoV0 {
    pub version: u8,
    pub platform: u8,
    pub chipset: u16,
    pub revision: u8,
    pub family: u8,
    pub pad06: [u8; 2],
    pub ram_size: u64,
    pub ram_user: u64,
    pub chip: [u8; 16],
    pub name: [u8; 64],
}

impl Default for NvDeviceInfoV0 {
    fn default() -> Self {
        Pod::zeroed()
    }
}

impl NvDeviceInfoV0 {
    /// Заполнить по [`GpuInfo`](crate::backend::GpuInfo) (строки обрезаются
    /// и всегда кончаются нулём).
    pub fn from_info(info: &crate::backend::GpuInfo) -> NvDeviceInfoV0 {
        let mut r: NvDeviceInfoV0 = Pod::zeroed();
        r.platform = NV_DEVICE_INFO_V0_PCIE;
        r.chipset = info.chipset;
        r.revision = info.pci_revision;
        r.family = NV_DEVICE_INFO_V0_AMPERE;
        r.ram_size = info.vram_bytes;
        r.ram_user = info.vram_bytes;
        copy_cstr(&mut r.chip, info.chip.as_bytes());
        copy_cstr(&mut r.name, info.name.as_bytes());
        r
    }
}

fn copy_cstr(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    for b in &mut dst[n..] {
        *b = 0;
    }
}

pod! {
    DrmVersion = 64,
    DrmUnique = 16,
    DrmGemClose = 8,
    DrmGetCap = 16,
    DrmSetClientCap = 16,
    DrmPrimeHandle = 12,
    DrmSyncobjCreate = 8,
    DrmSyncobjDestroy = 8,
    DrmSyncobjHandle = 16,
    DrmSyncobjWait = 40,
    DrmSyncobjTimelineWait = 48,
    DrmSyncobjArray = 16,
    DrmSyncobjTimelineArray = 24,
    DrmSyncobjTransfer = 32,
    DrmSyncobjEventfd = 24,
    NouveauGetparam = 16,
    NouveauSubchan = 8,
    NouveauChannelAlloc = 88,
    NouveauChannelFree = 4,
    NouveauGemInfo = 40,
    NouveauGemNew = 48,
    NouveauGemPushbuf = 64,
    NouveauGemCpuPrep = 8,
    NouveauGemCpuFini = 4,
    NouveauSync = 16,
    NouveauVmInit = 16,
    NouveauVmBindOp = 40,
    NouveauVmBind = 40,
    NouveauExecPush = 16,
    NouveauExec = 40,
    NvifIoctlV0 = 24,
    NvifIoctlSclassV0 = 8,
    NvifIoctlSclassOclassV0 = 8,
    NvifIoctlNewV0 = 32,
    NvifIoctlDel = 8,
    NvifIoctlMthdV0 = 8,
    NvDeviceV0 = 16,
    NvDeviceInfoV0 = 104,
}

// Смещения, на которые опирается NVK (разделы 1.2 и 3.4).
const _: () = {
    assert!(offset_of!(DrmVersion, name_len) == 16);
    assert!(offset_of!(DrmVersion, desc) == 56);
    assert!(offset_of!(NouveauChannelAlloc, channel) == 8);
    assert!(offset_of!(NouveauChannelAlloc, nr_subchan) == 84);
    assert!(offset_of!(NouveauGemInfo, map_handle) == 24);
    assert!(offset_of!(NouveauGemNew, align) == 44);
    assert!(offset_of!(NouveauVmBindOp, addr) == 16);
    assert!(offset_of!(NouveauVmBind, op_ptr) == 32);
    assert!(offset_of!(NouveauExec, push_ptr) == 32);
    assert!(offset_of!(NvifIoctlV0, owner) == 6);
    assert!(offset_of!(NvifIoctlV0, route) == 7);
    assert!(offset_of!(NvifIoctlV0, token) == 8);
    assert!(offset_of!(NvifIoctlV0, object) == 16);
    assert!(offset_of!(NvifIoctlNewV0, route) == 7);
    assert!(offset_of!(NvifIoctlNewV0, handle) == 24);
    assert!(offset_of!(NvifIoctlNewV0, oclass) == 28);
    assert!(offset_of!(NvDeviceInfoV0, platform) == 1);
    assert!(offset_of!(NvDeviceInfoV0, chipset) == 2);
    assert!(offset_of!(NvDeviceInfoV0, ram_user) == 16);
    assert!(offset_of!(NvDeviceInfoV0, chip) == 24);
    assert!(offset_of!(NvDeviceInfoV0, name) == 40);
    assert!(offset_of!(DrmSyncobjTimelineWait, count_handles) == 24);
};

// ---------------------------------------------------------------------------
// Разбор вызова NVIF

/// Тело вызова NVIF, разобранное по месту. Смещение тела в буфере —
/// `size_of::<NvifIoctlV0>()`; ответ пишется обратно туда же.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NvifCall {
    /// `count` мест под классы после заголовка SCLASS.
    Sclass { body: NvifIoctlSclassV0 },
    /// Создание объекта; `args` — смещение аргументов класса в буфере.
    New { body: NvifIoctlNewV0, args: usize },
    Del,
    /// Метод; `args` — смещение аргументов метода в буфере.
    Mthd { body: NvifIoctlMthdV0, args: usize },
    /// Тип, которого мы не эмулируем.
    Other(u8),
}

/// Разобрать буфер вызова NVIF (весь `size` байт из ioctl).
pub fn nvif_parse(buf: &[u8]) -> Result<(NvifIoctlV0, NvifCall), crate::Errno> {
    use crate::Errno;
    let hdr = NvifIoctlV0::read(buf).ok_or(Errno::EINVAL)?;
    if hdr.version != 0 {
        return Err(Errno::ENOSYS);
    }
    let body = &buf[size_of::<NvifIoctlV0>()..];
    let off = size_of::<NvifIoctlV0>();
    let call = match hdr.ty {
        NVIF_IOCTL_V0_SCLASS => {
            let b = NvifIoctlSclassV0::read(body).ok_or(Errno::EINVAL)?;
            let need = size_of::<NvifIoctlSclassV0>() + b.count as usize * size_of::<NvifIoctlSclassOclassV0>();
            if body.len() < need {
                return Err(Errno::EINVAL);
            }
            NvifCall::Sclass { body: b }
        }
        NVIF_IOCTL_V0_NEW => {
            let b = NvifIoctlNewV0::read(body).ok_or(Errno::EINVAL)?;
            NvifCall::New { body: b, args: off + size_of::<NvifIoctlNewV0>() }
        }
        NVIF_IOCTL_V0_DEL => NvifCall::Del,
        NVIF_IOCTL_V0_MTHD => {
            let b = NvifIoctlMthdV0::read(body).ok_or(Errno::EINVAL)?;
            NvifCall::Mthd { body: b, args: off + size_of::<NvifIoctlMthdV0>() }
        }
        t => NvifCall::Other(t),
    };
    Ok((hdr, call))
}

/// Записать ответ SCLASS: `count` = сколько классов есть, первые
/// `min(мест, классов)` записей заполнены (`minver = maxver = -1`, NVK их не
/// читает), хвост мест обнулён (Mesa копирует все 16 записей).
pub fn nvif_sclass_reply(buf: &mut [u8], classes: &[u32]) -> Result<(), crate::Errno> {
    let off = size_of::<NvifIoctlV0>();
    let mut b = NvifIoctlSclassV0::read(&buf[off..]).ok_or(crate::Errno::EINVAL)?;
    let slots = b.count as usize;
    let mut p = off + size_of::<NvifIoctlSclassV0>();
    for i in 0..slots {
        let e = match classes.get(i) {
            Some(&c) => NvifIoctlSclassOclassV0 { oclass: c as i32, minver: -1, maxver: -1 },
            None => NvifIoctlSclassOclassV0::default(),
        };
        if !e.write(&mut buf[p..]) {
            return Err(crate::Errno::EINVAL);
        }
        p += size_of::<NvifIoctlSclassOclassV0>();
    }
    b.count = classes.len().min(255) as u8;
    b.write(&mut buf[off..]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::GpuInfo;
    use alloc::vec;

    #[test]
    fn ioctl_decode() {
        let i = Ioctl::decode(DRM_IOCTL_NOUVEAU_VM_BIND);
        assert!(i.is_drm());
        assert_eq!(i.dir, IOC_READ | IOC_WRITE);
        assert_eq!(i.size, 40);
        assert_eq!(i.driver_nr(), Some(DRM_NOUVEAU_VM_BIND));
        assert_eq!(Ioctl::decode(DRM_IOCTL_VERSION).driver_nr(), None);
        // NVIF у libdrm: drmCommandWrite (IOW) с переменным размером.
        let nvif = iow(0x47, 72);
        assert_eq!(Ioctl::decode(nvif).driver_nr(), Some(DRM_NOUVEAU_NVIF));
        // Синхрообъекты (0xbf..0xcf) — ядро DRM, не драйвер.
        assert_eq!(Ioctl::decode(DRM_IOCTL_SYNCOBJ_WAIT).driver_nr(), None);
    }

    #[test]
    fn pod_round_trip() {
        let op = NouveauVmBindOp { op: 0, flags: 0x106, handle: 7, pad: 0, addr: 0x1000, bo_offset: 0, range: 0x2000 };
        let mut buf = [0u8; 40];
        assert!(op.write(&mut buf));
        assert_eq!(&buf[16..24], &0x1000u64.to_le_bytes());
        let back = NouveauVmBindOp::read(&buf).unwrap();
        assert_eq!(back, op);
        assert_eq!(back.kind(), 6);
        assert!(back.sparse());
        assert!(NouveauVmBindOp::read(&buf[..39]).is_none());
    }

    #[test]
    fn device_info() {
        let info = NvDeviceInfoV0::from_info(&GpuInfo::ga106(12 << 30, 4 << 30));
        let b = info.as_bytes();
        assert_eq!(b[1], 3);
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), 0x176);
        assert_eq!(u64::from_le_bytes(b[16..24].try_into().unwrap()), 12 << 30);
        assert_eq!(&b[24..30], b"GA106\0");
        assert!(b[40..].starts_with(b"NVIDIA GeForce RTX 3060\0"));
    }

    #[test]
    fn nvif_new_and_sclass() {
        // Вызов NVK №3 из раздела 1.2: NEW NV_DEVICE.
        let mut buf = vec![0u8; 24 + 32 + 16];
        let hdr = NvifIoctlV0 { ty: NVIF_IOCTL_V0_NEW, owner: 0xff, ..Default::default() };
        hdr.write(&mut buf);
        let new = NvifIoctlNewV0 { oclass: NV_DEVICE, object: 0xdead, token: 0xdead, ..Default::default() };
        new.write(&mut buf[24..]);
        NvDeviceV0 { device: !0, ..Default::default() }.write(&mut buf[56..]);
        let (h, call) = nvif_parse(&buf).unwrap();
        assert_eq!(h.owner, 0xff);
        match call {
            NvifCall::New { body, args } => {
                assert_eq!(body.oclass, NV_DEVICE);
                assert_eq!(body.object, 0xdead);
                assert_eq!(NvDeviceV0::read(&buf[args..]).unwrap().device, !0);
            }
            c => panic!("{c:?}"),
        }

        // SCLASS на 16 мест, хвост должен обнулиться.
        let mut buf = vec![0xaau8; 24 + 8 + 16 * 8];
        NvifIoctlV0 { ty: NVIF_IOCTL_V0_SCLASS, route: 0xff, token: 3, ..Default::default() }.write(&mut buf);
        NvifIoctlSclassV0 { count: 16, ..Default::default() }.write(&mut buf[24..]);
        assert!(matches!(nvif_parse(&buf).unwrap().1, NvifCall::Sclass { .. }));
        nvif_sclass_reply(&mut buf, &crate::backend::class::GA106_GR_CHANNEL).unwrap();
        assert_eq!(buf[25], 6);
        let e2 = NvifIoctlSclassOclassV0::read(&buf[32 + 2 * 8..]).unwrap();
        assert_eq!(e2.oclass, 0xc797);
        assert!(buf[32 + 6 * 8..].iter().all(|&b| b == 0));

        // Короткий SCLASS (мест больше, чем байтов) — EINVAL.
        assert_eq!(nvif_parse(&buf[..40]).unwrap_err(), crate::Errno::EINVAL);
    }
}
