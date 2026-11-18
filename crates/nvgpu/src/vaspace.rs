// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Учёт адресного пространства файла DRM (то, что у nouveau делает
//! `drm_gpuvm` + `nouveau_uvmm.c`): какие отображения есть, где разреженные
//! области, какая часть отдана ядру. VM_BIND проверяется здесь целиком и
//! превращается в список [`Action`] для [`GpuBackend`] — слой DRM (WP-C)
//! только применяет его ([`apply`]) и держит ссылки на буферы, пока они
//! отображены ([`VaSpace::bo_mapped`]).
//!
//! Правила (Linux 6.12 `nouveau_uvmm.c`):
//! * адрес, длина и смещение в буфере кратны 4 КиБ, длина > 0, конец не дальше
//!   `1 << 47`, с областью ядра не пересекается — иначе `EINVAL`;
//! * MAP: `bo_offset < размер`, `range <= размер - bo_offset` — иначе
//!   `EINVAL`; отображение целиком внутри разреженной области или целиком
//!   вне всех — иначе `ENOSPC`; перекрытые старые отображения заменяются
//!   (режутся по краям);
//! * MAP|SPARSE (handle 0): новая разреженная область; там уже есть
//!   отображения — `ENOSPC`, пересекается с другой областью — `EEXIST`;
//! * UNMAP|SPARSE: область должна совпасть точно — иначе `ENOENT`;
//!   отображения внутри снимаются, разреженность тоже;
//! * UNMAP: снимает всё в диапазоне; внутри разреженной области PTE
//!   возвращаются в «разреженное».
//! * MAP без SPARSE с handle 0 — по спецификации (2.5, «[?]») понимаем как
//!   UNMAP: внутри области это и есть «вернуть разреженность».
//!
//! Пачка операций атомарна: при ошибке в любой состояние не меняется.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::backend::{BoHandle, Errno, GpuBackend, VmHandle};
use crate::gmmu::{NVK_VA_LIMIT, PAGE};
use crate::uapi::{DRM_NOUVEAU_VM_BIND_OP_MAP, DRM_NOUVEAU_VM_BIND_OP_UNMAP, DRM_NOUVEAU_VM_BIND_SPARSE, NouveauVmBindOp};

/// Отображение: `[va, end)` -> буфер `bo` с `bo_off`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub end: u64,
    /// Ключ буфера, выбранный вызывающим (ручка бэкенда).
    pub bo: BoHandle,
    pub bo_off: u64,
    pub kind: u8,
}

/// Операция VM_BIND, уже разобранная.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindOp {
    Map { va: u64, len: u64, bo: BoHandle, bo_off: u64, bo_size: u64, kind: u8 },
    Unmap { va: u64, len: u64 },
    SparseCreate { va: u64, len: u64 },
    SparseDestroy { va: u64, len: u64 },
}

impl BindOp {
    /// Разобрать `drm_nouveau_vm_bind_op`. `lookup(handle)` — буфер файла и
    /// его размер (`None` -> `ENOENT`).
    pub fn parse(op: &NouveauVmBindOp, lookup: impl FnOnce(u32) -> Option<(BoHandle, u64)>) -> Result<BindOp, Errno> {
        let sparse = op.flags & DRM_NOUVEAU_VM_BIND_SPARSE != 0;
        match op.op {
            DRM_NOUVEAU_VM_BIND_OP_MAP if sparse => Ok(BindOp::SparseCreate { va: op.addr, len: op.range }),
            DRM_NOUVEAU_VM_BIND_OP_MAP if op.handle == 0 => Ok(BindOp::Unmap { va: op.addr, len: op.range }),
            DRM_NOUVEAU_VM_BIND_OP_MAP => {
                let (bo, bo_size) = lookup(op.handle).ok_or(Errno::ENOENT)?;
                Ok(BindOp::Map { va: op.addr, len: op.range, bo, bo_off: op.bo_offset, bo_size, kind: op.kind() })
            }
            DRM_NOUVEAU_VM_BIND_OP_UNMAP if sparse => Ok(BindOp::SparseDestroy { va: op.addr, len: op.range }),
            DRM_NOUVEAU_VM_BIND_OP_UNMAP => Ok(BindOp::Unmap { va: op.addr, len: op.range }),
            _ => Err(Errno::EINVAL),
        }
    }
}

/// Что сделать с бэкендом (по порядку).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Map { va: u64, len: u64, bo: BoHandle, bo_off: u64, kind: u8 },
    Unmap { va: u64, len: u64, restore_sparse: bool },
    Sparse { va: u64, len: u64, on: bool },
}

