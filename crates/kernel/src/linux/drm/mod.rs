//! `/dev/dri/renderD128` для программ Linux: uAPI DRM nouveau поверх
//! [`nvgpu::GpuBackend`] (18.11.2026, «Видеокарта для игр: NVK поверх
//! GSP-RM», пакет WP-C спецификации `nvk-spec.md`).
//!
//! # Зачем
//!
//! Mesa NVK (`libvulkan_nouveau.so` из Debian, неизменённый) говорит с
//! ядром только через узел DRM: `drmGetDevices2` libdrm находит его по
//! `/dev/dri` и `/sys/dev/char/226:128/…`, `DRM_IOCTL_VERSION` должен
//! назвать драйвер «nouveau», дальше — `VM_INIT`, NVIF, `GEM_NEW`,
//! `VM_BIND`, `EXEC` и объекты синхронизации. Здесь — всё это, а сама
//! видеокарта — за трейтом бэкенда: настоящий (GSP-RM на RTX 3060, WP-A)
//! или программный `SoftGpu` (WP-B) для QEMU.
//!
//! # Когда узел есть
//!
//! Только когда бэкенд поставлен и включён: настоящий регистрирует WP-A
//! ([`set_backend`], после подъёма GSP), а показывается он после команды
//! `nvk hw on` ([`set_hw`]); программный — `nvk soft on` ([`set_soft`]). Без
//! бэкенда нет
//! ни `/dev/dri`, ни записей sysfs — система ведёт себя как раньше, и Mesa
//! берёт lavapipe.
//!
//! # Устройство
//!
//! * [`DrmFile`] — открытый узел (как `struct drm_file` + `nouveau_cli`):
//!   таблицы ручек BO и syncobj, адресное пространство после `VM_INIT`,
//!   каналы, объекты NVIF. Всё под одним спящим [`Mutex`]; ожидание заборов
//!   идёт без него.
//! * [`gem`] — буферы, PRIME (dma-buf fd) и `mmap` буфера.
//! * [`sync`] — заборы каналов и syncobj (двоичные и timeline), sync_file.
//! * [`nouveau`] — ioctl nouveau, [`nvif`] — крошечная эмуляция NVIF,
//!   [`vm`] — учёт VA (`VM_BIND`), [`sysfs`] — файлы для libdrm.

pub mod amdgpu;
mod gem;
mod nouveau;
mod nvif;
pub(crate) mod sync;
pub mod sysfs;
mod vm;

pub use sync::{FENCE_SPIN_US, FENCE_STEP_US, fence_bench, fence_fine_sleeps, fence_wait_stats};

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nvgpu::GpuBackend;

use super::e;
use crate::sync::{Mutex, SpinLock};
use crate::user::files::Object;

pub use gem::Bo;

/// Путь узла рендера.
pub const RENDER_NODE: &str = "/dev/dri/renderD128";
/// `major` узлов DRM у Linux.
pub const DRM_MAJOR: u32 = 226;
/// `minor` узла рендера (128 + номер карты).
pub const RENDER_MINOR: u32 = 128;

/// `st_rdev` узла рендера: `makedev(226, 128)` в кодировке glibc.
#[must_use]
pub const fn render_rdev() -> u64 {
    let (major, minor) = (DRM_MAJOR as u64, RENDER_MINOR as u64);
    ((major & 0xffff_f000) << 32) | ((major & 0xfff) << 8) | ((minor & 0xffff_ff00) << 12) | (minor & 0xff)
}

// ---- бэкенд -------------------------------------------------------------------

/// Бэкенды: настоящий (регистрирует WP-A) и программный (`nvk soft on`).
struct Backends {
    real: Option<Arc<dyn GpuBackend>>,
    soft: Option<Arc<dyn GpuBackend>>,
}

static BACKENDS: SpinLock<Backends> = SpinLock::new(Backends { real: None, soft: None });

