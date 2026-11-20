//! NVK поверх GSP-RM (18.11.2026, WP-A): каналы GPFIFO и настоящий
//! [`nvgpu::GpuBackend`] для RTX 3060.
//!
//! # Канал (nouveau `fifo/r535.c` + `r570/fifo.c`, `nouveau_chan.c`, `nouveau_dma.c`)
//!
//! * Блок экземпляров (4 КиБ, CPU-арена) с корнем пространства (`Vmm::join`),
//!   USERD (своя страница; chid = 8 × номер страницы, индекс 0), pushbuf
//!   0x12000 в памяти машины: `[0, 0x10000)` — сегменты ядра (заборы),
//!   `[0x10000, 0x12000)` — кольцо GPFIFO на 1024 записи; страница забора;
//!   буфер методов (`CE_GET_FAULT_METHOD_BUFFER_SIZE`).
//! * RM: объект `AMPERE_CHANNEL_GPFIFO_A` (0xc56f) под устройством клиента
//!   пространства, `NV_CHANNEL_ALLOC_PARAMS` (0x170 у r570: + `tpcConfigID`),
//!   затем `NVA06F_CTRL_CMD_BIND` и `GPFIFO_SCHEDULE` (r570 их по-прежнему
//!   шлёт из общего `r535_chan_ramfc_write`), `GET_WORK_SUBMIT_TOKEN`.
//! * Отправка: записи GPFIFO, `USERD+0x8c = put`, звонок `BAR0 0xbb0090 =
//!   token`. Забор — сегмент ядра SEMAPHOREA..D + NON_STALL_INTERRUPT (класс
//!   906F, у C56F те же методы): GPU пишет номер в страницу забора, процессор
//!   её опрашивает.
//! * Убитый канал: событие RC_TRIGGERED (`nvidia_gsp::RC_CHIDS`) или
//!   сторож (забор не идёт 10 с) → `chan_seq_done` = ENODEV.
//!
//! # Замки
//!
//! Состояние бэкенда — под `crate::sync::Mutex` (спящий), RM — под замком
//! сеанса GSP (`with_rm`), порядок всегда «бэкенд → сеанс».

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering, fence};

use nvgpu::gpfifo::{self, PushBuilder, Ring};
use nvgpu::{BoHandle, ChanHandle, Errno, GpuBackend, GpuInfo, ObjHandle, Place, Push, VmHandle};

use super::nvidia_gsp::{Log, Res, Rm};
use super::nvidia_rm_info::{self as info, HwInfo, say, with_rm};
use super::nvidia_rm_vmm::{H_DEVICE, H_VASPACE, PAGE, Vmm};
use super::nvidia_rm_vram as vram;
use crate::mm::{FrameAllocator, PhysAddr};
use crate::sync::{Mutex, SpinLock};

/// Пространство golden context (номер клиента RM `0xc1d0_0010 + 0xfff`).
pub(crate) const GOLDEN_VM: u32 = 0xfff;
/// chid golden-канала: `rsvd_chids` r570 = 1.
pub(crate) const GOLDEN_CHID: u32 = 1;

const RING_ENTRIES: u32 = gpfifo::GPFIFO_ENTRIES;
const PUSH_BYTES: u64 = gpfifo::PUSHBUF_BYTES;
const KPUSH_BYTES: u64 = gpfifo::PUSHBUF_KERNEL_AREA;
const RING_OFFSET: u64 = gpfifo::PUSHBUF_RING_OFFSET;
const GP_GET: u64 = gpfifo::USERD_GP_GET;
const GP_PUT: u64 = gpfifo::USERD_GP_PUT;
/// Сторож: забор не продвигается столько — канал убит (nouveau: 10 с).
const WATCHDOG_MS: u64 = 10_000;

// --- память машины для GPU -------------------------------------------------------

/// Непрерывная память машины, обнулённая, с возвратом (в отличие от
/// `nvidia_gsp::Mem`, которая живёт вечно).
pub(crate) struct Dma {
    pub(crate) phys: u64,
    pub(crate) pages: usize,
}

impl Dma {
    pub(crate) fn new(pages: usize) -> Res<Self> {
        let phys = crate::mm::frame::with(|f| f.allocate_contiguous(pages))
            .flatten()
            .ok_or_else(|| format!("no {pages} contiguous pages of memory"))?
            .as_u64();
        let dma = Self { phys, pages };
        // SAFETY: страницы только что выделены и принадлежат этому куску.
        unsafe { core::ptr::write_bytes(dma.ptr(), 0, pages * PAGE as usize) };
        Ok(dma)
    }

    pub(crate) fn ptr(&self) -> *mut u8 {
        PhysAddr::new(self.phys).to_direct_map().as_usize() as *mut u8
    }

    pub(crate) fn frames(&self) -> Vec<u64> {
        (0..self.pages as u64).map(|i| self.phys + i * PAGE).collect()
    }

    pub(crate) fn w32(&self, at: usize, v: u32) {
        // SAFETY: внутри куска (вызывающие держат смещения в его пределах).
        unsafe { (self.ptr().add(at) as *mut u32).write_volatile(v) }
    }

    pub(crate) fn r32(&self, at: usize) -> u32 {
        // SAFETY: см. `w32`.
        unsafe { (self.ptr().add(at) as *const u32).read_volatile() }
    }

    pub(crate) fn free(self) {
        for i in 0..self.pages as u64 {
            // SAFETY: кадры выделены этим куском и больше никем не используются
            // (GPU к ним уже не обращается — вызывающий снял канал).
            crate::mm::frame::with(|f| unsafe { f.free(PhysAddr::new(self.phys + i * PAGE)) });
        }
    }
}

fn free_frames(frames: &[u64]) {
    for &p in frames {
        // SAFETY: кадры буфера, отданные GPU-бэкенду; отображений больше нет.
        crate::mm::frame::with(|f| unsafe { f.free(PhysAddr::new(p)) });
    }
}

// --- параметры канала ----------------------------------------------------------

pub(crate) struct ChanParams {
    pub(crate) gpfifo_va: u64,
    pub(crate) entries: u32,
    pub(crate) chid: u32,
    pub(crate) privileged: bool,
    pub(crate) vaspace: u32,
    pub(crate) engine: u32,
    pub(crate) inst: u64,
    pub(crate) userd: u64,
    pub(crate) mthdbuf: (u64, u32),
}

