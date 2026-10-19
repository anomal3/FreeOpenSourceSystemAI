//! Встроенная графика Intel: родное разрешение панели без смены частот
//! (фаза 70c).
//!
//! # Что здесь делается и чего нет
//!
//! Это не драйвер видеокарты в смысле Linux `i915`: ни 3D, ни второго
//! монитора, ни смены частот развёртки. Задача одна — показать картинку в
//! **родном разрешении встроенной панели** там, где прошивка его не дала.
//!
//! # Почему этого хватает
//!
//! Панель ноутбука (LVDS или eDP) понимает только свою развёртку. Поэтому
//! прошивка, которой GOP разрешил лишь 1024×768 (ASUS K53SD с панелью
//! 1366×768), всё равно гонит в панель родные 1366×768, а картинку
//! 1024×768 растягивает **масштабировщиком панели** (panel fitter) — отсюда
//! мыло на экране. Значит, частоты уже правильные, и менять надо три вещи:
//! размер источника трубы (`PIPESRC`), шаг строки плоскости (`DSPSTRIDE`) и
//! выключить масштабировщик (`PF_CTL`). Так же поступает и Linux, когда
//! перенимает экран у прошивки без полной перенастройки («fastset»).
//!
//! Кадр остаётся там же, где его оставила прошивка: в апертуре видеопамяти
//! (BAR2), со смещения `DSPSURF`. Прежде чем растить его, проверяется, что
//! таблица GTT отображает весь новый размер, — иначе запись ушла бы мимо.
//!
//! # Какие поколения
//!
//! Ironlake (5) — Broadwell (8): у них одинаковые адреса трубы, плоскости и
//! масштабировщика (`0x6xxxx`/`0x7xxxx`, `PF_CTL` в `0x68080`). Skylake и
//! новее масштабируют иначе (`PS_CTRL`), и до проверки на железе их здесь нет
//! — на них, как правило, и не нужно: их GOP родное разрешение знает.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use boot_info::Framebuffer;

use super::DisplayError;
use crate::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::sync::SpinLock;

const VENDOR: u16 = 0x8086;

/// Размер окна регистров BAR0, которое отображается: два мегабайта регистров
/// и следом GTT (у поколений 5–7; у Broadwell GTT с восьмого мегабайта).
const MMIO_LEN: usize = 16 * 1024 * 1024;

// Регистры поколений 5–8 (PRM, «Display Registers»). `pipe` — 0, 1, 2.
const fn htotal(pipe: usize) -> usize {
    0x60000 + pipe * 0x1000
}
const fn vtotal(pipe: usize) -> usize {
    0x6000C + pipe * 0x1000
}
const fn pipesrc(pipe: usize) -> usize {
    0x6001C + pipe * 0x1000
}
const fn pipeconf(pipe: usize) -> usize {
    0x70008 + pipe * 0x1000
}
const fn dspcntr(plane: usize) -> usize {
    0x70180 + plane * 0x1000
}
const fn dsplinoff(plane: usize) -> usize {
    0x70184 + plane * 0x1000
}
const fn dspstride(plane: usize) -> usize {
    0x70188 + plane * 0x1000
}
const fn dspsurf(plane: usize) -> usize {
    0x7019C + plane * 0x1000
}
const fn dsptileoff(plane: usize) -> usize {
    0x701A4 + plane * 0x1000
}
const fn pf_ctl(pipe: usize) -> usize {
    0x68080 + pipe * 0x800
}
const fn pf_win_pos(pipe: usize) -> usize {
    0x68070 + pipe * 0x800
}
const fn pf_win_sz(pipe: usize) -> usize {
    0x68074 + pipe * 0x800
}
/// Haswell/Broadwell: тайминги встроенной панели eDP — у своего транскодера.
const HTOTAL_EDP: usize = 0x6F000;
const VTOTAL_EDP: usize = 0x6F00C;
const TRANS_DDI_FUNC_CTL_EDP: usize = 0x6F400;

const ENABLE: u32 = 1 << 31;
/// Масштабировщик включён (`PF_CTL`, бит 31).
const PF_ENABLE: u32 = 1 << 31;

/// Поколение по идентификатору устройства.
fn generation(device: u16) -> Option<u8> {
    match device {
        0x0042 | 0x0046 => Some(5),
        0x0102 | 0x0106 | 0x010A | 0x0112 | 0x0116 | 0x0122 | 0x0126 => Some(6),
        0x0152 | 0x0156 | 0x015A | 0x0162 | 0x0166 | 0x016A => Some(7),
        d if matches!(d & 0xFF00, 0x0400 | 0x0A00 | 0x0C00 | 0x0D00) => Some(75),
        d if d & 0xFF00 == 0x1600 => Some(8),
        _ => None,
    }
}