/// Показывать ли узел настоящей карты (`nvk hw on|off`). До 20.11 — нет
/// (пока M5 не пройден, DXVK выбрал бы сломанный NVK вместо lavapipe). С
/// D4 (20.11.2026, `gpu gametest` 7/7 на RTX 3060) — да: узел появляется сам,
/// как только GSP-RM поднят и бэкенд зарегистрирован, и игра из «Пуска» сразу
/// берёт видеокарту (первый запуск TMNF в hw-log12 шёл на процессоре).
pub const DEFAULT_HW: bool = true;
static HW: AtomicBool = AtomicBool::new(DEFAULT_HW);

/// «LOCAL»-память NVK, не делимую с другими, — в видеопамять (`nvk local
/// vram|sys`, D4 20.11.2026; см. `gem::Bo::new`).
static LOCAL_VRAM: AtomicBool = AtomicBool::new(true);

pub fn set_local_in_vram(on: bool) {
    LOCAL_VRAM.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn local_in_vram() -> bool {
    LOCAL_VRAM.load(Ordering::Relaxed)
}

/// Трассировка ioctl в журнал (`nvk trace on`).
static TRACE: AtomicBool = AtomicBool::new(false);

/// Зарегистрировать (или снять — `None`) бэкенд настоящей видеокарты. Зовёт
/// WP-A, когда сессия GSP поднята, и `None` — перед выгрузкой GSP. Узел
/// появится только после `nvk hw on`. Уже открытые файлы держат свой
/// бэкенд до закрытия; новые `open` видят новый.
pub fn set_backend(gpu: Option<Arc<dyn GpuBackend>>) {
    match &gpu {
        Some(gpu) => crate::kprintln!(
            "nvgpu: drm backend real registered ({}), node {}{}",
            gpu.info().name,
            if hw() { "/dev/dri/renderD128 on" } else { "off until `nvk hw on`" },
            if local_in_vram() { ", NVK local memory in VRAM" } else { ", NVK local memory in system RAM" }
        ),
        None => crate::kprintln!("nvgpu: drm backend real removed"),
    }
    BACKENDS.lock().real = gpu;
}

/// `nvk hw on|off`: показывать ли `/dev/dri/renderD128` настоящей карты.
pub fn set_hw(on: bool) {
    HW.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn hw() -> bool {
    HW.load(Ordering::Relaxed)
}

/// Программная видеокарта `SoftGpu` (команда `nvk soft on|off`): только для
/// QEMU и проб. `false` — не поставилась.
pub fn set_soft(on: bool) -> bool {
    if !on {
        BACKENDS.lock().soft = None;
        return true;
    }
    if BACKENDS.lock().soft.is_some() {
        return true;
    }
    // Создаётся без замка: выделяет память.
    let Some(gpu) = soft::new() else { return false };
    crate::kprintln!("nvgpu: drm backend soft ({})", gpu.info().name);
    BACKENDS.lock().soft.get_or_insert(gpu);
    true
}

mod soft;

/// `(настоящий зарегистрирован, soft стоит)`.
#[must_use]
pub fn backend_state() -> (bool, bool) {
    let b = BACKENDS.lock();
    (b.real.is_some(), b.soft.is_some())
}

/// Бэкенд узла, если узел сейчас существует: настоящий при `nvk hw on`,
/// иначе программный.
pub(crate) fn backend() -> Option<Arc<dyn GpuBackend>> {
    let b = BACKENDS.lock();
    if hw() {
        if let Some(real) = &b.real {
            return Some(Arc::clone(real));
        }
    }
    b.soft.clone()
}

/// Есть ли сейчас `/dev/dri/renderD128`: узел AMD (`amdnode soft on`) или NVIDIA.
#[must_use]
pub fn node_visible() -> bool {
    amdgpu::active() || backend().is_some()
}

/// Что про карту узла знает sysfs: драйвер, идентификаторы PCI, слот.
pub(crate) struct Ident {
    pub driver: &'static str,
    pub vendor: u16,
    pub device: u16,
    pub revision: u8,
    pub subsys_vendor: u16,
    pub subsys_device: u16,
    pub slot_name: alloc::string::String,
}

/// Описание карты узла: AMD, если он включён, иначе NVIDIA.
pub(crate) fn node_ident() -> Option<Ident> {
    if let Some(ident) = amdgpu::ident() {
        return Some(ident);
    }
    let gpu = backend()?;
    let info = gpu.info();
    let slot = info.pci_slot;
    Some(Ident {
        driver: "nouveau",
        vendor: info.pci_vendor,
        device: info.pci_device,
        revision: info.pci_revision,
        subsys_vendor: info.subsys_vendor,
        subsys_device: info.subsys_device,
        slot_name: alloc::format!("{:04x}:{:02x}:{:02x}.{:x}", slot.domain, slot.bus, slot.dev, slot.func),
    })
}

/// `nvk trace on|off`.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn trace() -> bool {
    TRACE.load(Ordering::Relaxed) || TRACE_LEFT.load(Ordering::Relaxed) > 0
}

/// Трассировка следующих `ioctls` вызовов (потом сама гаснет): `gpu
/// gametest` включает её на vkprobe, чтобы журнал на флешке показал, на каком
/// вызове NVK споткнулся о настоящую карту, не забив кольцо журнала.
static TRACE_LEFT: AtomicU64 = AtomicU64::new(0);

pub fn trace_next(ioctls: u64) {
    TRACE_LEFT.store(ioctls, Ordering::Relaxed);
}

// ---- открытый файл ------------------------------------------------------------

/// Открытый узел рендера.
pub struct DrmFile {
    pub(super) gpu: Arc<dyn GpuBackend>,
    pub(super) st: Mutex<FileState>,
    pid: u32,
}

/// Состояние файла; порядок полей — порядок разбора при закрытии (см.
/// [`Drop`] у [`FileState`]).
pub(super) struct FileState {
    pub chans: BTreeMap<u32, Arc<nouveau::Chan>>,
    pub nvif: BTreeMap<u64, nvif::NvifObj>,
    pub vm: Option<vm::Vm>,
    /// `CHANNEL_ALLOC` до `VM_INIT` — у Linux это навсегда выключает uvmm
    /// файла; у нас каналов без VM нет, и `VM_INIT` после такого — `ENOSYS`.
    pub vm_disabled: bool,
    pub bos: BTreeMap<u32, Arc<Bo>>,
    pub next_bo: u32,
    pub syncobjs: BTreeMap<u32, Arc<sync::Syncobj>>,
    pub next_sync: u32,
}

impl FileState {
    fn new() -> Self {
        Self {
            chans: BTreeMap::new(),
            nvif: BTreeMap::new(),
            vm: None,
            vm_disabled: false,
            bos: BTreeMap::new(),
            next_bo: 1,
            syncobjs: BTreeMap::new(),
            next_sync: 1,
        }
    }

    /// Новая ручка BO (ненулевая, не занятая).
    pub fn add_bo(&mut self, bo: Arc<Bo>) -> u32 {
        while self.next_bo == 0 || self.bos.contains_key(&self.next_bo) {
            self.next_bo = self.next_bo.wrapping_add(1);
        }
        let handle = self.next_bo;
        self.next_bo = self.next_bo.wrapping_add(1);
        self.bos.insert(handle, bo);
        handle
    }

    pub fn add_syncobj(&mut self, obj: Arc<sync::Syncobj>) -> u32 {
        while self.next_sync == 0 || self.syncobjs.contains_key(&self.next_sync) {
            self.next_sync = self.next_sync.wrapping_add(1);
        }
        let handle = self.next_sync;
        self.next_sync = self.next_sync.wrapping_add(1);
        self.syncobjs.insert(handle, obj);
        handle
    }
}

impl Drop for FileState {
    /// Процесс умер или закрыл файл (риск 10 спецификации): сперва каналы —
    /// каждый ждёт свои заборы (до 2 с) и лишь потом освобождается, — затем
    /// адресное пространство (ссылки на BO из отображений уходят с ним), и
    /// только потом ручки BO: кадры не вернутся в пул, пока GPU может в них
    /// писать.
    fn drop(&mut self) {
        self.nvif.clear();
        let chans = core::mem::take(&mut self.chans);
        drop(chans);
        drop(self.vm.take());
        self.bos.clear();
        self.syncobjs.clear();
    }
}

impl sync::SyncHost for DrmFile {
    fn syncobj_get(&self, handle: u32) -> Option<Arc<sync::Syncobj>> {
        self.st.lock().syncobjs.get(&handle).cloned()
    }
    fn syncobj_add(&self, obj: Arc<sync::Syncobj>) -> u32 {
        self.st.lock().add_syncobj(obj)
    }
    fn syncobj_remove(&self, handle: u32) -> bool {
        self.st.lock().syncobjs.remove(&handle).is_some()
    }
}

impl Drop for DrmFile {
    fn drop(&mut self) {
        let st = self.st.lock();
        crate::kprintln!(
            "nvgpu: drm close pid {} (gem {} chans {} vm {})",
            self.pid,
            st.bos.len(),
            st.chans.len(),
            u8::from(st.vm.is_some())
        );
    }
}

impl Object for DrmFile {
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

/// Поставить объект в таблицу дескрипторов текущей программы.
pub(super) fn install(object: Arc<dyn Object>, nonblock: bool, cloexec: bool) -> i64 {
    match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, nonblock)?;
        program.files.set_cloexec(fd, cloexec);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    }
}

