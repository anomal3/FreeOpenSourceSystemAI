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

mod task;

pub use task::{
    SpawnError, TaskId, TaskState, UserMachine, Wait, STACK_GUARD_SIZE, TASK_STACK_SIZE,
};

use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

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
pub const MAX_TASKS: usize = 32 + MAX_CPUS;

/// Длина кванта в тиках таймера. При `TIMER_HZ = 100` это 50 мс — заметно
/// больше, чем стоит переключение, и заметно меньше, чем человек замечает.
const SLICE_TICKS: u32 = 5;

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

/// Сколько раз попросили не отбирать процессор.
///
/// Счётчик, а не флаг: просьбы вкладываются друг в друга, и «включить обратно»
/// после внутренней означало бы отдать процессор посреди внешней.
///
/// Один на всю машину, а не на процессор, и это осознанно. Просьбу держит
/// задача, а не процессор: задача, попросившая, может уснуть на мьютексе вывода
/// и проснуться на другом процессоре — и снятие просьбы там расстроило бы
/// счётчики обоих. Цена общего счётчика — пока одна задача печатает строку,
/// квант не истекает ни у кого; строка печатается миллисекунды.
static PREEMPT_HELD: AtomicUsize = AtomicUsize::new(0);

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
static IDLE_TICKS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

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
    /// Запущено ли планирование. До [`run`] переключаться не на что и не с чего.
    running: bool,
}

static SCHED: SpinLock<Scheduler> = SpinLock::new(Scheduler::new());

