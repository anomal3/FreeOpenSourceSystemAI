//! Буферы GEM (раздел 1.4 спецификации): `GEM_NEW`, `GEM_INFO`,
//! `GEM_CLOSE`, `GEM_CPU_PREP/FINI`, PRIME (dma-buf fd) и `mmap` буфера.
//!
//! # Где живёт буфер
//!
//! Всё, что процессор отображает (`NOUVEAU_GEM_DOMAIN_MAPPABLE`), — в
//! системной памяти: окно BAR1 у RTX 3060 — 256 МиБ и занято служебным
//! (раздел 0.4). Только буфер «видеопамять, без отображения» уходит в VRAM.
//!
//! # mmap
//!
//! `map_handle` — поддельное смещение `mmap` (как у Linux: от
//! `0x1_0000_0000`, у каждого буфера своё). `mmap(fd, map_handle)` отдаёт
//! [`SharedMem`], собранную из кадров буфера ([`SharedMem::from_frames`]):
//! кадры принадлежат бэкенду, а объект памяти держит [`Bo`] живым, пока
//! отображение есть. Один объект памяти на буфер — сколько бы раз его ни
//! отображали.

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use nvgpu::{BoHandle, GpuBackend, Place};

use super::{DrmFile, get, put};
use crate::linux::e;
use crate::mm::PhysAddr;
use crate::sync::SpinLock;
use crate::user::files::Object;
use crate::user::shm::SharedMem;

pub use nvgpu::uapi::{
    NOUVEAU_GEM_DOMAIN_CPU as DOMAIN_CPU, NOUVEAU_GEM_DOMAIN_GART as DOMAIN_GART, NOUVEAU_GEM_DOMAIN_MAPPABLE as DOMAIN_MAPPABLE,
    NOUVEAU_GEM_DOMAIN_NO_SHARE as DOMAIN_NO_SHARE, NOUVEAU_GEM_DOMAIN_VRAM as DOMAIN_VRAM,
};
use nvgpu::uapi::{
    DrmPrimeHandle as PrimeHandle, NOUVEAU_GEM_CPU_PREP_NOWAIT, NOUVEAU_GEM_CPU_PREP_WRITE, NouveauGemCpuPrep,
    NouveauGemInfo as GemInfo, NouveauGemNew as GemNew,
};

const PAGE: u64 = 4096;

/// Следующее поддельное смещение `mmap`.
static NEXT_MAP: AtomicU64 = AtomicU64::new(0x1_0000_0000);

/// Буфер.
pub struct Bo {
    gpu: Arc<dyn GpuBackend>,
    pub handle: BoHandle,
    /// Байты, кратно 4 КиБ.
    pub size: u64,
    pub place: Place,
    /// Домен для `GEM_INFO`: `VRAM` или `GART`.
    pub domain: u32,
    pub map_handle: u64,
    pub no_share: bool,
    /// `tile_mode`, `tile_flags` (NVK хранит их здесь для экспорта, раздел
    /// `HAS_VMA_TILEMODE`).
    pub tile: SpinLock<(u32, u32)>,
    /// Кадры системного буфера (физические адреса по 4 КиБ).
    frames: Option<Vec<u64>>,
    /// Отображаемая память буфера, пока её кто-то держит.
    mem: SpinLock<Weak<SharedMem>>,
}

impl Drop for Bo {
    fn drop(&mut self) {
        self.gpu.bo_free(self.handle);
    }
}

