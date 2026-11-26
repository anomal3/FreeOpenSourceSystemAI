//! Планировщик задач: round-robin с вытеснением, на одном процессоре или на
//! нескольких.
//!
//! # Что здесь от чего отделено
//!
//! Три вещи, которые обычно слипаются в одну функцию, здесь разнесены:
//!
//! | что                    | кто                                    |
//! |------------------------|----------------------------------------|
//! | *повод* переключиться  | [`yield_now`], [`exit_current_with`], [`preempt_point`] |
//! | *решение* кого пустить | [`Scheduler::pick_next`]               |
//! | *механизм* перехода    | [`schedule`] + `arch::switch_context`  |
//!
//! Разделение не украшение, и Phase 13b это подтвердила: вытеснение оказалось
//! ровно одной новой причиной вызвать [`schedule`] и ничем больше — ни выбор
//! следующей задачи, ни само переключение под него не менялись. Фаза 43
//! подтвердила ещё раз: второй процессор поменял *решение* (кого можно брать) и
//! *механизм* (когда задача перестаёт быть занятой), а поводы остались теми же.
//!
//! # Вытеснение: почему переключение отложено до конца прерывания
//!
//! Напрашивается переключаться прямо в обработчике таймера, как только истёк
//! квант. Так делать **нельзя**, и причина одна на обе архитектуры: пока
//! прерывание не подтверждено контроллеру, оно остаётся активным и закрывает
//! собой всё, что не выше его приоритета. Уйдя в другую задачу до
//! подтверждения, ядро оставило бы её работать с намертво замолчавшим таймером
//! — то есть вытеснить её было бы уже нечем, — а подтверждение пришло бы только
//! тогда, когда управление случайно вернулось бы в покинутый обработчик.
//!
//! Поэтому [`on_timer_tick`] ничего не переключает: он считает квант и поднимает
//! флаг. Переключается ядро в [`preempt_point`] — точке, которую арх-слой
//! вызывает в конце обработки внешнего прерывания, уже после `EOI`.
//!
//! Второе следствие того же порядка: к моменту [`preempt_point`] обработчик уже
//! доделал свою работу, и на стеке задачи остаётся только кадр возврата из
//! прерывания. Задача, снятая с процессора, продолжится именно с него —
//! неважно, исполняла она код ядра или код программы в третьем кольце, и
//! неважно, на каком процессоре её продолжат.
//!
//! # Главная ловушка: лок через переключение
//!
//! Состояние планировщика живёт под [`SpinLock`]. Держать этот лок в момент
//! `switch_context` **нельзя**: управление уйдёт в другую задачу, а охранник
//! останется на покинутом стеке, и лок не освободится никогда.
//!
//! Но и просто отпустить лок перед переключением недостаточно. `SpinGuard`
//! возвращает прерывания в то состояние, в котором их застал, и между «`current`
//! уже указывает на следующую задачу» и «`switch_context` выполнен» открылось бы
//! окно, в котором таймерное прерывание вызывает [`schedule`] повторно —
//! и сохраняет регистры *текущего* стека в *чужой* `Context`.
//!
//! Решение в [`schedule`]: прерывания запрещаются **снаружи** лока и остаются
//! запрещёнными на всё переключение. Тогда `lock()` застаёт их уже запрещёнными
//! и при уничтожении охранника не включает — лок отпущен, окна нет.
//!
//! # Несколько процессоров: окно, которое запрет прерываний не закрывает
//!
//! Запрет прерываний закрывает окно для **своего** процессора. Соседний в это
//! окно видит таблицу задач как есть: уходящая задача уже `Ready`, а её регистры
//! ещё не сохранены — их сохранит `switch_context`, который начнётся только
//! после того, как лок отпущен. Сосед, выбравший её в этот миг, загрузил бы
//! контекст, которого ещё нет, и продолжил бы на стеке, по которому идёт другой
//! процессор. Два исполнения на одном стеке — отказ, объяснить который не
//! получится ни по какому журналу.
//!
//! Закрывает окно метка [`task::Task::cpu`]: «контекст этой задачи жив на
//! процессоре N». Её ставит тот, кто задачу берёт, а снимает не тот, кто
//! отдаёт, а **следующая** задача на том же процессоре — в [`finish_switch`],
//! то есть уже на своём стеке, когда переключение закончилось. Задача с меткой
//! в выборе не участвует. Ровно так же устроено `on_cpu` в Linux, и по той же
//! причине.
//!
//! # Первый запуск
//!
//! [`run`] вызывается из контекста, у которого нет `Task`. Вместо особого
//! «первого переключения» планировщик заводит **холостую задачу** (`id 0`),
//! которая этот контекст и представляет: её стек — стек ядра, её `Context` пуст
//! ровно до первого `switch_context`, который его и заполнит.
//!
//! Холостых задач столько, сколько процессоров, и у каждой свой слот — номер
//! процессора. Холостая задача приколочена к своему процессору: чужой её не
//! выберет никогда, свой выбирает только тогда, когда исполнять больше некого.
//!
//! # Уборка завершённых
//!
//! Задача, вернувшая управление из `entry`, всё ещё исполняется на собственном
//! стеке: освободить его в этот момент — это отпилить сук под собой. Поэтому
//! [`exit_current`] лишь помечает задачу `Finished` и уступает процессор
//! навсегда, а стек освобождает [`Scheduler::reap`] — не раньше, чем с задачи
//! снята метка процессора. До фазы 43 условием было «не текущая», и на двух
//! процессорах оно перестало быть достаточным: задача может уже не быть
//! текущей ни на одном из них и всё ещё исполнять свой последний
//! `switch_context`.
//!
//! Сама структура `Task` при этом остаётся жить: она невелика, а список
//! завершённых задач с их счётчиками — единственная диагностика того, что
//! вообще происходило.

// Часть API планировщика заведомо не имеет вызывающих на этом этапе: диагностика
// (`switch_count`, `result_of`) нужна снаружи, а не изнутри, а `set_preemption`
// вызывается ровно один раз при запуске. Это спроектированный контракт, а не
// забытый код (та же ситуация и та же причина, что в `mm/mod.rs`).
#![allow(dead_code)]

pub mod hrsleep;
pub mod prio;
mod task;
mod waitq;

pub use task::{
    SpawnError, TaskId, TaskState, UserMachine, Wait, STACK_GUARD_SIZE, TASK_STACK_SIZE,
};

use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use alloc::boxed::Box;

use task::{box_task, Stack, Task};

use crate::arch::{self, Context};
use crate::kprintln;
use crate::smp::{self, MAX_CPUS};
use crate::sync::SpinLock;

/// Сколько задач помещается в таблицу.
///
/// Таблица фиксированного размера, а не `Vec`, сознательно: `Vec` при росте
/// переносит содержимое, а планировщик держит сырые указатели на `Context`
/// задач — и, что важнее, `Vec::push` паникует при нехватке памяти, тогда как
/// [`spawn`] обязан вернуть ошибку.
///
/// Первые [`MAX_CPUS`] слотов принадлежат холостым задачам процессоров, поэтому
/// к прежним тридцати двум они прибавлены, а не вычтены из них: программам
/// второй процессор места не отнимает.
///
/// Двести пятьдесят шесть, а не тридцать два, с фазы 71h: Chromium без окна —
/// это пять-шесть процессов по десятку-два потоков, и тридцати двух мест ему
/// не хватало на первую же страницу (`pthread_create` — `EAGAIN`). Стек ядра
/// задаче выдаётся при запуске, поэтому пустые места стоят только записи в
/// таблицах по слотам.
///
/// Тысяча двадцать четыре с 01.11: Rider (Java и .NET) сам держит около
/// двухсот потоков, и рядом с ним каждый процесс вкладки Chromium получал
/// `EAGAIN` на `pthread_create`, выходил с кодом 191 — и браузер закрывался
/// на первой же странице. Пустой слот — восемь байт в таблице задач и около
/// восьмисот в журнале вызовов (`linux::recent`); обходы планировщика идут
/// только до отметки занятых слотов (`Scheduler::high`): по всей тысяче
/// отладочное ядро не успевало (сценарий `smp`).
pub const MAX_TASKS: usize = 1024 + MAX_CPUS;

/// Длина кванта в тиках таймера: 10 мс.
///
/// Было 50 мс — «заметно меньше, чем человек замечает». Для счёта верно, для
/// пальца нет, и это выяснилось на телефоне. События указателя разбирает не та
/// задача, что опрашивает панель: тач кладёт касание в очередь, а забирает его
/// оболочка. Между ними стоит планировщик, и при кванте в 50 мс каждое касание
/// ждало своей очереди до пятидесяти миллисекунд — при том, что кадр к этому
/// времени рисовался за десять.
///
/// Десять миллисекунд — всё ещё вдесятеро больше стоимости переключения
/// (сотни наносекунд), но уже незаметно для руки.
const SLICE_TICKS: u32 = 10;

/// Длина кванта в миллисекундах — то же число, но в единицах, которые понимает
/// читающий журнал загрузки.
pub const SLICE_MS: u64 = SLICE_TICKS as u64 * 1000 / crate::irq::TIMER_HZ as u64;

/// Разрешено ли вытеснение по таймеру.
///
/// Отдельный флаг, а не `const`: включение вытеснения — это изменение
/// поведения, которое надо уметь откатить в рантайме, когда что-то пойдёт не
/// так. По умолчанию выключено — до [`run`] переключаться всё равно не на что;
/// включает его [`set_preemption`].
static PREEMPTION: AtomicBool = AtomicBool::new(false);

/// Истёк ли квант текущей задачи — у каждого процессора свой.
///
/// Флаг, а не немедленное переключение: поднимает его обработчик таймера,
/// разбирает — [`preempt_point`] в конце прерывания. Флаг один на процессор,
/// потому что и квант у каждого свой: истёкший квант одной задачи не повод
/// снимать с процессора другую.
static NEED_RESCHED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// Сколько тиков осталось текущей задаче каждого процессора до конца кванта.
///
/// Вне лока планировщика, и это не оптимизация. Обработчик таймера на
/// процессоре A не имеет права ждать лок, который держит процессор B, дольше,
/// чем тот его держит, — но и терять тик тоже не хочется: пропущенный тик
/// означает квант, который не кончается. Своя ячейка на процессор снимает
/// вопрос целиком — пишет в неё только свой обработчик и свой [`schedule`], а
/// друг друга они не прерывают, потому что `schedule` идёт с запрещёнными
/// прерываниями.
static SLICE_LEFT: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(SLICE_TICKS) }; MAX_CPUS];

/// Сколько тиков застали процессор в холостой задаче.
///
/// Считается по-прежнему, но долей простоя больше не служит: см. [`dump`].
static IDLE_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Сколько тиков насчитал таймер к началу планирования.
///
/// Нужно вместе с [`STARTED_MS`] и ровно для одного: сравнивать пришедшие тики
/// с положенными можно только за одно и то же окно. Планировщик запускается в
/// конце загрузки, и тики, накопившиеся до него, к этому счёту отношения не
/// имеют.
static STARTED_TICKS: AtomicU64 = AtomicU64::new(0);

/// Когда началось планирование, мс с загрузки.
///
/// Нужно ровно для доли простоя: до этой отметки холостой задачи не
/// существовало, и тики, пришедшие за время загрузки, к простою отношения не
/// имеют. Делитель «всё время с включения таймера» занижал долю ровно на
/// длительность загрузки — а она у отладочной сборки больше, чем всё
/// остальное измерение.
static STARTED_MS: AtomicU64 = AtomicU64::new(0);

/// Исполняет ли процессор сейчас свою холостую задачу. Нужно обработчику
/// таймера, который не берёт лок.
static ON_IDLE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(true) }; MAX_CPUS];

/// Запущено ли планирование — то же, что `Scheduler::running`, но читаемое без
/// лока.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Почему происходит переключение.
///
/// Различие нужно ровно для одного: счётчика снятий не по своей воле. Он и есть
/// то, чем вытеснение вообще можно наблюдать снаружи — у программы, не делающей
/// системных вызовов, других поводов сменить задачу не существует.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cause {
    /// Задача уступила процессор сама: `yield`, ожидание, завершение.
    Voluntary,
    /// У задачи кончился квант.
    Preempted,
}

/// То, что планировщик знает об одном процессоре.
struct Cpu {
    /// Слот исполняющейся на нём задачи.
    current: usize,
    /// Слот задачи, которая только что уступила этот процессор и чей контекст,
    /// возможно, ещё сохраняется. Метку [`task::Task::cpu`] с неё снимает
    /// [`finish_switch`] — см. заголовок модуля.
    previous: Option<usize>,
    /// Сколько переключений было на этом процессоре.
    switches: u64,
    /// Сколько из них по истёкшему кванту.
    forced: u64,
}

/// Всё изменяемое состояние планировщика.
struct Scheduler {
    tasks: [Option<Box<Task>>; MAX_TASKS],
    cpus: [Cpu; MAX_CPUS],
    /// Откуда брать следующий идентификатор. Не совпадает с номером слота:
    /// слоты переиспользуются, идентификаторы — нет.
    next_id: u32,
    /// Слотов от начала таблицы, хоть раз занятых: дальше пусто, и обходы
    /// (выбор следующей задачи на каждом переключении, будильники на каждом
    /// тике) идут только до этой отметки, а не по всем [`MAX_TASKS`].
    high: usize,
    /// Запущено ли планирование. До [`run`] переключаться не на что и не с чего.
    running: bool,
}

static SCHED: SpinLock<Scheduler> = SpinLock::new(Scheduler::new());

/// Ближайший срок среди ждущих со сроком, мс (`Scheduler::wake_expired`).
/// Пишется только под замком `SCHED`: при постановке срока — минимум, при
/// обходе — заново. Ноль — «обойти при первом же переключении».
static NEXT_DUE: AtomicU64 = AtomicU64::new(0);

/// Задача встала в ожидание со сроком `at` (мс); звать под замком `SCHED`.
fn note_deadline(slot: usize, at: u64) {
    TIMED.set(slot);
    NEXT_DUE.fetch_min(at, Ordering::Relaxed);
}

/// Slot sets (G2, 21.11.2026): one bit per task slot, so that the hot paths
/// stop walking every slot (and dereferencing every `Box<Task>`) under the
/// scheduler lock. Each set is a *superset*: a bit is set (under `SCHED`)
/// whenever a task enters the state, and cleared lazily (also under `SCHED`)
/// by whoever finds the task no longer in it. Readable without the lock as a
/// hint ("maybe there is work").
const SLOT_WORDS: usize = MAX_TASKS.div_ceil(64);

struct SlotBits([AtomicU64; SLOT_WORDS]);

impl SlotBits {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; SLOT_WORDS])
    }

    fn set(&self, slot: usize) {
        self.0[slot / 64].fetch_or(1 << (slot % 64), Ordering::SeqCst);
    }

    fn clear(&self, slot: usize) {
        self.0[slot / 64].fetch_and(!(1 << (slot % 64)), Ordering::Relaxed);
    }

    fn any(&self) -> bool {
        self.0.iter().any(|word| word.load(Ordering::SeqCst) != 0)
    }

    fn has(&self, slot: usize) -> bool {
        self.0[slot / 64].load(Ordering::Relaxed) & (1 << (slot % 64)) != 0
    }

    /// Visit set slots of `[from, to)`; `visit` returns `true` to stop.
    fn scan(&self, from: usize, to: usize, visit: &mut impl FnMut(usize) -> bool) -> bool {
        let mut word = from / 64;
        while word * 64 < to {
            let mut bits = self.0[word].load(Ordering::Relaxed);
            let base = word * 64;
            if base < from {
                bits &= !0u64 << (from - base);
            }
            if to - base < 64 {
                bits &= (1u64 << (to - base)) - 1;
            }
            while bits != 0 {
                let slot = base + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if visit(slot) {
                    return true;
                }
            }
            word += 1;
        }
        false
    }

    /// Visit set slots below `high` round-robin, starting at `start`.
    fn scan_from(&self, start: usize, high: usize, mut visit: impl FnMut(usize) -> bool) {
        let start = start.min(high);
        if !self.scan(start, high, &mut visit) {
            self.scan(0, start, &mut visit);
        }
    }

    /// Visit slots set both here and in `other` of `[from, to)`.
    fn scan_and(&self, other: &SlotBits, from: usize, to: usize, visit: &mut impl FnMut(usize) -> bool) -> bool {
        let mut word = from / 64;
        while word * 64 < to {
            let mut bits = self.0[word].load(Ordering::Relaxed) & other.0[word].load(Ordering::Relaxed);
            let base = word * 64;
            if base < from {
                bits &= !0u64 << (from - base);
            }
            if to - base < 64 {
                bits &= (1u64 << (to - base)) - 1;
            }
            while bits != 0 {
                let slot = base + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if visit(slot) {
                    return true;
                }
            }
            word += 1;
        }
        false
    }

    /// [`SlotBits::scan_and`] round-robin from `start`, below `high`.
    fn scan_and_from(&self, other: &SlotBits, start: usize, high: usize, mut visit: impl FnMut(usize) -> bool) {
        let start = start.min(high);
        if !self.scan_and(other, start, high, &mut visit) {
            self.scan_and(other, 0, start, &mut visit);
        }
    }
}

/// Tasks in state `Ready` (pick_next looks only here).
static READY: SlotBits = SlotBits::new();
/// Tasks blocked with a deadline (wake_expired looks only here).
static TIMED: SlotBits = SlotBits::new();
/// Tasks blocked on an interrupt source (wake_irq) and on input (wake_input).
static IRQ_WAITERS: SlotBits = SlotBits::new();
static INPUT_WAITERS: SlotBits = SlotBits::new();

/// Task id per slot (0 - empty or idle): `wake(id)` and `current()` without
/// walking the table. Written under `SCHED` when a task takes the slot.
static SLOT_ID: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];
/// `Scheduler::high`, readable without the lock.
static HIGH: AtomicUsize = AtomicUsize::new(MAX_CPUS);
/// Bumped by `wake(id)` before it looks at the task: a task that checked its
/// condition and is about to block (wait queues) does not block if it moved.
static WAKE_SEQ: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];
/// Non-daemon tasks that have not finished (`alive()` without the lock).
static ALIVE: AtomicUsize = AtomicUsize::new(0);
/// A task finished and its stack may wait for `reap`.
static REAP_PENDING: AtomicBool = AtomicBool::new(false);

/// Slot of a live task with this id (lock-free hint, verify under `SCHED`).
fn slot_of(id: TaskId) -> Option<usize> {
    let raw = id.as_u32();
    if raw == 0 {
        return None;
    }
    let high = HIGH.load(Ordering::Acquire).min(MAX_TASKS);
    (MAX_CPUS..high).find(|&slot| SLOT_ID[slot].load(Ordering::Relaxed) == raw)
}

/// Is there anything for an idle CPU (or a yield) to do? Lock-free, may say
/// "yes" spuriously, never "no" when a task became ready before the call.
fn has_work() -> bool {
    READY.any()
        || DEFERRED_WAKES_ANY.load(Ordering::Acquire)
        || REAP_PENDING.load(Ordering::Relaxed)
        || crate::time::uptime_ms() >= NEXT_DUE.load(Ordering::Relaxed)
}

// --- Per-CPU ready queues (`schedq percpu`, 22.11.2026) -----------------------
//
// hw-log14: D2R on 16 CPUs waited 340-600 ms per 30 s for the one scheduler
// lock, mostly in `schedule_with`. In `percpu` mode the switch itself - who
// runs next, the bookkeeping of the two tasks - takes no global lock:
//
// * every CPU has its ready queue (`HOMEQ`, a slot bitmap); a woken task goes
//   to an idle CPU (its last one if that idles) and that CPU is kicked, else
//   to the queue of the CPU it last ran on. `READY` stays the union of all
//   queues (`has_work`, and `global` mode picks from it);
// * a CPU picks boosted, then driver, then (with input waiting) desktop tasks
//   from anywhere, then its own queue round-robin, then steals any ready task;
// * a task is taken by a compare-and-swap of its `ON_CPU` flag (set while its
//   context lives on a CPU, cleared by `finish_switch` of the next task), then
//   its queue bits are cleared, and only then is `RUNSTATE` checked: a waker
//   writes `RUNSTATE` before it sets the bits, so a wake-up is never lost;
// * the global lock stays for what needs it - going to sleep (wakers check
//   the state under it), wake-ups, deadlines, reaping, spawn, diagnostics -
//   and for those only when there is something to do.
//
// `global` is the old path (one lock around the whole switch). Both modes keep
// the same task fields; `set_percpu` flips the mode under the lock, waiting
// for CPUs inside a lock-free switch (`IN_PERCPU`).

