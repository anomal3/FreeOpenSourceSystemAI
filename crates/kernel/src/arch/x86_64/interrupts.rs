//! IDT, заглушки векторов и разбор исключений x86-64.
//!
//! # Почему не `extern "x86-interrupt"`
//!
//! Специальное соглашение о вызове `x86-interrupt` умеет всё, что здесь нужно:
//! оно само сохраняет регистры, само снимает со стека код ошибки и заканчивает
//! функцию через `iretq`. Беда в том, что оно закрыто фичей `abi_x86_interrupt`,
//! а атрибуты `#![feature(...)]` действуют на крейт целиком и объявляются в
//! корне — то есть потребовали бы правки `main.rs`.
//!
//! Второй путь — написать пролог и эпилог самому. С Rust 1.88 для этого не
//! нужно вообще никаких фич: `naked`-функции и `core::arch::naked_asm`
//! стабильны, а `global_asm!` был стабилен и раньше. Взят он, и не только из-за
//! отсутствия feature-флага: ручной пролог даёт то, чего `x86-interrupt` не
//! даёт в принципе, — **единый кадр со всеми регистрами общего назначения**,
//! который можно напечатать при отказе. С `x86-interrupt` обработчик видит
//! только `rip`/`rsp`/`rflags`, а `rax`..`r15` компилятор сохраняет так, как
//! ему удобно, и добраться до них нельзя.
//!
//! # Как устроены заглушки
//!
//! На каждый из 256 векторов ассемблер порождает короткую заглушку. Все они
//! одинакового размера (16 байт), поэтому адрес заглушки вектора `v` — это
//! просто `isr_stub_table + v * 16`, и таблице указателей взяться неоткуда.
//! Заглушка приводит стек к единому виду и уходит в общий пролог:
//!
//! ```text
//!   +--------+ <- RSP на входе в общий пролог
//!   | vector |     номер вектора, положила заглушка
//!   | error  |     код ошибки: свой у 8/10-14/17/21/29/30, ноль у остальных
//!   | rip    |  \
//!   | cs     |   |
//!   | rflags |   |  положил процессор
//!   | rsp    |   |
//!   | ss     |  /
//!   +--------+
//! ```
//!
//! Ноль вместо отсутствующего кода ошибки кладётся именно затем, чтобы кадр был
//! один на все векторы: иначе `TrapFrame` пришлось бы разбирать двумя разными
//! способами, а эпилог — снимать со стека разное число слов.
//!
//! # Чего в прологе намеренно нет
//!
//! Состояния SSE/x87. Ядро собирается под `x86_64-unknown-none`, а этот таргет
//! объявляет `-mmx,-sse,...,+soft-float`: компилятор не порождает ни одной
//! инструкции, работающей с `xmm`/`st(i)`, поэтому сохранять там нечего.
//! Появится пользовательский режим — появится и `fxsave`, но уже при
//! переключении контекста, а не в каждом обработчике.

use super::apic;
use super::gdt::{self, DescriptorTablePointer, KERNEL_CODE_SELECTOR};
use crate::irq::fault::{self, Fault, TrapContext};
use crate::kprintln;
use crate::sync::Racy;
use core::arch::{asm, global_asm};
use core::mem::size_of;
use core::sync::atomic::{AtomicBool, Ordering};

/// Векторов в таблице. Меньше нельзя: процессор индексирует IDT номером
/// вектора, и отсутствующая запись для вектора, который всё-таки пришёл, — это
/// #GP, а из #GP без обработчика получается двойная ошибка.
const IDT_ENTRIES: usize = 256;

/// Размер одной заглушки. Должен совпадать с `.balign 16` в [`global_asm!`]
/// ниже: адрес заглушки считается умножением, а не берётся из таблицы.
const STUB_STRIDE: usize = 16;

/// Бит `IF` в `RFLAGS` — разрешены ли маскируемые прерывания.
const RFLAGS_IF: u64 = 1 << 9;

// --- Номера векторов ----------------------------------------------------------