/// Принадлежит ли слот холостой задаче какого-нибудь процессора.
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
        Self { tasks: [const { None }; MAX_TASKS], cpus, next_id: 1, running: false }
    }

    /// Слот задачи, исполняющейся на процессоре вызывающего.
    ///
    /// Вызывать только под локом: лок запрещает прерывания, и номер процессора
    /// не может смениться между вопросом и ответом.
    fn current(&self) -> usize {
        self.cpus[smp::cpu()].current
    }

    /// Куда положить новую задачу.
    ///
    /// Сначала пустой слот, потом — самый давний из завершённых, чей стек уже
    /// никем не исполняется. Второй проход появился вместе с программами-
    /// задачами: каждая команда `run` создаёт новую, и таблица кончилась бы на
    /// тридцать второй программе.
    fn free_slot(&self) -> Option<usize> {
        if let Some(empty) = (MAX_CPUS..MAX_TASKS).find(|&i| self.tasks[i].is_none()) {
            return Some(empty);
        }
        (MAX_CPUS..MAX_TASKS)
            .filter(|&i| {
                self.tasks[i]
                    .as_ref()
                    .is_some_and(|task| task.state == TaskState::Finished && task.cpu.is_none())
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
    fn pick_next(&self, cpu: usize) -> Option<usize> {
        let current = self.cpus[cpu].current;
        for step in 1..MAX_TASKS {
            let slot = (current + step) % MAX_TASKS;
            if is_idle_slot(slot) {
                continue;
            }
            if let Some(task) = &self.tasks[slot] {
                if task.state == TaskState::Ready && task.cpu.is_none() {
                    return Some(slot);
                }
            }
        }
        if is_idle_slot(current) {
            return None;
        }
        // Готова ли текущая задача продолжать: `Ready` здесь означает, что её
        // разбудили между «иду спать» и «уступаю процессор», — и уходить с него
        // ради того, чтобы тут же вернуться, незачем.
        match self.tasks[current].as_ref().map(|task| task.state) {
            Some(TaskState::Running | TaskState::Ready) => None,
            _ => Some(cpu),
        }
    }

    /// Разбудить всех, чей срок ожидания вышел.
    ///
    /// Проход по всей таблице на каждом переключении — это сорок сравнений, то
    /// есть заметно дешевле одного обращения к памяти мимо кеша.
    ///
    /// Возвращает `true`, если кто-то проснулся.
    fn wake_expired(&mut self, now: u64) -> bool {
        let mut woke = false;
        for task in self.tasks.iter_mut().flatten() {
            let due = match task.state {
                TaskState::Blocked(Wait::Until(tick) | Wait::Input(tick)) => now >= tick,
                _ => false,
            };
            if due {
                task.state = TaskState::Ready;
                woke = true;
            }
        }
        woke
    }

    /// Разбудить тех, кто ждал завершения задачи `id`.
    fn wake_waiters(&mut self, id: TaskId) {
        for task in self.tasks.iter_mut().flatten() {
            if task.state == TaskState::Blocked(Wait::Task(id)) {
                task.state = TaskState::Ready;
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
        let mut freed = 0;
        for (slot, entry) in self.tasks.iter_mut().enumerate() {
            if is_idle_slot(slot) {
                continue;
            }
            if let Some(task) = entry {
                if task.state == TaskState::Finished && task.cpu.is_none() && task.stack.is_some() {
                    // Вот здесь и происходит освобождение: `Drop for Stack`
                    // возвращает блок куче.
                    task.stack = None;
                    freed += 1;
                }
            }
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
    fn finish_switch(&mut self) {
        let cpu = smp::cpu();
        if let Some(previous) = self.cpus[cpu].previous.take() {
            if let Some(task) = self.tasks[previous].as_mut() {
                task.cpu = None;
            }
        }
    }

    /// Сколько всего было переключений, на всех процессорах.
    fn switches(&self) -> u64 {
        self.cpus.iter().map(|cpu| cpu.switches).sum()
    }

    /// Сколько из них по истёкшему кванту.
    fn forced(&self) -> u64 {
        self.cpus.iter().map(|cpu| cpu.forced).sum()
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
    let mut sched = SCHED.lock();
    if let Some(task) = sched.tasks.iter_mut().flatten().find(|task| task.id == id) {
        task.daemon = true;
    }
}

/// Служебная ли задача исполняется сейчас.
///
/// Спрашивает [`crate::user::syscall`]: службу запускает служба, и ребёнок
/// супервизора обязан унаследовать это свойство.
#[must_use]
pub fn is_daemon() -> bool {
    let sched = SCHED.lock();
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

    let mut sched = SCHED.lock();
    let Some(slot) = sched.free_slot() else {
        // `stack` уничтожается после охранника (обратный порядок объявления
        // локальных), то есть куча трогается уже без лока.
        return Err(SpawnError::TooManyTasks);
    };

    let id = TaskId::new(sched.next_id);
    // Контекст ведёт в общий батут, а настоящая точка входа ждёт в самой задаче:
    // первое, что обязан сделать новый стек, — снять метку с того, кто уступил
    // ему процессор (см. [`task_start`]).
    let mut task = Task::new(id, name, stack, task_start, slot);
    task.entry = Some(entry);
    task.arg = arg;
    let boxed = box_task(task).ok_or(SpawnError::OutOfMemory)?;

    sched.next_id += 1;
    sched.tasks[slot] = Some(boxed);
    Ok(id)
}

/// Номер слота исполняющейся задачи.
///
/// Слот, а не идентификатор: по нему индексируются таблицы, которые ведут о
/// задаче другие модули — адресное пространство программы и её открытые файлы
/// (см. [`crate::user`]). Слот задачи не меняется, когда её переносят на другой
/// процессор, поэтому ответ остаётся верным и после вытеснения.
#[must_use]
pub fn current_slot() -> usize {
    SCHED.lock().current()
}

/// Слот и состояние задачи по её идентификатору.
///
/// `None` означает «такой задачи нет»: либо она никогда не создавалась, либо её
/// слот уже переиспользован под другую.
#[must_use]
pub fn lookup(id: TaskId) -> Option<(usize, TaskState)> {
    let sched = SCHED.lock();
    sched
        .tasks
        .iter()
        .enumerate()
        .find_map(|(slot, entry)| match entry {
            Some(task) if task.id == id && !is_idle_slot(slot) => Some((slot, task.state)),
            _ => None,
        })
}

/// Объявить, в каком адресном пространстве исполняется текущая задача.
///
/// `None` возвращает её в пространство ядра. Значение переставляет процессор не
/// здесь, а на ближайшем переключении задач; сам переход выполняет вызывающий,
/// потому что он же и знает, в какой момент это безопасно.
pub fn set_current_space(root: Option<crate::mm::PhysAddr>) {
    let mut sched = SCHED.lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.user.space_root = root;
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
    let mut sched = SCHED.lock();
    let current = sched.current();
    if let Some(task) = sched.tasks[current].as_mut() {
        task.user.return_stack = stack;
    }
    arch::set_trap_stack(stack);
}

/// Стек, на который текущая задача возвращается из третьего кольца. Ноль —
/// задача программ не запускала.
pub extern "C" fn return_stack() -> usize {
    let sched = SCHED.lock();
    let current = sched.current();
    sched.tasks[current].as_ref().map_or(0, |task| task.user.return_stack)
}

/// Завершилась ли задача, и с каким кодом.
///
/// `None` — задача ещё жива либо её слот уже переиспользован под другую.
#[must_use]
pub fn result_of(id: TaskId) -> Option<i64> {
    let sched = SCHED.lock();
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
            let mut sched = SCHED.lock();
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
                        task.state = TaskState::Blocked(Wait::Task(id));
                    }
                }
            }
        }
        schedule();
    }
}

/// Уснуть на указанное число миллисекунд.
///
/// Округление вверх — до целого тика: вернуться раньше срока хуже, чем
/// проспать лишние девять миллисекунд. Ноль означает «не спать вовсе».
pub fn sleep_ms(ms: u64) {
    if ms == 0 {
        return;
    }
    let hz = u64::from(crate::irq::TIMER_HZ);
    let ticks = ms.saturating_mul(hz).div_ceil(1000).max(1);
    sleep_until(crate::irq::ticks().saturating_add(ticks));
}

/// Уснуть до тика с указанным номером.
pub fn sleep_until(tick: u64) {
    {
        let mut sched = SCHED.lock();
        if !sched.running {
            // До запуска планирования спать не на чем и некому.
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            task.state = TaskState::Blocked(Wait::Until(tick));
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
        let mut sched = SCHED.lock();
        if !sched.running || changed() {
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            task.state = TaskState::Blocked(Wait::Input(until));
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
    {
        let mut sched = SCHED.lock();
        if !sched.running || free() {
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            task.state = TaskState::Blocked(Wait::Lock(address));
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
        let mut sched = SCHED.lock();
        if !sched.running || ready() {
            return;
        }
        let current = sched.current();
        if let Some(task) = sched.tasks[current].as_mut() {
            task.state = TaskState::Blocked(Wait::Irq(source));
        }
    }
    schedule();
}

/// Разбудить тех, кто ждёт прерывания от источника `source`.
///
/// Вызывается из обработчика прерывания, поэтому делает ровно одно: переводит
/// ждущих в готовые. Разбор события — работа задачи, а не обработчика.
pub fn wake_irq(source: u32) {
    let mut sched = SCHED.lock();
    for task in sched.tasks.iter_mut().flatten() {
        if task.state == TaskState::Blocked(Wait::Irq(source)) {
            task.state = TaskState::Ready;
        }
    }
}

/// Разбудить тех, кто ждёт лок по адресу `address`.
///
/// Будятся **все** ждущие, а не один: проснувшиеся попробуют захватить лок по
/// очереди, и не сумевшие уснут снова.
pub fn wake_lock(address: usize) {
    let mut sched = SCHED.lock();
    for task in sched.tasks.iter_mut().flatten() {
        if task.state == TaskState::Blocked(Wait::Lock(address)) {
            task.state = TaskState::Ready;
        }
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
    for task in sched.tasks.iter_mut().flatten() {
        if matches!(task.state, TaskState::Blocked(Wait::Input(_))) {
            task.state = TaskState::Ready;
        }
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
    let Some(mut sched) = SCHED.try_lock() else {
        return;
    };
    let found = sched
        .tasks
        .iter_mut()
        .enumerate()
        .find(|(slot, entry)| !is_idle_slot(*slot) && entry.as_ref().is_some_and(|t| t.id == id))
        .and_then(|(_, entry)| entry.as_mut());
    if let Some(task) = found {
        if matches!(task.state, TaskState::Blocked(_)) {
            task.state = TaskState::Ready;
        }
    }
}

/// Добровольно уступить процессор.
///
/// Безопасно вызывать до [`run`] и вне любой задачи: [`schedule`] в этом случае
/// просто ничего не делает.
pub fn yield_now() {
    schedule();
}

/// Единственная точка переключения контекста в ядре.
pub fn schedule() {
    schedule_with(Cause::Voluntary);
}

/// Переключиться, помня, по чьей воле это происходит.
fn schedule_with(cause: Cause) {
    // Прерывания запрещаются здесь, а не полагаясь на `SpinLock`, и остаются
    // запрещёнными на всё переключение. Почему именно так — подробно в
    // заголовке модуля («Главная ловушка»).
    let irq_was_enabled = arch::interrupts::enabled();
    arch::interrupts::disable();

    let mut from: *mut Context = ptr::null_mut();
    let mut to: *const Context = ptr::null();

    {
        let mut sched = SCHED.lock();
        // Номер процессора стабилен до конца блока: прерывания запрещены, и
        // перенести эту задачу на другой процессор нечем.
        let cpu = smp::cpu();
        if sched.running {
            sched.reap();
            // Сроки проверяются здесь, а не в обработчике таймера: обработчик
            // обязан быть коротким и не имеет права ждать лока.
            sched.wake_expired(crate::irq::ticks());

            let current = sched.cpus[cpu].current;
            match sched.pick_next(cpu) {
                None => {
                    // Остаёмся. Задача, которую разбудили до того, как она ушла,
                    // снова исполняется, а не числится готовой.
                    if !is_idle_slot(current) {
                        if let Some(task) = sched.tasks[current].as_mut() {
                            if task.state == TaskState::Ready {
                                task.state = TaskState::Running;
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
                        let next_user = sched.tasks[next].as_ref().map_or_else(
                            task::UserMachine::default,
                            |task| task.user,
                        );
                        // Стек ловушки — тот же адрес, что стек возврата из
                        // третьего кольца, и лежит он в задаче. Ставится на
                        // **этом** процессоре: здесь задача и продолжится.
                        arch::set_trap_stack(next_user.return_stack);

                        // Адресное пространство. У задачи без программы его нет,
                        // и тогда процессор возвращается на таблицы ядра.
                        // SAFETY: любой корень программы построен копией
                        // ядерного и содержит все его отображения, поэтому
                        // переключение не уводит из-под ног ни код, ни стек.
                        unsafe {
                            match next_user.space_root {
                                Some(root) => arch::activate_space(root),
                                None => arch::activate_kernel_space(),
                            }
                        }

                        // Векторные регистры — здесь же, под тем же локом и с
                        // теми же запрещёнными прерываниями. Регистры принадлежат
                        // процессору, а область — задаче, поэтому сохранить своё
                        // и поставить чужое можно только здесь, до смены стека.
                        let save_to = sched.tasks[current]
                            .as_ref()
                            .and_then(|task| task.fpu.as_ref())
                            .map(task::FpuArea::as_ptr);
                        let load_from = sched.tasks[next]
                            .as_ref()
                            .and_then(|task| task.fpu.as_ref())
                            .map(task::FpuArea::as_ptr);
                        if let Some(area) = save_to {
                            // SAFETY: область принадлежит задаче, которая
                            // исполняется прямо сейчас, выделена с нужным
                            // размером и выравниванием и жива, пока жива задача.
                            unsafe { arch::fpu::save(area) };
                        }
                        if let Some(area) = load_from {
                            // SAFETY: область заполнена либо `init_area` при
                            // создании задачи, либо предыдущим сохранением.
                            unsafe { arch::fpu::restore(area) };
                        }

                        if let Some(task) = sched.tasks[current].as_mut() {
                            if task.state == TaskState::Running {
                                task.state = TaskState::Ready;
                            }
                            // Счётчик снятий висит на **покидаемой** задаче:
                            // вопрос, на который он отвечает, — уступает ли она
                            // сама. Метка процессора с неё не снимается: её
                            // снимет следующая задача, когда переключение
                            // закончится.
                            if cause == Cause::Preempted {
                                task.preempted += 1;
                            }
                        }
                        if let Some(task) = sched.tasks[next].as_mut() {
                            task.state = TaskState::Running;
                            task.cpu = Some(cpu);
                            task.switches += 1;
                        }
                        let state = &mut sched.cpus[cpu];
                        state.previous = Some(current);
                        state.current = next;
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
    }

    if !from.is_null() && !to.is_null() {
        // SAFETY: оба указателя ведут в `MaybeUninit<Context>` внутри живых
        // `Box<Task>`. Ни одну из двух задач не освободит никто: уходящая несёт
        // метку этого процессора, и уборщик её не тронет, пока метку не снимет
        // `finish_switch`; приходящая помечена этим процессором только что. Та
        // же метка не даёт соседнему процессору взять ни одну из них. `to`
        // описывает задачу с отображённым стеком — построенную `Context::new`
        // либо сохранённую предыдущим вызовом этой функции.
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
    SCHED.lock().finish_switch();
}

/// Батут, с которого начинает любая задача, созданная [`spawn_raw`].
///
/// Существует ради одного действия: снять метку с задачи, уступившей процессор.
/// Обычное переключение делает это после возврата из `switch_context`, а у новой
/// задачи возврата нет — её стек начинается здесь. Забудь она, и уступившая
/// задача осталась бы помеченной навсегда: её не взял бы ни один процессор, а
/// завершившуюся — не убрал бы уборщик.
extern "C" fn task_start(slot: usize) -> ! {
    let start = {
        let mut sched = SCHED.lock();
        sched.finish_switch();
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

/// Учёт кванта. Предназначено для вызова из `irq::on_timer_tick`.
///
/// Ничего не переключает — только считает и, когда квант истёк, поднимает флаг
/// для [`preempt_point`]. Лока не берёт вовсе: всё, что трогает, принадлежит
/// своему процессору (см. [`SLICE_LEFT`]).
pub fn on_timer_tick() {
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
    let left = SLICE_LEFT[cpu].load(Ordering::Relaxed).saturating_sub(1);
    if left == 0 {
        SLICE_LEFT[cpu].store(SLICE_TICKS, Ordering::Relaxed);
        NEED_RESCHED[cpu].store(true, Ordering::Relaxed);
    } else {
        SLICE_LEFT[cpu].store(left, Ordering::Relaxed);
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
    // Кто-то просил не отбирать процессор. Флаг `NEED_RESCHED` при этом **не
    // сбрасывается**: он подождёт ближайшего прерывания после того, как просьбу
    // снимут.
    if PREEMPT_HELD.load(Ordering::Relaxed) != 0 {
        return;
    }
    // `swap`, а не пара «прочитать — сбросить»: флаг поднимает обработчик
    // таймера, и между чтением и сбросом он успел бы прийти ещё раз.
    if !NEED_RESCHED[smp::cpu()].swap(false, Ordering::Relaxed) {
        return;
    }
    schedule_with(Cause::Preempted);
}

/// Попросить не отбирать процессор, пока живёт возвращённое значение.
///
/// # Зачем это понадобилось
///
/// Строка в журнале — единственное доказательство, которым располагает и
/// человек, и стенд. Собирается она из кусков, и с вытеснением её разрывает
/// чужой вывод посередине. Прерывания при этом **не** запрещаются: строка в UART
/// уходит байтами с ожиданием готовности.
///
/// На нескольких процессорах одной этой просьбы мало: соседний процессор печатает
/// независимо от того, отбирают ли процессор у нас. Там строку держит целой лок
/// вывода, который ждёт соседа, а не пишет поверх него (см. [`crate::serial`]).
pub fn hold_preemption() -> PreemptionHold {
    PREEMPT_HELD.fetch_add(1, Ordering::Relaxed);
    PreemptionHold(())
}

/// Просьба не отбирать процессор; снимается при уничтожении.
pub struct PreemptionHold(());

impl Drop for PreemptionHold {
    fn drop(&mut self) {
        PREEMPT_HELD.fetch_sub(1, Ordering::Relaxed);
    }
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

    let mut sched = SCHED.lock();
    sched.tasks[index] = Some(boxed);
    sched.cpus[index].current = index;
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
    let task = SCHED.lock().tasks[index].take();
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
        let mut sched = SCHED.lock();
        if sched.running {
            kprintln!("sched: FATAL: run() called twice");
            arch::halt();
        }
        sched.tasks[0] = Some(idle);
        sched.cpus[0].current = 0;
        sched.running = true;
    }
    RUNNING.store(true, Ordering::Release);

    // Холостая задача загрузочного процессора живёт здесь. Процессор
    // останавливается до ближайшего прерывания, когда готовых задач нет, — ровно
    // так же, как холостой цикл остальных процессоров в `smp::secondary_main`.
    // Отличие одно: только этот цикл решает, что работа кончилась.
    loop {
        let alive = {
            let sched = SCHED.lock();
            sched.alive()
        };
        if alive == 0 {
            break;
        }
        schedule();

        // Гонка между этой проверкой и остановкой безобидна: прерывание,
        // пришедшее в промежутке, разбудит задачу, но `wfi`/`hlt` уже начнётся —
        // и закончится на ближайшем тике таймера, то есть не позже чем через
        // 10 мс.
        arch::wait_for_interrupt();
    }

    // Остальные процессоры останавливаются до сводки: иначе служебные задачи на
    // них продолжали бы исполняться после строки «CPU halted», и сводка
    // описывала бы машину, которая всё ещё работает.
    let stopped = smp::stop_others();

    let (freed, switches) = {
        let mut sched = SCHED.lock();
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
    let sched = SCHED.lock();
    sched.tasks[sched.current()].as_ref().map_or(TaskId::IDLE, |task| task.id)
}

/// Имя исполняющейся задачи.
#[must_use]
pub fn current_name() -> &'static str {
    let sched = SCHED.lock();
    sched.tasks[sched.current()].as_ref().map_or("none", |task| task.name)
}

/// Сколько всего было переключений контекста.
#[must_use]
pub fn switch_count() -> u64 {
    SCHED.lock().switches()
}

/// Запущено ли планирование.
#[must_use]
pub fn is_running() -> bool {
    SCHED.lock().running
}

/// Сколько задач ещё не завершились, не считая холостых.
#[must_use]
pub fn alive() -> usize {
    SCHED.lock().alive()
}

/// Компактный список задач с состояниями.
pub fn dump() {
    // Строки собираются и печатаются под локом. Это допустимо ровно потому, что
    // `kprintln!` не обращается к планировщику: иначе получилась бы рекурсия на
    // неперевходимом локе.
    let sched = SCHED.lock();
    for entry in sched.tasks.iter().flatten() {
        let stack = match entry.stack.as_ref() {
            Some(stack) if stack.guard_intact() => "held",
            Some(_) => "OVERFLOWN",
            None => "freed",
        };
        kprintln!(
            "  {} {:<8} {:<9} {:>3} switches ({} forced), stack {}",
            entry.id,
            entry.name,
            entry.state,
            entry.switches,
            entry.preempted,
            stack
        );
    }
    kprintln!(
        "  preemption : {}, {} ms slice, {} forced switch(es)",
        if PREEMPTION.load(Ordering::Relaxed) { "on" } else { "off" },
        SLICE_MS,
        sched.forced()
    );

    // Доля простоя — по всем работающим процессорам сразу: тиков у каждого
    // столько же, сколько у загрузочного, а считает их только он.
    let online: alloc::vec::Vec<usize> =
        (0..MAX_CPUS).filter(|&cpu| sched.tasks[cpu].is_some()).collect();
    let ticks = crate::irq::ticks();
    let idle_total: u64 = online.iter().map(|&cpu| IDLE_TICKS[cpu].load(Ordering::Relaxed)).sum();
    let span = ticks.saturating_mul(online.len().max(1) as u64);
    let percent = if span == 0 { 0 } else { idle_total * 100 / span };
    kprintln!(
        "  idle       : {}% of {ticks} tick(s) with nothing to run",
        percent
    );

    // Строка на процессор — проверка, которой до фазы 43 не существовало. Общий
    // счётчик переключений не отличит «четыре процессора работают» от «один
    // работает за четверых»; счётчик на каждом процессоре — отличит, а «кто
    // исполняется прямо сейчас» показывает одновременность напрямую: две
    // задачи не могут исполняться в один миг на одном процессоре.
    for &cpu in &online {
        let state = &sched.cpus[cpu];
        let what = if is_idle_slot(state.current) {
            alloc::string::String::from("idle")
        } else {
            match sched.tasks[state.current].as_ref() {
                Some(task) => alloc::format!("running {} ({})", task.id, task.name),
                None => alloc::string::String::from("unknown"),
            }
        };
        let idle = IDLE_TICKS[cpu].load(Ordering::Relaxed);
        let idle_percent = if ticks == 0 { 0 } else { idle * 100 / ticks };
        kprintln!(
            "  cpu {cpu}      : {what}, {} switches ({} forced), idle {idle_percent}% of its ticks",
            state.switches,
            state.forced
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
        let mut sched = SCHED.lock();
        let current = sched.current();
        let mut id = TaskId::IDLE;
        if let Some(task) = sched.tasks[current].as_mut() {
            task.state = TaskState::Finished;
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
