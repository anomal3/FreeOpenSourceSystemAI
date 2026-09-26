// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Драйвер учебного устройства QEMU `edu` (`1234:11e8`) — программой, а не в
//! ядре (веха «драйверы по VID:PID», часть Д1).
//!
//! # Зачем именно `edu`
//!
//! Потому что у него есть ровно то, что нужно доказать, и ничего сверх:
//! окно регистров, прерывание MSI и DMA. Встроенного драйвера у ядра для него
//! нет и не будет — это и есть «устройство, которого система не знала в день
//! выпуска». Едет драйвер только пакетом (`edu-1.0.fpk`, право `devices`), в
//! `/bin` его нет.
//!
//! # Что делает
//!
//! 1. Берёт устройство и отображает его окно регистров (BAR 0, мегабайт).
//! 2. Читает метку (`0x00`, младший байт `0xed`) и проверяет «живость»: карта
//!    отвечает на записанное число его инверсией (`0x04`).
//! 3. Считает 10! — пишет число в `0x08`, просит прерывание по готовности
//!    (`0x20`, бит 7) и **спит до него**, а не опрашивает: так проверяется, что
//!    MSI доходит до программы. Признак снимается записью в `0x64`.
//! 4. Гоняет 64 байта в карту и обратно по DMA (`0x80`–`0x98`, буфер карты с
//!    `0x40000`) и сверяет — так проверяется, что физический адрес, выданный
//!    ядром, верен и устройство пишет туда, куда сказано.
//!
//! Регистры описаны в `hw/misc/edu.c` и `docs/specs/edu.rst` QEMU.

#![no_std]
#![no_main]

use user_progs::{Line, device_map, device_open, device_wait, dma_alloc, exit, println};

const VENDOR: u16 = 0x1234;
const DEVICE: u16 = 0x11e8;

const REG_ID: usize = 0x00;
const REG_LIVENESS: usize = 0x04;
const REG_FACTORIAL: usize = 0x08;
const REG_STATUS: usize = 0x20;
const REG_IRQ_STATUS: usize = 0x24;
const REG_IRQ_ACK: usize = 0x64;
const REG_DMA_SRC: usize = 0x80;
const REG_DMA_DST: usize = 0x88;
const REG_DMA_COUNT: usize = 0x90;
const REG_DMA_CMD: usize = 0x98;

/// `0x20`, бит 0: карта ещё считает; бит 7: прерывание по готовности.
const STATUS_COMPUTING: u32 = 1 << 0;
const STATUS_IRQ_ON_DONE: u32 = 1 << 7;
/// Признаки прерывания: факториал готов, DMA закончен.
const IRQ_FACTORIAL: u32 = 0x1;
const IRQ_DMA: u32 = 0x100;
/// Команда DMA: пуск, направление «из карты в память», прерывание по концу.
const DMA_START: u64 = 1 << 0;
const DMA_FROM_CARD: u64 = 1 << 1;
const DMA_IRQ: u64 = 1 << 2;
/// Где у карты её собственный буфер для DMA.
const CARD_BUFFER: u64 = 0x40000;
const DMA_BYTES: usize = 64;

/// Сколько ждать прерывания. Карта считает и копирует за микросекунды; секунда —
/// с большим запасом на отладочную сборку под эмуляцией.
const WAIT_MS: u64 = 1000;

struct Card {
    base: usize,
}

impl Card {
    fn read32(&self, reg: usize) -> u32 {
        // SAFETY: окно регистров отображено ядром целиком (мегабайт), смещения
        // — из описания карты.
        unsafe { ((self.base + reg) as *const u32).read_volatile() }
    }

    fn write32(&self, reg: usize, value: u32) {
        // SAFETY: см. `read32`.
        unsafe { ((self.base + reg) as *mut u32).write_volatile(value) }
    }

    fn write64(&self, reg: usize, value: u64) {
        // SAFETY: см. `read32`; регистры DMA принимают запись в восемь байт.
        unsafe { ((self.base + reg) as *mut u64).write_volatile(value) }
    }

