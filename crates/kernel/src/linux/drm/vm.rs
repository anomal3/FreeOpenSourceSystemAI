//! Адресное пространство GPU файла (`VM_INIT`/`VM_BIND`, как `nouveau_uvmm`).
//!
//! Правила и учёт — `nvgpu::vaspace::VaSpace` (WP-B, проверено на хосте):
//! пачка операций проверяется целиком и атомарно превращается в действия для
//! бэкенда (`apply` пишет таблицы и сбрасывает TLB). Здесь — только ссылки на
//! буферы: пока буфер где-то отображён, его кадры не уходят, даже если ручку
//! уже закрыли (`GEM_CLOSE` после `VM_BIND` — обычное дело у NVK).
//!
//! hw-log17 (D2R, 23.11.2026): VM_BIND took 5-15 ms per call late in the
//! session and ran under the file lock, so EXEC of the same file waited
//! behind it. Now the space has its own lock ([`Vm`] is shared, `VM_BIND`
//! holds only that), buffers are released by the batch's own list (not by a
//! scan over all mappings), and every phase is timed for perf-diag
//! ([`perf_line`]).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use nvgpu::vaspace::{Action, BindOp, VaSpace, apply};
use nvgpu::{BoHandle, GpuBackend, VmHandle};

use super::Bo;
use crate::sync::Mutex;

fn errno(err: nvgpu::Errno) -> i64 {
    -i64::from(err.0)
}

/// The address space of a file; shared by `VM_BIND` calls (its own lock) and
/// channels (the handle).
pub struct Vm {
    pub handle: VmHandle,
    inner: Mutex<Inner>,
}

struct Inner {
    gpu: Arc<dyn GpuBackend>,
    handle: VmHandle,
    space: VaSpace,
    /// Буферы, отображённые хоть где-то.
    refs: BTreeMap<BoHandle, Arc<Bo>>,
}

impl Vm {
    /// `VM_INIT`: проверить область ядра и создать пространство у бэкенда.
    pub fn new(gpu: &Arc<dyn GpuBackend>, kernel_addr: u64, kernel_size: u64) -> Result<Self, i64> {
        let space = VaSpace::new(kernel_addr, kernel_size).map_err(errno)?;
        let handle = gpu.vm_create((kernel_addr, kernel_size)).map_err(errno)?;
        Ok(Self { handle, inner: Mutex::new(Inner { gpu: Arc::clone(gpu), handle, space, refs: BTreeMap::new() }) })
    }

    /// Пачка `VM_BIND`: `ops` уже разобраны, `bos` — буферы операций MAP.
    /// Ошибка проверки — ничего не изменилось; ошибка бэкенда — в журнал и
    /// ответ (таблицы могли измениться частично, учёт — уже новый).
    pub fn bind(&self, ops: &[BindOp], bos: Vec<Arc<Bo>>) -> Result<(), i64> {
        let t0 = crate::time::uptime_ns();
        let mut inner = self.inner.lock();
        let t1 = crate::time::uptime_ns();
        add(Phase::Lock, t1 - t0);
        let (actions, released) = match inner.space.bind_released(ops) {
            Ok(done) => done,
            Err(err) => return Err(errno(err)),
        };
        let t2 = crate::time::uptime_ns();
        add(Phase::Space, t2 - t1);
        let candidates: Vec<BoHandle> = bos.iter().map(|bo| bo.handle).collect();
        for bo in bos {
            inner.refs.insert(bo.handle, bo);
        }
        let result = inner.apply_timed(&actions);
        // The tables no longer point at these buffers and the TLB is flushed
        // (`apply_timed` ends with it): only now may their frames go back.
        // A buffer mapped and unmapped again inside the batch is dropped too.
        let t3 = crate::time::uptime_ns();
        let mut dropped = Vec::new();
        for handle in released.into_iter().chain(candidates) {
            if !inner.space.bo_mapped(handle) {
                dropped.extend(inner.refs.remove(&handle));
            }
        }
        let held = inner.refs.len() as u64;
        drop(inner);
        drop(dropped);
        add(Phase::Refs, crate::time::uptime_ns() - t3);
        STATS[Phase::Ops as usize].fetch_add(ops.len() as u64, Ordering::Relaxed);
        STATS[Phase::Held as usize].store(held, Ordering::Relaxed);
        if let Err(err) = result {
            crate::kprintln!("nvgpu: drm vm {} bind: backend error {err} ({} actions)", self.handle.0, actions.len());
        }
        result
    }
}