/// Per-CPU mode is on (`schedq percpu|global`).
static PERCPU: AtomicBool = AtomicBool::new(true);
/// A CPU is inside the lock-free part of a `percpu` switch.
static IN_PERCPU: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Ready queue of each CPU (superset: a bit may outlive the task's stay).
static HOMEQ: [SlotBits; MAX_CPUS] = [const { SlotBits::new() }; MAX_CPUS];
/// Which CPU's queue a ready task was put in.
static HOME: [AtomicU16; MAX_TASKS] = [const { AtomicU16::new(0) }; MAX_TASKS];
/// The CPU a task last ran on (where its cache is).
static LAST_CPU: [AtomicU16; MAX_TASKS] = [const { AtomicU16::new(0) }; MAX_TASKS];
/// The task's context lives on a CPU (running, or still being saved): nobody
/// else may take it. Both modes (the old `Task::cpu` + `Task::leaving` pair).
static ON_CPU: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];
/// `Task::state` without its payload, readable without the lock.
static RUNSTATE: [AtomicU8; MAX_TASKS] = [const { AtomicU8::new(RS_EMPTY) }; MAX_TASKS];
const RS_EMPTY: u8 = 0;
const RS_READY: u8 = 1;
const RS_RUNNING: u8 = 2;
const RS_BLOCKED: u8 = 3;
const RS_FINISHED: u8 = 4;
/// Tasks with `Task::boost`, driver and desktop tasks (`rank_of`).
static BOOSTED: SlotBits = SlotBits::new();
static DRIVERS: SlotBits = SlotBits::new();
static DESKTOPS: SlotBits = SlotBits::new();
/// The task in each slot, for the lock-free switch (published under the lock
/// when the slot gets its task; the `Box` does not move).
static TASK_PTR: [core::sync::atomic::AtomicPtr<Task>; MAX_TASKS] =
    [const { core::sync::atomic::AtomicPtr::new(ptr::null_mut()) }; MAX_TASKS];
/// The slot that ran before the current one on each CPU (both modes).
static PREV_SLOT: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(usize::MAX) }; MAX_CPUS];
/// Switches (and forced ones) done by `percpu` switches, per CPU.
static PC_SWITCHES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static PC_FORCED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// `perf-diag`: picks from the own queue, from elsewhere (steals and the
/// priority classes), lost claims, switches that needed the global lock.
static PC_LOCAL: AtomicU64 = AtomicU64::new(0);
static PC_STOLEN: AtomicU64 = AtomicU64::new(0);
static PC_LOST: AtomicU64 = AtomicU64::new(0);
static PC_LOCKED: AtomicU64 = AtomicU64::new(0);

fn runstate_of(state: TaskState) -> u8 {
    match state {
        TaskState::Ready => RS_READY,
        TaskState::Running => RS_RUNNING,
        TaskState::Blocked(_) => RS_BLOCKED,
        TaskState::Finished => RS_FINISHED,
    }
}

/// Write `task.state` and its lock-free mirror. Every state change goes here.
fn set_state(slot: usize, task: &mut Task, state: TaskState) {
    task.state = state;
    RUNSTATE[slot].store(runstate_of(state), Ordering::SeqCst);
}

fn set_boost(slot: usize, task: &mut Task, on: bool) {
    task.boost = on;
    if on {
        BOOSTED.set(slot);
    } else {
        BOOSTED.clear(slot);
    }
}

/// Is `schedq percpu` on.
#[must_use]
pub fn percpu() -> bool {
    PERCPU.load(Ordering::Relaxed)
}

/// `(percpu on, own-queue picks, picks from elsewhere, lost claims, switches
/// that took the global lock)` for `perf-diag`.
#[must_use]
pub fn percpu_stats() -> (bool, u64, u64, u64, u64) {
    (
        percpu(),
        PC_LOCAL.load(Ordering::Relaxed),
        PC_STOLEN.load(Ordering::Relaxed),
        PC_LOST.load(Ordering::Relaxed),
        PC_LOCKED.load(Ordering::Relaxed),
    )
}

/// `schedq percpu|global`: switch the scheduler mode now. Under the global
/// lock; to `global` it waits for CPUs that are inside a lock-free switch.
pub fn set_percpu(on: bool) {
    let sched = sched_lock();
    if PERCPU.load(Ordering::SeqCst) == on {
        return;
    }
    if on {
        // Every ready task goes to the queue of the CPU it last ran on.
        let high = sched.high;
        READY.scan_from(MAX_CPUS, high, |slot| {
            if !is_idle_slot(slot) && RUNSTATE[slot].load(Ordering::SeqCst) == RS_READY {
                let home = usize::from(LAST_CPU[slot].load(Ordering::Relaxed));
                let home = if home < MAX_CPUS && smp::is_online(home) { home } else { 0 };
                HOME[slot].store(home as u16, Ordering::Relaxed);
                HOMEQ[home].set(slot);
            }
            false
        });
        PERCPU.store(true, Ordering::SeqCst);
    } else {
        PERCPU.store(false, Ordering::SeqCst);
        // `READY` already holds every queued task; wait out the CPUs that
        // decided on `percpu` before the flag moved.
        for flag in &IN_PERCPU {
            while flag.load(Ordering::SeqCst) {
                core::hint::spin_loop();
            }
        }
    }
    drop(sched);
}

/// Where a task made ready goes: an idle CPU (its last one first) that has not
/// been kicked yet, else the CPU it last ran on.
fn choose_cpu(slot: usize) -> usize {
    let me = smp::cpu();
    let last = usize::from(LAST_CPU[slot].load(Ordering::Relaxed));
    let last_ok = last < MAX_CPUS && smp::is_online(last);
    let free = |cpu: usize| smp::is_online(cpu) && ON_IDLE[cpu].load(Ordering::Relaxed) && !KICKED[cpu].load(Ordering::Relaxed);
    if !RUNNING.load(Ordering::Relaxed) {
        return if last_ok { last } else { me };
    }
    if last_ok && free(last) {
        return last;
    }
    let online = smp::online().clamp(1, MAX_CPUS);
    for step in 1..=online {
        let cpu = (me + step) % online;
        if free(cpu) {
            return cpu;
        }
    }
    if last_ok { last } else { me }
}

/// Put a ready task in a CPU's queue and kick that CPU if it idles.
/// `RUNSTATE` must already say ready (see the protocol above).
fn enqueue_percpu(slot: usize) {
    let target = choose_cpu(slot);
    prio::on_wake(slot, target);
    HOME[slot].store(target as u16, Ordering::Relaxed);
    HOMEQ[target].set(slot);
    READY.set(slot);
    let me = smp::cpu();
    let quiet = QUIET_WAKER[RUNNING_SLOT[me].load(Ordering::Relaxed)].load(Ordering::Relaxed);
    if target != me && !quiet && ON_IDLE[target].load(Ordering::Relaxed) {
        kick_cpu(target);
    }
}

/// The task wakes everybody on a key (`wake_futex(.., 0)`, I/O events): no
/// kicks, as in `global` mode (10k IPIs a second on Wine's start, 18.11).
static QUIET_WAKER: [AtomicBool; MAX_TASKS] = [const { AtomicBool::new(false) }; MAX_TASKS];

/// Try to take the ready task in `slot` for this CPU (found in `found_in`'s
/// queue, or in no queue). `true` - it is ours: `ON_CPU` is set and its bits
/// are cleared.
fn try_claim(slot: usize, found_in: Option<usize>) -> bool {
    if is_idle_slot(slot) || RUNSTATE[slot].load(Ordering::SeqCst) != RS_READY {
        return false;
    }
    if ON_CPU[slot].compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed).is_err() {
        return false;
    }
    // Ours for now: clear the bits first, then look at the state - a waker
    // writes the state first and then sets the bits.
    if let Some(cpu) = found_in {
        HOMEQ[cpu].clear(slot);
    }
    HOMEQ[usize::from(HOME[slot].load(Ordering::Relaxed)).min(MAX_CPUS - 1)].clear(slot);
    READY.clear(slot);
    core::sync::atomic::fence(Ordering::SeqCst);
    if RUNSTATE[slot].load(Ordering::SeqCst) == RS_READY && !TASK_PTR[slot].load(Ordering::Acquire).is_null() {
        return true;
    }
    // Not ready after all (a stale bit): give it back. A wake-up after the
    // bits were cleared set them again.
    ON_CPU[slot].store(false, Ordering::SeqCst);
    PC_LOST.fetch_add(1, Ordering::Relaxed);
    false
}

/// The `percpu` choice for `cpu`, whose current task is `current`. `None` -
/// nobody else is ready.
fn pick_percpu(cpu: usize, current: usize, cause: Cause) -> Option<usize> {
    let high = HIGH.load(Ordering::Acquire).min(MAX_TASKS);
    let mut got = None;
    // Boosted (woken by an interrupt or input), then drivers, then the
    // desktop while input waits - from any queue, as `pick_next` does.
    let classes: [(&SlotBits, bool); 3] = [
        (&BOOSTED, true),
        (&DRIVERS, true),
        (&DESKTOPS, crate::input::has_events_now() || crate::ui::input_waiting()),
    ];
    for (class, wanted) in classes {
        if !wanted || got.is_some() {
            continue;
        }
        READY.scan_and_from(class, current + 1, high, |slot| {
            if try_claim(slot, None) {
                got = Some(slot);
                return true;
            }
            false
        });
    }
    if got.is_some() {
        PC_STOLEN.fetch_add(1, Ordering::Relaxed);
        return got;
    }
    // Priorities (25.11): by weight and virtual run time; round-robin
    // below only when the chosen task was taken by another CPU meanwhile.
    if prio::picker() {
        match pick_weighted(cpu, current, cause, high) {
            Weighted::Pick(slot) => return Some(slot),
            Weighted::Stay => return None,
            Weighted::Fallback => {}
        }
    }
    // The own queue, round-robin after the current task.
    HOMEQ[cpu].scan_from(current + 1, high, |slot| {
        if try_claim(slot, Some(cpu)) {
            got = Some(slot);
            return true;
        }
        false
    });
    if got.is_some() {
        PC_LOCAL.fetch_add(1, Ordering::Relaxed);
        return got;
    }
    // Steal: any ready task, starting at a different place for each CPU.
    let start = MAX_CPUS + (cpu * 37) % high.saturating_sub(MAX_CPUS).max(1);
    READY.scan_from(start, high, |slot| {
        if try_claim(slot, None) {
            got = Some(slot);
            return true;
        }
        false
    });
    if got.is_some() {
        PC_STOLEN.fetch_add(1, Ordering::Relaxed);
    }
    got
}

/// What [`pick_weighted`] decided.
enum Weighted {
    /// This task, claimed for the CPU.
    Pick(usize),
    /// The running task keeps the CPU.
    Stay,
    /// Nothing claimable by weight: the round-robin walks decide.
    Fallback,
}

/// The `percpu` choice by priority (`prio on`, see `prio.rs`), after the
/// boosted/driver/desktop classes.
///
/// * Every ready task is ranked by its virtual run time (one scale for the
///   machine); a task of another CPU's queue counts [`prio::MIGRATE_COST`]
///   later (its caches are elsewhere).
/// * At the end of a slice the running task stays while its own virtual
///   time is not above the best ranked; otherwise the best is taken - from
///   another queue too: a task queued behind heavier ones falls behind and
///   moves to the CPU that runs lighter work (prio scenario, 4 CPUs).
/// * `sched_yield` and a task going to sleep switch to the best ranked
///   ready task, as before to anybody ready.
fn pick_weighted(cpu: usize, current: usize, cause: Cause, high: usize) -> Weighted {
    let now_ns = crate::time::uptime_ns();
    let running = !is_idle_slot(current) && matches!(RUNSTATE[current].load(Ordering::SeqCst), RS_RUNNING | RS_READY);
    // (slot, rank, from the own queue)
    let mut best: Option<(usize, i64, bool)> = None;
    READY.scan(0, high, &mut |slot| {
        if is_idle_slot(slot) || slot == current || RUNSTATE[slot].load(Ordering::Relaxed) != RS_READY || ON_CPU[slot].load(Ordering::Relaxed) {
            return false;
        }
        let local = HOMEQ[cpu].has(slot);
        // Queued for an idle CPU (kicked, about to run there): not ours -
        // taking it leaves that CPU idle and the task taken from here waiting.
        let home = usize::from(HOME[slot].load(Ordering::Relaxed)).min(MAX_CPUS - 1);
        if !local && home != cpu && ON_IDLE[home].load(Ordering::Relaxed) {
            return false;
        }
        let rank = prio::lag(slot).saturating_add(if local { 0 } else { prio::MIGRATE_COST });
        if best.is_none_or(|(_, other, _)| rank < other) {
            best = Some((slot, rank, local));
        }
        false
    });
    let choice = match best {
        Some((_, rank, _)) if running && cause == Cause::Preempted && rank >= prio::live_lag(current, now_ns) => None,
        other => other,
    };
    let Some((slot, _, local)) = choice else {
        if running {
            prio::on_stay(current, cpu, now_ns);
            if cause == Cause::Preempted {
                prio::STAYS.fetch_add(1, Ordering::Relaxed);
            }
            return Weighted::Stay;
        }
        return Weighted::Fallback;
    };
    if !try_claim(slot, if local { Some(cpu) } else { None }) {
        return Weighted::Fallback;
    }
    if local {
        PC_LOCAL.fetch_add(1, Ordering::Relaxed);
    } else {
        PC_STOLEN.fetch_add(1, Ordering::Relaxed);
        if running {
            prio::PULLS.fetch_add(1, Ordering::Relaxed);
        }
    }
    Weighted::Pick(slot)
}

/// The task in `slot` through its published pointer.
///
/// # Safety
///
/// The caller owns the task's switch fields: it is the current task of this
/// CPU, or this CPU just claimed it (`ON_CPU`).
unsafe fn task_at(slot: usize) -> Option<&'static mut Task> {
    let pointer = TASK_PTR[slot].load(Ordering::Acquire);
    // SAFETY: the function's contract; the `Box` lives while the slot does.
    unsafe { pointer.as_mut() }
}

/// What a switch decided, carried out after the decision (both modes).
#[derive(Default)]
struct Plan {
    from: Option<*mut Context>,
    to: Option<*const Context>,
    to_tls: u64,
    to_gs: u64,
    to_fs_selector: u16,
    to_tls_entries: [u64; 3],
    switch_user: Option<task::UserMachine>,
    save_to: Option<*mut u8>,
    load_from: Option<*mut u8>,
    expired: usize,
}

/// The `percpu` switch decision. `None` - the mode went back to `global`
/// meanwhile; the caller takes the global path.
fn schedule_percpu(cause: Cause, block: Option<Block>) -> Option<Plan> {
    let cpu = smp::cpu();
    let mut plan = Plan::default();
    // 1. What needs the global lock, only when there is some of it: going to
    //    sleep (wakers look at the state under the lock), deferred wakes,
    //    deadlines, reaping. Without a sleep, a busy lock is left to others.
    let due = DEFERRED_WAKES_ANY.load(Ordering::Acquire)
        || REAP_PENDING.load(Ordering::Relaxed)
        || crate::time::uptime_ms() >= NEXT_DUE.load(Ordering::Relaxed);
    if block.is_some() || due {
        let guard = if block.is_some() { Some(sched_lock()) } else { SCHED.try_lock() };
        if let Some(mut sched) = guard {
            PC_LOCKED.fetch_add(1, Ordering::Relaxed);
            if !PERCPU.load(Ordering::SeqCst) {
                return None;
            }
            KICKED[cpu].store(false, Ordering::Release);
            let current = RUNNING_SLOT[cpu].load(Ordering::Relaxed);
            if let Some(block) = block {
                let woken = waitq::woken(current).is_some() || WAKE_SEQ[current].load(Ordering::SeqCst) != block.seq;
                if !woken && !is_idle_slot(current) {
                    if let Some(task) = sched.tasks[current].as_mut() {
                        if task.state == TaskState::Running {
                            set_state(current, task, TaskState::Blocked(block.wait));
                            if let Some(at) = block.until {
                                note_deadline(current, at);
                            }
                        }
                    }
                }
            }
            sched.take_deferred_wakes();
            if sched.running {
                sched.reap();
                plan.expired = sched.wake_expired(crate::time::uptime_ms());
            }
        }
    }

    // 2. The decision, without the global lock.
    IN_PERCPU[cpu].store(true, Ordering::SeqCst);
    if !PERCPU.load(Ordering::SeqCst) || !RUNNING.load(Ordering::Acquire) {
        IN_PERCPU[cpu].store(false, Ordering::SeqCst);
        return if PERCPU.load(Ordering::SeqCst) { Some(plan) } else { None };
    }
    KICKED[cpu].store(false, Ordering::Release);
    let current = RUNNING_SLOT[cpu].load(Ordering::Relaxed);
    // SAFETY: the current task of this CPU.
    let current_state = unsafe { task_at(current) }.map(|task| task.state);
    let next = match pick_percpu(cpu, current, cause) {
        Some(next) => Some(next),
        None if is_idle_slot(current) => None,
        None => match current_state {
            Some(TaskState::Running | TaskState::Ready) => {
                // Stays: woken before it went - it runs, not waits.
                // SAFETY: as above.
                if let Some(task) = unsafe { task_at(current) } {
                    if task.state == TaskState::Ready {
                        set_state(current, task, TaskState::Running);
                    }
                }
                None
            }
            // Asleep or finished, and nobody else: this CPU's idle task.
            _ => Some(cpu),
        },
    };
    if let Some(next) = next {
        // SAFETY: `current` runs here; `next` was claimed by this CPU (or is
        // its idle task).
        let (Some(source), Some(target)) = (unsafe { task_at(current) }, unsafe { task_at(next) }) else {
            // No task behind a slot (cannot happen: both are published
            // before they run): give the claim back, stay.
            if !is_idle_slot(next) && next != current {
                ON_CPU[next].store(false, Ordering::SeqCst);
                HOMEQ[cpu].set(next);
                READY.set(next);
            }
            IN_PERCPU[cpu].store(false, Ordering::SeqCst);
            return Some(plan);
        };
        if let Some(stack) = source.stack.as_ref() {
            if !stack.guard_intact() {
                report_overflow(source.name, source.id);
            }
        }
        plan.to = Some(target.context.as_ptr());
        plan.from = Some(source.context.as_mut_ptr());
        plan.switch_user = Some(target.user);
        plan.save_to = source.fpu.as_ref().map(task::FpuArea::as_ptr);
        plan.load_from = target.fpu.as_ref().map(task::FpuArea::as_ptr);
        let now = crate::time::uptime_ms();
        let now_ns = crate::time::uptime_ns();
        // The leaving task: back into this CPU's queue if it still wants
        // the CPU (claimable once `finish_switch` clears its `ON_CPU`).
        if source.state == TaskState::Running {
            set_state(current, source, TaskState::Ready);
            if !is_idle_slot(current) {
                HOME[current].store(cpu as u16, Ordering::Relaxed);
                HOMEQ[cpu].set(current);
                READY.set(current);
            }
        }
        if source.ran_since_ms != 0 {
            source.cpu_ms += now.saturating_sub(source.ran_since_ms);
            source.ran_since_ms = 0;
        }
        cputime_switch_out(current, now_ns);
        if cause == Cause::Preempted {
            source.preempted += 1;
        }
        source.fs_selector = arch::user_fs_selector();
        let (fs, gs) = arch::live_user_bases();
        if let Some(fs) = fs {
            source.tls = fs;
        }
        if let Some(gs) = gs {
            source.gs = gs;
        }
        source.cpu = None;
        LEAVING[cpu].store(current + 1, Ordering::Release);
        // The coming task.
        set_state(next, target, TaskState::Running);
        target.cpu = Some(cpu);
        target.switches += 1;
        if target.boost {
            set_boost(next, target, false);
        }
        target.ran_since_ms = now;
        cputime_switch_in(next, now_ns, target.user.space_root.map_or(0, |root| root.as_u64()));
        if target.woken_by_irq_ms != 0 {
            let waited = now.saturating_sub(target.woken_by_irq_ms);
            target.longest_irq_wait_ms = target.longest_irq_wait_ms.max(waited);
            target.woken_by_irq_ms = 0;
        }
        plan.to_tls = target.tls;
        plan.to_gs = target.gs;
        plan.to_fs_selector = target.fs_selector;
        plan.to_tls_entries = target.tls_entries;
        if !is_idle_slot(next) {
            LAST_CPU[next].store(cpu as u16, Ordering::Relaxed);
        }
        // What this CPU runs, for the floor of virtual time (25.11).
        prio::on_pick(next, cpu);
        ON_CPU[next].store(true, Ordering::SeqCst);
        PREV_SLOT[cpu].store(current, Ordering::Relaxed);
        RUNNING_SLOT[cpu].store(next, Ordering::Relaxed);
        PC_SWITCHES[cpu].fetch_add(1, Ordering::Relaxed);
        if cause == Cause::Preempted {
            PC_FORCED[cpu].fetch_add(1, Ordering::Relaxed);
        }
        SLICE_LEFT[cpu].store(SLICE_TICKS, Ordering::Relaxed);
        ON_IDLE[cpu].store(is_idle_slot(next), Ordering::Relaxed);
    }
    IN_PERCPU[cpu].store(false, Ordering::SeqCst);
    Some(plan)
}