impl Bo {
    /// Новый буфер у бэкенда.
    pub fn new(gpu: &Arc<dyn GpuBackend>, size: u64, domain: u32, tile: (u32, u32)) -> Result<Arc<Self>, i64> {
        let size = size.checked_next_multiple_of(PAGE).ok_or(-e::EINVAL)?;
        if size == 0 || size > gpu.info().vram_bytes.max(gpu.info().gart_bytes) {
            return Err(if size == 0 { -e::EINVAL } else { -e::ENOMEM });
        }
        // Видеопамять — если её просили и не нужен процессор. «LOCAL» NVK
        // (домен `VRAM|GART`) — тоже в видеопамять, если буфер не делится с
        // другими (`NO_SHARE`, D4 20.11.2026): до того вся память изображений
        // и вершин игры лежала в памяти машины, и GPU рисовал через PCIe.
        // Делимые буферы (кадры WSI, dma-buf) остаются в памяти машины: их
        // читает стол процессором. `nvk local sys` — как раньше.
        let gpu_only = domain & DOMAIN_VRAM != 0
            && domain & DOMAIN_MAPPABLE == 0
            && (domain & DOMAIN_GART == 0 || (domain & DOMAIN_NO_SHARE != 0 && super::local_in_vram()));
        let place = if gpu_only { Place::Vram } else { Place::Sysmem };
        let (place, handle) = match gpu.bo_alloc(size, place) {
            Ok(handle) => (place, handle),
            // Видеопамять кончилась — `LOCAL` можно и в памяти машины.
            Err(_) if place == Place::Vram && domain & DOMAIN_GART != 0 => {
                (Place::Sysmem, gpu.bo_alloc(size, Place::Sysmem).map_err(|err| -i64::from(err.0))?)
            }
            Err(err) => return Err(-i64::from(err.0)),
        };
        let frames = match place {
            Place::Sysmem => match gpu.bo_frames(handle) {
                Some(frames) if frames.len() as u64 * PAGE >= size => Some(frames),
                _ => {
                    gpu.bo_free(handle);
                    return Err(-e::ENOMEM);
                }
            },
            Place::Vram => None,
        };
        // Смещения не пересекаются; между буферами — страница зазора.
        let map_handle = NEXT_MAP.fetch_add(size + PAGE, Ordering::Relaxed);
        Ok(Arc::new(Self {
            gpu: Arc::clone(gpu),
            handle,
            size,
            place,
            domain: if place == Place::Vram { DOMAIN_VRAM } else { DOMAIN_GART },
            map_handle,
            no_share: domain & DOMAIN_NO_SHARE != 0,
            tile: SpinLock::new(tile),
            frames,
            mem: SpinLock::new(Weak::new()),
        }))
    }

    /// Кадры системного буфера (для сервера Wayland, dma-buf).
    #[must_use]
    pub fn frames(&self) -> Option<&[u64]> {
        self.frames.as_deref()
    }

    /// Память для `mmap` (одна на буфер). `None` — буфер в видеопамяти.
    fn shared(self: &Arc<Self>) -> Option<Arc<SharedMem>> {
        let frames = self.frames.as_ref()?;
        let mut slot = self.mem.lock();
        if let Some(mem) = slot.upgrade() {
            return Some(mem);
        }
        let phys: Vec<PhysAddr> = frames.iter().map(|&addr| PhysAddr(addr)).collect();
        let keep: Arc<dyn Any + Send + Sync> = Arc::clone(self) as Arc<dyn Any + Send + Sync>;
        let mem = SharedMem::from_frames(&phys, self.size, keep);
        *slot = Arc::downgrade(&mem);
        Some(mem)
    }
}

/// Память буфера для `mmap` и для сервера Wayland ([`super::dmabuf_memory`]).
pub fn shared_of(bo: &Arc<Bo>) -> Option<Arc<SharedMem>> {
    bo.shared()
}

// ---- mmap ------------------------------------------------------------------------

/// `mmap(drm_fd, offset)`: буфер файла, у которого `map_handle ≤ offset`.
pub fn mmap_file(file: &DrmFile, offset: u64, len: u64) -> Result<(Arc<SharedMem>, u64), i64> {
    let bo = {
        let st = file.st.lock();
        st.bos.values().find(|bo| offset >= bo.map_handle && offset < bo.map_handle + bo.size).cloned()
    };
    let Some(bo) = bo else { return Err(-e::EINVAL) };
    mmap_at(&bo, offset - bo.map_handle, len)
}

/// `mmap` дескриптора dma-buf: смещение — от начала буфера.
pub fn mmap_bo(bo: &Arc<Bo>, offset: u64, len: u64) -> Result<(Arc<SharedMem>, u64), i64> {
    mmap_at(bo, offset, len)
}

fn mmap_at(bo: &Arc<Bo>, inside: u64, len: u64) -> Result<(Arc<SharedMem>, u64), i64> {
    if inside % PAGE != 0 || inside.checked_add(len).is_none_or(|end| end > bo.size) {
        return Err(-e::EINVAL);
    }
    // Буфер в видеопамяти процессору не виден (окна BAR1 под него нет).
    let mem = bo.shared().ok_or(-e::EINVAL)?;
    Ok((mem, inside))
}

