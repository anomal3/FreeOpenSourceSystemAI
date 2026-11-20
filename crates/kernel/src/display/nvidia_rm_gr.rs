//! NVK поверх GSP-RM (18.11.2026, WP-A): графический движок (GR).
//!
//! Повторяет nouveau `gr/r535.c` (r570 пользуется тем же кодом —
//! `r535_gr_oneinit`, `r535_gr_promote_ctx`):
//!
//! 1. **Golden context** — один раз до первого канала GR: своё адресное
//!    пространство, привилегированный канал (chid `rsvd_chids` = 1 у r570),
//!    `INTERNAL_STATIC_KGR_GET_CONTEXT_BUFFERS_INFO` → таблица буферов,
//!    все буферы выделяются и отдаются RM (`GPU_PROMOTE_CTX`), затем объект
//!    3D-класса на канале — RM строит по нему golden context. Канал и
//!    пространство снимаются; глобальные буферы живут до выгрузки GSP-RM.
//! 2. **Канал пользователя** — при первом объекте класса GR: свои MAIN и
//!    PATCH, глобальные — общие (кроме UNRESTRICTED_PRIV_ACCESS_MAP),
//!    отображение в kernel-VA канала (priv, ro по таблице), PROMOTE_CTX.
//!    RM требует, чтобы эти отображения жили, пока жив канал.

use alloc::format;
use alloc::vec::Vec;

use super::nvidia_gsp::{Log, Res, Rm, hex_head, le32};
use super::nvidia_rm_info::{HwInfo, say};
use super::nvidia_rm_vmm::{H_SUBDEVICE, PAGE, Vmm};
use super::nvidia_rm_vram as vram;
use crate::sync::SpinLock;

/// `NV2080_CTRL_GPU_PROMOTE_CTX_BUFFER_ID_*`.
const BUF_MAIN: u16 = 0;
const BUF_PRIV_ACCESS_MAP: u16 = 10;
const BUF_UNRESTRICTED_PRIV_ACCESS_MAP: u16 = 11;
const BUF_ATTRIBUTE_CB: u16 = 5;

/// Описание буфера контекста.
#[derive(Clone, Copy, Debug)]
struct CtxBuf {
    id: u16,
    size: u64,
    page: u8,
    align: u8,
    global: bool,
    init: bool,
    ro: bool,
    /// Видеопамять буфера, выделенного при golden (глобальные — общие).
    mem: u64,
}

struct Gr {
    bufs: Vec<CtxBuf>,
}

static GR: SpinLock<Option<Gr>> = SpinLock::new(None);

/// Сделан ли golden context в этом сеансе GSP.
pub(crate) fn ready() -> bool {
    GR.lock().is_some()
}

/// Забыть (GSP-RM выгружен: буферы — в видеопамяти, которую RM уже не знает).
pub(crate) fn reset() {
    if let Some(mut gr) = GR.try_lock() {
        *gr = None;
    }
}

/// Таблица буферов из ответа RM (`r535_gr_oneinit`, отображение
/// `NV0080_CTRL_FIFO_GET_ENGINE_CONTEXT_PROPERTIES_ENGINE_ID_*` → buffer id).
fn table(info: &[u8], log: &mut Log) -> Vec<CtxBuf> {
    // (id0, id1, global, init, ro)
    const MAP: [(usize, u16, bool, bool, bool); 8] = [
        (0x00, 0, false, true, false),  // GRAPHICS -> MAIN
        (0x10, 2, false, true, false),  // GRAPHICS_PATCH -> PATCH
        (0x11, 3, true, false, false),  // GRAPHICS_BUNDLE_CB -> BUFFER_BUNDLE_CB
        (0x0d, 4, true, false, false),  // GRAPHICS_PAGEPOOL -> PAGEPOOL
        (0x13, 5, true, false, false),  // GRAPHICS_ATTRIBUTE_CB -> ATTRIBUTE_CB
        (0x14, 6, true, false, false),  // GRAPHICS_RTV_CB_GLOBAL -> RTV_CB_GLOBAL
        (0x17, 9, true, true, false),   // GRAPHICS_FECS_EVENT -> FECS_EVENT
        (0x18, 10, true, true, true),   // GRAPHICS_PRIV_ACCESS_MAP -> PRIV_ACCESS_MAP
    ];
    let mut bufs = Vec::new();
    let mut seen = alloc::string::String::new();
    // r570: 0x1a записей на движок GR (r535 — 0x19), GR0 — первый.
    for i in 0..0x1a {
        let size = u64::from(le32(info, i * 8).unwrap_or(0));
        let alignment = le32(info, i * 8 + 4).unwrap_or(0);
        if size != 0 {
            seen.push_str(&format!(" {i:#x}:{size:#x}/{alignment:#x}"));
        }
        let Some(&(_, id, global, init, ro)) = MAP.iter().find(|m| m.0 == i) else { continue };
        let mut size = size;
        if id == BUF_MAIN {
            size = size.div_ceil(PAGE) * PAGE + 64 * PAGE; // заголовки подконтекстов
        }
        let page = if size >= 1 << 21 {
            21
        } else if size >= 1 << 16 {
            16
        } else {
            12
        };
        let align = if id == BUF_ATTRIBUTE_CB { (64 - (size.max(1) - 1).leading_zeros()) as u8 } else { page };
        let buf = CtxBuf { id, size, page, align, global, init, ro, mem: 0 };
        bufs.push(buf);
        if id == BUF_PRIV_ACCESS_MAP {
            bufs.push(CtxBuf { id: BUF_UNRESTRICTED_PRIV_ACCESS_MAP, ..buf });
        }
    }
    say(log, format!("nvgpu: gr ctx buffer sizes from RM (id:size/align):{seen}"));
    bufs
}

