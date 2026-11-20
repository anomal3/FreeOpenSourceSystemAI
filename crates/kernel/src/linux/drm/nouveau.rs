//! ioctl nouveau (раздел 1 и 2.1 спецификации): `GETPARAM`,
//! `CHANNEL_ALLOC/FREE`, NVIF (в [`super::nvif`]), `VM_INIT`, `VM_BIND`,
//! `EXEC`, `GEM_*`.
//!
//! `EXEC` в v1 синхронный в одном месте: заборы ожидания (`wait`) с других
//! каналов ждутся здесь же, процессором (риск 8 спецификации); заборы того же
//! канала не ждутся — порядок даёт GPFIFO. `VM_BIND` применяется сразу после
//! своих ожиданий, его сигналы — уже сигнальные заборы.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use nvgpu::vaspace::BindOp;
use nvgpu::{ChanHandle, GpuBackend, ObjHandle, Push};

use super::sync::{Fence, Syncobj};
use super::{DrmFile, get, get_array, put};
use crate::linux::e;
use crate::sync::Mutex;

// Полные номера (раздел 3.4); NVIF — только по `nr` (libdrm шлёт его как
// `DRM_IOW(0x47, size)`), размер переменный.
use nvgpu::uapi::{
    DRM_COMMAND_BASE, DRM_IOCTL_NOUVEAU_CHANNEL_ALLOC as CHANNEL_ALLOC, DRM_IOCTL_NOUVEAU_CHANNEL_FREE as CHANNEL_FREE,
    DRM_IOCTL_NOUVEAU_EXEC as EXEC, DRM_IOCTL_NOUVEAU_GEM_CPU_FINI as GEM_CPU_FINI,
    DRM_IOCTL_NOUVEAU_GEM_CPU_PREP as GEM_CPU_PREP, DRM_IOCTL_NOUVEAU_GEM_INFO as GEM_INFO,
    DRM_IOCTL_NOUVEAU_GEM_NEW as GEM_NEW, DRM_IOCTL_NOUVEAU_GEM_PUSHBUF as GEM_PUSHBUF,
    DRM_IOCTL_NOUVEAU_GETPARAM as GETPARAM, DRM_IOCTL_NOUVEAU_VM_BIND as VM_BIND, DRM_IOCTL_NOUVEAU_VM_INIT as VM_INIT,
    DRM_NOUVEAU_NVIF, DRM_NOUVEAU_SYNC_TIMELINE_SYNCOBJ as SYNC_TIMELINE, DRM_NOUVEAU_SYNC_TYPE_MASK as SYNC_TYPE_MASK,
    DRM_NOUVEAU_VM_BIND_RUN_ASYNC as BIND_RUN_ASYNC,
    NOUVEAU_EXEC_PUSH_MAX as EXEC_PUSH_MAX, NOUVEAU_EXEC_PUSH_MAX_LEN, NOUVEAU_VM_BIND_MAX_OPS as MAX_BIND_OPS,
    NouveauChannelAlloc as ChannelAlloc, NouveauExec as Exec, NouveauExecPush as ExecPush, NouveauSubchan,
    NouveauSync as DrmSync, NouveauVmBind as VmBind, NouveauVmBindOp as VmBindOp, NouveauVmInit,
};
const NR_NVIF: u32 = DRM_COMMAND_BASE + DRM_NOUVEAU_NVIF;

