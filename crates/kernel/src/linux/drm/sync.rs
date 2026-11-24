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
//! вопросом к бэкенду. Ожидание — короткий опрос, затем сон, который
//! будит прерывание GPU (`display::nvidia_rm_intr`, `fenceirq`) или шаг
//! таймера, если прерывания нет.

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use nvgpu::Errno;
use nvgpu::syncobj::{WaitPoll, wait_poll};

use super::nouveau::ChanCore;
use super::{EOPNOTSUPP, ETIME, get, get_array, put};
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
///
/// hw-log14: with only 1 ms sleeps (the timer tick) every wait rounded the
/// fence up to the next tick or two - DRM ioctls averaged 0.4-1.3 ms in D2R.
/// There is no GPU completion interrupt to wake on (v1), so the first
/// [`FENCE_SPIN_US`] are a poll loop that stays on the CPU (with
/// `sched_yield` between polls - a ready task still gets the CPU); a fence
/// that completes within it is seen within microseconds. Longer waits sleep
/// as before.
pub fn wait_until(deadline_ns: Option<u64>, mut ready: impl FnMut() -> Result<bool, i64>) -> Result<(), i64> {
    use crate::display::nvidia_rm_intr::{self as fenceirq, End, Mode, SRC_GPU};
    static KEY: u8 = 0;
    let started = crate::time::uptime_ns();
    let mut spin_ns = FENCE_SPIN_US.load(Ordering::Relaxed) * 1000;
    let mut slept = false;
    let mut step = FENCE_STEP_US.load(Ordering::Relaxed).max(1);
    // How the last sleep ended (GPU fence interrupt, 24.11): `None` - no
    // sleep yet or the interrupt is not in use.
    let mut last: Option<bool> = None;
    loop {
        // Read before checking: an interrupt after this read ends the sleep.
        let mode = fenceirq::mode();
        let seen = crate::sched::hrsleep::generation(SRC_GPU);
        refresh();
        if ready()? {
            let kind = usize::from(slept);
            WAITS[kind].fetch_add(1, Ordering::Relaxed);
            WAIT_NS[kind].fetch_add(crate::time::uptime_ns().saturating_sub(started), Ordering::Relaxed);
            if let Some(by_irq) = last {
                fenceirq::after_wait(if by_irq { End::Irq } else { End::TimerDone });
            }
            return Ok(());
        }
        if last.is_some() {
            fenceirq::after_wait(End::Timer);
        }
        let now = crate::time::uptime_ns();
        if deadline_ns.is_some_and(|at| at <= now) {
            return Err(-ETIME);
        }
        if crate::linux::signal::pending() || crate::user::kill_pending() {
            return Err(-e::EINTR);
        }
        // With the interrupt live a short poll is enough: the wake-up costs
        // microseconds, not a 50-200 us step.
        if mode == Mode::Live {
            spin_ns = spin_ns.min(FENCE_IRQ_SPIN_US * 1000);
        }
        if now.saturating_sub(started) < spin_ns {
            crate::sched::yield_now();
            // A poll every ~5 us: `refresh` takes the channel registry lock
            // and asks every channel.
            let next = crate::time::uptime_ns() + 5_000;
            while crate::time::uptime_ns() < next {
                core::hint::spin_loop();
            }
            continue;
        }
        slept = true;
        // hw-log15: whole-tick sleeps made every wait past the poll cost
        // 1.85 ms. Fine sleeps (hrsleep: one-shot timer) in short steps that
        // grow 50 -> 100 -> 200 us: a fence that needs 0.3 ms of GPU time
        // now costs ~0.35 ms, and a long wait costs a few wake-ups per ms.
        // hw-log17: the GPU fence interrupt (`fenceirq`) ends the sleep when
        // a channel finishes; live, the step is only a backstop.
        let step_us = FENCE_STEP_US.load(Ordering::Relaxed);
        if step_us != 0 && crate::sched::hrsleep::enabled() {
            let now = crate::time::uptime_ns();
            let (len_us, sources) = match mode {
                Mode::Live => (FENCE_IRQ_BACKSTOP_US, SRC_GPU),
                Mode::Trial => (step, SRC_GPU),
                Mode::Off => (step, 0),
            };
            let mut at = now + len_us * 1000;
            if let Some(limit) = deadline_ns {
                at = at.min(limit);
            }
            FINE_SLEEPS.fetch_add(1, Ordering::Relaxed);
            crate::sched::hrsleep::sleep_until_ns(at, sources, seen, || false);
            last = (sources != 0).then(|| crate::sched::hrsleep::generation(SRC_GPU) != seen);
            if mode != Mode::Live {
                step = (step * 2).min(step_us * 4);
            }
            continue;
        }
        last = None;
        let until = crate::time::uptime_ms() + 1;
        crate::sched::block_on_futex(core::ptr::addr_of!(KEY) as usize, Some(until), || false);
    }
}

/// Poll window of a fence wait while the GPU fence interrupt is live, us.
const FENCE_IRQ_SPIN_US: u64 = 20;
/// Backstop sleep of a fence wait while the interrupt is live, us (a missed
/// interrupt costs at most this; `fenceirq` counts such waits and falls
/// back if they are many).
const FENCE_IRQ_BACKSTOP_US: u64 = 1000;

/// How long a fence wait polls before it sleeps, us (`fencespin N`, 0 - sleep
/// at once, as before).
pub static FENCE_SPIN_US: AtomicU64 = AtomicU64::new(200);
/// First fine sleep step after the poll, us; doubles up to 4x (`fencestep N`;
/// 0 - 1 ms tick sleeps as before 22.11).
pub static FENCE_STEP_US: AtomicU64 = AtomicU64::new(50);
/// Fine sleep steps taken by fence waits (perf-diag).
static FINE_SLEEPS: AtomicU64 = AtomicU64::new(0);