/// Объект за дескриптором текущей программы.
pub(super) fn object_of(fd: usize) -> Option<Arc<dyn Object>> {
    crate::user::with_current(|program| program.files.object(fd)).flatten().map(|(object, _)| object)
}

// ---- крючки из `linux/mod.rs` ---------------------------------------------------

/// `open("/dev/dri/renderD128")`; `None` — путь не наш или узла нет.
pub fn open(linux: &str, flags: usize) -> Option<i64> {
    const O_NONBLOCK: usize = 0o4000;
    const O_CLOEXEC: usize = 0o2_000_000;
    if linux != RENDER_NODE {
        return None;
    }
    if amdgpu::active() {
        return Some(amdgpu::open(flags));
    }
    let gpu = backend()?;
    let pid = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
    crate::kprintln!("nvgpu: drm open pid {pid}");
    let file: Arc<dyn Object> = Arc::new(DrmFile { gpu, st: Mutex::new(FileState::new()), pid });
    Some(install(file, flags & O_NONBLOCK != 0, flags & O_CLOEXEC != 0))
}

/// `st_rdev` для `stat` по пути: узел рендера, если он есть.
#[must_use]
pub fn rdev(linux: &str) -> Option<u64> {
    (linux == RENDER_NODE && node_visible()).then(render_rdev)
}

