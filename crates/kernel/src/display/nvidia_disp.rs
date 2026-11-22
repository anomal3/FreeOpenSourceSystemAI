//! Видеокарты NVIDIA, этап 6 (16.11.2026): свой режим экрана через GSP-RM.
//!
//! # Что здесь
//!
//! После запуска GSP-RM (`nvidia_gsp`) — то, что nouveau делает в своём пути
//! GSP (`r535/r570 disp.c`, `dispnv50`): память экземпляров дисплея (хеш-
//! таблица RAMHT и объекты ctxdma) в видеопамяти, корневой объект дисплея
//! GA102 (`0xc670`), каналы core (`0xc67d`) и window (`0xc67e`) с буферами
//! команд в памяти машины, и для каждого монитора на DisplayPort:
//! назначение SOR, тренировка линии (RM делает её сам по `DP_CTRL`), поток
//! SST, растр головки из EDID, окно с картинкой в видеопамяти и единичные
//! таблицы цвета. Картинку процессор рисует через окно BAR1 (физический
//! режим, см. `nvidia_gsp` шаг 12).
//!
//! Числа, раскладки и порядок — по спецификации (рабочие файлы разбора,
//! ROADMAP «Видеокарты NVIDIA»); непроверенное помечено в журнале.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use core::sync::atomic::{Ordering, fence};

use super::nvidia::Bar0;
use super::nvidia_gsp::{H_CLIENT, H_DEVICE, Log, Mem, Res, Rm, SESSION, le32};

// --- раскладка видеопамяти (область 2 RM: свободная, с 0x5150000) ----------

const INST: u64 = 0x0600_0000;
const SYNC: u64 = 0x0601_0000;
const fn olut(head: u32) -> u64 {
    0x0602_0000 + head as u64 * 0x8000
}
const fn ilut(head: u32) -> u64 {
    0x0602_4000 + head as u64 * 0x8000
}
/// Картинка головки: по 34 МиБ (4K с запасом), три головки — под 256 МиБ BAR1.
pub const fn fb(head: u32) -> u64 {
    0x0800_0000 + head as u64 * 0x0220_0000
}
/// Что отображается из BAR1 для процессора.
const VRAM_MAP_START: u64 = INST;
const VRAM_MAP_END: u64 = 0x1000_0000;

const H_ROOT: u32 = 0xc670_0000;
const H_CORE: u32 = 0xc67d_0000;
const H_SYNC: u32 = 0xf000_0000;
const H_VRAM: u32 = 0xf000_0001;
const H_FB: u32 = 0xfb00_0000;
/// G3: system memory for direct scanout of a client buffer.
const H_SYS: u32 = 0xf000_0002;
/// Объекты ctxdma в памяти экземпляров.
const OBJ_SYNC: u32 = 0x2000;
const OBJ_VRAM: u32 = 0x2020;
const OBJ_FB: u32 = 0x2040;
const OBJ_SYS: u32 = 0x2060;

/// G3: direct scanout of fullscreen client buffers (`wldirect flip on`).
/// Off by default until checked on the RTX 3060 (sysmem scanout, notifier,
/// cursor channel - see ROADMAP "Scan-out flip").
pub static FLIP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// G3: flips stopped after the display misbehaved (a flip not taken in
/// [`FLIP_TIMEOUT_MS`]); `wldirect flip on` clears it.
pub static FLIP_STOPPED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// G3: `wldirect flip timed` - do not use the window notifier: a flip counts
/// as on screen two refresh periods after it was kicked (fallback in case the
/// notifier never moves on the card).
pub static FLIP_TIMED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// G3: some head scans system memory or has a flip pending - only then does
/// [`scanning`] need a look (the Wayland server asks on every release).
static ANY_FLIP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// G3: a flip whose notifier has not said BEGUN/FINISHED by then stops flips.
const FLIP_TIMEOUT_MS: u64 = 250;
/// G3: window notifiers of the flips, one 16-byte `NV_DISP_NOTIFIER` per
/// head inside the SYNC ctxdma (`SET_NOTIFIER_CONTROL` offset 11:4).
const FLIP_NTFY: u64 = SYNC + 0x100;
/// G3: hardware cursor images (64x64 A8R8G8B8, 16 KiB per head) - free VRAM
/// between the LUTs (end 0x0604_0000) and the NVK CPU arena (0x0610_0000).
const fn curs(head: u32) -> u64 {
    0x0604_0000 + head as u64 * 0x4000
}
/// G3: side of the cursor image (HEAD_SET_CONTROL_CURSOR SIZE W64_H64).
pub const CURSOR_SIDE: u32 = 64;
/// G3: cursor channel (class 0xc67a, PIO) user area of head `h` in BAR0.
const fn curs_user(head: u32) -> usize {
    0x006d_8000 + head as usize * 0x1000
}

// --- видеопамять через BAR1 ---------------------------------------------------

#[derive(Clone, Copy)]
struct Vram {
    /// Виртуальный адрес, под которым видна видеопамять с `VRAM_MAP_START`.
    base: usize,
}

impl Vram {
    fn ptr(self, at: u64) -> *mut u32 {
        (self.base + (at - VRAM_MAP_START) as usize) as *mut u32
    }
    fn w32(self, at: u64, v: u32) {
        // SAFETY: адрес внутри отображённого куска BAR1.
        unsafe { self.ptr(at).write_volatile(v) }
    }
    fn r32(self, at: u64) -> u32 {
        // SAFETY: см. `w32`.
        unsafe { self.ptr(at).read_volatile() }
    }
    fn zero(self, at: u64, bytes: u64) {
        for i in (0..bytes).step_by(4) {
            self.w32(at + i, 0);
        }
    }
}

/// Состояние дисплея: что создано на карте.
struct Disp {
    vram: Vram,
    core: Channel,
    windows: [Option<Channel>; 8],
    window_mask: u32,
    owners_sent: bool,
    used_sors: u8,
    gpu_max_rate: u32,
    increased_wm: bool,
    /// Головки, которые уже наши: (головка, выход, режим).
    heads: Vec<(u32, u32, edid::Mode)>,
    /// Основной монитор: тот, что зажгла прошивка (на нём был стол).
    primary: u32,
    /// G3: BAR0 for the window channel flips outside a modeset.
    bar: Bar0,
    /// G3: per head, the system memory address scanned out instead of its
    /// frame buffer (`None` - the frame buffer).
    flipped: [Option<u64>; 4],
    /// G3: per head, a flip kicked and not on screen yet: what the display
    /// showed before it (still read until the flip latches) and when it was
    /// kicked (ms).
    flip_wait: [Option<(Option<u64>, u64)>; 4],
    /// G3: per head, the cursor channel: 0 not tried, 1 up, 2 failed.
    cursor_chan: [u8; 4],
    /// G3: per head, the cursor is enabled in the core channel.
    cursor_on: [bool; 4],
}

// SAFETY: только адреса окна BAR1 и память буферов команд — владелец один.
unsafe impl Send for Disp {}

static DISP: crate::sync::SpinLock<Option<Disp>> = crate::sync::SpinLock::new(None);

/// Забыть каналы и головки: GSP-RM выгружается (`nvidia_gsp::unload`), и
/// всё, что создано на карте, разбирает он сам вместе с нашим клиентом.
pub(crate) fn forget() {
    if let Some(mut disp) = DISP.try_lock() {
        *disp = None;
    }
}

/// Канал дисплея: буфер команд в памяти машины и его регистры в BAR0.
#[derive(Clone, Copy)]
struct Channel {
    pb: Mem,
    user: usize,
    cur: u32,
    chn: u32,
}

impl Channel {
    const MAX: u32 = 1000;

    fn push(&mut self, method: u32, data: &[u32]) {
        let need = 1 + data.len() as u32;
        if self.cur + need + 2 > Self::MAX {
            // Прыжок в начало (только после того, как карта ушла дальше начала).
            self.pb.w32(self.cur as usize * 4, 0x2000_0000);
            self.cur = 0;
        }
        self.pb.w32(self.cur as usize * 4, ((data.len() as u32) << 18) | method);
        for (i, v) in data.iter().enumerate() {
            self.pb.w32((self.cur as usize + 1 + i) * 4, *v);
        }
        self.cur += need;
    }

    fn kick(&mut self, bar: Bar0) {
        fence(Ordering::SeqCst);
        bar.wr(self.user, self.cur * 4);
    }

