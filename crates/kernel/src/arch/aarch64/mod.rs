//! aarch64: UART PL011 через MMIO, страничная трансляция, исключения и
//! прерывания, остановка процессора.

pub mod context;
pub mod gic;
pub mod interrupts;
pub mod paging;
pub mod timer;

// Имена, которые `arch::mod` должен отдать наружу вместе с `ARCH_NAME`, `Serial`
// и `halt`. Пока интеграции нет, внутри крейта их никто не читает — отсюда и
// `allow`; снять, как только `main.rs` начнёт строить адресное пространство.
#[allow(unused_imports)]
pub use paging::{
    PageTables, StackEntry, build_kernel_address_space, kernel_stack_top, switch_stack,
};

// Управление прерываниями отдаётся наружу модулем целиком: `interrupts::init`
// ставит `VBAR_EL1` и поднимает GIC с таймером, `enable`/`disable`/`enabled`
// управляют маской `I` в `DAIF`, `without_interrupts` нужен примитивам
// синхронизации. Размаскирование — отдельный шаг, за который отвечает
// вызывающий: `init` его сознательно не делает.
#[allow(unused_imports)]
pub use interrupts::{disable, enable, enabled, without_interrupts};

use crate::serial::SerialDevice;
use boot_info::Arch;
use core::arch::asm;
use core::ptr;

pub const ARCH_NAME: &str = "aarch64";

/// Та же архитектура в кодировке хэндоффа — для сверки с тем, что заявил
/// загрузчик.
pub const ARCH_ID: Arch = Arch::AArch64;

/// Физический адрес PL011 на QEMU `-machine virt`.
///
/// В отличие от x86_64, где COM1 закреплён за портом 0x3F8 архитектурой
/// платформы, на ARM никакого «общепринятого» адреса UART не существует: у
/// Raspberry Pi это 0x3F20_1000 (BCM2835) или 0xFE20_1000 (BCM2711), у
/// Rockchip — своё, и так далее. Значение ниже верно ровно для одной машины —
/// QEMU virt, — и захардкожено осознанно: на Phase 1 device tree ещё не
/// разбирается, а без вывода в порт отладка после `ExitBootServices` невозможна.
///
/// TODO(Phase 2-3): адрес и тип UART должны приезжать из HAL — из FDT
/// (`BootInfo::device_tree`, узел `/pl011@...`) либо из ACPI SPCR
/// (`BootInfo::acpi_rsdp`). До тех пор ядро запускается только на QEMU virt.
const QEMU_VIRT_PL011: usize = 0x0900_0000;

// Смещения регистров PL011 (ARM PrimeCell UART, DDI0183).
const REG_DR: usize = 0x00; // Data
const REG_FR: usize = 0x18; // Flag
const REG_IBRD: usize = 0x24; // Integer Baud Rate Divisor
const REG_FBRD: usize = 0x28; // Fractional Baud Rate Divisor
const REG_LCRH: usize = 0x2C; // Line Control
const REG_CR: usize = 0x30; // Control
const REG_IMSC: usize = 0x38; // Interrupt Mask Set/Clear
const REG_ICR: usize = 0x44; // Interrupt Clear

const FR_TXFF: u32 = 1 << 5; // передающее FIFO заполнено

const LCRH_FEN: u32 = 1 << 4; // включить FIFO
const LCRH_WLEN_8: u32 = 0b11 << 5; // 8 бит данных

const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;

/// Делители для 115200 бод при UARTCLK 24 МГц (частота QEMU virt):
/// 24_000_000 / (16 * 115200) = 13.0208 → целая часть 13, дробная 0.0208*64 ≈ 1.
/// QEMU скорость игнорирует, но на настоящем PL011 без этого вывод будет мусором.
const IBRD_115200: u32 = 13;
const FBRD_115200: u32 = 1;

/// Ограничение на опрос флага TXFF — см. те же соображения, что и на x86_64:
/// повиснуть в единственном канале диагностики хуже, чем потерять байт.
const TX_SPIN_LIMIT: u32 = 100_000;

