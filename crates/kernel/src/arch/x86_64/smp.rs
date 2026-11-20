//! Остальные процессоры на x86-64: трамплин реального режима и INIT–SIPI–SIPI.
//!
//! # Почему процессор просыпается в 1978 году
//!
//! Процессор, которому послали `INIT`, сбрасывается и ждёт `SIPI` —
//! межпроцессорного прерывания с восьмибитным номером страницы. Получив его, он
//! начинает исполнять код **в реальном режиме** с адреса `номер × 0x1000`: без
//! защиты, без страниц, шестнадцатиразрядными регистрами и в первом мегабайте
//! памяти. Из длинного режима загрузочного процессора он не унаследовал ничего.
//!
//! Отсюда трамплин: страница ниже 640 КиБ, в которую ядро копирует код,
//! проводящий процессор через защищённый режим в длинный — ровно тот путь,
//! который когда-то прошла прошивка на загрузочном. Страницу откладывает
//! распределитель кадров при запуске (см. `mm::frame`), потому что к этому
//! моменту всё остальное в первом мегабайте уже занято таблицами страниц.
//!
//! # Что трамплин берёт у загрузочного процессора
//!
//! `CR0`, `CR3`, `CR4` и `EFER` — готовыми значениями, прочитанными на
//! загрузочном, а не собранными заново. Собирать их второй раз значило бы
//! держать вторую копию решений, разбросанных по ядру (`WP` из подкачки,
//! `OSFXSR` и `OSXSAVE` из векторных расширений, `NXE` из разметки страниц), и
//! однажды забыть в ней бит — а забытый `NXE` превращает первое же обращение к
//! странице с запретом исполнения в отказ с «зарезервированным битом», то есть
//! в загадку.
//!
//! Одно ограничение названо вслух: `CR3` загружается из тридцатидвухразрядного
//! режима, поэтому корень таблиц ядра обязан лежать ниже 4 ГиБ. Он там и лежит —
//! таблицы строятся первыми, из младших кадров, — но проверяется это, а не
//! предполагается.
//!
//! # Почему ассемблер в синтаксисе AT&T
//!
//! Только ради шестнадцатиразрядной части. У `lgdt` и дальнего возврата в
//! реальном режиме размер операнда задаётся суффиксом (`lgdtl`, `lretl`), и в
//! синтаксисе Intel у LLVM нет однозначной записи для «тридцатидвухразрядный
//! операнд в шестнадцатиразрядном коде». Ошибка здесь не даёт сообщения — она
//! даёт процессор, ушедший по адресу, у которого обрезано старшее слово.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{apic, gdt, paging, rdmsr};
use crate::mm::PhysAddr;
use crate::smp::Found;

/// Как архитектура называет аппаратный номер процессора — для журнала.
pub const ID_NAME: &str = "apic id";

/// Номер текущего процессора. См. [`gdt::current_cpu`].
#[inline]
#[must_use]
pub fn current_index() -> usize {
    gdt::current_cpu()
}

// --- Поиск процессоров ----------------------------------------------------------

/// Запись MADT «Processor Local APIC» (ACPI 6.5, 5.2.12.2).
const MADT_ENTRY_LOCAL_APIC: u8 = 0;
/// Запись MADT «Processor Local x2APIC» (5.2.12.12): номера шире байта.
const MADT_ENTRY_X2APIC: u8 = 9;
/// Флаг записи: процессор включён. Без него запись описывает гнездо, в котором
/// процессора нет, или процессор, который прошивка просит не трогать.
const MADT_CPU_ENABLED: u32 = 1 << 0;