    /// Дождаться, пока карта прочитает всё (GET == PUT).
    fn idle(&self, bar: Bar0) -> Res<()> {
        let want = self.cur * 4;
        if bar.wait(2_000_000, |b| b.rd(self.user + 4) == want) {
            return Ok(());
        }
        Err(format!(
            "channel {}: GET {:#x} PUT {:#x} (exception {:#x} {:#x} {:#x})",
            self.chn,
            bar.rd(self.user + 4),
            bar.rd(self.user),
            bar.rd(0x0061_1020 + self.chn as usize * 12),
            bar.rd(0x0061_1024 + self.chn as usize * 12),
            bar.rd(0x0061_1028 + self.chn as usize * 12)
        ))
    }
}

fn u32s(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn put(buf: &mut [u8], at: usize, v: u32) {
    buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// Хеш RAMHT, как его считают RM и железо (`instmemHashFunc_v03_00`).
fn ramht_hash(handle: u32, client: u32, chn: u32) -> u32 {
    ((handle & 0x3ff)
        ^ ((handle >> 10) & 0x3ff)
        ^ ((handle >> 20) & 0x3ff)
        ^ (((client & 0xff) << 2) | (handle >> 30))
        ^ (((chn & 0xf) << 6) | ((client >> 8) & 0x3f))
        ^ ((chn >> 4) & 0x7))
        & 0x3ff
}

/// Хеш nouveau — запись кладётся и туда (см. спецификацию, сомнение 3).
fn ramht_hash_nouveau(handle: u32, chn: u32) -> u32 {
    ((handle & 0x3ff) ^ ((handle >> 10) & 0x3ff) ^ ((handle >> 20) & 0x3ff) ^ (handle >> 30) ^ (chn << 6)) & 0x3ff
}

fn ramht_insert(vram: Vram, handle: u32, chn: u32, object: u32) {
    let context = (H_CLIENT & 0x3fff) | ((object >> 5) << 14) | (chn << 25);
    for start in [ramht_hash(handle, H_CLIENT, chn), ramht_hash_nouveau(handle, chn)] {
        for probe in 0..1024u32 {
            let slot = u64::from((start + probe) & 0x3ff) * 8;
            let (h, c) = (vram.r32(INST + slot), vram.r32(INST + slot + 4));
            if (h, c) == (handle, context) {
                break;
            }
            if h == 0 && c == 0 {
                vram.w32(INST + slot, handle);
                vram.w32(INST + slot + 4, context);
                break;
            }
        }
    }
}

fn ctxdma(vram: Vram, object: u32, flags: u32, start: u64, limit: u64) {
    let at = INST + u64::from(object);
    let words = [flags, (start >> 8) as u32, ((start >> 40) & 0x7f) as u32, (limit >> 8) as u32, ((limit >> 40) & 0x7f) as u32, 0];
    for (i, w) in words.iter().enumerate() {
        vram.w32(at + i as u64 * 4, *w);
    }
}

// --- главное -------------------------------------------------------------------

/// Выставить режим на всех подключённых мониторах DisplayPort.
/// `wanted` — выбор «Параметров»: (выход, ширина, высота, частота мГц).
pub fn modeset(wanted: &[(u32, u32, u32, u32)]) -> Log {
    let mut log = Log::new();
    let mut session = SESSION.lock();
    let Some(session) = session.as_mut() else {
        log.push("display: GSP-RM is not running (gpu gsp first)".into());
        return log;
    };
    let bar = session.bar;
    let internal = (session.internal_client, session.internal_subdevice);
    let mut rm = Rm { queues: &mut session.queues, bar, log: Vec::new() };
    let result = run(&mut rm, &mut log, internal, wanted);
    log.extend(core::mem::take(&mut rm.log));
    if let Err(why) = result {
        log.push(format!("display: stopped - {why}"));
    }
    for line in &log {
        crate::kprintln!("  nvidia-disp : {line}");
    }
    log
}

#[allow(clippy::too_many_lines)]
fn run(rm: &mut Rm, log: &mut Log, internal: (u32, u32), wanted: &[(u32, u32, u32, u32)]) -> Res<()> {
    let (device, _) = super::nvidia::card().ok_or("no NVIDIA card")?;

    // Уже поднят — только новые режимы (смена разрешения из «Параметров»).
    let mut disp = DISP.lock().take();
    if disp.is_none() {
        disp = Some(bring_up(rm, log, internal, device)?);
    }
    let mut disp = disp.ok_or("no display")?;
    let result = light(rm, log, &mut disp, wanted);
    // Созданное на карте не теряется и при ошибке: повторить можно без
    // перезапуска GSP.
    *DISP.lock() = Some(disp);
    result
}

#[allow(clippy::too_many_lines)]
fn light(rm: &mut Rm, log: &mut Log, disp: &mut Disp, wanted: &[(u32, u32, u32, u32)]) -> Res<()> {

    // Подключённые выходы и их протоколы.
    let supported = le32(&rm.disp(0x0073_0107, &u32s(&[0, 0, 0]))?, 4)?;
    let connected = le32(&rm.disp(0x0073_0108, &u32s(&[0, 0, supported, 0]))?, 8)?;
    let mut lit: Vec<(u32, u32)> = Vec::new(); // (голова, выход), зажжённые прошивкой
    for head in 0..4u32 {
        if let Ok(r) = rm.disp(0x0073_010c, &u32s(&[0, head, 0, 0])) {
            let id = le32(&r, 12).unwrap_or(0);
            if id != 0 {
                lit.push((head, id));
            }
        }
    }
    log.push(format!("outputs supported {supported:#x}, connected {connected:#x}, lit by firmware {lit:x?}"));
    if disp.primary == 0 {
        disp.primary = lit.first().map(|(_, id)| *id).unwrap_or(connected & connected.wrapping_neg());
    }

    // Головы: сначала тем, кого зажгла прошивка, — их головы; остальным — свободные.
    let mut plan: Vec<(u32, u32)> = Vec::new();
    for bit in 0..32 {
        let id = 1u32 << bit;
        if connected & id == 0 {
            continue;
        }
        let head = match lit.iter().find(|(_, o)| *o == id) {
            Some((h, _)) => *h,
            None => (0..4u32)
                .find(|h| !lit.iter().any(|(lh, _)| lh == h) && !plan.iter().any(|(ph, _)| ph == h) && disp.window_mask & (1 << (2 * h)) != 0)
                .ok_or("no free head")?,
        };
        plan.push((head, id));
    }
    // Зажжённые прошивкой — первыми: их головы уже светятся, и первое
    // UPDATE должно нести их состояние, а не гасить их (16.11: BenQ погас
    // вместе с неудачным вторым монитором). Их SOR заняты — RM не должен
    // отдать их другому выходу.
    plan.sort_by_key(|(_, id)| !lit.iter().any(|(_, o)| o == id));
    if disp.heads.is_empty() {
        for (_, id) in &lit {
            if let Ok(or) = old_sor(rm, *id) {
                if or < 8 {
                    disp.used_sors |= 1 << or;
                }
            }
        }
    }

    for (head, id) in plan {
        let mut info = u32s(&[0, id]);
        info.resize(0x38, 0);
        let r = rm.disp(0x0073_028b, &info)?;
        let protocol = le32(&r, 16)?;
        if protocol != 8 && protocol != 9 {
            log.push(format!("output {id:#x}: protocol {protocol} (HDMI/DVI) - not done yet, skipped"));
            continue;
        }
        let link = if protocol == 9 { 2 } else { 1 };
        let mut edid = u32s(&[0, id, 2048, 0]);
        edid.resize(16 + 2048, 0);
        let r = rm.disp(0x0073_0245, &edid)?;
        let size = le32(&r, 8)? as usize;
        let mut monitor = edid::parse(r.get(16..16 + size.min(2048)).unwrap_or(&[])).map_err(|e| format!("EDID of {id:#x}: {e:?}"))?;
        // Только режимы, которые пролезают в линию DisplayPort (24 бита на
        // точку, 8b/10b: полезных 8 бит из 10 на каждый символ линии). 17.11:
        // LHC P2410R предлагает 180 Гц (720 МГц), а его DP 1.2 — 4×5,4 Гбит/с,
        // 17,28 Гбит/с полезных: тренировка линии не проходила ни на одной
        // скорости, и монитор гас. Такие режимы «Параметры» больше не видят.
        if let Ok(caps) = dpcd_caps(rm, id) {
            let rate = (u32::from(caps[1]) * 27_000).min(disp.gpu_max_rate);
            let lanes = u64::from(caps[2] & 0x1f).min(4);
            let budget = u64::from(rate) * 8 * lanes;
            let before = monitor.modes.len();
            monitor.modes.retain(|m| u64::from(m.clock_khz) * 24 <= budget);
            if monitor.modes.len() != before {
                log.push(format!(
                    "output {id:#x}: {} mode(s) above the DP link ({rate} kHz x{lanes}) hidden",
                    before - monitor.modes.len()
                ));
            }
        }
        // Без явного выбора — режим, сохранённый «Параметрами» для этого
        // монитора (по паспорту), а если монитор его больше не предлагает или
        // сохранённого нет — родной.
        let mode = match wanted.iter().find(|w| w.0 == id) {
            Some(&(_, w, h, r)) => monitor.find(w, h, r).copied().ok_or(format!("{w}x{h} not offered by the monitor"))?,
            None => match super::outputs::saved_mode(&monitor) {
                Some(saved) => saved,
                None => *monitor.preferred().ok_or("monitor has no modes")?,
            },
        };
        if disp.heads.iter().any(|(h, o, m)| *h == head && *o == id && *m == mode) {
            log.push(format!("head {head}: output {id:#x} already at {}x{}", mode.width, mode.height));
            continue;
        }
        let first = !disp.heads.iter().any(|(h, _, _)| *h == head);
        let was_lit = lit.iter().any(|(h, o)| *h == head && *o == id) || !first;
        log.push(format!(
            "head {head}: output {id:#x} '{}' -> {}x{}@{}.{:03} ({} kHz, {:?})",
            monitor.name,
            mode.width,
            mode.height,
            mode.refresh_mhz() / 1000,
            mode.refresh_mhz() % 1000,
            mode.clock_khz,
            mode.source
        ));
        set_head(rm, log, disp, head, id, link, mode, was_lit)?;
        disp.heads.retain(|(h, _, _)| *h != head);
        disp.heads.push((head, id, mode));
        // Видно на экране: реестр мониторов и стол получают картинку.
        super::outputs::nvidia_lit(id, head, mode, disp.vram.base + (fb(head) - VRAM_MAP_START) as usize, &monitor);
    }
    Ok(())
}

/// Один раз: память экземпляров, корень дисплея, каналы.
#[allow(clippy::too_many_lines)]
fn bring_up(rm: &mut Rm, log: &mut Log, internal: (u32, u32), device: crate::pci::Device) -> Res<Disp> {
    let bar = rm.bar;
    // BAR1 (физический режим) → видеопамять для процессора.
    let bar1 = device.memory_bar(1).ok_or("no BAR1")?.as_u64();
    // Окно общее с NVK (`nvidia_rm_vram`) и отображается один раз: второе
    // `map_active` того же куска отвечает AlreadyMapped (18.11).
    const _: () = assert!(VRAM_MAP_START == super::nvidia_gsp::BAR1_WINDOW_START && VRAM_MAP_END == super::nvidia_gsp::BAR1_WINDOW_END);
    let vram = Vram { base: super::nvidia_gsp::bar1_window(device)? };
    vram.zero(INST, 0x10000);
    vram.zero(SYNC, 0x1000);
    vram.w32(INST + 0x8000, 0x1234_5678);
    let check = vram.r32(INST + 0x8000);
    vram.w32(INST + 0x8000, 0);
    log.push(format!("BAR1 {bar1:#x}: VRAM {INST:#x} reads back {check:#x} (want 0x12345678)"));
    if check != 0x1234_5678 {
        return Err("VRAM is not writable through BAR1".into());
    }

    // Статические сведения дисплея: какие окна есть.
    let r = rm.control(internal.0, internal.1, 0x2080_0a01, &[0u8; 0x24])?;
    let window_mask = le32(&r, 4)?;
    log.push(format!("display static info: windows {window_mask:#x}, heads {}, channels {}", le32(&r, 0xc)?, le32(&r, 0x20)?));

    // Возможности DP карты.
    let mut caps = u32s(&[0, u32::MAX]);
    caps.resize(0x40, 0);
    let (gpu_max_rate, increased_wm) = match rm.disp(0x0073_1369, &caps) {
        Ok(r) => {
            let code = le32(&r, 8)? & 7;
            let rate = match code {
                1 => 162_000,
                2 => 270_000,
                3 => 540_000,
                _ => 810_000,
            };
            (rate, r.get(0x1a).copied().unwrap_or(0) != 0)
        }
        Err(why) => {
            log.push(format!("DP_GET_CAPS - {why}"));
            (540_000, false)
        }
    };

    // Память экземпляров: хеш-таблица и объекты ctxdma.
    let mut wim = Vec::new();
    wim.extend_from_slice(&INST.to_le_bytes());
    wim.extend_from_slice(&0x10000u64.to_le_bytes());
    wim.extend_from_slice(&u32s(&[2, 2]));
    rm.control(internal.0, internal.1, 0x2080_0a49, &wim)?;
    // Флаги как у nouveau (`gv100_dmaobj_new`): 0x40 — страницы «по
    // умолчанию»; без него (было 5) core UPDATE на RTX 3060 не завершался
    // (16.11, первая проверка на ПК).
    ctxdma(vram, OBJ_SYNC, 0x45, SYNC, SYNC + 0xfff);
    ctxdma(vram, OBJ_VRAM, 0x45, 0, VRAM_MAP_END - 1);
    ctxdma(vram, OBJ_FB, 0x45, 0, VRAM_MAP_END - 1);
    // G3 (21.11): system memory (PCI, coherent: target 2 as in nouveau's
    // `gv100_dmaobj_new`) for scanning out a game's buffer directly
    // (`scanout`, opt-in `scanout flip on`). Untested on hardware.
    ctxdma(vram, OBJ_SYS, 0x46, 0, (1u64 << 40) - 1);

    // Ручной DisplayPort: линию тренируем мы (через RM), а не RM сам.
    rm.disp(0x0073_1365, &u32s(&[0]))?;
    rm.alloc(H_DEVICE, H_ROOT, 0xc670, &[])?;
    log.push("display root 0xc670 allocated".into());

    // Канал core.
    let mut core = new_channel(rm, internal, 0xc67d, 0, 0x0068_0000, 0)?;
    ramht_insert(vram, H_SYNC, 0, OBJ_SYNC);
    ramht_insert(vram, H_VRAM, 0, OBJ_VRAM);
    core.push(0x0208, &[H_SYNC]);
    for w in 0..8u32 {
        if window_mask & (1 << w) == 0 {
            continue;
        }
        core.push(0x1004 + w * 0x80, &[0xf]);
        core.push(0x1008 + w * 0x80, &[0]);
        core.push(0x1010 + w * 0x80, &[0x0011_7fff]);
    }
    core.kick(bar);
    core.idle(bar)?;
    log.push(format!("core channel up (GET {:#x})", bar.rd(0x0068_0004)));
    log.push(format!("after core init: {}", diag(bar, vram, &[0])));

    // Окна главных плоскостей головок: 0, 2, 4, 6.
    let mut windows = [None; 8];
    for w in [0u32, 2, 4, 6] {
        if window_mask & (1 << w) == 0 {
            continue;
        }
        let chn = 1 + w;
        match new_channel(rm, internal, 0xc67e, w, 0x0069_0000 + w as usize * 0x1000, chn) {
            Ok(channel) => {
                ramht_insert(vram, H_SYNC, chn, OBJ_SYNC);
                ramht_insert(vram, H_VRAM, chn, OBJ_VRAM);
                ramht_insert(vram, H_FB, chn, OBJ_FB);
                ramht_insert(vram, H_SYS, chn, OBJ_SYS);
                windows[w as usize] = Some(channel);
            }
            Err(why) => log.push(format!("window {w}: {why}")),
        }
    }
    log.push(format!("window channels up: {:?}", windows.iter().enumerate().filter(|(_, c)| c.is_some()).map(|(i, _)| i).collect::<Vec<_>>()));
    Ok(Disp {
        vram,
        core,
        windows,
        window_mask,
        owners_sent: false,
        used_sors: 0,
        gpu_max_rate,
        increased_wm,
        heads: Vec::new(),
        primary: 0,
        bar,
        flipped: [None; 4],
        flip_wait: [None; 4],
        cursor_chan: [0; 4],
        cursor_on: [false; 4],
    })
}

fn new_channel(rm: &mut Rm, internal: (u32, u32), class: u32, instance: u32, user: usize, chn: u32) -> Res<Channel> {
    let pb = Mem::new(1)?;
    let mut p = alloc::vec![0u8; 0x38];
    put(&mut p, 0x00, 1); // память машины
    p[0x08..0x10].copy_from_slice(&pb.phys.to_le_bytes());
    p[0x10..0x18].copy_from_slice(&0xfffu64.to_le_bytes());
    put(&mut p, 0x18, 1); // согласованная
    put(&mut p, 0x1c, class);
    put(&mut p, 0x20, instance);
    p[0x24] = 1;
    put(&mut p, 0x28, 3); // PHYS_PCI_COHERENT
    put(&mut p, 0x2c, 0);
    put(&mut p, 0x30, 1);
    rm.control(internal.0, internal.1, 0x2080_0a58, &p)?;
    let mut a = alloc::vec![0u8; 0x28];
    put(&mut a, 0x00, instance);
    put(&mut a, 0x20, 1);
    rm.alloc(H_ROOT, (class << 16) | instance, class, &a)?;
    Ok(Channel { pb, user, cur: 0, chn })
}

/// Возможности приёмника DisplayPort: первые 15 байт DPCD, а если приёмник
/// объявил расширенный блок (бит 7 байта `0x000E`), — они же из `0x2200`.
///
/// 17.11, LHC P2410R: в старом поле монитор пишет «DP 1.2, 5,4 Гбит/с», а
/// настоящие возможности (DP 1.4, 8,1 Гбит/с на линию) — только в `0x2200`;
/// так делают мониторы DP 1.4 ради старых источников. Без этого 180 Гц при
/// 2560x1440 (720 МГц) не помещались в линию, хотя в Windows работают.
fn dpcd_caps(rm: &mut Rm, id: u32) -> Res<Vec<u8>> {
    let base = aux(rm, id, 9, 0, &[], 15)?;
    if base.get(0x0e).is_some_and(|b| b & 0x80 != 0) {
        if let Ok(ext) = aux(rm, id, 9, 0x2200, &[], 15) {
            if ext.len() >= 4 && ext[0] >= base[0] {
                return Ok(ext);
            }
        }
    }
    Ok(base)
}

/// AUX DisplayPort: прочитать (`cmd` 9) или записать (8) до 16 байт DPCD.
fn aux(rm: &mut Rm, id: u32, cmd: u32, addr: u32, data: &[u8], len: usize) -> Res<Vec<u8>> {
    let mut p = alloc::vec![0u8; 0x30];
    put(&mut p, 0x04, id);
    p[0x08] = u8::from(len == 0);
    put(&mut p, 0x0c, cmd);
    put(&mut p, 0x10, addr);
    p[0x14..0x14 + data.len()].copy_from_slice(data);
    put(&mut p, 0x24, len.saturating_sub(1) as u32);
    for _ in 0..3 {
        match rm.disp(0x0073_1341, &p) {
            Ok(r) => {
                let reply = le32(&r, 0x28)?;
                if reply != 0 {
                    return Err(format!("AUX {addr:#x}: reply {reply}"));
                }
                return Ok(r[0x14..0x14 + len.min(16)].to_vec());
            }
            Err(why) if why.contains("status 0x3") => crate::sched::sleep_ms(10),
            Err(why) => return Err(why),
        }
    }
    Err(format!("AUX {addr:#x}: busy"))
}

/// Слепок состояния дисплея для журнала: указатели каналов, исключения,
/// супервизор, уведомитель через BAR1 и через PRAMIN (видит ли дисплей то же,
/// что процессор). Только чтение — подтверждать прерывания будет GSP-RM.
fn diag(bar: Bar0, vram: Vram, heads: &[u32]) -> String {
    let mut out = String::new();
    let _ = write!(out, "core PUT {:#x} GET {:#x}", bar.rd(0x0068_0000), bar.rd(0x0068_0004));
    for w in [0u32, 2, 4, 6] {
        let at = 0x0069_0000 + w as usize * 0x1000;
        let _ = write!(out, " w{w} {:#x}/{:#x}", bar.rd(at), bar.rd(at + 4));
    }
    for chid in [0usize, 1, 3] {
        let _ = write!(
            out,
            " | ch{chid} ctl {:#x} pb {:#x} {:#x} {:#x} {:#x} exc {:#x} {:#x} {:#x}",
            bar.rd(0x0061_04e0 + chid * 4),
            bar.rd(0x0061_0b20 + chid * 16),
            bar.rd(0x0061_0b24 + chid * 16),
            bar.rd(0x0061_0b28 + chid * 16),
            bar.rd(0x0061_0b2c + chid * 16),
            bar.rd(0x0061_1020 + chid * 12),
            bar.rd(0x0061_1024 + chid * 12),
            bar.rd(0x0061_1028 + chid * 12),
        );
    }
    let _ = write!(
        out,
        " | idle core {:#x} win {:#x} | intr {:#x} {:#x} {:#x} {:#x} {:#x} detail {:#x} | sv {:#x}",
        bar.rd(0x0061_0630),
        bar.rd(0x0061_0664),
        bar.rd(0x0061_1ec0),
        bar.rd(0x0061_184c),
        bar.rd(0x0061_1850),
        bar.rd(0x0061_1854),
        bar.rd(0x0061_1c30),
        bar.rd(0x0061_1848),
        bar.rd(0x0061_07a8),
    );
    let _ = write!(out, " | inst {:#x} {:#x}", bar.rd(0x0061_0010), bar.rd(0x0061_0014));
    for &h in heads {
        let h = h as usize;
        let _ = write!(
            out,
            " | head{h} sv {:#x} stat {:#x} {:#x} armed-ctl {:#x} {:#x} asy {:#x}",
            bar.rd(0x0061_07ac + h * 4),
            bar.rd(0x0061_1800 + h * 4),
            bar.rd(0x0061_2078 + h * 0x800),
            bar.rd(0x0068_8000 + 0x2004 + h * 0x400),
            bar.rd(0x0068_8000 + 0x2064 + h * 0x400),
            bar.rd(0x0068_0000 + 0x2064 + h * 0x400),
        );
    }
    // Уведомитель и начало RAMHT глазами PRAMIN (окно 64 КиБ по 0x1700).
    let saved = bar.rd(0x1700);
    bar.wr(0x1700, (SYNC >> 16) as u32);
    let ntfy = [bar.rd(0x0070_0000 + (SYNC & 0xffff) as usize), bar.rd(0x0070_0004 + (SYNC & 0xffff) as usize)];
    bar.wr(0x1700, (INST >> 16) as u32);
    let ramht = [bar.rd(0x0070_0000 + (INST & 0xffff) as usize), bar.rd(0x0070_0004 + (INST & 0xffff) as usize)];
    bar.wr(0x1700, saved);
    let _ = write!(
        out,
        " | ntfy bar1 {:#x} {:#x} pramin {:#x} {:#x} | inst0 bar1 {:#x} pramin {:#x}",
        vram.r32(SYNC),
        vram.r32(SYNC + 4),
        ntfy[0],
        ntfy[1],
        vram.r32(INST),
        ramht[0]
    );
    let _ = ramht[1];
    out
}

/// Обновление core с уведомлением; `windows` — окна, сблокированные с ним.
fn core_update(disp: &mut Disp, bar: Bar0, windows: u32) -> Res<()> {
    for i in 0..4 {
        disp.vram.w32(SYNC + i * 4, 0);
    }
    disp.core.push(0x020c, &[0x0000_1000]);
    disp.core.push(0x0218, &[0, windows]);
    disp.core.push(0x0200, &[1]);
    disp.core.push(0x020c, &[0]);
    disp.core.kick(bar);
    let vram = disp.vram;
    if !bar.wait(2_000_000, |_| vram.r32(SYNC) >> 30 == 2) {
        let idle = disp.core.idle(bar).err().unwrap_or_default();
        let heads: Vec<u32> = (0..4).collect();
        return Err(format!("core UPDATE did not finish (notifier {:#x}, {idle}) | {}", vram.r32(SYNC), diag(bar, vram, &heads)));
    }
    disp.core.idle(bar)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn set_head(rm: &mut Rm, log: &mut Log, disp: &mut Disp, head: u32, id: u32, link: u32, mode: edid::Mode, was_lit: bool) -> Res<()> {
    let bar = rm.bar;
    let hb = head * 0x400;
    let w = 2 * head;
    if disp.windows[w as usize].is_none() {
        return Err(format!("no window channel {w} for head {head}"));
    }

    // DPCD: возможности приёмника (с расширенным блоком DP 1.4).
    let dpcd = dpcd_caps(rm, id)?;
    let (max_rate, max_lanes, ef) = (u32::from(dpcd[1]) * 27_000, u32::from(dpcd[2] & 0x1f).min(4), dpcd[2] & 0x80 != 0);
    log.push(format!("DPCD rev {:#x}, max {} kHz x{}, enhanced framing {ef}", dpcd[0], max_rate, max_lanes));

    // C1: погасить то, что зажгла прошивка (или наш прежний режим).
    if was_lit {
        let power = aux(rm, id, 9, 0x600, &[], 1)?;
        let _ = aux(rm, id, 8, 0x600, &[(power[0] & !7) | 2], 1);
        let or = old_sor(rm, id)?;
        disp.core.push(0x2020 + hb, &[0]);
        disp.core.push(0x0300 + or * 0x20, &[0]);
        core_update(disp, bar, 0)?;
        let mut p = u32s(&[0, id, 0x1 | 0x2 | (1 << 13) | if ef { 0x80 } else { 0 }, 0x14 << 8, 0, 0, 0]);
        p.resize(0x1c, 0);
        let _ = rm.disp(0x0073_1343, &p);
        disp.used_sors &= !(1 << or);
        log.push(format!("C1: head {head} released (SOR {or})"));
    }

    // C2: SOR, линия, поток, головка.
    let mut a = alloc::vec![0u8; 0x50];
    put(&mut a, 0x04, id);
    a[0x08] = disp.used_sors;
    let r = rm.disp(0x0073_1152, &a)?;
    let mut or = None;
    for i in 0..4 {
        if le32(&r, 0x28 + i * 8)? & id != 0 {
            or = Some(i as u32);
        }
    }
    let or = or.ok_or("ASSIGN_SOR gave no SOR")?;
    disp.used_sors |= 1 << or;
    let power = aux(rm, id, 9, 0x600, &[], 1)?;
    if power[0] & 7 != 1 {
        let _ = aux(rm, id, 8, 0x600, &[(power[0] & !7) | 1], 1);
    }
    let needed = u64::from(mode.clock_khz) * 24 / 8;
    let rates: Vec<u32> = [810_000u32, 540_000, 270_000, 162_000].into_iter().filter(|r| *r <= max_rate.min(disp.gpu_max_rate)).collect();
    let mut trained = None;
    let mut lanes = max_lanes;
    'outer: while lanes >= 1 {
        for rate in &rates {
            if u64::from(*rate) * u64::from(lanes) < needed {
                continue;
            }
            let mut cmd = 0x1 | 0x2 | (1 << 13);
            if ef {
                cmd |= 0x80;
            }
            if dpcd[2] & 0x20 != 0 && dpcd[3] & 0x80 == 0 {
                cmd |= 1 << 10;
            }
            let mut p = u32s(&[0, id, cmd, lanes | ((rate / 27_000) << 8), 0, 0, 0]);
            p.resize(0x1c, 0);
            match rm.disp(0x0073_1343, &p) {
                Ok(r) if le32(&r, 0x10).unwrap_or(1) == 0 => {
                    trained = Some((lanes, *rate));
                    break 'outer;
                }
                Ok(r) => log.push(format!("DP train x{lanes} {rate}: err {:#x}", le32(&r, 0x10).unwrap_or(0))),
                Err(why) => log.push(format!("DP train x{lanes} {rate}: {why}")),
            }
        }
        lanes /= 2;
    }
    let (lanes, rate) = trained.ok_or("DP link training failed at every rate")?;
    log.push(format!("C2: SOR {or}, link trained x{lanes} at {rate} kHz"));

    // Поток SST: watermark и символы гашения (формулы nouveau).
    let (wm, hsym, vsym) = watermark(mode, lanes, rate, ef, disp.increased_wm).ok_or("mode does not fit the link")?;
    let mut s = alloc::vec![0u8; 0x54];
    put(&mut s, 0x04, head);
    put(&mut s, 0x08, or);
    put(&mut s, 0x0c, u32::from(link == 2));
    s[0x10] = 1;
    put(&mut s, 0x18, hsym);
    put(&mut s, 0x1c, vsym);
    s[0x44] = u8::from(ef);
    put(&mut s, 0x48, 64);
    put(&mut s, 0x4c, wm);
    rm.disp(0x0073_1362, &s)?;

    // Головка: растр по EDID.
    let hsync_start = mode.h_sync_start;
    let vsync_start = mode.v_sync_start;
    let h_synce = mode.h_sync_end - hsync_start - 1;
    let h_blanke = mode.h_total - hsync_start - 1;
    let h_blanks = h_blanke + mode.width;
    let v_synce = mode.v_sync_end - vsync_start - 1;
    let v_blanke = mode.v_total - vsync_start - 1;
    let v_blanks = v_blanke + mode.height;
    let size = mode.width | (mode.height << 16);
    let hz = mode.clock_khz * 1000;
    let polarity = (u32::from(!mode.h_sync_positive) << 2) | (u32::from(!mode.v_sync_positive) << 3);
    disp.core.push(0x2020 + hb, &[id]);
    disp.core.push(0x0300 + or * 0x20, &[(1 << head) | (if link == 2 { 9 } else { 8 } << 8)]);
    disp.core.push(0x204c + hb, &[size]);
    disp.core.push(0x2058 + hb, &[size]);
    disp.core.push(
        0x2064 + hb,
        &[mode.v_total << 16 | mode.h_total, v_synce << 16 | h_synce, v_blanke << 16 | h_blanke, v_blanks << 16 | h_blanks],
    );
    disp.core.push(0x2074 + hb, &[1]);
    disp.core.push(0x2008 + hb, &[0]);
    disp.core.push(0x200c + hb, &[hz]);
    disp.core.push(0x2028 + hb, &[hz]);
    disp.core.push(0x2030 + hb, &[0x0000_1114]);
    disp.core.push(0x2018 + hb, &[0x10]);
    disp.core.push(0x2000 + hb, &[0]);
    disp.core.push(0x2004 + hb, &[0xfc00_0040 | polarity]);
    if !disp.owners_sent {
        for win in 0..8u32 {
            if disp.window_mask & (1 << win) != 0 {
                disp.core.push(0x1000 + win * 0x80, &[win >> 1]);
            }
        }
        disp.owners_sent = true;
    }
    // G3: a cursor left on by a flipped output goes off with the modeset.
    if core::mem::take(&mut disp.cursor_on[head as usize]) {
        disp.core.push(0x209c + hb, &[0xcf]);
        disp.core.push(0x2088 + hb, &[0]);
    }
    core_update(disp, bar, 0)?;
    log.push(format!("C2: head {head} raster {}x{} up ({:#x} armed)", mode.h_total, mode.v_total, bar.rd(0x0068_a064 + hb as usize)));

    // C3: таблицы цвета, окно с картинкой.
    load_lut(disp.vram, olut(head), false);
    load_lut(disp.vram, ilut(head), true);
    disp.core.push(0x2280 + hb, &[0x0004_0509, u32::MAX, H_VRAM, (olut(head) >> 8) as u32]);
    let pitch = (mode.width * 4).div_ceil(64) * 64;
    // Картинка — тёмно-синяя до первого кадра стола. Wake from screen off
    // (22.11) keeps it: the desktop repaints it, a blue flash would flicker.
    if !KEEP_FB.load(Ordering::Relaxed) {
        for y in 0..mode.height {
            let row = fb(head) + u64::from(y) * u64::from(pitch);
            for x in (0..mode.width).step_by(1) {
                disp.vram.w32(row + u64::from(x) * 4, 0x0010_1828);
            }
        }
    }
    let window = disp.windows[w as usize].as_mut().ok_or("window channel gone")?;
    window.push(0x0308, &[1]);
    window.push(0x0224, &[size, 0, 0xcf, pitch >> 6]);
    window.push(0x0240, &[H_FB]);
    window.push(0x0260, &[(fb(head) >> 8) as u32]);
    window.push(0x0290, &[0]);
    window.push(0x0298, &[size]);
    window.push(0x02a4, &[size]);
    window.push(0x0440, &[0x0004_0508, H_VRAM, (ilut(head) >> 8) as u32]);
    window.push(0x02ec, &[0xff0, 0xff, 0x4422, 0xffff_0000, 0xffff_0000, 0xffff_0000, 0xffff_0000]);
    window.push(0x0370, &[1, 1 << w]);
    window.push(0x0200, &[1]);
    window.kick(bar);
    let window = *window;
    core_update(disp, bar, 1 << w)?;
    window.idle(bar)?;
    if let Some(slot) = disp.flipped.get_mut(head as usize) {
        *slot = None;
    }
    // The modeset showed the frame buffer: no flip pending.
    if let Some(slot) = disp.flip_wait.get_mut(head as usize) {
        *slot = None;
    }
    log.push(format!("C3: head {head} shows {}x{} from VRAM {:#x} (pitch {pitch})", mode.width, mode.height, fb(head)));
    Ok(())
}

/// SOR, которым выход светится сейчас (`OR_GET_INFO`).
fn old_sor(rm: &mut Rm, id: u32) -> Res<u32> {
    let mut info = u32s(&[0, id]);
    info.resize(0x38, 0);
    let r = rm.disp(0x0073_028b, &info)?;
    Ok(le32(&r, 8)? & 3)
}

/// Единичная таблица цвета: 0x20 нулей, 1025 записей по 8 байт.
fn load_lut(vram: Vram, at: u64, fp16: bool) {
    vram.zero(at, 0x20);
    for i in 0..1025u32 {
        let v = (i.min(1023) << 16) >> 10;
        let c = if fp16 { half(v) } else { v };
        vram.w32(at + 0x20 + u64::from(i) * 8, c | (c << 16));
        vram.w32(at + 0x24 + u64::from(i) * 8, c);
    }
}

/// U0.16 → половинная точность (как `fixedU0_16_FP16` nouveau).
fn half(v: u32) -> u32 {
    if v == 0 {
        return 0;
    }
    let p = 31 - v.leading_zeros();
    ((p - 1) << 10) | ((((v << (15 - p)) & 0xffff) << 1 & 0xffc0) >> 6)
}

/// Watermark, символы гашения строки и кадра для SST (`nv50_sor_dp_watermark_sst`).
fn watermark(mode: edid::Mode, lanes: u32, rate_khz: u32, ef: bool, increased: bool) -> Option<(u32, u32, u32)> {
    let min_rate = u64::from(rate_khz) * 1000;
    let pclk = u64::from(mode.clock_khz) * 1000;
    let (depth, pf, tu) = (24u64, 100_000u64, 64u64);
    let lanes64 = u64::from(lanes);
    let (adj, lim) = if increased { (8u64, 22u64) } else { (2, 20) };
    if pclk * depth >= 8 * min_rate * lanes64 {
        return None;
    }
    let ratio = pclk * depth * pf / (8 * min_rate * lanes64);
    if ratio > pf {
        return None;
    }
    let wm_f = ratio * tu * (pf - ratio) / pf;
    let mut wm = adj + (2 * (depth * pf / (8 * lanes64)) + wm_f) / pf;
    let surface = u64::from(mode.width);
    let raster = u64::from(mode.h_total);
    let symbols = surface * depth / (8 * lanes64);
    if wm > 39 || wm > symbols {
        return None;
    }
    wm = wm.max(lim);
    let mut blanking = 3 * 8 * lanes64 + if ef { 3 * 8 * lanes64 } else { 0 } + 3 * 8 * 4;
    let rem = surface % lanes64;
    if rem != 0 {
        blanking += (lanes64 - rem) * depth;
    }
    let clocks = blanking * pf / (8 * lanes64);
    let min_hblank = clocks * pclk / min_rate / pf + 12;
    if min_hblank > raster - surface || surface <= 60 {
        return None;
    }
    let tail = match lanes {
        1 => 9,
        2 => 6,
        _ => 3,
    };
    let h = ((raster - surface - min_hblank) * min_rate / pclk) as i64 - 1 - 3 - tail;
    let vtail = match lanes {
        1 => 39,
        2 => 21,
        _ => 12,
    };
    let v = if surface < 40 { 0 } else { ((surface - 40) * min_rate / pclk) as i64 - 1 - vtail };
    Some((wm as u32, h.max(0) as u32, v.max(0) as u32))
}

// --- screen off (22.11, DPMS-like) ----------------------------------------------

/// Heads put to standby by [`set_power`]: (head, output, mode) to light again.
static ASLEEP: crate::sync::SpinLock<Vec<(u32, u32, edid::Mode)>> = crate::sync::SpinLock::new(Vec::new());
/// [`set_head`] keeps the frame buffer (wake: no dark-blue flash).
static KEEP_FB: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Monitors off (`false`) or on again (`true`), the way nouveau disables and
/// enables a DP output (`nv50_sor_atomic_disable` / `_enable`):
///
/// * off - DPCD `SET_POWER` (0x600) = D3 over AUX, so the sink powers its
///   receiver down instead of hunting for a lost signal; then the head is
///   taken off its SOR (core `HEAD_SET_CONTROL_OUTPUT_RESOURCE` and
///   `SOR_SET_CONTROL` owner 0, UPDATE) and the link released (`DP_CTRL`) -
///   exactly step C1 of a modeset. The monitor sees no signal and sleeps
///   (backlight off), the frame buffer stays in VRAM.
/// * on - D0 (retried: a sleeping sink may NAK the first AUX transfers), then
///   [`set_head`] of the saved mode: SOR, link training, raster, window -
///   without repainting the frame buffer. If that fails - the same with C1
///   first (full modeset of that head).
///
/// `None` - the display is not ours (GSP-RM not running or not set up).
pub fn set_power(on: bool) -> Option<String> {
    let mut session = SESSION.lock();
    let session = session.as_mut()?;
    let bar = session.bar;
    let mut rm = Rm { queues: &mut session.queues, bar, log: Vec::new() };
    let mut log = Log::new();
    let mut guard = DISP.lock();
    let disp = guard.as_mut()?;
    let text = if on { wake(&mut rm, &mut log, disp) } else { sleep(&mut rm, &mut log, disp) };
    drop(guard);
    log.extend(core::mem::take(&mut rm.log));
    for line in &log {
        crate::kprintln!("  nvidia-disp : {line}");
    }
    Some(text)
}

fn sleep(rm: &mut Rm, log: &mut Log, disp: &mut Disp) -> String {
    if !ASLEEP.lock().is_empty() {
        return String::from("nvidia already off");
    }
    let mut asleep = Vec::new();
    let heads = disp.heads.clone();
    for &(head, id, mode) in &heads {
        match sleep_head(rm, disp, head, id) {
            Ok(or) => {
                log.push(format!("power: head {head} output {id:#x} off (DP D3, SOR {or} released)"));
                asleep.push((head, id, mode));
            }
            Err(why) => log.push(format!("power: head {head} output {id:#x} stays lit: {why}")),
        }
    }
    let text = format!("nvidia {} of {} monitor(s) in standby", asleep.len(), heads.len());
    *ASLEEP.lock() = asleep;
    text
}

fn sleep_head(rm: &mut Rm, disp: &mut Disp, head: u32, id: u32) -> Res<u32> {
    let bar = rm.bar;
    let hb = head * 0x400;
    // MAX_LANE_COUNT: bit 7 - enhanced framing (the link release repeats it).
    let ef = aux(rm, id, 9, 0x0002, &[], 1)?.first().is_some_and(|b| b & 0x80 != 0);
    let or = old_sor(rm, id)?;
    let power = aux(rm, id, 9, 0x600, &[], 1)?;
    let _ = aux(rm, id, 8, 0x600, &[(power[0] & !7) | 2], 1);
    disp.core.push(0x2020 + hb, &[0]);
    disp.core.push(0x0300 + or * 0x20, &[0]);
    core_update(disp, bar, 0)?;
    let mut p = u32s(&[0, id, 0x1 | 0x2 | (1 << 13) | if ef { 0x80 } else { 0 }, 0x14 << 8, 0, 0, 0]);
    p.resize(0x1c, 0);
    let _ = rm.disp(0x0073_1343, &p);
    disp.used_sors &= !(1 << or);
    Ok(or)
}

fn wake(rm: &mut Rm, log: &mut Log, disp: &mut Disp) -> String {
    let asleep = core::mem::take(&mut *ASLEEP.lock());
    if asleep.is_empty() {
        return String::from("nvidia: nothing was off");
    }
    let mut lit = 0;
    for &(head, id, mode) in &asleep {
        let mut answered = false;
        for _ in 0..5 {
            if let Ok(power) = aux(rm, id, 9, 0x600, &[], 1) {
                if aux(rm, id, 8, 0x600, &[(power[0] & !7) | 1], 1).is_ok() {
                    answered = true;
                    break;
                }
            }
            crate::sched::sleep_ms(10);
        }
        // DP: the sink may take up to 1 ms to leave D3 (drm_dp_link_power_up).
        crate::sched::sleep_ms(2);
        let link = link_of(rm, id);
        KEEP_FB.store(true, Ordering::Relaxed);
        let result = match set_head(rm, log, disp, head, id, link, mode, false) {
            Ok(()) => Ok(()),
            Err(why) => {
                log.push(format!("power: head {head} quick wake failed ({why}), full modeset of the head"));
                set_head(rm, log, disp, head, id, link, mode, true)
            }
        };
        KEEP_FB.store(false, Ordering::Relaxed);
        match result {
            Ok(()) => {
                lit += 1;
                log.push(format!(
                    "power: head {head} output {id:#x} on again{}",
                    if answered { "" } else { " (the sink did not answer D0 over AUX)" }
                ));
            }
            Err(why) => log.push(format!("power: head {head} output {id:#x} did not come back: {why}")),
        }
    }
    format!("nvidia {lit} of {} monitor(s) back", asleep.len())
}

/// Sublink of a DP output (`OR_GET_INFO` protocol: 9 - DP B).
fn link_of(rm: &mut Rm, id: u32) -> u32 {
    let mut info = u32s(&[0, id]);
    info.resize(0x38, 0);
    match rm.disp(0x0073_028b, &info).ok().and_then(|r| le32(&r, 16).ok()) {
        Some(9) => 2,
        _ => 1,
    }
}

/// Для журнала «Параметров».
#[must_use]
pub fn describe() -> String {
    DISP.lock().as_ref().map_or_else(
        || String::from("not set up"),
        |d| format!("{} head(s): {:?}", d.heads.len(), d.heads.iter().map(|(h, o, m)| (*h, *o, m.width, m.height)).collect::<Vec<_>>()),
    )
}

/// Смена режима основного монитора (путь «Параметров» и стола): выставить
/// `width`×`height` (частота `refresh_mhz`, 0 — какую предложит монитор) и
/// отдать картинку, в которую стол будет рисовать.
pub fn set_primary(width: u32, height: u32, refresh_mhz: u32) -> Result<boot_info::Framebuffer, super::DisplayError> {
    let primary = DISP.lock().as_ref().map_or(0, |d| d.primary);
    let wanted: Vec<(u32, u32, u32, u32)> = if primary != 0 { alloc::vec![(primary, width, height, refresh_mhz)] } else { Vec::new() };
    let log = modeset(&wanted);
    let disp = DISP.lock();
    let disp = disp.as_ref().ok_or(super::DisplayError::Refused("NVIDIA display did not come up"))?;
    let (head, _, mode) = disp
        .heads
        .iter()
        .find(|(_, id, _)| *id == disp.primary)
        .copied()
        .ok_or(super::DisplayError::Refused("the primary monitor is not lit"))?;
    if mode.width != width || mode.height != height {
        crate::kprintln!("  nvidia-disp : asked {width}x{height}, monitor runs {}x{} ({} log lines)", mode.width, mode.height, log.len());
    }
    let pitch = (mode.width * 4).div_ceil(64) * 64;
    Ok(boot_info::Framebuffer {
        base: (disp.vram.base + (fb(head) - VRAM_MAP_START) as usize) as u64,
        size: u64::from(pitch) * u64::from(mode.height),
        width: mode.width,
        height: mode.height,
        stride: pitch / 4,
        format: boot_info::PixelFormat::Bgr,
    })
}

/// G3 (21.11): scan out `phys` (physically contiguous system memory,
/// `pitch` bytes per row, the head's size and format) on the head of output
/// `id` instead of its frame buffer - a page flip of the window channel, no
/// copy; `None` - back to the frame buffer. Never waits:
/// [`super::FlipError::Pending`] - the previous flip is not on screen yet
/// (its window notifier still says NOT_BEGUN) or the channel has not read it,
/// try next frame; [`super::FlipError::Refused`] - copy instead. A flip the
/// display does not take in [`FLIP_TIMEOUT_MS`] stops flips
/// ([`FLIP_STOPPED`]).
///
/// The flip latches at the next vblank (window `SET_PRESENT_CONTROL`
/// NON_TEARING, set by the modeset); its notifier (`NV_DISP_NOTIFIER` at
/// [`FLIP_NTFY`]) leaves NOT_BEGUN when the new surface is in use - only
/// then is the buffer shown before free for the client ([`scanning`]).
pub fn scanout(id: u32, phys: Option<u64>, pitch: u32) -> Result<(), super::FlipError> {
    use super::FlipError::{Pending, Refused};
    let mut guard = DISP.try_lock().ok_or(Pending("display busy"))?;
    let disp = guard.as_mut().ok_or_else(|| Refused("display not set up".into()))?;
    let (head, _, mode) = disp.heads.iter().find(|(_, o, _)| *o == id).copied().ok_or_else(|| Refused("output not lit".into()))?;
    let slot = head as usize;
    poll_flip(disp, slot, mode)?;
    if disp.flipped.get(slot).copied().flatten() == phys {
        return Ok(());
    }
    let fb_pitch = (mode.width * 4).div_ceil(64) * 64;
    if phys.is_some() && (pitch != fb_pitch || pitch % 64 != 0) {
        return Err(Refused(format!("rows of {pitch} bytes, the head scans {fb_pitch} (multiple of 64)")));
    }
    if phys.is_some_and(|addr| addr % 256 != 0) {
        return Err(Refused("the buffer does not start on 256 bytes".into()));
    }
    let bar = disp.bar;
    let vram = disp.vram;
    let timed = FLIP_TIMED.load(Ordering::Relaxed);
    let window = disp.windows.get_mut(2 * slot).and_then(Option::as_mut).ok_or_else(|| Refused("no window channel".into()))?;
    // The previous update must have been read (GET == PUT), else next frame.
    if bar.rd(window.user + 4) != window.cur * 4 {
        return Err(Pending("window channel busy"));
    }
    let (ctx, at) = match phys {
        Some(addr) => (H_SYS, addr),
        None => (H_FB, fb(head)),
    };
    let ntfy = FLIP_NTFY + slot as u64 * 0x10;
    if timed {
        window.push(0x021c, &[0]);
    } else {
        for i in 0..4 {
            vram.w32(ntfy + i * 4, 0);
        }
        // SET_CONTEXT_DMA_NOTIFIER, SET_NOTIFIER_CONTROL (MODE_WRITE,
        // OFFSET 11:4 = byte offset in the SYNC ctxdma).
        window.push(0x021c, &[H_SYNC]);
        window.push(0x0220, &[((ntfy - SYNC) as u32) & 0xff0]);
    }
    window.push(0x0240, &[ctx]);
    window.push(0x0260, &[(at >> 8) as u32]);
    // No interlock with core: a plain flip (the modeset left core
    // interlock flags set - an UPDATE would wait for a core update).
    window.push(0x0370, &[0, 0]);
    window.push(0x0200, &[1]);
    window.kick(bar);
    disp.flip_wait[slot] = Some((disp.flipped[slot], crate::time::uptime_ms()));
    disp.flipped[slot] = phys;
    ANY_FLIP.store(true, Ordering::Relaxed);
    Ok(())
}

/// G3: has the last flip of head `slot` reached the screen? `Ok` - yes (or
/// none pending), `Pending` - not yet, `Refused` - not in
/// [`FLIP_TIMEOUT_MS`]: flips stop ([`FLIP_STOPPED`]) and the display is
/// taken as showing the new surface from now on.
fn poll_flip(disp: &mut Disp, slot: usize, mode: edid::Mode) -> Result<(), super::FlipError> {
    let Some((_, kicked)) = disp.flip_wait.get(slot).copied().flatten() else {
        return Ok(());
    };
    let now = crate::time::uptime_ms();
    let elapsed = now.saturating_sub(kicked);
    let done = if FLIP_TIMED.load(Ordering::Relaxed) {
        // Two refresh periods: the flip latched at one of the vblanks.
        let frame_ms = 1_000_000 / u64::from(mode.refresh_mhz().max(1)) + 1;
        elapsed >= 2 * frame_ms
    } else {
        disp.vram.r32(FLIP_NTFY + slot as u64 * 0x10) >> 30 != 0
    };
    if done {
        disp.flip_wait[slot] = None;
        return Ok(());
    }
    if elapsed < FLIP_TIMEOUT_MS {
        return Err(super::FlipError::Pending("previous flip not on screen yet"));
    }
    disp.flip_wait[slot] = None;
    FLIP_STOPPED.store(true, Ordering::Relaxed);
    let chn = 1 + 2 * slot;
    let window = disp.windows.get(2 * slot).copied().flatten();
    let why = format!(
        "head {slot}: flip not on screen after {elapsed} ms (notifier {:#x}, GET {:#x} PUT {:#x}, exception {:#x} {:#x} {:#x}) - flips stopped (`wldirect flip timed` skips the notifier)",
        disp.vram.r32(FLIP_NTFY + slot as u64 * 0x10),
        window.map_or(0, |w| disp.bar.rd(w.user + 4)),
        window.map_or(0, |w| disp.bar.rd(w.user)),
        disp.bar.rd(0x0061_1020 + chn * 12),
        disp.bar.rd(0x0061_1024 + chn * 12),
        disp.bar.rd(0x0061_1028 + chn * 12),
    );
    crate::kprintln!("  nvidia-disp : {why}");
    Err(super::FlipError::Refused(why))
}

/// G3: may the display still read `[start, start + len)` of system memory -
/// a head scans it out, or showed it before a flip that is not on screen
/// yet? The Wayland server keeps such a client buffer (no `release`) until
/// this says `false`. A busy display lock answers `true` (ask again).
#[must_use]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn scanning(start: u64, len: u64) -> bool {
    if !ANY_FLIP.load(Ordering::Relaxed) {
        return false;
    }
    let Some(mut guard) = DISP.try_lock() else {
        return true;
    };
    let Some(disp) = guard.as_mut() else {
        return false;
    };
    let inside = |addr: Option<u64>| addr.is_some_and(|addr| addr >= start && addr < start.saturating_add(len));
    let heads: Vec<(u32, edid::Mode)> = disp.heads.iter().map(|(h, _, m)| (*h, *m)).collect();
    let mut hit = false;
    for (head, mode) in heads {
        let slot = head as usize;
        if slot >= 4 {
            continue;
        }
        let _ = poll_flip(disp, slot, mode);
        hit |= inside(disp.flipped[slot]) || disp.flip_wait[slot].is_some_and(|(before, _)| inside(before));
    }
    if !disp.flipped.iter().any(Option::is_some) && !disp.flip_wait.iter().any(Option::is_some) {
        ANY_FLIP.store(false, Ordering::Relaxed);
    }
    hit
}

/// G3: see [`ANY_FLIP`].
#[must_use]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn flip_any() -> bool {
    ANY_FLIP.load(Ordering::Relaxed)
}

