//! Остальные процессоры на AArch64: `PSCI CPU_ON` и вход с выключенным MMU.
//!
//! # Как просыпается процессор
//!
//! Проще, чем на x86-64, и по той же причине, по которой проще выключение: у ARM
//! нет чипсета, которому пишут в регистр, — есть прошивка, которую просят.
//! `CPU_ON` получает три числа: кого будить (`MPIDR`), с какого **физического**
//! адреса начать и что положить в `x0`. Процессор просыпается в EL1 с
//! выключенным MMU, замаскированными прерываниями и неизвестным всем
//! остальным.
//!
//! «Выключенный MMU» и определяет устройство входа. Первые инструкции исполняются
//! по физическим адресам, поэтому вход — ассемблер, не трогающий ничего, кроме
//! регистров и одной записи ([`Record`]), в которой загрузочный процессор оставил
//! значения своих системных регистров трансляции. Включив MMU теми же
//! значениями, процессор продолжает по тем же адресам: ядро отображено на себя
//! (виртуальный адрес образа равен физическому), и следующая инструкция после
//! `sctlr_el1` выбирается уже через таблицы — но из того же места.
//!
//! # Номер процессора — в `TPIDR_EL1`
//!
//! Регистр для этого и существует: программа из EL0 его не видит и не пишет, а
//! ядро пишет туда своё. Записывается он первой же строкой на Rust, до любого
//! лока, — и на загрузочном тоже, до того, как появится второй процессор, потому
//! что прошивка вправе оставить там что угодно.

use core::arch::{asm, global_asm};
use core::mem::offset_of;

use super::acpi::MPIDR_AFFINITY;
use super::{gic, paging, power};
use crate::kprintln;
use crate::mm::{PageFlags, PhysAddr, VirtAddr};
use crate::smp::Found;
use crate::sync::Racy;

/// Как архитектура называет аппаратный номер процессора — для журнала.
pub const ID_NAME: &str = "mpidr";

/// SGI, которым процессоры просят друг друга остановиться.
pub const IPI_STOP: u32 = 0;

