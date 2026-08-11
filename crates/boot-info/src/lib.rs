//! The hand-off contract between the UEFI bootloader and the kernel.
//!
//! This crate is deliberately dependency-free and `#![no_std]`. It is linked
//! into two binaries that are compiled for *different targets* (a UEFI
//! application and a freestanding kernel), so every type crossing the boundary
//! is `#[repr(C)]` with explicit padding and no Rust-layout-dependent types
//! (no `Option`, no references, no enums without `repr`).
//!
//! Absent values are encoded as sentinel `0` addresses rather than `Option`,
//! because `Option<T>` has no guaranteed C layout for the types used here.

#![no_std]

/// Identifies a valid [`BootInfo`] hand-off. Spells "FREEOS" plus a tag.
pub const BOOT_INFO_MAGIC: u64 = 0x4652_4545_4F53_0001;

/// Incremented whenever [`BootInfo`] changes shape. The kernel refuses to boot
/// on a mismatch instead of silently reading garbage from an older bootloader.
///
/// Revision 2 added [`BootInfo::kernel`], which the kernel needs to apply W^X
/// to its own image.
pub const BOOT_INFO_REVISION: u32 = 2;

/// Which instruction set the bootloader was built for.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64 = 1,
    AArch64 = 2,
}

/// Byte order of the colour channels within a 32-bit framebuffer pixel.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// Byte 0 red, byte 1 green, byte 2 blue, byte 3 reserved.
    Rgb = 0,
    /// Byte 0 blue, byte 1 green, byte 2 red, byte 3 reserved.
    Bgr = 1,
    /// A format we could not translate; treat the framebuffer as unusable.
    Unknown = 2,
}

/// A linear framebuffer obtained from the UEFI Graphics Output Protocol.
///
/// Check [`Framebuffer::is_present`] before use: a machine booted headless has
/// no GOP and reports `base == 0`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer {
    /// Physical address of the first pixel. `0` means "no framebuffer".
    pub base: u64,
    /// Total size of the framebuffer in bytes.
    pub size: u64,
    /// Visible width in pixels.
    pub width: u32,
    /// Visible height in pixels.
    pub height: u32,
    /// Pixels per scanline. May exceed `width`: rows can be padded, so address
    /// a pixel as `base + (y * stride + x) * 4`, never `y * width + x`.
    pub stride: u32,
    pub format: PixelFormat,
}

impl Framebuffer {
    /// A sentinel meaning the firmware exposed no usable GOP framebuffer.
    pub const NONE: Self = Self {
        base: 0,
        size: 0,
        width: 0,
        height: 0,
        stride: 0,
        format: PixelFormat::Unknown,
    };

    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.base != 0
    }
}

/// How the kernel may treat a physical memory range.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free for the kernel's frame allocator.
    Usable = 0,
    /// Firmware-reserved or MMIO. Never allocate from this.
    Reserved = 1,
    /// ACPI tables; reclaimable once the kernel has parsed them.
    AcpiReclaimable = 2,
    /// ACPI non-volatile storage; must be preserved.
    AcpiNvs = 3,
    /// Holds the bootloader itself, the memory map, and `BootInfo`. Reclaimable
    /// once the kernel has copied out everything it needs.
    BootloaderReclaimable = 4,
    /// The loaded kernel image.
    Kernel = 5,
    /// Backing store of the framebuffer.
    Framebuffer = 6,
}

/// One physical memory range. Regions are sorted by `start` and never overlap.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    pub start: u64,
    /// Length in bytes. Always a multiple of 4 KiB.
    pub len: u64,
    pub kind: MemoryKind,
    _reserved: u32,
}

impl MemoryRegion {
    #[must_use]
    pub const fn new(start: u64, len: u64, kind: MemoryKind) -> Self {
        Self { start, len, kind, _reserved: 0 }
    }

    /// One past the last byte of this region.
    #[must_use]
    pub const fn end(&self) -> u64 {
        self.start + self.len
    }
}

/// Points at the array of [`MemoryRegion`]s the bootloader built.
///
/// The array lives in [`MemoryKind::BootloaderReclaimable`] memory, so the
/// kernel must copy it before reclaiming that memory.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryMap {
    /// Physical address of the first `MemoryRegion`.
    pub ptr: u64,
    /// Number of entries.
    pub len: u64,
}

impl MemoryMap {
    pub const EMPTY: Self = Self { ptr: 0, len: 0 };

    /// # Safety
    ///
    /// Only valid while the region array is still mapped and identity-mapped at
    /// `ptr`, and before the bootloader-reclaimable memory has been reused.
    #[must_use]
    pub unsafe fn as_slice(&self) -> &[MemoryRegion] {
        if self.ptr == 0 || self.len == 0 {
            return &[];
        }
        // SAFETY: the caller guarantees `ptr` still points at `len` initialised,
        // properly aligned `MemoryRegion`s for the lifetime of the borrow.
        unsafe { core::slice::from_raw_parts(self.ptr as *const MemoryRegion, self.len as usize) }
    }
}

