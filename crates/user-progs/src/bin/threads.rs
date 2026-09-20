//! Программа, которая заводит потоки.
//!
//! Существует ради четырёх утверждений, каждое из которых на однопоточном ядре
//! проверить нечем.
//!
//! 1. **Память у потоков общая.** Счётчик один на всех, и сумма обязана
//!    сойтись точно. Разные адресные пространства дали бы каждому потоку свою
//!    копию, и сумма оказалась бы вчетверо меньше — самая частая ошибка первой
//!    редакции такой фазы.
//! 2. **Хранилище у каждого своё.** Каждый поток кладёт в свою базу своё число
//!    и читает его обратно **через регистр базы**, а не по запомненному
//!    адресу: совпадение у двоих означало бы один регистр на всех, то есть
//!    незаписанный `FS` / `TPIDR_EL0`.
//! 3. **Завершение потока не убивает процесс.** Потоки уходят по одному, а
//!    главная задача продолжает работать и печатает итог.
//! 4. **Замок на ожидании по адресу работает и вправду усыпляет.** Под ним
//!    лежит **неатомарный** счётчик: атомарный сошёлся бы и без замка, и
//!    проверка доказывала бы только то, что атомики работают. Плюс число
//!    засыпаний — замок, крутящий процессор вместо сна, выглядит снаружи так
//!    же, как настоящий.
//!
//! Со словом `guard` программа вместо этого переполняет стек потока — доказывая,
//! что под ним лежит сторожевая страница и переполнение кончается снятием, а не
//! порчей соседней области.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use user_progs::{Args, Lock, exit, print, print_u64, println, set_tls, sleep_ms, thread_create, thread_exit};

/// Сколько потоков завести.
const THREADS: usize = 4;

/// Сколько раз каждый прибавит к общему счётчику.
const STEPS: u32 = 1000;

/// Стек потока. Одна страница под сторожем — этой программе хватает с запасом.
const STACK_BYTES: usize = 16 * 1024;

/// Общий счётчик — то самое, что доказывает общую память.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Замок на потоках (фаза 55c) и счётчик **без** атомарности под ним.
///
/// Неатомарный нарочно: атомарный счётчик сошёлся бы и без замка, и проверка
/// доказывала бы только то, что атомики работают. Обычное сложение,
/// разорванное посередине, теряет прибавления — и сумма это покажет.
static LOCK: Lock = Lock::new();
static mut GUARDED: u64 = 0;

/// Сколько потоков уже закончили.
static DONE: AtomicUsize = AtomicUsize::new(0);

/// Сколько потоков увидели в своём хранилище **чужое** число.
static TLS_WRONG: AtomicUsize = AtomicUsize::new(0);

/// Хранилище потока: по одному на каждого, в обычной памяти программы.
///
/// Лежит в общей памяти нарочно: раздельность обеспечивает не размещение, а
/// регистр базы. Положи мы их в разные области — проверка доказывала бы, что
/// разные адреса дают разные значения, то есть ничего.
#[repr(C, align(16))]
struct ThreadLocal {
    /// Первое слово: его читают через базу, а не по адресу.
    mark: u64,
}

static mut LOCALS: [ThreadLocal; THREADS] = [const { ThreadLocal { mark: 0 } }; THREADS];

/// Прочитать первое слово своего хранилища — **через регистр базы**.
///
/// На x86-64 это обращение через сегмент `FS`: адрес складывается из базы,
/// которую ядро положило в `IA32_FS_BASE`, и смещения. На AArch64 сегментов
/// нет, поэтому база сначала читается из `TPIDR_EL0`, а потом по ней идёт
/// обычное обращение. В обоих случаях адрес берётся из регистра, а не из
/// памяти программы, — иначе проверка не проверяла бы ничего.
fn read_own_mark() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let value: u64;
        // SAFETY: база сегмента `FS` указывает на `ThreadLocal` этого потока —
        // её поставил сам поток через `set_tls`.
        unsafe {
            core::arch::asm!("mov {}, fs:[0]", out(reg) value, options(nostack, readonly));
        }
        value
    }
    #[cfg(target_arch = "aarch64")]
    {
        let base: u64;
        // SAFETY: чтение `TPIDR_EL0` побочных эффектов не имеет.
        unsafe {
            core::arch::asm!("mrs {}, tpidr_el0", out(reg) base, options(nomem, nostack));
        }
        // SAFETY: база указывает на `ThreadLocal` этого потока.
        unsafe { core::ptr::read_volatile(base as *const u64) }
    }
}