/// `NV_CHANNEL_ALLOC_PARAMS` 570.144 (0x170 B; смещения — спецификация 2.4 и
/// `r570/nvrm/fifo.h`).
pub(crate) fn channel_params(c: &ChanParams) -> Vec<u8> {
    let mut p = alloc::vec![0u8; 0x170];
    let put32 = |p: &mut [u8], at: usize, v: u32| p[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |p: &mut [u8], at: usize, v: u64| p[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put64(&mut p, 0x08, c.gpfifo_va);
    put32(&mut p, 0x10, c.entries);
    // flags: RUNQUEUE 0 (бит 4), PRIVILEGED (бит 5), USERD_INDEX_VALUE
    // (10:8), USERD_INDEX_PAGE_VALUE (20:12), USERD_INDEX_PAGE_FIXED (21).
    let flags = (u32::from(c.privileged) << 5) | ((c.chid % 8) << 8) | ((c.chid / 8) << 12) | (1 << 21);
    put32(&mut p, 0x14, flags);
    put32(&mut p, 0x1c, c.vaspace);
    put32(&mut p, 0x80, c.engine);
    // instanceMem / userdMem / ramfcMem — видеопамять (2), кэш 1.
    for (at, base, size) in [(0x90, c.inst, 0x1000u64), (0xa8, c.userd, 0x200), (0xc0, c.inst, 0x200)] {
        put64(&mut p, at, base);
        put64(&mut p, at + 8, size);
        put32(&mut p, at + 0x10, 2);
        put32(&mut p, at + 0x14, 1);
    }
    // mthdbufMem — память машины (1), без кэша.
    put64(&mut p, 0xd8, c.mthdbuf.0);
    put64(&mut p, 0xe0, u64::from(c.mthdbuf.1));
    put32(&mut p, 0xe8, 1);
    put32(&mut p, 0xec, 0);
    // internalFlags: PRIVILEGE USER 0 / ADMIN 1, ERROR_NOTIFIER NONE, ECC NONE.
    put32(&mut p, 0xf4, 0x14 | u32::from(c.privileged));
    p
}

// --- состояние -------------------------------------------------------------------

struct Bo {
    place: Place,
    size: u64,
    vram: u64,
    frames: Vec<u64>,
}

/// На каком runlist канал.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Runlist {
    /// Runlist GR (так создаёт каналы NVK): 3D, compute, 2D, M2MF, copy.
    Gr,
    /// Свой runlist асинхронного CE: только copy (опыт без GR).
    AsyncCopy,
}

struct Chan {
    vm: VmHandle,
    chid: u32,
    handle: u32,
    runlist: u32,
    /// Тип движка канала (GR0 или COPYn) и CE для объектов copy.
    engine: u32,
    copy_engine: u32,
    inst: u64,
    userd: u64,
    push: Dma,
    push_va: u64,
    fence: Dma,
    fence_va: u64,
    mthdbuf: Dma,
    ring: Ring,
    kpush: u32,
    seq: u64,
    token: u32,
    /// Жетон, который вернул GSP-RM (`GET_WORK_SUBMIT_TOKEN`), — для опыта.
    rm_token: Option<u32>,
    /// Начало регистров runlist (`RUNLIST_PRI_BASE` таблицы движков).
    pri_base: u32,
    killed: bool,
    /// (ручка бэкенда, хэндл RM, класс)
    objs: Vec<(u32, u32, u32)>,
    grctx: Option<Vec<(u64, u64)>>,
    submits: u64,
    /// Когда забор последний раз продвинулся (для сторожа).
    progress: (u64, u64),
}

struct State {
    hw: HwInfo,
    bos: BTreeMap<u32, Bo>,
    vms: BTreeMap<u32, Vmm>,
    chans: BTreeMap<u32, Chan>,
    next: u32,
    /// Занятые страницы USERD (chid = 8 × номер).
    userd_pages: Vec<bool>,
}

impl State {
    fn handle(&mut self) -> u32 {
        self.next = self.next.wrapping_add(1).max(1);
        self.next
    }
}

/// Настоящий бэкенд GPU поверх GSP-RM.
pub struct RmBackend {
    info: GpuInfo,
    state: Mutex<State>,
}

static BACKEND: SpinLock<Option<Arc<RmBackend>>> = SpinLock::new(None);
/// GSP-RM выгружается — бэкенд больше не трогает карту.
static DEAD: AtomicBool = AtomicBool::new(false);

/// Какой жетон звонка брать (19.11, hw-log10: CE2 на runlist 1 молчал с
/// жетоном RM 0x8, расчётный 0x10008).
///
/// Почему расчётный: `NVC36F_CTRL_CMD_GPFIFO_GET_WORK_SUBMIT_TOKEN` у
/// nvidia.ko обрабатывает CPU-RM (`kfifoGenerateWorkSubmitTokenHal_GA100`,
/// ветка `!RMCFG_FEATURE_PLATFORM_GSP`): `RUNLIST_ID << 16 | VECTOR = chid`
/// — этим жетоном звонят CUDA и UVM. Мы же спрашиваем сам GSP-RM, а у него
/// ветка `PLATFORM_GSP` — `kfifoGenerateInternalWorkSubmitToken` (внутренний
/// жетон, без runlist). nouveau r535 (`r535_chan_doorbell_handle`) звонит
/// `runl->id << 16 | chid`, где `runl->id` — RUNLIST из той же таблицы
/// движков. На runlist 0 они совпадают (потому GR и работал).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// `runlist << 16 | chid` (как nouveau r535 и CPU-RM nvidia.ko).
    Calc,
    /// Что вернул GSP-RM.
    Rm,
    /// `DOORBELL_CONFIG` runlist (`pri + 0x8`, биты 31:16) `<< 16 | chid` —
    /// как nouveau без GSP (`ga100_runl_new`).
    Dbcfg,
}

/// Выбор руками (`gpu nvktest ... token-rm|token-dbcfg`); `None` — сперва
/// выученный для runlist, иначе расчётный.
static FORCED_TOKEN: SpinLock<Option<TokenKind>> = SpinLock::new(None);
/// Что сработало на железе: (runlist, жетон). Новые каналы берут его.
static LEARNED_TOKEN: SpinLock<Vec<(u32, TokenKind)>> = SpinLock::new(Vec::new());

fn token_of(kind: TokenKind, runlist: u32, chid: u32, rm: Option<u32>, pri_base: u32) -> Option<u32> {
    match kind {
        TokenKind::Calc => Some(gpfifo::doorbell_token(runlist, chid)),
        TokenKind::Rm => rm,
        TokenKind::Dbcfg => {
            let (_, bar) = info::card().ok()?;
            let dbcfg = runlist_reg(bar, pri_base, 0x8)?;
            Some((dbcfg & 0xffff_0000) | chid)
        }
    }
}

/// Регистр runlist `pri + off`, если адрес похож на правду (окно BAR0 —
/// 16 МиБ; у SOFTWARE `pri` = 0).
fn runlist_reg(bar: super::nvidia::Bar0, pri_base: u32, off: u32) -> Option<u32> {
    if pri_base == 0 || pri_base >= 0x0100_0000 - 0x400 {
        return None;
    }
    Some(bar.rd((pri_base + off) as usize))
}

/// Регистры runlist и запись канала в его CHRAM — для журнала: включён ли
/// канал (CHRAM бит 1 ENABLE, 8 PENDING? — пишем сырьём), куда смотрит звонок.
fn runlist_state(pri_base: u32, chid: u32) -> String {
    let Ok((_, bar)) = info::card() else { return "no card".into() };
    let Some(chcfg) = runlist_reg(bar, pri_base, 0x4) else { return format!("runlist pri {pri_base:#x}: not readable") };
    let dbcfg = runlist_reg(bar, pri_base, 0x8).unwrap_or(0);
    let chram = chcfg & 0xffff_fff0;
    let entry = if chram != 0 && chram < 0x0100_0000 - 0x1000 && chcfg != 0xffff_ffff && chcfg >> 16 != 0xbadf {
        format!("{:#010x}", bar.rd((chram + chid * 4) as usize))
    } else {
        "-".into()
    };
    format!(
        "runlist pri {pri_base:#x}: chcfg {chcfg:#x} (chram {chram:#x}, {} ch) dbcfg {dbcfg:#x} (doorbell id {:#x}) submit {:#x} chram[{chid}] {entry}",
        1u32 << (chcfg & 0xf),
        dbcfg >> 16,
        runlist_reg(bar, pri_base, 0x88).unwrap_or(0),
    )
}