/// Отдать RM буферы контекста канала `chan` (`r535_gr_promote_ctx`).
/// `golden` — все буферы новые (и становятся глобальными). Возвращает новые
/// куски видеопамяти (для освобождения вместе с каналом).
fn promote(rm: &mut Rm, vmm: &mut Vmm, chan: u32, bufs: &mut [CtxBuf], golden: bool, log: &mut Log) -> Res<Vec<(u64, u64)>> {
    let mut p = alloc::vec![0u8; 0x230];
    p[0..4].copy_from_slice(&1u32.to_le_bytes()); // engineType GR0
    p[0x0c..0x10].copy_from_slice(&vmm.client.to_le_bytes()); // hChanClient
    p[0x10..0x14].copy_from_slice(&chan.to_le_bytes()); // hObject
    let mut fresh = Vec::new();
    let mut count = 0usize;
    let mut line = format!("nvgpu: gr {} promote chan {chan:#x}:", if golden { "golden" } else { "chan" });
    for buf in bufs.iter_mut() {
        let alloc = golden || !buf.global;
        let mut nonmapped = false;
        let mem = if alloc {
            let mem = vram::bulk_alloc(buf.size, 1 << buf.page)?;
            fresh.push((mem, buf.size));
            if golden {
                buf.mem = mem;
            }
            nonmapped = buf.id == BUF_PRIV_ACCESS_MAP;
            mem
        } else {
            if buf.id == BUF_UNRESTRICTED_PRIV_ACCESS_MAP {
                continue;
            }
            buf.mem
        };
        let init = buf.init && alloc;
        let mut va = 0;
        if !nonmapped {
            let len = buf.size.div_ceil(PAGE) * PAGE;
            va = vmm.kva_alloc(len, 1 << buf.align.min(32))?;
            vmm.map_vram(va, mem, len, true, buf.ro, 0)?;
        }
        if count >= 16 {
            return Err("more than 16 context buffers".into());
        }
        let at = 0x30 + count * 0x20;
        if init {
            p[at..at + 8].copy_from_slice(&mem.to_le_bytes());
            p[at + 0x10..at + 0x18].copy_from_slice(&buf.size.to_le_bytes());
            p[at + 0x18..at + 0x1c].copy_from_slice(&4u32.to_le_bytes());
        }
        p[at + 8..at + 0x10].copy_from_slice(&va.to_le_bytes());
        p[at + 0x1c..at + 0x1e].copy_from_slice(&buf.id.to_le_bytes());
        p[at + 0x1e] = u8::from(init);
        p[at + 0x1f] = u8::from(nonmapped);
        line.push_str(&format!(
            " [id {} size {:#x} page {} {}{}{} pa {mem:#x} va {va:#x}{}]",
            buf.id,
            buf.size,
            buf.page,
            if buf.global { "global" } else { "local" },
            if init { " init" } else { "" },
            if buf.ro { " ro" } else { "" },
            if nonmapped { " nonmapped" } else { "" }
        ));
        count += 1;
    }
    p[0x28..0x2c].copy_from_slice(&(count as u32).to_le_bytes());
    vmm.flush_tlb(log);
    say(log, line);
    let status = rm.control(vmm.client, H_SUBDEVICE, 0x2080_012b, &p);
    say(
        log,
        format!(
            "nvgpu: gr promote status {}",
            match &status {
                Ok(_) => "ok".into(),
                Err(why) => format!("FAILED {why} (params {})", hex_head(&p, 0x70)),
            }
        ),
    );
    status?;
    Ok(fresh)
}

