//! `perf-diag` (P1 плана Proton, 18.11.2026): раз в 30 с, пока программы
//! Linux делают системные вызовы, — строки в журнал ядра о том, куда ушло
//! время: системные вызовы по времени и числу, отказы страниц по видам,
//! сбросы трансляций, futex, простой процессоров и очередь планировщика,
//! кадры сервера Wayland и стола (копия буфера клиента, сборка, вывод по
//! мониторам).
//!
//! Зачем: в журналах флешки с ПК Романа (hw-log8c/9c) не было ни одной
//! цифры о скорости — «тормозит» нельзя было объяснить ничем, кроме
//! догадок. Журнал флешки возвращается всегда; `lxstat` по кабелю — нет.
//!
//! Цена: счётчики — атомики, которые и так ведутся (`profile`, стол); сам
//! отчёт — одна служебная задача, которая просыпается четыре раза в секунду
//! взглянуть на очередь планировщика и раз в 30 с печатает. Нет вызовов
//! Linux за окно — нет и строк. Команда `perfdiag on|off|now`.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::profile;
use crate::kprintln;

/// Окно отчёта.
const PERIOD_MS: u64 = 30_000;
/// Шаг выборки очереди планировщика.
const SAMPLE_MS: u64 = 250;
/// Сколько вызовов в строке «по времени» и «по числу».
const TOP: usize = 8;

static ENABLED: AtomicBool = AtomicBool::new(true);
static STARTED: AtomicBool = AtomicBool::new(false);
/// `perfdiag now` — напечатать, не дожидаясь конца окна.
static FORCE: AtomicBool = AtomicBool::new(false);

/// futex Linux: ожидания, пробуждения (вызовы) и сколько потоков разбудили;
/// `futex_waitv` — отдельно (fsync Wine).
pub static FUTEX_WAITS: AtomicU64 = AtomicU64::new(0);
pub static FUTEX_WAKES: AtomicU64 = AtomicU64::new(0);
pub static FUTEX_WOKEN: AtomicU64 = AtomicU64::new(0);
pub static FUTEXV_WAITS: AtomicU64 = AtomicU64::new(0);

/// Сервер Wayland: `commit`ы, перерисовки окон, наносекунды и точки копии
/// буфера клиента в окно стола, отвеченные `frame`, перерисовки, не
/// прошедшие из-за занятого стола.
pub static WL_COMMITS: AtomicU64 = AtomicU64::new(0);
pub static WL_PAINTS: AtomicU64 = AtomicU64::new(0);
pub static WL_PAINT_NS: AtomicU64 = AtomicU64::new(0);
pub static WL_PAINT_PX: AtomicU64 = AtomicU64::new(0);
pub static WL_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static WL_BUSY: AtomicU64 = AtomicU64::new(0);
/// Перерисовки по короткому пути: строки клиента — одной копией, без
/// прохода по точкам (P8).
pub static WL_DIRECT: AtomicU64 = AtomicU64::new(0);
/// Кадры, показанные без копии (D4, 20.11.2026): пиксели окна — сам
/// dma-buf клиента; и сколько нс ушло на подмену с переносом в окно.
pub static WL_ZERO_COPY: AtomicU64 = AtomicU64::new(0);
pub static WL_ZERO_COPY_NS: AtomicU64 = AtomicU64::new(0);
/// Слои с масштабом `wp_viewport` (D4).
pub static WL_SCALED: AtomicU64 = AtomicU64::new(0);
/// Кадры без темпа (`MAILBOX`/`IMMEDIATE`), пришедшие раньше кадра монитора
/// (D7, 21.11.2026): их показ отложен до срока, и показан будет последний.
pub static WL_COALESCED: AtomicU64 = AtomicU64::new(0);
/// Самая быстрая копия за окно, нс на мегаточку (19.11.2026): среднее — это
/// время по часам, и на загруженной машине в него входит вытеснение задачи
/// Wayland посреди копии (очередь из пяти-шести готовых потоков llvmpipe по
/// 10 мс кванта); лучшая — сколько стоит сама копия.
pub static WL_PAINT_BEST: AtomicU64 = AtomicU64::new(u64::MAX);

/// Включить или выключить отчёт.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Напечатать отчёт за то, что накопилось, сейчас.
pub fn request_now() {
    FORCE.store(true, Ordering::Relaxed);
    ensure_started();
}

/// Поднять службу отчёта (первый вызов Linux). Дальше — одна загрузка флага.
#[inline]
pub fn ensure_started() {
    if STARTED.load(Ordering::Relaxed) || STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if crate::sched::spawn_daemon("perf-diag", run).is_err() {
        STARTED.store(false, Ordering::Release);
    }
}