/// Найти процессоры в MADT.
///
/// Загрузочный процессор узнаётся по номеру своего локального APIC, а не по
/// первой записи: порядок записей спецификация не закрепляет.
#[must_use]
pub fn discover() -> Found {
    let boot = u64::from(apic::local_id());
    let mut found = Found::only(boot);

    // SAFETY: адрес RSDP запомнен при разборе хэндоффа и указывает на таблицы
    // в памяти, которую ядро не переиспользует; ноль означает «таблиц нет».
    let Ok(madt) = (unsafe { crate::acpi::find_table(crate::acpi::rsdp(), b"APIC") }) else {
        return found;
    };

    // После заголовка — адрес локального APIC и флаги, восемь байт.
    let mut at = crate::acpi::SDT_HEADER_LEN + 8;
    while at + 2 <= madt.len() {
        let kind = madt[at];
        let len = usize::from(madt[at + 1]);
        // Нулевая длина — испорченная таблица: без проверки обход зациклился
        // бы навсегда.
        if len < 2 || at + len > madt.len() {
            break;
        }
        let entry = match kind {
            MADT_ENTRY_LOCAL_APIC if len >= 8 => {
                Some((u64::from(madt[at + 3]), crate::acpi::read_u32(madt, at + 4)))
            }
            MADT_ENTRY_X2APIC if len >= 16 => Some((
                u64::from(crate::acpi::read_u32(madt, at + 4)),
                crate::acpi::read_u32(madt, at + 8),
            )),
            _ => None,
        };
        if let Some((id, flags)) = entry {
            if flags & MADT_CPU_ENABLED != 0 {
                found.push(id, 0);
            }
        }
        at += len;
    }
    found
}

// --- Трамплин -------------------------------------------------------------------

/// Вершина стека трамплина внутри его страницы.
///
/// Стек нужен дальнему возврату в шестнадцатиразрядной и тридцатидвухразрядной
/// частях — по восемь байт каждой. Код и данные трамплина обязаны
/// заканчиваться ниже, что и проверяет [`prepare`].
const TRAMPOLINE_STACK: usize = 0x0FF0;