/// `st_rdev` для `fstat` открытого: `drmGetDevice2` libdrm узнаёт устройство
/// по нему.
#[must_use]
pub fn object_rdev(object: &dyn Object) -> Option<u64> {
    (object.as_any().downcast_ref::<DrmFile>().is_some() || object.as_any().downcast_ref::<amdgpu::AmdFile>().is_some())
        .then(render_rdev)
}

/// `readlink` ссылок sysfs (`…/device/subsystem` → `…/bus/pci`).
pub fn readlink(linux: &str, buf: usize, size: usize) -> Option<i64> {
    let target = sysfs::link(linux)?;
    let len = target.len().min(size);
    Some(match super::ipc::write_user(buf, &target.as_bytes()[..len]) {
        Ok(()) => len as i64,
        Err(err) => err,
    })
}

/// `mmap` дескриптора DRM или dma-buf: разделяемая память буфера и смещение
/// в ней. `None` — дескриптор не наш.
pub fn mmap_object(fd: usize, offset: u64, len: usize) -> Option<Result<(Arc<crate::user::shm::SharedMem>, u64), i64>> {
    let object = object_of(fd)?;
    if let Some(file) = object.as_any().downcast_ref::<DrmFile>() {
        return Some(gem::mmap_file(file, offset, len as u64));
    }
    if let Some(file) = object.as_any().downcast_ref::<amdgpu::AmdFile>() {
        return Some(amdgpu::mmap_file(file, offset, len as u64));
    }
    let buf = object.as_any().downcast_ref::<gem::DmaBuf>()?;
    Some(gem::mmap_bo(&buf.0, offset, len as u64))
}