/// G3: hardware cursor of the head of output `id` (over a flipped output the
/// desktop does not compose, so the pointer is the display's): `image` -
/// new picture ([`CURSOR_SIDE`]² points `0xAARRGGBB`, straight alpha; all
/// zero hides it without a core update), `None` - keep the picture; `at` -
/// its top-left corner in the head's coordinates. The first call allocates
/// the cursor channel (class 0xc67a, PIO) and enables the cursor with one
/// core UPDATE. `Err(Some(why))` - no cursor (the caller composes the
/// pointer), `Err(None)` - a lock is busy, try again next frame.
pub fn cursor_set(id: u32, image: Option<&[u32]>, at: (i32, i32)) -> Result<(), Option<String>> {
    // Busy locks are `Err(None)`: try again next frame.
    let head = {
        let guard = DISP.try_lock().ok_or(None)?;
        let disp = guard.as_ref().ok_or_else(|| Some(String::from("display not set up")))?;
        let (head, _, _) = disp.heads.iter().find(|(_, o, _)| *o == id).copied().ok_or_else(|| Some(String::from("output not lit")))?;
        if head >= 4 {
            return Err(Some("no such head".into()));
        }
        match disp.cursor_chan[head as usize] {
            2 => return Err(Some("no cursor channel".into())),
            1 => None,
            _ => Some(head),
        }
    };
    // The channel once (lock order as in `modeset`: the RM session, then
    // the display) - the RM session is not needed after that.
    if let Some(head) = head {
        let mut session = SESSION.try_lock().ok_or(None)?;
        let session = session.as_mut().ok_or_else(|| Some(String::from("GSP-RM is not running")))?;
        let mut guard = DISP.try_lock().ok_or(None)?;
        let disp = guard.as_mut().ok_or_else(|| Some(String::from("display not set up")))?;
        if disp.cursor_chan[head as usize] == 0 {
            let internal = (session.internal_client, session.internal_subdevice);
            let mut rm = Rm { queues: &mut session.queues, bar: disp.bar, log: Vec::new() };
            match cursor_channel(&mut rm, internal, head) {
                Ok(()) => {
                    disp.cursor_chan[head as usize] = 1;
                    crate::kprintln!("  nvidia-disp : head {head}: cursor channel 0xc67a up");
                }
                Err(why) => {
                    disp.cursor_chan[head as usize] = 2;
                    crate::kprintln!("  nvidia-disp : head {head}: no cursor channel - {why}");
                    return Err(Some(why));
                }
            }
        }
    }
    let mut guard = DISP.try_lock().ok_or(None)?;
    let disp = guard.as_mut().ok_or_else(|| Some(String::from("display not set up")))?;
    let (head, _, _) = disp.heads.iter().find(|(_, o, _)| *o == id).copied().ok_or_else(|| Some(String::from("output not lit")))?;
    let slot = head as usize;
    let bar = disp.bar;
    if let Some(image) = image {
        let points = (CURSOR_SIDE * CURSOR_SIDE) as usize;
        if image.len() != points {
            return Err(Some("cursor image of a wrong size".into()));
        }
        let base = curs(head);
        for (i, &argb) in image.iter().enumerate() {
            // Premultiplied for the BLEND composition (K1 x cursor +
            // (1 - K1 x alpha) x viewport, as nouveau sets it).
            let a = argb >> 24;
            let mul = |c: u32| ((c & 0xff) * a + 127) / 255;
            let pm = (a << 24) | (mul(argb >> 16) << 16) | (mul(argb >> 8) << 8) | mul(argb);
            disp.vram.w32(base + i as u64 * 4, pm);
        }
    }
    if !disp.cursor_on[slot] {
        let hb = head * 0x400;
        // HEAD_SET_CONTEXT_DMA_CURSOR(0), HEAD_SET_OFFSET_CURSOR(0),
        // HEAD_SET_CONTROL_CURSOR (ENABLE, A8R8G8B8, W64_H64, hot spot 0 -
        // the position is the top-left corner), ..._COMPOSITION (K1 0xff,
        // cursor factor K1, viewport factor NEG_K1_TIMES_SRC, BLEND).
        disp.core.push(0x2088 + hb, &[H_VRAM]);
        disp.core.push(0x2090 + hb, &[(curs(head) >> 8) as u32]);
        disp.core.push(0x209c + hb, &[(1 << 31) | (1 << 8) | 0xcf]);
        disp.core.push(0x20a0 + hb, &[0x0000_72ff]);
        core_update(disp, bar, 0).map_err(|why| {
            disp.cursor_chan[slot] = 2;
            Some(format!("cursor enable: {why}"))
        })?;
        disp.cursor_on[slot] = true;
    }
    // SET_CURSOR_HOT_SPOT_POINT_OUT(0) and UPDATE through the PIO channel,
    // when it has room (FREE.COUNT, as nouveau's `curs507a_space`).
    let user = curs_user(head);
    if bar.rd(user + 0x8) & 0x3f < 4 {
        return Ok(());
    }
    let (x, y) = at;
    bar.wr(user + 0x208, (x as u32 & 0xffff) | ((y as u32 & 0xffff) << 16));
    bar.wr(user + 0x200, 1);
    Ok(())
}

