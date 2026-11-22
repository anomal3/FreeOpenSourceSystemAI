//! Файлы `/dev/dri` и sysfs, которые читает libdrm 2.4.124
//! (`drmGetDevices2`, раздел 1.1 спецификации):
//!
//! 1. `opendir("/dev/dri")`, `stat` каждого `renderD*` (`S_IFCHR`, `st_rdev`
//!    = 226:128 — это отвечает [`super::rdev`]);
//! 2. `stat("/sys/dev/char/226:128/device/drm")` — должен быть каталог;
//! 3. `readlink(".../device/subsystem")` — цель кончается на `/pci`;
//! 4. `realpath(".../device")` (не вышло — берётся путь как есть) и
//!    `.../device/uevent` со строкой `PCI_SLOT_NAME=`;
//! 5. `.../device/{revision,vendor,device,subsystem_vendor,subsystem_device}`
//!    — шестнадцатеричным текстом.
//!
//! Те же файлы — под `/sys/class/drm/renderD128/`. Без бэкенда здесь нет
//! ничего.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::vfs::NodeKind;

/// Каталоги узла: `/sys/dev/char/226:128` и `/sys/class/drm/renderD128`.
const MINOR_DIRS: [&str; 2] = ["/sys/dev/char/226:128", "/sys/class/drm/renderD128"];

/// Остаток пути внутри каталога узла: `""`, `"device"`, `"device/vendor"` …
fn in_minor(path: &str) -> Option<&str> {
    MINOR_DIRS.iter().find_map(|dir| {
        let rest = path.strip_prefix(dir)?;
        if rest.is_empty() {
            Some("")
        } else {
            rest.strip_prefix('/')
        }
    })
}

/// Файлы каталога `device` (карта на шине PCI).
const DEVICE_FILES: [&str; 7] = ["uevent", "vendor", "device", "subsystem_vendor", "subsystem_device", "revision", "class"];

/// Каталог для `opendir`/`stat`; `None` — не наш путь или узла нет.
#[must_use]
pub fn directory(path: &str) -> Option<Vec<(String, NodeKind)>> {
    if !path.starts_with("/dev/dri") && !path.starts_with("/sys/") {
        return None;
    }
    super::node_visible().then_some(())?;
    let file = |name: &str| (String::from(name), NodeKind::File);
    let dir = |name: &str| (String::from(name), NodeKind::Directory);
    match path {
        "/dev/dri" => Some(alloc::vec![file("renderD128")]),
        "/sys/dev" => Some(alloc::vec![dir("char")]),
        "/sys/dev/char" => Some(alloc::vec![dir("226:128")]),
        "/sys/class/drm" => Some(alloc::vec![dir("renderD128")]),
        _ => match in_minor(path)? {
            "" => Some(alloc::vec![dir("device"), file("dev"), file("uevent"), file("subsystem")]),
            "device" => {
                let mut names: Vec<(String, NodeKind)> = DEVICE_FILES.iter().map(|name| file(name)).collect();
                names.push(dir("drm"));
                names.push(file("subsystem"));
                Some(names)
            }
            "device/drm" => Some(alloc::vec![dir("renderD128")]),
            "device/drm/renderD128" => Some(alloc::vec![file("dev"), file("uevent")]),
            _ => None,
        },
    }
}

/// Текст файла sysfs; `None` — не наш путь или узла нет.
#[must_use]
pub fn text(path: &str) -> Option<String> {
    if !path.starts_with("/sys/") {
        return None;
    }
    let rest = in_minor(path)?;
    let info = super::node_ident()?;
    let slot_name = info.slot_name.clone();
    let minor_uevent = || format!("MAJOR={}\nMINOR={}\nDEVNAME=dri/renderD128\nDEVTYPE=drm_minor\n", super::DRM_MAJOR, super::RENDER_MINOR);
    Some(match rest {
        "dev" | "device/drm/renderD128/dev" => format!("{}:{}\n", super::DRM_MAJOR, super::RENDER_MINOR),
        "uevent" | "device/drm/renderD128/uevent" => minor_uevent(),
        "device/uevent" => format!(
            "DRIVER={}\nPCI_CLASS=30000\nPCI_ID={:04X}:{:04X}\nPCI_SUBSYS_ID={:04X}:{:04X}\nPCI_SLOT_NAME={slot_name}\n\
             MODALIAS=pci:v{:08X}d{:08X}sv{:08X}sd{:08X}bc03sc00i00\n",
            info.driver,
            info.vendor,
            info.device,
            info.subsys_vendor,
            info.subsys_device,
            info.vendor,
            info.device,
            info.subsys_vendor,
            info.subsys_device
        ),
        "device/vendor" => format!("0x{:04x}\n", info.vendor),
        "device/device" => format!("0x{:04x}\n", info.device),
        "device/subsystem_vendor" => format!("0x{:04x}\n", info.subsys_vendor),
        "device/subsystem_device" => format!("0x{:04x}\n", info.subsys_device),
        "device/revision" => format!("0x{:02x}\n", info.revision),
        "device/class" => String::from("0x030000\n"),
        _ => return None,
    })
}

/// Есть ли такой путь как файл (для `stat`/`access`): текст sysfs или сам
/// узел рендера.
#[must_use]
pub fn exists(path: &str) -> bool {
    (path == super::RENDER_NODE && super::node_visible()) || text(path).is_some() || link(path).is_some()
}

/// Цель символической ссылки sysfs: `subsystem` — шина (`get_subsystem_type`
/// libdrm смотрит только на хвост `/pci`).
#[must_use]
pub fn link(path: &str) -> Option<&'static str> {
    if !path.starts_with("/sys/") {
        return None;
    }
    let target = match in_minor(path)? {
        "device/subsystem" => "../../../../bus/pci",
        "subsystem" => "../../../../class/drm",
        _ => return None,
    };
    super::node_visible().then_some(())?;
    Some(target)
}