/// Ожидание замка планировщика (20.11, `perf-diag`): сколько раз замок
/// оказался занят и сколько наносекунд ждали. На игре с 16 процессорами
/// через него идут все переключения, futex и пробуждения.
static SCHED_WAITS: AtomicU64 = AtomicU64::new(0);
static SCHED_WAIT_NS: AtomicU64 = AtomicU64::new(0);

/// `SCHED.lock()` со счётом ожидания: свободный замок — без часов.
#[track_caller]
fn sched_lock() -> crate::sync::SpinGuard<'static, Scheduler> {
    if !SCHED.is_locked() {
        return SCHED.lock();
    }
    let since = crate::time::uptime_ns();
    let guard = SCHED.lock();
    let waited = crate::time::uptime_ns().saturating_sub(since);
    SCHED_WAITS.fetch_add(1, Ordering::Relaxed);
    SCHED_WAIT_NS.fetch_add(waited, Ordering::Relaxed);
    note_wait_site(core::panic::Location::caller(), waited);
    guard
}

/// Кто ждал замок планировщика дольше всех (место вызова, раз, нс) — для
/// `perf-diag`: 16 ячеек, место ищется линейно; не нашлось ячейки — в счёт
/// не идёт. Пишется под замком (вызывающий его уже взял).
static WAIT_SITES: [AtomicUsize; 16] = [const { AtomicUsize::new(0) }; 16];
static WAIT_SITE_COUNT: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
static WAIT_SITE_NS: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

fn note_wait_site(site: &'static core::panic::Location<'static>, waited: u64) {
    let key = core::ptr::from_ref(site).addr();
    for (index, cell) in WAIT_SITES.iter().enumerate() {
        let now = cell.load(Ordering::Relaxed);
        if now == 0 {
            cell.store(key, Ordering::Relaxed);
        } else if now != key {
            continue;
        }
        WAIT_SITE_COUNT[index].fetch_add(1, Ordering::Relaxed);
        WAIT_SITE_NS[index].fetch_add(waited, Ordering::Relaxed);
        return;
    }
}

/// Три места, ждавшие замок дольше всех с прошлого вызова: `(файл:строка,
/// раз, нс)`; счёт обнуляется.
pub fn take_lock_wait_sites() -> alloc::vec::Vec<(alloc::string::String, u64, u64)> {
    let mut rows = alloc::vec::Vec::new();
    {
        // Под замком: `note_wait_site` пишет ячейки тоже под ним.
        let _sched = SCHED.lock();
        for index in 0..WAIT_SITES.len() {
            let key = WAIT_SITES[index].swap(0, Ordering::Relaxed);
            let count = WAIT_SITE_COUNT[index].swap(0, Ordering::Relaxed);
            let ns = WAIT_SITE_NS[index].swap(0, Ordering::Relaxed);
            if key != 0 && count != 0 {
                rows.push((key, count, ns));
            }
        }
    }
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    rows.truncate(3);
    rows.into_iter()
        .map(|(key, count, ns)| {
            // SAFETY: ключ — адрес `&'static Location`, записанный выше.
            let site: &'static core::panic::Location<'static> = unsafe { &*(key as *const core::panic::Location<'static>) };
            (alloc::format!("{}:{}", site.file().rsplit(['/', '\\']).next().unwrap_or(""), site.line()), count, ns)
        })
        .collect()
}

/// Ожидания замка планировщика: `(раз, нс)`.
#[must_use]
pub fn lock_wait_stats() -> (u64, u64) {
    (SCHED_WAITS.load(Ordering::Relaxed), SCHED_WAIT_NS.load(Ordering::Relaxed))
}

/// Слот задачи, исполняющейся на процессоре, — без замка планировщика
/// (владелец мьютекса для сторожа замков).
static RUNNING_SLOT: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];
/// С какой миллисекунды задача спит на замке, и о каком замке уже сказано.
static LOCK_SINCE: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
static LOCK_REPORTED: [AtomicUsize; MAX_TASKS] = [const { AtomicUsize::new(0) }; MAX_TASKS];

/// Слот задачи на этом процессоре (без замка планировщика).
#[must_use]
pub fn running_slot() -> usize {
    RUNNING_SLOT[smp::cpu()].load(Ordering::Relaxed)
}

/// Slot of the task running on another CPU right now (`None` - its idle
/// task or no such CPU). A racy snapshot, for diagnostics only.
#[must_use]
pub fn running_slot_on(cpu: usize) -> Option<usize> {
    let slot = RUNNING_SLOT.get(cpu)?.load(Ordering::Relaxed);
    (!is_idle_slot(slot)).then_some(slot)
}

// --- CPU time without the scheduler lock (hw-log14) --------------------------
//
// D2R called getrusage ~0.9 M times a second next to sched_yield (2:1) and got
// zeros: its spin-with-a-CPU-time-budget never saw time move. `cpu_ms` takes
// the scheduler lock and walks the table - not something to call a million
// times a second. These per-slot atomics are written at the switch (under the
// lock, which serializes writers) and read lock-free: nanoseconds run so far,
// the start of the running stretch (0 - not running) and the address space
// of the task (threads of one process share it).
static CPU_NS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
static RUN_START_NS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
static SLOT_SPACE: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// The largest process time a thread has been told (keeps RUSAGE_SELF
/// monotonic for each caller when a dead sibling's slot is reused).
static SELF_SEEN_NS: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];

fn cputime_switch_out(slot: usize, now_ns: u64) {
    let start = RUN_START_NS[slot].swap(0, Ordering::Relaxed);
    if start != 0 {
        let ran = now_ns.saturating_sub(start);
        CPU_NS[slot].fetch_add(ran, Ordering::Relaxed);
        // Virtual run time and the share by nice (25.11, `prio`).
        prio::charge(slot, ran);
    }
}

fn cputime_switch_in(slot: usize, now_ns: u64, space: u64) {
    if space != 0 {
        SLOT_SPACE[slot].store(space, Ordering::Relaxed);
    }
    RUN_START_NS[slot].store(now_ns.max(1), Ordering::Relaxed);
}

/// A new task in `slot`: its time starts from zero.
fn cputime_reset(slot: usize) {
    CPU_NS[slot].store(0, Ordering::Relaxed);
    RUN_START_NS[slot].store(0, Ordering::Relaxed);
    SLOT_SPACE[slot].store(0, Ordering::Relaxed);
    SELF_SEEN_NS[slot].store(0, Ordering::Relaxed);
}

fn slot_cpu_ns(slot: usize, now_ns: u64) -> u64 {
    let start = RUN_START_NS[slot].load(Ordering::Relaxed);
    let done = CPU_NS[slot].load(Ordering::Relaxed);
    done + if start != 0 { now_ns.saturating_sub(start) } else { 0 }
}

/// CPU time of the running task, ns (`RUSAGE_THREAD`,
/// `CLOCK_THREAD_CPUTIME_ID`). No lock.
#[must_use]
pub fn thread_cpu_ns() -> u64 {
    // Interrupts off so the task is not switched out between the two reads.
    arch::interrupts::without_interrupts(|| slot_cpu_ns(running_slot(), crate::time::uptime_ns()))
}

/// CPU time of the running task's process - the tasks that share its address
/// space - ns (`RUSAGE_SELF`, `CLOCK_PROCESS_CPUTIME_ID`). Threads that have
/// exited count until their slot is reused; the answer never goes back for
/// one caller. No lock: a walk over the used slots.
#[must_use]
pub fn process_cpu_ns() -> u64 {
    let (me, now) = arch::interrupts::without_interrupts(|| (running_slot(), crate::time::uptime_ns()));
    let space = SLOT_SPACE[me].load(Ordering::Relaxed);
    let total = if space == 0 {
        slot_cpu_ns(me, now)
    } else {
        let high = HIGH.load(Ordering::Acquire).min(MAX_TASKS);
        (MAX_CPUS..high)
            .filter(|&slot| SLOT_SPACE[slot].load(Ordering::Relaxed) == space)
            .map(|slot| slot_cpu_ns(slot, now))
            .sum()
    };
    SELF_SEEN_NS[me].fetch_max(total, Ordering::Relaxed).max(total)
}