// ---- ioctl ---------------------------------------------------------------------

fn info_of(handle: u32, bo: &Bo) -> GemInfo {
    let (tile_mode, tile_flags) = *bo.tile.lock();
    GemInfo { handle, domain: bo.domain, size: bo.size, offset: 0, map_handle: bo.map_handle, tile_mode, tile_flags }
}

/// `DRM_IOCTL_NOUVEAU_GEM_NEW`.
pub fn gem_new(file: &DrmFile, arg: usize) -> i64 {
    let Ok(mut args) = get::<GemNew>(arg) else { return -e::EFAULT };
    // Как `nouveau_gem_new`: ни VRAM, ни GART — значит, CPU (системная
    // память); NO_SHARE — только у файла с VM (буфер делит `resv` VM).
    let mut domain = args.info.domain;
    if domain & (DOMAIN_VRAM | DOMAIN_GART) == 0 {
        domain |= DOMAIN_CPU;
    }
    if domain & DOMAIN_NO_SHARE != 0 && file.st.lock().vm.is_none() {
        return -e::EINVAL;
    }
    if args.align != 0 && !args.align.is_power_of_two() {
        return -e::EINVAL;
    }
    let bo = match Bo::new(&file.gpu, args.info.size, domain, (args.info.tile_mode, args.info.tile_flags)) {
        Ok(bo) => bo,
        Err(err) => return err,
    };
    let handle = file.st.lock().add_bo(Arc::clone(&bo));
    args.info = info_of(handle, &bo);
    if put(arg, args).is_err() {
        file.st.lock().bos.remove(&handle);
        return -e::EFAULT;
    }
    0
}

/// `DRM_IOCTL_NOUVEAU_GEM_INFO`.
pub fn gem_info(file: &DrmFile, arg: usize) -> i64 {
    let Ok(args) = get::<GemInfo>(arg) else { return -e::EFAULT };
    let Some(bo) = file.st.lock().bos.get(&args.handle).cloned() else { return -e::ENOENT };
    if put(arg, info_of(args.handle, &bo)).is_err() { -e::EFAULT } else { 0 }
}

/// `DRM_IOCTL_NOUVEAU_GEM_CPU_PREP`: ждать, пока GPU не закончит с буфером.
/// Кто из заборов трогает буфер, мы не следим (у Linux — `dma_resv`
/// буфера); ждём все каналы файла — шире, но верно.
pub fn gem_cpu_prep(file: &DrmFile, arg: usize) -> i64 {
    const NOWAIT: u32 = NOUVEAU_GEM_CPU_PREP_NOWAIT;
    let Ok(NouveauGemCpuPrep { handle, flags }) = get::<NouveauGemCpuPrep>(arg) else { return -e::EFAULT };
    if flags & !(NOWAIT | NOUVEAU_GEM_CPU_PREP_WRITE) != 0 {
        return -e::EINVAL;
    }
    let chans: Vec<_> = {
        let st = file.st.lock();
        if !st.bos.contains_key(&handle) {
            return -e::ENOENT;
        }
        st.chans.values().map(|chan| Arc::clone(&chan.core)).collect()
    };
    let idle = || chans.iter().all(|chan| chan.idle());
    if flags & NOWAIT != 0 {
        return if idle() { 0 } else { -super::EBUSY };
    }
    // Как у Linux: 30 с, потом `EBUSY`.
    let deadline = crate::time::uptime_ns() + 30_000_000_000;
    match super::sync::wait_until(Some(deadline), || Ok(idle())) {
        Ok(()) => 0,
        Err(err) if err == -super::ETIME => -super::EBUSY,
        Err(err) => err,
    }
}