global_asm!(
    r#"
    .pushsection .rodata.smp_trampoline, "a"
    .balign 16
    .globl smp_trampoline_start
    .hidden smp_trampoline_start
smp_trampoline_start:
    .code16
    // Реальный режим: CS указывает на страницу трамплина, IP = 0. Сегменты
    // данных и стека берутся оттуда же — тогда адрес внутри страницы и есть
    // смещение от её начала.
    cli
    cld
    movw    %cs, %ax
    movw    %ax, %ds
    movw    %ax, %es
    movw    %ax, %ss
    movw    $0x0ff0, %sp
    // Базовый адрес GDT — тридцатидвухразрядный: суффикс `l` добавляет
    // префикс размера операнда. Без него `lgdt` в реальном режиме берёт
    // двадцать четыре бита и отрезает старший байт.
    lgdtl   (smp_trampoline_gdt_pointer - smp_trampoline_start)
    movl    %cr0, %eax
    orl     $1, %eax
    movl    %eax, %cr0
    // Защищённый режим, но CS пока шестнадцатиразрядный. Дальний возврат на
    // тридцатидвухразрядный сегмент кода: адрес трамплина известен только во
    // время работы, поэтому переход собирается на стеке, а не пишется числом.
    movl    (smp_trampoline_base - smp_trampoline_start), %eax
    addl    $(smp_trampoline_32 - smp_trampoline_start), %eax
    pushl   $0x18
    pushl   %eax
    lretl

    .code32
smp_trampoline_32:
    movw    $0x10, %dx
    movw    %dx, %ds
    movw    %dx, %es
    movw    %dx, %ss
    // EAX всё ещё хранит адрес этой метки — из него получается база страницы.
    movl    %eax, %ebx
    subl    $(smp_trampoline_32 - smp_trampoline_start), %ebx
    leal    0x0ff0(%ebx), %esp
    // Порядок включения длинного режима: PAE и корень таблиц, затем LME в EFER,
    // и только потом PG. Включённый PG при LME = 0 дал бы обычный 32-разрядный
    // режим с таблицами, которые построены для длинного.
    movl    (smp_trampoline_cr4 - smp_trampoline_start)(%ebx), %eax
    movl    %eax, %cr4
    movl    (smp_trampoline_cr3 - smp_trampoline_start)(%ebx), %eax
    movl    %eax, %cr3
    movl    $0xC0000080, %ecx
    movl    (smp_trampoline_efer - smp_trampoline_start)(%ebx), %eax
    movl    (smp_trampoline_efer - smp_trampoline_start + 4)(%ebx), %edx
    wrmsr
    // Кадр дальнего возврата кладётся на стек **до** включения страниц, и это
    // не порядок ради порядка. Стек лежит в странице трамплина, а в таблицах
    // ядра она отображена только на чтение и исполнение; при `CR0.WP` запись в
    // неё после включения страниц — отказ, а обработчиков у этого процессора
    // ещё нет, и отказ становится тройной ошибкой, то есть сбросом всей машины.
    // Первая версия клала кадр после — и машина перезагружалась молча, сразу за
    // заголовком «processors» в журнале.
    leal    (smp_trampoline_64 - smp_trampoline_start)(%ebx), %eax
    pushl   $0x08
    pushl   %eax
    movl    (smp_trampoline_cr0 - smp_trampoline_start)(%ebx), %eax
    movl    %eax, %cr0
    // Режим совместимости: таблицы ядра уже действуют, страница трамплина
    // отображена в них на своём же адресе. Дальний возврат на 64-разрядный
    // сегмент кода только **читает** стек и переводит процессор в длинный режим.
    lretl

    .code64
smp_trampoline_64:
    movw    $0x10, %dx
    movw    %dx, %ds
    movw    %dx, %es
    movw    %dx, %ss
    xorl    %edx, %edx
    movw    %dx, %fs
    movw    %dx, %gs
    // Старшая половина RBX после смены режима не определена.
    movl    %ebx, %ebx
    movq    (smp_trampoline_stack - smp_trampoline_start)(%rbx), %rsp
    movq    (smp_trampoline_arg - smp_trampoline_start)(%rbx), %rdi
    movq    (smp_trampoline_entry - smp_trampoline_start)(%rbx), %rax
    xorl    %ebp, %ebp
    // Фиктивный адрес возврата: точка входа — функция System V и ждёт на входе
    // RSP ≡ 8 (mod 16), а вершина стека кратна шестнадцати.
    pushq   $0
    jmpq    *%rax

    .balign 8
    .globl smp_trampoline_gdt_pointer
    .hidden smp_trampoline_gdt_pointer
smp_trampoline_gdt_pointer:
    .word   smp_trampoline_gdt_end - smp_trampoline_gdt - 1
    .long   0
    .balign 8
    .globl smp_trampoline_gdt
    .hidden smp_trampoline_gdt
smp_trampoline_gdt:
    // Бит «доступен» (Accessed) у всех трёх записей выставлен заранее: 0x9B и
    // 0x93, а не 0x9A и 0x92. Процессор, загружая сегмент, ставит этот бит
    // сам — то есть **пишет в GDT**, — а таблица лежит в странице трамплина,
    // которая после включения страниц отображена только на чтение. Дальний
    // возврат в 64-разрядный сегмент кода делал эту запись уже при
    // включённых страницах: отказ страницы, обработчиков у процессора нет,
    // тройная ошибка, и Intel сбрасывает всю машину. Найдено на ASUS K53SD
    // (Core i5 Sandy Bridge): загрузка по кругу сразу за заголовком
    // «processors». Эмулятор QEMU без ускорения при дальнем возврате этот бит
    // не пишет, и стенд с `-smp 4` проходил. У GDT ядра бит выставлен по той
    // же причине — см. `gdt::DESC_ACCESSED`.
    .quad   0
    // 0x08: код, 64 бита (L).
    .quad   0x00209B0000000000
    // 0x10: данные, плоские 4 ГиБ.
    .quad   0x00CF93000000FFFF
    // 0x18: код, 32 бита, плоский.
    .quad   0x00CF9B000000FFFF
smp_trampoline_gdt_end:
    .globl smp_trampoline_base
    .hidden smp_trampoline_base
smp_trampoline_base:
    .long   0
    .balign 8
    .globl smp_trampoline_cr0
    .hidden smp_trampoline_cr0
smp_trampoline_cr0:
    .quad   0
    .globl smp_trampoline_cr3
    .hidden smp_trampoline_cr3
smp_trampoline_cr3:
    .quad   0
    .globl smp_trampoline_cr4
    .hidden smp_trampoline_cr4
smp_trampoline_cr4:
    .quad   0
    .globl smp_trampoline_efer
    .hidden smp_trampoline_efer
smp_trampoline_efer:
    .quad   0
    .globl smp_trampoline_stack
    .hidden smp_trampoline_stack
smp_trampoline_stack:
    .quad   0
    .globl smp_trampoline_entry
    .hidden smp_trampoline_entry
smp_trampoline_entry:
    .quad   0
    .globl smp_trampoline_arg
    .hidden smp_trampoline_arg
smp_trampoline_arg:
    .quad   0
    .globl smp_trampoline_end
    .hidden smp_trampoline_end
smp_trampoline_end:
    .popsection
"#,
    options(att_syntax)
);