impl Inner {
    /// `nvgpu::vaspace::apply` with a timer per kind of action; one TLB
    /// flush for the whole batch.
    fn apply_timed(&self, actions: &[Action]) -> Result<(), i64> {
        let gpu = &*self.gpu;
        let mut r = Ok(());
        for a in actions {
            let t = crate::time::uptime_ns();
            let (x, phase, bytes) = match *a {
                Action::Map { va, len, bo, bo_off, kind } => (gpu.vm_map(self.handle, va, bo, bo_off, len, kind), Phase::Map, len),
                Action::Unmap { va, len, restore_sparse } => (gpu.vm_unmap(self.handle, va, len, restore_sparse), Phase::Unmap, len),
                Action::Sparse { va, len, on } => (gpu.vm_sparse(self.handle, va, len, on), Phase::Sparse, len),
            };
            add(phase, crate::time::uptime_ns() - t);
            STATS[phase as usize + 1].fetch_add(1, Ordering::Relaxed);
            STATS[phase as usize + 2].fetch_add(bytes, Ordering::Relaxed);
            if x.is_err() && r.is_ok() {
                r = x;
            }
        }
        // Nothing changed in the tables (an unmap of nothing): no flush.
        if !actions.is_empty() {
            let t = crate::time::uptime_ns();
            gpu.vm_flush(self.handle);
            add(Phase::Flush, crate::time::uptime_ns() - t);
        }
        r.map_err(errno)
    }
}

impl Drop for Inner {
    /// Каналы файла уже сняты (см. `FileState::drop`): снять отображения,
    /// разобрать пространство у бэкенда и только потом отпустить буферы.
    fn drop(&mut self) {
        let actions = self.space.clear();
        let _ = apply(&*self.gpu, self.handle, &actions);
        self.gpu.vm_destroy(self.handle);
        self.refs.clear();
    }
}

// ---- perf-diag ------------------------------------------------------------------

/// Phases of a `VM_BIND` call (ns sums in [`STATS`]); `Map`, `Unmap` and
/// `Sparse` are followed by their count and bytes.
#[derive(Clone, Copy)]
#[repr(usize)]
pub(super) enum Phase {
    /// Copy-in, sync objects, op parse and buffer lookup (under the file lock).
    Parse = 0,
    /// Waiting for the batch's wait fences.
    Wait = 1,
    /// Waiting for the address space lock (another bind of the file).
    Lock = 2,
    /// `VaSpace::bind_released`: checks and bookkeeping.
    Space = 3,
    Map = 4,
    Unmap = 7,
    Sparse = 10,
    /// One TLB flush per call.
    Flush = 13,
    /// Dropping released buffers (frees their memory).
    Refs = 14,
    /// Whole calls: count (sum is in `Total + 1`), ns, worst ns.
    Calls = 15,
    Total = 16,
    Worst = 17,
    /// Bind operations.
    Ops = 18,
    /// Buffers held by the last bind's space (a level, not a sum).
    Held = 19,
}
const STAT_COUNT: usize = 20;
static STATS: [AtomicU64; STAT_COUNT] = [const { AtomicU64::new(0) }; STAT_COUNT];

pub(super) fn add(phase: Phase, ns: u64) {
    STATS[phase as usize].fetch_add(ns, Ordering::Relaxed);
}

/// A whole `VM_BIND` call took `ns`.
pub(super) fn call_done(ns: u64) {
    STATS[Phase::Calls as usize].fetch_add(1, Ordering::Relaxed);
    STATS[Phase::Total as usize].fetch_add(ns, Ordering::Relaxed);
    STATS[Phase::Worst as usize].fetch_max(ns, Ordering::Relaxed);
}

/// perf-diag line for the window since the last call (counters restart);
/// `None` - no `VM_BIND` in the window.
pub fn perf_line(secs: u64) -> Option<String> {
    let take: [u64; STAT_COUNT] = core::array::from_fn(|i| if i == Phase::Held as usize { STATS[i].load(Ordering::Relaxed) } else { STATS[i].swap(0, Ordering::Relaxed) });
    let calls = take[Phase::Calls as usize];
    if calls == 0 {
        return None;
    }
    let us = |ns: u64| ns / calls / 1000;
    let per = |phase: Phase| -> String {
        let i = phase as usize;
        let n = take[i + 1];
        alloc::format!("{} us ({n}x, {} us each, {} MiB)", us(take[i]), take[i] / n.max(1) / 1000, take[i + 2] >> 20)
    };
    let backend_lock = crate::display::nvidia_rm_chan::VMM_LOCK_WAIT_NS.swap(0, Ordering::Relaxed);
    Some(alloc::format!(
        "{secs} s vm_bind: {calls} calls, {} ops, {} us avg (worst {} us); per call: parse {} us, wait fences {} us, vm lock {} us, bookkeeping {} us, map {}, unmap {}, sparse {}, tlb flush {} us, release {} us; {} buffers held; backend space lock waits {} ms in the window",
        take[Phase::Ops as usize],
        us(take[Phase::Total as usize]),
        take[Phase::Worst as usize] / 1000,
        us(take[Phase::Parse as usize]),
        us(take[Phase::Wait as usize]),
        us(take[Phase::Lock as usize]),
        us(take[Phase::Space as usize]),
        per(Phase::Map),
        per(Phase::Unmap),
        per(Phase::Sparse),
        us(take[Phase::Flush as usize]),
        us(take[Phase::Refs as usize]),
        take[Phase::Held as usize],
        backend_lock / 1_000_000,
    ))
}