/// Применить действия к бэкенду и сбросить TLB.
pub fn apply(gpu: &dyn GpuBackend, vm: VmHandle, actions: &[Action]) -> Result<(), Errno> {
    let mut r = Ok(());
    for a in actions {
        let x = match *a {
            Action::Map { va, len, bo, bo_off, kind } => gpu.vm_map(vm, va, bo, bo_off, len, kind),
            Action::Unmap { va, len, restore_sparse } => gpu.vm_unmap(vm, va, len, restore_sparse),
            Action::Sparse { va, len, on } => gpu.vm_sparse(vm, va, len, on),
        };
        if x.is_err() && r.is_ok() {
            r = x;
        }
    }
    gpu.vm_flush(vm);
    r
}

/// Адресное пространство одного файла.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaSpace {
    /// Область ядра из VM_INIT.
    pub kernel: (u64, u64),
    maps: BTreeMap<u64, Mapping>,
    /// Разреженные области: начало -> конец.
    regions: BTreeMap<u64, u64>,
}

impl VaSpace {
    /// VM_INIT: проверка как у nouveau (`addr + size` без переполнения и не
    /// дальше `1 << 47`), плюс выравнивание на страницу.
    pub fn new(kernel_addr: u64, kernel_size: u64) -> Result<VaSpace, Errno> {
        let end = kernel_addr.checked_add(kernel_size).ok_or(Errno::EINVAL)?;
        if end > NVK_VA_LIMIT || kernel_addr % PAGE != 0 || kernel_size % PAGE != 0 {
            return Err(Errno::EINVAL);
        }
        Ok(VaSpace { kernel: (kernel_addr, kernel_size), maps: BTreeMap::new(), regions: BTreeMap::new() })
    }

    pub fn mappings(&self) -> impl Iterator<Item = (u64, &Mapping)> {
        self.maps.iter().map(|(&v, m)| (v, m))
    }

    pub fn regions(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.regions.iter().map(|(&s, &e)| (s, e))
    }

    /// Отображён ли где-нибудь буфер (держать его живым).
    pub fn bo_mapped(&self, bo: BoHandle) -> bool {
        self.maps.values().any(|m| m.bo == bo)
    }

    /// Что отображено по адресу.
    pub fn lookup(&self, va: u64) -> Option<(u64, Mapping)> {
        let (&s, m) = self.maps.range(..=va).next_back()?;
        (va < m.end).then_some((s, *m))
    }

    fn check_range(&self, va: u64, len: u64) -> Result<u64, Errno> {
        if va % PAGE != 0 || len % PAGE != 0 || len == 0 {
            return Err(Errno::EINVAL);
        }
        let end = va.checked_add(len).ok_or(Errno::EINVAL)?;
        if end > NVK_VA_LIMIT {
            return Err(Errno::EINVAL);
        }
        let (ks, kl) = self.kernel;
        if kl != 0 && va < ks + kl && ks < end {
            return Err(Errno::EINVAL);
        }
        Ok(end)
    }

    /// Первая область, пересекающая `[va, end)`.
    fn region_overlap(&self, va: u64, end: u64) -> Option<(u64, u64)> {
        if let Some((&s, &e)) = self.regions.range(..=va).next_back() {
            if e > va {
                return Some((s, e));
            }
        }
        self.regions.range(va..end).next().map(|(&s, &e)| (s, e))
    }

    fn any_mapping(&self, va: u64, end: u64) -> bool {
        if let Some((_, m)) = self.maps.range(..=va).next_back() {
            if m.end > va {
                return true;
            }
        }
        self.maps.range(va..end).next().is_some()
    }

    /// Вырезать `[va, end)` из отображений (края режутся).
    fn cut(&mut self, va: u64, end: u64) -> bool {
        let mut hit = false;
        // Отображение, начинающееся раньше и заходящее внутрь.
        if let Some((&s, &m)) = self.maps.range(..va).next_back() {
            if m.end > va {
                hit = true;
                self.maps.insert(s, Mapping { end: va, ..m });
                if m.end > end {
                    self.maps.insert(end, Mapping { bo_off: m.bo_off + (end - s), ..m });
                }
            }
        }
        let inside: Vec<u64> = self.maps.range(va..end).map(|(&s, _)| s).collect();
        for s in inside {
            hit = true;
            let m = self.maps.remove(&s).unwrap();
            if m.end > end {
                self.maps.insert(end, Mapping { bo_off: m.bo_off + (end - s), ..m });
            }
        }
        hit
    }

