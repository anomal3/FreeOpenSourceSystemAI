//! NVK поверх GSP-RM (18.11.2026, WP-A): сведения о карте для слоя DRM и
//! общие помощники `nvidia_rm_*`.
//!
//! * [`with_rm`] — разговор с RM под замком сеанса, журнал событий — сразу в
//!   `kprintln` (строки `nvgpu:` попадают на флешку, даже если дальше зависнет).
//! * [`query`] — таблица движков (`FIFO_GET_DEVICE_INFO_TABLE`), GPC/TPC,
//!   размер буфера методов CE, `nvgpu::GpuInfo` (GETPARAM, NVIF INFO, sysfs).
//! * [`ptimer_ns`], [`bar1_check`] — регистры.
//!
//! Раскладки — open-gpu-kernel-modules 570.144 (`ctrl2080fifo.h`,
//! `ctrl2080gr.h`, `ctrl2080ce.h`), разбор — nouveau `fifo/r535.c`
//! `r535_fifo_runl_ctor` (r570 использует его же).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::nvidia::Bar0;
use super::nvidia_gsp::{Log, Res, Rm, SESSION, StaticInfo, le32};

/// Строка журнала NVK: сразу в `kprintln` и в отчёт.
pub(crate) fn say(log: &mut Log, line: String) {
    crate::kprintln!("  {line}");
    log.push(line);
}

/// Выполнить `f` с RM: замок сеанса (ждём до 10 с), внутренние хэндлы RM
/// (`internal_client`, `internal_subdevice`). События, пришедшие во время
/// разговора, — в журнал.
pub(crate) fn with_rm<R>(log: &mut Log, f: impl FnOnce(&mut Rm, (u32, u32)) -> R) -> Res<R> {
    let mut guard = SESSION.lock_within(10_000).ok_or("GSP-RM session busy for 10 s")?;
    let session = guard.as_mut().ok_or("GSP-RM is not running (gpu gsp first)")?;
    let internal = (session.internal_client, session.internal_subdevice);
    let mut rm = Rm { queues: &mut session.queues, bar: session.bar, log: Vec::new() };
    let result = f(&mut rm, internal);
    for line in core::mem::take(&mut rm.log) {
        // gsp_event уже напечатал свои строки `nvgpu: gsp event`.
        if line.starts_with("nvgpu:") {
            log.push(line);
        } else {
            say(log, format!("nvgpu: rm: {line}"));
        }
    }
    Ok(result)
}

/// Разобрать события GSP, пришедшие без запроса (между вызовами RM). Не
/// ждёт замка: занят — значит, события разбирает тот, кто говорит с RM.
pub(crate) fn pump_events(log: &mut Log) {
    if let Some(mut guard) = SESSION.try_lock() {
        if let Some(session) = guard.as_mut() {
            let mut rm = Rm { queues: &mut session.queues, bar: session.bar, log: Vec::new() };
            if let Err(why) = rm.pump() {
                rm.log.push(format!("nvgpu: event pump - {why}"));
            }
            for line in rm.log {
                say(log, line);
            }
        }
    }
}

/// Статические сведения GSP (копия) — без них NVK не стартует.
pub(crate) fn static_info() -> Option<StaticInfo> {
    SESSION.lock_within(10_000)?.as_ref().map(|s| s.info.clone())
}

/// Карта: устройство PCI и BAR0.
pub(crate) fn card() -> Res<(crate::pci::Device, Bar0)> {
    super::nvidia::card().ok_or_else(|| "no NVIDIA card".into())
}

/// PTIMER в наносекундах (`usermode + 0x80/0x84`, BAR0 0xbb0080; hi/lo/hi).
pub(crate) fn ptimer_ns(bar: Bar0) -> u64 {
    loop {
        let hi = bar.rd(0x00bb_0084);
        let lo = bar.rd(0x00bb_0080);
        if bar.rd(0x00bb_0084) == hi {
            return (u64::from(hi) << 32) | u64::from(lo);
        }
    }
}

/// Остался ли BAR1 в физическом режиме (RM мог перепривязать его во время
/// своих операций — спецификация, риск 4). Если нет — вернуть и сказать.
pub(crate) fn bar1_check(bar: Bar0, log: &mut Log, after: &str) {
    let (block, bind) = (bar.rd(0x00b8_0f40), bar.rd(0x00b8_0f50));
    if block == 0 && bind & 3 == 0 {
        return;
    }
    bar.wr(0x00b8_0f40, 0);
    let ok = bar.wait(2_000_000, |b| b.rd(0x00b8_0f50) & 3 == 0);
    say(
        log,
        format!(
            "nvgpu: BAR1 left physical mode after {after} (BAR1_BLOCK {block:#x} BIND {bind:#x}) - rebound: {}",
            if ok { "ok" } else { "bind did NOT finish" }
        ),
    );
}