/// Fine sleep steps of fence waits since boot.
#[must_use]
pub fn fence_fine_sleeps() -> u64 {
    FINE_SLEEPS.load(Ordering::Relaxed)
}

/// `hrbench` (kernel shell): `count` waits through [`wait_until`] on a
/// "fence" that signals `signal_us` after the wait began, with fine step
/// `step_us` (0 - tick sleeps as before 22.11). Returns `(average, worst)`
/// wait length, us: how late a fence wait sees its fence.
pub fn fence_bench(signal_us: u64, count: u32, step_us: u64) -> (u64, u64) {
    let saved = FENCE_STEP_US.swap(step_us, Ordering::Relaxed);
    let (mut sum, mut worst) = (0u64, 0u64);
    for _ in 0..count {
        let started = crate::time::uptime_ns();
        let at = started + signal_us * 1000;
        let _ = wait_until(None, || Ok(crate::time::uptime_ns() >= at));
        let took = crate::time::uptime_ns() - started;
        sum += took;
        worst = worst.max(took);
    }
    FENCE_STEP_US.store(saved, Ordering::Relaxed);
    (sum / u64::from(count.max(1)) / 1000, worst / 1000)
}
/// Finished fence waits: [done while polling, done after sleeping] and their
/// total time, ns.
static WAITS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static WAIT_NS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];

/// Fence waits since boot: `(done while polling, their ns, done after
/// sleeping, their ns)` - for perf-diag.
#[must_use]
pub fn fence_wait_stats() -> (u64, u64, u64, u64) {
    (
        WAITS[0].load(Ordering::Relaxed),
        WAIT_NS[0].load(Ordering::Relaxed),
        WAITS[1].load(Ordering::Relaxed),
        WAIT_NS[1].load(Ordering::Relaxed),
    )
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

/// Файл узла, у которого есть таблица syncobj (`DrmFile` у NVIDIA, `AmdFile` у AMD):
/// ioctl syncobj ниже одинаковы для любого драйвера DRM.
pub trait SyncHost {
    fn syncobj_get(&self, handle: u32) -> Option<Arc<Syncobj>>;
    fn syncobj_add(&self, obj: Arc<Syncobj>) -> u32;
    fn syncobj_remove(&self, handle: u32) -> bool;
}

/// ioctl syncobj; `None` — номер не отсюда.
pub fn ioctl(file: &dyn SyncHost, request: u32, arg: usize) -> Option<i64> {
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
pub fn lookup(file: &dyn SyncHost, handle: u32) -> Result<Arc<Syncobj>, i64> {
    file.syncobj_get(handle).ok_or(-e::ENOENT)
}

fn lookup_all(file: &dyn SyncHost, handles: &[u32]) -> Result<Vec<Arc<Syncobj>>, i64> {
    handles.iter().map(|&h| file.syncobj_get(h).ok_or(-e::ENOENT)).collect()
}

fn create(file: &dyn SyncHost, arg: usize) -> i64 {
    let Ok(DrmSyncobjCreate { flags, .. }) = get::<DrmSyncobjCreate>(arg) else { return -e::EFAULT };
    if flags & !CREATE_SIGNALED != 0 {
        return -e::EINVAL;
    }
    let handle = file.syncobj_add(Syncobj::new(flags & CREATE_SIGNALED != 0));
    if put(arg, handle).is_err() {
        file.syncobj_remove(handle);
        return -e::EFAULT;
    }
    0
}

fn destroy(file: &dyn SyncHost, arg: usize) -> i64 {
    let Ok(DrmSyncobjDestroy { handle, pad }) = get::<DrmSyncobjDestroy>(arg) else { return -e::EFAULT };
    if pad != 0 {
        return -e::EINVAL;
    }
    if file.syncobj_remove(handle) { 0 } else { -e::EINVAL }
}

fn handle_to_fd(file: &dyn SyncHost, arg: usize) -> i64 {
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

fn fd_to_handle(file: &dyn SyncHost, arg: usize) -> i64 {
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
    let handle = file.syncobj_add(Arc::clone(&shared.0));
    args.handle = handle;
    if put(arg, args).is_err() {
        file.syncobj_remove(handle);
        return -e::EFAULT;
    }
    0
}

/// `struct drm_syncobj_wait` (40 байт) и `drm_syncobj_timeline_wait` (48):
/// у второго после `handles` — `points`.
fn wait(file: &dyn SyncHost, arg: usize, timeline: bool) -> i64 {
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
fn reset_signal(file: &dyn SyncHost, arg: usize, signal: bool) -> i64 {
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
fn timeline_args(file: &dyn SyncHost, arg: usize) -> Result<(Vec<Arc<Syncobj>>, Vec<u64>, u32, u64), i64> {
    let TimelineArrayArgs { handles: handles_ptr, points: points_ptr, count_handles: count, flags } = get::<TimelineArrayArgs>(arg).map_err(|_| -e::EFAULT)?;
    if count == 0 || count as usize > MAX_HANDLES {
        return Err(-e::EINVAL);
    }
    let handles = get_array::<u32>(handles_ptr, count as usize)?;
    let points = get_array::<u64>(points_ptr, count as usize)?;
    Ok((lookup_all(file, &handles)?, points, flags, points_ptr))
}

fn timeline_signal(file: &dyn SyncHost, arg: usize) -> i64 {
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

fn query(file: &dyn SyncHost, arg: usize) -> i64 {
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

fn transfer(file: &dyn SyncHost, arg: usize) -> i64 {
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
