//! NVK поверх GSP-RM (18.11.2026, WP-A): видеопамять для таблиц страниц GPU,
//! блоков экземпляров каналов, USERD и буферов контекста GR.
//!
//! # Раскладка
//!
//! В Linux nouveau на GSP-RM видеопамятью по-прежнему распоряжается ядро
//! (`fb/r535.c`: свой распределитель поверх областей, которые назвал RM), и
//! так же здесь. Две арены внутри свободной области RM (`reserved == 0`):
//!
//! * **CPU-арена** `[0x0610_0000, 0x0800_0000)` — внутри окна BAR1
//!   (физический режим: смещение окна == адрес видеопамяти), процессор пишет
//!   сюда сам: PD3..PT, блоки экземпляров, USERD. Дисплей (`nvidia_disp`)
//!   занимает `0x0600_0000..0x0603_c000` (INST, SYNC, LUT) и кадры головок
//!   `0x0800_0000..0x0e60_0000` — спецификация предлагала
//!   `[0x0800_0000, 0x1000_0000)`, но там кадры, поэтому арена ниже них.
//! * **Bulk-арена** `[0x1000_0000, конец области)` — только для GPU: буферы
//!   контекста GR, буферы BO в видеопамяти. Процессор их не видит (BAR1 —
//!   256 МиБ).
//!
//! На карте Романа (журнал hw-log9c, 18.11) свободная область RM — 2:
//! `0x5150000..0x2f06dffff`; обе арены в ней. Если арены не попадают в
//! свободную область — `init` отказывается громко.

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{Ordering, fence};

use super::nvidia_gsp::{BAR1_WINDOW_END, BAR1_WINDOW_START, Log, Res, StaticInfo};
use crate::sync::SpinLock;
use nvgpu::arena::{Arena, VramAlloc};

/// CPU-арена (см. заголовок модуля).
pub(crate) const CPU_ARENA: (u64, u64) = (0x0610_0000, 0x0800_0000);
/// Начало bulk-арены: сразу за окном BAR1.
pub(crate) const BULK_START: u64 = 0x1000_0000;

const PAGE: u64 = 0x1000;

struct State {
    /// Арены (распределитель WP-B, `nvgpu::arena`).
    alloc: VramAlloc,
    /// Границы — для журнала и `free` (какой арене принадлежит адрес).
    cpu: (u64, u64),
    bulk: (u64, u64),
    window: usize,
}

impl State {
    fn describe(&self) -> String {
        format!(
            "cpu {:#x}..{:#x} ({} MiB), bulk {:#x}..{:#x} ({} MiB), used {} KiB",
            self.cpu.0,
            self.cpu.1,
            (self.cpu.1 - self.cpu.0) >> 20,
            self.bulk.0,
            self.bulk.1,
            (self.bulk.1 - self.bulk.0) >> 20,
            self.alloc.used() >> 10
        )
    }
}

static STATE: SpinLock<Option<State>> = SpinLock::new(None);

/// Поднять арены по областям RM (раз на сеанс GSP). Повторный вызов — ничего.
pub(crate) fn init(info: &StaticInfo, device: crate::pci::Device, log: &mut Log) -> Res<()> {
    if STATE.lock().is_some() {
        return Ok(());
    }
    let free_region = |addr: u64| {
        info.regions.iter().find(|r| r.reserved == 0 && r.protected == 0 && r.base <= addr && addr <= r.limit).copied()
    };
    let cpu_region = free_region(CPU_ARENA.0)
        .filter(|r| CPU_ARENA.1 - 1 <= r.limit)
        .ok_or_else(|| format!("CPU arena {:#x}..{:#x} is not inside a free FB region ({:x?})", CPU_ARENA.0, CPU_ARENA.1, info.regions))?;
    let bulk_region =
        free_region(BULK_START).ok_or_else(|| format!("bulk arena start {BULK_START:#x} is not inside a free FB region"))?;
    let window = super::nvidia_gsp::bar1_window(device)?;
    let bulk_end = bulk_region.limit + 1;
    let state = State {
        alloc: VramAlloc::with_ranges(&[(CPU_ARENA.0, CPU_ARENA.1 - CPU_ARENA.0)], &[(BULK_START, bulk_end - BULK_START)]),
        cpu: CPU_ARENA,
        bulk: (BULK_START, bulk_end),
        window,
    };
    // Проверка окна: запись и чтение через BAR1.
    let probe = CPU_ARENA.0;
    let ptr = (window + (probe - BAR1_WINDOW_START) as usize) as *mut u32;
    // SAFETY: адрес внутри отображённого окна BAR1, в нашей арене.
    let back = unsafe {
        ptr.write_volatile(0x4e56_4b21);
        fence(Ordering::SeqCst);
        let v = ptr.read_volatile();
        ptr.write_volatile(0);
        v
    };
    log.push(format!(
        "nvgpu: vram arenas {} (cpu in region {:#x}..{:#x}, bulk in region {:#x}..{:#x}); BAR1 window probe {back:#x} (want 0x4e564b21)",
        state.describe(),
        cpu_region.base,
        cpu_region.limit,
        bulk_region.base,
        bulk_region.limit
    ));
    if back != 0x4e56_4b21 {
        return Err("VRAM is not writable through the BAR1 window".into());
    }
    WINDOW.store(state.window, Ordering::Relaxed);
    *STATE.lock() = Some(state);
    Ok(())
}