/// Отображённые регистры найденного адаптера.
struct Adapter {
    device: crate::pci::Device,
    generation: u8,
    mmio: VirtAddr,
    aperture: u64,
}

static ADAPTER: SpinLock<Option<Adapter>> = SpinLock::new(None);
/// Экран уже переведён в родное разрешение этим драйвером.
static ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

impl Adapter {
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: окно регистров отображено как память устройства, смещение —
        // внутри него (вызывающие передают адреса из PRM).
        unsafe { ((self.mmio.as_usize() + offset) as *const u32).read_volatile() }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: см. `read`.
        unsafe { ((self.mmio.as_usize() + offset) as *mut u32).write_volatile(value) }
    }

    /// Запись GTT для страницы с этим графическим адресом: `Some(физический)`
    /// у действительной, `None` — дыра.
    fn gtt_entry(&self, graphics: u64) -> Option<u64> {
        let page = (graphics / PAGE_SIZE as u64) as usize;
        if self.generation == 8 {
            let at = 8 * 1024 * 1024 + page * 8;
            if at + 8 > MMIO_LEN {
                return None;
            }
            let low = u64::from(self.read(at));
            let high = u64::from(self.read(at + 4));
            let entry = low | (high << 32);
            (entry & 1 != 0).then_some(entry & 0x0000_007F_FFFF_F000)
        } else {
            let at = 2 * 1024 * 1024 + page * 4;
            if at + 4 > 4 * 1024 * 1024 {
                return None;
            }
            let entry = u64::from(self.read(at));
            (entry & 1 != 0).then_some((entry & 0xFFFF_F000) | ((entry & 0xFF0) << 28))
        }
    }

    /// Тайминги трубы: активная ширина и высота — то, что уходит в панель.
    fn timings(&self, pipe: usize) -> (u32, u32) {
        let (h, v) = if self.generation >= 75 && self.edp_pipe() == Some(pipe) {
            (self.read(HTOTAL_EDP), self.read(VTOTAL_EDP))
        } else {
            (self.read(htotal(pipe)), self.read(vtotal(pipe)))
        };
        ((h & 0xFFF) + 1, (v & 0xFFF) + 1)
    }

    /// К какой трубе подключён транскодер eDP (Haswell, Broadwell).
    fn edp_pipe(&self) -> Option<usize> {
        let ctl = self.read(TRANS_DDI_FUNC_CTL_EDP);
        if ctl & ENABLE == 0 {
            return None;
        }
        Some(match (ctl >> 12) & 0x7 {
            0b101 => 1,
            0b110 => 2,
            _ => 0,
        })
    }

    fn source(&self, pipe: usize) -> (u32, u32) {
        let src = self.read(pipesrc(pipe));
        (((src >> 16) & 0xFFF) + 1, (src & 0xFFF) + 1)
    }

    /// Труба, на которой картинка прошивки: включена и показывает плоскость с
    /// тем же графическим адресом, что у буфера GOP.
    fn active_pipe(&self, surface: u64) -> Option<usize> {
        (0..3).find(|pipe| {
            self.read(pipeconf(*pipe)) & ENABLE != 0
                && self.read(dspcntr(*pipe)) & ENABLE != 0
                && u64::from(self.read(dspsurf(*pipe)) & !0xFFF) == surface
        })
    }
}

/// Найти адаптер Intel, на котором картинка прошивки, и отобразить его
/// регистры. Один раз: дальше — из запаса.
fn adapter(current: &Framebuffer) -> Result<(), DisplayError> {
    let mut guard = ADAPTER.lock();
    if guard.is_some() {
        return Ok(());
    }
    let rsdp = crate::acpi::rsdp();
    if rsdp == 0 {
        return Err(DisplayError::NoDriver("no ACPI, no PCI bus"));
    }
    let mut found = None;
    // SAFETY: обход шины только читает конфигурационное пространство.
    unsafe {
        if let Ok(root) = crate::pci::Root::discover(rsdp) {
            crate::pci::for_each(&root, |device| {
                if device.vendor == VENDOR && device.class == 0x03 {
                    found = Some(*device);
                    return false;
                }
                true
            });
        }
    }
    let device = found.ok_or(DisplayError::NoDriver("no Intel graphics on this machine"))?;
    let generation = generation(device.device).ok_or(DisplayError::NoDriver("this Intel graphics generation is not supported yet"))?;
    let mmio_phys = device.memory_bar(0).ok_or(DisplayError::Refused("no register window (BAR0)"))?;
    let aperture = device.memory_bar(2).ok_or(DisplayError::Refused("no aperture (BAR2)"))?.as_u64();
    let base = super::physical(current.base);
    // Картинка на этом адаптере? На машине с двумя видеокартами экраном может
    // владеть другая — и тогда менять этой режим значит портить чужой экран.
    if base < aperture || base >= aperture + (1 << 30) {
        return Err(DisplayError::NoDriver("the picture is not on the Intel adapter"));
    }
    let len = if generation == 8 { MMIO_LEN } else { 4 * 1024 * 1024 };
    let mmio = mmio_phys.to_direct_map();
    // SAFETY: окно регистров устройства (BAR0), по нему ядро не исполняется.
    unsafe { crate::arch::map_active(mmio, mmio_phys, len, PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE) }
        .map_err(|_| DisplayError::OutOfMemory)?;
    crate::devices::claim(device.address, "intel display");
    *guard = Some(Adapter { device, generation, mmio, aperture });
    Ok(())
}

