//! Вывод по последовательному порту — единственный канал диагностики ядра.
//!
//! После `ExitBootServices` UEFI stdout мёртв: прошивка больше не обслуживает
//! ни консоль, ни GOP-протоколы. Всё, что остаётся, — это UART, который никакой
//! инициализации со стороны ОС не требует и работает даже когда фреймбуфера нет.

use crate::arch;
use crate::sync::Racy;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

/// Общий интерфейс UART, который реализует каждая архитектура.
///
/// Намеренно минимальный: побайтовая запись — единственное, что одинаково
/// выражается и через порты ввода-вывода (16550), и через MMIO (PL011).
pub trait SerialDevice {
    /// Привести порт в состояние 8N1 / 115200 и включить передатчик.
    fn init(&mut self);
    /// Отправить один байт, дождавшись освобождения передатчика.
    fn write_byte(&mut self, byte: u8);
}

static SERIAL: Racy<arch::Serial> = Racy::new(arch::Serial::PLATFORM);

/// Пока порт не проинициализирован, вывод отбрасывается: писать в неготовый
/// UART бессмысленно, а на некоторых платформах ещё и опасно.
static READY: AtomicBool = AtomicBool::new(false);

/// Инициализировать порт. Вызывается один раз, самым первым делом в ядре.
pub fn init() {
    // SAFETY: единственный поток исполнения, прерывания выключены, повторных
    // входов нет — эксклюзивность ссылки обеспечена структурой запуска ядра.
    let device = unsafe { &mut *SERIAL.get() };
    device.init();
    READY.store(true, Ordering::Release);
}

struct Port;

impl Write for Port {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // SAFETY: см. `init` — доступ к глобальному порту не пересекается сам с
        // собой, потому что исполнение однопоточное и невытесняемое.
        let device = unsafe { &mut *SERIAL.get() };
        for byte in s.bytes() {
            // Терминалы, к которым QEMU подключает последовательный порт,
            // ожидают CRLF: без '\r' строки уезжают лесенкой.
            if byte == b'\n' {
                device.write_byte(b'\r');
            }
            device.write_byte(byte);
        }
        Ok(())
    }
}

/// Точка входа макросов вывода. Не вызывать напрямую.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    // Ошибка форматирования в UART невозможна: `write_byte` не возвращает
    // ошибок, — но `write_fmt` обязан вернуть Result, и игнорировать его здесь
    // корректно.
    let _ = Port.write_fmt(args);
}