/// `gpu nvktest ... token-rm|token-calc|token-dbcfg`.
pub(crate) fn force_token(kind: Option<TokenKind>) {
    *FORCED_TOKEN.lock() = kind;
}

fn errno(log: &mut Log, what: &str, why: String) -> Errno {
    say(log, format!("nvgpu: {what} - {why}"));
    if why.contains("not running") || why.contains("busy") { Errno::EIO } else if why.contains("full") || why.contains("no ") && why.contains("pages") { Errno::ENOMEM } else { Errno::EINVAL }
}

/// Бэкенд, если поднят.
pub fn backend() -> Option<Arc<RmBackend>> {
    BACKEND.lock().clone()
}

/// Поднять бэкенд (раз на сеанс GSP): арены видеопамяти, сведения о карте.
/// Ничего на карте не создаёт.
pub fn attach(log: &mut Log) -> Res<Arc<RmBackend>> {
    if let Some(b) = backend() {
        // После выгрузки GSP-RM старый бэкенд мёртв — нужен новый.
        if !DEAD.load(Ordering::Acquire) {
            return Ok(b);
        }
        *BACKEND.lock() = None;
    }
    let stat = info::static_info().ok_or("GSP-RM is not running (gpu gsp first)")?;
    let (device, _) = info::card()?;
    vram::init(&stat, device, log)?;
    let hw = with_rm(log, |rm, internal| {
        let mut lines = Log::new();
        let r = info::query(rm, internal, &stat, vram::user_bytes(), &mut lines);
        (r, lines)
    })?;
    log.extend(hw.1);
    let hw = hw.0?;
    DEAD.store(false, Ordering::Release);
    super::nvidia_gsp::RC_CHIDS.lock().clear();
    let backend = Arc::new(RmBackend {
        info: hw.gpu.clone(),
        state: Mutex::new(State { hw, bos: BTreeMap::new(), vms: BTreeMap::new(), chans: BTreeMap::new(), next: 0, userd_pages: alloc::vec![false; 256] }),
    });
    *BACKEND.lock() = Some(backend.clone());
    // Слою DRM (WP-C): только регистрация — узел /dev/dri/renderD128
    // настоящей карты появляется после `nvk hw on`.
    let gpu: Arc<dyn GpuBackend> = backend.clone();
    crate::linux::drm::set_backend(Some(gpu));
    say(log, "nvgpu: backend attached (DRM node stays off until 'nvk hw on')".into());
    Ok(backend)
}

/// Выгрузка GSP-RM (`nvidia_gsp::unload`, сеанс уже взят): бэкенд мёртв,
/// клиенты RM NVK снимаются, арены и GR забываются.
pub(crate) fn shutdown(rm: &mut Rm) -> Log {
    let mut log = Log::new();
    DEAD.store(true, Ordering::Release);
    if let Some(mut b) = BACKEND.try_lock() {
        *b = None;
    }
    // Новые open узла DRM больше не увидят карту; открытые получают ENODEV.
    crate::linux::drm::set_backend(None);
    let clients: Vec<u32> = core::mem::take(&mut *super::nvidia_rm_vmm::LIVE_CLIENTS.lock());
    for client in clients {
        match rm.free_in(client, 0, client) {
            Ok(()) => log.push(format!("NVK client {client:#x} freed")),
            Err(why) => log.push(format!("NVK client {client:#x} free - {why}")),
        }
    }
    super::nvidia_rm_gr::reset();
    vram::reset();
    log
}

impl RmBackend {
    fn live(&self) -> Result<(), Errno> {
        if DEAD.load(Ordering::Acquire) { Err(Errno::ENODEV) } else { Ok(()) }
    }

    /// Сведения RM о карте (для `gpu info`).
    pub(crate) fn hw(&self) -> HwInfo {
        self.state.lock().hw.clone()
    }