unsafe extern "C" {
    static smp_trampoline_start: [u8; 0];
    static smp_trampoline_end: [u8; 0];
    static smp_trampoline_gdt_pointer: [u8; 0];
    static smp_trampoline_gdt: [u8; 0];
    static smp_trampoline_base: [u8; 0];
    static smp_trampoline_cr0: [u8; 0];
    static smp_trampoline_cr3: [u8; 0];
    static smp_trampoline_cr4: [u8; 0];
    static smp_trampoline_efer: [u8; 0];
    static smp_trampoline_stack: [u8; 0];
    static smp_trampoline_entry: [u8; 0];
    static smp_trampoline_arg: [u8; 0];
}

/// Смещение символа трамплина от его начала.
fn offset(symbol: *const [u8; 0]) -> usize {
    symbol as usize - (&raw const smp_trampoline_start) as usize
}

/// Физический адрес страницы трамплина. Ноль — не подготовлена.
static TRAMPOLINE: AtomicU64 = AtomicU64::new(0);

/// `EFER`: системные вызовы через `syscall`, длинный режим, запрет исполнения.
const EFER: u32 = 0xC000_0080;
/// Биты `EFER`, которые трамплин переносит. Остальные либо только для чтения
/// (`LMA` — процессор выставит его сам), либо ядру не принадлежат.
const EFER_CARRIED: u64 = (1 << 0) | (1 << 8) | (1 << 11);
/// `CR4.PCIDE` нельзя включить вне длинного режима, а трамплин пишет `CR4` в
/// защищённом. Ядро его и не включает; бит снимается на случай, если включит
/// прошивка.
const CR4_PCIDE: u64 = 1 << 17;

/// Записать число в страницу трамплина.
///
/// # Safety
///
/// `page` — начало целой страницы, доступной на запись; `at + 8 <= PAGE_SIZE`.
unsafe fn put(page: *mut u8, at: usize, value: u64) {
    // SAFETY: контракт функции; поле может лежать невыровненным.
    unsafe { page.add(at).cast::<u64>().write_unaligned(value) };
}

/// Скопировать трамплин в отложенную страницу и сделать её исполнимой.
///
/// # Ошибки
///
/// Строка о причине, по которой остальные процессоры не будут запущены. Все
/// причины — свойства машины, а не сбои: запускать их на такой машине этим
/// способом нельзя.
pub fn prepare() -> Result<(), &'static str> {
    // Зовётся загрузочным процессором — его номер для [`kick`].
    APIC_IDS[current_index()].store(apic::local_id(), Ordering::Relaxed);
    let start = (&raw const smp_trampoline_start) as usize;
    let size = (&raw const smp_trampoline_end) as usize - start;
    if size > TRAMPOLINE_STACK - 64 {
        return Err("the start-up code does not fit its page");
    }
    let Some(frame) = crate::mm::frame::take_low_frame() else {
        return Err("no free page below 640 KiB for the start-up code");
    };
    let Some(start_root) = crate::mm::frame::take_low_root() else {
        return Err("no second free page below 640 KiB for the start-up tables");
    };
    let base = frame.as_u64();
    // Просыпается процессор на своём корне: верхняя половина ядра плюс одна
    // страница трамплина на её же адресе. В таблицах ядра нижней памяти нет —
    // ядро живёт в верхней половине, нижняя отдана программам. Первое, что
    // делает [`secondary_entry`], — уходит на корень ядра.
    // SAFETY: таблицы ядра активны; оба кадра отложены и принадлежат только нам.
    if unsafe { paging::build_start_root(start_root, frame) }.is_err() {
        return Err("the start-up page cannot be mapped");
    }
    let root = start_root.as_u64();
    let page = frame.to_direct_map().as_mut_ptr::<u8>();

    let (cr0, cr4): (u64, u64);
    // SAFETY: чтение управляющих регистров в кольце ноль побочных эффектов не
    // имеет.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
    }
    // SAFETY: `EFER` существует на всяком процессоре с длинным режимом, а ядро в
    // длинном режиме исполняется.
    let efer = unsafe { rdmsr(EFER) } & EFER_CARRIED;

    // SAFETY: кадр отложен распределителем и принадлежит только нам; прямое
    // отображение даёт его на запись; `size` проверен против размера страницы.
    unsafe {
        core::ptr::copy_nonoverlapping(start as *const u8, page, size);
        // Указатель на GDT: два байта предела, затем база — адрес таблицы в
        // этой странице.
        page.add(offset(&raw const smp_trampoline_gdt_pointer) + 2)
            .cast::<u32>()
            .write_unaligned((base as usize + offset(&raw const smp_trampoline_gdt)) as u32);
        page.add(offset(&raw const smp_trampoline_base)).cast::<u32>().write_unaligned(base as u32);
        put(page, offset(&raw const smp_trampoline_cr0), cr0);
        put(page, offset(&raw const smp_trampoline_cr3), root);
        put(page, offset(&raw const smp_trampoline_cr4), cr4 & !CR4_PCIDE);
        put(page, offset(&raw const smp_trampoline_efer), efer);
    }

    TRAMPOLINE.store(base, Ordering::Release);
    Ok(())
}