/// ioctl nouveau; `None` — номер не отсюда.
pub fn ioctl(file: &DrmFile, request: u32, arg: usize) -> Option<i64> {
    if request & 0xff == NR_NVIF {
        let size = ((request >> 16) & 0x3fff) as usize;
        return Some(super::nvif::ioctl(file, arg, size));
    }
    Some(match request {
        GETPARAM => getparam(file, arg),
        CHANNEL_ALLOC => channel_alloc(file, arg),
        CHANNEL_FREE => channel_free(file, arg),
        VM_INIT => vm_init(file, arg),
        VM_BIND => vm_bind(file, arg),
        EXEC => exec(file, arg),
        GEM_NEW => super::gem::gem_new(file, arg),
        GEM_INFO => super::gem::gem_info(file, arg),
        GEM_CPU_PREP => super::gem::gem_cpu_prep(file, arg),
        GEM_CPU_FINI => {
            let Ok(handle) = get::<u32>(arg) else { return Some(-e::EFAULT) };
            if file.st.lock().bos.contains_key(&handle) { 0 } else { -e::ENOENT }
        }
        // Старый путь подачи; в режиме uvmm Linux отвечает `ENOSYS`.
        GEM_PUSHBUF => -e::ENOSYS,
        _ => return None,
    })
}

fn errno(err: nvgpu::Errno) -> i64 {
    -i64::from(err.0)
}

// ---- GETPARAM ------------------------------------------------------------------

fn getparam(file: &DrmFile, arg: usize) -> i64 {
    let Ok([param, _]) = get::<[u64; 2]>(arg) else { return -e::EFAULT };
    let gpu = &file.gpu;
    let info = gpu.info();
    let value: u64 = match param {
        3 => u64::from(info.pci_vendor),  // PCI_VENDOR
        4 => u64::from(info.pci_device),  // PCI_DEVICE
        5 => 2,                           // BUS_TYPE: PCIE
        8 => info.vram_bytes,             // FB_SIZE
        9 => info.gart_bytes,             // AGP_SIZE
        11 => u64::from(info.chipset),    // CHIPSET_ID
        12 => 0,                          // VM_VRAM_BASE
        13 => info.graph_units(),         // GRAPH_UNITS
        14 => gpu.time_ns(),              // PTIMER_TIME
        15 | 16 => 1,                     // HAS_BO_USAGE, HAS_PAGEFLIP
        17 => u64::from(EXEC_PUSH_MAX),   // EXEC_PUSH_MAX
        18 => info.bar_bytes,             // VRAM_BAR_SIZE
        19 => gpu.vram_used().max(1),     // VRAM_USED (ноль Mesa читает как «нет»)
        20 => 1,                          // HAS_VMA_TILEMODE
        _ => return -e::EINVAL,
    };
    if put(arg + 8, value).is_err() { -e::EFAULT } else { 0 }
}

// ---- каналы ----------------------------------------------------------------------

/// То, что о канале нужно заборам: живут дольше канала (syncobj может
/// держать забор сколько угодно).
pub struct ChanCore {
    gpu: Arc<dyn GpuBackend>,
    pub handle: ChanHandle,
    /// Контекст заборов в реестре `sync`.
    pub ctx: u64,
    /// Последний поданный номер забора.
    pub last: AtomicU64,
    /// Последний известный выполненный номер (кэш для `sync::done`; убитый
    /// канал — `u64::MAX`).
    pub completed: AtomicU64,
    /// Канал снят: все его заборы считаются выполненными.
    gone: AtomicBool,
}

impl ChanCore {
    /// Спросить бэкенд и освежить кэш; ответ — выполненный номер.
    pub fn poll(&self) -> u64 {
        if self.gone.load(Ordering::Acquire) {
            return u64::MAX;
        }
        let done = self.gpu.chan_seq_done(self.handle).unwrap_or(u64::MAX);
        self.completed.fetch_max(done, Ordering::AcqRel);
        self.completed.load(Ordering::Acquire)
    }

    /// Выполнен ли номер `seq` (с вопросом к бэкенду). Убитый канал —
    /// «выполнен» (с ошибкой, как у Linux).
    #[must_use]
    pub fn done(&self, seq: u64) -> bool {
        self.poll() >= seq
    }

    /// То же по кэшу — без вопроса к бэкенду (можно под замками).
    #[must_use]
    pub fn cached_done(&self, seq: u64) -> bool {
        self.gone.load(Ordering::Acquire) || self.completed.load(Ordering::Acquire) >= seq
    }