    /// Канал на выбранном runlist (трейт создаёт на GR).
    pub(crate) fn chan_create_on(&self, vm: VmHandle, runlist: Runlist, log: &mut Log) -> Result<ChanHandle, Errno> {
        self.live()?;
        let mut st = self.state.lock();
        let st = &mut *st;
        let (engine, copy_engine, runlist_id) = match runlist {
            Runlist::Gr => {
                let ce = st.hw.gr_copy().map_or(info::ENGINE_COPY0, |e| e.rm_type);
                (info::ENGINE_GR0, ce, st.hw.gr_runlist.unwrap_or(0))
            }
            Runlist::AsyncCopy => {
                let e = st.hw.async_copy().cloned().ok_or_else(|| errno(log, "chan", "no async copy engine in the device table".into()))?;
                (e.rm_type, e.rm_type, e.runlist)
            }
        };
        let pri_base = st.hw.engines.iter().find(|e| e.rm_type == engine).map_or(0, |e| e.pri_base);
        // GR: golden context до первого канала (nouveau делает его перед
        // любым каналом; для опыта AsyncCopy — не обязательно).
        if runlist == Runlist::Gr && !super::nvidia_rm_gr::ready() {
            let hw = st.hw.clone();
            let r = with_rm(log, |rm, internal| {
                let mut lines = Log::new();
                let r = super::nvidia_rm_gr::golden(rm, internal, &hw, &mut lines);
                (r, lines)
            })
            .map_err(|why| errno(log, "gr golden", why))?;
            log.extend(r.1);
            r.0.map_err(|why| errno(log, "gr golden", why))?;
        }
        let page = st.userd_pages.iter().skip(1).position(|u| !u).map(|p| p + 1).ok_or(Errno::EBUSY)?;
        let chid = page as u32 * 8;
        let mthdbuf_size = st.hw.mthdbuf_size;
        let vmm = st.vms.get_mut(&vm.0).ok_or(Errno::ENOENT)?;
        let made = (|| -> Res<Chan> {
            let inst = vram::cpu_alloc(PAGE, PAGE)?;
            let userd = vram::cpu_alloc(PAGE, PAGE)?;
            vmm.join(inst);
            let push = Dma::new((PUSH_BYTES / PAGE) as usize)?;
            let push_va = vmm.kva_alloc(PUSH_BYTES, 0x1_0000)?;
            vmm.map_sys(push_va, &push.frames(), false, false, 0)?;
            let fence = Dma::new(1)?;
            let fence_va = vmm.kva_alloc(PAGE, PAGE)?;
            vmm.map_sys(fence_va, &fence.frames(), false, false, 0)?;
            let mthdbuf = Dma::new((mthdbuf_size as usize).div_ceil(PAGE as usize).max(1))?;
            Ok(Chan {
                vm,
                chid,
                handle: 0xf1f0_0000 | chid,
                runlist: runlist_id,
                engine,
                copy_engine,
                inst,
                userd,
                push,
                push_va,
                fence,
                fence_va,
                mthdbuf,
                ring: Ring::default(),
                kpush: 0,
                seq: 0,
                token: gpfifo::doorbell_token(runlist_id, chid),
                rm_token: None,
                pri_base,
                killed: false,
                objs: Vec::new(),
                grctx: None,
                submits: 0,
                progress: (0, crate::time::uptime_ms()),
            })
        })()
        .map_err(|why| errno(log, "chan memory", why))?;
        vmm.flush_tlb(log);
        let mut chan = made;
        let params = channel_params(&ChanParams {
            gpfifo_va: chan.push_va + RING_OFFSET,
            entries: RING_ENTRIES,
            chid,
            privileged: false,
            vaspace: H_VASPACE,
            engine,
            inst: chan.inst,
            userd: chan.userd,
            mthdbuf: (chan.mthdbuf.phys, mthdbuf_size),
        });
        let client = vmm.client;
        let pdb_before = vram::r64(chan.inst + 0x200);
        let r = with_rm(log, |rm, _| {
            let mut lines = Log::new();
            let alloc = rm.alloc_in(client, H_DEVICE, chan.handle, 0xc56f, &params);
            if let Err(why) = alloc {
                return Err(format!("{why} (params {})", super::nvidia_gsp::hex_head(&params, 0x100)));
            }
            let pdb_after = vram::r64(chan.inst + 0x200);
            let bind = rm.control(client, chan.handle, 0xa06f_0104, &engine.to_le_bytes());
            let sched = rm.control(client, chan.handle, 0xa06f_0103, &[1u8, 0]);
            let token = rm.control(client, chan.handle, 0xc36f_0108, &[0u8; 4]);
            let st = |r: &Res<Vec<u8>>| r.as_ref().map_or_else(|e| format!("FAILED {e}"), |_| "ok".into());
            let rm_token = token.as_ref().ok().and_then(|t| super::nvidia_gsp::le32(t, 0).ok());
            lines.push(format!(
                "nvgpu: chan {} chid {chid} runl {runlist_id} engine {engine} ce {copy_engine} token rm {} calc {:#x} flags {:#x} ring {:#x} userd {:#x} inst {:#x} \
                 pdb before {pdb_before:#x} after alloc {pdb_after:#x}{} bind {} sched {}",
                chan.handle,
                rm_token.map_or_else(|| st(&token), |t| format!("{t:#x}")),
                chan.token,
                super::nvidia_gsp::le32(&params, 0x14).unwrap_or(0),
                chan.push_va + RING_OFFSET,
                chan.userd,
                chan.inst,
                if pdb_after == pdb_before { "" } else { " (RM CHANGED PDB)" },
                st(&bind),
                st(&sched)
            ));
            info::bar1_check(rm.bar, &mut lines, "channel alloc");
            if bind.is_err() || sched.is_err() {
                let _ = rm.free_in(client, H_DEVICE, chan.handle);
                return Err("BIND/SCHEDULE failed".into());
            }
            Ok((rm_token, lines))
        })
        .and_then(|r| r);
        match r {
            Ok((token, lines)) => {
                for l in lines {
                    say(log, l);
                }
                chan.rm_token = token;
                // Жетон: заданный руками, выученный на этом runlist, иначе
                // расчётный (см. `TokenKind`).
                let forced = *FORCED_TOKEN.lock();
                let learned = LEARNED_TOKEN.lock().iter().find(|(r, _)| *r == runlist_id).map(|(_, k)| *k);
                let kind = forced.or(learned).unwrap_or(TokenKind::Calc);
                if let Some(t) = token_of(kind, runlist_id, chid, token, pri_base) {
                    chan.token = t;
                }
                say(log, format!("nvgpu: chan chid {chid} runl {runlist_id} rings token {:#x} ({kind:?}{}); {}", chan.token, if forced.is_some() { ", forced" } else if learned.is_some() { ", learned" } else { "" }, runlist_state(pri_base, chid)));
            }
            Err(why) => {
                release_chan(chan, vmm);
                return Err(errno(log, "chan alloc", why));
            }
        }
        st.userd_pages[page] = true;
        // Прежний канал с этим chid мог быть убит — его RC к новому не относится.
        super::nvidia_gsp::RC_CHIDS.lock().retain(|(c, _)| *c != chid);
        let h = st.handle();
        st.chans.insert(h, chan);
        Ok(ChanHandle(h))
    }

    /// Дождаться забора `seq` (не дольше `ms`), разбирая события GSP.
    pub(crate) fn wait_seq(&self, ch: ChanHandle, seq: u64, ms: u64, log: &mut Log) -> Result<u64, Errno> {
        let deadline = crate::time::uptime_ms() + ms;
        loop {
            info::pump_events(log);
            match self.chan_seq_done(ch) {
                Ok(done) if done >= seq => return Ok(done),
                Ok(_) => {}
                Err(e) => return Err(e),
            }
            if crate::time::uptime_ms() > deadline {
                say(log, format!("nvgpu: wait chan {} seq {seq} timed out after {ms} ms: {}", ch.0, self.chan_state(ch)));
                return Err(Errno::ETIMEDOUT);
            }
            super::nvidia_gsp::pause_briefly();
        }
    }

    /// Состояние канала для журнала (при ошибке).
    pub(crate) fn chan_state(&self, ch: ChanHandle) -> String {
        let st = self.state.lock();
        let Some(c) = st.chans.get(&ch.0) else { return "no such channel".into() };
        let bar = info::card()
            .map(|(_, b)| format!("doorbell token {:#x} (rm {:x?}), 0xb830b0 {:#x}; {}", c.token, c.rm_token, b.rd(0x00b8_30b0), runlist_state(c.pri_base, c.chid)))
            .unwrap_or_default();
        format!(
            "chid {} runlist {} engine {} GP_GET {} GP_PUT {} (ours {}) fence {} seq {} killed {} inst PDB {:#x} rc {:x?}; {}; {bar}",
            c.chid,
            c.runlist,
            c.engine,
            vram::r32(c.userd + GP_GET),
            vram::r32(c.userd + GP_PUT),
            c.ring.put,
            c.fence.r32(0),
            c.seq,
            c.killed,
            vram::r64(c.inst + 0x200),
            super::nvidia_gsp::RC_CHIDS.lock().clone(),
            st.vms.get(&c.vm.0).map_or_else(|| "no vm".into(), Vmm::describe)
        )
    }