/// Номер текущего процессора — из `TPIDR_EL1`.
#[inline]
#[must_use]
pub fn current_index() -> usize {
    let value: u64;
    // SAFETY: чтение системного регистра без побочных эффектов.
    unsafe { asm!("mrs {}, tpidr_el1", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value as usize
}

/// `MPIDR` текущего процессора, только биты сродства.
fn own_mpidr() -> u64 {
    let value: u64;
    // SAFETY: чтение системного регистра без побочных эффектов.
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value & MPIDR_AFFINITY
}

/// Сродство в раскладке `GICR_TYPER`: Aff3.Aff2.Aff1.Aff0 подряд.
const fn affinity(mpidr: u64) -> u32 {
    ((((mpidr >> 32) & 0xFF) << 24) | (mpidr & 0xFF_FFFF)) as u32
}

/// Найти процессоры в MADT.
#[must_use]
pub fn discover() -> Found {
    // Без ACPI (телефон, 11.11) — процессоры и способ вызова PSCI из дерева
    // устройств: `/psci` (`method`) и `/cpus/cpu@*` (`reg`, `enable-method`).
    if crate::acpi::rsdp() == 0
        && let Some(found) = super::cpus_from_tree(own_mpidr())
    {
        return found;
    }
    // SAFETY: адрес RSDP запомнен при разборе хэндоффа; ноль означает «таблиц
    // нет» и обрабатывается внутри.
    unsafe { super::acpi::processors(crate::acpi::rsdp(), own_mpidr()) }
}

/// То, что загрузочный процессор оставляет проснувшемуся.
///
/// `repr(C)` и все поля по восемь байт: читает запись ассемблер по смещениям,
/// посчитанным из этой же структуры.
#[repr(C, align(64))]
struct Record {
    mair: u64,
    tcr: u64,
    ttbr0: u64,
    ttbr1: u64,
    sctlr: u64,
    /// Вершина стека холостой задачи процессора.
    stack: u64,
    /// Куда перейти, включив MMU.
    entry: u64,
    /// Номер процессора — аргумент точки входа.
    index: u64,
    /// Докуда дошёл проснувшийся (11.11, телефон): 1 — вошёл, 2 — MMU
    /// включён, 3 — Rust, 4 — прерывания настроены. Смещение 64 — своя
    /// строка кэша (запись выровнена на 64): первую отметку он пишет мимо
    /// кэша, и чужая грязная строка не должна её затереть.
    stage: u64,
    /// Куда проснувшийся прыгнул из ассемблера (диагностика).
    jumped: u64,
}

/// Одна запись на всех: процессоры запускаются по одному, и прежний к моменту
/// запуска следующего свою уже прочитал.
static RECORD: Racy<Record> = Racy::new(Record {
    mair: 0,
    tcr: 0,
    ttbr0: 0,
    ttbr1: 0,
    sctlr: 0,
    stack: 0,
    entry: 0,
    index: 0,
    stage: 0,
    jumped: 0,
});

/// Куда прыгнул последний будимый процессор (диагностика).
pub fn last_jump() -> u64 {
    let record = RECORD.get();
    clean_to_memory(record as usize, core::mem::size_of::<Record>());
    // SAFETY: чтение поля записи.
    unsafe { core::ptr::read_volatile(&raw const (*record).jumped) }
}

/// Этап, до которого дошёл последний будимый процессор (см. `Record::stage`).
pub fn last_stage() -> u64 {
    let record = RECORD.get();
    clean_to_memory(record as usize, core::mem::size_of::<Record>());
    // SAFETY: чтение поля записи; барьер выше.
    unsafe { core::ptr::read_volatile(&raw const (*record).stage) }
}

global_asm!(
    r#"
.section .text.smp_entry, "ax", %progbits
.balign 16
.globl aarch64_secondary_entry
.hidden aarch64_secondary_entry
aarch64_secondary_entry:
    // MMU выключен, адреса физические; x0 — запись загрузочного процессора.
    msr     daifset, #0xf
    mov     x19, x0
    mov     x1, #1
    str     x1, [x19, #{stage}]
    dsb     sy
    // Телефон (11.11): ATF будит остальные процессоры на EL2, хотя ядро живёт
    // на EL1 — записи в регистры EL1 тогда ничего не значат, MMU не
    // включается, и первый же доступ к верхней половине падает. Спуститься
    // на EL1, как `head_fdt.S` делает на загрузочном.
    mrs     x1, CurrentEL
    lsr     x1, x1, #2
    cmp     x1, #2
    b.ne    1f
    mrs     x1, cnthctl_el2
    orr     x1, x1, #3
    msr     cnthctl_el2, x1
    msr     cntvoff_el2, xzr
    movz    x1, #0x8000, lsl #16
    msr     hcr_el2, x1
    movz    x1, #0x33ff
    msr     cptr_el2, x1
    msr     hstr_el2, xzr
    movz    x1, #0x0800
    movk    x1, #0x30d0, lsl #16
    msr     sctlr_el1, x1
    movz    x1, #0x3c5
    msr     spsr_el2, x1
    adr     x1, 1f
    msr     elr_el2, x1
    eret
1:
    // Тот же порядок, что при активации таблиц на загрузочном: сначала вся
    // геометрия трансляции, один `isb`, и только потом включение MMU.
    ldr     x1, [x19, #{mair}]
    msr     mair_el1, x1
    ldr     x1, [x19, #{tcr}]
    msr     tcr_el1, x1
    ldr     x1, [x19, #{ttbr0}]
    msr     ttbr0_el1, x1
    ldr     x1, [x19, #{ttbr1}]
    msr     ttbr1_el1, x1
    isb
    tlbi    vmalle1
    ldr     x1, [x19, #{sctlr}]
    msr     sctlr_el1, x1
    isb
    mov     x1, #2
    str     x1, [x19, #{stage}]
    // MMU включён; ядро отображено на себя, поэтому x19 и счётчик команд
    // по-прежнему верны. Стек — холостой задачи, в верхней половине.
    // FP/SIMD — сразу (11.11, телефон): ATF будит ядро с CPACR_EL1 = 0, и
    // первая же команда SIMD в коде на Rust ловушкой уходила в никуда —
    // векторов исключений у проснувшегося ещё нет.
    mov     x1, #(3 << 20)
    msr     cpacr_el1, x1
    isb
    msr     spsel, #1
    ldr     x1, [x19, #{stack}]
    mov     sp, x1
    mov     x1, #5
    str     x1, [x19, #{stage}]
    ldr     x0, [x19, #{index}]
    ldr     x2, [x19, #{entry}]
    str     x2, [x19, #{jumped}]
    mov     x1, #6
    str     x1, [x19, #{stage}]
    dsb     sy
    mov     x29, xzr
    mov     x30, xzr
    br      x2
"#,
    mair = const offset_of!(Record, mair),
    tcr = const offset_of!(Record, tcr),
    ttbr0 = const offset_of!(Record, ttbr0),
    ttbr1 = const offset_of!(Record, ttbr1),
    sctlr = const offset_of!(Record, sctlr),
    stack = const offset_of!(Record, stack),
    entry = const offset_of!(Record, entry),
    index = const offset_of!(Record, index),
    stage = const offset_of!(Record, stage),
    jumped = const offset_of!(Record, jumped),
);

unsafe extern "C" {
    fn aarch64_secondary_entry() -> !;
}

/// Подготовить запуск: убедиться, что есть кого просить, и записать значения
/// регистров трансляции.
///
/// # Ошибки
///
/// Строка о причине, по которой остальные процессоры не будут запущены.
pub fn prepare() -> Result<(), &'static str> {
    // SAFETY: адрес RSDP запомнен при разборе хэндоффа; ноль обрабатывается
    // внутри.
    if !unsafe { power::psci_available(crate::acpi::rsdp()) } {
        return Err("the firmware does not advertise PSCI");
    }

    let (ttbr0, ttbr1) = paging::kernel_roots();
    let (mair, tcr, sctlr): (u64, u64, u64);
    // SAFETY: запись `TPIDR_EL1` меняет только то, что читает `current_index`, и
    // до этой строки его не читает никто; чтения регистров трансляции побочных
    // эффектов не имеют.
    unsafe {
        asm!("msr tpidr_el1, xzr", options(nomem, nostack, preserves_flags));
        asm!("mrs {}, mair_el1", out(reg) mair, options(nomem, nostack, preserves_flags));
        asm!("mrs {}, tcr_el1", out(reg) tcr, options(nomem, nostack, preserves_flags));
        asm!("mrs {}, sctlr_el1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
    }

    let record = RECORD.get();
    // SAFETY: запись принадлежит только запуску процессоров, а запуск ещё не
    // начался.
    unsafe {
        (*record).mair = mair;
        (*record).tcr = tcr;
        // Корни ядра, а не содержимое `TTBR0_EL1`: там мог бы стоять корень
        // программы. Сейчас программ ещё нет, но полагаться на это незачем.
        (*record).ttbr0 = ttbr0.as_u64();
        (*record).ttbr1 = ttbr1.as_u64();
        (*record).sctlr = sctlr;
    }
    Ok(())
}

/// Разбудить процессор `index` с `MPIDR` `id` на стеке `stack_top`.
///
/// `extra` — адрес его redistributor'а из MADT, если прошивка его назвала.
pub fn start_cpu(index: usize, id: u64, extra: u64, stack_top: usize) -> bool {
    if gic::version() == Some(gic::Version::V3) {
        // Без своего redistributor'а процессор не получит ни таймера, ни просьбы
        // остановиться: такой процессор хуже, чем спящий.
        let found = if extra != 0 {
            Some(extra as usize)
        } else {
            gic::find_redistributor(affinity(id))
        };
        let Some(address) = found else {
            kprintln!("  smp         : cpu {index}: no GICv3 redistributor for mpidr {id:#x}");
            return false;
        };
        // Окно из записи процессора может лежать вне диапазона, отображённого при
        // запуске; внутри диапазона повторное отображение тем же кадром безвредно.
        //
        // SAFETY: таблицы ядра активны; адрес — регистры контроллера, объявленные
        // прошивкой, и Device-семантика для них обязательна.
        let mapped = unsafe {
            crate::arch::map_active(
                VirtAddr::new(address),
                PhysAddr::new(address as u64),
                gic::redistributor_size(),
                PageFlags::READ.union(PageFlags::WRITE).union(PageFlags::DEVICE),
            )
        };
        if let Err(err) = mapped {
            kprintln!("  smp         : cpu {index}: cannot map redistributor {address:#x}: {err:?}");
            return false;
        }
        gic::set_redistributor(index, address);
    }

    let record = RECORD.get();
    // SAFETY: процессоры запускаются по одному, и прежний свою запись уже
    // прочитал: он сообщил о себе, прежде чем мы дошли сюда.
    unsafe {
        (*record).stage = 0;
        (*record).stack = stack_top as u64;
        (*record).entry = secondary_entry as *const () as usize as u64;
        (*record).index = index as u64;
    }

    // Адреса физические, и они совпадают с виртуальными: образ ядра отображён
    // на себя.
    let entry = aarch64_secondary_entry as *const () as usize as u64;
    // Проснувшийся читает запись и код входа с выключенным MMU, то есть мимо
    // кэша (11.11, телефон: настоящий Cortex-A53, не QEMU) — вытолкнуть их в
    // память.
    clean_to_memory(record as usize, core::mem::size_of::<Record>());
    clean_to_memory(entry as usize, 0x100);
    // SAFETY: барьеры без побочных эффектов на данные.
    unsafe { asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
    // SAFETY: номер функции и соглашение — из PSCI и FADT; адрес входа — код
    // ядра, запись — данные ядра, и то и другое по физическим адресам.
    match unsafe { power::cpu_on(crate::acpi::rsdp(), id, entry, record as u64) } {
        Ok(()) => true,
        Err(code) => {
            kprintln!("  smp         : cpu {index}: PSCI CPU_ON refused with {code}");
            false
        }
    }
}

/// Вытолкнуть строки кэша диапазона в память (`dc civac`).
fn clean_to_memory(start: usize, len: usize) {
    let mut line = start & !63;
    while line < start + len {
        // SAFETY: адрес — в образе ядра, отображённом на себя.
        unsafe { asm!("dc civac, {}", in(reg) line, options(nostack, preserves_flags)) };
        line += 64;
    }
}

/// Первая функция на Rust, которую исполняет проснувшийся процессор.
extern "C" fn secondary_entry(index: usize) -> ! {
    // SAFETY: запись `TPIDR_EL1` — первое, что делает процессор, до любого лока
    // (см. заголовок модуля).
    unsafe { asm!("msr tpidr_el1, {}", in(reg) index, options(nomem, nostack, preserves_flags)) };
    let record = RECORD.get();
    // SAFETY: запись этапа; запись принадлежит запуску этого процессора.
    unsafe { core::ptr::write_volatile(&raw mut (*record).stage, 3) };
    super::interrupts::init_secondary();
    // SAFETY: см. выше.
    unsafe { core::ptr::write_volatile(&raw mut (*record).stage, 4) };
    super::fpu::enable_access();
    crate::smp::secondary_main(index)
}

/// Попросить остановиться все процессоры, кроме текущего.
pub fn stop_others() {
    // SAFETY: контроллер опознан и включён загрузочным процессором ещё до того,
    // как появились остальные; номер SGI меньше шестнадцати.
    unsafe { gic::send_sgi_to_others(IPI_STOP) };
}