    #[must_use]
    pub fn idle(&self) -> bool {
        self.done(self.last.load(Ordering::Acquire))
    }

    /// Убит ли канал (ошибка GPU, таймаут).
    #[must_use]
    pub fn killed(&self) -> bool {
        !self.gone.load(Ordering::Acquire) && self.gpu.chan_seq_done(self.handle).is_err()
    }
}

/// Канал файла: владеет каналом бэкенда и объектами движков в нём.
pub struct Chan {
    pub core: Arc<ChanCore>,
    /// Объекты движков: печенье NVIF → ручка бэкенда.
    pub objs: Mutex<BTreeMap<u64, ObjHandle>>,
}

impl Drop for Chan {
    /// Снять канал: дождаться его заборов (до 2 с, дальше — как есть: канал
    /// всё равно уходит), снять объекты, потом сам канал.
    fn drop(&mut self) {
        let core = &self.core;
        let deadline = crate::time::uptime_ns() + 2_000_000_000;
        if super::sync::wait_until(Some(deadline), || Ok(core.idle())).is_err() {
            crate::kprintln!("nvgpu: drm chan {} not idle at free (seq {})", core.handle.0, core.last.load(Ordering::Relaxed));
        }
        for (_, obj) in core::mem::take(&mut *self.objs.lock()) {
            core.gpu.chan_obj_free(core.handle, obj);
        }
        core.gpu.chan_destroy(core.handle);
        core.gone.store(true, Ordering::Release);
        super::sync::unregister(core.ctx);
    }
}

fn channel_alloc(file: &DrmFile, arg: usize) -> i64 {
    let Ok(mut args) = get::<ChannelAlloc>(arg) else { return -e::EFAULT };
    // NVK не выбирает движок: оба ctxdma нулевые — канал GR. Другие
    // движки (`fb_ctxdma_handle == ~0`) — нет.
    if args.fb_ctxdma_handle == !0 || args.tt_ctxdma_handle == !0 {
        return -e::ENOSYS;
    }
    let mut st = file.st.lock();
    let Some(vm) = st.vm.as_ref() else {
        // Как Linux: канал до `VM_INIT` навсегда выключает uvmm файла; у нас
        // каналов без VM нет.
        st.vm_disabled = true;
        return -e::ENODEV;
    };
    let handle = match file.gpu.chan_create(vm.handle) {
        Ok(handle) => handle,
        Err(err) => return errno(err),
    };
    let core = Arc::new(ChanCore {
        gpu: Arc::clone(&file.gpu),
        handle,
        ctx: super::sync::new_ctx(),
        last: AtomicU64::new(0),
        completed: AtomicU64::new(0),
        gone: AtomicBool::new(false),
    });
    super::sync::register(&core);
    let id = handle.0;
    st.chans.insert(id, Arc::new(Chan { core, objs: Mutex::new(BTreeMap::new()) }));
    drop(st);
    args.channel = id as i32;
    args.pushbuf_domains = super::gem::DOMAIN_VRAM | super::gem::DOMAIN_GART;
    args.nr_subchan = 0;
    args.subchan = [NouveauSubchan { handle: 0, grclass: 0 }; 8];
    if put(arg, args).is_err() {
        let chan = file.st.lock().chans.remove(&id);
        drop(chan);
        return -e::EFAULT;
    }
    if super::trace() {
        crate::kprintln!("nvgpu: drm chan {id} alloc");
    }
    0
}

fn channel_free(file: &DrmFile, arg: usize) -> i64 {
    let Ok(channel) = get::<i32>(arg) else { return -e::EFAULT };
    let chan = {
        let mut st = file.st.lock();
        let chan = st.chans.remove(&(channel as u32));
        // Объекты NVIF этого канала уходят вместе с ним.
        st.nvif.retain(|_, obj| !matches!(obj, super::nvif::NvifObj::Engine { chan, .. } if *chan == channel as u32));
        chan
    };
    // Снимается без замка файла: ожидание заборов может быть долгим.
    match chan {
        Some(chan) => {
            drop(chan);
            0
        }
        None => -e::ENOENT,
    }
}