/// Буфер за dma-buf fd для сервера Wayland (`zwp_linux_dmabuf_v1`, M7, WP-D):
/// разделяемая память буфера (кадры системной памяти, те же, что видит GPU)
/// и его длина в байтах. `None` — объект не dma-buf или буфер в видеопамяти
/// (процессору не виден; NVK кладёт буферы WSI в системную память).
///
/// Объект берётся из дескриптора (`files.object(fd)`) или из ручки,
/// пришедшей через `SCM_RIGHTS`. Память держит буфер живым, пока её держат.
#[must_use]
pub fn dmabuf_memory(object: &dyn Object) -> Option<(Arc<crate::user::shm::SharedMem>, u64)> {
    let buf = object.as_any().downcast_ref::<gem::DmaBuf>()?;
    let bo = buf.bo();
    Some((gem::shared_of(bo)?, bo.size))
}

/// ioctl на дескрипторе DRM, dma-buf, syncobj или sync_file; `None` — не наш.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> Option<i64> {
    let object = object_of(fd)?;
    let request = request as u32;
    if let Some(file) = object.as_any().downcast_ref::<amdgpu::AmdFile>() {
        let answer = amdgpu::ioctl(file, canonical(request), arg);
        if trace() || (answer < 0 && answer != -e::EAGAIN && answer != -ETIME) {
            log_ioctl(request, answer);
        }
        return Some(answer);
    }
    if let Some(file) = object.as_any().downcast_ref::<DrmFile>() {
        let answer = file_ioctl(file, canonical(request), arg);
        if trace() || (answer < 0 && answer != -e::EAGAIN && answer != -ETIME) {
            log_ioctl(request, answer);
        }
        return Some(answer);
    }
    if object.as_any().downcast_ref::<gem::DmaBuf>().is_some() {
        return Some(gem::dmabuf_ioctl(&object, request, arg));
    }
    if object.as_any().downcast_ref::<sync::SyncFile>().is_some() || object.as_any().downcast_ref::<sync::SyncobjFile>().is_some() {
        return Some(-e::ENOTTY);
    }
    None
}

/// Номер ioctl так, как его знает ядро. Linux (`drm_ioctl`) берёт из запроса
/// программы только тип, номер и размер, а направление копирования — из своей
/// таблицы: libdrm `drmCommandWrite` шлёт `DRM_IOW` там, где в заголовке
/// `DRM_IOWR` (20.11, QEMU: Mesa NVK звал VM_INIT как 0x40106450, а не
/// 0xc0106450, получал ENOTTY и откатывался на lavapipe). Неизвестное —
/// как есть (NVIF разбирается по одному номеру).
fn canonical(request: u32) -> u32 {
    const KNOWN: &[u32] = &[
        nvgpu::uapi::DRM_IOCTL_VERSION,
        nvgpu::uapi::DRM_IOCTL_GET_UNIQUE,
        nvgpu::uapi::DRM_IOCTL_GET_MAGIC,
        nvgpu::uapi::DRM_IOCTL_GEM_CLOSE,
        nvgpu::uapi::DRM_IOCTL_GET_CAP,
        nvgpu::uapi::DRM_IOCTL_SET_CLIENT_CAP,
        nvgpu::uapi::DRM_IOCTL_AUTH_MAGIC,
        nvgpu::uapi::DRM_IOCTL_SET_MASTER,
        nvgpu::uapi::DRM_IOCTL_DROP_MASTER,
        nvgpu::uapi::DRM_IOCTL_PRIME_HANDLE_TO_FD,
        nvgpu::uapi::DRM_IOCTL_PRIME_FD_TO_HANDLE,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_CREATE,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_DESTROY,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_WAIT,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_RESET,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_SIGNAL,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_QUERY,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_TRANSFER,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL,
        nvgpu::uapi::DRM_IOCTL_SYNCOBJ_EVENTFD,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_GETPARAM,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_CHANNEL_ALLOC,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_CHANNEL_FREE,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_VM_INIT,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_VM_BIND,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_EXEC,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_GEM_NEW,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_GEM_PUSHBUF,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_GEM_CPU_PREP,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_GEM_CPU_FINI,
        nvgpu::uapi::DRM_IOCTL_NOUVEAU_GEM_INFO,
    ];
    if (request >> 8) & 0xff != DRM_TYPE {
        return request;
    }
    let key = request & 0x3fff_ffff;
    KNOWN.iter().copied().find(|k| k & 0x3fff_ffff == key).unwrap_or(request)
}