/// Экран у этого драйвера: переведён в родное разрешение.
#[must_use]
pub fn active() -> bool {
    ACTIVE.load(core::sync::atomic::Ordering::Relaxed)
}

/// Родное разрешение панели, если прошивка показывает меньшее и растягивает
/// его масштабировщиком. `None` — менять нечего или нечем.
pub fn native(current: &Framebuffer) -> Option<(u32, u32)> {
    adapter(current).ok()?;
    let guard = ADAPTER.lock();
    let adapter = guard.as_ref()?;
    let surface = super::physical(current.base).checked_sub(adapter.aperture)?;
    let pipe = adapter.active_pipe(surface)?;
    let (w, h) = adapter.timings(pipe);
    let (sw, sh) = adapter.source(pipe);
    let fitted = adapter.read(pf_ctl(pipe)) & PF_ENABLE != 0;
    (fitted && (w, h) != (sw, sh) && w >= sw && h >= sh).then_some((w, h))
}

/// Перевести экран в родное разрешение панели.
pub fn set_mode(current: &Framebuffer, width: u32, height: u32) -> Result<Framebuffer, DisplayError> {
    adapter(current)?;
    let guard = ADAPTER.lock();
    let adapter = guard.as_ref().ok_or(DisplayError::NoDriver("no Intel graphics"))?;
    let base = super::physical(current.base);
    let surface = base.checked_sub(adapter.aperture).ok_or(DisplayError::Refused("the picture is outside the aperture"))?;
    let pipe = adapter.active_pipe(surface).ok_or(DisplayError::Refused("no pipe shows the firmware picture"))?;
    let (panel_w, panel_h) = adapter.timings(pipe);
    if (width, height) != (panel_w, panel_h) && (width, height) != adapter.source(pipe) {
        return Err(DisplayError::Refused("only the panel's own mode can be set without changing clocks"));
    }

    // Шаг строки кратен 64 байтам — требование плоскости.
    let stride = (width * 4).next_multiple_of(64);
    let needed = u64::from(stride) * u64::from(height);
    // Вся новая картинка обязана быть отображена GTT: апертура показывает
    // видеопамять только через неё, и страница-дыра значила бы запись в никуда.
    let mut page = surface & !0xFFF;
    while page < surface + needed {
        if adapter.gtt_entry(page).is_none() {
            return Err(DisplayError::TooLarge { needed, available: page - surface });
        }
        page += PAGE_SIZE as u64;
    }

    // Новое окно процессора в ту же видеопамять, побольше.
    let len = (needed as usize).next_multiple_of(PAGE_SIZE);
    let phys = PhysAddr::new(base);
    let virt = phys.to_direct_map();
    // SAFETY: память кадра адаптера (апертура); записи можно объединять.
    unsafe { crate::arch::map_active(virt, phys, len, PageFlags::READ | PageFlags::WRITE | PageFlags::WRITE_COMBINE) }
        .map_err(|_| DisplayError::OutOfMemory)?;
    // Новая область кадра — чёрной: за краем старой картинки лежит то, что
    // оставила прошивка, и до первой перерисовки стола это видно.
    // SAFETY: только что отображённая память кадра.
    unsafe { core::ptr::write_bytes(virt.as_usize() as *mut u8, 0, len) };

    // Порядок как у Linux при fastset: масштабировщик прочь, источник трубы
    // и шаг плоскости — новые; запись `DSPSURF` защёлкивает плоскость на
    // ближайшем кадровом гасящем импульсе.
    adapter.write(pf_ctl(pipe), 0);
    adapter.write(pf_win_pos(pipe), 0);
    adapter.write(pf_win_sz(pipe), 0);
    adapter.write(pipesrc(pipe), ((width - 1) << 16) | (height - 1));
    adapter.write(dspstride(pipe), stride);
    if adapter.generation <= 6 {
        adapter.write(dsplinoff(pipe), 0);
    }
    adapter.write(dsptileoff(pipe), 0);
    adapter.write(dspsurf(pipe), adapter.read(dspsurf(pipe)));
    ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);

    Ok(Framebuffer {
        base: virt.as_usize() as u64,
        size: len as u64,
        width,
        height,
        stride: stride / 4,
        format: current.format,
    })
}