/// `DRM_IOCTL_GEM_CLOSE`.
pub fn close(file: &DrmFile, arg: usize) -> i64 {
    let Ok([handle, _pad]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
    // Буфер, отображённый в VM или в память программы, живёт дальше —
    // уходит только ручка.
    let bo = file.st.lock().bos.remove(&handle);
    if bo.is_some() { 0 } else { -e::EINVAL }
}

/// `DRM_IOCTL_PRIME_HANDLE_TO_FD`: dma-buf fd буфера.
pub fn prime_handle_to_fd(file: &DrmFile, arg: usize) -> i64 {
    const O_RDWR: u32 = 2;
    const O_CLOEXEC: u32 = 0o2_000_000;
    let Ok(mut args) = get::<PrimeHandle>(arg) else { return -e::EFAULT };
    if args.flags & !(O_RDWR | O_CLOEXEC) != 0 {
        return -e::EINVAL;
    }
    let Some(bo) = file.st.lock().bos.get(&args.handle).cloned() else { return -e::ENOENT };
    // `nouveau_gem_prime_export`: буфер NO_SHARE не экспортируется.
    if bo.no_share {
        return -e::EPERM;
    }
    let fd = super::install(Arc::new(DmaBuf(bo)), false, args.flags & O_CLOEXEC != 0);
    if fd < 0 {
        return fd;
    }
    args.fd = fd as i32;
    if put(arg, args).is_err() { -e::EFAULT } else { 0 }
}

/// `DRM_IOCTL_PRIME_FD_TO_HANDLE`: ручка для dma-buf; уже есть у файла —
/// та же ручка (как у Linux).
pub fn prime_fd_to_handle(file: &DrmFile, arg: usize) -> i64 {
    let Ok(mut args) = get::<PrimeHandle>(arg) else { return -e::EFAULT };
    let Some(object) = super::object_of(args.fd as u32 as usize) else { return -e::EBADF };
    let Some(buf) = object.as_any().downcast_ref::<DmaBuf>() else { return -e::EINVAL };
    let handle = {
        let mut st = file.st.lock();
        match st.bos.iter().find(|(_, bo)| Arc::ptr_eq(bo, &buf.0)) {
            Some((handle, _)) => *handle,
            None => st.add_bo(Arc::clone(&buf.0)),
        }
    };
    args.handle = handle;
    if put(arg, args).is_err() { -e::EFAULT } else { 0 }
}

// ---- dma-buf ---------------------------------------------------------------------

/// dma-buf fd: буфер, переданный другому процессу или другому API
/// (Wayland `zwp_linux_dmabuf_v1`, M7).
pub struct DmaBuf(pub Arc<Bo>);

impl DmaBuf {
    #[must_use]
    pub fn bo(&self) -> &Arc<Bo> {
        &self.0
    }
}

impl Object for DmaBuf {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (true, true, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// ioctl на dma-buf: `DMA_BUF_IOCTL_SYNC` — память когерентна, делать
/// нечего; неявная синхронизация через sync_file (EXPORT/IMPORT) — нет
/// (`ENOTTY`: Mesa тогда обходится без неё).
pub fn dmabuf_ioctl(dmabuf: &Arc<dyn Object>, request: u32, arg: usize) -> i64 {
    const SYNC: u32 = 0x4008_6200;
    // `struct dma_buf_export_sync_file` / `dma_buf_import_sync_file`:
    // `{u32 flags; s32 fd}` (Linux 6.0+). Mesa WSI (`wsi_common_drm.c`)
    // ставит ими забор кадра на dma-buf при показе (NVK неявной синхронизации
    // ядра не знает) и берёт забор перед записью в кадр.
    const EXPORT_SYNC_FILE: u32 = 0xc008_6202;
    const IMPORT_SYNC_FILE: u32 = 0x4008_6203;
    match request {
        SYNC => {
            if get::<u64>(arg).is_err() { -e::EFAULT } else { 0 }
        }
        // Наш стол пишет в буферы клиента только процессором и сразу —
        // забор «уже выполнен».
        EXPORT_SYNC_FILE => {
            let Ok(mut args) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
            if args[0] & !3 != 0 || args[0] == 0 {
                return -e::EINVAL;
            }
            let fd = super::install(Arc::new(super::sync::SyncFile(nvgpu::syncobj::Fence::signaled())), false, true);
            if fd < 0 {
                return fd;
            }
            args[1] = fd as u32;
            if put(arg, args).is_err() { -e::EFAULT } else { 0 }
        }
        // Неявной синхронизации у стола нет: он читает кадр сразу после
        // `commit`. Поэтому забор кадра (конец отрисовки на GPU) ждётся
        // здесь, до показа, — не дольше 2 с (сторож канала — 10 с).
        // 25.11: by default not here - the fence is noted on the buffer and
        // the Wayland server waits for it before the show ([`frame_pending`]).
        IMPORT_SYNC_FILE => {
            let Ok(args) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
            if args[0] & !3 != 0 || args[0] == 0 {
                return -e::EINVAL;
            }
            let Some(object) = super::object_of(args[1] as usize) else { return -e::EBADF };
            let Some(sync) = object.as_any().downcast_ref::<super::sync::SyncFile>() else { return -e::EINVAL };
            super::sync::refresh();
            if super::sync::signaled(&sync.0) {
                return 0;
            }
            let first = dmabuf.as_any().downcast_ref::<DmaBuf>().and_then(|buf| buf.bo().frames().and_then(|frames| frames.first().copied()));
            if let (true, Some(first)) = (FENCE_ASYNC.load(Ordering::Relaxed), first) {
                note_pending(first, sync.0.clone());
                return 0;
            }
            let deadline = crate::time::uptime_ns() + 2_000_000_000;
            match super::sync::wait_until(Some(deadline), || Ok(super::sync::signaled(&sync.0))) {
                Ok(()) => 0,
                Err(err) if err == -super::ETIME => {
                    crate::kprintln!("nvgpu: dma-buf IMPORT_SYNC_FILE: frame fence not done in 2 s - shown anyway");
                    0
                }
                Err(err) => err,
            }
        }
        _ => -e::ENOTTY,
    }
}

// ---- frame fences for the desktop (25.11) -------------------------------------

/// 25.11 (hw-log19: `DMA_BUF_IMPORT_SYNC_FILE` 3611x in 30 s, 3.2 ms each,
/// worst 19.5 ms, in GOH): the frame fence Mesa WSI puts on the dma-buf at
/// present was waited for inside the ioctl - the game's present thread (DXVK:
/// the same queue thread that submits the next frame) stood until the GPU
/// finished, so CPU and GPU never overlapped. Now the fence is noted for the
/// buffer and the Wayland server shows the buffer once the fence is done
/// ([`frame_pending`]), like a compositor with implicit sync on Linux.
/// `wldirect fence wait` - the old wait in the ioctl.
pub static FENCE_ASYNC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Pending frame fences: first physical frame of the buffer, the fence, and
/// when it was noted (ns).
static PENDING: SpinLock<Vec<(u64, super::sync::Fence, u64)>> = SpinLock::new(Vec::new());

/// The longest a frame waits for its fence before it is shown anyway (as
/// the old wait in the ioctl).
const FENCE_SHOW_MAX_NS: u64 = 2_000_000_000;

/// Frames noted with a pending fence since boot (perf-diag/`wldirect`).
pub static FENCES_NOTED: AtomicU64 = AtomicU64::new(0);

fn note_pending(first: u64, fence: super::sync::Fence) {
    let now = crate::time::uptime_ns();
    let mut list = PENDING.lock();
    // A buffer gets a new frame fence only after the client had it back, so
    // its older entry is over; done or too old entries go too.
    list.retain(|(at, old, since)| *at != first && now.saturating_sub(*since) < FENCE_SHOW_MAX_NS && !super::sync::signaled(old));
    list.push((first, fence, now));
    FENCES_NOTED.fetch_add(1, Ordering::Relaxed);
}

/// Is the GPU still drawing the frame in the dma-buf whose memory starts at
/// physical `first` (a fence imported at present not done yet)? Past
/// [`FENCE_SHOW_MAX_NS`] - no (logged): the frame is shown anyway.
#[must_use]
pub fn frame_pending(first: u64) -> bool {
    if PENDING.lock().is_empty() {
        return false;
    }
    super::sync::refresh();
    let now = crate::time::uptime_ns();
    let mut list = PENDING.lock();
    let Some(index) = list.iter().position(|(at, _, _)| *at == first) else {
        return false;
    };
    let (_, fence, since) = &list[index];
    if super::sync::signaled(fence) {
        list.swap_remove(index);
        return false;
    }
    if now.saturating_sub(*since) >= FENCE_SHOW_MAX_NS {
        list.swap_remove(index);
        drop(list);
        crate::kprintln!("nvgpu: dma-buf frame fence not done in 2 s - shown anyway");
        return false;
    }
    true
}