/// UART PL011, отображённый в память.
pub struct Serial {
    base: usize,
}

impl Serial {
    /// UART, с которого ядро начинает говорить на этой платформе.
    pub const PLATFORM: Self = Self { base: QEMU_VIRT_PL011 };

    /// # Safety
    ///
    /// `self.base + offset` обязан указывать на существующий регистр PL011 в
    /// доступном (identity-mapped либо ещё не включённом MMU) адресном
    /// пространстве.
    unsafe fn read(&self, offset: usize) -> u32 {
        // SAFETY: `read_volatile` обязателен для MMIO — компилятору нельзя
        // разрешать кэшировать или выбрасывать чтение регистра состояния,
        // значение которого меняет устройство, а не программа. Адрес выровнен
        // на 4 байта, так как все смещения регистров кратны 4.
        unsafe { ptr::read_volatile((self.base + offset) as *const u32) }
    }

    /// # Safety
    ///
    /// См. [`Serial::read`]. Кроме того, запись в регистр PL011 меняет
    /// состояние устройства, и вызывающий отвечает за корректность значения.
    unsafe fn write(&self, offset: usize, value: u32) {
        // SAFETY: `write_volatile` не даёт компилятору объединить или удалить
        // записи в регистры — для устройства важен и сам факт записи, и порядок.
        unsafe { ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
}

impl SerialDevice for Serial {
    fn init(&mut self) {
        // SAFETY: `self.base` — задокументированный адрес PL011 на QEMU virt.
        // К моменту вызова `ExitBootServices` уже сделан, MMU либо выключен,
        // либо оставлен прошивкой с identity-отображением устройств, поэтому
        // физический адрес доступен напрямую. Порядок операций — из даташита:
        // выключить UART, сбросить все флаги прерываний, задать скорость и
        // формат, замаскировать прерывания, включить обратно.
        unsafe {
            self.write(REG_CR, 0);
            self.write(REG_ICR, 0x7FF);
            self.write(REG_IBRD, IBRD_115200);
            self.write(REG_FBRD, FBRD_115200);
            self.write(REG_LCRH, LCRH_FEN | LCRH_WLEN_8);
            self.write(REG_IMSC, 0);
            self.write(REG_CR, CR_UARTEN | CR_TXE | CR_RXE);
        }
    }

    fn write_byte(&mut self, byte: u8) {
        let mut spins = 0u32;
        loop {
            // SAFETY: чтение регистра флагов PL011 не имеет побочных эффектов.
            let flags = unsafe { self.read(REG_FR) };
            if flags & FR_TXFF == 0 {
                break;
            }
            spins += 1;
            if spins >= TX_SPIN_LIMIT {
                return; // UART отсутствует или не разгребает FIFO
            }
        }
        // SAFETY: FIFO не заполнено (проверено выше), запись в DR ставит байт
        // в очередь на передачу и не затрагивает ничего другого.
        unsafe { self.write(REG_DR, u32::from(byte)) };
    }
}

/// Остановить процессор навсегда.
///
/// `wfi` (wait for interrupt) снимает ядро с конвейера до внешнего события —
/// как и `hlt` на x86_64, это нулевое энергопотребление вместо `loop {}`,
/// который жёг бы 100% CPU. `daifset` предварительно маскирует Debug, SError,
/// IRQ и FIQ: векторов исключений ядро ещё не установило, и пришедшее
/// прерывание ушло бы в мусорный обработчик.
pub fn halt() -> ! {
    // SAFETY: из функции нет возврата, поэтому маскирование прерываний уже
    // ни на какой последующий код повлиять не может.
    unsafe { asm!("msr daifset, #0b1111", options(nomem, nostack, preserves_flags)) };
    loop {
        // SAFETY: `wfi` при замаскированных прерываниях просто останавливает
        // ядро; память и стек не затрагиваются.
        unsafe { asm!("wfi", options(nomem, nostack, preserves_flags)) };
    }
}