    /// Действия «очистить `[va, end)`»: внутри областей — в разреженное.
    fn clear_actions(&self, va: u64, end: u64, out: &mut Vec<Action>) {
        let mut cur = va;
        while cur < end {
            match self.region_overlap(cur, end) {
                Some((s, e)) if s <= cur => {
                    let to = e.min(end);
                    out.push(Action::Unmap { va: cur, len: to - cur, restore_sparse: true });
                    cur = to;
                }
                Some((s, _)) => {
                    out.push(Action::Unmap { va: cur, len: s - cur, restore_sparse: false });
                    cur = s;
                }
                None => {
                    out.push(Action::Unmap { va: cur, len: end - cur, restore_sparse: false });
                    cur = end;
                }
            }
        }
    }

    fn op(&mut self, op: &BindOp, out: &mut Vec<Action>) -> Result<(), Errno> {
        match *op {
            BindOp::Map { va, len, bo, bo_off, bo_size, kind } => {
                if bo_off % PAGE != 0 || bo_off >= bo_size || len > bo_size - bo_off {
                    return Err(Errno::EINVAL);
                }
                let end = self.check_range(va, len)?;
                crate::gmmu::pte_kind(kind)?;
                if let Some((s, e)) = self.region_overlap(va, end) {
                    if s > va || e < end {
                        return Err(Errno::ENOSPC);
                    }
                }
                self.cut(va, end);
                self.maps.insert(va, Mapping { end, bo, bo_off, kind });
                out.push(Action::Map { va, len, bo, bo_off, kind });
            }
            BindOp::Unmap { va, len } => {
                let end = self.check_range(va, len)?;
                self.cut(va, end);
                self.clear_actions(va, end, out);
            }
            BindOp::SparseCreate { va, len } => {
                let end = self.check_range(va, len)?;
                if self.region_overlap(va, end).is_some() {
                    return Err(Errno::EEXIST);
                }
                if self.any_mapping(va, end) {
                    return Err(Errno::ENOSPC);
                }
                self.regions.insert(va, end);
                out.push(Action::Sparse { va, len, on: true });
            }
            BindOp::SparseDestroy { va, len } => {
                let end = self.check_range(va, len)?;
                if self.regions.get(&va) != Some(&end) {
                    return Err(Errno::ENOENT);
                }
                self.cut(va, end);
                self.regions.remove(&va);
                out.push(Action::Sparse { va, len, on: false });
            }
        }
        Ok(())
    }

    /// Проверить и выполнить пачку операций. При ошибке состояние прежнее.
    pub fn bind(&mut self, ops: &[BindOp]) -> Result<Vec<Action>, Errno> {
        let mut next = self.clone();
        let mut out = Vec::new();
        for op in ops {
            next.op(op, &mut out)?;
        }
        *self = next;
        Ok(out)
    }