/// Everything the bootloader hands the kernel at `ExitBootServices` time.
///
/// The kernel receives this by pointer and must validate [`BootInfo::is_valid`]
/// before touching any other field.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BootInfo {
    pub magic: u64,
    pub revision: u32,
    pub arch: Arch,
    pub framebuffer: Framebuffer,
    pub memory_map: MemoryMap,
    /// Where the kernel image was placed, and its per-segment permissions.
    pub kernel: KernelImage,
    /// Physical address of the ACPI RSDP, or `0` if the firmware exposed none.
    pub acpi_rsdp: u64,
    /// Physical address of a flattened device tree, or `0` if none. Reserved
    /// for ARM platforms whose firmware provides DTB instead of ACPI.
    pub device_tree: u64,
}

/// Segment is readable.
pub const SEG_READ: u32 = 1 << 0;
/// Segment is writable.
pub const SEG_WRITE: u32 = 1 << 1;
/// Segment holds executable code.
pub const SEG_EXEC: u32 = 1 << 2;

/// One loaded segment of the kernel image, with the permissions the ELF asked
/// for.
///
/// The kernel cannot derive these itself: it is linked without a linker script,
/// so it has no `__text_start`-style symbols to consult. The bootloader, on the
/// other hand, has already parsed the program headers and knows exactly which
/// bytes are code and which are data — so it passes that knowledge along rather
/// than making the kernel rediscover it.
///
/// Without this, the only way to build page tables would be to map the whole
/// image writable *and* executable, which is precisely the W^X violation the
/// mapping is supposed to prevent.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KernelSegment {
    /// Address in memory after placement and relocation, rounded down to a page.
    pub base: u64,
    /// Length in bytes, rounded up to a whole number of pages.
    pub len: u64,
    /// Any combination of [`SEG_READ`], [`SEG_WRITE`], [`SEG_EXEC`].
    pub flags: u32,
    _reserved: u32,
}

impl KernelSegment {
    #[must_use]
    pub const fn new(base: u64, len: u64, flags: u32) -> Self {
        Self { base, len, flags, _reserved: 0 }
    }

    #[must_use]
    pub const fn is_executable(&self) -> bool {
        self.flags & SEG_EXEC != 0
    }

    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.flags & SEG_WRITE != 0
    }
}

/// Where the kernel image ended up, and how its segments want to be protected.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KernelImage {
    /// Lowest address of the placed image.
    pub base: u64,
    /// Total span in bytes, covering every segment and any padding between them.
    pub size: u64,
    /// Physical address of a [`KernelSegment`] array, or `0` if none was passed.
    pub segments_ptr: u64,
    /// Number of entries in that array.
    pub segments_len: u64,
}

impl KernelImage {
    pub const EMPTY: Self = Self { base: 0, size: 0, segments_ptr: 0, segments_len: 0 };

    /// # Safety
    ///
    /// Same conditions as [`MemoryMap::as_slice`]: the array must still be
    /// mapped at `segments_ptr` and its memory not yet reclaimed.
    #[must_use]
    pub unsafe fn segments(&self) -> &[KernelSegment] {
        if self.segments_ptr == 0 || self.segments_len == 0 {
            return &[];
        }
        // SAFETY: the caller guarantees the array is still live and correctly
        // aligned for `segments_len` entries.
        unsafe {
            core::slice::from_raw_parts(
                self.segments_ptr as *const KernelSegment,
                self.segments_len as usize,
            )
        }
    }
}

/// Сигнатура точки входа ядра.
///
/// Загрузчик берёт адрес из ELF-заголовка ядра, приводит его к этому типу и
/// вызывает, передавая указатель на [`BootInfo`]. Возврат не предусмотрен: к
/// моменту вызова `ExitBootServices` уже сделан, и возвращаться попросту
/// некуда — прошивки больше нет.
///
/// Явный ABI здесь обязателен, и `extern "C"` для этого НЕ годится.
///
/// Две стороны вызова собираются под разные таргеты, и `extern "C"` означает у
/// них разное: под `x86_64-unknown-uefi` это Microsoft x64 (первый аргумент в
/// `RCX`), а под `x86_64-unknown-none` — System V (первый аргумент в `RDI`).
/// Обе стороны компилируются молча, а в рантайме ядро читает регистр, в котором
/// лежит мусор, — ошибка проявляется как «BootInfo повреждён», уводя от
/// настоящей причины. Поэтому на x86-64 соглашение фиксируется явно.
///
/// На AArch64 расхождения нет: и UEFI, и freestanding-таргет используют
/// AAPCS64, так что `extern "C"` там означает ровно одно и то же.
#[cfg(target_arch = "x86_64")]
pub type KernelEntry = extern "sysv64" fn(boot_info: *const BootInfo) -> !;

#[cfg(target_arch = "aarch64")]
pub type KernelEntry = extern "C" fn(boot_info: *const BootInfo) -> !;

impl BootInfo {
    #[must_use]
    pub const fn new(arch: Arch) -> Self {
        Self {
            magic: BOOT_INFO_MAGIC,
            revision: BOOT_INFO_REVISION,
            arch,
            framebuffer: Framebuffer::NONE,
            memory_map: MemoryMap::EMPTY,
            kernel: KernelImage::EMPTY,
            acpi_rsdp: 0,
            device_tree: 0,
        }
    }

    /// True when this really is a `BootInfo` of a revision the kernel understands.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.magic == BOOT_INFO_MAGIC && self.revision == BOOT_INFO_REVISION
    }
}