/// Значения счётчиков на начало окна.
struct Mark {
    at_ms: u64,
    calls: Vec<(u64, u64)>,
    /// `(просьб, ожидание нс, худшее нс, без адресатов, адресатов)`.
    shootdowns: (u64, u64, u64, u64, u64),
    kicks: (u64, u64, u64),
    sched_lock: (u64, u64),
    mutex_spins: u64,
    futex: [u64; 4],
    deferred: (u64, u64),
    switches: u64,
    idle_ticks: u64,
    wayland: [u64; 11],
    desktop: (u64, u64, u64, u64, u64),
    flush_ns: u64,
    outputs: [(u64, u64); crate::ui::OUTPUT_STATS],
    /// G2: `(wait-queue sleeps, wakes by key, idle rounds without the lock,
    /// zeroed frames from the caches, cache refills)`.
    g2: [u64; 5],
}

impl Mark {
    fn take() -> Self {
        let mut calls = vec![(0u64, 0u64); profile::COUNT];
        profile::snapshot(&mut calls);
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        Self {
            at_ms: crate::time::uptime_ms(),
            calls,
            shootdowns: crate::arch::smp::shootdown_stats(),
            sched_lock: crate::sched::lock_wait_stats(),
            mutex_spins: crate::sync::MUTEX_SPIN_WINS.load(Ordering::Relaxed),
            kicks: {
                let (sent, taken) = crate::arch::smp::kick_stats();
                (sent, taken, crate::sched::poll_kicks())
            },
            futex: [load(&FUTEX_WAITS), load(&FUTEX_WAKES), load(&FUTEX_WOKEN), load(&FUTEXV_WAITS)],
            deferred: crate::sched::deferred_wake_stats(),
            switches: crate::sched::switch_count(),
            idle_ticks: crate::sched::idle_ticks_total(),
            wayland: [
                load(&WL_COMMITS),
                load(&WL_PAINTS),
                load(&WL_PAINT_NS),
                load(&WL_PAINT_PX),
                load(&WL_FRAMES),
                load(&WL_BUSY),
                load(&WL_DIRECT),
                load(&WL_ZERO_COPY),
                load(&WL_ZERO_COPY_NS),
                load(&WL_SCALED),
                load(&WL_COALESCED),
            ],
            desktop: crate::ui::timing(),
            flush_ns: crate::ui::flush_ns(),
            outputs: core::array::from_fn(crate::ui::output_blit),
            g2: {
                let (sleeps, wakes) = crate::sched::wait_queue_stats();
                [
                    sleeps,
                    wakes,
                    crate::sched::idle_skips(),
                    crate::mm::frame::ZEROED_HITS.load(Ordering::Relaxed),
                    crate::mm::frame::ZEROED_REFILLS.load(Ordering::Relaxed),
                ]
            },
        }
    }
}

/// Выборка очереди за окно: сумма готовых, худшее, сумма исполняющихся,
/// число выборок.
#[derive(Default)]
struct Queue {
    ready: u64,
    worst: u64,
    running: u64,
    samples: u64,
}

fn run() {
    let mut mark = Mark::take();
    let mut queue = Queue::default();
    loop {
        crate::sched::sleep_ms(SAMPLE_MS);
        if !ENABLED.load(Ordering::Relaxed) && !FORCE.load(Ordering::Relaxed) {
            // Выключено — окно начинается заново при включении.
            mark = Mark::take();
            queue = Queue::default();
            continue;
        }
        let (ready, running) = crate::sched::queue_facts();
        queue.ready += ready as u64;
        queue.worst = queue.worst.max(ready as u64);
        queue.running += running as u64;
        queue.samples += 1;
        let now = crate::time::uptime_ms();
        let forced = FORCE.swap(false, Ordering::Relaxed);
        if !forced && now.saturating_sub(mark.at_ms) < PERIOD_MS {
            continue;
        }
        let next = Mark::take();
        report(&mark, &next, &queue, forced);
        mark = next;
        queue = Queue::default();
    }
}

/// Миллисекунды с одной цифрой после запятой.
fn ms(ns: u64) -> String {
    let tenths = ns / 100_000;
    alloc::format!("{}.{}", tenths / 10, tenths % 10)
}