/// Канал файла по номеру.
pub fn chan(file: &DrmFile, id: u32) -> Option<Arc<Chan>> {
    file.st.lock().chans.get(&id).cloned()
}

// ---- VM_INIT / VM_BIND -------------------------------------------------------------

fn vm_init(file: &DrmFile, arg: usize) -> i64 {
    let Ok(NouveauVmInit { kernel_managed_addr: addr, kernel_managed_size: size }) = get::<NouveauVmInit>(arg) else {
        return -e::EFAULT;
    };
    let mut st = file.st.lock();
    if st.vm_disabled {
        return -e::ENOSYS;
    }
    if st.vm.is_some() {
        return -e::EEXIST;
    }
    match super::vm::Vm::new(&file.gpu, addr, size) {
        Ok(vm) => {
            st.vm = Some(vm);
            0
        }
        Err(err) => err,
    }
}

/// Разобранный сигнал/ожидание: объект и точка (0 — двоичный).
struct SyncRef {
    obj: Arc<Syncobj>,
    point: u64,
}

fn syncs(file: &DrmFile, ptr: u64, count: u32) -> Result<Vec<SyncRef>, i64> {
    if count > 1024 {
        return Err(-e::EINVAL);
    }
    let raw = get_array::<DrmSync>(ptr, count as usize)?;
    let st = file.st.lock();
    raw.iter()
        .map(|sync| {
            if sync.flags & !SYNC_TYPE_MASK != 0 || sync.flags & SYNC_TYPE_MASK > SYNC_TIMELINE {
                return Err(-e::EINVAL);
            }
            let obj = st.syncobjs.get(&sync.handle).cloned().ok_or(-e::ENOENT)?;
            let point = if sync.flags & SYNC_TYPE_MASK == SYNC_TIMELINE { sync.timeline_value } else { 0 };
            Ok(SyncRef { obj, point })
        })
        .collect()
}

/// Дождаться заборов `waits` (кроме упорядоченных на канале `on`): сперва
/// подачи (Mesa подаёт ожидания раньше сигналов, но потоки разные), затем
/// выполнения.
fn wait_syncs(waits: &[SyncRef], on: Option<&Arc<ChanCore>>) -> Result<(), i64> {
    for wait in waits {
        let mut fence = None;
        super::sync::wait_until(None, || {
            fence = wait.obj.fence(wait.point);
            Ok(fence.is_some())
        })?;
        let fence = fence.expect("fence present");
        if on.is_some_and(|chan| super::sync::ordered_on(&fence, chan)) {
            continue;
        }
        super::sync::wait_until(None, || Ok(super::sync::signaled(&fence)))?;
    }
    Ok(())
}

fn signal_syncs(sigs: &[SyncRef], fence: &Fence) {
    for sig in sigs {
        sig.obj.add_point(sig.point, fence.clone());
    }
}