/// Команда `intel` оболочки ядра: что видит драйвер — и сырые регистры для
/// разбора на чужом железе (`intel peek 70180`, `intel poke 68080 0`).
pub fn command(current: &Framebuffer, argument: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Err(err) = adapter(current) {
        out.push(format!("intel: {err}"));
        return out;
    }
    let guard = ADAPTER.lock();
    let Some(adapter) = guard.as_ref() else {
        return out;
    };
    let mut words = argument.split_whitespace();
    let parse = |text: Option<&str>| text.and_then(|t| usize::from_str_radix(t.trim_start_matches("0x"), 16).ok());
    match words.next() {
        Some("peek") => match parse(words.next()) {
            Some(offset) if offset + 4 <= 4 * 1024 * 1024 => out.push(format!("{offset:#07x} = {:#010x}", adapter.read(offset & !3))),
            _ => out.push(String::from("usage: intel peek <hex offset>")),
        },
        Some("poke") => match (parse(words.next()), parse(words.next())) {
            (Some(offset), Some(value)) if offset + 4 <= 4 * 1024 * 1024 => {
                adapter.write(offset & !3, value as u32);
                out.push(format!("{offset:#07x} <- {value:#010x}, reads {:#010x}", adapter.read(offset & !3)));
            }
            _ => out.push(String::from("usage: intel poke <hex offset> <hex value>")),
        },
        _ => {
            out.push(format!(
                "intel: {} {:04x}:{:04x} generation {}, registers at {:#x}, aperture at {:#x}",
                adapter.device.address, adapter.device.vendor, adapter.device.device,
                if adapter.generation == 75 { String::from("7.5") } else { format!("{}", adapter.generation) },
                super::physical(adapter.mmio.as_usize() as u64),
                adapter.aperture
            ));
            let surface = super::physical(current.base).saturating_sub(adapter.aperture);
            out.push(format!("intel: firmware picture {}x{} stride {} at aperture offset {surface:#x}", current.width, current.height, current.stride));
            for pipe in 0..3 {
                let conf = adapter.read(pipeconf(pipe));
                if conf & ENABLE == 0 {
                    out.push(format!("intel: pipe {} off", b'A' + pipe as u8));
                    continue;
                }
                let (w, h) = adapter.timings(pipe);
                let (sw, sh) = adapter.source(pipe);
                out.push(format!(
                    "intel: pipe {} timings {w}x{h}, source {sw}x{sh}, fitter {:#010x} pos {:#010x} size {:#010x}",
                    b'A' + pipe as u8,
                    adapter.read(pf_ctl(pipe)),
                    adapter.read(pf_win_pos(pipe)),
                    adapter.read(pf_win_sz(pipe))
                ));
                out.push(format!(
                    "intel: plane {} control {:#010x} stride {} surface {:#x}",
                    b'A' + pipe as u8,
                    adapter.read(dspcntr(pipe)),
                    adapter.read(dspstride(pipe)),
                    adapter.read(dspsurf(pipe))
                ));
            }
            if adapter.generation >= 75 {
                out.push(format!("intel: eDP transcoder {:#010x}", adapter.read(TRANS_DDI_FUNC_CTL_EDP)));
            }
            // Сколько GTT отображает подряд от начала картинки — столько и
            // можно отдать под кадр.
            let mut mapped = 0u64;
            while mapped < 64 * 1024 * 1024 && adapter.gtt_entry(surface + mapped).is_some() {
                mapped += PAGE_SIZE as u64;
            }
            out.push(format!("intel: GTT maps {} KiB from the picture on", mapped / 1024));
            drop(guard);
            match native(current) {
                Some((w, h)) => out.push(format!("intel: the panel is {w}x{h} and the firmware stretches its picture; 'intel native' switches")),
                None => out.push(String::from("intel: the firmware picture is already the panel's own mode")),
            }
        }
    }
    out
}