/// Сторож замков (фаза 74): раз в 5 с ищет задачи, спящие на мьютексе
/// дольше 15 с, и печатает цепочку «кто ждёт — кто держит — чего ждёт
/// держатель». Только по таблице планировщика: имя программы спрашивается у
/// таблицы программ под её замком, а взаимная блокировка как раз на нём
/// (Rider в VirtualBox, 29.10) повесила бы и сторожа.
pub fn lock_watch_task() {
    loop {
        sleep_ms(5_000);
        let now = crate::time::uptime_ms();
        let mut found: alloc::vec::Vec<(usize, &'static str, usize, u64, usize, &'static str, Option<(&'static str, u64)>, usize)> =
            alloc::vec::Vec::new();
        {
            let sched = sched_lock();
            for (slot, entry) in sched.slots().iter().enumerate() {
                let Some(task) = entry else { continue };
                let TaskState::Blocked(Wait::Lock(address)) = task.state else { continue };
                let since = LOCK_SINCE[slot].load(Ordering::Relaxed);
                if since == 0 || now.saturating_sub(since) < 15_000 || LOCK_REPORTED[slot].load(Ordering::Relaxed) == address {
                    continue;
                }
                LOCK_REPORTED[slot].store(address, Ordering::Relaxed);
                // SAFETY: задача спит на этом мьютексе, значит он жив; первое
                // поле `Mutex` — владелец (`repr(C)`).
                let owner = unsafe { (*(address as *const AtomicUsize)).load(Ordering::Relaxed) };
                // SAFETY: второе поле — место захвата (`Location` или ноль).
                let site = unsafe { (*((address + 8) as *const AtomicUsize)).load(Ordering::Relaxed) };
                let (holder_name, holder_state) = match owner.checked_sub(1).and_then(|h| sched.tasks.get(h)?.as_ref()) {
                    Some(holder) => (holder.name, holder.state.waiting_for()),
                    None => ("?", None),
                };
                if found.try_reserve(1).is_ok() {
                    found.push((slot, task.name, address, now - since, owner.wrapping_sub(1), holder_name, holder_state, site));
                }
            }
        }
        for (slot, name, address, waited, holder, holder_name, holder_state, site) in found {
            if site != 0 {
                // SAFETY: место пишет `Mutex::lock` — `&'static Location`.
                let at = unsafe { &*(site as *const core::panic::Location<'static>) };
                kprintln!("  lockwatch   : the lock at {address:#x} was taken at {}:{}", at.file(), at.line());
            }
            match holder_state {
                Some((what, number)) => kprintln!(
                    "  lockwatch   : #{slot} {name} waits {} s for the lock at {address:#x}, held by #{holder} {holder_name}, which is {what} {number:#x}",
                    waited / 1000
                ),
                None => kprintln!(
                    "  lockwatch   : #{slot} {name} waits {} s for the lock at {address:#x}, held by #{holder} {holder_name} (running or ready)",
                    waited / 1000
                ),
            }
        }
    }
}

/// Принадлежит ли слот холостой задаче какого-нибудь процессора.
/// Очередь вне очереди программ (см. [`Sched::pick_next`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Rank {
    /// Драйверы, отвечающие на прерывания: ввод (USB, тачскрин телефона) и
    /// сеть — всегда первыми; они крошечные и почти всё время спят.
    Driver,
    /// Стол и оболочка (`shell`), сервер Wayland — пока человек ждёт ответа.
    Desktop,
}

fn rank_of(name: &str) -> Option<Rank> {
    match name {
        // `usb-gadget` и `powerkey` — телефон (10.11): связь по кабелю и
        // пинальщик сторожевого таймера голодали за тяжёлой страницей, и
        // сторож перезагружал живую машину.
        "usb" | "ehci" | "ohci" | "touch" | "net" | "usb-gadget" | "powerkey" => Some(Rank::Driver),
        "shell" | "wayland" => Some(Rank::Desktop),
        _ => None,
    }
}

const fn is_idle_slot(slot: usize) -> bool {
    slot < MAX_CPUS
}

impl Scheduler {
    const fn new() -> Self {
        let mut cpus = [const { Cpu { current: 0, previous: None, switches: 0, forced: 0 } }; MAX_CPUS];
        // Холостая задача процессора N лежит в слоте N, и с неё он начинает.
        let mut index = 0;
        while index < MAX_CPUS {
            cpus[index].current = index;
            index += 1;
        }
        Self { tasks: [const { None }; MAX_TASKS], cpus, next_id: 1, high: MAX_CPUS, running: false }
    }

    /// Слот задачи, исполняющейся на процессоре вызывающего.
    ///
    /// Вызывать только под локом: лок запрещает прерывания, и номер процессора
    /// не может смениться между вопросом и ответом.
    fn current(&self) -> usize {
        // `RUNNING_SLOT`, written with every switch in both modes (a
        // `percpu` switch does not touch `cpus`).
        RUNNING_SLOT[smp::cpu()].load(Ordering::Relaxed)
    }

    /// Куда положить новую задачу.
    ///
    /// Сначала пустой слот, потом — самый давний из завершённых, чей стек уже
    /// никем не исполняется. Второй проход появился вместе с программами-
    /// задачами: каждая команда `run` создаёт новую, и таблица кончилась бы на
    /// тридцать второй программе.
    fn slots(&self) -> &[Option<Box<Task>>] {
        &self.tasks[..self.high]
    }

    fn slots_mut(&mut self) -> &mut [Option<Box<Task>>] {
        let high = self.high;
        &mut self.tasks[..high]
    }

    fn free_slot(&self) -> Option<usize> {
        if let Some(empty) = (MAX_CPUS..MAX_TASKS).find(|&i| self.tasks[i].is_none()) {
            return Some(empty);
        }
        (MAX_CPUS..MAX_TASKS)
            .filter(|&i| {
                self.tasks[i]
                    .as_ref()
                    .is_some_and(|task| task.state == TaskState::Finished && off_cpu(i))
            })
            .min_by_key(|&i| self.tasks[i].as_ref().map_or(u32::MAX, |task| task.id.as_u32()))
    }

    /// Кого исполнять следующим на процессоре `cpu`.
    ///
    /// Round-robin: обход от слота за текущим по кругу, первая готовая задача,
    /// **чей контекст не жив ни на одном процессоре**, побеждает. Холостые
    /// задачи в обход не входят — ни своя, ни чужие.
    ///
    /// `None` означает «оставить всё как есть». Это ответ в двух случаях:
    /// процессор уже простаивает и брать нечего, либо текущая задача всё ещё
    /// хочет процессор, а больше его не хочет никто. Второй случай до фазы 43
    /// отвечал «уйти в холостую», и на одном процессоре это стоило лишь пары
    /// переключений; на нескольких такая задача на миг становилась готовой и
    /// ничьей — и её было видно в `tasks` вместо «исполняется».
    fn pick_next(&self, cpu: usize, cause: Cause) -> Option<usize> {
        let current = RUNNING_SLOT[cpu].load(Ordering::Relaxed);
        // One walk over the ready set (G2, 21.11.2026) instead of three walks
        // over every slot: round-robin from the slot after the current one,
        // the first boosted task wins at once, otherwise remember the first
        // driver, the first desktop task and the first task of any kind -
        // the same order of preference as the three walks had.
        let high = self.high;
        let (mut boosted, mut driver, mut desktop, mut any) = (None, None, None, None);
        // `prio on` (25.11): the task with the least virtual run time.
        let weighted = prio::picker();
        let mut fair: Option<(usize, i64)> = None;
        READY.scan_from(current + 1, high, |slot| {
            if is_idle_slot(slot) {
                READY.clear(slot);
                return false;
            }
            if slot == current {
                return false;
            }
            let Some(task) = self.tasks[slot].as_ref() else {
                READY.clear(slot);
                return false;
            };
            if task.state != TaskState::Ready {
                READY.clear(slot);
                return false;
            }
            if !off_cpu(slot) {
                return false;
            }
            if task.boost {
                boosted = Some(slot);
                return true;
            }
            match rank_of(task.name) {
                Some(Rank::Driver) if driver.is_none() => driver = Some(slot),
                Some(Rank::Desktop) if desktop.is_none() => desktop = Some(slot),
                _ => {}
            }
            if any.is_none() {
                any = Some(slot);
            }
            if weighted {
                let lag = prio::lag(slot);
                if fair.is_none_or(|(_, best)| lag < best) {
                    fair = Some((slot, lag));
                }
            }
            false
        });
        if let Some(slot) = boosted.or(driver) {
            return Some(slot);
        }
        if desktop.is_some() && (crate::input::has_events_now() || crate::ui::input_waiting()) {
            return desktop;
        }
        if let Some((slot, lag)) = fair {
            // The end of a slice: the running task stays while it still has
            // the least virtual run time.
            let running = !is_idle_slot(current)
                && self.tasks[current].as_ref().is_some_and(|task| matches!(task.state, TaskState::Running | TaskState::Ready));
            if running && cause == Cause::Preempted && lag >= prio::live_lag(current, crate::time::uptime_ns()) {
                prio::on_stay(current, cpu, crate::time::uptime_ns());
                prio::STAYS.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            return Some(slot);
        }
        if any.is_some() {
            return any;
        }
        if is_idle_slot(current) {
            return None;
        }
        match self.tasks[current].as_ref().map(|task| task.state) {
            Some(TaskState::Running | TaskState::Ready) => None,
            _ => Some(cpu),
        }
    }

    /// Make the task in `slot` ready (and visible to `pick_next`).
    fn set_ready(&mut self, slot: usize) {
        if let Some(task) = self.tasks[slot].as_mut() {
            set_state(slot, task, TaskState::Ready);
            if !is_idle_slot(slot) {
                if PERCPU.load(Ordering::Relaxed) {
                    enqueue_percpu(slot);
                } else {
                    prio::on_wake(slot, usize::from(LAST_CPU[slot].load(Ordering::Relaxed)));
                    READY.set(slot);
                }
            }
        }
    }

    /// Пробуждения, отложенные `wake` при занятом замке (13.11).
    fn take_deferred_wakes(&mut self) {
        if !DEFERRED_WAKES_ANY.swap(false, Ordering::AcqRel) {
            return;
        }
        if DEFERRED_OVERFLOW.swap(false, Ordering::AcqRel) {
            for slot in MAX_CPUS..self.high {
                if self.tasks[slot].as_ref().is_some_and(|task| matches!(task.state, TaskState::Blocked(_))) {
                    self.set_ready(slot);
                }
            }
        }
        for cell in &DEFERRED_WAKES {
            let raw = cell.swap(0, Ordering::AcqRel);
            if raw == 0 {
                continue;
            }
            let id = TaskId::new(raw);
            if let Some(slot) = slot_of(id) {
                if self.tasks[slot].as_ref().is_some_and(|task| task.id == id && matches!(task.state, TaskState::Blocked(_))) {
                    self.set_ready(slot);
                }
            }
        }
    }

    /// Разбудить всех, чей срок ожидания вышел.
    ///
    /// Проход по всей таблице на каждом переключении — это сорок сравнений, то
    /// есть заметно дешевле одного обращения к памяти мимо кеша.
    ///
    /// Возвращает, сколько задач проснулось.
    fn wake_expired(&mut self, now: u64) -> usize {
        // Ближайший срок известен (20.11): до него таблицу не обходить. На
        // игре это ~30 тыс. переключений в секунду по 100-200 задач Wine под
        // общим замком.
        if now < NEXT_DUE.load(Ordering::Relaxed) {
            return 0;
        }
        // Only the tasks that went to sleep with a deadline (G2).
        let mut woke = 0;
        let mut next = u64::MAX;
        let mut expired = [0u16; 64];
        let mut count = 0;
        let high = self.high;
        TIMED.scan_from(0, high, |slot| {
            let tick = match self.tasks[slot].as_ref().map(|task| task.state) {
                Some(TaskState::Blocked(
                    Wait::Until(tick)
                    | Wait::Input(tick)
                    | Wait::IrqUntil(_, tick)
                    | Wait::FutexUntil(_, tick)
                    | Wait::FutexMany(tick),
                )) => tick,
                _ => {
                    TIMED.clear(slot);
                    return false;
                }
            };
            if now >= tick {
                if count == expired.len() {
                    // More next time: `next` stays "now".
                    next = now;
                    return true;
                }
                expired[count] = slot as u16;
                count += 1;
            } else {
                next = next.min(tick);
            }
            false
        });
        for &slot in &expired[..count] {
            TIMED.clear(usize::from(slot));
            self.set_ready(usize::from(slot));
            woke += 1;
        }
        NEXT_DUE.store(next, Ordering::Relaxed);
        woke
    }

    /// Разбудить тех, кто ждал завершения задачи `id`.
    fn wake_waiters(&mut self, id: TaskId) {
        for slot in 0..self.high {
            if self.tasks[slot].as_ref().is_some_and(|task| task.state == TaskState::Blocked(Wait::Task(id))) {
                self.set_ready(slot);
            }
        }
    }

    /// Освободить стеки завершённых задач.
    ///
    /// Задача с меткой процессора пропускается — и это не оптимизация, а условие
    /// корректности: её последний `switch_context` может исполняться прямо
    /// сейчас, на этом или на соседнем процессоре, и её стек — та самая земля под
    /// ногами.
    ///
    /// Возвращает число освобождённых стеков.
    fn reap(&mut self) -> usize {
        // Only after some task finished (G2): this walk ran on every switch.
        if !REAP_PENDING.swap(false, Ordering::AcqRel) {
            return 0;
        }
        let mut freed = 0;
        let mut left = false;
        for (slot, entry) in self.slots_mut().iter_mut().enumerate() {
            if is_idle_slot(slot) {
                continue;
            }
            if let Some(task) = entry {
                if task.state == TaskState::Finished && task.stack.is_some() {
                    if !off_cpu(slot) {
                        left = true;
                        continue;
                    }
                    // Вот здесь и происходит освобождение: `Drop for Stack`
                    // возвращает блок куче.
                    task.stack = None;
                    freed += 1;
                }
            }
        }
        if left {
            REAP_PENDING.store(true, Ordering::Release);
        }
        freed
    }

    /// Сколько задач (не считая холостых и служебных) ещё не завершились.
    ///
    /// Служебные не считаются, и это не деталь учёта: по этому числу [`run`]
    /// решает, что работы больше нет и машину пора остановить.
    fn alive(&self) -> usize {
        self.tasks
            .iter()
            .enumerate()
            .filter(|(slot, entry)| {
                !is_idle_slot(*slot)
                    && entry
                        .as_ref()
                        .is_some_and(|t| !t.daemon && t.state != TaskState::Finished)
            })
            .count()
    }

    /// Снять метку процессора с задачи, уступившей процессор вызывающему.
    /// Сколько всего было переключений, на всех процессорах.
    fn switches(&self) -> u64 {
        self.cpus.iter().map(|cpu| cpu.switches).sum::<u64>() + PC_SWITCHES.iter().map(|n| n.load(Ordering::Relaxed)).sum::<u64>()
    }

    /// Сколько из них по истёкшему кванту.
    fn forced(&self) -> u64 {
        self.cpus.iter().map(|cpu| cpu.forced).sum::<u64>() + PC_FORCED.iter().map(|n| n.load(Ordering::Relaxed)).sum::<u64>()
    }
}

/// Создать задачу.
///
/// `entry` — обычная `fn()`, которой позволено вернуться: возврат перехватит
/// [`trampoline`].
///
/// # Ошибки
///
/// [`SpawnError::OutOfMemory`] при нехватке кучи (под стек или под саму задачу),
/// [`SpawnError::TooManyTasks`] при исчерпании таблицы. Паники нет ни в одном из
/// путей: не сумев создать задачу, ядро продолжает работать с уже созданными.
pub fn spawn(name: &'static str, entry: fn()) -> Result<TaskId, SpawnError> {
    // Точка входа уезжает батуту аргументом: `Context::new` умеет передать
    // ровно одно машинное слово, а `fn()` в него помещается целиком.
    spawn_raw(name, TASK_STACK_SIZE, trampoline, entry as usize)
}

/// Создать служебную задачу — ту, которая работает, пока работает система, и
/// сама по себе поводом ей работать не является.
///
/// # Ошибки
///
/// Те же, что у [`spawn`].
pub fn spawn_daemon(name: &'static str, entry: fn()) -> Result<TaskId, SpawnError> {
    let id = spawn(name, entry)?;
    mark_daemon(id);
    Ok(id)
}

/// Объявить уже созданную задачу служебной.
pub fn mark_daemon(id: TaskId) {
    let mut sched = sched_lock();
    if let Some(task) = sched.slots_mut().iter_mut().flatten().find(|task| task.id == id) {
        if !task.daemon && task.state != TaskState::Finished {
            ALIVE.fetch_sub(1, Ordering::Relaxed);
        }
        task.daemon = true;
    }
}

/// Служебная ли задача исполняется сейчас.
///
/// Спрашивает [`crate::user::syscall`]: службу запускает служба, и ребёнок
/// супервизора обязан унаследовать это свойство.
#[must_use]
pub fn is_daemon() -> bool {
    let sched = sched_lock();
    let current = sched.current();
    sched.tasks[current].as_ref().is_some_and(|task| task.daemon)
}

/// Создать задачу с собственной точкой входа и своим размером стека.
///
/// Нужно программам вне ядра: их точка входа не возвращается вовсе (она сама
/// завершает задачу), а стека им требуется заметно больше.
///
/// # Ошибки
///
/// [`SpawnError::OutOfMemory`] при нехватке кучи (под стек или под саму задачу),
/// [`SpawnError::TooManyTasks`] при исчерпании таблицы. Паники нет ни в одном из
/// путей.
pub fn spawn_raw(
    name: &'static str,
    stack_size: usize,
    entry: extern "C" fn(usize) -> !,
    arg: usize,
) -> Result<TaskId, SpawnError> {
    // Стек выделяется до захвата лока: аллокатор кучи при отказе печатает
    // многострочную диагностику, и делать это, удерживая лок планировщика,
    // значит растягивать критическую секцию на скорость UART.
    let stack = Stack::allocate(stack_size).ok_or(SpawnError::OutOfMemory)?;

    let mut sched = sched_lock();
    let Some(slot) = sched.free_slot() else {
        // `stack` уничтожается после охранника (обратный порядок объявления
        // локальных), то есть куча трогается уже без лока.
        return Err(SpawnError::TooManyTasks);
    };

    let id = TaskId::new(sched.next_id);
    // Сессия терминала — от того, кто порождает.
    let parent = sched.current();
    SESSION[slot].store(SESSION[parent].load(Ordering::Relaxed), Ordering::Relaxed);
    // Контекст ведёт в общий батут, а настоящая точка входа ждёт в самой задаче:
    // первое, что обязан сделать новый стек, — снять метку с того, кто уступил
    // ему процессор (см. [`task_start`]).
    let mut task = Task::new(id, name, stack, task_start, slot);
    task.entry = Some(entry);
    task.arg = arg;
    let boxed = box_task(task).ok_or(SpawnError::OutOfMemory)?;

    sched.next_id += 1;
    cputime_reset(slot);
    // Nice and policy from the parent (fork, clone, spawn - as Linux).
    prio::on_spawn(slot, parent);
    sched.tasks[slot] = Some(boxed);
    publish(&mut sched, slot);
    match rank_of(name) {
        Some(Rank::Driver) => DRIVERS.set(slot),
        _ => DRIVERS.clear(slot),
    }
    match rank_of(name) {
        Some(Rank::Desktop) => DESKTOPS.set(slot),
        _ => DESKTOPS.clear(slot),
    }
    BOOSTED.clear(slot);
    LAST_CPU[slot].store(smp::cpu() as u16, Ordering::Relaxed);
    sched.high = sched.high.max(slot + 1);
    HIGH.store(sched.high, Ordering::Release);
    SLOT_ID[slot].store(id.as_u32(), Ordering::Relaxed);
    ALIVE.fetch_add(1, Ordering::Relaxed);
    // A new task is `Ready` from the start.
    RUNSTATE[slot].store(RS_READY, Ordering::SeqCst);
    if PERCPU.load(Ordering::Relaxed) {
        enqueue_percpu(slot);
    } else {
        READY.set(slot);
    }
    Ok(id)
}

/// The slot got (or lost) its task: publish the pointer for the lock-free
/// switch. Under the lock.
fn publish(sched: &mut Scheduler, slot: usize) {
    // The pointer before the state: a CPU that sees "ready" (with a stale
    // queue bit) must find this task, not the one that had the slot before.
    let pointer = sched.tasks[slot].as_mut().map_or(ptr::null_mut(), |task| &mut **task as *mut Task);
    TASK_PTR[slot].store(pointer, Ordering::SeqCst);
    let state = sched.tasks[slot].as_ref().map_or(RS_EMPTY, |task| runstate_of(task.state));
    RUNSTATE[slot].store(state, Ordering::SeqCst);
}

/// Сколько сессий терминала (вкладок) бывает одновременно.
pub const MAX_SESSIONS: usize = 8;

/// Вкладка терминала, которой принадлежит задача: её ввод, её вывод, её
/// программа переднего плана. Наследуется при порождении — программа,
/// запущенная во вкладке, и её дети печатают туда же. Ноль — первая вкладка
/// (и всё, что запущено не из терминала: службы, стол).
static SESSION: [AtomicU8; MAX_TASKS] = [const { AtomicU8::new(0) }; MAX_TASKS];

/// Сессия терминала текущей задачи.
#[must_use]
pub fn session() -> usize {
    usize::from(SESSION[current_slot()].load(Ordering::Relaxed)).min(MAX_SESSIONS - 1)
}

/// Отнести текущую задачу к сессии `session` (задача оболочки вкладки — при
/// старте).
pub fn set_session(session: usize) {
    SESSION[current_slot()].store(session.min(MAX_SESSIONS - 1) as u8, Ordering::Relaxed);
}

/// Задать базу хранилища потока для исполняющейся задачи.
///
/// Ставится дважды и это не лишнее: в задаче — чтобы переключение контекста
/// вернуло её при следующем заходе, и в регистре — чтобы она подействовала
/// прямо сейчас, без ожидания переключения.
pub fn set_current_tls(base: u64) {
    // Всё — под локом (прерывания запрещены, 13.11): вытеснение между записью
    // в задачу и сбросом селектора возвращало в задачу старый селектор `FS`
    // (переключение сохраняет живой), и поток бежал с чужой базой.
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.tls = base;
        task.fs_selector = 0;
    }
    // Как `arch_prctl(ARCH_SET_FS)` Linux: селектор `FS` — нулевой, база — из
    // MSR (12.11: обработчик сигнала Wine, пойманного в 32-битном коде).
    arch::clear_fs_selector();
    arch::set_user_tls(base);
    drop(sched);
}

/// База `FS` и `GS` исполняющейся задачи — из неё самой, под локом (13.11):
/// кеш процессора (`arch::user_tls`) читается в два шага, и вытеснение с
/// переносом между ними давало ребёнку `clone` базы чужой задачи.
#[must_use]
pub fn current_bases() -> (u64, u64) {
    let sched = sched_lock();
    let (tls, gs) = sched.tasks[sched.current()].as_ref().map_or((0, 0), |task| (task.tls, task.gs));
    // Истина — в процессоре (20.11): задача пишет в себя базы только на
    // переключении, а между ними программа меняет их `wrfsbase` или быстрым
    // `arch_prctl`.
    let (fs_now, gs_now) = arch::live_user_bases();
    (fs_now.unwrap_or(tls), gs_now.unwrap_or(gs))
}

/// Дескрипторы TLS исполняющейся задачи (`get_thread_area`, поиск свободной
/// записи `set_thread_area`).
#[must_use]
pub fn current_tls_entries() -> [u64; 3] {
    let sched = sched_lock();
    sched.tasks[sched.current()].as_ref().map_or([0; 3], |task| task.tls_entries)
}

/// Записать дескриптор TLS `slot` исполняющейся задачи — в неё и в GDT этого
/// процессора (`set_thread_area`, 12.11).
pub fn set_current_tls_entry(slot: usize, descriptor: u64) {
    if slot >= 3 {
        return;
    }
    // Под локом планировщика прерывания запрещены: задачу не перенесут на
    // другой процессор между записью в неё и в GDT этого.
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.tls_entries[slot] = descriptor;
    }
    arch::set_tls_entry(slot, descriptor);
    drop(sched);
}

/// Задать базу `GS` исполняющейся задачи (программы Linux на x86-64,
/// `arch_prctl(ARCH_SET_GS)`; Wine держит там TEB) — как [`set_current_tls`].
pub fn set_current_gs(base: u64) {
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.gs = base;
    }
    arch::set_user_gs(base);
    drop(sched);
}

/// `arch_prctl(ARCH_SET_FS)` без замка планировщика (20.11). Wine без
/// FSGSBASE зовёт его на каждом переходе PE <-> unix — до 900 тыс. раз в
/// секунду, и каждый вызов брал общий замок `SCHED` со всех 16 процессоров.
/// В задачу база попадает на переключении ([`arch::live_user_bases`]), а
/// текущую [`current_bases`] читает из процессора. Прерывания закрыты, чтобы
/// задача не переехала между записью регистра и кеша.
pub fn set_current_tls_fast(base: u64) {
    if !cfg!(target_arch = "x86_64") {
        set_current_tls(base);
        return;
    }
    arch::interrupts::without_interrupts(|| {
        arch::clear_fs_selector();
        arch::set_user_tls(base);
    });
}

/// `arch_prctl(ARCH_SET_GS)` без замка — как [`set_current_tls_fast`].
pub fn set_current_gs_fast(base: u64) {
    if !cfg!(target_arch = "x86_64") {
        set_current_gs(base);
        return;
    }
    arch::interrupts::without_interrupts(|| arch::set_user_gs(base));
}

/// Номер слота исполняющейся задачи.
///
/// Слот, а не идентификатор: по нему индексируются таблицы, которые ведут о
/// задаче другие модули — адресное пространство программы и её открытые файлы
/// (см. [`crate::user`]). Слот задачи не меняется, когда её переносят на другой
/// процессор, поэтому ответ остаётся верным и после вытеснения.
#[must_use]
pub fn current_slot() -> usize {
    // Без замка (20.11): его брал почти каждый системный вызов программы
    // (`user::current_process`), а это — общий замок всех процессоров. При
    // закрытых прерываниях задача не уедет с процессора, и `RUNNING_SLOT`
    // этого процессора — её слот (пишется вместе с `cpus[].current`).
    arch::interrupts::without_interrupts(running_slot)
}

/// Приписать исполняющейся задаче `bytes` байт обмена с носителем.
///
/// Вызывать без замка планировщика на руках: он берётся здесь. Обмен с
/// носителем длится миллисекунды, взятие замка рядом с ним незаметно.
pub fn charge_disk(bytes: usize) {
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.disk_bytes = task.disk_bytes.saturating_add(bytes as u64);
    }
}

/// Приписать исполняющейся задаче `bytes` байт сетевого обмена. Условие то
/// же, что у [`charge_disk`]: замок планировщика не должен быть взят.
pub fn charge_net(bytes: usize) {
    if bytes == 0 {
        return;
    }
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.net_bytes = task.net_bytes.saturating_add(bytes as u64);
    }
}

/// Холостая ли это задача.
///
/// Существует ради одного ответа, который система давала неверно: `tasks`
/// показывает `#0 idle`, а `kill 0` отвечал «такой задачи не существует».
/// Задача существует — просто [`lookup`] намеренно её не отдаёт, чтобы её
/// нельзя было снять. Два разных ответа об одном и том же — хуже, чем отказ:
/// человек, увидев «не существует», ищет ошибку в номере, а её там нет.
#[must_use]
pub fn is_idle(id: TaskId) -> bool {
    let sched = sched_lock();
    sched
        .tasks
        .iter()
        .enumerate()
        .any(|(slot, entry)| match entry {
            Some(task) => task.id == id && is_idle_slot(slot),
            None => false,
        })
}

/// Слот и состояние задачи по её идентификатору.
///
/// `None` означает «такой задачи нет»: либо она никогда не создавалась, либо её
/// слот уже переиспользован под другую. Холостые задачи сюда не попадают
/// намеренно — снять их нельзя, а отличить «нет такой» от «эту нельзя»
/// помогает [`is_idle`].
#[must_use]
pub fn lookup(id: TaskId) -> Option<(usize, TaskState)> {
    let sched = sched_lock();
    sched
        .tasks
        .iter()
        .enumerate()
        .find_map(|(slot, entry)| match entry {
            Some(task) if task.id == id && !is_idle_slot(slot) => Some((slot, task.state)),
            _ => None,
        })
}

/// Task id in a slot without the lock (0 - none): a hint, as `SLOT_ID`.
#[must_use]
pub fn slot_task_id(slot: usize) -> Option<u32> {
    SLOT_ID.get(slot).map(|id| id.load(Ordering::Relaxed)).filter(|&id| id != 0)
}

/// Name of the task in a slot (perf-diag).
#[must_use]
pub fn slot_task_name(slot: usize) -> Option<&'static str> {
    let sched = sched_lock();
    sched.tasks.get(slot).and_then(Option::as_ref).map(|task| task.name)
}

/// Идентификатор задачи в слоте — обратное к [`lookup`].
///
/// Нужен тому, кто знает слоты, а не задачи: таблица программ разложена по
/// слотам, а будить надо по идентификатору. `None` — слот пуст или холостой.
#[must_use]
pub fn id_of_slot(slot: usize) -> Option<TaskId> {
    if is_idle_slot(slot) {
        return None;
    }
    let sched = sched_lock();
    sched.tasks.get(slot).and_then(Option::as_ref).map(|task| task.id)
}

/// Объявить, в каком адресном пространстве исполняется текущая задача.
///
/// `None` возвращает её в пространство ядра. Значение переставляет процессор не
/// здесь, а на ближайшем переключении задач; сам переход выполняет вызывающий,
/// потому что он же и знает, в какой момент это безопасно.
pub fn set_current_space(root: Option<crate::mm::PhysAddr>) {
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.user.space_root = root;
    }
    // An exiting thread drops its space but its time stays with the process.
    if let Some(root) = root {
        SLOT_SPACE[current].store(root.as_u64(), Ordering::Relaxed);
    }
}

/// Запомнить стек, на который вернётся текущая задача из третьего кольца.
///
/// Зовёт его вход в программу (`arch::enter_user`), сложив свой кадр. До фазы 43
/// значение лежало в глобальной переменной арх-слоя, а планировщик переставлял
/// его на каждом переключении; на двух процессорах глобальная переменная одна на
/// двух исполняющих программы, и `exit` одной вернул бы управление в кадр
/// другой. Теперь оно живёт в задаче — там, где ему и место.
///
/// Стек ловушки выставляется здесь же, для этого процессора. Если задачу
/// перенесут на другой раньше, чем она дойдёт до третьего кольца, стек ловушки
/// там поставит [`schedule`] — из того же поля.
pub extern "C" fn remember_return_stack(stack: usize) {
    let mut sched = sched_lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.user.return_stack = stack;
    }
    arch::set_trap_stack(stack);
}