/// Golden context (раз на сеанс GSP). `chan_params` строит параметры
/// канала (общие с `nvidia_rm_chan`).
pub(crate) fn golden(rm: &mut Rm, internal: (u32, u32), hw: &HwInfo, log: &mut Log) -> Res<()> {
    if ready() {
        return Ok(());
    }
    let started = crate::time::uptime_ms();
    // 1. Пространство и память канала (0x12000: inst, USERD +0x1000).
    let mut vmm = Vmm::create(rm, super::nvidia_rm_chan::GOLDEN_VM, (1 << 39, 1 << 39), log)?;
    let result = golden_in(rm, internal, hw, &mut vmm, log);
    vmm.destroy(rm, log);
    match &result {
        Ok(()) => say(log, format!("nvgpu: gr golden: done in {} ms", crate::time::uptime_ms() - started)),
        Err(why) => say(log, format!("nvgpu: gr golden: FAILED - {why}")),
    }
    result
}

fn golden_in(rm: &mut Rm, internal: (u32, u32), hw: &HwInfo, vmm: &mut Vmm, log: &mut Log) -> Res<()> {
    use super::nvidia_rm_chan::{ChanParams, Dma, GOLDEN_CHID, channel_params};
    let inst = vram::cpu_alloc(0x12000, PAGE)?;
    let mthdbuf = Dma::new((hw.mthdbuf_size as usize).div_ceil(0x1000).max(1))?;
    let handle = 0xf1f0_0000;
    let params = channel_params(&ChanParams {
        gpfifo_va: 0,
        entries: 0x1000 / 8,
        chid: GOLDEN_CHID,
        privileged: true,
        vaspace: super::nvidia_rm_vmm::H_VASPACE,
        engine: super::nvidia_rm_info::ENGINE_GR0,
        inst,
        userd: inst + 0x1000,
        mthdbuf: (mthdbuf.phys, hw.mthdbuf_size),
    });
    let result = (|| -> Res<()> {
        rm.alloc_in(vmm.client, super::nvidia_rm_vmm::H_DEVICE, handle, 0xc56f, &params)?;
        say(
            log,
            format!(
                "nvgpu: gr golden channel {handle:#x} chid {GOLDEN_CHID} ok; inst {inst:#x} PDB written by RM: {:#x} limit {:#x}",
                vram::r64(inst + 0x200),
                vram::r64(inst + 0x208)
            ),
        );
        // 2. Буферы контекста.
        let info = rm.control(internal.0, internal.1, 0x2080_0a32, &alloc::vec![0u8; 0x680])?;
        let mut bufs = table(&info, log);
        if bufs.is_empty() {
            return Err("RM reported no context buffers".into());
        }
        // 3. Golden PROMOTE_CTX (все буферы новые).
        promote(rm, vmm, handle, &mut bufs, true, log)?;
        // 4. Объект 3D на канале — RM строит golden context.
        let class = hw.gpu.classes.iter().copied().filter(|c| c & 0xff == 0x97).max().unwrap_or(0xc797);
        let threed = rm.alloc_in(vmm.client, handle, 0x9700_0000, class, &[]);
        say(log, format!("nvgpu: gr golden: 3D class {class:#x} on the golden channel: {}", threed.as_ref().map_or_else(|e| e.clone(), |_| "ok".into())));
        threed?;
        let _ = rm.free_in(vmm.client, handle, 0x9700_0000);
        *GR.lock() = Some(Gr { bufs });
        Ok(())
    })();
    // Канал снимается всегда; пространство — вызывающий.
    if let Err(why) = rm.free_in(vmm.client, super::nvidia_rm_vmm::H_DEVICE, handle) {
        say(log, format!("nvgpu: gr golden channel free - {why}"));
    }
    vram::free(inst, 0x12000);
    mthdbuf.free();
    result
}

/// Буферы контекста для канала пользователя (при первом объекте GR).
/// Возвращает новые куски видеопамяти (освободить после снятия канала).
pub(crate) fn promote_channel(rm: &mut Rm, vmm: &mut Vmm, chan: u32, log: &mut Log) -> Res<Vec<(u64, u64)>> {
    let mut bufs = GR.lock().as_ref().map(|g| g.bufs.clone()).ok_or("GR golden context is not done")?;
    promote(rm, vmm, chan, &mut bufs, false, log)
}