/// Забыть арены (GSP-RM выгружен: вся видеопамять снова ничья).
pub(crate) fn reset() {
    if let Some(mut state) = STATE.try_lock() {
        *state = None;
    }
}

/// Кусок CPU-арены, обнулённый. Выравнивание — степень двойки, не меньше 4 КиБ.
pub(crate) fn cpu_alloc(size: u64, align: u64) -> Res<u64> {
    let addr = {
        let mut state = STATE.lock();
        let state = state.as_mut().ok_or("VRAM arenas are not set up")?;
        state.alloc.alloc(Arena::Cpu, size.div_ceil(PAGE) * PAGE, align.max(PAGE)).ok_or_else(|| format!("CPU arena full ({size:#x} B wanted; {})", state.describe()))?
    };
    zero(addr, size.div_ceil(PAGE) * PAGE);
    Ok(addr)
}

/// Кусок bulk-арены (не обнуляется: процессору не виден).
pub(crate) fn bulk_alloc(size: u64, align: u64) -> Res<u64> {
    let mut state = STATE.lock();
    let state = state.as_mut().ok_or("VRAM arenas are not set up")?;
    state.alloc.alloc(Arena::Bulk, size.div_ceil(PAGE) * PAGE, align.max(PAGE)).ok_or_else(|| format!("bulk arena full ({size:#x} B wanted; {})", state.describe()))
}

/// Вернуть кусок (любой арены).
pub(crate) fn free(addr: u64, size: u64) {
    let mut state = STATE.lock();
    if let Some(state) = state.as_mut() {
        let arena = if (state.cpu.0..state.cpu.1).contains(&addr) { Arena::Cpu } else { Arena::Bulk };
        if !state.alloc.free(arena, addr, size.div_ceil(PAGE) * PAGE) {
            crate::kprintln!("  nvgpu: BUG: VRAM free {addr:#x}+{size:#x} not allocated");
        }
    }
}

/// Сколько занято (GETPARAM VRAM_USED).
pub(crate) fn used() -> u64 {
    STATE.lock().as_ref().map_or(0, |s| s.alloc.used())
}

/// Размер bulk-арены — видеопамять, которую получают пользователи (FB_SIZE).
pub(crate) fn user_bytes() -> u64 {
    STATE.lock().as_ref().map_or(0, |s| s.bulk.1 - s.bulk.0)
}

pub(crate) fn describe() -> String {
    STATE.lock().as_ref().map_or_else(|| "not set up".into(), State::describe)
}

/// Окно BAR1 отображено навсегда (`nvidia_gsp::bar1_window`) — адрес держится
/// в атомике, чтобы запись PTE не брала замок арен.
static WINDOW: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

fn window() -> usize {
    WINDOW.load(Ordering::Relaxed)
}

/// Указатель процессора на видеопамять `addr` (только окно BAR1).
fn ptr(addr: u64, width: u64) -> Option<*mut u8> {
    if addr < BAR1_WINDOW_START || addr + width > BAR1_WINDOW_END {
        crate::kprintln!("  nvgpu: BUG: CPU access to VRAM {addr:#x} outside the BAR1 window");
        return None;
    }
    let base = window();
    (base != 0).then(|| (base + (addr - BAR1_WINDOW_START) as usize) as *mut u8)
}

pub(crate) fn w32(addr: u64, v: u32) {
    if let Some(p) = ptr(addr, 4) {
        // SAFETY: внутри отображённого окна BAR1; запись в видеопамять.
        unsafe { (p as *mut u32).write_volatile(v) }
    }
}

pub(crate) fn r32(addr: u64) -> u32 {
    // SAFETY: см. `w32`.
    ptr(addr, 4).map_or(u32::MAX, |p| unsafe { (p as *const u32).read_volatile() })
}

/// 64 бита двумя словами (младшее первым) — окно WC, порядок важен только
/// до барьера [`flush`].
pub(crate) fn w64(addr: u64, v: u64) {
    w32(addr, v as u32);
    w32(addr + 4, (v >> 32) as u32);
}

pub(crate) fn r64(addr: u64) -> u64 {
    u64::from(r32(addr)) | (u64::from(r32(addr + 4)) << 32)
}

pub(crate) fn zero(addr: u64, len: u64) {
    if let Some(p) = ptr(addr, len) {
        for i in (0..len as usize).step_by(8) {
            // SAFETY: внутри окна (проверено для всего куска).
            unsafe { (p.add(i) as *mut u64).write_volatile(0) }
        }
    }
}

/// Дождаться, пока записи процессора через окно WC дойдут до карты:
/// барьер и чтение назад (спецификация, риск 5).
pub(crate) fn flush(addr: u64) {
    fence(Ordering::SeqCst);
    let _ = r32(addr);
}
