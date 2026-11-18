//! Адресное пространство GPU файла (`VM_INIT`/`VM_BIND`, как `nouveau_uvmm`).
//!
//! Правила и учёт — `nvgpu::vaspace::VaSpace` (WP-B, проверено на хосте):
//! пачка операций проверяется целиком и атомарно превращается в действия для
//! бэкенда (`apply` пишет таблицы и сбрасывает TLB). Здесь — только ссылки на
//! буферы: пока буфер где-то отображён, его кадры не уходят, даже если ручку
//! уже закрыли (`GEM_CLOSE` после `VM_BIND` — обычное дело у NVK).

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;

use nvgpu::vaspace::{BindOp, VaSpace, apply};
use nvgpu::{BoHandle, GpuBackend, VmHandle};

use super::Bo;

fn errno(err: nvgpu::Errno) -> i64 {
    -i64::from(err.0)
}

pub struct Vm {
    gpu: Arc<dyn GpuBackend>,
    pub handle: VmHandle,
    space: VaSpace,
    /// Буферы, отображённые хоть где-то.
    refs: BTreeMap<BoHandle, Arc<Bo>>,
}

impl Vm {
    /// `VM_INIT`: проверить область ядра и создать пространство у бэкенда.
    pub fn new(gpu: &Arc<dyn GpuBackend>, kernel_addr: u64, kernel_size: u64) -> Result<Self, i64> {
        let space = VaSpace::new(kernel_addr, kernel_size).map_err(errno)?;
        let handle = gpu.vm_create((kernel_addr, kernel_size)).map_err(errno)?;
        Ok(Self { gpu: Arc::clone(gpu), handle, space, refs: BTreeMap::new() })
    }

    /// Пачка `VM_BIND`: `ops` уже разобраны, `bos` — буферы операций MAP.
    /// Ошибка проверки — ничего не изменилось; ошибка бэкенда — в журнал и
    /// ответ (таблицы могли измениться частично, учёт — уже новый).
    pub fn bind(&mut self, ops: &[BindOp], bos: Vec<Arc<Bo>>) -> Result<(), i64> {
        let actions = self.space.bind(ops).map_err(errno)?;
        for bo in bos {
            self.refs.insert(bo.handle, bo);
        }
        let result = apply(&*self.gpu, self.handle, &actions).map_err(errno);
        let space = &self.space;
        self.refs.retain(|handle, _| space.bo_mapped(*handle));
        if let Err(err) = result {
            crate::kprintln!("nvgpu: drm vm {} bind: backend error {err} ({} actions)", self.handle.0, actions.len());
        }
        result
    }

    /// Сколько буферов отображено (для журнала).
    #[must_use]
    pub fn mapped_bos(&self) -> usize {
        self.refs.len()
    }
}

impl Drop for Vm {
    /// Каналы файла уже сняты (см. `FileState::drop`): снять отображения,
    /// разобрать пространство у бэкенда и только потом отпустить буферы.
    fn drop(&mut self) {
        let actions = self.space.clear();
        let _ = apply(&*self.gpu, self.handle, &actions);
        self.gpu.vm_destroy(self.handle);
        self.refs.clear();
    }
}
