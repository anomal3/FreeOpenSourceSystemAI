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
/// Объекты ctxdma в памяти экземпляров.
const OBJ_SYNC: u32 = 0x2000;
const OBJ_VRAM: u32 = 0x2020;
const OBJ_FB: u32 = 0x2040;

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
                windows[w as usize] = Some(channel);
            }
            Err(why) => log.push(format!("window {w}: {why}")),
        }
    }
    log.push(format!("window channels up: {:?}", windows.iter().enumerate().filter(|(_, c)| c.is_some()).map(|(i, _)| i).collect::<Vec<_>>()));
    Ok(Disp { vram, core, windows, window_mask, owners_sent: false, used_sors: 0, gpu_max_rate, increased_wm, heads: Vec::new(), primary: 0 })
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
    core_update(disp, bar, 0)?;
    log.push(format!("C2: head {head} raster {}x{} up ({:#x} armed)", mode.h_total, mode.v_total, bar.rd(0x0068_a064 + hb as usize)));

    // C3: таблицы цвета, окно с картинкой.
    load_lut(disp.vram, olut(head), false);
    load_lut(disp.vram, ilut(head), true);
    disp.core.push(0x2280 + hb, &[0x0004_0509, u32::MAX, H_VRAM, (olut(head) >> 8) as u32]);
    let pitch = (mode.width * 4).div_ceil(64) * 64;
    // Картинка — тёмно-синяя до первого кадра стола.
    for y in 0..mode.height {
        let row = fb(head) + u64::from(y) * u64::from(pitch);
        for x in (0..mode.width).step_by(1) {
            disp.vram.w32(row + u64::from(x) * 4, 0x0010_1828);
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

/// Размер картинки основного монитора, если он зажжён.
#[must_use]
pub fn primary_size() -> Option<(u32, u32)> {
    let disp = DISP.lock();
    let disp = disp.as_ref()?;
    disp.heads.iter().find(|(_, id, _)| *id == disp.primary).map(|(_, _, m)| (m.width, m.height))
}
