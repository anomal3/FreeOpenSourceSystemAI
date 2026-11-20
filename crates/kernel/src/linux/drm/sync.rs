//! Заборы и объекты синхронизации DRM (`drm_syncobj`, раздел 1.5
//! спецификации): то, чем NVK ждёт GPU и чем Vulkan-семафоры (двоичные и
//! timeline) передаются между очередями и процессами.
//!
//! # Модель
//!
//! Учёт — `nvgpu::syncobj` (WP-B, проверен на хосте): забор — набор пар
//! «контекст, номер», точки шкалы, правила `WAIT_ALL/FOR_SUBMIT/AVAILABLE`
//! Linux. Здесь — сон, замки и ответ на «выполнен ли номер N канала C».
//!
//! Контекст — номер канала в реестре ([`register`]): реестр держит слабые
//! ссылки, так что syncobj, переживший канал, канал не держит (снятый канал
//! — «всё выполнено», как сигнальный с ошибкой забор Linux). Ответ берётся
//! из кэша канала ([`ChanCore::completed`]): его можно спрашивать под
//! замком объекта, а освежают его ожидания ([`refresh`]) — вне замков,
//! вопросом к бэкенду. Прерываний нет (v1): ожидание — опрос раз в
//! миллисекунду.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use nvgpu::Errno;
use nvgpu::syncobj::{WaitPoll, wait_poll};

use super::nouveau::ChanCore;
use super::{DrmFile, EOPNOTSUPP, ETIME, get, get_array, put};
use crate::linux::e;
use crate::sync::SpinLock;
use crate::user::files::Object;

pub use nvgpu::syncobj::Fence;

// ---- реестр каналов ----------------------------------------------------------------

static CHANS: SpinLock<BTreeMap<u64, Weak<ChanCore>>> = SpinLock::new(BTreeMap::new());
static NEXT_CTX: AtomicU64 = AtomicU64::new(1);

/// Новый номер контекста заборов.
pub fn new_ctx() -> u64 {
    NEXT_CTX.fetch_add(1, Ordering::Relaxed)
}

/// Записать канал в реестр (после создания).
pub fn register(chan: &Arc<ChanCore>) {
    CHANS.lock().insert(chan.ctx, Arc::downgrade(chan));
}

/// Убрать канал из реестра (он снят: его заборы выполнены).
pub fn unregister(ctx: u64) {
    CHANS.lock().remove(&ctx);
}

/// Освежить кэш выполненных номеров всех каналов — вопросом к бэкенду, без
/// замков.
pub fn refresh() {
    let live: Vec<Arc<ChanCore>> = CHANS.lock().values().filter_map(Weak::upgrade).collect();
    for chan in live {
        chan.poll();
    }
}

/// «Номер `seq` контекста `ctx` выполнен» — по кэшу; можно под замками.
pub fn done(ctx: u64, seq: u64) -> Result<bool, Errno> {
    let chan = CHANS.lock().get(&ctx).and_then(Weak::upgrade);
    Ok(chan.is_none_or(|chan| chan.cached_done(seq)))
}

/// Выполнен ли забор (по кэшу; освежить — [`refresh`]).
#[must_use]
pub fn signaled(fence: &Fence) -> bool {
    fence.is_signaled(&done)
}

/// Забор номера `seq` канала.
#[must_use]
pub fn chan_fence(chan: &ChanCore, seq: u64) -> Fence {
    Fence::seq(chan.ctx, seq)
}

/// Все ли зависимости забора — на этом канале (порядок даёт GPFIFO).
#[must_use]
pub fn ordered_on(fence: &Fence, chan: &ChanCore) -> bool {
    fence.deps.iter().all(|(ctx, _)| *ctx == chan.ctx)
}

// ---- объект ------------------------------------------------------------------------

/// Объект синхронизации: учёт `nvgpu` под замком.
pub struct Syncobj {
    st: SpinLock<nvgpu::syncobj::Syncobj>,
}