const VECTOR_DIVIDE_ERROR: u8 = 0;
const VECTOR_BREAKPOINT: u8 = 3;
const VECTOR_INVALID_OPCODE: u8 = 6;
const VECTOR_DOUBLE_FAULT: u8 = 8;
const VECTOR_GENERAL_PROTECTION: u8 = 13;
const VECTOR_PAGE_FAULT: u8 = 14;

/// Первый вектор, свободный от архитектурных исключений. 0..31 зарезервированы
/// Intel, и назначать их устройствам нельзя.
const VECTOR_FIRST_EXTERNAL: u8 = 32;

// --- Биты кода ошибки #PF -----------------------------------------------------
//
// SDM, Vol. 3A, 4.7. Значение бита `P` инвертировано относительно интуиции:
// единица означает, что страница **есть**, а отказ произошёл по правам.

/// `P` — страница присутствует, значит отказ по правам, а не по отсутствию.
const PF_PRESENT: u64 = 1 << 0;
/// `W/R` — обращение было записью.
const PF_WRITE: u64 = 1 << 1;
/// `U/S` — обращение из пользовательского режима.
const PF_USER: u64 = 1 << 2;
/// `RSVD` — в записи таблицы страниц установлен зарезервированный бит.
const PF_RESERVED: u64 = 1 << 3;
/// `I/D` — отказ произошёл при выборке инструкции.
const PF_INSTRUCTION_FETCH: u64 = 1 << 4;

// --- Биты дескриптора вектора -------------------------------------------------

/// Тип «64-битные ворота прерывания».
///
/// Отличие от ворот ловушки (0xF) ровно одно и ровно то, что нужно: при входе
/// процессор сбрасывает `IF`. Обработчик исполняется с запрещёнными
/// прерываниями и не может быть прерван сам собой — а значит стек не растёт
/// неограниченно, и `crate::irq::on_timer_tick` выполняется без гонки.
const GATE_TYPE_INTERRUPT: u8 = 0xE;
/// `P` — дескриптор валиден.
const GATE_PRESENT: u8 = 1 << 7;

