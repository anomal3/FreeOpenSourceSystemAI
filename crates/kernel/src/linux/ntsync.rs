//! `/dev/ntsync` для программ Linux: объекты синхронизации Windows в ядре
//! (16.11.2026, путь к Proton без fsync).
//!
//! # Зачем
//!
//! Wine 11 (и GE-Proton11-12) держит события, мьютексы и семафоры Windows
//! не в wineserver и не в словах памяти (fsync), а в драйвере ядра
//! `drivers/misc/ntsync.c`: `WaitForMultipleObjects` — один `ioctl`, без
//! похода к серверу. wineserver открывает устройство при первом потоке и,
//! если `open` удался, **отката уже нет**: на каждый поток он делает
//! `CREATE_EVENT` (событие «алерта»), на каждый объект Windows — `CREATE_*`, и
//! любая ошибка ломает программу. Поэтому здесь — весь интерфейс mainline
//! (`include/uapi/linux/ntsync.h`) с той же семантикой, а на случай беды —
//! выключатель [`set_enabled`]: выключенное устройство не существует
//! (`ENOENT`), и Wine идёт старым путём (fsync или сервер).
//!
//! # Как устроено
//!
//! Каждый `open("/dev/ntsync")` — своё [`Device`] со своим `SpinLock`; fd
//! устройства wineserver отдаёт клиентам через `SCM_RIGHTS`, и ожидание
//! проверяет, что все объекты — с того же устройства (как `obj->dev == dev`).
//! Состояние всех объектов устройства и очереди их ждущих лежат под одним
//! локом: так `WAIT_ALL` атомарен даром, без `wait_all_lock`/`all_hint`
//! mainline (та сложность — ради масштабирования на сотни процессоров).
//!
//! Объект за fd — [`NtObj`]: ссылка на устройство и номер ячейки в его
//! таблице. Ячейка живёт, пока жив хоть один `Arc` объекта: fd, `dup`,
//! `SCM_RIGHTS` — и ждущий, который на время ожидания держит свою ссылку
//! (закрытый посреди ожидания fd не роняет ожидание, как в Linux).
//!
//! Ждущий — [`Waiter`]: его задача, `owner` (tid Windows для мьютексов) и
//! атомарный `signaled` (−1 — ещё не победил, иначе индекс объекта). Будящая
//! сторона под локом устройства выбирает победителя (CAS −1 → индекс), сразу
//! «захватывает» за него объект и зовёт [`crate::sched::wake`]. Сам ждущий
//! спит в [`crate::sched::block_on_futex`] с ключом — адресом своего
//! `Waiter`, а условие пробуждения читает только этот атомик: лок устройства
//! внутри лока планировщика не берётся никогда (порядок: устройство →
//! планировщик). Потерять пробуждение нельзя: `signaled` ставится до `wake`,
//! условие проверяется под локом планировщика, а занятый планировщик кладёт
//! `wake` в отложенную очередь.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use super::e;
use super::ipc::{get, put, read_user};
use crate::sched::TaskId;
use crate::sync::SpinLock;
use crate::user::files::Object;

pub const PATH: &str = "/dev/ntsync";

/// Включено ли устройство после загрузки. Проба `ntsyncprobe` проходит, но
/// 16.11 с настоящим Wine (Trackmania на живой флешке) `wineboot` трижды не
/// приготовил префикс («could not load kernel32.dll», один процесс снят #DF),
/// а без ntsync тот же шаг проходит. Пока не разобрано — выключено; включить
/// командой оболочки `ntsync on` (Proton берёт его сам, fsync — запасной).
pub const DEFAULT_ENABLED: bool = false;

static ENABLED: AtomicBool = AtomicBool::new(DEFAULT_ENABLED);

/// Показать или спрятать `/dev/ntsync` (команда оболочки `ntsync on|off`).
/// Уже открытые устройства и объекты продолжают работать: прятать их у
/// живого wineserver — значит сломать его посреди работы.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `EOWNERDEAD`: мьютекс брошен (поток-владелец умер, `MUTEX_KILL`).
const EOWNERDEAD: i64 = 130;

/// Больше объектов в одном ожидании нельзя (`NTSYNC_MAX_WAIT_COUNT`).
const MAX_WAIT_COUNT: usize = 64;
/// Срок ожидания по `CLOCK_REALTIME`, а не `CLOCK_MONOTONIC`.
const WAIT_REALTIME: u32 = 1;