impl Syncobj {
    #[must_use]
    pub fn new(signaled: bool) -> Arc<Self> {
        Arc::new(Self { st: SpinLock::new(nvgpu::syncobj::Syncobj::new(signaled)) })
    }

    /// Двоичная замена забора.
    pub fn replace(&self, fence: Option<Fence>) {
        self.st.lock().replace(fence);
    }

    /// `RESET`: ни забора, ни шкалы.
    pub fn reset(&self) {
        self.st.lock().reset();
    }

    /// Точка шкалы (0 — замена забора). Заодно свернуть сигнальную голову:
    /// иначе шкала семафора Vulkan росла бы без конца.
    pub fn add_point(&self, point: u64, fence: Fence) {
        refresh();
        let mut st = self.st.lock();
        st.add_point(point, fence);
        st.collect(&done);
    }

    /// Забор точки; `None` — ещё не подана.
    #[must_use]
    pub fn fence(&self, point: u64) -> Option<Fence> {
        self.st.lock().fence_for(point)
    }

    /// Снимок учёта (опрос ожидания идёт по нему, без замка).
    fn snapshot(&self) -> nvgpu::syncobj::Syncobj {
        self.st.lock().clone()
    }

    /// `QUERY`: сигнальное значение шкалы или наибольшая поданная точка.
    #[must_use]
    pub fn query(&self, last_submitted: bool) -> u64 {
        refresh();
        let mut st = self.st.lock();
        if last_submitted { st.last_submitted() } else { st.collect(&done) }
    }

    /// `TRANSFER`: точка источника — в точку здесь.
    pub fn transfer_from(&self, src: &Syncobj, src_point: u64, dst_point: u64) -> Result<(), i64> {
        let src = src.snapshot();
        self.st.lock().transfer_from(&src, src_point, dst_point).map_err(|err| -i64::from(err.0))
    }
}

// ---- дескрипторы -----------------------------------------------------------------

/// Непрозрачный fd объекта синхронизации (`HANDLE_TO_FD` без флагов).
pub struct SyncobjFile(pub Arc<Syncobj>);

/// `sync_file`: один забор; читаем (`poll`), когда сигнален.
pub struct SyncFile(pub Fence);