/// Одна запись IDT (SDM, Vol. 3A, 6.14.1).
///
/// Адрес обработчика разрезан на три куска — наследие 32-битного формата, в
/// который 64-битное смещение просто не помещалось.
#[repr(C)]
#[derive(Clone, Copy)]
struct GateDescriptor {
    offset_low: u16,
    selector: u16,
    /// Биты 0..2 — номер IST, остальные нули.
    ist: u8,
    /// `P`, `DPL`, тип.
    flags: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

const _: () = assert!(size_of::<GateDescriptor>() == 16);

impl GateDescriptor {
    /// Пустая запись: `P = 0`. Обращение к такому вектору даёт #GP, что честнее
    /// прыжка по нулевому адресу.
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        flags: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn new(handler: usize, ist: u8) -> Self {
        let handler = handler as u64;
        Self {
            offset_low: handler as u16,
            selector: KERNEL_CODE_SELECTOR,
            ist: ist & 0b111,
            flags: GATE_PRESENT | GATE_TYPE_INTERRUPT,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

/// Выравнивание на 16 — рекомендация SDM: так дескриптор не пересекает границу
/// строки кеша, а доставка прерывания не платит за лишнее чтение.
#[repr(C, align(16))]
struct Idt([GateDescriptor; IDT_ENTRIES]);

static IDT: Racy<Idt> = Racy::new(Idt([GateDescriptor::MISSING; IDT_ENTRIES]));

// --- Кадр прерывания ----------------------------------------------------------

/// Всё, что общий пролог кладёт на стек, в порядке возрастания адресов.
///
/// Порядок полей обязан совпадать с порядком `push` в [`global_asm!`] ниже,
/// прочитанным задом наперёд: первый `push` оказывается по старшему адресу.
#[repr(C)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    /// Номер вектора, положенный заглушкой.
    pub vector: u64,
    /// Код ошибки: свой у части исключений, ноль у остальных.
    pub error: u64,
    // Дальше — то, что положил процессор.
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

global_asm!(
    // Отдельная секция вида `.text.*` попадает в `.text` при линковке по
    // умолчанию, то есть получает права R+X от `map_kernel_image`.
    ".pushsection .text.isr, \"ax\", @progbits",
    ".balign 16",

    // --- общий пролог и эпилог ---
    //
    // Выравнивание стека: при входе в исключение процессор округляет RSP вниз
    // до 16, затем кладёт 5 слов (40 байт), заглушка добавляет ещё 2 (16), а
    // пролог — 15 регистров (120). Итого 176 байт, кратно 16, — ровно то, чего
    // System V требует от RSP в точке `call`.
    "isr_common:",
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbp",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    // System V требует DF = 0 на входе в функцию, а прерванный код мог оставить
    // флаг взведённым.
    "cld",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    // Снять номер вектора и код ошибки: `iretq` о них не знает и ожидает, что
    // на вершине лежит `rip`.
    "add rsp, 16",
    "iretq",

    // --- таблица заглушек ---
    ".balign 16",
    ".globl isr_stub_table",
    ".hidden isr_stub_table",
    "isr_stub_table:",
    ".set isr_vector, 0",
    ".rept 256",
    ".balign 16",
    // Код ошибки процессор кладёт сам только у #DF, #TS, #NP, #SS, #GP, #PF,
    // #AC, #CP, #HV и #VC. Для остальных векторов его подменяет ноль, чтобы
    // кадр был одинаковым.
    ".if (isr_vector != 8) && (isr_vector != 10) && (isr_vector != 11) && (isr_vector != 12) && (isr_vector != 13) && (isr_vector != 14) && (isr_vector != 17) && (isr_vector != 21) && (isr_vector != 29) && (isr_vector != 30)",
    "push 0",
    ".endif",
    "push isr_vector",
    "jmp isr_common",
    ".set isr_vector, isr_vector + 1",
    ".endr",
    ".popsection",
    dispatch = sym dispatch,
);

unsafe extern "C" {
    /// Начало таблицы заглушек. Тип `[u8; 0]` — способ получить адрес символа,
    /// не обещая, что по нему что-то можно прочитать.
    #[link_name = "isr_stub_table"]
    static ISR_STUB_TABLE: [u8; 0];
}

/// Адрес заглушки вектора.
fn stub(vector: u8) -> usize {
    let base = (&raw const ISR_STUB_TABLE).cast::<u8>() as usize;
    base + usize::from(vector) * STUB_STRIDE
}

// --- Разбор исключений --------------------------------------------------------

/// Мнемоника и человекочитаемое имя вектора.
///
/// [`crate::irq::fault`] печатает только то, что умеет объяснить арх-независимо;
/// имя вектора добавляет к этому конкретику, ради которой иначе пришлось бы
/// лезть в SDM.
fn vector_name(vector: u8) -> &'static str {
    match vector {
        0 => "#DE divide error",
        1 => "#DB debug",
        2 => "NMI",
        3 => "#BP breakpoint",
        4 => "#OF overflow",
        5 => "#BR bound range exceeded",
        6 => "#UD invalid opcode",
        7 => "#NM device not available",
        8 => "#DF double fault",
        10 => "#TS invalid TSS",
        11 => "#NP segment not present",
        12 => "#SS stack-segment fault",
        13 => "#GP general protection fault",
        14 => "#PF page fault",
        16 => "#MF x87 floating-point error",
        17 => "#AC alignment check",
        18 => "#MC machine check",
        19 => "#XM SIMD floating-point error",
        20 => "#VE virtualization exception",
        21 => "#CP control protection exception",
        VECTOR_FIRST_EXTERNAL..=u8::MAX => "external interrupt",
        _ => "reserved vector",
    }
}

/// Точка, в которую приходят все 256 векторов.
///
/// Единственный вызывающий — `isr_common` из [`global_asm!`] выше; из Rust эту
/// функцию не зовёт никто, и ABI у неё `extern "C"` именно поэтому.
extern "C" fn dispatch(frame: *mut TrapFrame) {
    // SAFETY: `isr_common` кладёт в RDI собственный RSP сразу после того, как
    // уложил на стек весь кадр в раскладке `TrapFrame`. Кадр живёт до `iretq`,
    // то есть заведомо дольше этой функции, и никакой другой ссылки на него в
    // это время не существует: обработчик исполняется с `IF = 0` (ворота
    // прерывания), а ядро однопоточно.
    let frame = unsafe { &mut *frame };
    // Маска — страховка от кодирования: `push imm8` знакорасширяет операнд до
    // 64 бит, и стоит ассемблеру выбрать эту форму для вектора больше 127, как
    // на стеке окажется 0xFFFF_FFFF_FFFF_FF80 вместо 0x80. Сейчас он выбирает
    // `push imm32` и такого не происходит, но зависеть от этого выбора не
    // хочется: номер вектора по определению не шире байта.
    let vector = (frame.vector & 0xFF) as u8;

    // Внешние прерывания и исключения разведены по разным `match` не ради
    // красоты: у них разный смысл по умолчанию. Неизвестное исключение — отказ,
    // после которого продолжать нельзя; неизвестное внешнее прерывание —
    // недоразумение, которое достаточно подтвердить и забыть.
    if vector >= VECTOR_FIRST_EXTERNAL {
        match vector {
            apic::VECTOR_TIMER => {
                crate::irq::on_timer_tick();
                apic::eoi();
            }
            // Спурьёзное прерывание — способ APIC сказать «я собирался что-то
            // доставить, но передумал». Единственная особенность: EOI на него
            // посылать нельзя, иначе будет подтверждён чужой, ещё не
            // обработанный вектор.
            apic::VECTOR_SPURIOUS => {}
            _ => {
                // Молча проглотить нельзя: без EOI локальный APIC не доставит
                // ничего следующего, включая тик таймера.
                kprintln!("interrupts: unexpected external vector {vector:#04x}, acknowledged");
                apic::eoi();
            }
        }
        return;
    }

    match vector {
        VECTOR_PAGE_FAULT => page_fault(frame),
        VECTOR_BREAKPOINT => breakpoint(frame),
        VECTOR_DIVIDE_ERROR => fatal(frame, Fault::DivideByZero),
        VECTOR_INVALID_OPCODE => fatal(frame, Fault::InvalidOpcode),
        VECTOR_DOUBLE_FAULT => fatal(frame, Fault::DoubleFault),
        VECTOR_GENERAL_PROTECTION => general_protection(frame),
        _ => fatal(frame, Fault::Other(u64::from(vector))),
    }
}

/// Отказ страницы: перевести аппаратные признаки в [`Fault::PageFault`].
fn page_fault(frame: &TrapFrame) -> ! {
    announce(frame);

    let addr = read_cr2();
    let error = frame.error;

    // Отдельная строка про зарезервированный бит: `fault::handle` о нём не
    // знает, а причина у такого отказа совсем другая — не права доступа, а
    // испорченная (или построенная не по правилам) запись таблицы страниц.
    if error & PF_RESERVED != 0 {
        kprintln!("  a page table entry for {addr:#018x} has a reserved bit set");
    }
    if error & PF_USER != 0 {
        kprintln!("  the faulting access came from user mode");
    }

    fault::handle(
        Fault::PageFault {
            addr,
            write: error & PF_WRITE != 0,
            fetch: error & PF_INSTRUCTION_FETCH != 0,
            protection: error & PF_PRESENT != 0,
        },
        context(frame),
    )
}

/// #GP: у кода ошибки здесь своя структура — это селектор, из-за которого всё
/// сломалось, либо ноль, если виноват не сегмент.
fn general_protection(frame: &TrapFrame) -> ! {
    announce(frame);
    if frame.error == 0 {
        kprintln!("  not caused by a segment selector; typical causes are a non-canonical");
        kprintln!("  address or a write to a reserved bit of a control register or MSR");
    } else {
        // Биты 0..1 — где искать дескриптор (0 = GDT, 1/3 = IDT, 2 = LDT),
        // биты 3..15 — его индекс.
        let table = match frame.error & 0b11 {
            0 => "GDT",
            2 => "LDT",
            _ => "IDT",
        };
        kprintln!("  offending descriptor: {table} #{}", (frame.error >> 3) & 0x1FFF);
    }
    fault::handle(
        Fault::Other(u64::from(VECTOR_GENERAL_PROTECTION)),
        context(frame),
    )
}

/// `int3` — единственное исключение, которое ядро переживает и продолжает
/// работу.
///
/// Так и задумано: точка останова — инструмент отладки, а не отказ.
/// [`fault::handle`] не возвращается по построению, поэтому здесь он не
/// вызывается: обработчик печатает состояние и делает `iretq` на инструкцию,
/// следующую за `int3` (её адрес процессор уже положил в `rip`).
fn breakpoint(frame: &TrapFrame) {
    kprintln!("x86_64: breakpoint at {:#018x}, resuming", frame.rip);
    registers(frame);
}

/// Отказ, после которого продолжать нельзя.
fn fatal(frame: &TrapFrame, kind: Fault) -> ! {
    announce(frame);
    fault::handle(kind, context(frame))
}

/// Заголовок диагностики: что за вектор и в каком состоянии был процессор.
fn announce(frame: &TrapFrame) {
    let vector = (frame.vector & 0xFF) as u8;
    kprintln!();
    kprintln!(
        "x86_64: exception {vector} ({}), error code {:#x}",
        vector_name(vector),
        frame.error
    );
    registers(frame);
}

fn registers(frame: &TrapFrame) {
    kprintln!("  rax {:#018x}  rbx {:#018x}  rcx {:#018x}", frame.rax, frame.rbx, frame.rcx);
    kprintln!("  rdx {:#018x}  rsi {:#018x}  rdi {:#018x}", frame.rdx, frame.rsi, frame.rdi);
    kprintln!("  rbp {:#018x}  r8  {:#018x}  r9  {:#018x}", frame.rbp, frame.r8, frame.r9);
    kprintln!("  r10 {:#018x}  r11 {:#018x}  r12 {:#018x}", frame.r10, frame.r11, frame.r12);
    kprintln!("  r13 {:#018x}  r14 {:#018x}  r15 {:#018x}", frame.r13, frame.r14, frame.r15);
    kprintln!(
        "  cs  {:#06x}          ss  {:#06x}          rflags {:#018x}",
        frame.cs,
        frame.ss,
        frame.rflags
    );
}

fn context(frame: &TrapFrame) -> TrapContext {
    TrapContext {
        pc: frame.rip as usize,
        sp: frame.rsp as usize,
        error: frame.error,
    }
}

/// Адрес, обращение к которому вызвало #PF.
fn read_cr2() -> usize {
    let value: usize;
    // SAFETY: чтение CR2 в ring 0 всегда разрешено и не имеет побочных
    // эффектов. `preserves_flags` не заявляем: SDM объявляет флаги после
    // `mov ..., cr2` неопределёнными.
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack)) };
    value
}