/// Сколько ждать после `INIT`, прежде чем слать `SIPI`.
///
/// Десять миллисекунд — число из спецификации MultiProcessor, и процессоры,
/// которые его не требуют, на лишнее ожидание не обижаются.
const INIT_DELAY_MS: u64 = 10;

/// Сколько ждать ответа на первый `SIPI`, прежде чем послать второй.
const SIPI_RETRY_MS: u64 = 100;

/// Разбудить процессор `index` с номером APIC `id` на стеке `stack_top`.
///
/// Возвращает `false`, если будить нечем; дождаться, что он проснулся, —
/// работа вызывающего.
pub fn start_cpu(index: usize, id: u64, _extra: u64, stack_top: usize) -> bool {
    let base = TRAMPOLINE.load(Ordering::Acquire);
    if base == 0 {
        return false;
    }
    let Ok(apic_id) = u32::try_from(id) else {
        return false;
    };
    if let Some(cell) = APIC_IDS.get(index) {
        cell.store(apic_id, Ordering::Relaxed);
    }

    gdt::prepare_secondary(index);

    let page = PhysAddr::new(base).to_direct_map().as_mut_ptr::<u8>();
    // SAFETY: страница трамплина принадлежит только нам, а процессоры
    // запускаются по одному — прежний уже не читает её поля.
    unsafe {
        put(page, offset(&raw const smp_trampoline_stack), stack_top as u64);
        put(
            page,
            offset(&raw const smp_trampoline_entry),
            secondary_entry as *const () as usize as u64,
        );
        put(page, offset(&raw const smp_trampoline_arg), index as u64);
    }

    // Номер страницы — восемь бит вектора `SIPI`; `prepare` взял её ниже 640 КиБ.
    let vector = (base >> 12) as u8;
    let before = crate::smp::online();

    apic::send_init(apic_id);
    delay_ms(INIT_DELAY_MS);
    apic::send_startup(apic_id, vector);

    // Второй `SIPI` — только если первый не разбудил. Спецификация велит слать
    // оба; процессор, уже проснувшийся от первого, второй не заметит, но и
    // слать его такому незачем.
    let deadline = crate::time::uptime_ms() + SIPI_RETRY_MS;
    while crate::smp::online() == before && crate::time::uptime_ms() < deadline {
        core::hint::spin_loop();
    }
    if crate::smp::online() == before {
        apic::send_startup(apic_id, vector);
    }
    true
}

/// Подождать, не отдавая процессор: планировщика ещё нет.
fn delay_ms(ms: u64) {
    let until = crate::time::uptime_ms() + ms;
    while crate::time::uptime_ms() < until {
        core::hint::spin_loop();
    }
}