// Номера ioctl — полностью, с направлением и размером: не тот размер у
// mainline — «нет такого вызова» (`ENOTTY`), так же и здесь.
// На fd устройства:
const CREATE_SEM: u32 = 0x4008_4E80;
const WAIT_ANY: u32 = 0xC028_4E82;
const WAIT_ALL: u32 = 0xC028_4E83;
const CREATE_MUTEX: u32 = 0x4008_4E84;
const CREATE_EVENT: u32 = 0x4008_4E87;
// На fd объекта:
const SEM_RELEASE: u32 = 0xC004_4E81;
const MUTEX_UNLOCK: u32 = 0xC008_4E85;
const MUTEX_KILL: u32 = 0x4004_4E86;
const EVENT_SET: u32 = 0x8004_4E88;
const EVENT_RESET: u32 = 0x8004_4E89;
const EVENT_PULSE: u32 = 0x8004_4E8A;
const SEM_READ: u32 = 0x8008_4E8B;
const MUTEX_READ: u32 = 0x8008_4E8C;
const EVENT_READ: u32 = 0x8008_4E8D;

/// Есть ли такой путь (для `stat`/`access`).
#[must_use]
pub fn exists(linux: &str) -> bool {
    linux == PATH && enabled()
}

/// Открыть `/dev/ntsync`; `None` — путь не наш (или устройство выключено:
/// тогда дальше по цепочке путей его никто не знает — `ENOENT`).
pub fn open(linux: &str, flags: usize) -> Option<i64> {
    const O_NONBLOCK: usize = 0o4000;
    const O_CLOEXEC: usize = 0o2_000_000;
    if !exists(linux) {
        return None;
    }
    let device = Arc::new(Device { state: SpinLock::new(State { objs: Vec::new(), free: Vec::new() }) });
    let object: Arc<dyn Object> = Arc::new(DevFile(device));
    Some(install(object, flags & O_NONBLOCK != 0, flags & O_CLOEXEC != 0))
}

fn install(object: Arc<dyn Object>, nonblock: bool, cloexec: bool) -> i64 {
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

/// ioctl на fd устройства или объекта; `None` — дескриптор не наш.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> Option<i64> {
    let (object, _) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    // `cmd` у Linux — `unsigned int`: старшая половина регистра не в счёт.
    let request = request as u32;
    if let Some(dev) = object.as_any().downcast_ref::<DevFile>() {
        return Some(match request {
            CREATE_SEM => create_sem(&dev.0, arg),
            CREATE_MUTEX => create_mutex(&dev.0, arg),
            CREATE_EVENT => create_event(&dev.0, arg),
            WAIT_ANY => wait(&dev.0, arg, false),
            WAIT_ALL => wait(&dev.0, arg, true),
            _ => -e::ENOTTY,
        });
    }
    let obj = object.as_any().downcast_ref::<NtObj>()?;
    Some(match request {
        SEM_RELEASE => obj.sem_release(arg),
        MUTEX_UNLOCK => obj.mutex_unlock(arg),
        MUTEX_KILL => obj.mutex_kill(arg),
        EVENT_SET => obj.event_set(arg, false),
        EVENT_PULSE => obj.event_set(arg, true),
        EVENT_RESET => obj.event_reset(arg),
        SEM_READ => obj.sem_read(arg),
        MUTEX_READ => obj.mutex_read(arg),
        EVENT_READ => obj.event_read(arg),
        _ => -e::ENOTTY,
    })
}

// ---- устройство и его состояние --------------------------------------------------

struct Device {
    state: SpinLock<State>,
}

/// Объекты устройства: ячейки по номерам и список свободных номеров.
struct State {
    objs: Vec<Option<Slot>>,
    free: Vec<usize>,
}

/// Объект и его ждущие.
struct Slot {
    kind: Kind,
    /// Ждущие «любого» (и алерт `WAIT_ALL`) — в порядке постановки: будятся
    /// по очереди, пока объект сигнален (справедливость mainline).
    any: VecDeque<(Arc<Waiter>, u32)>,
    /// Ждущие «всех сразу».
    all: Vec<Arc<Waiter>>,
}