// --- движки --------------------------------------------------------------------

/// `RM_ENGINE_TYPE`/`NV2080_ENGINE_TYPE` (у этих двух номера совпадают):
/// GR0 = 1, COPY0..COPY9 = 9..18.
pub(crate) const ENGINE_GR0: u32 = 1;
pub(crate) const ENGINE_COPY0: u32 = 9;

/// Строка таблицы движков RM.
#[derive(Clone, Debug)]
pub(crate) struct Engine {
    pub(crate) rm_type: u32,
    pub(crate) runlist: u32,
    pub(crate) pri_base: u32,
    pub(crate) name: String,
}

impl Engine {
    pub(crate) fn is_copy(&self) -> bool {
        (ENGINE_COPY0..ENGINE_COPY0 + 10).contains(&self.rm_type)
    }
}

/// Всё, что RM сказал о карте для NVK.
#[derive(Clone, Debug)]
pub(crate) struct HwInfo {
    pub(crate) engines: Vec<Engine>,
    pub(crate) gr_runlist: Option<u32>,
    pub(crate) mthdbuf_size: u32,
    pub(crate) gpu: nvgpu::GpuInfo,
}

impl HwInfo {
    /// CE на том же runlist, что GR (копирование из канала GR).
    pub(crate) fn gr_copy(&self) -> Option<&Engine> {
        let runlist = self.gr_runlist?;
        self.engines.iter().find(|e| e.is_copy() && e.runlist == runlist)
    }

    /// CE со своим runlist (асинхронное копирование; канал без GR).
    pub(crate) fn async_copy(&self) -> Option<&Engine> {
        self.engines.iter().find(|e| e.is_copy() && Some(e.runlist) != self.gr_runlist)
    }
}