    /// Канал стоит (GP_GET не двигается) — позвонить другими жетонами по
    /// очереди и записать, какой сдвинул GP_GET (19.11: один прогон на
    /// железе должен решить вопрос). Сработавший запоминается для runlist.
    /// Звонок чужим жетоном безвреден: он лишь просит планировщик проверить
    /// GP_PUT того канала.
    pub(crate) fn doorbell_rescue(&self, ch: ChanHandle, seq: u64, log: &mut Log) -> Option<TokenKind> {
        let (runlist, chid, current, rm_token, pri_base, userd) = {
            let st = self.state.lock();
            let c = st.chans.get(&ch.0)?;
            (c.runlist, c.chid, c.token, c.rm_token, c.pri_base, c.userd)
        };
        let get0 = vram::r32(userd + GP_GET);
        say(log, format!("nvgpu: doorbell runlist {runlist} chid {chid}: token {current:#x} gave no progress (GP_GET {get0}); {}", runlist_state(pri_base, chid)));
        let mut tried = alloc::vec![current];
        for kind in [TokenKind::Calc, TokenKind::Rm, TokenKind::Dbcfg] {
            let Some(token) = token_of(kind, runlist, chid, rm_token, pri_base) else {
                say(log, format!("nvgpu: doorbell runlist {runlist}: token {kind:?} unknown - skipped"));
                continue;
            };
            if tried.contains(&token) {
                continue;
            }
            tried.push(token);
            let Ok((_, bar)) = info::card() else { return None };
            fence(Ordering::SeqCst);
            bar.wr(gpfifo::DOORBELL as usize, token);
            let deadline = crate::time::uptime_ms() + 1500;
            let mut moved = false;
            while crate::time::uptime_ms() < deadline {
                if vram::r32(userd + GP_GET) != get0 || self.chan_seq_done(ch).is_ok_and(|d| d >= seq) {
                    moved = true;
                    break;
                }
                super::nvidia_gsp::pause_briefly();
            }
            say(
                log,
                format!(
                    "nvgpu: doorbell runlist {runlist} chid {chid} token {token:#x} ({kind:?}): {} (GP_GET {}); {}",
                    if moved { "WORKS" } else { "no progress" },
                    vram::r32(userd + GP_GET),
                    runlist_state(pri_base, chid)
                ),
            );
            if moved {
                if let Some(c) = self.state.lock().chans.get_mut(&ch.0) {
                    c.token = token;
                }
                let mut learned = LEARNED_TOKEN.lock();
                learned.retain(|(r, _)| *r != runlist);
                learned.push((runlist, kind));
                return Some(kind);
            }
        }
        None
    }

    /// Карта Bo → кадры/видеопамять (для самопроверки).
    pub(crate) fn bo_ptr(&self, bo: BoHandle) -> Option<Vec<u64>> {
        self.bo_frames(bo)
    }

    /// Как GPU увидит `va` в пространстве `vm` (для журнала).
    pub(crate) fn vm_pte(&self, vm: VmHandle, va: u64) -> String {
        self.state.lock().vms.get(&vm.0).map_or_else(|| "no vm".into(), |v| v.translate(va))
    }
}

/// Вернуть память канала (RM-объект уже снят или не создавался).
fn release_chan(chan: Chan, vmm: &mut Vmm) {
    let _ = vmm.unmap(chan.push_va, PUSH_BYTES, false);
    let _ = vmm.unmap(chan.fence_va, PAGE, false);
    vram::free(chan.inst, PAGE);
    vram::free(chan.userd, PAGE);
    if let Some(ctx) = chan.grctx {
        for (mem, size) in ctx {
            vram::free(mem, size);
        }
    }
    chan.push.free();
    chan.fence.free();
    chan.mthdbuf.free();
}


impl GpuBackend for RmBackend {
    fn info(&self) -> &GpuInfo {
        &self.info
    }

    fn bo_alloc(&self, size: u64, place: Place) -> Result<BoHandle, Errno> {
        self.live()?;
        if size == 0 {
            return Err(Errno::EINVAL);
        }
        let size = size.div_ceil(PAGE) * PAGE;
        let bo = match place {
            Place::Vram => {
                let mem = vram::bulk_alloc(size, 0x1_0000).map_err(|_| Errno::ENOMEM)?;
                Bo { place, size, vram: mem, frames: Vec::new() }
            }
            Place::Sysmem => {
                let mut frames = Vec::with_capacity((size / PAGE) as usize);
                for _ in 0..size / PAGE {
                    match crate::mm::frame::with(|f| f.allocate()).flatten() {
                        Some(p) => {
                            // SAFETY: кадр только что выделен; прямое отображение.
                            unsafe { core::ptr::write_bytes(p.to_direct_map().as_usize() as *mut u8, 0, PAGE as usize) };
                            frames.push(p.as_u64());
                        }
                        None => {
                            free_frames(&frames);
                            return Err(Errno::ENOMEM);
                        }
                    }
                }
                Bo { place, size, vram: 0, frames }
            }
        };
        let mut st = self.state.lock();
        let h = st.handle();
        st.bos.insert(h, bo);
        Ok(BoHandle(h))
    }

    fn bo_free(&self, bo: BoHandle) {
        let Some(b) = self.state.lock().bos.remove(&bo.0) else { return };
        match b.place {
            Place::Vram => vram::free(b.vram, b.size),
            Place::Sysmem => free_frames(&b.frames),
        }
    }

    fn bo_frames(&self, bo: BoHandle) -> Option<Vec<u64>> {
        self.state.lock().bos.get(&bo.0).filter(|b| b.place == Place::Sysmem).map(|b| b.frames.clone())
    }

    fn vm_create(&self, kernel_va: (u64, u64)) -> Result<VmHandle, Errno> {
        self.live()?;
        let mut log = Log::new();
        let mut st = self.state.lock();
        let h = st.handle();
        let vmm = with_rm(&mut log, |rm, _| {
            let mut lines = Log::new();
            let r = Vmm::create(rm, h, kernel_va, &mut lines);
            (r, lines)
        })
        .map_err(|why| errno(&mut log, "vm_create", why))?;
        log.extend(vmm.1);
        let vmm = vmm.0.map_err(|why| errno(&mut log, "vm_create", why))?;
        st.vms.insert(h, vmm);
        Ok(VmHandle(h))
    }

    fn vm_destroy(&self, vm: VmHandle) {
        let mut log = Log::new();
        let st = self.state.lock();
        // Каналы пространства — сначала.
        let chans: Vec<u32> = st.chans.iter().filter(|(_, c)| c.vm == vm).map(|(h, _)| *h).collect();
        drop(st);
        for c in chans {
            self.chan_destroy(ChanHandle(c));
        }
        let Some(mut vmm) = self.state.lock().vms.remove(&vm.0) else { return };
        if DEAD.load(Ordering::Acquire) {
            return;
        }
        let _ = with_rm(&mut log, |rm, _| vmm.destroy(rm, &mut Log::new()));
    }

    fn vm_map(&self, vm: VmHandle, va: u64, bo: BoHandle, bo_off: u64, len: u64, kind: u8) -> Result<(), Errno> {
        self.live()?;
        let kind = nvgpu::gmmu::pte_kind(kind)?;
        let mut st = self.state.lock();
        let st = &mut *st;
        let b = st.bos.get(&bo.0).ok_or(Errno::ENOENT)?;
        if bo_off % PAGE != 0 || len % PAGE != 0 || bo_off.checked_add(len).is_none_or(|e| e > b.size) {
            return Err(Errno::EINVAL);
        }
        let vmm = st.vms.get_mut(&vm.0).ok_or(Errno::ENOENT)?;
        let r = match b.place {
            Place::Sysmem => {
                let first = (bo_off / PAGE) as usize;
                vmm.map_sys(va, &b.frames[first..first + (len / PAGE) as usize], false, false, kind)
            }
            Place::Vram => vmm.map_vram(va, b.vram + bo_off, len, false, false, kind),
        };
        r.map_err(|why| errno(&mut Log::new(), "vm_map", why))
    }

