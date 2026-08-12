//! Классификация отказов процессора и единая реакция на них.
//!
//! Арх-код ловит исключение, переводит его в [`Fault`] и передаёт сюда. Так
//! диагностика пишется один раз, а не отдельно для каждой архитектуры, и
//! сообщения не расходятся между платформами.

// Часть вариантов возникает только на одной из архитектур: отдельного
// исключения деления на ноль на AArch64 нет вовсе, а точку останова x86-64
// обрабатывает и возвращается из неё, не доходя сюда. Неиспользуемый на «чужой»
// платформе вариант — не забытый код, а цена одного перечисления на обе.
#![allow(dead_code)]
// `PageFault` и `DoubleFault` — устоявшиеся названия. Сократить их до `Page` и
// `Double` ради того, чтобы имя варианта не повторяло имя перечисления, значит
// заменить общепринятый термин на загадку.
#![allow(clippy::enum_variant_names)]

use crate::mm::{PAGE_SIZE, STACK_SIZE, STACK_TOP};
use crate::{arch, kprintln};

/// Что именно произошло.
///
/// Список намеренно короткий: сюда попадает то, на что ядро способно осмысленно
/// среагировать или о чём обязано внятно сообщить. Всё прочее приезжает как
/// [`Fault::Other`] с исходным номером вектора.
#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// Обращение по неотображённому адресу либо с недостаточными правами.
    PageFault {
        /// Адрес, обращение к которому не удалось.
        addr: usize,
        /// Отказ произошёл на записи (иначе — на чтении).
        write: bool,
        /// Отказ произошёл при выборке инструкции. Верный признак того, что
        /// управление ушло в данные — например по испорченному указателю.
        fetch: bool,
        /// Страница отображена, но права не позволяют такое обращение. В
        /// отличие от «страницы нет», это чаще всего означает нарушение W^X.
        protection: bool,
    },
    /// Недопустимая инструкция.
    InvalidOpcode,
    /// Деление на ноль.
    DivideByZero,
    /// Отказ во время обработки другого отказа. Обрабатывается на отдельном
    /// стеке: обычный к этому моменту может быть уже непригоден.
    DoubleFault,
    /// Точка останова — единственный отказ, который не обязан быть фатальным.
    Breakpoint,
    /// Всё остальное, с исходным номером вектора или кодом класса.
    Other(u64),
}

/// Регистры, которых достаточно, чтобы понять, где всё сломалось.
#[derive(Debug, Clone, Copy, Default)]
pub struct TrapContext {
    /// Адрес инструкции, вызвавшей отказ.
    pub pc: usize,
    /// Указатель стека на момент отказа.
    pub sp: usize,
    /// Код ошибки, если архитектура его предоставила.
    pub error: u64,
}

/// Единая реакция на отказ: диагностика и остановка.
///
/// Возврата нет намеренно. Продолжать исполнение после отказа, не разобравшись
/// в причине, — верный способ превратить понятную ошибку в загадочную: реальное
/// повреждение состояния проявится позже и совсем в другом месте.
pub fn handle(fault: Fault, ctx: TrapContext) -> ! {
    kprintln!();
    kprintln!("*** CPU FAULT ***");

    match fault {
        Fault::PageFault { addr, write, fetch, protection } => {
            let access = if fetch {
                "instruction fetch"
            } else if write {
                "write"
            } else {
                "read"
            };
            let cause = if protection { "protection violation" } else { "page not present" };
            kprintln!("page fault: {access} at {addr:#018x} ({cause})");
            explain_address(addr, fetch);
        }
        Fault::DoubleFault => {
            kprintln!("double fault: a fault occurred while handling another one");
            kprintln!("the first fault's own handler could not run to completion");
        }
        Fault::InvalidOpcode => {
            kprintln!("invalid opcode at {:#018x}", ctx.pc);
            kprintln!("execution most likely reached data rather than code");
        }
        Fault::DivideByZero => kprintln!("divide by zero"),
        Fault::Breakpoint => kprintln!("breakpoint"),
        Fault::Other(code) => kprintln!("unhandled exception, code {code:#x}"),
    }

    kprintln!("  pc          : {:#018x}", ctx.pc);
    kprintln!("  sp          : {:#018x}", ctx.sp);
    if ctx.error != 0 {
        kprintln!("  error code  : {:#x}", ctx.error);
    }
    kprintln!("  uptime      : {} ms", crate::time::uptime_ms());
    kprintln!();
    kprintln!("FreeOS kernel: halted by an unrecoverable fault.");
    arch::halt();
}

/// Подсказать, во что попал адрес, если это узнаваемое место.
///
/// Самое ценное здесь — распознать страницу-ловушку под стеком. Без такой
/// подсказки переполнение стека выглядит как обращение по случайному адресу в
/// верхней половине, и на выяснение уходит несоразмерно много времени.
fn explain_address(addr: usize, fetch: bool) {
    let stack_bottom = STACK_TOP - STACK_SIZE;
    let guard_bottom = stack_bottom - PAGE_SIZE;

    if (guard_bottom..stack_bottom).contains(&addr) {
        kprintln!("  --> this is the kernel stack guard page: the stack overflowed");
        kprintln!("      stack spans {stack_bottom:#018x}..{STACK_TOP:#018x}");
        return;
    }
    if addr < PAGE_SIZE {
        kprintln!("  --> near address zero: a null pointer was dereferenced");
        return;
    }
    if fetch {
        kprintln!("  --> a fetch fault means control flow left executable memory");
    }
}
