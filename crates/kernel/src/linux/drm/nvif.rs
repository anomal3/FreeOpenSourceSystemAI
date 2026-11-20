//! Эмуляция NVIF (`DRM_NOUVEAU_NVIF`, nr 0x47) — то, что разрешает
//! `nouveau_abi16_ioctl` Linux 6.12 (раздел 1.2/1.3 спецификации):
//!
//! * `NEW` с `route == 0` — объект `NV_DEVICE` (0x80) под печеньем
//!   `object`; с `route == 0xff` — объект класса движка в канале `token`;
//! * `MTHD` `NV_DEVICE_V0_INFO` на устройстве — чипсет, видеопамять, имена;
//! * `SCLASS` на канале — классы движков (`GpuInfo::classes`);
//! * `DEL` — снять объект; незнакомое печенье — не ошибка.
//!
//! Разбор и ответы — `nvgpu::uapi` (WP-B). Печенье (`object`) выбирает Mesa;
//! ручка движка у RM — забота бэкенда (ручка 0 NVK для copy до RM не
//! доходит, раздел 1.3).
//!
//! Одно расхождение с Linux: `DEL` Mesa несёт 8 байт `nvif_ioctl_del`, а
//! abi16 принимает только пустое тело (`EINVAL`, и объект у Linux остаётся
//! до закрытия канала — потому Mesa ошибки `DEL` и не смотрит). Здесь `DEL`
//! с телом 0 или 8 байт снимает объект сразу.

use nvgpu::uapi::{NV_DEVICE, NV_DEVICE_V0_INFO, NvDeviceInfoV0, NvifCall, Pod, nvif_parse, nvif_sclass_reply};

use super::DrmFile;
use crate::linux::e;

/// Объект NVIF файла.
pub enum NvifObj {
    Device,
    /// Объект движка в канале `chan`.
    Engine { chan: u32, class: u32 },
}

/// `struct nvif_ioctl_v0` — 24 байта перед телом.
const HDR: usize = 24;

/// Весь вызов: прочитать `size` байт, разобрать, ответ — в тот же буфер.
pub fn ioctl(file: &DrmFile, arg: usize, size: usize) -> i64 {
    if size < HDR || size > 4096 {
        return -e::EINVAL;
    }
    let mut buf = match crate::linux::ipc::read_user(arg, size) {
        Ok(buf) => buf,
        Err(err) => return err,
    };
    let (hdr, call) = match nvif_parse(&buf) {
        Ok(parsed) => parsed,
        Err(_) => return -e::EINVAL,
    };
    if hdr.route != 0 && hdr.route != 0xff {
        return -e::EINVAL;
    }
    let argc = size - HDR;
    let answer = match call {
        NvifCall::Sclass { .. } => sclass(file, &mut buf, hdr.route, hdr.token, argc),
        NvifCall::New { body, .. } => new(file, hdr.route, hdr.object, hdr.token, body),
        NvifCall::Del => del(file, hdr.object, argc),
        NvifCall::Mthd { body, args } => mthd(file, &mut buf, hdr.route, hdr.object, body.method, args),
        NvifCall::Other(_) => -e::EINVAL,
    };
    if super::trace() {
        crate::kprintln!(
            "nvgpu: drm nvif type {} route {:#x} token {:#x} object {:#x} -> {answer}",
            hdr.ty,
            hdr.route,
            hdr.token,
            hdr.object
        );
    }
    if answer != 0 {
        return answer;
    }
    match crate::linux::ipc::write_user(arg, &buf) {
        Ok(()) => 0,
        Err(err) => err,
    }
}

/// Классы движков канала `token`; `count` в ответе — сколько их есть.
fn sclass(file: &DrmFile, buf: &mut [u8], route: u8, token: u64, argc: usize) -> i64 {
    if route == 0 || argc < 8 {
        return -e::EINVAL;
    }
    let count = buf[HDR + 1] as usize;
    if argc - 8 != count * 8 {
        return -e::EINVAL;
    }
    let Ok(chan_id) = u32::try_from(token) else { return -e::EINVAL };
    if super::nouveau::chan(file, chan_id).is_none() {
        return -e::EINVAL;
    }
    match nvif_sclass_reply(buf, &file.gpu.info().classes) {
        Ok(()) => 0,
        Err(err) => -i64::from(err.0),
    }
}

fn new(file: &DrmFile, route: u8, object: u64, token: u64, body: nvgpu::uapi::NvifIoctlNewV0) -> i64 {
    if body.version != 0 {
        return -e::EINVAL;
    }
    let cookie = body.object;
    if route == 0 {
        if object != 0 || body.oclass != NV_DEVICE {
            return -e::EINVAL;
        }
        let mut st = file.st.lock();
        if st.nvif.contains_key(&cookie) {
            return -e::EEXIST;
        }
        st.nvif.insert(cookie, NvifObj::Device);
        return 0;
    }
    let Ok(chan_id) = u32::try_from(token) else { return -e::EINVAL };
    let Some(chan) = super::nouveau::chan(file, chan_id) else { return -e::EINVAL };
    if file.st.lock().nvif.contains_key(&cookie) {
        return -e::EEXIST;
    }
    let class = body.oclass as u32;
    if !file.gpu.info().classes.contains(&class) {
        return -e::ENODEV;
    }
    let obj = match file.gpu.chan_obj(chan.core.handle, class) {
        Ok(obj) => obj,
        Err(err) => return -i64::from(err.0),
    };
    chan.objs.lock().insert(cookie, obj);
    file.st.lock().nvif.insert(cookie, NvifObj::Engine { chan: chan_id, class });
    if super::trace() {
        crate::kprintln!("nvgpu: obj class {class:#x} chan {chan_id} handle {:#x}", obj.0);
    }
    0
}

fn del(file: &DrmFile, object: u64, argc: usize) -> i64 {
    if argc != 0 && argc != 8 {
        return -e::EINVAL;
    }
    let removed = file.st.lock().nvif.remove(&object);
    if let Some(NvifObj::Engine { chan, .. }) = removed {
        if let Some(chan) = super::nouveau::chan(file, chan) {
            let obj = chan.objs.lock().remove(&object);
            if let Some(obj) = obj {
                file.gpu.chan_obj_free(chan.core.handle, obj);
            }
        }
    }
    0
}

/// `NV_DEVICE_V0_INFO` (104 байта после `nvif_ioctl_mthd_v0`).
fn mthd(file: &DrmFile, buf: &mut [u8], route: u8, object: u64, method: u8, args: usize) -> i64 {
    if route != 0 {
        return -e::EINVAL;
    }
    if !matches!(file.st.lock().nvif.get(&object), Some(NvifObj::Device)) {
        return -e::EINVAL;
    }
    if method != NV_DEVICE_V0_INFO || buf.len() - args != core::mem::size_of::<NvDeviceInfoV0>() {
        return -e::EINVAL;
    }
    if buf[args] != 0 {
        return -e::EINVAL;
    }
    NvDeviceInfoV0::from_info(file.gpu.info()).write(&mut buf[args..]);
    0
}