/// Стек, на который текущая задача возвращается из третьего кольца. Ноль —
/// задача программ не запускала.
pub extern "C" fn return_stack() -> usize {
    let sched = sched_lock();
    let current = sched.current();
    sched.tasks[current].as_ref().map_or(0, |task| task.user.return_stack)
}

/// Завершилась ли задача, и с каким кодом.
///
/// `None` — задача ещё жива либо её слот уже переиспользован под другую.
#[must_use]
pub fn result_of(id: TaskId) -> Option<i64> {
    let sched = sched_lock();
    sched
        .tasks
        .iter()
        .flatten()
        .find(|task| task.id == id && task.state == TaskState::Finished)
        .map(|task| task.result)
}

/// Дождаться завершения задачи и вернуть её код.
///
/// `None` означает, что ждать нечего: задачи с таким номером в таблице нет —
/// либо не было, либо её слот уже переиспользован.
pub fn wait(id: TaskId) -> Option<i64> {
    loop {
        // Проверка результата и блокировка — под одним локом. Между ними задача
        // успела бы завершиться, и разбудить ждущего стало бы некому: тот, кто
        // будит, проходит по таблице ровно один раз, в момент завершения, — и
        // делает это под тем же локом, так что на соседнем процессоре он
        // дождётся, пока мы уснём.
        {
            let mut sched = sched_lock();
            let found = sched
                .tasks
                .iter()
                .enumerate()
                .find(|(slot, entry)| {
                    !is_idle_slot(*slot) && entry.as_ref().is_some_and(|task| task.id == id)
                })
                .and_then(|(_, entry)| entry.as_ref().map(|task| (task.state, task.result)));
            match found {
                Some((TaskState::Finished, code)) => return Some(code),
                None => return None,
                Some(_) => {
                    let current = sched.current();
                    if let Some(task) = sched.tasks[current].as_mut() {
                        set_state(current, task, TaskState::Blocked(Wait::Task(id)));
                    }
                }
            }
        }
        schedule();
    }
}

/// Уснуть на указанное число миллисекунд. Ноль означает «не спать вовсе».
pub fn sleep_ms(ms: u64) {
    if ms == 0 {
        return;
    }
    sleep_until(crate::time::uptime_ms().saturating_add(ms.max(1)));
}

/// Уснуть до этой отметки времени работы системы (миллисекунды).
pub fn sleep_until(at_ms: u64) {
    {
        let mut sched = sched_lock();
        if !sched.running {
            // До запуска планирования спать не на чем и некому.
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            set_state(current, task, TaskState::Blocked(Wait::Until(at_ms)));
            note_deadline(current, at_ms);
        }
    }
    schedule();
}

/// Заблокироваться до события ввода, но не дольше тика `until`.
///
/// `changed` вызывается **под локом планировщика**: если он отвечает `true`,
/// блокировки не происходит. Это и есть защита от потерянного пробуждения: тот,
/// кто кладёт событие, сначала отмечает его, а потом будит — и будит под этим же
/// локом. Если отметка появилась после нашей проверки, будящий ждёт лок и
/// застаёт нас уже спящими; до проверки — проверка это увидит. На одном
/// процессоре окно закрывал ещё и запрет прерываний; на нескольких его
/// закрывает только порядок «отметить, затем разбудить под локом».
pub fn block_on_input(until: u64, changed: impl FnOnce() -> bool) {
    {
        let mut sched = sched_lock();
        if !sched.running || changed() {
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            set_state(current, task, TaskState::Blocked(Wait::Input(until)));
            note_deadline(current, until);
            INPUT_WAITERS.set(current);
        }
    }
    schedule();
}

/// Заблокироваться до освобождения лока по адресу `address`.
///
/// `free` вызывается **под локом планировщика**: если он отвечает `true`, лок
/// уже свободен и засыпать нельзя. Отпускающий мьютекс сначала снимает флаг, а
/// потом будит ждущих под этим же локом, поэтому освобождение между проверкой и
/// засыпанием не теряется: будящий дождётся, пока мы уснём.
///
/// Срока у ожидания нет намеренно — см. [`Wait::Lock`].
pub fn block_on_lock(address: usize, free: impl FnOnce() -> bool) {
    // Through the wait queue of the address (G2): `free` is checked after
    // the waiter is queued, and `wake_lock` takes the queue's bucket after
    // the lock was released - see `waitq`.
    LOCK_SINCE[current_slot()].store(crate::time::uptime_ms().max(1), Ordering::Relaxed);
    let _ = wq_block(waitq::Kind::Lock, &[address], Wait::Lock(address), None, free);
}

/// Заблокироваться до прерывания от источника `source`, но не дольше чем на
/// `ms` миллисекунд.
///
/// Срок хранится **временем работы системы в миллисекундах**, и с 21.09.2026
/// это принципиально. Прежде он хранился тиками — числом пришедших прерываний
/// таймера, — и это переживало ровно до того дня, когда выяснилось, что
/// прерывания теряются: под эмулятором их доставку ограничивает шаг таймера
/// хоста, и сна в три секунды хватало на сорок семь. Источник времени не
/// должен зависеть от доставки прерываний — тот же урок, что и у
/// [`crate::time::uptime_ms`], и здесь он стоил целой группы красных
/// сценариев.
///
/// (Историческое: первая редакция хранила срок в миллисекундах, а сравнивала
/// с тиками, и это тоже был дефект — но обратный по знаку. Теперь обе стороны
/// говорят на одном языке.)
///
/// `ready` — та же проверка «не пришло ли уже», что и у [`block_on_irq`], и по
/// той же причине.
pub fn block_on_irq_until(source: u32, ms: u64, ready: impl FnOnce() -> bool) {
    let until = crate::time::uptime_ms().saturating_add(ms.max(1));
    {
        let mut sched = sched_lock();
        if !sched.running || ready() {
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            set_state(current, task, TaskState::Blocked(Wait::IrqUntil(source, until)));
            note_deadline(current, until);
            IRQ_WAITERS.set(current);
        }
    }
    schedule();
}

/// Заблокироваться до прерывания от источника `source`.
///
/// `ready` вызывается **под локом планировщика** и отвечает, не пришло ли
/// событие уже. Обработчик сначала выставляет признак, а потом будит под этим же
/// локом — поэтому событие между «проверил» и «уснул» не теряется ни на одном
/// процессоре, ни на соседнем.
pub fn block_on_irq(source: u32, ready: impl FnOnce() -> bool) {
    {
        let mut sched = sched_lock();
        if !sched.running || ready() {
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            set_state(current, task, TaskState::Blocked(Wait::Irq(source)));
            IRQ_WAITERS.set(current);
        }
    }
    schedule();
}

/// Разбудить тех, кто ждёт прерывания от источника `source`.
///
/// Вызывается из обработчика прерывания, поэтому делает ровно одно: переводит
/// ждущих в готовые. Разбор события — работа задачи, а не обработчика.
pub fn wake_irq(source: u32) {
    let mut woken = 0usize;
    let mut sched = sched_lock();
    let high = sched.high;
    // Only the tasks that wait for some interrupt (G2), not every slot.
    let mut found = [0u16; 32];
    let mut count = 0;
    IRQ_WAITERS.scan_from(0, high, |slot| {
        // Оба вида ожидания прерывания, и это не перечисление ради полноты:
        // обработчик не знает и не должен знать, поставил ли ждущий себе
        // будильник. Пропущенный здесь вариант выглядел бы как «прерывания
        // настроены, а задача просыпается только по сроку».
        match sched.tasks[slot].as_ref().map(|task| task.state) {
            Some(TaskState::Blocked(Wait::Irq(waited) | Wait::IrqUntil(waited, _))) => {
                if waited == source && count < found.len() {
                    found[count] = slot as u16;
                    count += 1;
                }
            }
            _ => IRQ_WAITERS.clear(slot),
        }
        false
    });
    for &slot in &found[..count] {
        let slot = usize::from(slot);
        IRQ_WAITERS.clear(slot);
        sched.set_ready(slot);
        if let Some(task) = sched.tasks[slot].as_mut() {
            // Не ноль, даже если часы ещё на нуле: ноль значит «отметки нет».
            task.woken_by_irq_ms = crate::time::uptime_ms().max(1);
            set_boost(slot, task, true);
            woken += 1;
        }
    }
    // Счётчик обновляется после обхода, но до освобождения лока: так он не
    // может разойтись с тем, что видит следующий вызов. Стоит это одной записи
    // в атомик — на фоне обхода таблицы задач под локом это ничто.
    crate::irq::note_wake(source, woken);
    // Без `ask_reschedule`: разбуженная прерыванием задача и так первая в
    // очереди и получит процессор на ближайшем тике. Немедленная остановка
    // всех процессоров на каждом прерывании (60c582b) рвала порядок работы
    // сетевого стека: на сетевых пакетах `httpd`, `update-tls` и `driver-net`
    // теряли ответы (полный прогон 25.10; без неё — 10 из 10). Немедленно
    // переключается только ввод — см. [`wake_input`].
}

/// Разбудить тех, кто ждёт лок по адресу `address`.
///
/// Будятся **все** ждущие, а не один: проснувшиеся попробуют захватить лок по
/// очереди, и не сумевшие уснут снова.
pub fn wake_lock(address: usize) {
    // From the address's wait queue (G2): no walk over every slot.
    let woke = waitq::wake(waitq::Kind::Lock, address, 0, make_ready_slots);
    // Пинок одному простаивающему (20.11): лок возьмёт один, остальные
    // уснут снова; без пинка разбуженный ждал тика (10 мс) на холостом ЦП.
    if woke > 0 {
        kick_woken(1);
    }
}

/// Make tasks taken out of a wait queue ready - those that already sleep
/// on an address; the others will see `waitq::woken` and not block.
fn make_ready_slots(slots: &[u16]) {
    let mut sched = sched_lock();
    for &slot in slots {
        let slot = usize::from(slot);
        let sleeping = sched.tasks[slot].as_ref().is_some_and(|task| {
            matches!(
                task.state,
                TaskState::Blocked(Wait::Futex(_) | Wait::FutexUntil(..) | Wait::FutexMany(_) | Wait::Lock(_))
            )
        });
        if sleeping {
            sched.set_ready(slot);
        }
    }
}

/// What a wait on addresses ended with.
enum WqEnd {
    /// `ready` said "do not sleep" (or scheduling has not started).
    NotSlept,
    /// Slept and woke: by the key with this index, or by something else
    /// (deadline, signal, `wake`) - `None`.
    Woke(Option<usize>),
}

/// Sleep on `keys` (see `waitq` for the protocol).
fn wq_block(kind: waitq::Kind, keys: &[usize], wait: Wait, until: Option<u64>, ready: impl FnOnce() -> bool) -> WqEnd {
    if !RUNNING.load(Ordering::Acquire) {
        // Before `run` there is nothing to switch to.
        drop(ready);
        return WqEnd::NotSlept;
    }
    let slot = current_slot();
    let seq = WAKE_SEQ[slot].load(Ordering::SeqCst);
    waitq::begin(slot);
    let mut queued = 0;
    for (index, &key) in keys.iter().enumerate() {
        if !waitq::enqueue(kind, key, slot, index) {
            break;
        }
        queued += 1;
    }
    let unqueue = |count: usize| {
        for &key in &keys[..count] {
            waitq::dequeue(kind, key, slot);
        }
    };
    if queued < keys.len() {
        // No memory for an entry: return as a spurious wake-up, the caller
        // re-checks and comes back.
        unqueue(queued);
        return WqEnd::Woke(None);
    }
    core::sync::atomic::fence(Ordering::SeqCst);
    if ready() {
        unqueue(queued);
        return WqEnd::NotSlept;
    }
    WQ_SLEEPS.fetch_add(1, Ordering::Relaxed);
    schedule_with(Cause::Voluntary, Some(Block { wait, until, seq }));
    // A single key taken by its waker is already out of the queue.
    if keys.len() > 1 || waitq::woken(slot).is_none() {
        unqueue(queued);
    }
    WqEnd::Woke(waitq::woken(slot))
}

/// Sleeps through wait queues (`perf-diag`).
static WQ_SLEEPS: AtomicU64 = AtomicU64::new(0);

/// Wait-queue counters: `(sleeps, wakes by key)`.
#[must_use]
pub fn wait_queue_stats() -> (u64, u64) {
    (WQ_SLEEPS.load(Ordering::Relaxed), waitq::WAKES.load(Ordering::Relaxed))
}

/// A request to block the current task inside `schedule_with`, under the
/// same scheduler lock that picks the next task (G2: one lock round instead
/// of two per sleep).
#[derive(Clone, Copy)]
struct Block {
    wait: Wait,
    until: Option<u64>,
    /// `WAKE_SEQ` of the task when it checked its condition.
    seq: u32,
}

/// Заблокироваться на ключе `key`, пока `ready` отвечает `false`.
///
/// `ready` вызывается **под локом планировщика**, и это не подробность, а
/// единственное, что делает ожидание на адресе рабочим. Между «посмотрел на
/// число» и «уснул» не должно поместиться ничего: разбудивший, успевший в эту
/// щель, разбудил бы того, кто ещё не спит, и спящий не проснулся бы никогда.
/// Пока засыпающий держит лок планировщика, прерывания запрещены, и другой
/// задаче на этом процессоре не достанется ни такта, а на соседнем — ни лока.
///
/// `until` — отметка времени работы системы в миллисекундах, после которой
/// ждать хватит (фаза 57); `None` — без срока. Вышел ли срок, решает
/// вызывающий, сравнив часы после возврата: пробуждение и срок могли
/// прийти в один и тот же миг, и перепроверка условия всё равно на нём.
///
/// G2 (21.11.2026): the waiter sits in the wait queue of `key` and `ready`
/// runs with no lock held, after the waiter was queued (see `waitq`): the
/// waker changes the word before it takes the queue, so the window between
/// "looked at the word" and "slept" is closed by the queue, not by the
/// scheduler lock. Returns `true` when a wake on `key` ended the wait - even
/// if the deadline passed at the same moment (that wake must not be lost).
pub fn block_on_futex(key: usize, until: Option<u64>, ready: impl FnOnce() -> bool) -> bool {
    let wait = match until {
        Some(at) => Wait::FutexUntil(key, at),
        None => Wait::Futex(key),
    };
    matches!(wq_block(waitq::Kind::Futex, &[key], wait, until, ready), WqEnd::Woke(Some(_)))
}

/// Разбудить ждущих на ключе `key`. `count` в ноль означает «всех».
///
/// Возвращает, скольких разбудили: программе это нужно, чтобы отличить «никто
/// не ждал» от «разбудил» — без этого отпускающий замок не может сказать,
/// стоило ли вообще звать ядро.
pub fn wake_futex(key: usize, count: usize) -> usize {
    // Only the bucket of `key` and one short scheduler-lock round (G2).
    let woken = if count == 0 && PERCPU.load(Ordering::Relaxed) {
        // `percpu` kicks the CPU it queues a task on; not for "wake all" -
        // see below.
        let slot = current_slot();
        QUIET_WAKER[slot].store(true, Ordering::Relaxed);
        let woken = waitq::wake(waitq::Kind::Futex, key, count, make_ready_slots);
        QUIET_WAKER[slot].store(false, Ordering::Relaxed);
        woken
    } else {
        waitq::wake(waitq::Kind::Futex, key, count, make_ready_slots)
    };
    // Пинок — только адресному пробуждению (futex программы: передача работы
    // потоку). «Всех» (`count` 0) будит `io_events::notify` на каждое событие
    // ввода-вывода — с пинком это было 10 тыс. IPI в секунду и вдвое больше
    // переключений на запуске Wine (Блокнот 56 с вместо 36 с, 18.11).
    if count != 0 {
        kick_woken(woken);
    }
    woken
}

/// Пинки включены (`schedkick on|off` — сравнить на железе с прежним
/// поведением, P9).
static KICKS: AtomicBool = AtomicBool::new(true);

