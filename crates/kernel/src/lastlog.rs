// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Журнал прошлой загрузки телефона: зеркало кольца [`crate::klog`] в памяти,
//! которую тёплый сброс не стирает (05.11).
//!
//! # Зачем
//!
//! NetSurf вешал телефон целиком и за миллисекунды: экран стоит, USB молчит,
//! паники нет, выход один — зажать кнопки. Кольцо журнала живёт в `.bss` и
//! обнуляется следующей загрузкой, так что о зависании не оставалось ни
//! строки. Теперь машину сбрасывает сторожевой таймер (`mtk::watchdog_arm`), LK
//! открывает fastboot, а журнал зависшей загрузки ждёт в памяти, пока ядро не
//! запустят снова, — и отдаётся по `fastboot oem lastlog`.
//!
//! # Как
//!
//! Участок 128 КиБ ОЗУ занимается у распределителя кадров до первого
//! выданного кадра, как у [`crate::crashlog`]. В нём два слота по 64 КиБ:
//! заголовок и кольцо той же ёмкости и той же раскладки, что у журнала
//! (позиция — число байт с запуска по модулю ёмкости), поэтому каждая запись в
//! журнал повторяется в слот ровно по тому же смещению. Записанное
//! выталкивается из кеша в память (`dc cvac`) сразу: строка, оставшаяся в
//! кеше, при сбросе пропадает, а последние строки и есть то, ради чего всё.
//!
//! Слоты чередуются: загрузка пишет в тот, где **не** лежит журнал прошлой, и
//! прошлый остаётся на месте до следующей загрузки — копировать его некуда и
//! незачем.
//!
//! Заголовок закрыт суммой по полям, которые не меняются (метка, номер
//! загрузки, ёмкость, физический адрес слота): после холодного старта память —
//! мусор, и мусор не выдаётся за журнал. Сам текст суммой не закрыт — сумма по
//! кольцу на каждую строку стоила бы 32 КиБ счёта; вместо неё при загрузке
//! считается, сколько байт не похожи на текст, и это число печатается рядом.
//!
//! # Чего это не умеет
//!
//! Холодный старт (питание сняли) память стирает. LK вправе затереть участок:
//! адрес выбран высоко, вдали от мест, которые он занимает на глазах (см.
//! [`CANDIDATES`]), но буфер приёма `fastboot flash` и его собственные
//! участки — его дело, и проверить это можно только на аппарате. Без режима
//! «DDR reserve» TOPRGU память при сбросе какое-то время не обновляется;
//! обычно это переживается, но отдельные испорченные байты возможны — их счёт
//! и печатается.

#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::klog::CAPACITY;
use crate::kprintln;
use crate::mm::PhysAddr;
use crate::sync::SpinLock;

/// Где участок может лежать — по порядку предпочтения. Берётся первый, вся
/// память которого по карте свободна: занятое LK (`/memreserve/`,
/// `reserved-memory`) карта уже вычла.
///
/// Высоко и круглым числом, подальше от мест LK: ядро он кладёт с
/// `0x40080000`, дерево — на `0x47880000`, RAM-диск — на `0x51b00000`, свои
/// участки (`0x7ec50000..0x80000000`, от `0xbfc00000`) держит у границ.
/// Адрес обязан быть одним от загрузки к загрузке — иначе новая не нашла бы
/// старый журнал.
const CANDIDATES: &[u64] = &[0xB000_0000, 0x9800_0000, 0x6800_0000];

/// Слот: страница заголовка и кольцо ёмкостью с журнал.
const SLOT: usize = 0x1_0000;
const LENGTH: u64 = 2 * SLOT as u64;
const DATA: usize = 0x1000;
const _: () = assert!(DATA + CAPACITY <= SLOT);
const MASK: u64 = CAPACITY as u64 - 1;

/// «FOSLAST1» младшим байтом вперёд.
const MAGIC: u64 = u64::from_le_bytes(*b"FOSLAST1");

/// Поля заголовка — слова по порядку. Пишутся по одному и `volatile`.
const F_MAGIC: usize = 0;
const F_BOOT: usize = 1;
const F_CAPACITY: usize = 2;
const F_CRC: usize = 3;
const F_WRITTEN: usize = 4;
const F_FLAGS: usize = 5;
const F_LAST_MS: usize = 6;
/// Сколько байт заголовка выталкивать из кеша.
const HEADER: usize = 8 * 8;

/// Флаг: загрузка кончилась перезагрузкой по просьбе человека.
const CLEAN: u64 = 1;

/// Физический адрес участка, ноль — не занят.
static CLAIMED: AtomicU64 = AtomicU64::new(0);
/// Слот этой загрузки в прямом отображении, ноль — зеркала нет.
static CURRENT: AtomicUsize = AtomicUsize::new(0);
/// Слот прошлой загрузки в прямом отображении, ноль — журнала нет.
static PREVIOUS_BASE: AtomicUsize = AtomicUsize::new(0);
static PREVIOUS: SpinLock<Option<Previous>> = SpinLock::new(None);