// --- Инициализация ------------------------------------------------------------

static INITIALISED: AtomicBool = AtomicBool::new(false);

/// Поднять всё, что нужно для приёма прерываний: GDT с TSS, IDT, контроллер
/// прерываний и системный таймер.
///
/// Прерывания при выходе остаются **запрещёнными**: разрешать их — отдельное
/// решение вызывающего, принимаемое тогда, когда остальное ядро к ним готово.
/// Разница существенная: с момента `sti` любая критическая секция может быть
/// прервана, и всё, что до сих пор полагалось на однопоточность (в частности
/// [`crate::sync::Racy`]), обязано перейти на [`without_interrupts`].
pub fn init() {
    if INITIALISED.swap(true, Ordering::Relaxed) {
        return;
    }

    // Заголовок секции печатает вызывающий (`main.rs`), здесь — только детали:
    // иначе на одну секцию приходится две одинаковых шапки.
    gdt::init();
    kprintln!(
        "  gdt/tss     : cs {:#06x}, ds {:#06x}, ist{} = #DF, ist{} = #PF",
        KERNEL_CODE_SELECTOR,
        super::gdt::KERNEL_DATA_SELECTOR,
        gdt::IST_DOUBLE_FAULT,
        gdt::IST_PAGE_FAULT
    );

    load_idt();
    kprintln!("  idt         : {IDT_ENTRIES} vectors, interrupt gates at ring 0");

    apic::init();
}

