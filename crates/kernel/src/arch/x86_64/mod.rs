//! x86_64: UART 16550 на COM1, остановка процессора и страничная трансляция.

pub mod paging;

pub use paging::{PageTable, build_kernel_address_space, switch_stack};

use crate::serial::SerialDevice;
use boot_info::Arch;
use core::arch::asm;

pub const ARCH_NAME: &str = "x86_64";

/// Та же архитектура в кодировке хэндоффа — для сверки с тем, что заявил
/// загрузчик.
pub const ARCH_ID: Arch = Arch::X86_64;

/// Базовый порт COM1. На PC-совместимых машинах (включая QEMU `-machine q35`)
/// он зафиксирован архитектурой платформы ещё со времён IBM PC, и прошивка его
/// не переносит — в отличие от aarch64, здесь константа не является допущением
/// про конкретную плату.
const COM1: u16 = 0x3F8;

// Регистры 16550 адресуются как смещения от базового порта.
const REG_DATA: u16 = 0; // RBR/THR, а при DLAB=1 — младший байт делителя
const REG_IER: u16 = 1; // Interrupt Enable, а при DLAB=1 — старший байт делителя
const REG_FCR: u16 = 2; // FIFO Control (запись)
const REG_LCR: u16 = 3; // Line Control
const REG_MCR: u16 = 4; // Modem Control
const REG_LSR: u16 = 5; // Line Status

const LCR_DLAB: u8 = 0x80; // открывает доступ к делителю вместо IER/RBR
const LCR_8N1: u8 = 0x03; // 8 бит данных, без чётности, 1 стоп-бит
const LSR_THR_EMPTY: u8 = 0x20; // регистр передатчика готов принять байт

/// Делитель тактовой частоты 1.8432 МГц: 115200 бод = 1843200 / (16 * 1).
const BAUD_DIVISOR: u16 = 1;

/// Сколько раз опросить LSR перед тем, как признать порт неработающим.
///
/// Без этого ограничения ядро на машине без COM1 (LSR читается как 0xFF или
/// 0x00) зависло бы в первом же `println` — а это единственная диагностика,
/// которая у нас есть. Лучше потерять байт, чем всё ядро.
const TX_SPIN_LIMIT: u32 = 100_000;

/// Запись байта в порт ввода-вывода.
///
/// # Safety
///
/// Запись в порт — это обращение к устройству: вызывающий обязан гарантировать,
/// что по адресу `port` находится именно тот регистр, который он собирается
/// изменить, и что запись в него не нарушит работу остальной системы.
unsafe fn outb(port: u16, value: u8) {
    // SAFETY: инструкция `out` не трогает память (`nomem`) и стек (`nostack`)
    // и не меняет флаги (`preserves_flags`); корректность самого адреса порта —
    // на совести вызывающего, см. контракт функции.
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
    }
}

/// Чтение байта из порта ввода-вывода.
///
/// # Safety
///
/// См. [`outb`]. Дополнительно: у некоторых устройств чтение регистра имеет
/// побочный эффект (сброс флага, извлечение байта из FIFO), поэтому читать
/// произвольные порты «на пробу» нельзя.
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: те же условия, что и в `outb`.
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    value
}

/// UART 16550, адресуемый через порты ввода-вывода.
pub struct Serial {
    base: u16,
}

impl Serial {
    /// UART, с которого ядро начинает говорить на этой платформе.
    pub const PLATFORM: Self = Self { base: COM1 };
}

impl SerialDevice for Serial {
    fn init(&mut self) {
        let base = self.base;
        // SAFETY: `base` — COM1, стандартный для PC диапазон портов 0x3F8..0x3FF;
        // последовательность соответствует даташиту 16550: запретить прерывания,
        // открыть делитель (DLAB=1), задать скорость, вернуть DLAB=0 вместе с
        // форматом 8N1, включить и очистить FIFO, поднять DTR/RTS/OUT2.
        // Никакое другое устройство в системе эти порты не использует, поэтому
        // записи ни на что больше не влияют.
        unsafe {
            outb(base + REG_IER, 0x00);
            outb(base + REG_LCR, LCR_DLAB);
            outb(base + REG_DATA, (BAUD_DIVISOR & 0xFF) as u8);
            outb(base + REG_IER, (BAUD_DIVISOR >> 8) as u8);
            outb(base + REG_LCR, LCR_8N1);
            // 0xC7 = FIFO вкл + сброс приёмного и передающего FIFO + порог 14 байт.
            outb(base + REG_FCR, 0xC7);
            // 0x0B = DTR | RTS | OUT2. OUT2 нужен, чтобы линия прерывания UART
            // вообще доходила до PIC; прерывания мы не включаем, но состояние
            // порта оставляем каноническим для следующих фаз.
            outb(base + REG_MCR, 0x0B);
        }
    }

    fn write_byte(&mut self, byte: u8) {
        let base = self.base;
        let mut spins = 0u32;
        loop {
            // SAFETY: чтение LSR (0x3FD) не имеет побочных эффектов — это
            // регистр состояния, и его опрос является штатным способом дождаться
            // освобождения передатчика.
            let status = unsafe { inb(base + REG_LSR) };
            if status & LSR_THR_EMPTY != 0 {
                break;
            }
            spins += 1;
            if spins >= TX_SPIN_LIMIT {
                return; // порта нет либо он завис — молча теряем байт
            }
        }
        // SAFETY: THR свободен (проверено выше), запись байта в 0x3F8 при DLAB=0
        // отправляет его в линию и ничего больше не затрагивает.
        unsafe { outb(base + REG_DATA, byte) };
    }
}

/// Остановить процессор навсегда.
///
/// `hlt` переводит ядро в спящее состояние до следующего прерывания, поэтому
/// цикл `cli; hlt` не потребляет ни такта — в отличие от `loop {}`, который
/// крутил бы CPU на 100% и грел хост под QEMU. `cli` обязателен: таблицы
/// прерываний ещё нет, и любое пришедшее прерывание превратилось бы в
/// тройную ошибку с перезагрузкой машины.
pub fn halt() -> ! {
    // SAFETY: возврата из этой функции не предусмотрено, поэтому запрет
    // прерываний ничего не ломает — никакой код после неё не выполнится.
    unsafe { asm!("cli", options(nomem, nostack, preserves_flags)) };
    loop {
        // SAFETY: `hlt` при запрещённых прерываниях лишь останавливает конвейер;
        // память и стек не затрагиваются.
        unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
    }
}