/// Что оставила прошлая загрузка.
#[derive(Clone, Copy, Debug)]
pub struct Previous {
    /// Её номер (считается от первой загрузки с этим участком).
    pub boot: u64,
    /// Сколько байт она напечатала с запуска.
    pub written: u64,
    /// Кончилась перезагрузкой по просьбе человека, а не сбросом.
    pub clean: bool,
    /// Время последней строки, мс от её запуска.
    pub last_ms: u64,
    /// Сколько сохранённых байт не похожи на текст.
    pub odd: usize,
}

impl Previous {
    /// Самая старая сохранившаяся позиция.
    #[must_use]
    pub fn oldest(&self) -> u64 {
        self.written.saturating_sub(CAPACITY as u64)
    }
}

/// Занять участок у распределителя кадров — только на телефоне и до того,
/// как выдан первый кадр (таблицы страниц легли бы поверх журнала).
pub fn claim() {
    if !crate::arch::is_phone() {
        return;
    }
    for &start in CANDIDATES {
        let taken = crate::mm::frame::with(|frames| frames.claim_range(start, LENGTH)).unwrap_or(false);
        if taken {
            CLAIMED.store(start, Ordering::Relaxed);
            return;
        }
    }
    kprintln!("  lastlog     : no candidate range is free memory here; no log across resets");
}

fn field(base: usize, index: usize) -> *mut u64 {
    (base as *mut u64).wrapping_add(index)
}

/// # Safety
///
/// `base` — слот участка, занятого в [`claim`] и отображённого.
unsafe fn get(base: usize, index: usize) -> u64 {
    // SAFETY: контракт функции.
    unsafe { field(base, index).read_volatile() }
}

/// # Safety
///
/// См. [`get`].
unsafe fn set(base: usize, index: usize, value: u64) {
    // SAFETY: контракт функции.
    unsafe { field(base, index).write_volatile(value) }
}

/// CRC-32 (IEEE) по словам — побитно: считается дважды за загрузку.
fn crc32(words: &[u64]) -> u64 {
    let mut crc = !0u32;
    for word in words {
        for byte in word.to_le_bytes() {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
    }
    u64::from(!crc)
}

fn header_crc(boot: u64, phys: u64) -> u64 {
    crc32(&[MAGIC, boot, CAPACITY as u64, phys])
}

/// Номер загрузки, если в слоте лежит настоящий заголовок.
///
/// # Safety
///
/// См. [`get`].
unsafe fn valid(base: usize, phys: u64) -> Option<u64> {
    // SAFETY: контракт функции.
    unsafe {
        let boot = get(base, F_BOOT);
        (get(base, F_MAGIC) == MAGIC
            && get(base, F_CAPACITY) == CAPACITY as u64
            && get(base, F_CRC) == header_crc(boot, phys))
        .then_some(boot)
    }
}

/// # Safety
///
/// См. [`get`].
unsafe fn describe(base: usize, boot: u64) -> Previous {
    // SAFETY: контракт функции; индекс по маске не выходит из кольца слота.
    unsafe {
        let written = get(base, F_WRITTEN);
        let flags = get(base, F_FLAGS);
        let last_ms = get(base, F_LAST_MS);
        let data = (base + DATA) as *const u8;
        let mut odd = 0;
        for at in written.saturating_sub(CAPACITY as u64)..written {
            let byte = data.add((at & MASK) as usize).read_volatile();
            if (byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t')) || byte == 0x7f {
                odd += 1;
            }
        }
        Previous { boot, written, clean: flags & CLEAN != 0, last_ms, odd }
    }
}

/// Найти журнал прошлой загрузки и завести зеркало этой.
///
/// Зовётся, когда есть прямое отображение; всё, что напечатано до этого
/// мига, переносится из кольца целиком.
pub fn attach() {
    let start = CLAIMED.load(Ordering::Relaxed);
    if start == 0 {
        return;
    }
    let base = PhysAddr::new(start).to_direct_map().as_usize();
    let slots = [base, base + SLOT];
    let phys = [start, start + SLOT as u64];
    // SAFETY: участок занят в `claim`, отображён прямым отображением как
    // обычная память, и, кроме этого модуля, его не трогает никто.
    let boots = [0, 1].map(|index| unsafe { valid(slots[index], phys[index]) });
    let previous = match boots {
        [Some(first), Some(second)] => Some(usize::from(second > first)),
        [Some(_), None] => Some(0),
        [None, Some(_)] => Some(1),
        [None, None] => None,
    };
    let current = previous.map_or(0, |index| 1 - index);
    let boot = previous.and_then(|index| boots[index]).map_or(1, |boot| boot.wrapping_add(1));

    match previous {
        Some(index) => {
            // SAFETY: см. выше.
            let info = unsafe { describe(slots[index], boots[index].unwrap_or(0)) };
            PREVIOUS_BASE.store(slots[index], Ordering::Relaxed);
            *PREVIOUS.lock() = Some(info);
            let kept = info.written - info.oldest();
            let why = if info.clean { "clean reboot" } else { "no clean reboot: reset by watchdog?" };
            kprintln!("  lastlog     : previous boot left {kept} bytes ({why})");
            let odd = if info.odd > 0 { alloc::format!(", {} bytes look damaged", info.odd) } else { alloc::string::String::new() };
            kprintln!(
                "  lastlog     : boot #{} wrote its last line at {}.{:03} s{odd}; `fastboot oem lastlog`",
                info.boot,
                info.last_ms / 1000,
                info.last_ms % 1000
            );
        }
        None => kprintln!("  lastlog     : no log from a previous boot"),
    }

    let slot = slots[current];
    // SAFETY: см. выше. Метка — последней: до неё слот за журнал не сойдёт.
    unsafe {
        set(slot, F_MAGIC, 0);
        set(slot, F_BOOT, boot);
        set(slot, F_CAPACITY, CAPACITY as u64);
        set(slot, F_CRC, header_crc(boot, phys[current]));
        set(slot, F_WRITTEN, 0);
        set(slot, F_FLAGS, 0);
        set(slot, F_LAST_MS, crate::time::uptime_ms());
        set(slot, F_MAGIC, MAGIC);
    }
    crate::klog::with_ring(|bytes, written| {
        // SAFETY: см. выше; кольцо и слот одной ёмкости. Под замком кольца
        // новых строк нет, и ни одна не теряется и не попадает дважды.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), (slot + DATA) as *mut u8, CAPACITY);
            set(slot, F_WRITTEN, written);
        }
        clean(slot, DATA + CAPACITY);
        CURRENT.store(slot, Ordering::Release);
    });
    kprintln!("  lastlog     : this boot (#{boot}) is mirrored at {:#x}", phys[current]);
}