fn vm_bind(file: &DrmFile, arg: usize) -> i64 {
    let Ok(args) = get::<VmBind>(arg) else { return -e::EFAULT };
    if args.flags & !BIND_RUN_ASYNC != 0 {
        return -e::EINVAL;
    }
    // Синхронный вызов без ожиданий и сигналов (`nouveau_uvmm_vm_bind_ucopy`).
    if args.flags & BIND_RUN_ASYNC == 0 && (args.wait_count != 0 || args.sig_count != 0) {
        return -e::EINVAL;
    }
    if args.op_count > MAX_BIND_OPS {
        return -e::EINVAL;
    }
    let ops = match get_array::<VmBindOp>(args.op_ptr, args.op_count as usize) {
        Ok(ops) => ops,
        Err(err) => return err,
    };
    let (waits, sigs) = match (syncs(file, args.wait_ptr, args.wait_count), syncs(file, args.sig_ptr, args.sig_count)) {
        (Ok(waits), Ok(sigs)) => (waits, sigs),
        (Err(err), _) | (_, Err(err)) => return err,
    };
    // Разбор операций до всякого ожидания (ошибки — сразу): буферы
    // ищутся по ручкам сейчас, ссылки на них держит пачка.
    let mut parsed = Vec::with_capacity(ops.len());
    let mut bos = Vec::new();
    {
        let st = file.st.lock();
        for op in &ops {
            let mut found = None;
            let one = BindOp::parse(op, |handle| {
                let bo = st.bos.get(&handle)?;
                found = Some(Arc::clone(bo));
                Some((bo.handle, bo.size))
            });
            match one {
                Ok(one) => parsed.push(one),
                Err(err) => {
                    if super::trace() {
                        crate::kprintln!("nvgpu: drm vm_bind op {} flags {:#x} va {:#x} range {:#x} -> parse {}", op.op, op.flags, op.addr, op.range, err.0);
                    }
                    return -i64::from(err.0);
                }
            }
            bos.extend(found);
        }
    }
    if let Err(err) = wait_syncs(&waits, None) {
        return err;
    }
    let result = {
        let mut st = file.st.lock();
        let Some(vm) = st.vm.as_mut() else { return -e::ENOSYS };
        vm.bind(&parsed, bos)
    };
    match result {
        Ok(()) => {
            signal_syncs(&sigs, &Fence::signaled());
            0
        }
        Err(err) => {
            if super::trace() {
                crate::kprintln!("nvgpu: drm vm_bind {} ops -> {err}", parsed.len());
            }
            err
        }
    }
}

// ---- EXEC ------------------------------------------------------------------------

/// Счётчик `EXEC` для журнала (первые 64 — построчно).
static EXECS: AtomicU64 = AtomicU64::new(0);

fn exec(file: &DrmFile, arg: usize) -> i64 {
    let Ok(args) = get::<Exec>(arg) else { return -e::EFAULT };
    let Some(chan) = chan(file, args.channel) else { return -e::ENOENT };
    if args.push_count > EXEC_PUSH_MAX {
        return -e::EINVAL;
    }
    let raw = match get_array::<ExecPush>(args.push_ptr, args.push_count as usize) {
        Ok(raw) => raw,
        Err(err) => return err,
    };
    let mut pushes = Vec::with_capacity(raw.len());
    for push in &raw {
        if push.va % 4 != 0 || push.va_len % 4 != 0 || push.va_len >= NOUVEAU_EXEC_PUSH_MAX_LEN || push.flags & !Push::NO_PREFETCH != 0 {
            return -e::EINVAL;
        }
        pushes.push(Push { va: push.va, len: push.va_len, flags: push.flags });
    }
    let (waits, sigs) = match (syncs(file, args.wait_ptr, args.wait_count), syncs(file, args.sig_ptr, args.sig_count)) {
        (Ok(waits), Ok(sigs)) => (waits, sigs),
        (Err(err), _) | (_, Err(err)) => return err,
    };
    let core = Arc::clone(&chan.core);
    // Убитый канал: пустой `EXEC` — так NVK и спрашивает (раздел 1.3).
    if core.killed() {
        return -e::ENODEV;
    }
    if let Err(err) = wait_syncs(&waits, Some(&core)) {
        return err;
    }
    let seq = if pushes.is_empty() {
        core.last.load(Ordering::Acquire)
    } else {
        match file.gpu.chan_submit(core.handle, &pushes) {
            Ok(seq) => {
                core.last.fetch_max(seq, Ordering::AcqRel);
                seq
            }
            Err(err) => return errno(err),
        }
    };
    let n = EXECS.fetch_add(1, Ordering::Relaxed);
    if n < 64 || super::trace() {
        crate::kprintln!(
            "nvgpu: exec chan {} pushes {} waits {} sigs {} seq {}",
            core.handle.0,
            pushes.len(),
            waits.len(),
            sigs.len(),
            seq
        );
    }
    signal_syncs(&sigs, &super::sync::chan_fence(&core, seq));
    drop(chan);
    0
}