/// Первая функция на Rust, которую исполняет проснувшийся процессор.
///
/// Порядок тот же, что у загрузочного, и по той же причине: сначала таблицы
/// дескрипторов (без них номер процессора не читается, а значит нельзя брать
/// ни одного лока, включая лок вывода), потом прерывания, потом векторные
/// расширения.
extern "sysv64" fn secondary_entry(index: usize) -> ! {
    // С корня запуска (нижняя страница трамплина плюс верхняя половина) — на
    // корень ядра: он содержит всё то же наверху, а трамплин больше не нужен.
    // SAFETY: таблицы ядра построены и активны на загрузочном процессоре.
    unsafe { paging::activate_kernel_space_early() };
    // SAFETY: загрузочный подготовил TSS этого процессора до запуска, и это
    // первый код на нём; ни одного лока до этой строки не взято.
    unsafe { gdt::load_secondary(index) };
    // Таблица типов памяти — регистр процессора, а не машины: загрузочный
    // запрограммировал свою, этот обязан сделать то же. Иначе одни и те же
    // страницы экрана окажутся быстрыми на одном ядре и некешируемыми на
    // остальных — в зависимости от того, кто рисует кадр.
    super::paging::enable_write_combining();
    super::interrupts::load_idt_secondary();
    apic::init_secondary();
    super::fpu::init_secondary();
    crate::smp::secondary_main(index)
}

/// Попросить остановиться все процессоры, кроме текущего.
pub fn stop_others() {
    apic::send_to_others(apic::VECTOR_IPI_STOP);
}

// --- Сброс трансляций на всех процессорах (фаза 56) ----------------------------

/// Номер последней просьбы сбросить трансляции программ.
static FLUSH_ASKED: AtomicU64 = AtomicU64::new(0);

/// По какую просьбу включительно каждый процессор уже сбросил.
static FLUSH_DONE: [AtomicU64; crate::smp::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::smp::MAX_CPUS];

/// Сколько ждать ответа, прежде чем сказать о молчащем процессоре вслух.
///
/// Секунда — на порядки больше, чем любой исправный процессор держит
/// прерывания закрытыми (самое долгое такое место — сборка кадра, сотни
/// миллисекунд в отладочной сборке). Дольше — это не «медленно», а «не
/// ответит», и ждать вечно значило бы превратить чужую поломку в свою.
const FLUSH_PATIENCE_MS: u64 = 1_000;