/// Заполнить и загрузить IDT.
fn load_idt() {
    let idt = IDT.get();
    for vector in 0..IDT_ENTRIES {
        let vector = vector as u8;
        // Отдельный стек нужен там, где обычный может быть уже непригоден:
        // см. подробный разбор в доккомментарии `gdt`.
        let ist = match vector {
            VECTOR_DOUBLE_FAULT => gdt::IST_DOUBLE_FAULT,
            VECTOR_PAGE_FAULT => gdt::IST_PAGE_FAULT,
            _ => 0,
        };
        // SAFETY: `IDT` — статика этого модуля, а ядро на момент установки
        // однопоточно и работает с запрещёнными прерываниями (см. контракт
        // `init`), поэтому другой ссылки на таблицу сейчас нет. Индекс меньше
        // `IDT_ENTRIES` по построению цикла.
        unsafe {
            (*idt).0[usize::from(vector)] = GateDescriptor::new(stub(vector), ist);
        }
    }

    let pointer = DescriptorTablePointer {
        limit: (size_of::<Idt>() - 1) as u16,
        base: idt as u64,
    };
    // SAFETY: таблица заполнена целиком выше, все 256 записей ссылаются на
    // заглушки внутри образа ядра и используют селектор кода из только что
    // установленной GDT. Статика живёт всё время работы ядра, а процессор
    // обращается к ней при каждом прерывании.
    unsafe {
        asm!("lidt [{}]", in(reg) &raw const pointer, options(readonly, nostack, preserves_flags));
    }
}