    fn vm_sparse(&self, vm: VmHandle, va: u64, len: u64, on: bool) -> Result<(), Errno> {
        self.live()?;
        let mut st = self.state.lock();
        let vmm = st.vms.get_mut(&vm.0).ok_or(Errno::ENOENT)?;
        vmm.unmap(va, len, on).map_err(|why| errno(&mut Log::new(), "vm_sparse", why))
    }

    fn vm_unmap(&self, vm: VmHandle, va: u64, len: u64, restore_sparse: bool) -> Result<(), Errno> {
        self.live()?;
        let mut st = self.state.lock();
        let vmm = st.vms.get_mut(&vm.0).ok_or(Errno::ENOENT)?;
        vmm.unmap(va, len, restore_sparse).map_err(|why| errno(&mut Log::new(), "vm_unmap", why))
    }

    fn vm_flush(&self, vm: VmHandle) {
        if self.live().is_err() {
            return;
        }
        let st = self.state.lock();
        if let Some(vmm) = st.vms.get(&vm.0) {
            vmm.flush_tlb(&mut Log::new());
        }
    }

    fn chan_create(&self, vm: VmHandle) -> Result<ChanHandle, Errno> {
        self.chan_create_on(vm, Runlist::Gr, &mut Log::new())
    }

    fn chan_obj(&self, ch: ChanHandle, class: u32) -> Result<ObjHandle, Errno> {
        self.live()?;
        let mut log = Log::new();
        let mut st = self.state.lock();
        let st = &mut *st;
        let h = st.handle();
        let c = st.chans.get_mut(&ch.0).ok_or(Errno::ENOENT)?;
        let vmm = st.vms.get_mut(&c.vm.0).ok_or(Errno::ENOENT)?;
        let is_copy = class & 0xff == 0xb5;
        // Объект GR — сперва буферы контекста канала (раз на канал).
        let promote = !is_copy && c.grctx.is_none();
        let rm_handle = 0x0b00_0000 | (c.chid << 12) | (c.objs.len() as u32 & 0xfff);
        let params = if is_copy {
            let mut p = alloc::vec![0u8; 8];
            p[0..4].copy_from_slice(&1u32.to_le_bytes());
            p[4..8].copy_from_slice(&c.copy_engine.to_le_bytes());
            p
        } else {
            Vec::new()
        };
        let (client, handle, chid) = (vmm.client, c.handle, c.chid);
        let r = with_rm(&mut log, |rm, _| -> Res<Option<Vec<(u64, u64)>>> {
            let ctx = if promote { Some(super::nvidia_rm_gr::promote_channel(rm, vmm, handle, &mut Log::new())?) } else { None };
            let r = rm.alloc_in(client, handle, rm_handle, class, &params);
            crate::kprintln!(
                "  nvgpu: obj class {class:#x} chan {} chid {chid} rm {rm_handle:#x} status {}",
                ch.0,
                r.as_ref().map_or_else(|e| e.clone(), |_| "ok".into())
            );
            r?;
            Ok(ctx)
        });
        let ctx = r.and_then(|r| r).map_err(|why| errno(&mut log, "chan_obj", why))?;
        if let Some(ctx) = ctx {
            c.grctx = Some(ctx);
        }
        c.objs.push((h, rm_handle, class));
        Ok(ObjHandle(h))
    }

    fn chan_obj_free(&self, ch: ChanHandle, o: ObjHandle) {
        let mut st = self.state.lock();
        let st = &mut *st;
        let Some(c) = st.chans.get_mut(&ch.0) else { return };
        let Some(i) = c.objs.iter().position(|x| x.0 == o.0) else { return };
        let (_, rm_handle, _) = c.objs.remove(i);
        let Some(vmm) = st.vms.get(&c.vm.0) else { return };
        if !DEAD.load(Ordering::Acquire) {
            let (client, handle) = (vmm.client, c.handle);
            let _ = with_rm(&mut Log::new(), |rm, _| rm.free_in(client, handle, rm_handle));
        }
    }

    fn chan_destroy(&self, ch: ChanHandle) {
        let mut log = Log::new();
        let mut st = self.state.lock();
        let st = &mut *st;
        let Some(chan) = st.chans.remove(&ch.0) else { return };
        if let Some(p) = st.userd_pages.get_mut((chan.chid / 8) as usize) {
            *p = false;
        }
        let Some(vmm) = st.vms.get_mut(&chan.vm.0) else { return };
        if DEAD.load(Ordering::Acquire) {
            return; // RM и память карты уже не наши; кадры не трогаем.
        }
        // RM снимает канал (и объекты под ним) — после этого GPU не пишет
        // в его память, её можно вернуть.
        let (client, handle) = (vmm.client, chan.handle);
        let r = with_rm(&mut log, |rm, _| rm.free_in(client, H_DEVICE, handle));
        say(&mut log, format!("nvgpu: chan {} chid {} freed: {:?} ({} submits, seq {})", ch.0, chan.chid, r, chan.submits, chan.seq));
        release_chan(chan, vmm);
        vmm.flush_tlb(&mut log);
    }