/// Точка входа потока. Номер приезжает аргументом.
extern "C" fn worker(index: usize) -> ! {
    // Хранилище поток ставит себе сам: так не нужно ни передавать адрес через
    // создание, ни гадать, когда новый поток дойдёт до своей инициализации.
    //
    // SAFETY: индекс приходит от главной задачи и меньше `THREADS`; у каждого
    // потока свой элемент, и чужого никто не трогает.
    let local = unsafe { &raw mut LOCALS[index] };
    // SAFETY: адрес принадлежит памяти программы и живёт столько же, сколько
    // она сама.
    unsafe {
        (*local).mark = 0xA500 + index as u64;
    }
    set_tls(local as usize);

    for _ in 0..STEPS {
        COUNTER.fetch_add(1, Ordering::Relaxed);
        LOCK.acquire();
        // SAFETY: обращение к общему числу происходит под замком, и другого
        // пути к нему в программе нет.
        unsafe {
            GUARDED = core::ptr::read_volatile(&raw const GUARDED) + 1;
        }
        // Участок под замком нарочно не мгновенный. С мгновенным потоки на
        // нём не встречаются вовсе: замок берётся и отпускается быстрее, чем
        // истекает квант, и ожидание по адресу не срабатывает ни разу — то
        // есть проверка 55c не проверяет ничего. Первая редакция так и
        // показала: «усыпил 0 раз» при сошедшемся счётчике.
        for _ in 0..400 {
            core::hint::black_box(0u32);
        }
        LOCK.release();
    }

    // Сон между записью и чтением — не украшение: он даёт планировщику увести
    // процессор к другому потоку, и если база одна на всех, обратно вернётся
    // чужое число.
    sleep_ms(20);
    if read_own_mark() != 0xA500 + index as u64 {
        TLS_WRONG.fetch_add(1, Ordering::Relaxed);
    }

    print("threads: worker ");
    print_u64(index as u64);
    println(" done");
    DONE.fetch_add(1, Ordering::Relaxed);
    thread_exit(0)
}

/// Точка входа потока, который переполняет свой стек.
extern "C" fn overflower(_arg: usize) -> ! {
    println("threads: about to run off the bottom of a thread stack");
    // Рекурсия, которую нечем свернуть: каждый виток съедает кадр, и упереться
    // она обязана в сторожевую страницу.
    let depth = core::hint::black_box(0usize);
    eat_stack(depth);
    println("threads: the thread stack had no guard below it");
    thread_exit(1)
}

/// Съесть стек. Не встраивается — иначе кадра не будет вовсе.
#[inline(never)]
fn eat_stack(depth: usize) -> usize {
    let filler = core::hint::black_box([depth as u8; 512]);
    if filler[0] == 0xFF {
        return depth;
    }
    eat_stack(depth + 1) + usize::from(filler[1])
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра ровно в том виде, в каком их описывает
    // договор.
    let args = unsafe { Args::new(argc, argv) };

    if args.get(1) == Some("guard") {
        if thread_create(overflower, 0, STACK_BYTES).is_none() {
            println("threads: could not create the thread");
            exit(1);
        }
        // Ждём долго и вслух: если сторожа нет, поток вернётся и напечатает об
        // этом сам, а сценарий ловит обе строки.
        sleep_ms(3_000);
        println("threads: the overflowing thread is gone");
        exit(0);
    }

    println("threads: starting 4 threads");
    for index in 0..THREADS {
        match thread_create(worker, index, STACK_BYTES) {
            Some(id) => {
                print("threads: started thread ");
                print_u64(u64::from(id));
                println("");
            }
            None => {
                println("threads: could not create a thread");
                exit(1);
            }
        }
    }

    // Ждём уступками, а не сном: проверка про потоки, и лишний механизм в ней
    // только мешал бы понять, что именно сломалось. Предел витков конечен —
    // повиснуть в проверке хуже, чем провалить её.
    let mut spins = 0u32;
    while DONE.load(Ordering::Relaxed) < THREADS && spins < 60_000 {
        spins += 1;
        sleep_ms(1);
    }

    // SAFETY: все потоки закончили — сюда мы дошли, только дождавшись их.
    let guarded = unsafe { core::ptr::read_volatile(&raw const GUARDED) };
    print("threads: guarded counter is ");
    print_u64(guarded);
    print(" of ");
    print_u64(u64::from(STEPS) * THREADS as u64);
    println("");
    print("threads: the lock put someone to sleep ");
    print_u64(u64::from(LOCK.sleeps()));
    println(" time(s)");

    print("threads: counter is ");
    print_u64(u64::from(COUNTER.load(Ordering::Relaxed)));
    print(" of ");
    print_u64(u64::from(STEPS) * THREADS as u64);
    println("");

    print("threads: ");
    print_u64(DONE.load(Ordering::Relaxed) as u64);
    println(" thread(s) finished, and the process is still here");

    let wrong = TLS_WRONG.load(Ordering::Relaxed);
    if wrong == 0 {
        println("threads: every thread read back its own storage");
    } else {
        print("threads: ");
        print_u64(wrong as u64);
        println(" thread(s) read someone else's storage");
    }
    exit(0)
}
