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
use core::sync::atomic::{AtomicU64, Ordering};

use super::{apic, gdt, paging, rdmsr};
use crate::mm::{PageFlags, PhysAddr, VirtAddr, PAGE_SIZE};
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
    .quad   0
    // 0x08: код, 64 бита (L).
    .quad   0x00209A0000000000
    // 0x10: данные, плоские 4 ГиБ.
    .quad   0x00CF92000000FFFF
    // 0x18: код, 32 бита, плоский.
    .quad   0x00CF9A000000FFFF
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
    let root = paging::kernel_root().as_u64();
    if root >= 1 << 32 {
        return Err("the kernel page tables lie above 4 GiB, beyond a 32-bit CR3");
    }
    let start = (&raw const smp_trampoline_start) as usize;
    let size = (&raw const smp_trampoline_end) as usize - start;
    if size > TRAMPOLINE_STACK - 64 {
        return Err("the start-up code does not fit its page");
    }
    let Some(frame) = crate::mm::frame::take_low_frame() else {
        return Err("no free page below 640 KiB for the start-up code");
    };
    let base = frame.as_u64();
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

    // После включения страниц процессор выбирает следующую инструкцию по
    // тождественному адресу страницы — а прямое отображение памяти запрещает
    // исполнение. Страница получает право исполнения и теряет право записи:
    // писать в неё дальше будет только загрузочный процессор, и только через
    // прямое отображение.
    //
    // SAFETY: таблицы ядра активны; адрес — тождественное отображение кадра,
    // который принадлежит только трамплину.
    let mapped = unsafe {
        paging::map_active(
            VirtAddr::new(base as usize),
            frame,
            PAGE_SIZE,
            PageFlags::READ | PageFlags::EXEC,
        )
    };
    if mapped.is_err() {
        return Err("the start-up page cannot be made executable");
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
    // SAFETY: загрузочный подготовил TSS этого процессора до запуска, и это
    // первый код на нём; ни одного лока до этой строки не взято.
    unsafe { gdt::load_secondary(index) };
    super::interrupts::load_idt_secondary();
    apic::init_secondary();
    super::fpu::init_secondary();
    crate::smp::secondary_main(index)
}

/// Попросить остановиться все процессоры, кроме текущего.
pub fn stop_others() {
    apic::send_to_others(apic::VECTOR_IPI_STOP);
}