// --- Управление флагом IF -----------------------------------------------------

/// Разрешить маскируемые прерывания.
///
/// # Panics
///
/// Не паникует, но вызывать до [`init`] бессмысленно: IDT ещё нет, и первое же
/// прерывание превратится в тройную ошибку.
pub fn enable() {
    // SAFETY: `sti` разрешает доставку прерываний. Память и стек не
    // затрагиваются; `preserves_flags` заявлять нельзя — инструкция меняет `IF`.
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Запретить маскируемые прерывания.
pub fn disable() {
    // SAFETY: `cli` только сбрасывает `IF`.
    unsafe { asm!("cli", options(nomem, nostack)) };
}

/// Разрешены ли сейчас маскируемые прерывания.
#[must_use]
pub fn enabled() -> bool {
    let flags: u64;
    // SAFETY: `pushfq`/`pop` кладут и снимают одно слово с текущего стека —
    // отсюда отсутствие `nostack`. Сами флаги не меняются.
    unsafe { asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags)) };
    flags & RFLAGS_IF != 0
}

/// Выполнить `f` с запрещёнными прерываниями, вернув `IF` **ровно в то
/// состояние, в котором он был**.
///
/// Восстановление, а не безусловный `sti`, — принципиально. Иначе вложенный
/// вызов разрешил бы прерывания при выходе из внутренней критической секции,
/// хотя внешняя ещё не закончилась; ошибка при этом не проявляется на тестах и
/// ловится только по редкому повреждению состояния. По той же причине функция
/// не пытается «оптимизировать» повторный `cli`: он стоит несколько тактов, а
/// рассуждение остаётся простым.
///
/// Раскрутки стека в ядре нет (`panic = "abort"`, паника заканчивается
/// остановкой процессора), поэтому страж на случай паники внутри `f` не нужен:
/// восстанавливать `IF` будет уже некому и незачем.
pub fn without_interrupts<T>(f: impl FnOnce() -> T) -> T {
    let was_enabled = enabled();
    if was_enabled {
        disable();
    }
    let result = f();
    if was_enabled {
        enable();
    }
    result
}