/// Повторить запись журнала в слоте этой загрузки. Зовёт [`crate::klog`] под
/// своим замком: `at` — смещение в кольце, `written` — новый счёт байт.
pub fn mirror(at: usize, bytes: &[u8], written: u64) {
    let base = CURRENT.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    let data = base + DATA + at;
    // SAFETY: слот заведён в `attach`; `at + bytes.len()` не больше ёмкости —
    // кольцо журнала режет запись по своему концу.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), data as *mut u8, bytes.len());
        set(base, F_WRITTEN, written);
        set(base, F_LAST_MS, crate::time::uptime_ms());
    }
    clean(data, bytes.len());
    clean(base, HEADER);
}

/// Отметить эту загрузку кончившейся по просьбе человека: следующая скажет
/// «clean reboot», а не «watchdog?».
pub fn mark_clean() {
    let base = CURRENT.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    // SAFETY: слот заведён в `attach`.
    unsafe { set(base, F_FLAGS, get(base, F_FLAGS) | CLEAN) };
    clean(base, HEADER);
}

/// Что оставила прошлая загрузка, если оставила.
#[must_use]
pub fn previous() -> Option<Previous> {
    *PREVIOUS.lock()
}

/// Прочитать журнал прошлой загрузки с позиции `from` — так же, как
/// [`crate::klog::read`] читает журнал этой.
pub fn read_previous(from: u64, out: &mut [u8]) -> (usize, u64) {
    let base = PREVIOUS_BASE.load(Ordering::Relaxed);
    let Some(info) = previous() else {
        return (0, from);
    };
    if base == 0 {
        return (0, from);
    }
    let data = (base + DATA) as *const u8;
    let mut at = from.max(info.oldest());
    let mut count = 0;
    while at < info.written && count < out.len() {
        // SAFETY: индекс по маске внутри кольца слота; слот прошлой загрузки
        // эта загрузка не пишет.
        out[count] = unsafe { data.add((at & MASK) as usize).read_volatile() };
        count += 1;
        at += 1;
    }
    (count, at)
}

/// Вытолкнуть строки кеша с этим диапазоном в память.
fn clean(addr: usize, len: usize) {
    #[cfg(target_arch = "aarch64")]
    {
        // Строка кеша Cortex-A53 — 64 байта.
        const LINE: usize = 64;
        let mut line = addr & !(LINE - 1);
        while line < addr + len {
            // SAFETY: адрес отображён как обычная память; `dc cvac` только
            // выталкивает строку, содержимое не меняется.
            unsafe { core::arch::asm!("dc cvac, {}", in(reg) line, options(nostack, preserves_flags)) };
            line += LINE;
        }
        // SAFETY: барьер без побочных эффектов.
        unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = (addr, len);
}
