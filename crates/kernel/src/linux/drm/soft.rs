//! `SoftGpu` (WP-B, `nvgpu::softgpu`) как бэкенд узла: программная видеокарта
//! для QEMU и проб (`nvk soft on`).
//!
//! Хозяин [`KernelHost`] даёт ей кадры пула ядра и доступ к ним через прямое
//! отображение. Системные буферы (`bo_frames`) — ровно эти кадры, так что
//! `mmap` буфера и «GPU» видят одну память. «Видеопамять» `SoftGpu` берёт у
//! того же хозяина по кадру, лениво.

use alloc::sync::Arc;

use nvgpu::{GpuBackend, SoftGpu, SoftHost};

use crate::mm::{FrameAllocator, PhysAddr};

/// Кадры и часы ядра для `SoftGpu`.
struct KernelHost;

impl KernelHost {
    fn bytes(phys: u64) -> *mut u8 {
        PhysAddr(phys & !0xfff).to_direct_map().as_mut_ptr::<u8>()
    }
}

impl SoftHost for KernelHost {
    fn alloc_frame(&self) -> Option<u64> {
        let frame = crate::mm::frame::with(|pool| {
            // Запас ядра не трогаем — как `SharedMem`.
            if pool.stats().free <= crate::user::RESERVE_FRAMES {
                return None;
            }
            pool.allocate()
        })
        .flatten()?;
        // SAFETY: кадр только что выдан пулом и ничей; прямое отображение
        // покрывает всю память.
        unsafe { core::ptr::write_bytes(Self::bytes(frame.0), 0, 4096) };
        Some(frame.0)
    }

    fn free_frame(&self, phys: u64) {
        crate::mm::frame::with(|pool| {
            // SAFETY: кадр выдан `alloc_frame` этого хозяина, и `SoftGpu`
            // отдаёт его один раз, когда ссылок на него не осталось (буфер
            // освобождается после последнего отображения — см. `gem::Bo`).
            unsafe { pool.free(PhysAddr(phys)) };
        });
    }

    fn read_phys(&self, phys: u64, buf: &mut [u8]) {
        let inside = (phys & 0xfff) as usize;
        let len = buf.len().min(4096 - inside);
        // SAFETY: кадр выдан этим хозяином и жив; внутри одной страницы.
        unsafe { core::ptr::copy_nonoverlapping(Self::bytes(phys).add(inside), buf.as_mut_ptr(), len) };
    }

    fn write_phys(&self, phys: u64, data: &[u8]) {
        let inside = (phys & 0xfff) as usize;
        let len = data.len().min(4096 - inside);
        // SAFETY: см. `read_phys`.
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), Self::bytes(phys).add(inside), len) };
    }

    fn now_ns(&self) -> u64 {
        crate::time::uptime_ns()
    }

    fn log(&self, line: &str) {
        crate::kprintln!("nvgpu: {line}");
    }
}

/// Новый `SoftGpu` (RTX 3060 по `GpuInfo::ga106`, 256 МиБ «видеопамяти»).
pub(super) fn new() -> Option<Arc<dyn GpuBackend>> {
    let host: Arc<dyn SoftHost> = Arc::new(KernelHost);
    Some(Arc::new(SoftGpu::new(host)))
}