/// G3: turn the hardware cursor of output `id` off (the flip ended - the
/// desktop draws the pointer into the frame buffer again).
pub fn cursor_off(id: u32) {
    let Some(mut guard) = DISP.try_lock() else {
        return;
    };
    let Some(disp) = guard.as_mut() else {
        return;
    };
    let Some((head, _, _)) = disp.heads.iter().find(|(_, o, _)| *o == id).copied() else {
        return;
    };
    if !disp.cursor_on.get(head as usize).copied().unwrap_or(false) {
        return;
    }
    let hb = head * 0x400;
    disp.core.push(0x209c + hb, &[0xcf]);
    disp.core.push(0x2088 + hb, &[0]);
    let bar = disp.bar;
    if let Err(why) = core_update(disp, bar, 0) {
        crate::kprintln!("  nvidia-disp : head {head}: cursor off - {why}");
    }
    disp.cursor_on[head as usize] = false;
}

/// G3: the cursor immediate channel of `head` (nouveau `r535_curs_init`):
/// the push buffer control with `valid` 0 (PIO, no push buffer), then the
/// object `(0xc67a << 16) | head` with NV50VAIO_CHANNELPIO_ALLOCATION_PARAMETERS.
fn cursor_channel(rm: &mut Rm, internal: (u32, u32), head: u32) -> Res<()> {
    let mut p = alloc::vec![0u8; 0x38];
    put(&mut p, 0x1c, 0xc67a);
    put(&mut p, 0x20, head);
    p[0x24] = 0;
    put(&mut p, 0x30, 1);
    rm.control(internal.0, internal.1, 0x2080_0a58, &p)?;
    let mut a = alloc::vec![0u8; 0x10];
    put(&mut a, 0x00, head);
    rm.alloc(H_ROOT, (0xc67a << 16) | head, 0xc67a, &a)?;
    Ok(())
}

/// Размер картинки основного монитора, если он зажжён.
#[must_use]
pub fn primary_size() -> Option<(u32, u32)> {
    let disp = DISP.lock();
    let disp = disp.as_ref()?;
    disp.heads.iter().find(|(_, id, _)| *id == disp.primary).map(|(_, _, m)| (m.width, m.height))
}