#[derive(Clone, Copy)]
enum Kind {
    Sem { count: u32, max: u32 },
    /// `owner` — tid Windows; мьютекс рекурсивен по нему (`count` — глубина).
    /// `dead` — брошен: ближайший захват вернёт `EOWNERDEAD`.
    Mutex { count: u32, owner: u32, dead: bool },
    Event { manual: bool, signaled: bool },
}

struct Waiter {
    task: TaskId,
    owner: u32,
    /// −1 — ещё не победил; иначе индекс объекта (алерт — `count`).
    signaled: AtomicI32,
    /// Захваченный мьютекс оказался брошенным.
    ownerdead: AtomicBool,
    /// Ячейки объектов по индексам; алерт, если есть, — последним.
    ids: Vec<usize>,
    /// Сколько из `ids` — объекты (без алерта).
    count: usize,
}

impl Kind {
    /// Можно ли захватить объект владельцу `owner`.
    fn signaled(&self, owner: u32) -> bool {
        match *self {
            Self::Sem { count, .. } => count != 0,
            Self::Mutex { count, owner: held, .. } => (held == 0 || held == owner) && count < u32::MAX,
            Self::Event { signaled, .. } => signaled,
        }
    }

    /// Захватить за ждущего (сигнальность проверена).
    fn acquire(&mut self, waiter: &Waiter) {
        match self {
            Self::Sem { count, .. } => *count -= 1,
            Self::Mutex { count, owner, dead } => {
                if *dead {
                    waiter.ownerdead.store(true, Ordering::Relaxed);
                }
                *dead = false;
                *count += 1;
                *owner = waiter.owner;
            }
            Self::Event { manual, signaled } => {
                if !*manual {
                    *signaled = false;
                }
            }
        }
    }
}