fn report(old: &Mark, new: &Mark, queue: &Queue, forced: bool) {
    let span_ms = new.at_ms.saturating_sub(old.at_ms).max(1);
    let secs = span_ms / 1000;
    // Разности вызовов: (номер, раз, наносекунд).
    let mut rows: Vec<(usize, u64, u64)> = new
        .calls
        .iter()
        .zip(&old.calls)
        .enumerate()
        .filter_map(|(number, (now, was))| {
            let calls = now.0.saturating_sub(was.0);
            (calls > 0).then(|| (number, calls, now.1.saturating_sub(was.1)))
        })
        .collect();
    let syscalls: u64 = rows.iter().filter(|row| row.0 < 500).map(|row| row.1).sum();
    // Ни одного вызова Linux за окно — программ Linux нет или они спят
    // целиком: печатать нечего (кроме `perfdiag now`).
    if syscalls == 0 && !forced {
        return;
    }
    let name = |number: usize| -> String {
        let known = profile::name(number);
        if known.is_empty() { alloc::format!("#{number}") } else { String::from(known) }
    };

    // 1. Вызовы: по времени и по числу.
    let mut line = String::new();
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    let _ = write!(line, "{secs} s, {syscalls} syscalls; by time:");
    for (number, calls, nanos) in rows.iter().filter(|row| row.0 < 500).take(TOP) {
        let _ = write!(line, " {} {calls}x {} ms,", name(*number), ms(*nanos));
    }
    line.pop();
    kprintln!("  perf-diag   : {line}");
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let mut line = String::new();
    let _ = write!(line, "{secs} s by count:");
    for (number, calls, nanos) in rows.iter().filter(|row| row.0 < 500).take(TOP) {
        let per = if *calls == 0 { 0 } else { nanos / calls / 1000 };
        let _ = write!(line, " {} {calls} ({per} us),", name(*number));
    }
    line.pop();
    kprintln!("  perf-diag   : {line}");

    // 2. Память и синхронизация.
    let mut line = String::new();
    let _ = write!(line, "{secs} s faults:");
    let mut any = false;
    for (number, calls, nanos) in rows.iter().filter(|row| (500..=506).contains(&row.0)) {
        let kind = name(*number);
        let kind = kind.strip_prefix("#pf ").unwrap_or(&kind);
        let _ = write!(line, " {kind} {calls} ({} ms),", ms(*nanos));
        any = true;
    }
    if !any {
        line.push_str(" none,");
    }
    let shoot = new.shootdowns.0.saturating_sub(old.shootdowns.0);
    let shoot_ns = new.shootdowns.1.saturating_sub(old.shootdowns.1);
    // Худшее — за окно (счётчик забывается при каждом снимке); без адресатов
    // — сбросы, которым прерывания не понадобились вовсе (19.11).
    let shoot_worst = new.shootdowns.2;
    let shoot_local = new.shootdowns.3.saturating_sub(old.shootdowns.3);
    let shoot_cpus = new.shootdowns.4.saturating_sub(old.shootdowns.4);
    let kicks = (
        new.kicks.0.saturating_sub(old.kicks.0),
        new.kicks.1.saturating_sub(old.kicks.1),
        new.kicks.2.saturating_sub(old.kicks.2),
    );
    let futex: [u64; 4] = core::array::from_fn(|i| new.futex[i].saturating_sub(old.futex[i]));
    let deferred = (new.deferred.0.saturating_sub(old.deferred.0), new.deferred.1.saturating_sub(old.deferred.1));
    let _ = write!(
        line,
        " tlb shootdowns {shoot} to {shoot_cpus} cpus (wait {} ms, {} us each, worst {} us), {shoot_local} needed no IPI; kick IPIs {} sent {} taken, {} by idle poll; futex wait {} wake {} woke {} waitv {}; deferred wakes {} (overflow {}); sched lock waited {}x {} ms; mutex taken spinning {}",
        ms(shoot_ns),
        shoot_ns / shoot.max(1) / 1000,
        shoot_worst / 1000,
        kicks.0,
        kicks.1,
        kicks.2,
        futex[0],
        futex[1],
        futex[2],
        futex[3],
        deferred.0,
        deferred.1,
        new.sched_lock.0.saturating_sub(old.sched_lock.0),
        ms(new.sched_lock.1.saturating_sub(old.sched_lock.1)),
        new.mutex_spins.saturating_sub(old.mutex_spins),
    );
    // Кто ждал замок планировщика дольше всех (20.11).
    for (site, count, ns) in crate::sched::take_lock_wait_sites() {
        let _ = write!(line, "; {site} {count}x {} ms", ms(ns));
    }
    kprintln!("  perf-diag   : {line}");
    // G2: the worst shootdown - who asked, which CPU answered late and the
    // spin lock that kept it deaf; wait queues, idle rounds, zeroed frames.
    let (asked_at, slow_cpu, deaf_at, slow_total) = crate::arch::smp::shootdown_worst();
    let g2: [u64; 5] = core::array::from_fn(|i| new.g2[i].saturating_sub(old.g2[i]));
    let mut line = String::new();
    let _ = write!(line, "{secs} s tlb worst {} us asked at {asked_at}", shoot_worst / 1000);
    match (slow_cpu, deaf_at) {
        (Some(cpu), Some(at)) => {
            let _ = write!(line, ", cpu {cpu} late (interrupts off in a spin lock taken at {at})");
        }
        (Some(cpu), None) => {
            let _ = write!(line, ", cpu {cpu} late (not in a spin lock: interrupt handler or switch)");
        }
        _ => {}
    }
    let _ = write!(
        line,
        ", {slow_total} slow (>0.5 ms) since boot; wait queues {} sleeps {} woken by key; idle rounds without the lock {}; zeroed frames {} from cpu caches, {} refills",
        g2[0], g2[1], g2[2], g2[3], g2[4]
    );
    kprintln!("  perf-diag   : {line}");

    // 3. Процессоры и очередь.
    let cpus = crate::smp::online().max(1) as u64;
    let ticks = span_ms * u64::from(crate::irq::TIMER_HZ) / 1000 * cpus;
    let idle = new.idle_ticks.saturating_sub(old.idle_ticks);
    let idle_pct = if ticks == 0 { 0 } else { (idle * 100 / ticks).min(100) };
    let samples = queue.samples.max(1);
    let avg = |sum: u64| alloc::format!("{}.{}", sum / samples, sum * 10 / samples % 10);
    kprintln!(
        "  perf-diag   : {secs} s cpu: idle {idle_pct}% of {cpus} cpus, run queue {} ready on average (worst {}), {} running; {} switches",
        avg(queue.ready),
        queue.worst,
        avg(queue.running),
        new.switches.saturating_sub(old.switches),
    );

    // 4. Кадры: сервер Wayland (копия буфера клиента) и стол (сборка, вывод).
    let wl: [u64; 11] = core::array::from_fn(|i| new.wayland[i].saturating_sub(old.wayland[i]));
    let paints = wl[1].max(1);
    let (frames, _bands, draw, blit, points) = (
        new.desktop.0.saturating_sub(old.desktop.0),
        new.desktop.1.saturating_sub(old.desktop.1),
        new.desktop.2.saturating_sub(old.desktop.2),
        new.desktop.3.saturating_sub(old.desktop.3),
        new.desktop.4.saturating_sub(old.desktop.4),
    );
    let flush = new.flush_ns.saturating_sub(old.flush_ns);
    let per_frame = frames.max(1);
    let rate = |count: u64| alloc::format!("{}.{}", count * 1000 / span_ms, count * 10_000 / span_ms % 10);
    let mut line = String::new();
    let _ = write!(
        line,
        "{secs} s frames: wayland {} commits ({}/s), {} paints ({} direct), {} zero-copy ({} ms each), {} scaled, {} unpaced held, copy {} ms/paint ({} Kpx, {} MB/s, best {} MB/s), {} frame callbacks, {} busy; desktop {frames} frames ({}/s): compose {} ms, blit {} ms, flush {} ms per frame, {} Kpx/frame, worst {} ms",
        wl[0],
        rate(wl[0]),
        wl[1],
        wl[6],
        wl[7],
        ms(wl[8] / wl[7].max(1)),
        wl[9],
        wl[10],
        ms(wl[2] / paints),
        wl[3] / paints / 1000,
        // Байт за микросекунду — это и есть МБ/с (по 4 байта на точку).
        if wl[2] == 0 { 0 } else { wl[3] * 4 * 1000 / wl[2] },
        // нс на мегаточку → МБ/с: 4 МБ за столько нс.
        match WL_PAINT_BEST.swap(u64::MAX, Ordering::Relaxed) {
            u64::MAX | 0 => 0,
            best => 4_000_000_000 / best,
        },
        wl[4],
        wl[5],
        rate(frames),
        ms(draw / per_frame),
        ms(blit / per_frame),
        ms(flush / per_frame),
        points / per_frame / 1000,
        crate::ui::take_worst_frame_ns() / 1_000_000,
    );
    for (index, (now, was)) in new.outputs.iter().zip(&old.outputs).enumerate() {
        let (ns, px) = (now.0.saturating_sub(was.0), now.1.saturating_sub(was.1));
        if px == 0 {
            continue;
        }
        // Мегаточек в секунду вывода — сравнимо между мониторами разного
        // размера и между железом и QEMU.
        let mpx_s = if ns == 0 { 0 } else { px * 1000 / ns };
        let _ = write!(line, "; out{index} {} ms/frame ({mpx_s} Mpx/s)", ms(ns / per_frame));
    }
    kprintln!("  perf-diag   : {line}");
}

/// Строка состояния для команды `perfdiag`.
#[must_use]
pub fn status() -> String {
    alloc::format!(
        "perf-diag is {}: every {} s while Linux programs make system calls",
        if enabled() { "on" } else { "off" },
        PERIOD_MS / 1000
    )
}