/// Сколько строк об ошибках ioctl уже ушло в журнал (без `nvk trace` — не
/// больше [`ERROR_LOG_LIMIT`]).
static ERRORS_LOGGED: AtomicU64 = AtomicU64::new(0);
const ERROR_LOG_LIMIT: u64 = 64;

fn log_ioctl(request: u32, answer: i64) {
    let _ = TRACE_LEFT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1));
    if !trace() && ERRORS_LOGGED.fetch_add(1, Ordering::Relaxed) >= ERROR_LOG_LIMIT {
        return;
    }
    crate::kprintln!("nvgpu: drm ioctl {:#x} nr {:#x} -> {}", request, request & 0xff, answer);
}

// ---- ioctl ядра DRM -------------------------------------------------------------

/// `ETIME` (ожидание syncobj вышло по сроку).
pub(super) const ETIME: i64 = 62;
pub(super) const EOPNOTSUPP: i64 = 95;
pub(super) const EBUSY: i64 = 16;

// Номера — полностью, с направлением и размером (раздел 3.4 спецификации;
// `nvgpu::uapi` сверяет их с размерами структур при сборке).
use nvgpu::uapi::{
    DRM_IOCTL_AUTH_MAGIC as AUTH_MAGIC, DRM_IOCTL_BASE as DRM_TYPE, DRM_IOCTL_DROP_MASTER as DROP_MASTER,
    DRM_IOCTL_GEM_CLOSE as GEM_CLOSE, DRM_IOCTL_GET_CAP as GET_CAP, DRM_IOCTL_GET_MAGIC as GET_MAGIC,
    DRM_IOCTL_GET_UNIQUE as GET_UNIQUE, DRM_IOCTL_PRIME_FD_TO_HANDLE as PRIME_FD_TO_HANDLE,
    DRM_IOCTL_PRIME_HANDLE_TO_FD as PRIME_HANDLE_TO_FD, DRM_IOCTL_SET_CLIENT_CAP as SET_CLIENT_CAP,
    DRM_IOCTL_SET_MASTER as SET_MASTER, DRM_IOCTL_VERSION as VERSION, DrmGetCap, DrmVersion,
};

fn file_ioctl(file: &DrmFile, request: u32, arg: usize) -> i64 {
    match request {
        VERSION => version(arg),
        GET_UNIQUE => get_unique(arg),
        GET_CAP => get_cap(arg),
        SET_CLIENT_CAP => {
            // Все клиентские возможности — про вывод изображения (atomic,
            // universal planes …); у узла рендера их нет.
            if get::<[u64; 2]>(arg).is_err() { -e::EFAULT } else { -EOPNOTSUPP }
        }
        // Не разрешены узлу рендера (`DRM_RENDER_ALLOW` нет).
        GET_MAGIC | AUTH_MAGIC | SET_MASTER | DROP_MASTER => -e::EACCES,
        GEM_CLOSE => gem::close(file, arg),
        PRIME_HANDLE_TO_FD => gem::prime_handle_to_fd(file, arg),
        PRIME_FD_TO_HANDLE => gem::prime_fd_to_handle(file, arg),
        _ if (request >> 8) & 0xff != DRM_TYPE => -e::ENOTTY,
        _ => {
            if let Some(answer) = sync::ioctl(file, request, arg) {
                return answer;
            }
            nouveau::ioctl(file, request, arg).unwrap_or(-e::ENOTTY)
        }
    }
}