pub fn set_kicks(on: bool) {
    KICKS.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn kicks() -> bool {
    KICKS.load(Ordering::Relaxed)
}

/// Процессор уже пнут и ещё не заходил в планировщик — второй пинок ему не
/// нужен.
static KICKED: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// Пнуть до `count` простаивающих процессоров (P9, 18.11.2026).
///
/// Задача, ставшая готовой, раньше ждала, пока какой-нибудь процессор
/// сам зайдёт в планировщик, — у простаивающего это ближайший тик таймера,
/// до миллисекунды. На передачу работы между потоками (futex лавапайпа,
/// ответ wineserver через сокет) это ложилось на каждый шаг. Пинок — один
/// IPI на процессор, и только тем, кто стоит в `hlt`: занятый процессор
/// заберёт задачу на своём переключении.
fn kick_idle(count: usize) {
    if count == 0 || !KICKS.load(Ordering::Relaxed) || !RUNNING.load(Ordering::Relaxed) || smp::online() <= 1 {
        return;
    }
    let me = smp::cpu();
    let mut left = count;
    for cpu in 0..MAX_CPUS {
        if left == 0 {
            break;
        }
        if cpu == me || !smp::is_online(cpu) || !ON_IDLE[cpu].load(Ordering::Relaxed) {
            continue;
        }
        if kick_cpu(cpu) {
            left -= 1;
        }
    }
}

/// Пнуть один процессор, если его ещё не пнули. `false` — уже пнут.
fn kick_cpu(cpu: usize) -> bool {
    if !KICKS.load(Ordering::Relaxed) || !RUNNING.load(Ordering::Relaxed) || smp::online() <= 1 {
        return false;
    }
    if KICKED[cpu].swap(true, Ordering::SeqCst) {
        return false;
    }
    // Крутящемуся в `idle_wait` пинок — сам флаг (20.11): IPI ему не
    // нужен. Порядок SeqCst — пара с `idle_wait`: либо он увидит флаг,
    // либо мы — что он уже не крутится и идёт в `hlt`.
    if POLLING[cpu].load(Ordering::SeqCst) {
        POLL_KICKS.fetch_add(1, Ordering::Relaxed);
    } else {
        arch::smp::kick(cpu);
    }
    true
}

/// Kick after a wake-up: `percpu` mode already kicked the CPU it queued the
/// task on.
fn kick_woken(count: usize) {
    if !PERCPU.load(Ordering::Relaxed) {
        kick_idle(count);
    }
}

/// Сколько микросекунд холостой процессор ждёт пинка в цикле, прежде чем
/// уйти в `hlt` (20.11, `idlepoll N`; 0 — сразу `hlt`, как раньше). На игре
/// Wine/llvmpipe будит потоки ~12 тыс. раз в секунду (perf-diag hw-log10:
/// 373 тыс. IPI за 30 с), и каждый раз — IPI, выход из `hlt` и обработчик.
/// Процессор, только что оставшийся без работы, почти всегда получает её
/// снова через десятки микросекунд: тогда хватает флага.
static IDLE_POLL_US: AtomicU64 = AtomicU64::new(50);
/// Процессор крутится в [`idle_wait`].
static POLLING: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
/// Пинков, отданных флагом без IPI (`perf-diag`).
static POLL_KICKS: AtomicU64 = AtomicU64::new(0);

pub fn set_idle_poll_us(us: u64) {
    IDLE_POLL_US.store(us.min(10_000), Ordering::Relaxed);
}

#[must_use]
pub fn idle_poll_us() -> u64 {
    IDLE_POLL_US.load(Ordering::Relaxed)
}

#[must_use]
pub fn poll_kicks() -> u64 {
    POLL_KICKS.load(Ordering::Relaxed)
}

/// Холостой процессор между заходами в планировщик (20.11): короткое
/// ожидание пинка в цикле, затем `hlt` — без окна между проверкой флага и
/// остановкой (`arch::wait_for_interrupt_unless`): раньше IPI, пришедший до
/// `hlt`, оставлял процессор спать до тика (10 мс).
pub fn idle_wait() {
    // Zero frames for the next page faults while there is nothing else to
    // do (G2; returns at once unless faults used this CPU's cache).
    if RUNNING.load(Ordering::Relaxed) && !KICKED[smp::cpu()].load(Ordering::Relaxed) {
        crate::mm::frame::refill_idle();
        // Serial lines queued by prints that kept interrupts off (G4).
        crate::serial::pump();
    }
    let cpu = smp::cpu();
    let budget = IDLE_POLL_US.load(Ordering::Relaxed);
    if budget != 0 && KICKS.load(Ordering::Relaxed) && RUNNING.load(Ordering::Relaxed) && smp::online() > 1 {
        POLLING[cpu].store(true, Ordering::SeqCst);
        let deadline = crate::time::uptime_ns().saturating_add(budget * 1000);
        while !KICKED[cpu].load(Ordering::SeqCst) && crate::time::uptime_ns() < deadline {
            // A fine sleeper's deadline (hrsleep): wake it from here.
            if hrsleep::due() {
                hrsleep::expire();
            }
            for _ in 0..64 {
                core::hint::spin_loop();
            }
        }
        POLLING[cpu].store(false, Ordering::SeqCst);
    }
    arch::wait_for_interrupt_unless(&KICKED[cpu]);
}

/// Снимок очереди для `perf-diag` (P1): сколько задач готово и ждёт
/// процессора и сколько исполняется прямо сейчас (холостые не в счёт).
#[must_use]
pub fn queue_facts() -> (usize, usize) {
    let sched = sched_lock();
    let (mut ready, mut running) = (0, 0);
    for (slot, task) in sched.tasks.iter().enumerate() {
        if is_idle_slot(slot) {
            continue;
        }
        match task.as_ref().map(|task| task.state) {
            Some(TaskState::Ready) => ready += 1,
            Some(TaskState::Running) => running += 1,
            _ => {}
        }
    }
    (ready, running)
}

/// Тики простоя всех процессоров с запуска планировщика — для доли простоя
/// в `perf-diag`.
#[must_use]
pub fn idle_ticks_total() -> u64 {
    IDLE_TICKS.iter().map(|ticks| ticks.load(Ordering::Relaxed)).sum()
}

/// Отложенные пробуждения: сколько было и сколько не нашло ячейки (тогда
/// будятся все спящие — см. [`defer_wake`]).
#[must_use]
pub fn deferred_wake_stats() -> (u64, u64) {
    (DEFERRED_COUNT.load(Ordering::Relaxed), DEFERRED_OVERFLOWS.load(Ordering::Relaxed))
}

/// Заблокироваться сразу на нескольких ключах (`futex_waitv`, 12.11).
///
/// `ready` — как у [`block_on_futex`], под локом планировщика: проверка всех
/// слов и сон без щели между ними. `until` — срок в миллисекундах работы
/// системы. Возвращает номер ключа, по которому разбудили; `None` — не
/// уснули (`ready` ответил «да»), вышел срок или разбудил кто-то другой
/// (сигнал) — разбирается вызывающий.
pub fn block_on_futexes(
    keys: alloc::boxed::Box<[usize]>,
    until: Option<u64>,
    ready: impl FnOnce() -> bool,
) -> Option<usize> {
    // Every key in its own wait queue (G2); the keys are checked after all
    // of them are queued.
    let wait = Wait::FutexMany(until.unwrap_or(u64::MAX));
    let end = wq_block(waitq::Kind::Futex, &keys, wait, until, ready);
    drop(keys);
    match end {
        WqEnd::Woke(index) => index,
        WqEnd::NotSlept => None,
    }
}

/// Разбудить всех, кто ждёт ввода.
///
/// Вызывается драйвером, положившим событие в очередь, — как правило из
/// обработчика прерывания. `try_lock` отказывает только тогда, когда лок держит
/// сам этот процессор (см. [`SpinLock::try_lock`]): тогда ждать нельзя, и
/// пробуждение откладывается до срока, который в [`Wait::Input`] есть всегда
/// именно ради этого случая. Лок соседа `try_lock` дожидается.
pub fn wake_input() {
    let Some(mut sched) = SCHED.try_lock() else {
        return;
    };
    let mut woke = false;
    let high = sched.high;
    let mut found = [0u16; 32];
    let mut count = 0;
    INPUT_WAITERS.scan_from(0, high, |slot| {
        if !matches!(sched.tasks[slot].as_ref().map(|task| task.state), Some(TaskState::Blocked(Wait::Input(_)))) {
            INPUT_WAITERS.clear(slot);
            return false;
        }
        if count < found.len() {
            found[count] = slot as u16;
            count += 1;
        }
        false
    });
    for &slot in &found[..count] {
        let slot = usize::from(slot);
        INPUT_WAITERS.clear(slot);
        sched.set_ready(slot);
        if let Some(task) = sched.tasks[slot].as_mut() {
            set_boost(slot, task, true);
            woke = true;
        }
    }
    drop(sched);
    if woke {
        ask_reschedule();
    }
}

/// Попросить все процессоры переключиться на ближайшем тике: разбуженная
/// задача с [`Task::boost`] получит процессор сразу, а не по истечении
/// чужого кванта.
fn ask_reschedule() {
    if !PREEMPTION.load(Ordering::Relaxed) {
        return;
    }
    for flag in &NEED_RESCHED {
        flag.store(true, Ordering::Relaxed);
    }
}

/// Разбудить задачу, что бы она ни ждала.
///
/// # Почему пробуждение не ломает того, кого разбудили
///
/// Потому что всякое ожидание в этом планировщике — цикл с перепроверкой
/// условия, а не однократное «усни и проснись готовым». Лишнее пробуждение стоит
/// одного витка и ничего не меняет.
pub fn wake(id: TaskId) {
    // Замок занят — пробуждение не теряется (13.11): оно ложится в очередь,
    // которую разбирает ближайшее переключение. Раньше `wake` молча
    // возвращался, и задача в `futex` без срока спала до чужого пробуждения
    // (сигнал wineserver клиенту на SMP).
    // Slot by id without walking the table (G2); the wake sequence moves
    // first, so a task between "checked its condition" and "blocks" in a
    // wait queue does not block (see `Block::seq`).
    let Some(slot) = slot_of(id) else {
        return;
    };
    WAKE_SEQ[slot].fetch_add(1, Ordering::SeqCst);
    let Some(mut sched) = SCHED.try_lock() else {
        defer_wake(id);
        return;
    };
    let woke = sched.tasks[slot]
        .as_ref()
        .is_some_and(|task| task.id == id && matches!(task.state, TaskState::Blocked(_)));
    if woke {
        sched.set_ready(slot);
    }
    drop(sched);
    if woke {
        kick_woken(1);
    }
}

/// Отложенные пробуждения (см. [`wake`]): номера задач, ноль — пусто.
static DEFERRED_WAKES: [core::sync::atomic::AtomicU32; 64] = [const { core::sync::atomic::AtomicU32::new(0) }; 64];
static DEFERRED_WAKES_ANY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Сколько пробуждений отложено и сколько из них не нашло ячейки.
static DEFERRED_COUNT: AtomicU64 = AtomicU64::new(0);
static DEFERRED_OVERFLOWS: AtomicU64 = AtomicU64::new(0);
/// Ячеек не хватило — ближайшее переключение будит всех спящих (P9).
static DEFERRED_OVERFLOW: AtomicBool = AtomicBool::new(false);

fn defer_wake(id: TaskId) {
    let raw = id.as_u32();
    if raw == 0 {
        return;
    }
    DEFERRED_COUNT.fetch_add(1, Ordering::Relaxed);
    let mut placed = false;
    for cell in &DEFERRED_WAKES {
        let current = cell.load(Ordering::Relaxed);
        if current == raw {
            placed = true;
            break;
        }
        if current == 0 && cell.compare_exchange(0, raw, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
            placed = true;
            break;
        }
    }
    if !placed {
        // Все 64 ячейки заняты (P9, 18.11.2026): раньше пробуждение здесь
        // молча терялось, и поток в ожидании без срока спал до чужого
        // пробуждения. Кого именно будили, уже не записать — разбудить всех
        // спящих: всякое ожидание здесь — цикл с перепроверкой условия, и
        // лишнее пробуждение стоит одного витка.
        DEFERRED_OVERFLOWS.fetch_add(1, Ordering::Relaxed);
        DEFERRED_OVERFLOW.store(true, Ordering::Release);
    }
    DEFERRED_WAKES_ANY.store(true, Ordering::Release);
    ask_reschedule();
}

/// Добровольно уступить процессор.
///
/// Безопасно вызывать до [`run`] и вне любой задачи: [`schedule`] в этом случае
/// просто ничего не делает.
pub fn yield_now() {
    // Nobody else is ready, no deadline is due: nothing to give the CPU to
    // (G2: `sched_yield` of Wine spin-waits cost 52-172 us each, all of it
    // the scheduler lock).
    if RUNNING.load(Ordering::Relaxed) && !has_work() {
        return;
    }
    // Work is there, but another CPU idles (hw-log14: D2R yielded ~0.4 M
    // times a second with 70-88 % idle CPUs and 2-7 tasks ready, and every
    // yield took the scheduler lock - 2.5 s of lock waits per 30 s). The
    // ready task is that CPU's to take: kick it and go on, without the lock.
    // A busy machine (no idle CPU, or one CPU) still switches here, and the
    // timer still preempts a spinner at the end of its slice.
    if RUNNING.load(Ordering::Relaxed)
        && FAST_YIELD.load(Ordering::Relaxed)
        && KICKS.load(Ordering::Relaxed)
        && idle_cpu_elsewhere()
    {
        kick_idle(1);
        YIELD_FAST.fetch_add(1, Ordering::Relaxed);
        return;
    }
    schedule();
}

/// `sched_yield` without the lock when another CPU idles (`yieldfast off` -
/// always through the scheduler, as before).
pub static FAST_YIELD: AtomicBool = AtomicBool::new(true);
/// Yields that kicked an idle CPU instead of switching (perf-diag).
static YIELD_FAST: AtomicU64 = AtomicU64::new(0);

/// Yields answered without the scheduler lock since boot.
#[must_use]
pub fn fast_yields() -> u64 {
    YIELD_FAST.load(Ordering::Relaxed)
}

/// Is some other online CPU in its idle loop right now?
fn idle_cpu_elsewhere() -> bool {
    if smp::online() <= 1 {
        return false;
    }
    let me = arch::interrupts::without_interrupts(smp::cpu);
    (0..MAX_CPUS).any(|cpu| cpu != me && smp::is_online(cpu) && ON_IDLE[cpu].load(Ordering::Relaxed))
}

/// Единственная точка переключения контекста в ядре.
pub fn schedule() {
    schedule_with(Cause::Voluntary, None);
}

/// Переключиться, помня, по чьей воле это происходит.
fn schedule_with(cause: Cause, block: Option<Block>) {
    // Прерывания запрещаются здесь, а не полагаясь на `SpinLock`, и остаются
    // запрещёнными на всё переключение. Почему именно так — подробно в
    // заголовке модуля («Главная ловушка»).
    let irq_was_enabled = arch::interrupts::enabled();
    arch::interrupts::disable();
    // Ни одного спинлока к переключению (20.11.2026, см. `sync::check_switch`).
    crate::sync::check_switch();

    // An idle CPU with nothing to do does not touch the scheduler lock (G2):
    // every idle loop round (each tick, each kick) took it and walked the
    // whole table - on 8-16 CPUs the most frequent waiter of the lock. The
    // kick flag is cleared before the check: a task made ready after the
    // check sets its bit and then kicks again.
    if block.is_none() && RUNNING.load(Ordering::Relaxed) {
        let cpu = smp::cpu();
        if is_idle_slot(RUNNING_SLOT[cpu].load(Ordering::Relaxed)) {
            KICKED[cpu].store(false, Ordering::SeqCst);
            core::sync::atomic::fence(Ordering::SeqCst);
            if !has_work() {
                IDLE_SKIPS.fetch_add(1, Ordering::Relaxed);
                if irq_was_enabled {
                    arch::interrupts::enable();
                }
                return;
            }
        }
    }

    let mut from: *mut Context = ptr::null_mut();
    let mut to: *const Context = ptr::null();
    // База хранилища приходящей задачи снимается под локом вместе с
    // указателями и ставится ниже, до самого переключения: регистр один на
    // процессор, и задача обязана найти на нём своё значение, а не соседкино.
    let mut to_tls: u64 = 0;
    let mut to_gs: u64 = 0;
    let mut to_fs_selector: u16 = 0;
    let mut to_tls_entries: [u64; 3] = [0; 3];
    let mut expired = 0;
    let mut switch_user: Option<task::UserMachine> = None;
    let mut save_to = None;
    let mut load_from = None;

    loop {
        // `schedq percpu`: the decision without the global lock.
        if PERCPU.load(Ordering::Acquire) {
            if let Some(plan) = schedule_percpu(cause, block) {
                if let (Some(source), Some(target)) = (plan.from, plan.to) {
                    from = source;
                    to = target;
                }
                to_tls = plan.to_tls;
                to_gs = plan.to_gs;
                to_fs_selector = plan.to_fs_selector;
                to_tls_entries = plan.to_tls_entries;
                expired = plan.expired;
                switch_user = plan.switch_user;
                save_to = plan.save_to;
                load_from = plan.load_from;
                break;
            }
        }
        let mut sched = sched_lock();
        // The mode moved to `percpu` while we waited: the global path must
        // not take a task that a lock-free switch may take too.
        if PERCPU.load(Ordering::SeqCst) {
            drop(sched);
            continue;
        }
        // Номер процессора стабилен до конца блока: прерывания запрещены, и
        // перенести эту задачу на другой процессор нечем.
        let cpu = smp::cpu();
        KICKED[cpu].store(false, Ordering::Release);
        // The task asked to sleep (wait queues, G2): blocked here, under the
        // lock that also picks the next task, unless a wake already came -
        // a key took it out of its queue, or `wake(id)` moved its sequence.
        if let Some(block) = block {
            let current = RUNNING_SLOT[cpu].load(Ordering::Relaxed);
            let woken = waitq::woken(current).is_some() || WAKE_SEQ[current].load(Ordering::SeqCst) != block.seq;
            if !woken && !is_idle_slot(current) {
                if let Some(task) = sched.tasks[current].as_mut() {
                    if task.state == TaskState::Running {
                        set_state(current, task, TaskState::Blocked(block.wait));
                        if let Some(at) = block.until {
                            note_deadline(current, at);
                        }
                    }
                }
            }
        }
        // Пробуждения, отложенные `wake` при занятом замке (13.11).
        sched.take_deferred_wakes();
        if sched.running {
            sched.reap();
            // Сроки проверяются здесь, а не в обработчике таймера: обработчик
            // обязан быть коротким и не имеет права ждать лока.
            // Сроки сравниваются со **временем**, а не с числом тиков:
            // тики теряются, а время идёт по счётчику, который сам себя
            // считает (см. `block_on_irq_until`).
            expired = sched.wake_expired(crate::time::uptime_ms());

            let current = RUNNING_SLOT[cpu].load(Ordering::Relaxed);
            match sched.pick_next(cpu, cause) {
                None => {
                    // Остаёмся. Задача, которую разбудили до того, как она ушла,
                    // снова исполняется, а не числится готовой.
                    if !is_idle_slot(current) {
                        if let Some(task) = sched.tasks[current].as_mut() {
                            if task.state == TaskState::Ready {
                                set_state(current, task, TaskState::Running);
                            }
                        }
                    }
                }
                Some(next) => {
                    // Полосу-сторож проверяем у той задачи, которую покидаем, и
                    // именно сейчас: её стек только что перестал расти.
                    if let Some(task) = sched.tasks[current].as_ref() {
                        if let Some(stack) = task.stack.as_ref() {
                            if !stack.guard_intact() {
                                report_overflow(task.name, task.id);
                            }
                        }
                    }

                    // Указатели берутся по очереди, а не одновременно: два
                    // одновременных заимствования одного массива компилятор не
                    // пропустит. Адреса устойчивы — при перекладывании слота
                    // переезжает `Box`, а не сама задача.
                    if let Some(target) = sched.tasks[next].as_ref() {
                        to = target.context.as_ptr();
                    }
                    if let Some(source) = sched.tasks[current].as_mut() {
                        from = source.context.as_mut_ptr();
                    }

                    if !from.is_null() && !to.is_null() {
                        // The trap stack, the address space and the vector
                        // registers are switched after the lock is released
                        // (G2): both tasks are pinned to this CPU by their
                        // labels (`cpu` of the next one, `leaving` of this
                        // one) until `finish_switch`, so nobody else touches
                        // their areas, and the scheduler lock is no longer
                        // held across `CR3` writes and `xsave`/`xrstor`.
                        switch_user = Some(sched.tasks[next].as_ref().map_or_else(
                            task::UserMachine::default,
                            |task| task.user,
                        ));
                        save_to = sched.tasks[current]
                            .as_ref()
                            .and_then(|task| task.fpu.as_ref())
                            .map(task::FpuArea::as_ptr);
                        load_from = sched.tasks[next]
                            .as_ref()
                            .and_then(|task| task.fpu.as_ref())
                            .map(task::FpuArea::as_ptr);

                        // Часы спрашиваются один раз на переключение и служат
                        // обоим: концом отрезка уходящей задачи и началом
                        // отрезка приходящей. Двумя вызовами промежуток между
                        // ними не достался бы никому, и сумма по задачам
                        // тихо отставала бы от времени работы системы.
                        //
                        // Читать их отсюда можно: `uptime_ms` берёт атомики и
                        // счётчик процессора, не заходя ни в один лок, — иначе
                        // это был бы второй лок под локом планировщика при
                        // запрещённых прерываниях.
                        let now = crate::time::uptime_ms();
                        let now_ns = crate::time::uptime_ns();
                        if let Some(task) = sched.tasks[current].as_mut() {
                            if task.state == TaskState::Running {
                                set_state(current, task, TaskState::Ready);
                                if !is_idle_slot(current) {
                                    READY.set(current);
                                }
                            }
                            // Отрезок закрывается здесь — в той же точке, где
                            // задача перестаёт исполняться. Отметка в ноль
                            // означает, что начала у отрезка не было (см.
                            // `Task::ran_since_ms`), и приписывать такой
                            // задаче время неоткуда.
                            if task.ran_since_ms != 0 {
                                task.cpu_ms += now.saturating_sub(task.ran_since_ms);
                                task.ran_since_ms = 0;
                            }
                            cputime_switch_out(current, now_ns);
                            // Счётчик снятий висит на **покидаемой** задаче:
                            // вопрос, на который он отвечает, — уступает ли она
                            // сама. Метка процессора с неё не снимается: её
                            // снимет следующая задача, когда переключение
                            // закончится.
                            if cause == Cause::Preempted {
                                task.preempted += 1;
                            }
                            // Селектор `FS` уходящей — у потока Wine в
                            // 32-битном коде он не нулевой (12.11).
                            task.fs_selector = arch::user_fs_selector();
                            // Базы FS/GS — из процессора (20.11): с FSGSBASE
                            // программа меняет их сама, а `arch_prctl` больше не
                            // пишет в задачу под этим замком.
                            let (fs, gs) = arch::live_user_bases();
                            if let Some(fs) = fs {
                                task.tls = fs;
                            }
                            if let Some(gs) = gs {
                                task.gs = gs;
                            }
                            // Метка процессора снимается сразу, а «ещё
                            // уходит» — флагом, который снимет пришедшая без
                            // замка (D4; since 22.11 its `ON_CPU` flag, the
                            // same in both modes).
                            task.cpu = None;
                            LEAVING[cpu].store(current + 1, Ordering::Release);
                        }
                        if let Some(task) = sched.tasks[next].as_mut() {
                            set_state(next, task, TaskState::Running);
                            task.cpu = Some(cpu);
                            task.switches += 1;
                            set_boost(next, task, false);
                            task.ran_since_ms = now;
                            cputime_switch_in(next, now_ns, task.user.space_root.map_or(0, |root| root.as_u64()));
                            if task.woken_by_irq_ms != 0 {
                                let waited = now.saturating_sub(task.woken_by_irq_ms);
                                task.longest_irq_wait_ms = task.longest_irq_wait_ms.max(waited);
                                task.woken_by_irq_ms = 0;
                            }
                            to_tls = task.tls;
                            to_gs = task.gs;
                            to_fs_selector = task.fs_selector;
                            to_tls_entries = task.tls_entries;
                        }
                        ON_CPU[next].store(true, Ordering::SeqCst);
                        if !is_idle_slot(next) {
                            LAST_CPU[next].store(cpu as u16, Ordering::Relaxed);
                        }
                        prio::on_pick(next, cpu);
                        PREV_SLOT[cpu].store(current, Ordering::Relaxed);
                        let state = &mut sched.cpus[cpu];
                        state.previous = Some(current);
                        state.current = next;
                        RUNNING_SLOT[cpu].store(next, Ordering::Relaxed);
                        state.switches += 1;
                        if cause == Cause::Preempted {
                            state.forced += 1;
                        }
                        SLICE_LEFT[cpu].store(SLICE_TICKS, Ordering::Relaxed);
                        ON_IDLE[cpu].store(is_idle_slot(next), Ordering::Relaxed);
                    }
                }
            }
        }
        // Охранник уничтожается здесь — до `switch_context`. Прерывания при
        // этом не включаются: `lock()` застал их уже запрещёнными.
        drop(sched);
        break;
    }
    if let Some(next_user) = switch_user {
        // Стек ловушки — тот же адрес, что стек возврата из третьего кольца,
        // и лежит он в задаче. Ставится на **этом** процессоре: здесь задача
        // и продолжится.
        arch::set_trap_stack(next_user.return_stack);
        // Адресное пространство. У задачи без программы его нет, и тогда
        // процессор возвращается на таблицы ядра.
        // SAFETY: любой корень программы построен копией ядерного и содержит
        // все его отображения, поэтому переключение не уводит из-под ног ни
        // код, ни стек.
        unsafe {
            match next_user.space_root {
                Some(root) => arch::activate_space(root),
                None => arch::activate_kernel_space(),
            }
        }
        // Векторные регистры: сохранить своё и поставить чужое можно только
        // здесь, до смены стека, с запрещёнными прерываниями.
        if let Some(area) = save_to {
            // SAFETY: область принадлежит задаче, которая исполняется прямо
            // сейчас (её метка `leaving` не даёт её взять никому), выделена с
            // нужным размером и выравниванием и жива, пока жива задача.
            unsafe { arch::fpu::save(area) };
        }
        if let Some(area) = load_from {
            // SAFETY: область заполнена либо `init_area` при создании задачи,
            // либо предыдущим сохранением; задача помечена этим процессором.
            unsafe { arch::fpu::restore(area) };
        }
    }
    // Tasks whose deadline passed became ready here; idle CPUs take them now
    // instead of at their next tick (G2: ready tasks waited while CPUs idled).
    if expired > 1 {
        kick_woken(expired - 1);
    }

    if !from.is_null() && !to.is_null() {
        // SAFETY: оба указателя ведут в `MaybeUninit<Context>` внутри живых
        // `Box<Task>`. Ни одну из двух задач не освободит никто: уходящая несёт
        // метку этого процессора, и уборщик её не тронет, пока метку не снимет
        // `finish_switch`; приходящая помечена этим процессором только что. Та
        // же метка не даёт соседнему процессору взять ни одну из них. `to`
        // описывает задачу с отображённым стеком — построенную `Context::new`
        // либо сохранённую предыдущим вызовом этой функции.
        arch::enter_user_segments(to_tls, to_fs_selector, &to_tls_entries);
        arch::set_user_gs(to_gs);
        unsafe { arch::switch_context(from, to) };
        // Сюда управление возвращается уже в контексте задачи, которая когда-то
        // уступила процессор, — возможно, на другом процессоре. Первое, что она
        // делает, — отпускает ту, что уступила процессор ей.
        finish_switch();
    }

    // `irq_was_enabled` прочитан с собственного стека задачи.
    if irq_was_enabled {
        arch::interrupts::enable();
    }
}

/// Снять метку процессора с задачи, только что уступившей его. См. заголовок
/// модуля.
fn finish_switch() {
    let leaving = LEAVING[smp::cpu()].swap(0, Ordering::AcqRel);
    if let Some(slot) = leaving.checked_sub(1) {
        // `Release` (SeqCst) — всё, что `switch_context` сохранил в её
        // контекст, видно тому, кто возьмёт её следующим (он берёт флаг
        // сравнением с обменом или читает его с `Acquire` под замком).
        ON_CPU[slot].store(false, Ordering::SeqCst);
    }
}

/// Слот задачи, уходящей с процессора (плюс один), — по процессору; ноль —
/// нет такой. Её `ON_CPU` снимает [`finish_switch`].
static LEAVING: [AtomicUsize; MAX_CPUS] = [const { AtomicUsize::new(0) }; MAX_CPUS];

/// Задачу можно взять или убрать: не исполняется и не уходит с процессора.
fn off_cpu(slot: usize) -> bool {
    !ON_CPU[slot].load(Ordering::Acquire)
}

/// Батут, с которого начинает любая задача, созданная [`spawn_raw`].
///
/// Существует ради одного действия: снять метку с задачи, уступившей процессор.
/// Обычное переключение делает это после возврата из `switch_context`, а у новой
/// задачи возврата нет — её стек начинается здесь. Забудь она, и уступившая
/// задача осталась бы помеченной навсегда: её не взял бы ни один процессор, а
/// завершившуюся — не убрал бы уборщик.
extern "C" fn task_start(slot: usize) -> ! {
    finish_switch();
    let start = {
        let sched = sched_lock();
        sched.tasks[slot]
            .as_ref()
            .and_then(|task| task.entry.map(|entry| (entry, task.arg)))
    };
    match start {
        // Прерывания здесь по-прежнему запрещены — их запретил `schedule`, — и
        // разрешает их сама точка входа, как было всегда.
        Some((entry, arg)) => entry(arg),
        None => {
            kprintln!("sched: FATAL: task in slot {slot} started without an entry point");
            smp::stop_others();
            arch::halt();
        }
    }
}

/// Просьба об аварийном дампе (клавиша Pause, 11.11).
static DUMP_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Попросить аварийный дамп: его напечатает ближайший тик загрузочного
/// процессора. Зовётся из обработчика клавиатуры.
pub fn request_dump() {
    arch::hypervisor_log(b"  dump        : Pause seen\n");
    DUMP_REQUESTED.store(true, Ordering::Release);
}

/// Сколько задач было с повисшей меткой процессора при прошлой проверке.
static STALE_SEEN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Из тика загрузочного процессора (11.11): аварийный дамп по просьбе и раз в
/// секунду — проверка повисших меток процессора.
///
/// Повисшая метка: задача `Ready`, `cpu = Some(c)`, а процессор `c` её не
/// исполняет и не только что от неё ушёл. Такую задачу `pick_next` не возьмёт
/// никогда, и снаружи это выглядит как «процессоры простаивают, а программа
/// стоит» (llama в VirtualBox, 11.11). Лок — `try_lock`: тик не ждёт.
pub fn diag_tick(ticks: u64) {
    /// Строка дампа — и в журнал ядра, и в журнал гипервизора: в VirtualBox
    /// экран бывает занят зависшим столом, а линия — встать посреди строки.
    struct HyperLog;
    impl core::fmt::Write for HyperLog {
        fn write_str(&mut self, text: &str) -> core::fmt::Result {
            arch::hypervisor_log(text.as_bytes());
            Ok(())
        }
    }
    macro_rules! dumpln {
        ($($arg:tt)*) => {{
            let _ = core::fmt::Write::write_fmt(&mut HyperLog, format_args!($($arg)*));
            arch::hypervisor_log(b"\n");
            crate::kprintln!($($arg)*);
        }};
    }

    let dump = DUMP_REQUESTED.swap(false, Ordering::AcqRel);
    if !dump && ticks % 1000 != 0 {
        return;
    }
    // Пульс тика загрузочного процессора — раз в 10 с, только в журнал
    // гипервизора: по нему видно, доходит ли тик до этой точки при зависании.
    if ticks % 10_000 == 0 {
        let mut line = HyperLog;
        let _ = core::fmt::Write::write_fmt(&mut line, format_args!("  pulse       : tick {ticks}\n"));
    }
    if dump {
        // До лока планировщика: это видно, даже если он занят навсегда.
        dumpln!("  dump        : tick {ticks}, uptime {} ms", crate::time::uptime_ms());
        for c in 0..smp::online() {
            dumpln!(
                "  dump        : cpu {c} idle {} idle ticks {} need resched {} slice left {}",
                ON_IDLE[c].load(Ordering::Relaxed),
                IDLE_TICKS[c].load(Ordering::Relaxed),
                NEED_RESCHED[c].load(Ordering::Relaxed),
                SLICE_LEFT[c].load(Ordering::Relaxed)
            );
        }
    }
    let Some(sched) = SCHED.try_lock() else {
        if dump {
            dumpln!("  dump        : scheduler lock is held by another CPU");
        }
        return;
    };
    let mut stale = 0usize;
    let cpu_current = |c: usize| RUNNING_SLOT[c].load(Ordering::Relaxed);
    let cpu_previous = |c: usize| Some(PREV_SLOT[c].load(Ordering::Relaxed)).filter(|&slot| slot != usize::MAX);
    // `percpu` switches write the labels without this lock: no stale check.
    let check = !PERCPU.load(Ordering::Relaxed);
    for (slot, entry) in sched.slots().iter().enumerate() {
        let Some(task) = entry else { continue };
        if let Some(c) = task.cpu.filter(|_| check) {
            let on = cpu_current(c) == slot || cpu_previous(c) == Some(slot);
            if !on && task.state != TaskState::Running {
                stale += 1;
                if dump || STALE_SEEN.load(Ordering::Relaxed) == 0 {
                    crate::kprintln!(
                        "  sched       : STALE cpu label: #{} {} {:?} marked cpu {c}, cpu {c} runs slot {} (prev {:?})",
                        task.id.as_u32(),
                        task.name,
                        task.state,
                        cpu_current(c),
                        cpu_previous(c)
                    );
                }
            }
        }
    }
    STALE_SEEN.store(stale, Ordering::Relaxed);
    if dump {
        // Экран не забирается: забор идёт через локи консоли, и зависни он —
        // не было бы и журнала гипервизора, единственного надёжного здесь.
        dumpln!("  dump        : uptime {} ms, {} stale label(s)", crate::time::uptime_ms(), stale);
        dumpln!("  dump        : scheduler queues {}", if PERCPU.load(Ordering::Relaxed) { "per cpu" } else { "global" });
        for c in 0..smp::online().min(MAX_CPUS) {
            dumpln!("  dump        : cpu {c} runs slot {} (prev {:?})", cpu_current(c), cpu_previous(c));
        }
        for (slot, entry) in sched.slots().iter().enumerate() {
            let Some(task) = entry else { continue };
            if task.state == TaskState::Finished {
                continue;
            }
            dumpln!(
                "  dump        : slot {slot} #{} {} {:?} cpu {:?} boost {}",
                task.id.as_u32(),
                task.name,
                task.state,
                task.cpu,
                task.boost
            );
        }
    }
}

/// Учёт кванта. Предназначено для вызова из `irq::on_timer_tick`.
///
/// Ничего не переключает — только считает и, когда квант истёк, поднимает флаг
/// для [`preempt_point`]. Лока не берёт вовсе: всё, что трогает, принадлежит
/// своему процессору (см. [`SLICE_LEFT`]).
pub fn on_timer_tick() {
    // Бегущая полоса экрана загрузки — и до планировщика: тик уже идёт.
    crate::console::splash_tick();
    if !RUNNING.load(Ordering::Relaxed) {
        return;
    }
    let cpu = smp::cpu();
    // Тик засчитывается той задаче, которую он застал. Холостая считается
    // отдельно: доля её тиков — единственная мера простоя, и мерить её снаружи
    // нечем.
    if ON_IDLE[cpu].load(Ordering::Relaxed) {
        IDLE_TICKS[cpu].fetch_add(1, Ordering::Relaxed);
    }
    watch_tick();
    let left = SLICE_LEFT[cpu].load(Ordering::Relaxed).saturating_sub(1);
    if left == 0 {
        SLICE_LEFT[cpu].store(SLICE_TICKS, Ordering::Relaxed);
        NEED_RESCHED[cpu].store(true, Ordering::Relaxed);
    } else {
        SLICE_LEFT[cpu].store(left, Ordering::Relaxed);
    }
}

/// Отладка телефона (05.11): раз в секунду из тика — какая задача на
/// процессоре, в полосу поверх экрана (`oem overlay on`). Зависший стол
/// оставляет её на последнем кадре: видно, кто держит процессор.
fn watch_tick() {
    static LAST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    if !crate::console::OVERLAY.load(Ordering::Relaxed) {
        return;
    }
    let now = crate::time::uptime_ms();
    if now.saturating_sub(LAST.load(Ordering::Relaxed)) < 1000 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    let mut value = now / 1000;
    loop {
        at -= 1;
        digits[at] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let seconds = core::str::from_utf8(&digits[at..]).unwrap_or("?");
    static SEEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let commits = crate::ui::COMMITS.load(Ordering::Relaxed);
    let mut per = commits.saturating_sub(SEEN.swap(commits, Ordering::Relaxed));
    let mut cdigits = [0u8; 20];
    let mut cat = cdigits.len();
    loop {
        cat -= 1;
        cdigits[cat] = b'0' + (per % 10) as u8;
        per /= 10;
        if per == 0 {
            break;
        }
    }
    let rate = core::str::from_utf8(&cdigits[cat..]).unwrap_or("?");
    // Где стоял процессор, когда пришёл тик: адрес возврата прерывания и
    // уровень (EL0 — программа, EL1 — ядро). По адресу ядра функция находится
    // `llvm-addr2line` по ELF ядра телефона.
    #[cfg(target_arch = "aarch64")]
    let (elr, spsr): (u64, u64) = {
        let elr: u64;
        let spsr: u64;
        // SAFETY: чтение системных регистров в обработчике прерывания.
        unsafe {
            core::arch::asm!("mrs {}, elr_el1", out(reg) elr, options(nomem, nostack));
            core::arch::asm!("mrs {}, spsr_el1", out(reg) spsr, options(nomem, nostack));
        }
        (elr, spsr)
    };
    #[cfg(not(target_arch = "aarch64"))]
    let (elr, spsr): (u64, u64) = (0, 0);
    let mut hex = [0u8; 16];
    for (index, slot) in hex.iter_mut().enumerate() {
        let nibble = (elr >> ((15 - index) * 4)) & 0xf;
        *slot = if nibble < 10 { b'0' + nibble as u8 } else { b'a' + (nibble - 10) as u8 };
    }
    let pc = core::str::from_utf8(&hex).unwrap_or("?");
    let level = if spsr & 0xc == 0 { " EL0 " } else { " EL1 " };
    let cpu = smp::cpu();
    let idle = ON_IDLE[cpu].load(Ordering::Relaxed);
    match SCHED.try_lock() {
        Some(sched) => {
            let name = sched.tasks[sched.current()].as_ref().map_or("none", |task| task.name);
            crate::console::overlay_note(&["t", seconds, " ", name, if idle { " idle" } else { "" }, " c", rate, level, pc]);
        }
        None => crate::console::overlay_note(&["t", seconds, " SCHED LOCKED c", rate, level, pc]),
    }
}

/// Место, где вытеснение действительно происходит.
///
/// Арх-слой обязан вызывать эту точку в конце обработки внешнего прерывания и
/// **после** того, как подтвердил его контроллеру. Порядок здесь — условие
/// работоспособности: подтверждение, унесённое в другую задачу, оставляет
/// контроллер с активным прерыванием, а систему — без таймера.
///
/// Прерывания при вызове запрещены — это состояние входа в обработчик, — и
/// [`schedule`] на них и рассчитывает.
pub fn preempt_point() {
    if !PREEMPTION.load(Ordering::Relaxed) {
        return;
    }
    // Просьбы «не отбирать процессор» здесь больше нет — она снята 25.09.2026,
    // и почему, сказано у `shell::print`. Не возвращать: одна задача, рисующая
    // строку в окно терминала, останавливала ею всю машину на секунду.
    // `swap`, а не пара «прочитать — сбросить»: флаг поднимает обработчик
    // таймера, и между чтением и сбросом он успел бы прийти ещё раз.
    if !NEED_RESCHED[smp::cpu()].swap(false, Ordering::Relaxed) {
        return;
    }
    // Nobody waits for a CPU: the slice is renewed without the lock (G2).
    if !has_work() {
        SLICE_LEFT[smp::cpu()].store(SLICE_TICKS, Ordering::Relaxed);
        return;
    }
    schedule_with(Cause::Preempted, None);
}

/// Включено ли вытеснение — то есть идёт ли планирование по-настоящему.
#[must_use]
pub fn preemption_on() -> bool {
    PREEMPTION.load(Ordering::Relaxed)
}

/// Включить или выключить вытеснение по таймеру.
///
/// Включается один раз, перед [`run`]. С фазы 27 у флага есть и второй,
/// постоянный вызывающий: [`crate::power::shut_down`] снимает вытеснение между
/// сбросом тома на диск и снятием питания.
pub fn set_preemption(enabled: bool) {
    PREEMPTION.store(enabled, Ordering::Relaxed);
    if !enabled {
        // Иначе поднятый флаг дождался бы обратного включения и сработал бы
        // в совершенно постороннем месте.
        for flag in &NEED_RESCHED {
            flag.store(false, Ordering::Relaxed);
        }
    }
}

/// Завести холостую задачу и стек для процессора `index` — до того, как он
/// проснётся.
///
/// Возвращает вершину стека, на котором процессор начнёт исполняться. Этот стек
/// и есть стек его холостой задачи: процессор становится ею сразу, без
/// переключения, так же как загрузочный становится холостой задачей в [`run`].
///
/// `None` — нет кучи или номер вне таблицы.
pub fn prepare_cpu(index: usize) -> Option<usize> {
    if index == 0 || index >= MAX_CPUS {
        return None;
    }
    let stack = Stack::allocate(TASK_STACK_SIZE)?;
    let top = stack.top().as_usize();
    let mut idle = Task::idle(index);
    idle.stack = Some(stack);
    let boxed = box_task(idle)?;

    let mut sched = sched_lock();
    sched.tasks[index] = Some(boxed);
    publish(&mut sched, index);
    sched.cpus[index].current = index;
    RUNNING_SLOT[index].store(index, Ordering::Relaxed);
    Some(top)
}

/// Забыть процессор, который так и не проснулся: отдать стек его холостой задачи.
///
/// Вызывать только тогда, когда процессор заведомо не исполняется на этом стеке
/// — то есть когда прошивка отвергла саму просьбу его запустить.
pub fn forget_cpu(index: usize) {
    if index == 0 || index >= MAX_CPUS {
        return;
    }
    // Задача уничтожается после охранника: `Drop` стека трогает кучу, и делать
    // это под локом планировщика незачем.
    let task = {
        let mut sched = sched_lock();
        let task = sched.tasks[index].take();
        publish(&mut sched, index);
        task
    };
    drop(task);
}

/// Запустить планирование. Возврата нет: когда все задачи завершатся, ядро
/// печатает сводку и останавливается.
pub fn run() -> ! {
    // Холостая задача создаётся до захвата лока по той же причине, что и стек в
    // `spawn`: диагностика отказа кучи не должна печататься под локом.
    let Some(idle) = box_task(Task::idle(0)) else {
        kprintln!("sched: FATAL: no heap for the idle task");
        arch::halt();
    };

    {
        let mut sched = sched_lock();
        if sched.running {
            kprintln!("sched: FATAL: run() called twice");
            arch::halt();
        }
        sched.tasks[0] = Some(idle);
        publish(&mut sched, 0);
        sched.cpus[0].current = 0;
        sched.running = true;
    }
    RUNNING.store(true, Ordering::Release);
    STARTED_MS.store(crate::time::uptime_ms(), Ordering::Relaxed);
    STARTED_TICKS.store(crate::irq::ticks(), Ordering::Relaxed);

    // Холостая задача загрузочного процессора живёт здесь. Процессор
    // останавливается до ближайшего прерывания, когда готовых задач нет, — ровно
    // так же, как холостой цикл остальных процессоров в `smp::secondary_main`.
    // Отличие одно: только этот цикл решает, что работа кончилась.
    loop {
        // A counter, not a walk under the lock on every idle round (G2).
        if ALIVE.load(Ordering::Acquire) == 0 {
            break;
        }
        schedule();

        // Ожидание пинка, затем `hlt` (см. `idle_wait`).
        idle_wait();
    }

    // Остальные процессоры останавливаются до сводки: иначе служебные задачи на
    // них продолжали бы исполняться после строки «CPU halted», и сводка
    // описывала бы машину, которая всё ещё работает.
    let stopped = smp::stop_others();

    let (freed, switches) = {
        let mut sched = sched_lock();
        (sched.reap(), sched.switches())
    };

    kprintln!("  done       : all tasks finished, {switches} switches, {freed} stack(s) reclaimed at exit");
    if stopped > 0 {
        kprintln!("  smp        : {stopped} other processor(s) asked to stop");
    }
    dump();

    kprintln!();
    kprintln!("All tasks finished, nothing left to schedule. CPU halted.");
    arch::halt();
}

/// Идентификатор исполняющейся задачи.
#[must_use]
pub fn current() -> TaskId {
    // Without the lock (G2): the slot's id is written before the task first
    // runs and does not change while it lives.
    let slot = current_slot();
    if is_idle_slot(slot) {
        return TaskId::IDLE;
    }
    TaskId::new(SLOT_ID[slot].load(Ordering::Relaxed))
}

/// Имя исполняющейся задачи.
#[must_use]
pub fn current_name() -> &'static str {
    let sched = sched_lock();
    sched.tasks[sched.current()].as_ref().map_or("none", |task| task.name)
}

/// Сколько всего было переключений контекста.
#[must_use]
pub fn switch_count() -> u64 {
    sched_lock().switches()
}

/// Сколько миллисекунд задача занимала процессор — включая незакрытый отрезок.
///
/// `None`, если задачи с таким номером нет.
///
/// Незакрытый отрезок добирается **здесь**, а не копится на каждом тике, и это
/// разница в цене: спрашивающих единицы, а тиков сотня в секунду на каждом
/// процессоре. Без добора задача, спросившая о себе, всегда получала бы время
/// своего предыдущего захода — то есть ноль, пока она не переключится хоть раз.
#[must_use]
pub fn cpu_ms(id: TaskId) -> Option<u64> {
    let sched = sched_lock();
    let task = sched.slots().iter().flatten().find(|task| task.id == id)?;
    let running = if task.ran_since_ms == 0 {
        0
    } else {
        crate::time::uptime_ms().saturating_sub(task.ran_since_ms)
    };
    Some(task.cpu_ms + running)
}

/// Запущено ли планирование.
#[must_use]
pub fn is_running() -> bool {
    sched_lock().running
}

/// Сколько задач ещё не завершились, не считая холостых.
#[must_use]
pub fn alive() -> usize {
    ALIVE.load(Ordering::Acquire)
}

/// Idle rounds that found nothing to do and skipped the lock (`perf-diag`).
static IDLE_SKIPS: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn idle_skips() -> u64 {
    IDLE_SKIPS.load(Ordering::Relaxed)
}

/// Компактный список задач с состояниями.
/// То, что о задаче знает диспетчер задач.
///
/// Снимок, а не ссылка: таблица живёт под замком планировщика, и отдавать
/// наружу что-то, кроме копии, значило бы держать этот замок, пока программа
/// рисует окно.
pub struct TaskFacts {
    /// Слот в таблице — по нему [`crate::user`] находит программу задачи.
    pub slot: usize,
    pub id: TaskId,
    pub name: &'static str,
    pub state: TaskState,
    /// Вместе с незакрытым отрезком исполняющейся задачи, как в [`dump`].
    pub cpu_ms: u64,
    pub daemon: bool,
    /// Сколько раз задачу ставили на процессор (фаза 69c, диспетчер задач).
    pub switches: u64,
    /// См. [`task::Task::disk_bytes`] и [`task::Task::net_bytes`].
    pub disk_bytes: u64,
    pub net_bytes: u64,
}

/// Снимок всех задач — для `SYS_TASKS`.
#[must_use]
pub fn snapshot() -> alloc::vec::Vec<TaskFacts> {
    let sched = sched_lock();
    let now = crate::time::uptime_ms();
    let mut out = alloc::vec::Vec::new();
    for (slot, entry) in sched.slots().iter().enumerate() {
        let Some(task) = entry else {
            continue;
        };
        let running = if task.ran_since_ms == 0 { 0 } else { now.saturating_sub(task.ran_since_ms) };
        if out.try_reserve(1).is_err() {
            break;
        }
        out.push(TaskFacts {
            slot,
            id: task.id,
            name: task.name,
            state: task.state,
            cpu_ms: task.cpu_ms + running,
            daemon: task.daemon,
            switches: task.switches,
            disk_bytes: task.disk_bytes,
            net_bytes: task.net_bytes,
        });
    }
    out
}

/// Строка таблицы задач, снятая под локом планировщика.
///
/// Заведена ради одного: печать [`dump`] спрашивает имя программы у таблицы
/// программ, а та под своим замком, и держать при этом замок планировщика
/// нельзя. Снимок разрывает эту связь.
struct Row {
    slot: usize,
    id: TaskId,
    name: &'static str,
    state: TaskState,
    switches: u64,
    preempted: u64,
    cpu_ms: u64,
    ran_since_ms: u64,
    longest_irq_wait_ms: u64,
    disk_bytes: u64,
    net_bytes: u64,
    stack: &'static str,
}

pub fn dump() {
    // Сначала — снимок под локом планировщика, и только потом печать.
    //
    // Раньше строки и собирались, и печатались под ним: `kprintln!` к
    // планировщику не обращается, и рекурсии на неперевходимом локе не
    // выходило. Теперь у каждой строки спрашивается ещё и имя программы, а
    // таблица программ живёт под **своим** замком — брать его под замком
    // планировщика нельзя (то же правило соблюдает `SYS_TASKS`, см.
    // `user::syscall::tasks`). Значит лок отпускается раньше печати.
    let rows = {
        let sched = sched_lock();
        let mut rows: alloc::vec::Vec<Row> = alloc::vec::Vec::new();
        for (slot, entry) in sched.slots().iter().enumerate() {
            let Some(entry) = entry else { continue };
            if rows.try_reserve(1).is_err() {
                break;
            }
            let stack = match entry.stack.as_ref() {
                Some(stack) if stack.guard_intact() => "held",
                Some(_) => "OVERFLOWN",
                None => "freed",
            };
            rows.push(Row {
                slot,
                id: entry.id,
                name: entry.name,
                state: entry.state,
                switches: entry.switches,
                preempted: entry.preempted,
                cpu_ms: entry.cpu_ms,
                ran_since_ms: entry.ran_since_ms,
                longest_irq_wait_ms: entry.longest_irq_wait_ms,
                disk_bytes: entry.disk_bytes,
                net_bytes: entry.net_bytes,
                stack,
            });
        }
        rows
    };

    for entry in &rows {
        let stack = entry.stack;
        // Время на процессоре — у исполняющейся задачи вместе с незакрытым
        // отрезком: иначе та, что считает прямо сейчас, показывала бы время
        // своего предыдущего захода и выглядела бы бездельницей.
        let cpu_ms = entry.cpu_ms
            + if entry.ran_since_ms == 0 {
                0
            } else {
                crate::time::uptime_ms().saturating_sub(entry.ran_since_ms)
            };
        // Имя программы вместо слова «program», если слот её исполняет.
        // «program» отвечало на вопрос, которого никто не задаёт: в таблице
        // из десяти строк восемь назывались одинаково, и понять, какая из них
        // встала, можно было только сверяясь со строкой запуска выше по
        // журналу. Ядро знает путь — тот же, что показывает «Диспетчер задач».
        let path = crate::user::program_facts(entry.slot).map(|(path, _)| path);
        let name = match path.as_deref() {
            Some(path) => path.rsplit('/').next().unwrap_or(entry.name),
            None => entry.name,
        };
        kprintln!(
            "  {} {:<8} {:<9} {:>3} switches ({} forced), {} ms on cpu, stack {}",
            entry.id,
            name,
            entry.state,
            entry.switches,
            entry.preempted,
            cpu_ms,
            stack
        );
        // Обмен с носителем и сетью — тоже своей строкой и только у тех, у
        // кого он был: те же числа, что в столбцах диспетчера задач.
        if entry.disk_bytes != 0 || entry.net_bytes != 0 {
            kprintln!("       disk {} KiB, network {} KiB", entry.disk_bytes / 1024, entry.net_bytes / 1024);
        }
        // Чего ждёт — отдельной строкой под своей задачей, а не колонкой:
        // ширина у поводов разная, и втиснутые в таблицу они бы её разъехали.
        // Печатается только у заблокированных — у остальных ответа нет.
        if let Some((why, value)) = entry.state.waiting_for() {
            if value == 0 {
                kprintln!("       {why}");
            } else {
                kprintln!("       {why} {value}");
            }
            // В каком вызове Linux стоит (фаза 74): по ключу futex не понять,
            // чего ждёт поток программы, а по номеру и аргументам — видно.
            #[cfg(target_arch = "x86_64")]
            if let Some([number, a0, a1, a2, a3]) = crate::linux::recent::in_call(entry.slot) {
                kprintln!("       in linux call {number}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x})");
                crate::linux::recent::dump_slot(entry.slot, 8);
                if number == 232 || number == 281 {
                    crate::linux::ipc::epoll_report(entry.slot, a0 as usize);
                }
            }
        }
        // Только у тех, кого будило прерывание: у прочих ответа нет, а ноль
        // выглядел бы как «ждала мгновенно».
        if entry.longest_irq_wait_ms != 0 {
            kprintln!(
                "       waited at most {} ms for a cpu after an interrupt woke it",
                entry.longest_irq_wait_ms
            );
        }
    }
    // Итоги берутся под локом заново, и печатать под ним тут можно: в отличие
    // от строк задач, ничего из таблицы программ здесь не спрашивается.
    let sched = sched_lock();
    kprintln!(
        "  preemption : {}, {} ms slice, {} forced switch(es)",
        if PREEMPTION.load(Ordering::Relaxed) { "on" } else { "off" },
        SLICE_MS,
        sched.forced()
    );
    // Два главных подозреваемых в опоздании готовой задачи — и место каждого.
    crate::latency::report();

    // ── Доля простоя ─────────────────────────────────────────────────────────
    //
    // Меряется **временем**, а не тиками, и это не придирка к единицам.
    // Прежний делитель — «тиков с включения таймера» — врал дважды. Во-первых,
    // он включал всю загрузку, когда холостой задачи ещё не было; отладочная
    // загрузка длиннее, чем всё остальное измерение, и доля простоя выходила
    // втрое меньше настоящей. Во-вторых, тик — величина, которую гость не
    // контролирует: на занятом хосте эмулятор их теряет пачками, и 18.09 в
    // журнале стояло «76078 ms, 3665 timer ticks» при таймере в 1000 Гц. Доля,
    // посчитанная в такой валюте, описывает потери тиков, а не машину.
    //
    // Время холостой задачи ядро и так считает — тем же способом, что у
    // всякой другой (`Task::cpu_ms`), и по монотонному счётчику, а не по
    // тикам.
    let online: alloc::vec::Vec<usize> =
        (0..MAX_CPUS).filter(|&cpu| sched.tasks[cpu].is_some()).collect();
    let now = crate::time::uptime_ms();
    let started = STARTED_MS.load(Ordering::Relaxed);
    let span_ms = now.saturating_sub(started) * online.len().max(1) as u64;
    let idle_ms: u64 = online
        .iter()
        .filter_map(|&cpu| sched.tasks[cpu].as_ref())
        .map(|idle| {
            idle.cpu_ms
                + if idle.ran_since_ms == 0 {
                    0
                } else {
                    now.saturating_sub(idle.ran_since_ms)
                }
        })
        .sum();
    let percent = if span_ms == 0 { 0 } else { idle_ms * 100 / span_ms };
    // Окно печатается с десятой долей: планировщик запускается в конце
    // загрузки, и на первых секундах работы целые секунды показывали бы ноль —
    // то есть «доля от нуля», строку, по которой ничего не проверишь.
    kprintln!(
        "  idle       : {percent}% of {}.{} s since scheduling started",
        span_ms / 1000,
        (span_ms % 1000) / 100
    );

    // Потерянные тики — отдельной строкой и только когда они потеряны.
    //
    // Строка говорит **что**, а не **почему**, и это не осторожность ради
    // осторожности: причин у потерянных тиков две, и различить их отсюда
    // нечем. Снаружи — занятый хост, не дающий гостю процессора (18.09 в
    // журнале стояло «76078 ms, 3665 timer ticks» при таймере в 1000 Гц).
    // Изнутри — собственные участки с запрещёнными прерываниями; у нас такой
    // уже ловили, и это печать в серийную линию. Назвать одну из двух причин
    // значило бы отправить следующего искать не там.
    //
    // Зачем вообще: всякое измерение скорости, сделанное в минуту, когда
    // четверть тиков не пришла, — измерение чужой загрузки, а не нашей
    // работы. Окно не меньше пяти секунд: на первой секунде после запуска
    // планировщика система печатает загрузочный журнал, и тики там теряются
    // по своей же причине, о которой и так известно.
    // Оба числа — за одно окно, с начала планирования: тики, накопившиеся за
    // загрузку, здесь ни при чём, а сравнение «всего тиков» с «положено за
    // последнюю секунду» дало бы бессмыслицу.
    let ticks = crate::irq::ticks().saturating_sub(STARTED_TICKS.load(Ordering::Relaxed));
    let due = now.saturating_sub(started) * u64::from(crate::irq::TIMER_HZ) / 1000;
    if due > 5000 && ticks * 4 < due * 3 {
        kprintln!(
            "  timer      : {ticks} tick(s) arrived where {due} were due -- a quarter of them              are missing, and speed measured now measures something else"
        );
    }

    // Строка на процессор — проверка, которой до фазы 43 не существовало. Общий
    // счётчик переключений не отличит «четыре процессора работают» от «один
    // работает за четверых»; счётчик на каждом процессоре — отличит, а «кто
    // исполняется прямо сейчас» показывает одновременность напрямую: две
    // задачи не могут исполняться в один миг на одном процессоре.
    for &cpu in &online {
        let state = &sched.cpus[cpu];
        let current = RUNNING_SLOT[cpu].load(Ordering::Relaxed);
        let what = if is_idle_slot(current) {
            alloc::string::String::from("idle")
        } else {
            match sched.tasks[current].as_ref() {
                Some(task) => alloc::format!("running {} ({})", task.id, task.name),
                None => alloc::string::String::from("unknown"),
            }
        };
        // Тот же знаменатель, что у общей доли: тики **с запуска
        // планировщика**, а не с включения таймера. Считать от включения
        // значило бы делить на время, когда холостой задачи не существовало.
        let idle = IDLE_TICKS[cpu].load(Ordering::Relaxed);
        let idle_percent = if ticks == 0 { 0 } else { idle * 100 / ticks };
        kprintln!(
            "  cpu {cpu}      : {what}, {} switches ({} forced), idle {idle_percent}% of its ticks",
            state.switches + PC_SWITCHES[cpu].load(Ordering::Relaxed),
            state.forced + PC_FORCED[cpu].load(Ordering::Relaxed)
        );
    }
}

/// Батут, с которого начинается любая задача, созданная [`spawn`].
///
/// Нужен из-за несовпадения двух вещей. Во-первых, `Context::new` умеет
/// передать управление только в `extern "C" fn(usize) -> !`, а пользовательская
/// точка входа — обычная `fn()`. Во-вторых и главное, `fn()` **вправе
/// вернуться**, а возвращаться ей некуда.
extern "C" fn trampoline(arg: usize) -> ! {
    // SAFETY: `arg` — это `fn()`, приведённая к `usize` в `spawn` и нигде больше
    // не порождаемая. Приведение указателя на функцию к `usize` и обратно
    // сохраняет значение, а размеры совпадают.
    let entry: fn() = unsafe { core::mem::transmute::<usize, fn()>(arg) };

    // Задача начинает исполняться с запрещёнными прерываниями: их запретил
    // `schedule()` перед переключением. Инвариант планировщика: задачи
    // исполняются с разрешёнными прерываниями, иначе таймер до них не дойдёт.
    arch::interrupts::enable();

    entry();

    exit_current_with(0)
}

/// Завершить текущую задачу с кодом возврата.
///
/// Освободить здесь стек нельзя — на нём в этот момент исполняется вот этот
/// самый код. Поэтому задача лишь помечается завершённой и уступает процессор;
/// стек заберёт уборщик, когда с неё снимут метку процессора.
pub fn exit_current_with(result: i64) -> ! {
    {
        let mut sched = sched_lock();
        let current = sched.current();
        let mut id = TaskId::IDLE;
        if let Some(task) = sched.tasks[current].as_mut() {
            if !task.daemon && task.state != TaskState::Finished && !is_idle_slot(current) {
                ALIVE.fetch_sub(1, Ordering::AcqRel);
            }
            REAP_PENDING.store(true, Ordering::Release);
            set_state(current, task, TaskState::Finished);
            task.result = result;
            id = task.id;
            // Программы у завершившейся задачи больше нет, и её адресное
            // пространство разобрано вызывающим. Отметку надо снять здесь же:
            // иначе ближайшее переключение попыталось бы поставить процессору
            // корень, которого уже не существует.
            task.user = UserMachine::default();
        }
        sched.wake_waiters(id);
    }

    schedule();

    // Возврата сюда быть не может: `pick_next` отбирает только `Ready`, а
    // завершённая задача уже не `Ready`.
    kprintln!("sched: FATAL: a finished task was resumed");
    smp::stop_others();
    arch::halt();
}

/// Сообщить о переполнении стека и остановиться.
///
/// Продолжать нельзя: полоса-сторож испорчена, значит запись ушла ниже дна
/// стека, и что именно она задела — уже неизвестно.
fn report_overflow(name: &'static str, id: TaskId) -> ! {
    // Сначала остальные процессоры: лок планировщика держим мы и не отпустим
    // никогда, и соседи иначе повисли бы на нём молча.
    smp::stop_others();
    kprintln!();
    kprintln!("*** TASK STACK OVERFLOW ***");
    kprintln!("  task      : {id} ({name})");
    kprintln!("  stack     : {} KiB usable, {} KiB guard band", TASK_STACK_SIZE / 1024, STACK_GUARD_SIZE / 1024);
    kprintln!("  the guard band below the stack was overwritten; neighbouring heap");
    kprintln!("  memory is likely corrupted, so execution stops here.");
    arch::halt();
}

// ---- демонстрация ------------------------------------------------------------

/// Сколько раз каждая задача демонстрации уступает процессор.
const DEMO_ROUNDS: u32 = 3;

/// Тело задачи демонстрации.
fn demo_task() {
    for round in 1..=DEMO_ROUNDS {
        kprintln!("  {:<7}: round {round} of {DEMO_ROUNDS}", current_name());
        yield_now();
    }
    DEMO_RUNNING.fetch_sub(1, Ordering::Relaxed);
}

/// Сколько демонстрационных задач ещё говорит.
///
/// Приглашение ядра ждёт **их**, а не «всех остальных»: на телефоне обычная
/// задача USB не заканчивается никогда, и ожидание «пока не останется одна
/// живая задача» не кончилось бы вовсе.
static DEMO_RUNNING: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[must_use]
pub fn demo_running() -> usize {
    DEMO_RUNNING.load(Ordering::Relaxed)
}

/// Создать демонстрационные задачи: несколько задач по очереди печатают себя и
/// завершаются.
///
/// Возвращает число созданных задач.
pub fn spawn_demo_tasks() -> usize {
    let mut spawned = 0;
    for name in ["alpha", "beta", "gamma"] {
        // Счётчик поднимается **до** создания задачи: задача, начавшая печатать
        // раньше, чем её посчитали, — это гонка, в которой приглашение решает,
        // что говорить уже некому.
        DEMO_RUNNING.fetch_add(1, Ordering::Relaxed);
        match spawn(name, demo_task) {
            Ok(_) => spawned += 1,
            Err(err) => {
                DEMO_RUNNING.fetch_sub(1, Ordering::Relaxed);
                kprintln!("  spawn {name} failed: {err}");
            }
        }
    }
    spawned
}