impl Object for SyncobjFile {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (false, false, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Object for SyncFile {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        refresh();
        (signaled(&self.0), false, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    /// Прерываний нет — `poll` перепроверяет забор каждую миллисекунду.
    fn deadline(&self) -> Option<u64> {
        (!signaled(&self.0)).then(|| crate::time::uptime_ms() + 1)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- ожидание ------------------------------------------------------------------

/// Ждать, пока `ready` не ответит `true`, до абсолютного срока `deadline_ns`
/// по `CLOCK_MONOTONIC` (`None` — без срока). Сигнал или снятие процесса —
/// `EINTR` (Linux: `ERESTARTSYS`); срок — `ETIME`.
pub fn wait_until(deadline_ns: Option<u64>, mut ready: impl FnMut() -> Result<bool, i64>) -> Result<(), i64> {
    static KEY: u8 = 0;
    loop {
        refresh();
        if ready()? {
            return Ok(());
        }
        let now = crate::time::uptime_ns();
        if deadline_ns.is_some_and(|at| at <= now) {
            return Err(-ETIME);
        }
        if crate::linux::signal::pending() || crate::user::kill_pending() {
            return Err(-e::EINTR);
        }
        let until = crate::time::uptime_ms() + 1;
        crate::sched::block_on_futex(core::ptr::addr_of!(KEY) as usize, Some(until), || false);
    }
}

/// Срок Linux (`timeout_nsec`, абсолютный монотонный) → наш: `i64::MAX` и
/// больше — без срока.
fn deadline(timeout: i64) -> Option<u64> {
    if timeout == i64::MAX { None } else { Some(timeout.max(0) as u64) }
}

// ---- ioctl ---------------------------------------------------------------------

use nvgpu::uapi::{
    DRM_IOCTL_SYNCOBJ_CREATE as CREATE, DRM_IOCTL_SYNCOBJ_DESTROY as DESTROY, DRM_IOCTL_SYNCOBJ_EVENTFD as EVENTFD,
    DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE as FD_TO_HANDLE, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD as HANDLE_TO_FD,
    DRM_IOCTL_SYNCOBJ_QUERY as QUERY, DRM_IOCTL_SYNCOBJ_RESET as RESET, DRM_IOCTL_SYNCOBJ_SIGNAL as SIGNAL,
    DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL as TIMELINE_SIGNAL, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT as TIMELINE_WAIT,
    DRM_IOCTL_SYNCOBJ_TRANSFER as TRANSFER, DRM_IOCTL_SYNCOBJ_WAIT as WAIT,
    DRM_SYNCOBJ_CREATE_SIGNALED as CREATE_SIGNALED, DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE as IMPORT_SYNC_FILE,
    DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE as EXPORT_SYNC_FILE,
    DRM_SYNCOBJ_QUERY_FLAGS_LAST_SUBMITTED as QUERY_LAST_SUBMITTED, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL as WAIT_ALL,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE as WAIT_AVAILABLE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_DEADLINE as WAIT_DEADLINE,
    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT as WAIT_FOR_SUBMIT, DrmSyncobjArray as ArrayArgs, DrmSyncobjCreate,
    DrmSyncobjDestroy, DrmSyncobjHandle as HandleArgs, DrmSyncobjTimelineArray as TimelineArrayArgs,
    DrmSyncobjTimelineWait as TimelineWaitArgs, DrmSyncobjTransfer as TransferArgs, DrmSyncobjWait as WaitArgs,
};

/// Самое большее объектов в одном ожидании — у Linux предела нет, у нас
/// разумный.
const MAX_HANDLES: usize = 4096;

/// ioctl syncobj; `None` — номер не отсюда.
pub fn ioctl(file: &DrmFile, request: u32, arg: usize) -> Option<i64> {
    Some(match request {
        CREATE => create(file, arg),
        DESTROY => destroy(file, arg),
        HANDLE_TO_FD => handle_to_fd(file, arg),
        FD_TO_HANDLE => fd_to_handle(file, arg),
        WAIT => wait(file, arg, false),
        TIMELINE_WAIT => wait(file, arg, true),
        RESET => reset_signal(file, arg, false),
        SIGNAL => reset_signal(file, arg, true),
        QUERY => query(file, arg),
        TIMELINE_SIGNAL => timeline_signal(file, arg),
        TRANSFER => transfer(file, arg),
        EVENTFD => -EOPNOTSUPP,
        _ => return None,
    })
}

/// Объект по ручке файла.
pub fn lookup(file: &DrmFile, handle: u32) -> Result<Arc<Syncobj>, i64> {
    file.st.lock().syncobjs.get(&handle).cloned().ok_or(-e::ENOENT)
}

fn lookup_all(file: &DrmFile, handles: &[u32]) -> Result<Vec<Arc<Syncobj>>, i64> {
    let st = file.st.lock();
    handles.iter().map(|h| st.syncobjs.get(h).cloned().ok_or(-e::ENOENT)).collect()
}

fn create(file: &DrmFile, arg: usize) -> i64 {
    let Ok(DrmSyncobjCreate { flags, .. }) = get::<DrmSyncobjCreate>(arg) else { return -e::EFAULT };
    if flags & !CREATE_SIGNALED != 0 {
        return -e::EINVAL;
    }
    let handle = file.st.lock().add_syncobj(Syncobj::new(flags & CREATE_SIGNALED != 0));
    if put(arg, handle).is_err() {
        file.st.lock().syncobjs.remove(&handle);
        return -e::EFAULT;
    }
    0
}

fn destroy(file: &DrmFile, arg: usize) -> i64 {
    let Ok(DrmSyncobjDestroy { handle, pad }) = get::<DrmSyncobjDestroy>(arg) else { return -e::EFAULT };
    if pad != 0 {
        return -e::EINVAL;
    }
    if file.st.lock().syncobjs.remove(&handle).is_some() { 0 } else { -e::EINVAL }
}

fn handle_to_fd(file: &DrmFile, arg: usize) -> i64 {
    let Ok(mut args) = get::<HandleArgs>(arg) else { return -e::EFAULT };
    if args.pad != 0 || args.flags & !EXPORT_SYNC_FILE != 0 {
        return -e::EINVAL;
    }
    let obj = match lookup(file, args.handle) {
        Ok(obj) => obj,
        Err(err) => return err,
    };
    let object: Arc<dyn Object> = if args.flags & EXPORT_SYNC_FILE != 0 {
        match obj.fence(0) {
            Some(fence) => Arc::new(SyncFile(fence)),
            None => return -e::EINVAL,
        }
    } else {
        Arc::new(SyncobjFile(obj))
    };
    let fd = super::install(object, false, true);
    if fd < 0 {
        return fd;
    }
    args.fd = fd as i32;
    if put(arg, args).is_err() { -e::EFAULT } else { 0 }
}

fn fd_to_handle(file: &DrmFile, arg: usize) -> i64 {
    let Ok(mut args) = get::<HandleArgs>(arg) else { return -e::EFAULT };
    if args.pad != 0 || args.flags & !IMPORT_SYNC_FILE != 0 {
        return -e::EINVAL;
    }
    let Some(object) = super::object_of(args.fd as u32 as usize) else { return -e::EINVAL };
    if args.flags & IMPORT_SYNC_FILE != 0 {
        let Some(sync) = object.as_any().downcast_ref::<SyncFile>() else { return -e::EINVAL };
        return match lookup(file, args.handle) {
            Ok(obj) => {
                obj.replace(Some(sync.0.clone()));
                0
            }
            Err(err) => err,
        };
    }
    let Some(shared) = object.as_any().downcast_ref::<SyncobjFile>() else { return -e::EINVAL };
    let handle = file.st.lock().add_syncobj(Arc::clone(&shared.0));
    args.handle = handle;
    if put(arg, args).is_err() {
        file.st.lock().syncobjs.remove(&handle);
        return -e::EFAULT;
    }
    0
}

/// `struct drm_syncobj_wait` (40 байт) и `drm_syncobj_timeline_wait` (48):
/// у второго после `handles` — `points`.
fn wait(file: &DrmFile, arg: usize, timeline: bool) -> i64 {
    let (handles_ptr, points_ptr, timeout, count, flags, at_first) = if timeline {
        let Ok(w) = get::<TimelineWaitArgs>(arg) else { return -e::EFAULT };
        (w.handles, w.points, w.timeout_nsec, w.count_handles, w.flags, 32)
    } else {
        let Ok(w) = get::<WaitArgs>(arg) else { return -e::EFAULT };
        (w.handles, 0, w.timeout_nsec, w.count_handles, w.flags, 24)
    };
    let allowed = WAIT_ALL | WAIT_FOR_SUBMIT | WAIT_DEADLINE | if timeline { WAIT_AVAILABLE } else { 0 };
    if flags & !allowed != 0 {
        return -e::EINVAL;
    }
    let count = count as usize;
    if count == 0 {
        return -e::EINVAL;
    }
    if count > MAX_HANDLES {
        return -e::EINVAL;
    }
    let handles = match get_array::<u32>(handles_ptr, count) {
        Ok(handles) => handles,
        Err(err) => return err,
    };
    let points = if timeline {
        match get_array::<u64>(points_ptr, count) {
            Ok(points) => points,
            Err(err) => return err,
        }
    } else {
        alloc::vec![0; count]
    };
    let objs = match lookup_all(file, &handles) {
        Ok(objs) => objs,
        Err(err) => return err,
    };
    // Опрос — `nvgpu::syncobj::wait_poll` по снимкам объектов: без
    // `WAIT_FOR_SUBMIT` не поданная точка — сразу `EINVAL`.
    let mut first = 0u32;
    let result = wait_until(deadline(timeout), || {
        let snaps: Vec<_> = objs.iter().map(|obj| obj.snapshot()).collect();
        let refs: Vec<&nvgpu::syncobj::Syncobj> = snaps.iter().collect();
        match wait_poll(&refs, &points, flags, &done) {
            Ok(WaitPoll::Ready { first: at }) => {
                first = at;
                Ok(true)
            }
            Ok(WaitPoll::Pending) => Ok(false),
            Err(err) => Err(-i64::from(err.0)),
        }
    });
    match result {
        Ok(()) => {
            if put(arg + at_first, first).is_err() { -e::EFAULT } else { 0 }
        }
        Err(err) => err,
    }
}

/// `RESET`/`SIGNAL`.
fn reset_signal(file: &DrmFile, arg: usize, signal: bool) -> i64 {
    let Ok(ArrayArgs { handles: ptr, count_handles: count, pad }) = get::<ArrayArgs>(arg) else { return -e::EFAULT };
    if pad != 0 {
        return -e::EINVAL;
    }
    if count == 0 || count as usize > MAX_HANDLES {
        return -e::EINVAL;
    }
    let handles = match get_array::<u32>(ptr, count as usize) {
        Ok(handles) => handles,
        Err(err) => return err,
    };
    let objs = match lookup_all(file, &handles) {
        Ok(objs) => objs,
        Err(err) => return err,
    };
    for obj in objs {
        if signal {
            obj.replace(Some(Fence::signaled()));
        } else {
            obj.reset();
        }
    }
    0
}

/// Ручки, точки, флаги и адрес точек `drm_syncobj_timeline_array`.
fn timeline_args(file: &DrmFile, arg: usize) -> Result<(Vec<Arc<Syncobj>>, Vec<u64>, u32, u64), i64> {
    let TimelineArrayArgs { handles: handles_ptr, points: points_ptr, count_handles: count, flags } = get::<TimelineArrayArgs>(arg).map_err(|_| -e::EFAULT)?;
    if count == 0 || count as usize > MAX_HANDLES {
        return Err(-e::EINVAL);
    }
    let handles = get_array::<u32>(handles_ptr, count as usize)?;
    let points = get_array::<u64>(points_ptr, count as usize)?;
    Ok((lookup_all(file, &handles)?, points, flags, points_ptr))
}

fn timeline_signal(file: &DrmFile, arg: usize) -> i64 {
    let (objs, points, flags, _) = match timeline_args(file, arg) {
        Ok(args) => args,
        Err(err) => return err,
    };
    if flags != 0 {
        return -e::EINVAL;
    }
    for (obj, point) in objs.iter().zip(points) {
        obj.add_point(point, Fence::signaled());
    }
    0
}

fn query(file: &DrmFile, arg: usize) -> i64 {
    let (objs, _, flags, points_ptr) = match timeline_args(file, arg) {
        Ok(args) => args,
        Err(err) => return err,
    };
    if flags & !QUERY_LAST_SUBMITTED != 0 {
        return -e::EINVAL;
    }
    let values: Vec<u8> = objs.iter().flat_map(|obj| obj.query(flags & QUERY_LAST_SUBMITTED != 0).to_ne_bytes()).collect();
    match crate::linux::ipc::write_user(points_ptr as usize, &values) {
        Ok(()) => 0,
        Err(err) => err,
    }
}

fn transfer(file: &DrmFile, arg: usize) -> i64 {
    let Ok(TransferArgs { src_handle: src, dst_handle: dst, src_point, dst_point, flags, pad }) = get::<TransferArgs>(arg) else { return -e::EFAULT };
    if pad != 0 || flags & !WAIT_FOR_SUBMIT != 0 {
        return -e::EINVAL;
    }
    let (src, dst) = match (lookup(file, src), lookup(file, dst)) {
        (Ok(src), Ok(dst)) => (src, dst),
        (Err(err), _) | (_, Err(err)) => return err,
    };
    if flags & WAIT_FOR_SUBMIT != 0 {
        if let Err(err) = wait_until(None, || Ok(src.fence(src_point).is_some())) {
            return err;
        }
    }
    match dst.transfer_from(&src, src_point, dst_point) {
        Ok(()) => 0,
        Err(err) => err,
    }
}