pub(super) fn get<T: Copy>(arg: usize) -> Result<T, i64> {
    super::ipc::get::<T>(arg)
}

pub(super) fn put<T: Copy>(arg: usize, value: T) -> Result<(), i64> {
    super::ipc::put(arg, value)
}

/// Массив `count` значений `T` из памяти программы.
pub(super) fn get_array<T: Copy>(ptr: u64, count: usize) -> Result<Vec<T>, i64> {
    let size = core::mem::size_of::<T>();
    let bytes = count.checked_mul(size).ok_or(-e::EINVAL)?;
    let raw = super::ipc::read_user(ptr as usize, bytes)?;
    Ok(raw
        .chunks_exact(size)
        // SAFETY: кусок ровно `size_of::<T>()` байт; `T` — POD из uAPI.
        .map(|chunk| unsafe { core::ptr::read_unaligned(chunk.as_ptr().cast::<T>()) })
        .collect())
}

/// `DRM_IOCTL_VERSION`: имя «nouveau», версия 1.4.0 (Mesa требует ≥ 1.3.1).
/// Два захода, как у `drm_copy_field`: в каждое поле — сколько влезло, а в
/// длину — полная длина строки.
fn version(arg: usize) -> i64 {
    const NAME: &[u8] = b"nouveau";
    const DATE: &[u8] = b"20120801";
    const DESC: &[u8] = b"nVidia Riva/TNT/GeForce/Quadro/Tesla/Tegra K1+";
    let Ok(mut v) = get::<DrmVersion>(arg) else { return -e::EFAULT };
    v.version_major = 1;
    v.version_minor = 4;
    v.version_patchlevel = 0;
    for (len, ptr, text) in [(&mut v.name_len, v.name, NAME), (&mut v.date_len, v.date, DATE), (&mut v.desc_len, v.desc, DESC)] {
        let take = (*len as usize).min(text.len());
        if take != 0 && ptr != 0 && super::ipc::write_user(ptr as usize, &text[..take]).is_err() {
            return -e::EFAULT;
        }
        *len = text.len() as u64;
    }
    if put(arg, v).is_err() { -e::EFAULT } else { 0 }
}

/// `DRM_IOCTL_GET_UNIQUE`: у узла рендера мастера нет — пустая строка.
fn get_unique(arg: usize) -> i64 {
    let Ok([_len, _ptr]) = get::<[u64; 2]>(arg) else { return -e::EFAULT };
    if put::<u64>(arg, 0).is_err() { -e::EFAULT } else { 0 }
}

/// `DRM_IOCTL_GET_CAP`: только то, что имеет смысл без вывода изображения.
fn get_cap(arg: usize) -> i64 {
    use nvgpu::uapi::{
        DRM_CAP_PRIME, DRM_CAP_SYNCOBJ, DRM_CAP_SYNCOBJ_TIMELINE, DRM_CAP_TIMESTAMP_MONOTONIC, DRM_PRIME_CAP_EXPORT,
        DRM_PRIME_CAP_IMPORT,
    };
    let Ok(mut cap) = get::<DrmGetCap>(arg) else { return -e::EFAULT };
    cap.value = match cap.capability {
        DRM_CAP_PRIME => DRM_PRIME_CAP_IMPORT | DRM_PRIME_CAP_EXPORT,
        DRM_CAP_TIMESTAMP_MONOTONIC | DRM_CAP_SYNCOBJ | DRM_CAP_SYNCOBJ_TIMELINE => 1,
        _ => return -e::EINVAL,
    };
    if put(arg, cap).is_err() { -e::EFAULT } else { 0 }
}