fn u32s(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

/// Таблица движков: `NV2080_CTRL_CMD_FIFO_GET_DEVICE_INFO_TABLE` (0x20801112)
/// на внутреннем подустройстве. Ответ 0xc8c: `baseIndex, numEntries, bMore`,
/// записи по 0x64 с 0xc: `engineData[16]` (2 = RM_ENGINE_TYPE, 3 = RUNLIST,
/// 11 = RUNLIST_PRI_BASE), имя с 0x54.
fn device_info(rm: &mut Rm, internal: (u32, u32), log: &mut Log) -> Res<Vec<Engine>> {
    let mut engines = Vec::new();
    let mut base = 0u32;
    for _ in 0..8 {
        let mut p = alloc::vec![0u8; 0xc8c];
        p[0..4].copy_from_slice(&base.to_le_bytes());
        let r = rm.control(internal.0, internal.1, 0x2080_1112, &p)?;
        let count = le32(&r, 4)?.min(32) as usize;
        let more = r.get(8).copied().unwrap_or(0) != 0;
        for i in 0..count {
            let at = 0xc + i * 0x64;
            let data = |k: usize| le32(&r, at + k * 4).unwrap_or(0);
            let name = r.get(at + 0x54..at + 0x64).unwrap_or(&[]);
            let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
            engines.push(Engine {
                rm_type: data(2),
                runlist: data(3),
                pri_base: data(11),
                name: String::from_utf8_lossy(&name[..end]).into_owned(),
            });
        }
        if !more || count == 0 {
            break;
        }
        base += count as u32;
    }
    let mut line = String::from("nvgpu: engines:");
    for e in &engines {
        line.push_str(&format!(" [{} type {} runlist {} pri {:#x}]", e.name, e.rm_type, e.runlist, e.pri_base));
    }
    say(log, line);
    Ok(engines)
}

/// GPC и TPC: `GR_GET_GPC_MASK` (0x2080122a, `{route 16 B; u32 gpcMask}`) и
/// `GR_GET_TPC_MASK` (0x2080122b, `{route; u32 gpcId; u32 tpcMask}`). Сперва
/// у внутреннего подустройства; отказ — запасные 3/14 GA106 (спецификация 2.1).
fn gpc_tpc(rm: &mut Rm, subdevice: (u32, u32), log: &mut Log) -> Option<(u32, u32, u32)> {
    let r = match rm.control(subdevice.0, subdevice.1, 0x2080_122a, &[0u8; 0x18]) {
        Ok(r) => r,
        Err(why) => {
            say(log, format!("nvgpu: GR_GET_GPC_MASK on {:#x}/{:#x} - {why}", subdevice.0, subdevice.1));
            return None;
        }
    };
    let gpc_mask = le32(&r, 0x10).unwrap_or(0);
    let mut tpcs = 0;
    let mut masks = Vec::new();
    for gpc in 0..32u32 {
        if gpc_mask & (1 << gpc) == 0 {
            continue;
        }
        let mut p = alloc::vec![0u8; 0x18];
        p[0x10..0x14].copy_from_slice(&gpc.to_le_bytes());
        match rm.control(subdevice.0, subdevice.1, 0x2080_122b, &p) {
            Ok(r) => {
                let mask = le32(&r, 0x14).unwrap_or(0);
                tpcs += mask.count_ones();
                masks.push(mask);
            }
            Err(why) => {
                say(log, format!("nvgpu: GR_GET_TPC_MASK gpc {gpc} - {why}"));
                return None;
            }
        }
    }
    say(log, format!("nvgpu: gpc mask {gpc_mask:#x}, tpc masks {masks:x?}"));
    Some((gpc_mask.count_ones(), tpcs, gpc_mask))
}

/// Спросить RM о карте (раз на сеанс). `vram_user` — сколько видеопамяти
/// получают буферы пользователей (bulk-арена).
pub(crate) fn query(rm: &mut Rm, internal: (u32, u32), info: &StaticInfo, vram_user: u64, log: &mut Log) -> Res<HwInfo> {
    let (device, bar) = card()?;
    let engines = device_info(rm, internal, log)?;
    let gr_runlist = engines.iter().find(|e| e.rm_type == ENGINE_GR0).map(|e| e.runlist);

    let mthdbuf_size = match rm.control(internal.0, internal.1, 0x2080_2a08, &u32s(&[0])) {
        Ok(r) => le32(&r, 0).unwrap_or(0),
        Err(why) => {
            say(log, format!("nvgpu: CE_GET_FAULT_METHOD_BUFFER_SIZE - {why}; using 64 KiB"));
            0x1_0000
        }
    };

    let (chip, revision) = super::nvidia::chip_of(bar.rd(0), bar.rd(0xa00));
    let gart = (crate::mm::frame::stats().free as u64 * crate::mm::PAGE_SIZE as u64) / 2;
    let mut gpu = nvgpu::GpuInfo::ga106(vram_user, gart);
    gpu.chipset = chip as u16;
    gpu.chip = String::from(super::nvidia::chip_name(chip));
    if !info.name.is_empty() {
        gpu.name = info.name.clone();
    }
    gpu.pci_vendor = device.vendor;
    gpu.pci_device = device.device;
    gpu.pci_revision = device.config8(8);
    let subsys = device.config32(0x2c);
    gpu.subsys_vendor = subsys as u16;
    gpu.subsys_device = (subsys >> 16) as u16;
    gpu.pci_slot = nvgpu::PciSlot {
        domain: device.address.segment,
        bus: device.address.bus,
        dev: device.address.device,
        func: device.address.function,
    };
    if let Some(size) = device.memory_bar_size(1) {
        gpu.bar_bytes = size;
    }
    match gpc_tpc(rm, internal, log) {
        Some((gpcs, tpcs, _)) if gpcs > 0 && tpcs > 0 => {
            gpu.gpcs = gpcs;
            gpu.tpcs = tpcs;
        }
        _ => say(log, format!("nvgpu: gpc/tpc not reported by RM - using GA106 defaults {}/{}", gpu.gpcs, gpu.tpcs)),
    }
    say(
        log,
        format!(
            "nvgpu: static info: chip {chip:x} rev {revision:x} {} '{}' pci {:04x}:{:04x} rev {:02x} subsys {:04x}:{:04x} at {:02x}:{:02x}.{} \
             vram user {} MiB bar1 {} MiB gart {} MiB gpc {} tpc {} gr runlist {gr_runlist:?} mthdbuf {mthdbuf_size:#x}",
            gpu.chip,
            gpu.name,
            gpu.pci_vendor,
            gpu.pci_device,
            gpu.pci_revision,
            gpu.subsys_vendor,
            gpu.subsys_device,
            gpu.pci_slot.bus,
            gpu.pci_slot.dev,
            gpu.pci_slot.func,
            gpu.vram_bytes >> 20,
            gpu.bar_bytes >> 20,
            gpu.gart_bytes >> 20,
            gpu.gpcs,
            gpu.tpcs
        ),
    );
    let mut regions = String::new();
    for r in &info.regions {
        regions.push_str(&format!(
            " [{:#x}..{:#x} rsvd {:#x} perf {} compr {} iso {} prot {}]",
            r.base, r.limit, r.reserved, r.perf, r.compr, r.iso, r.protected
        ));
    }
    say(
        log,
        format!(
            "nvgpu: fb {} MiB, BAR1 PDE {:#x}, BAR2 PDE {:#x}, fb regions:{regions}; arenas {}",
            info.fb_length >> 20,
            info.bar1_pde,
            info.bar2_pde,
            super::nvidia_rm_vram::describe()
        ),
    );
    Ok(HwInfo { engines, gr_runlist, mthdbuf_size, gpu })
}