    /// Всё снять (закрытие файла): действия, после которых в пространстве
    /// пусто.
    pub fn clear(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        for (&va, m) in &self.maps {
            out.push(Action::Unmap { va, len: m.end - va, restore_sparse: false });
        }
        for (&s, &e) in &self.regions {
            out.push(Action::Sparse { va: s, len: e - s, on: false });
        }
        self.maps.clear();
        self.regions.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B1: BoHandle = BoHandle(1);
    const B2: BoHandle = BoHandle(2);

    fn map(va: u64, len: u64, bo: BoHandle, bo_off: u64) -> BindOp {
        BindOp::Map { va, len, bo, bo_off, bo_size: 1 << 30, kind: 0 }
    }

    #[test]
    fn vm_init_rules() {
        // NVK присылает [1<<39, 1<<40).
        assert!(VaSpace::new(1 << 39, (1 << 40) - (1 << 39)).is_ok());
        assert!(VaSpace::new(1 << 46, 1 << 46).is_ok(), "конец ровно 1<<47 допустим");
        assert_eq!(VaSpace::new(1 << 46, 1 << 47), Err(Errno::EINVAL));
        assert_eq!(VaSpace::new(u64::MAX, 2), Err(Errno::EINVAL));
    }

    #[test]
    fn map_replace_split() {
        let mut v = VaSpace::new(1 << 39, 1 << 39).unwrap();
        let a = v.bind(&[map(0x10000, 0x8000, B1, 0)]).unwrap();
        assert_eq!(a, [Action::Map { va: 0x10000, len: 0x8000, bo: B1, bo_off: 0, kind: 0 }]);
        // Середину заменяем другим буфером: края остаются со своими смещениями.
        v.bind(&[map(0x12000, 0x2000, B2, 0x5000)]).unwrap();
        let m: Vec<_> = v.mappings().map(|(va, m)| (va, m.end, m.bo, m.bo_off)).collect();
        assert_eq!(m, [(0x10000, 0x12000, B1, 0), (0x12000, 0x14000, B2, 0x5000), (0x14000, 0x18000, B1, 0x4000)]);
        // Снимаем часть: по краям режется.
        let a = v.bind(&[BindOp::Unmap { va: 0x11000, len: 0x4000 }]).unwrap();
        assert_eq!(a, [Action::Unmap { va: 0x11000, len: 0x4000, restore_sparse: false }]);
        let m: Vec<_> = v.mappings().map(|(va, m)| (va, m.end, m.bo_off)).collect();
        assert_eq!(m, [(0x10000, 0x11000, 0), (0x15000, 0x18000, 0x5000)]);
        assert!(v.bo_mapped(B1));
        assert!(!v.bo_mapped(B2));
        assert_eq!(v.lookup(0x15800).unwrap().1.bo_off, 0x5000);
        assert!(v.lookup(0x12000).is_none());
    }

    #[test]
    fn validation() {
        let mut v = VaSpace::new(1 << 39, 1 << 39).unwrap();
        assert_eq!(v.bind(&[map(0x1800, 0x1000, B1, 0)]), Err(Errno::EINVAL));
        assert_eq!(v.bind(&[map(0x1000, 0, B1, 0)]), Err(Errno::EINVAL));
        assert_eq!(v.bind(&[map(0x1000, 0x1000, B1, 0x800)]), Err(Errno::EINVAL));
        assert_eq!(v.bind(&[BindOp::Map { va: 0x1000, len: 0x2000, bo: B1, bo_off: 0x1000, bo_size: 0x2000, kind: 0 }]), Err(Errno::EINVAL));
        assert_eq!(v.bind(&[BindOp::Map { va: 0x1000, len: 0x1000, bo: B1, bo_off: 0x2000, bo_size: 0x2000, kind: 0 }]), Err(Errno::EINVAL));
        assert_eq!(v.bind(&[map((1 << 39) - 0x1000, 0x2000, B1, 0)]), Err(Errno::EINVAL), "в область ядра");
        assert_eq!(v.bind(&[map(NVK_VA_LIMIT - 0x1000, 0x2000, B1, 0)]), Err(Errno::EINVAL));
        assert_eq!(v.bind(&[BindOp::Map { va: 0x1000, len: 0x1000, bo: B1, bo_off: 0, bo_size: 0x1000, kind: 7 }]), Err(Errno::EINVAL));
        // Атомарность: вторая операция плохая — первой тоже нет.
        assert_eq!(v.bind(&[map(0x1000, 0x1000, B1, 0), map(0x1800, 0x1000, B1, 0)]), Err(Errno::EINVAL));
        assert_eq!(v.mappings().count(), 0);
    }

    #[test]
    fn sparse_regions() {
        let mut v = VaSpace::new(1 << 39, 1 << 39).unwrap();
        let r = 0x1_0000_0000u64;
        let a = v.bind(&[BindOp::SparseCreate { va: r, len: 0x10_0000 }]).unwrap();
        assert_eq!(a, [Action::Sparse { va: r, len: 0x10_0000, on: true }]);
        assert_eq!(v.bind(&[BindOp::SparseCreate { va: r + 0x8_0000, len: 0x10_0000 }]), Err(Errno::EEXIST));
        // Внутри — можно; через край — ENOSPC.
        v.bind(&[map(r + 0x1000, 0x2000, B1, 0)]).unwrap();
        assert_eq!(v.bind(&[map(r - 0x1000, 0x2000, B1, 0)]), Err(Errno::ENOSPC));
        assert_eq!(v.bind(&[map(r + 0xf_f000, 0x2000, B1, 0)]), Err(Errno::ENOSPC));
        // Снятие внутри области — назад в разреженное; через край — по частям.
        let a = v.bind(&[BindOp::Unmap { va: r + 0x1000, len: 0x1000 }]).unwrap();
        assert_eq!(a, [Action::Unmap { va: r + 0x1000, len: 0x1000, restore_sparse: true }]);
        let a = v.bind(&[BindOp::Unmap { va: r - 0x1000, len: 0x3000 }]).unwrap();
        assert_eq!(a, [Action::Unmap { va: r - 0x1000, len: 0x1000, restore_sparse: false }, Action::Unmap { va: r, len: 0x2000, restore_sparse: true }]);
        // MAP без буфера (handle 0) = снять с восстановлением.
        let op = NouveauVmBindOp { op: DRM_NOUVEAU_VM_BIND_OP_MAP, addr: r, range: 0x1000, ..Default::default() };
        assert_eq!(BindOp::parse(&op, |_| None), Ok(BindOp::Unmap { va: r, len: 0x1000 }));
        // Область с отображением внутри создать нельзя.
        v.bind(&[map(0x2000_0000, 0x1000, B2, 0)]).unwrap();
        assert_eq!(v.bind(&[BindOp::SparseCreate { va: 0x2000_0000, len: 0x10_0000 }]), Err(Errno::ENOSPC));
        // Удаление: только точное совпадение.
        assert_eq!(v.bind(&[BindOp::SparseDestroy { va: r, len: 0x1000 }]), Err(Errno::ENOENT));
        v.bind(&[map(r + 0x4000, 0x1000, B1, 0)]).unwrap();
        let a = v.bind(&[BindOp::SparseDestroy { va: r, len: 0x10_0000 }]).unwrap();
        assert_eq!(a, [Action::Sparse { va: r, len: 0x10_0000, on: false }]);
        assert!(v.lookup(r + 0x4000).is_none(), "отображения внутри снялись");
        assert_eq!(v.regions().count(), 0);
        let a = v.clear();
        assert_eq!(a, [Action::Unmap { va: 0x2000_0000, len: 0x1000, restore_sparse: false }]);
    }

    #[test]
    fn parse_ops() {
        let lookup = |h: u32| (h == 5).then_some((BoHandle(50), 0x4000));
        let op = NouveauVmBindOp { op: 0, flags: 0x06, handle: 5, addr: 0x1000, bo_offset: 0x1000, range: 0x2000, pad: 0 };
        assert_eq!(BindOp::parse(&op, lookup), Ok(BindOp::Map { va: 0x1000, len: 0x2000, bo: BoHandle(50), bo_off: 0x1000, bo_size: 0x4000, kind: 6 }));
        let op = NouveauVmBindOp { handle: 6, ..op };
        assert_eq!(BindOp::parse(&op, lookup), Err(Errno::ENOENT));
        let op = NouveauVmBindOp { op: 1, flags: DRM_NOUVEAU_VM_BIND_SPARSE, ..op };
        assert_eq!(BindOp::parse(&op, lookup), Ok(BindOp::SparseDestroy { va: 0x1000, len: 0x2000 }));
        let op = NouveauVmBindOp { op: 9, ..op };
        assert_eq!(BindOp::parse(&op, lookup), Err(Errno::EINVAL));
    }

    #[test]
    fn with_softgpu() {
        use crate::backend::{GpuBackend, Place};
        use crate::gmmu::Walk;
        use crate::softgpu::{HeapHost, SoftGpu};
        use alloc::sync::Arc;
        let gpu = SoftGpu::new(Arc::new(HeapHost::new()));
        let mut v = VaSpace::new(1 << 39, 1 << 39).unwrap();
        let vm = gpu.vm_create(v.kernel).unwrap();
        let bo = gpu.bo_alloc(0x4000, Place::Sysmem).unwrap();
        let r = 0x4000_0000;
        let acts = v
            .bind(&[BindOp::SparseCreate { va: r, len: 0x40_0000 }, BindOp::Map { va: r + 0x1000, len: 0x4000, bo, bo_off: 0, bo_size: 0x4000, kind: 0 }])
            .unwrap();
        apply(&gpu, vm, &acts).unwrap();
        assert!(matches!(gpu.vm_translate(vm, r + 0x1000), Some(Walk::Mapped { .. })));
        assert_eq!(gpu.vm_translate(vm, r), Some(Walk::Sparse));
        let acts = v.bind(&[BindOp::Unmap { va: r + 0x1000, len: 0x4000 }]).unwrap();
        apply(&gpu, vm, &acts).unwrap();
        assert_eq!(gpu.vm_translate(vm, r + 0x1000), Some(Walk::Sparse));
        let acts = v.clear();
        apply(&gpu, vm, &acts).unwrap();
        assert_eq!(gpu.vm_translate(vm, r), Some(Walk::Unmapped));
    }
}