/// Сбросить трансляции программ на всех процессорах, кроме этого, и
/// дождаться, что сбросили (фаза 56).
///
/// # Зачем
///
/// `invlpg` сбрасывает трансляцию только на том процессоре, который его
/// исполнил. Пока у процесса был один поток, больше и не требовалось: уходя с
/// процессора, задача меняет `CR3`, и это сбрасывает её трансляции само. Поток
/// же, работающий на другом процессоре в том же пространстве, держит свои — и
/// после `munmap` пишет по ним в кадр, который пул уже отдал другому.
///
/// На AArch64 этого вопроса нет: `tlbi …is` рассылается всем процессорам
/// аппаратно, и `arch::flush_user_translations` там пуст.
///
/// # Почему ждём с закрытыми прерываниями и обслуживаем себя сами
///
/// Два процессора могут попросить друг друга одновременно. С открытыми
/// прерываниями это бы обошлось, но с ними задачу могли бы вытеснить посреди
/// ожидания и перенести на другой процессор — и она ждала бы ответа от того,
/// кому ничего не посылала. Поэтому ожидание идёт с закрытыми, а встречную
/// просьбу процессор выполняет в том же цикле, не дожидаясь прерывания.
///
/// Сбрасывается всё, а не страница: перезапись `CR3` тем же значением стоит
/// одного обхода таблиц на каждый следующий промах, и это дешевле, чем
/// передавать другим процессорам список адресов.
///
/// # Только тем, кто в этом пространстве (19.11.2026)
///
/// `root` — корень пространства, которое поменяли; `None` — всем (глобальный
/// `membarrier`). Раньше просьба шла всем процессорам и ждала всех — и
/// простаивающих в `hlt`, и занятых чужими программами, которым сброс не
/// нужен: `CR3` у нас без PCID, и смена пространства сама уносит все
/// трансляции программ. В QEMU (TCG на Windows) спящий процессор — поток
/// хоста, который будится по таймеру хоста; perf-diag видел ожидание до
/// 5 мс на сброс. Теперь прерывание получают только процессоры, у которых
/// в `CR3` сейчас стоит `root` ([`ACTIVE_ROOT`]), и ждём только их.
///
/// Почему не опоздать: процессор сначала пишет свой корень в
/// [`ACTIVE_ROOT`], потом `CR3` (сериализующая запись), а мы — таблицы,
/// затем полный барьер, затем читаем [`ACTIVE_ROOT`]. Либо мы видим его
/// корень и шлём просьбу, либо его обход таблиц начнётся после нашей
/// записи и увидит новые записи.
pub fn shootdown(root: Option<u64>) {
    let root = root.filter(|_| FLUSH_TARGETED.load(Ordering::Relaxed));
    super::interrupts::without_interrupts(|| {
        let me = current_index();
        // Записи таблиц — до чтения корней чужих процессоров (см. выше).
        core::sync::atomic::fence(Ordering::SeqCst);
        let mut targets = [false; crate::smp::MAX_CPUS];
        let mut any = false;
        for (cpu, target) in targets.iter_mut().enumerate() {
            if cpu == me || !crate::smp::is_online(cpu) {
                continue;
            }
            if root.is_some_and(|root| ACTIVE_ROOT[cpu].load(Ordering::SeqCst) != root) {
                continue;
            }
            *target = true;
            any = true;
        }
        if !any {
            FLUSH_SKIPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let ticket = FLUSH_ASKED.fetch_add(1, Ordering::AcqRel) + 1;
        let mut sent = 0u64;
        for (cpu, _) in targets.iter().enumerate().filter(|(_, target)| **target) {
            let id = APIC_IDS[cpu].load(Ordering::Relaxed);
            if id == u32::MAX {
                // Номера не знаем — тогда уж всем, как раньше.
                apic::send_to_others(apic::VECTOR_IPI_FLUSH);
                sent = crate::smp::MAX_CPUS as u64;
                break;
            }
            apic::send_to(id, apic::VECTOR_IPI_FLUSH);
            sent += 1;
        }
        FLUSH_TARGETS.fetch_add(sent.min(crate::smp::MAX_CPUS as u64), Ordering::Relaxed);

        let start = super::tsc::counter();
        // Сколько ждали ответов — в `perf-diag` (P1): число сбросов без цены
        // не говорит, тормозят ли они.
        let _spent = Spent(start);
        let patience = super::tsc::frequency() / 1000 * FLUSH_PATIENCE_MS;
        loop {
            serve_flush();
            let behind = (0..crate::smp::MAX_CPUS)
                .filter(|&cpu| targets[cpu])
                .find(|&cpu| FLUSH_DONE[cpu].load(Ordering::Acquire) < ticket);
            let Some(cpu) = behind else {
                break;
            };
            if patience != 0 && super::tsc::counter().wrapping_sub(start) > patience {
                crate::kprintln!(
                    "  smp         : WARNING: cpu {cpu} did not flush its translations within {FLUSH_PATIENCE_MS} ms                      (ticket {ticket}, done {:?}, interrupts {:?})",
                    FLUSH_DONE.each_ref().map(|done| done.load(Ordering::Relaxed)),
                    FLUSH_IRQS.each_ref().map(|count| count.load(Ordering::Relaxed)),
                );
                break;
            }
            core::hint::spin_loop();
        }
    });
}

/// Выполнить просьбы сбросить трансляции, пришедшие к этому процессору.
///
/// Зовётся обработчиком [`apic::VECTOR_IPI_FLUSH`] и циклом ожидания в
/// [`shootdown`]. Отвечает на все просьбы по текущую разом: сброс один и тот
/// же, сколько бы их ни накопилось.
/// Сколько прерываний сброса принял каждый процессор — для предупреждения
/// о молчащем: «не дошло» и «дошло, но не туда» различаются только им.
static FLUSH_IRQS: [AtomicU64; crate::smp::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::smp::MAX_CPUS];

/// Тактов TSC, ушедших на ожидание ответов в [`shootdown`].
static FLUSH_WAIT_TSC: AtomicU64 = AtomicU64::new(0);

/// Адресный сброс (19.11.2026); `tlbtarget off` — всем, как раньше.
pub static FLUSH_TARGETED: AtomicBool = AtomicBool::new(true);

/// Самое долгое ожидание одного сброса (тактов TSC) с прошлого вопроса.
static FLUSH_WORST_TSC: AtomicU64 = AtomicU64::new(0);

/// Сбросов, которым не нашлось ни одного процессора в том же пространстве
/// (прерываний не было вовсе), и сколько процессоров получили просьбу всего.
static FLUSH_SKIPPED: AtomicU64 = AtomicU64::new(0);
static FLUSH_TARGETS: AtomicU64 = AtomicU64::new(0);

/// Корень пространства, стоящий сейчас в `CR3` каждого процессора (0 —
/// ещё не знаем). Пишет [`note_active_root`] перед каждой записью `CR3`.
static ACTIVE_ROOT: [AtomicU64; crate::smp::MAX_CPUS] = [const { AtomicU64::new(0) }; crate::smp::MAX_CPUS];

/// Этот процессор переходит на корень `root` — запомнить **до** записи
/// `CR3` (см. [`shootdown`]). Прерывания должны быть закрыты: иначе
/// задачу унесут на другой процессор между записью и `CR3`.
pub fn note_active_root(root: u64) {
    ACTIVE_ROOT[current_index()].store(root, Ordering::SeqCst);
}

/// Засчитывает ожидание при выходе из [`shootdown`] любой ветвью.
struct Spent(u64);

impl Drop for Spent {
    fn drop(&mut self) {
        let spent = super::tsc::counter().wrapping_sub(self.0);
        FLUSH_WAIT_TSC.fetch_add(spent, Ordering::Relaxed);
        FLUSH_WORST_TSC.fetch_max(spent, Ordering::Relaxed);
    }
}

/// Сбросы трансляций с загрузки: `(просьб с прерываниями, суммарное
/// ожидание нс, худшее ожидание нс с прошлого вопроса — и забыть его,
/// сбросов без адресатов, процессоров-адресатов всего)` — для `perf-diag`
/// (P1, 18.11.2026; адресаты — 19.11).
#[must_use]
pub fn shootdown_stats() -> (u64, u64, u64, u64, u64) {
    let hz = u128::from(super::tsc::frequency().max(1));
    let ns = |ticks: u64| (u128::from(ticks) * 1_000_000_000 / hz) as u64;
    (
        FLUSH_ASKED.load(Ordering::Relaxed),
        ns(FLUSH_WAIT_TSC.load(Ordering::Relaxed)),
        ns(FLUSH_WORST_TSC.swap(0, Ordering::Relaxed)),
        FLUSH_SKIPPED.load(Ordering::Relaxed),
        FLUSH_TARGETS.load(Ordering::Relaxed),
    )
}

/// Номера APIC процессоров по их индексам — для адресного [`kick`].
static APIC_IDS: [core::sync::atomic::AtomicU32; crate::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU32::new(u32::MAX) }; crate::smp::MAX_CPUS];

