//! Макросы вывода ядра: одна строка уходит сразу в оба приёмника.
//!
//! Serial есть всегда, экран — только если загрузчик нашёл GOP. Поэтому
//! [`kprintln!`](crate::kprintln) пишет в оба, и каждый приёмник сам решает,
//! готов ли он: до `serial::init` и до `console::init` вызовы просто ничего
//! не делают, что позволяет пользоваться макросами с самой первой инструкции.
//!
//! Третий приёмник — [`klog`](crate::klog), кольцо в памяти. Он не показывает
//! ничего и нужен там, где первые два молчат: у телефона линии наружу нет, а
//! экран забирает рабочий стол. Строку оттуда потом отдаёт по кабелю fastboot.

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

/// Самая длинная печать, измеренная в единицах монотонного счётчика, и номер
/// тика, на котором она случилась.
///
/// Считается потому, что увидеть это иначе нечем, а число оказалось не таким,
/// каким его все представляли. Замер на 1280x720 в отладочной сборке: **771
/// миллисекунда** на один вызов — столько занимает прокрутка экранной консоли,
/// перерисовывающая до тысячи ячеек. Три четверти секунды, за которые не
/// приходит ни тик таймера, ни байт из линии; приёмного FIFO хватает на полторы
/// миллисекунды линии.
///
/// Отсюда же и мера, которой лечится следствие (см.
/// [`crate::input::ascii::rescue`]): приёмник вычитывается изнутри самой печати.
/// Саму глухоту это не отменяет — она остаётся открытым дефектом, и счётчик тут
/// затем, чтобы её было видно, а не затем, чтобы её забыли.
static LONGEST: AtomicU64 = AtomicU64::new(0);
static LONGEST_AT: AtomicU64 = AtomicU64::new(0);

/// Насколько долго ядро молчало в самый долгий раз: единицы монотонного
/// счётчика и номер тика таймера, на котором это случилось.
#[must_use]
pub fn longest_silence() -> (u64, u64) {
    (LONGEST.load(Ordering::Relaxed), LONGEST_AT.load(Ordering::Relaxed))
}

/// Точка входа макросов вывода. Не вызывать напрямую.
///
/// Вывод идёт с запрещёнными прерываниями, и это не перестраховка. И UART, и
/// экранная консоль хранят изменяемое состояние (позиция курсора, теневой
/// буфер), а обработчик исключения печатает диагностику. Прилети отказ посреди
/// вывода — обработчик продолжил бы печатать в наполовину обновлённое
/// состояние, и сообщение о причине сбоя оказалось бы искажено ровно тогда,
/// когда оно нужнее всего.
///
/// TODO(SMP): на нескольких ядрах запрета прерываний недостаточно — понадобится
/// спинлок вокруг каждого приёмника.
///
/// G4 (21.11): the line is formatted first, with interrupts as the caller had
/// them, into a stack buffer; the interrupts-off stretch below only copies
/// ready bytes into the three in-memory sinks (serial queue, console text,
/// klog ring) - one formatting instead of three and no device access. The
/// serial queue goes to the port in [`crate::serial::pump`] after interrupts
/// are open again (or from the idle loop / tick when the caller keeps them
/// off), the console is painted the same way. A machine that stops (panic,
/// power off) switches the serial queue off first, and the same stretch then
/// writes to the port synchronously.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    let mut line = Line { bytes: [0; LINE], len: 0 };
    if fmt::Write::write_fmt(&mut line, args).is_ok() {
        line.flush();
    }
    // Порт и экран — только теперь, с прерываниями как у вызывающего. До
    // разделения одна печать держала машину глухой до 0.9 секунды (экран) и
    // до 55 мс (линия на 16 ЦП, G2).
    crate::serial::pump();
    crate::console::paint();
}

/// Longest piece copied into the sinks in one interrupts-off stretch. Longer
/// prints are split at character boundaries (lines from other CPUs may then
/// fall between the pieces - lines of perf-diag are ~300 bytes).
const LINE: usize = 512;

/// Stack buffer the line is formatted into.
struct Line {
    bytes: [u8; LINE],
    len: usize,
}

impl Line {
    fn flush(&mut self) {
        if self.len == 0 {
            return;
        }
        // Only whole `&str` pieces (cut at character boundaries) are
        // appended, so the buffer holds valid UTF-8 up to `len`.
        let text = core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("");
        sinks(text);
        self.len = 0;
    }
}

impl fmt::Write for Line {
    fn write_str(&mut self, mut text: &str) -> fmt::Result {
        while !text.is_empty() {
            if self.len + text.len() <= LINE {
                self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
                self.len += text.len();
                return Ok(());
            }
            if self.len > 0 {
                self.flush();
                continue;
            }
            let mut cut = LINE;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            self.bytes[..cut].copy_from_slice(&text.as_bytes()[..cut]);
            self.len = cut;
            self.flush();
            text = &text[cut..];
        }
        Ok(())
    }
}

/// Copy a formatted piece into serial queue, console text and klog with
/// interrupts off (a fault handler printing in the middle would otherwise
/// write into half-updated sink state).
fn sinks(text: &str) {
    crate::arch::interrupts::without_interrupts(|| {
        // Два чтения монотонного счётчика на строку — цена того, чтобы длина
        // молчания перестала быть догадкой. См. [`LONGEST`].
        let before = crate::arch::monotonic::counter();
        crate::serial::queue(format_args!("{text}"));
        crate::console::_print(format_args!("{text}"));
        crate::klog::_print(format_args!("{text}"));
        let spent = crate::arch::monotonic::counter().wrapping_sub(before);
        if LONGEST.fetch_max(spent, Ordering::Relaxed) < spent {
            // Номер тика, а не `time::uptime_ms`: тик — это обычный атомик, а
            // время считает код, которому здесь, с запрещёнными прерываниями и
            // из-под чужих локов, делать нечего.
            LONGEST_AT.store(crate::irq::ticks(), Ordering::Relaxed);
        }
    });
}

/// Напечатать в serial и (если есть) на экран, без перевода строки.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::print::_print(::core::format_args!($($arg)*))
    };
}

/// Напечатать в serial и (если есть) на экран, с переводом строки.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {
        $crate::kprint!("{}\n", ::core::format_args!($($arg)*))
    };
}