impl Waiter {
    /// Стать победителем с индексом `index`; `false` — уже победил раньше.
    fn win(&self, index: u32) -> bool {
        self.signaled.compare_exchange(-1, index as i32, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
}

impl State {
    fn slot(&mut self, id: usize) -> &mut Slot {
        // Ячейка жива, пока жив хоть один `NtObj` с этим номером, а номер
        // знают только они и ждущие, которые держат `NtObj`.
        self.objs[id].as_mut().expect("ntsync: dead object slot")
    }

    fn add(&mut self, kind: Kind) -> usize {
        let slot = Slot { kind, any: VecDeque::new(), all: Vec::new() };
        if let Some(id) = self.free.pop() {
            self.objs[id] = Some(slot);
            id
        } else {
            self.objs.push(Some(slot));
            self.objs.len() - 1
        }
    }

    /// Разбудить ждущих «любого» на объекте `id`, пока он сигнален: каждому
    /// победителю — захват и `wake`.
    fn wake_any(&mut self, id: usize) {
        let slot = self.slot(id);
        for (waiter, index) in &slot.any {
            if !slot.kind.signaled(waiter.owner) {
                // Семафор и событие кончились для всех; мьютекс — только для
                // этого владельца, следующий может оказаться им же.
                if matches!(slot.kind, Kind::Mutex { .. }) {
                    continue;
                }
                break;
            }
            if waiter.win(*index) {
                slot.kind.acquire(waiter);
                crate::sched::wake(waiter.task);
            }
        }
    }

    /// Ждущий «всех сразу»: если сигнальны все его объекты — захватить все
    /// разом и разбудить (индекс 0).
    fn wake_all(&mut self, waiter: &Waiter) {
        if waiter.signaled.load(Ordering::Acquire) != -1 {
            return;
        }
        let ids = &waiter.ids[..waiter.count];
        if !ids.iter().all(|&id| self.slot(id).kind.signaled(waiter.owner)) {
            return;
        }
        if waiter.win(0) {
            for &id in ids {
                self.slot(id).kind.acquire(waiter);
            }
            crate::sched::wake(waiter.task);
        }
    }

    /// Состояние объекта `id` сменилось: сперва «всех сразу» (каждый из них
    /// смотрит на все свои объекты), потом «любого» по очереди.
    fn wake_obj(&mut self, id: usize) {
        let mut at = 0;
        while at < self.slot(id).all.len() {
            let waiter = Arc::clone(&self.slot(id).all[at]);
            self.wake_all(&waiter);
            at += 1;
        }
        self.wake_any(id);
    }
}

/// fd устройства.
struct DevFile(Arc<Device>);

/// fd объекта.
struct NtObj {
    dev: Arc<Device>,
    id: usize,
}

impl Drop for NtObj {
    fn drop(&mut self) {
        // Последняя ссылка: ждущих на объекте нет (каждый держит свою).
        let slot = {
            let mut state = self.dev.state.lock();
            state.free.push(self.id);
            state.objs[self.id].take()
        };
        drop(slot);
    }
}

impl Object for DevFile {
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
        0o020_000 // S_IFCHR
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Object for NtObj {
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
        0 // анонимный узел, как у mainline
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---- создание ------------------------------------------------------------------

/// Новый объект на устройстве и fd на него (`O_RDWR | O_CLOEXEC`, как
/// `anon_inode_getfd` mainline).
fn create(dev: &Arc<Device>, kind: Kind) -> i64 {
    let id = dev.state.lock().add(kind);
    let object: Arc<dyn Object> = Arc::new(NtObj { dev: Arc::clone(dev), id });
    // Не поставился — `Arc` пропадёт, и `Drop` вернёт ячейку.
    install(object, false, true)
}

fn create_sem(dev: &Arc<Device>, arg: usize) -> i64 {
    let Ok([count, max]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
    if count > max {
        return -e::EINVAL;
    }
    create(dev, Kind::Sem { count, max })
}

fn create_mutex(dev: &Arc<Device>, arg: usize) -> i64 {
    let Ok([owner, count]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
    // Владелец есть ⇔ глубина ненулевая.
    if (owner == 0) != (count == 0) {
        return -e::EINVAL;
    }
    create(dev, Kind::Mutex { count, owner, dead: false })
}

fn create_event(dev: &Arc<Device>, arg: usize) -> i64 {
    let Ok([manual, signaled]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
    create(dev, Kind::Event { manual: manual != 0, signaled: signaled != 0 })
}

// ---- операции над объектом -------------------------------------------------------

impl NtObj {
    fn sem_release(&self, arg: usize) -> i64 {
        let Ok(add) = get::<u32>(arg) else { return -e::EFAULT };
        let prev = {
            let mut state = self.dev.state.lock();
            let slot = state.slot(self.id);
            let Kind::Sem { count, max } = &mut slot.kind else { return -e::EINVAL };
            let prev = *count;
            match count.checked_add(add) {
                Some(sum) if sum <= *max => *count = sum,
                _ => return -e::EOVERFLOW,
            }
            state.wake_obj(self.id);
            prev
        };
        if put(arg, prev).is_err() { -e::EFAULT } else { 0 }
    }

    fn mutex_unlock(&self, arg: usize) -> i64 {
        let Ok([owner, _]) = get::<[u32; 2]>(arg) else { return -e::EFAULT };
        if owner == 0 {
            return -e::EINVAL;
        }
        let prev = {
            let mut state = self.dev.state.lock();
            let slot = state.slot(self.id);
            let Kind::Mutex { count, owner: held, .. } = &mut slot.kind else { return -e::EINVAL };
            if *held != owner {
                return -e::EPERM;
            }
            let prev = *count;
            *count -= 1;
            if *count == 0 {
                *held = 0;
            }
            state.wake_obj(self.id);
            prev
        };
        // В ответ — глубина до отпускания, полем `count`.
        if put(arg + 4, prev).is_err() { -e::EFAULT } else { 0 }
    }

    fn mutex_kill(&self, arg: usize) -> i64 {
        let Ok(owner) = get::<u32>(arg) else { return -e::EFAULT };
        if owner == 0 {
            return -e::EINVAL;
        }
        let mut state = self.dev.state.lock();
        let slot = state.slot(self.id);
        let Kind::Mutex { count, owner: held, dead } = &mut slot.kind else { return -e::EINVAL };
        if *held != owner {
            return -e::EPERM;
        }
        *dead = true;
        *held = 0;
        *count = 0;
        state.wake_obj(self.id);
        0
    }

    /// `EVENT_SET`, а с `pulse` — `EVENT_PULSE`: поднять, разбудить, кого
    /// можно (под тем же локом), и для пульса тут же опустить.
    fn event_set(&self, arg: usize, pulse: bool) -> i64 {
        let prev = {
            let mut state = self.dev.state.lock();
            let slot = state.slot(self.id);
            let Kind::Event { signaled, .. } = &mut slot.kind else { return -e::EINVAL };
            let prev = *signaled;
            *signaled = true;
            state.wake_obj(self.id);
            if pulse {
                if let Kind::Event { signaled, .. } = &mut state.slot(self.id).kind {
                    *signaled = false;
                }
            }
            prev
        };
        if put(arg, u32::from(prev)).is_err() { -e::EFAULT } else { 0 }
    }

    fn event_reset(&self, arg: usize) -> i64 {
        let prev = {
            let mut state = self.dev.state.lock();
            let Kind::Event { signaled, .. } = &mut state.slot(self.id).kind else { return -e::EINVAL };
            core::mem::replace(signaled, false)
        };
        if put(arg, u32::from(prev)).is_err() { -e::EFAULT } else { 0 }
    }

    fn kind(&self) -> Kind {
        self.dev.state.lock().slot(self.id).kind
    }

    fn sem_read(&self, arg: usize) -> i64 {
        let Kind::Sem { count, max } = self.kind() else { return -e::EINVAL };
        if put(arg, [count, max]).is_err() { -e::EFAULT } else { 0 }
    }

    /// Брошенный мьютекс — `EOWNERDEAD`, но поля всё равно отдаются.
    fn mutex_read(&self, arg: usize) -> i64 {
        let Kind::Mutex { count, owner, dead } = self.kind() else { return -e::EINVAL };
        if put(arg, [owner, count]).is_err() {
            return -e::EFAULT;
        }
        if dead { -EOWNERDEAD } else { 0 }
    }

    fn event_read(&self, arg: usize) -> i64 {
        let Kind::Event { manual, signaled } = self.kind() else { return -e::EINVAL };
        if put(arg, [u32::from(manual), u32::from(signaled)]).is_err() { -e::EFAULT } else { 0 }
    }
}

// ---- ожидание -------------------------------------------------------------------

/// `struct ntsync_wait_args`, 40 байт.
#[derive(Clone, Copy)]
#[repr(C)]
struct WaitArgs {
    timeout: u64,
    objs: u64,
    count: u32,
    index: u32,
    flags: u32,
    owner: u32,
    alert: u32,
    pad: u32,
}

/// Смещение `index` в [`WaitArgs`]: туда пишется победивший индекс.
const INDEX_AT: usize = 20;

/// `WAIT_ANY` (`all == false`) и `WAIT_ALL`.
fn wait(dev: &Arc<Device>, arg: usize, all: bool) -> i64 {
    let Ok(args) = get::<WaitArgs>(arg) else { return -e::EFAULT };
    if args.pad != 0 || args.flags & !WAIT_REALTIME != 0 || args.owner == 0 {
        return -e::EINVAL;
    }
    let count = args.count as usize;
    if count > MAX_WAIT_COUNT {
        return -e::EINVAL;
    }
    let Ok(raw) = read_user(args.objs as usize, count * 4) else { return -e::EFAULT };
    let mut fds: Vec<u32> = raw.chunks_exact(4).map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]])).collect();
    // Алерт ноль — его нет (fd 0 алертом быть не может, так в uapi).
    if args.alert != 0 {
        fds.push(args.alert);
    }

    // Объекты — ссылками, которые ждущий держит до конца ожидания. Не fd,
    // не ntsync или с другого устройства — `EINVAL` (не `EBADF`: так mainline).
    let Some(objects) = crate::user::with_current(|program| {
        fds.iter().map(|&fd| program.files.object(fd as usize).map(|(object, _)| object)).collect::<Option<Vec<_>>>()
    })
    .flatten() else {
        return -e::EINVAL;
    };
    let mut ids = Vec::with_capacity(objects.len());
    for object in &objects {
        match object.as_any().downcast_ref::<NtObj>() {
            Some(obj) if Arc::ptr_eq(&obj.dev, dev) => ids.push(obj.id),
            _ => return -e::EINVAL,
        }
    }
    // «Всех сразу» по одному объекту дважды — бессмыслица (захват дважды).
    if all && (1..count).any(|i| ids[..i].contains(&ids[i])) {
        return -e::EINVAL;
    }

    let deadline = deadline(args.timeout, args.flags & WAIT_REALTIME != 0);
    let task = crate::sched::current();
    let waiter = Arc::new(Waiter {
        task,
        owner: args.owner,
        signaled: AtomicI32::new(-1),
        ownerdead: AtomicBool::new(false),
        ids,
        count,
    });

    // Встать в очереди и сразу опросить: по индексам, алерт последним.
    {
        let mut state = dev.state.lock();
        let alert = (waiter.ids.len() > count).then(|| waiter.ids[count]);
        if all {
            for &id in &waiter.ids[..count] {
                state.slot(id).all.push(Arc::clone(&waiter));
            }
            if let Some(id) = alert {
                state.slot(id).any.push_back((Arc::clone(&waiter), count as u32));
            }
            state.wake_all(&waiter);
            if let Some(id) = alert {
                if waiter.signaled.load(Ordering::Acquire) == -1 {
                    state.wake_any(id);
                }
            }
        } else {
            for (index, &id) in waiter.ids.iter().enumerate() {
                state.slot(id).any.push_back((Arc::clone(&waiter), index as u32));
            }
            for &id in &waiter.ids {
                if waiter.signaled.load(Ordering::Acquire) != -1 {
                    break;
                }
                state.wake_any(id);
            }
        }
    }

    // Сон: победа — выход; сигнал — `EINTR` (Wine повторяет вызов, срок
    // абсолютный — повтор честен); срок вышел — `ETIMEDOUT`.
    let key = Arc::as_ptr(&waiter) as usize;
    let failure = loop {
        if waiter.signaled.load(Ordering::Acquire) != -1 {
            break 0;
        }
        if super::signal::pending() {
            break -e::EINTR;
        }
        // Процесс снимают (`exit_group`, kill) — поток в бесконечном
        // ожидании обязан выйти, как в остальных долгих ожиданиях (`ipc`,
        // `poll`). Без этого он переживал выход процесса (16.11, Wine).
        if crate::user::kill_pending() {
            break -e::EINTR;
        }
        let until = match deadline {
            None => None,
            Some(at) => {
                let now = u128::from(crate::time::uptime_ns());
                if at <= now {
                    break -e::ETIMEDOUT;
                }
                // hw-log15: a short timeout ends at its nanosecond deadline
                // (hrsleep), not 1-2 ticks after it.
                if at - now <= 20_000_000 && crate::sched::hrsleep::enabled() {
                    crate::sched::hrsleep::block_on_futex_ns(key, at as u64, || {
                        waiter.signaled.load(Ordering::Acquire) != -1
                    });
                    continue;
                }
                let ms = (at - now).div_ceil(1_000_000) as u64;
                Some(crate::time::uptime_ms().saturating_add(ms.max(1)))
            }
        };
        crate::sched::block_on_futex(key, until, || waiter.signaled.load(Ordering::Acquire) != -1);
    };

    // Выйти из очередей. После этого победить уже нельзя, и `signaled` —
    // окончательный: если победа пришла вместе с сигналом или сроком, она
    // важнее (иначе захваченный семафор/мьютекс пропал бы).
    {
        let mut state = dev.state.lock();
        for &id in &waiter.ids {
            let slot = state.slot(id);
            slot.any.retain(|(other, _)| !Arc::ptr_eq(other, &waiter));
            if all {
                slot.all.retain(|other| !Arc::ptr_eq(other, &waiter));
            }
        }
    }
    drop(objects);

    let signaled = waiter.signaled.load(Ordering::Acquire);
    if signaled == -1 {
        return failure;
    }
    if put(arg + INDEX_AT, signaled as u32).is_err() {
        return -e::EFAULT;
    }
    if waiter.ownerdead.load(Ordering::Relaxed) { -EOWNERDEAD } else { 0 }
}

/// Абсолютный срок в нс по монотонным часам ядра (`uptime_ns`); `None` —
/// без срока (`U64_MAX`). `REALTIME` переводится один раз, как у
/// `futex_waitv`.
///
/// «Сейчас» — `time::realtime_ns`, те же часы, что у `clock_gettime`
/// (19.11.2026). Раньше доли секунды брались от `uptime_ns`, а у часов суток
/// они сдвинуты на миг, когда приняли время прошивки: срок ошибался до
/// секунды — и проба ntsync падала, стоило загрузке сдвинуться на
/// миллисекунды (замер `memcpy` перед `adopt_boot_clock`).
fn deadline(timeout: u64, realtime: bool) -> Option<u128> {
    if timeout == u64::MAX {
        return None;
    }
    let wanted = u128::from(timeout);
    if !realtime {
        return Some(wanted);
    }
    let uptime = u128::from(crate::time::uptime_ns());
    let now = u128::from(crate::time::realtime_ns().unwrap_or(0));
    Some(uptime + wanted.saturating_sub(now))
}