    /// Дождаться прерывания и снять признак `bit`. Ответ — число пришедших
    /// прерываний (ноль или код ошибки — не дождались) либо `-1000`, если
    /// пришло, но не то.
    fn await_irq(&self, bit: u32) -> i64 {
        let came = device_wait(0, WAIT_MS);
        if came <= 0 {
            return came;
        }
        let status = self.read32(REG_IRQ_STATUS);
        self.write32(REG_IRQ_ACK, status);
        if status & bit == 0 { -1000 } else { came }
    }

    /// Копирование по DMA и ожидание его конца.
    fn dma(&self, src: u64, dst: u64, command: u64) -> bool {
        self.write64(REG_DMA_SRC, src);
        self.write64(REG_DMA_DST, dst);
        self.write64(REG_DMA_COUNT, DMA_BYTES as u64);
        self.write64(REG_DMA_CMD, command | DMA_START | DMA_IRQ);
        self.await_irq(IRQ_DMA) > 0
    }
}

fn fail(what: &str, code: i64) -> ! {
    Line::new().str("edudrv: ").str(what).str(" (").signed(code).str(")").end();
    exit(1)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let handle = device_open(VENDOR, DEVICE, 0);
    if handle < 0 {
        fail("cannot take the edu card", handle);
    }
    let base = device_map(handle as usize, 0);
    if base < 0 {
        fail("cannot map BAR 0", base);
    }
    let card = Card { base: base as usize };

    let id = card.read32(REG_ID);
    if id & 0xff != 0xed {
        fail("the card does not identify itself as edu", i64::from(id));
    }
    card.write32(REG_LIVENESS, 0x1234_5678);
    if card.read32(REG_LIVENESS) != !0x1234_5678 {
        fail("the liveness register did not invert", 0);
    }
    Line::new()
        .str("edudrv: card version ")
        .num(u64::from(id >> 24))
        .str(".")
        .num(u64::from((id >> 16) & 0xff))
        .str(", registers answer")
        .end();

    // Факториал по прерыванию.
    card.write32(REG_STATUS, STATUS_IRQ_ON_DONE);
    card.write32(REG_FACTORIAL, 10);
    let came = card.await_irq(IRQ_FACTORIAL);
    if came <= 0 {
        fail("no interrupt after the factorial", came);
    }
    if card.read32(REG_STATUS) & STATUS_COMPUTING != 0 {
        fail("the interrupt came before the result", 0);
    }
    let result = card.read32(REG_FACTORIAL);
    Line::new().str("edudrv: 10! = ").num(u64::from(result)).str(" by interrupt").end();
    if result != 3_628_800 {
        fail("wrong factorial", i64::from(result));
    }

    // DMA туда и обратно.
    let mut phys = 0u64;
    let buffer = dma_alloc(4096, &mut phys);
    if buffer < 0 {
        fail("no DMA memory", buffer);
    }
    let buffer = buffer as usize as *mut u8;
    for index in 0..DMA_BYTES {
        // SAFETY: страница памяти DMA отображена программе на запись.
        unsafe { buffer.add(index).write_volatile((index as u8).wrapping_mul(7).wrapping_add(3)) };
    }
    if !card.dma(phys, CARD_BUFFER, 0) {
        fail("no interrupt after DMA to the card", 0);
    }
    if !card.dma(CARD_BUFFER, phys + 2048, DMA_FROM_CARD) {
        fail("no interrupt after DMA from the card", 0);
    }
    for index in 0..DMA_BYTES {
        // SAFETY: см. выше; вторая половина той же страницы.
        let (sent, back) = unsafe {
            (buffer.add(index).read_volatile(), buffer.add(2048 + index).read_volatile())
        };
        if sent != back {
            fail("DMA brought back different bytes", index as i64);
        }
    }
    println("edudrv: 64 bytes went to the card and back by DMA");
    exit(0)
}
