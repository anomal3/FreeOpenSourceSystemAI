//! `zwp_linux_dmabuf_v1` сервера Wayland ядра (18.11.2026, NVK поверх GSP-RM,
//! веха M7): аппаратный драйвер Vulkan (Mesa NVK) показывает кадры только
//! буферами dma-buf — `wl_shm` у него нет.
//!
//! Подмодуль `wayland.rs` (`#[path]`): буфер dma-buf становится тем же
//! [`Buffer`], что и буфер `wl_shm`, — разделяемая память кадров системной
//! памяти, которые видит и видеокарта (узел DRM отдаёт их через
//! [`crate::linux::drm::dmabuf_memory`]). Дальше — общий путь: стол читает
//! буфер процессором, как память `memfd`.
//!
//! Что умеем (версия 3 — события `format` и `modifier`, без таблицы форматов
//! версии 4): однослойные ARGB8888/XRGB8888 (`AR24`/`XR24` — тот же порядок
//! байт, что `wl_shm` 0/1), модификатор только LINEAR (или INVALID —
//! «неявный», считается линейным); флаги (`y_invert` и т. п.) — нет. NVK кладёт
//! изображения WSI в системную память (`dmabuf_memory` отдаёт `None` для
//! видеопамяти — такой буфер получает `failed`/ошибку протокола).
//!
//! Не сделано: ожидание заборов буфера перед чтением (неявная синхронизация
//! dma-buf; сейчас стол может прочесть кадр, который видеокарта ещё пишет —
//! на SoftGpu работа синхронна), обратная связь версии 4 (`main_device`).

use alloc::sync::Arc;

use super::{Buffer, SHM_ARGB8888, SHM_XRGB8888};
use crate::user::files::Handle;
use crate::user::shm::SharedMem;

pub(super) const INTERFACE: &str = "zwp_linux_dmabuf_v1";
pub(super) const VERSION: u32 = 3;

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}
/// `DRM_FORMAT_*` и соответствующий формат `wl_shm` (тот же порядок байт).
pub(super) const FORMATS: [(u32, u32); 2] = [(fourcc(b"AR24"), SHM_ARGB8888), (fourcc(b"XR24"), SHM_XRGB8888)];
pub(super) const MOD_LINEAR: u64 = 0;
const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// Ошибки `zwp_linux_buffer_params_v1`.
pub(super) const ERR_ALREADY_USED: u32 = 0;
pub(super) const ERR_PLANE_IDX: u32 = 1;
pub(super) const ERR_PLANE_SET: u32 = 2;
pub(super) const ERR_INCOMPLETE: u32 = 3;
pub(super) const ERR_INVALID_FORMAT: u32 = 4;
pub(super) const ERR_INVALID_DIMENSIONS: u32 = 5;
pub(super) const ERR_OUT_OF_BOUNDS: u32 = 6;
pub(super) const ERR_INVALID_WL_BUFFER: u32 = 7;

/// Отказ: код ошибки протокола и текст.
pub(super) struct Fail(pub u32, pub &'static str);

struct Plane {
    /// Память буфера и его длина; `None` — дескриптор не dma-buf системной
    /// памяти (ошибка — при `create`, как велит протокол).
    mem: Option<(Arc<SharedMem>, u64)>,
    offset: u32,
    stride: u32,
    modifier: u64,
}

/// Объект `zwp_linux_buffer_params_v1`: набор плоскостей до `create`.
#[derive(Default)]
pub(super) struct Params {
    plane: Option<Plane>,
    used: bool,
}

impl Params {
    /// `add(fd, plane_idx, offset, stride, modifier_hi, modifier_lo)`.
    pub(super) fn add(&mut self, fd: &Handle, index: u32, offset: u32, stride: u32, modifier: u64) -> Result<(), Fail> {
        if self.used {
            return Err(Fail(ERR_ALREADY_USED, "params already used"));
        }
        if index != 0 {
            return Err(Fail(ERR_PLANE_IDX, "only single-plane formats are supported"));
        }
        if self.plane.is_some() {
            return Err(Fail(ERR_PLANE_SET, "plane 0 already set"));
        }
        let mem = fd.object().and_then(|object| crate::linux::drm::dmabuf_memory(object.as_ref()));
        self.plane = Some(Plane { mem, offset, stride, modifier });
        Ok(())
    }

    /// `create`/`create_immed`: буфер из плоскости 0. Параметры одноразовые.
    pub(super) fn create(&mut self, width: i32, height: i32, format: u32, flags: u32) -> Result<Buffer, Fail> {
        if self.used {
            return Err(Fail(ERR_ALREADY_USED, "params already used"));
        }
        self.used = true;
        let Some(plane) = self.plane.take() else {
            return Err(Fail(ERR_INCOMPLETE, "no plane added"));
        };
        let Some(&(_, shm)) = FORMATS.iter().find(|(drm, _)| *drm == format) else {
            return Err(Fail(ERR_INVALID_FORMAT, "unsupported format"));
        };
        if plane.modifier != MOD_LINEAR && plane.modifier != MOD_INVALID {
            return Err(Fail(ERR_INVALID_FORMAT, "only the linear modifier is supported"));
        }
        if flags != 0 {
            return Err(Fail(ERR_INVALID_FORMAT, "buffer flags are not supported"));
        }
        if width <= 0 || height <= 0 || u64::from(plane.stride) < width as u64 * 4 {
            return Err(Fail(ERR_INVALID_DIMENSIONS, "bad width, height or stride"));
        }
        let Some((mem, size)) = plane.mem else {
            return Err(Fail(ERR_INVALID_WL_BUFFER, "descriptor is not a CPU-visible dma-buf"));
        };
        let end = u64::from(plane.offset) + u64::from(plane.stride) * (height as u64 - 1) + width as u64 * 4;
        if end > size {
            return Err(Fail(ERR_OUT_OF_BOUNDS, "plane does not fit the dma-buf"));
        }
        Ok(Buffer { mem, offset: u64::from(plane.offset), width: width as u32, height: height as u32, stride: plane.stride, format: shm })
    }
}

/// Ошибки, о которых `create` (не `create_immed`) сообщает событием `failed`,
/// а не ошибкой протокола: буфер не удалось импортировать.
pub(super) const fn soft_failure(code: u32) -> bool {
    code == ERR_INVALID_WL_BUFFER
}