    fn chan_submit(&self, ch: ChanHandle, pushes: &[Push]) -> Result<u64, Errno> {
        self.live()?;
        let mut st = self.state.lock();
        let c = st.chans.get_mut(&ch.0).ok_or(Errno::ENOENT)?;
        if c.killed || rc_hit(c.chid) {
            c.killed = true;
            return Err(Errno::ENODEV);
        }
        if pushes.len() > 510 {
            return Err(Errno::EINVAL);
        }
        for p in pushes {
            if p.va % 4 != 0 || p.len % 4 != 0 || p.len >= 1 << 23 {
                return Err(Errno::EINVAL);
            }
        }
        // Канал простаивал (всё прежнее выполнено) — сторож считает от этой
        // отправки, а не от создания канала.
        if gpfifo::widen_seq(c.seq, c.fence.r32(0)) >= c.seq {
            c.progress = (c.seq, crate::time::uptime_ms());
        }
        // Место в кольце: записи пользователя + сегмент забора.
        let need = pushes.len() as u32 + 1;
        let deadline = crate::time::uptime_ms() + 2000;
        while c.ring.free(vram::r32(c.userd + GP_GET) % RING_ENTRIES) < need {
            if crate::time::uptime_ms() > deadline {
                return Err(Errno::EBUSY);
            }
            super::nvidia_gsp::pause_briefly();
        }
        let write = |c: &mut Chan, va: u64, len: u32, np: bool| {
            let e = gpfifo::gp_entry(va, len, np);
            let at = (RING_OFFSET + Ring::offset(c.ring.advance())) as usize;
            c.push.w32(at, e as u32);
            c.push.w32(at + 4, (e >> 32) as u32);
        };
        for p in pushes {
            write(c, p.va, p.len, p.flags & Push::NO_PREFETCH != 0);
        }
        // Сегмент ядра: SEMAPHOREA..D + NON_STALL_INTERRUPT (6 слов).
        c.seq += 1;
        if (c.kpush as usize + gpfifo::FENCE_DWORDS) * 4 > KPUSH_BYTES as usize {
            c.kpush = 0;
        }
        let seg = c.kpush as usize * 4;
        for (i, w) in gpfifo::fence_release(c.fence_va, c.seq as u32).iter().enumerate() {
            c.push.w32(seg + i * 4, *w);
        }
        c.kpush += gpfifo::FENCE_DWORDS as u32;
        let seg_va = c.push_va + seg as u64;
        write(c, seg_va, (gpfifo::FENCE_DWORDS * 4) as u32, false);
        // Барьер: кольцо (память машины) раньше GP_PUT (видеопамять, WC).
        fence(Ordering::SeqCst);
        vram::w32(c.userd + GP_PUT, c.ring.put);
        vram::flush(c.userd + GP_PUT);
        let Ok((_, bar)) = info::card() else { return Err(Errno::ENODEV) };
        bar.wr(gpfifo::DOORBELL as usize, c.token);
        c.submits += 1;
        if c.submits <= 64 {
            crate::kprintln!(
                "  nvgpu: exec chan {} pushes {} put {} get {} seq {} fence {}",
                ch.0,
                pushes.len(),
                c.ring.put,
                vram::r32(c.userd + GP_GET),
                c.seq,
                c.fence.r32(0)
            );
        }
        Ok(c.seq)
    }

    fn chan_seq_done(&self, ch: ChanHandle) -> Result<u64, Errno> {
        let mut st = self.state.lock();
        let c = st.chans.get_mut(&ch.0).ok_or(Errno::ENOENT)?;
        if c.killed || rc_hit(c.chid) {
            c.killed = true;
            return Err(Errno::ENODEV);
        }
        // Забор — 32 бита: продолжить до 64 относительно последнего номера.
        let low = c.fence.r32(0);
        let done = gpfifo::widen_seq(c.seq, low);
        let now = crate::time::uptime_ms();
        if done != c.progress.0 || done >= c.seq {
            c.progress = (done, now);
        } else if now - c.progress.1 > WATCHDOG_MS {
            crate::kprintln!(
                "  nvgpu: watchdog chan {} seq {} stuck: GP_GET {} GP_PUT {} sem {} -> killed",
                ch.0,
                c.seq,
                vram::r32(c.userd + GP_GET),
                c.ring.put,
                low
            );
            c.killed = true;
            return Err(Errno::ENODEV);
        }
        Ok(done)
    }

    fn time_ns(&self) -> u64 {
        info::card().map_or(0, |(_, bar)| info::ptimer_ns(bar))
    }

    fn vram_used(&self) -> u64 {
        vram::used().max(1)
    }
}

/// RM объявил канал `chid` убитым.
fn rc_hit(chid: u32) -> bool {
    super::nvidia_gsp::RC_CHIDS.lock().iter().any(|(c, _)| *c == chid)
}

// --- самопроверки (shell: gpu info, gpu nvktest) -------------------------------

fn frame_ptr(phys: u64) -> *mut u32 {
    PhysAddr::new(phys).to_direct_map().as_usize() as *mut u32
}

/// `gpu info` (веха M1): арены, сведения RM, PTIMER идёт.
pub fn info_report() -> Log {
    let mut log = Log::new();
    let backend = match attach(&mut log) {
        Ok(b) => b,
        Err(why) => {
            say(&mut log, format!("nvgpu: attach FAILED - {why}"));
            say(&mut log, "nvgpu: SELFTEST m1 FAIL".into());
            return log;
        }
    };
    let hw = backend.hw();
    let i = &backend.info;
    say(
        &mut log,
        format!(
            "nvgpu: chip {:x} {} '{}' vram user {} MiB bar1 {} MiB gpc {} tpc {} graph_units {:#x} classes {:x?}",
            i.chipset,
            i.chip,
            i.name,
            i.vram_bytes >> 20,
            i.bar_bytes >> 20,
            i.gpcs,
            i.tpcs,
            i.graph_units(),
            i.classes
        ),
    );
    say(
        &mut log,
        format!(
            "nvgpu: gr runlist {:?}, gr copy {:?}, async copy {:?}, arenas {}, vram used {} KiB, golden {}",
            hw.gr_runlist,
            hw.gr_copy().map(|e| (e.rm_type, e.runlist)),
            hw.async_copy().map(|e| (e.rm_type, e.runlist)),
            vram::describe(),
            backend.vram_used() >> 10,
            if super::nvidia_rm_gr::ready() { "done" } else { "not yet" }
        ),
    );
    let t0 = backend.time_ns();
    crate::sched::sleep_ms(10);
    let t1 = backend.time_ns();
    let ticking = t1 > t0 && t1 - t0 >= 1_000_000 && t1 - t0 < 1_000_000_000;
    say(&mut log, format!("nvgpu: ptimer {t0} -> {t1} ns over 10 ms ({})", if ticking { "ticking" } else { "NOT ticking as expected" }));
    say(&mut log, format!("nvgpu: SELFTEST m1 {}", if ticking { "PASS" } else { "FAIL" }));
    log
}