/// Сколько пинков послано и сколько принято — `perf-diag`.
static KICKS_SENT: AtomicU64 = AtomicU64::new(0);
static KICKS_TAKEN: AtomicU64 = AtomicU64::new(0);

/// Пнуть процессор `cpu` (P9): у планировщика для него готовая задача, а он,
/// возможно, стоит в `hlt` до тика.
pub fn kick(cpu: usize) {
    let id = APIC_IDS.get(cpu).map_or(u32::MAX, |id| id.load(Ordering::Relaxed));
    if id == u32::MAX || cpu == current_index() {
        return;
    }
    KICKS_SENT.fetch_add(1, Ordering::Relaxed);
    super::interrupts::without_interrupts(|| apic::send_to(id, apic::VECTOR_IPI_KICK));
}

/// Обработчик [`apic::VECTOR_IPI_KICK`].
pub fn on_kick() {
    KICKS_TAKEN.fetch_add(1, Ordering::Relaxed);
}

/// Пинки: посланные и принятые.
#[must_use]
pub fn kick_stats() -> (u64, u64) {
    (KICKS_SENT.load(Ordering::Relaxed), KICKS_TAKEN.load(Ordering::Relaxed))
}

/// Обработчик [`apic::VECTOR_IPI_FLUSH`].
pub fn on_flush_interrupt() {
    FLUSH_IRQS[current_index()].fetch_add(1, Ordering::Relaxed);
    serve_flush();
}

pub fn serve_flush() {
    let me = current_index();
    let asked = FLUSH_ASKED.load(Ordering::Acquire);
    if FLUSH_DONE[me].load(Ordering::Relaxed) < asked {
        paging::flush_local_translations();
        FLUSH_DONE[me].fetch_max(asked, Ordering::Release);
    }
}