/// Один прогон копирования: память машины → видеопамять → память машины
/// через VA канала, с семафорами CE и забором канала.
fn copy_test(b: &RmBackend, runlist: Runlist, log: &mut Log) -> Res<(u64, u64)> {
    const SIZE: u64 = 1 << 20;
    const BASE: u64 = 0x1_0000_0000;
    let e = |what: &'static str| move |err: Errno| format!("{what}: errno {}", err.0);
    let started = crate::time::uptime_ms();
    let vm = b.vm_create((1 << 39, 1 << 39)).map_err(e("vm_create"))?;
    let mut bos: Vec<BoHandle> = Vec::new();
    let mut chan = None;
    let result = (|| -> Res<(u64, u64)> {
        let ch = b.chan_create_on(vm, runlist, log).map_err(e("chan_create"))?;
        chan = Some(ch);
        if runlist == Runlist::Gr {
            for class in [0xc797u32, 0xc7c0] {
                let r = b.chan_obj(ch, class);
                say(log, format!("nvgpu: nvktest: class {class:#x} on the GR channel: {r:?}"));
                r.map_err(e("gr object"))?;
            }
        }
        b.chan_obj(ch, 0xc7b5).map_err(e("copy object"))?;
        let mut new_bo = |size: u64, place: Place| -> Res<BoHandle> {
            let bo = b.bo_alloc(size, place).map_err(e("bo_alloc"))?;
            bos.push(bo);
            Ok(bo)
        };
        let src = new_bo(SIZE, Place::Sysmem)?;
        let mid = new_bo(SIZE, Place::Vram)?;
        let dst = new_bo(SIZE, Place::Sysmem)?;
        let pb = new_bo(PAGE, Place::Sysmem)?;
        let sem = new_bo(PAGE, Place::Sysmem)?;
        let (src_va, mid_va, dst_va, pb_va, sem_va) = (BASE, BASE + SIZE, BASE + 2 * SIZE, BASE + 3 * SIZE, BASE + 3 * SIZE + PAGE);
        for (bo, va, len) in [(src, src_va, SIZE), (mid, mid_va, SIZE), (dst, dst_va, SIZE), (pb, pb_va, PAGE), (sem, sem_va, PAGE)] {
            b.vm_map(vm, va, bo, 0, len, 0).map_err(e("vm_map"))?;
        }
        b.vm_flush(vm);
        say(
            log,
            format!(
                "nvgpu: nvktest: gpu view: src {} | vram {} | dst {} | pb {} | sem {}",
                b.vm_pte(vm, src_va),
                b.vm_pte(vm, mid_va),
                b.vm_pte(vm, dst_va),
                b.vm_pte(vm, pb_va),
                b.vm_pte(vm, sem_va)
            ),
        );
        let src_frames = b.bo_ptr(src).ok_or("src frames")?;
        let dst_frames = b.bo_ptr(dst).ok_or("dst frames")?;
        let words_per_page = (PAGE / 4) as usize;
        for (i, &f) in src_frames.iter().enumerate() {
            for j in 0..words_per_page {
                // SAFETY: кадр буфера src, отображение прямое.
                unsafe { frame_ptr(f).add(j).write_volatile(((i * words_per_page + j) as u32) ^ 0x5a5a_0000) };
            }
        }
        for &f in &dst_frames {
            for j in 0..words_per_page {
                // SAFETY: кадр буфера dst.
                unsafe { frame_ptr(f).add(j).write_volatile(0xdead_beef) };
            }
        }
        // NVC7B5 на подканале 4 (как у NVK): копия с выпуском семафора CE.
        let mut pbuild = PushBuilder::new();
        pbuild.set_object(4, 0xc7b5);
        pbuild.ce_copy(4, src_va, mid_va, SIZE as u32, Some((sem_va, 1)));
        pbuild.ce_copy(4, mid_va, dst_va, SIZE as u32, Some((sem_va, 2)));
        let words = pbuild.words;
        let pb_frame = b.bo_ptr(pb).ok_or("pb frames")?[0];
        let sem_frame = b.bo_ptr(sem).ok_or("sem frames")?[0];
        for (i, w) in words.iter().enumerate() {
            // SAFETY: кадр пушбуфера (4 КиБ, слов меньше 1024).
            unsafe { frame_ptr(pb_frame).add(i).write_volatile(*w) };
        }
        fence(Ordering::SeqCst);
        let t0 = b.time_ns();
        let seq = b.chan_submit(ch, &[Push { va: pb_va, len: words.len() as u32 * 4, flags: 0 }]).map_err(e("chan_submit"))?;
        // Сперва 1,5 с выбранным жетоном; стоит — перебор жетонов (обе
        // попытки в журнале), потом ещё 3,5 с.
        let mut waited = b.wait_seq(ch, seq, 1500, log);
        if waited == Err(Errno::ETIMEDOUT) {
            let rescued = b.doorbell_rescue(ch, seq, log);
            say(log, format!("nvgpu: nvktest {runlist:?}: doorbell rescue -> {}", rescued.map_or_else(|| "nothing moved the channel".into(), |k| format!("{k:?} token works"))));
            waited = b.wait_seq(ch, seq, 3500, log);
        }
        let t1 = b.time_ns();
        // SAFETY: кадр семафора.
        let sem_value = unsafe { frame_ptr(sem_frame).read_volatile() };
        say(
            log,
            format!(
                "nvgpu: nvktest: submit seq {seq} -> {waited:?} in {} us (ptimer), ce semaphore {sem_value} (want 2); {}",
                t1.saturating_sub(t0) / 1000,
                b.chan_state(ch)
            ),
        );
        waited.map_err(e("fence wait"))?;
        let mut mismatches = 0u64;
        let mut first = None;
        for (i, (&s, &d)) in src_frames.iter().zip(dst_frames.iter()).enumerate() {
            for j in 0..words_per_page {
                // SAFETY: кадры буферов src/dst.
                let (a, c) = unsafe { (frame_ptr(s).add(j).read_volatile(), frame_ptr(d).add(j).read_volatile()) };
                if a != c {
                    mismatches += 1;
                    if first.is_none() {
                        first = Some((i * words_per_page + j, a, c));
                    }
                }
            }
        }
        if let Some((at, want, got)) = first {
            say(log, format!("nvgpu: nvktest: first mismatch at word {at}: want {want:#x} got {got:#x}"));
        }
        Ok((SIZE, mismatches))
    })();
    if let Some(ch) = chan {
        b.chan_destroy(ch);
    }
    for bo in bos {
        b.bo_free(bo);
    }
    b.vm_destroy(vm);
    say(log, format!("nvgpu: nvktest {runlist:?}: cleaned up after {} ms", crate::time::uptime_ms() - started));
    result
}

/// `gpu nvktest [ce|gr] [skip-pdes]` (веха M2): пространство, канал, объект
/// копирования, копия 1 МиБ память машины → видеопамять → память машины с
/// семафорами; по умолчанию — сперва канал асинхронного CE (без GR), потом
/// канал GR (golden context, 3D, compute, copy).
pub fn nvktest(argument: &str) -> Log {
    let mut log = Log::new();
    let words: Vec<&str> = argument.split_whitespace().collect();
    super::nvidia_rm_vmm::SKIP_PDES.store(words.contains(&"skip-pdes"), Ordering::Relaxed);
    force_token(if words.contains(&"token-rm") {
        Some(TokenKind::Rm)
    } else if words.contains(&"token-dbcfg") {
        Some(TokenKind::Dbcfg)
    } else if words.contains(&"token-calc") {
        Some(TokenKind::Calc)
    } else {
        None
    });
    let modes: &[Runlist] = if words.contains(&"ce") {
        &[Runlist::AsyncCopy]
    } else if words.contains(&"gr") {
        &[Runlist::Gr]
    } else {
        &[Runlist::AsyncCopy, Runlist::Gr]
    };
    say(&mut log, format!("nvgpu: nvktest start: modes {modes:?}, skip-pdes {}", words.contains(&"skip-pdes")));
    let backend = match attach(&mut log) {
        Ok(b) => b,
        Err(why) => {
            say(&mut log, format!("nvgpu: SELFTEST m2 ce-copy FAIL (attach - {why})"));
            return log;
        }
    };
    for &mode in modes {
        match copy_test(&backend, mode, &mut log) {
            Ok((bytes, 0)) => say(&mut log, format!("nvgpu: SELFTEST m2 ce-copy {mode:?} PASS (bytes {bytes} mismatches 0)")),
            Ok((bytes, bad)) => say(&mut log, format!("nvgpu: SELFTEST m2 ce-copy {mode:?} FAIL (bytes {bytes} mismatches {bad} words)")),
            Err(why) => say(&mut log, format!("nvgpu: SELFTEST m2 ce-copy {mode:?} FAIL ({why})")),
        }
        info::pump_events(&mut log);
    }
    log
}
