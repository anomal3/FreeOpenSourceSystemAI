//! Мониторы (16.11.2026): «Параметры → Дисплей» как в Windows — несколько
//! мониторов, у каждого разрешение и частота из его же паспорта (EDID),
//! расположение относительно друг друга, основной монитор.
//!
//! # Что здесь
//!
//! Реестр выходов: кто их даёт (драйвер и номер выхода у него), монитор на
//! выходе (EDID → [`edid::Monitor`]), его режимы, выбранный режим и место на
//! общем рабочем столе. Реестр не рисует и не выставляет режимы сам — это
//! делают драйверы ([`Backend`]); стол спрашивает у реестра раскладку, а
//! «Параметры» её меняют.
//!
//! # Координаты
//!
//! Место выхода — его левый верхний угол в координатах рабочего стола, как в
//! Windows: основной монитор в (0, 0), остальные где угодно вокруг, в том числе
//! левее и выше (отрицательные координаты). Стол сам переводит их в свои.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;

/// Кто показывает выход.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Картинка прошивки (UEFI GOP): один экран, режим не меняется.
    Firmware,
    /// virtio-gpu, номер выхода (scanout).
    Virtio(usize),
    /// Видеокарта NVIDIA через GSP-RM: идентификатор выхода (`displayId`).
    Nvidia(u32),
}

/// Один выход: монитор, его режимы и место на столе.
#[derive(Clone, Debug)]
pub struct Output {
    pub backend: Backend,
    /// Имя для «Параметров»: модель монитора или имя выхода.
    pub name: String,
    pub monitor: Option<edid::Monitor>,
    /// Режимы, которые монитор принимает (из EDID; без него — то, что есть).
    pub modes: Vec<edid::Mode>,
    /// Выставленный режим (ширина, высота, частота в мГц).
    pub current: Option<(u32, u32, u32)>,
    /// Место на рабочем столе.
    pub x: i32,
    pub y: i32,
    pub enabled: bool,
    pub primary: bool,
    /// Картинка выхода, в которую рисует стол (если драйвер её завёл).
    pub surface: Option<boot_info::Framebuffer>,
}

impl Output {
    /// Ширина и высота выставленного режима.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        self.current.map_or((0, 0), |(w, h, _)| (w, h))
    }

    /// Ключ для сохранённой раскладки: производитель, модель, серийный номер
    /// монитора — так раскладка переживает перестановку кабелей.
    #[must_use]
    pub fn key(&self) -> String {
        match &self.monitor {
            Some(m) => monitor_key(m),
            None => alloc::format!("{:?}", self.backend),
        }
    }
}

/// Ключ монитора по его паспорту (см. [`Output::key`]).
#[must_use]
pub fn monitor_key(m: &edid::Monitor) -> String {
    alloc::format!("{}{:04x}{:08x}", m.maker_str(), m.product, m.serial)
}

/// Ключи всех выходов по порядку реестра. Два одинаковых монитора без
/// серийного номера (у QEMU оба выхода отдают один и тот же паспорт) дали бы
/// один ключ — второму и следующим дописывается `#2`, `#3`.
#[must_use]
pub fn keys(list: &[Output]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for o in list {
        let base = o.key();
        let same = out.iter().filter(|k| **k == base || k.starts_with(&alloc::format!("{base}#"))).count();
        out.push(if same == 0 { base } else { alloc::format!("{base}#{}", same + 1) });
    }
    out
}

/// Сохранённое место монитора (строка `outputs=` в `desktop.cfg`): ключ
/// монитора, режим, место, включён ли, основной ли.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub key: String,
    pub w: u32,
    pub h: u32,
    pub refresh_mhz: u32,
    pub x: i32,
    pub y: i32,
    pub enabled: bool,
    pub primary: bool,
}

/// Раскладка, подтверждённая в «Параметрах» (читается при запуске стола).
static SAVED: SpinLock<Vec<Saved>> = SpinLock::new(Vec::new());

/// Разобрать строку `outputs=`: записи через `;`, каждая —
/// `ключ:ШxВ@мГц,x,y,вкл,осн`. Испорченная запись пропускается молча —
/// опечатка в одной не обязана отменять остальные.
#[must_use]
pub fn parse_saved(text: &str) -> Vec<Saved> {
    let mut out = Vec::new();
    for entry in text.split(';') {
        let Some((key, rest)) = entry.trim().rsplit_once(':') else { continue };
        let mut parts = rest.split(',');
        let mode = parts.next().unwrap_or("");
        let (size, hz) = mode.split_once('@').unwrap_or((mode, "0"));
        let Some((w, h)) = size.split_once('x') else { continue };
        let (Ok(w), Ok(h), Ok(refresh_mhz)) = (w.parse(), h.parse(), hz.parse()) else { continue };
        let nums: Vec<i32> = parts.filter_map(|p| p.trim().parse().ok()).collect();
        let [x, y, enabled, primary] = nums[..] else { continue };
        out.push(Saved { key: String::from(key), w, h, refresh_mhz, x, y, enabled: enabled != 0, primary: primary != 0 });
    }
    out
}

/// Строка `outputs=` из записей (обратное [`parse_saved`]).
#[must_use]
pub fn format_saved(list: &[Saved]) -> String {
    let mut out = String::new();
    for s in list {
        if !out.is_empty() {
            out.push(';');
        }
        out.push_str(&alloc::format!(
            "{}:{}x{}@{},{},{},{},{}",
            s.key,
            s.w,
            s.h,
            s.refresh_mhz,
            s.x,
            s.y,
            u8::from(s.enabled),
            u8::from(s.primary)
        ));
    }
    out
}

/// Запомнить подтверждённую раскладку (без записи в файл — это `prefs`).
pub fn set_saved(list: Vec<Saved>) {
    *SAVED.lock() = list;
}

#[must_use]
pub fn saved() -> Vec<Saved> {
    SAVED.lock().clone()
}

/// Сохранённый режим монитора с таким паспортом — если монитор его ещё
/// предлагает. Нужен драйверу NVIDIA, который зажигает мониторы сам, до
/// раскладки: иначе он поставил бы родной режим, а потом стол менял бы его
/// второй раз.
#[must_use]
pub fn saved_mode(m: &edid::Monitor) -> Option<edid::Mode> {
    let key = monitor_key(m);
    let saved = SAVED.lock();
    let s = saved.iter().find(|s| s.key == key && s.enabled)?;
    m.find(s.w, s.h, s.refresh_mhz).copied()
}

/// Режим из сохранённой записи, если монитор его предлагает; иначе — тот,
/// в котором выход включается сам ([`default_mode`]).
#[must_use]
pub fn mode_or_default(o: &Output, w: u32, h: u32, refresh_mhz: u32) -> (u32, u32, u32) {
    if let Some(m) = o.monitor.as_ref().and_then(|m| m.find(w, h, refresh_mhz)) {
        return (m.width, m.height, m.refresh_mhz());
    }
    if o.monitor.is_none() && o.modes.iter().any(|m| m.width == w && m.height == h) {
        return (w, h, refresh_mhz);
    }
    default_mode(o)
}

static OUTPUTS: SpinLock<Vec<Output>> = SpinLock::new(Vec::new());

/// Все известные выходы.
#[must_use]
pub fn list() -> Vec<Output> {
    OUTPUTS.lock().clone()
}

/// Заменить реестр (после опроса драйверов или применения раскладки).
pub fn replace(outputs: Vec<Output>) {
    *OUTPUTS.lock() = outputs;
}

/// Изменить один выход по его драйверу.
pub fn update(backend: Backend, change: impl FnOnce(&mut Output)) {
    if let Some(output) = OUTPUTS.lock().iter_mut().find(|o| o.backend == backend) {
        change(output);
    }
}

/// Режимы монитора из EDID; без паспорта — один режим `fallback`.
#[must_use]
pub fn modes_from(edid: &[u8], fallback: (u32, u32)) -> (Option<edid::Monitor>, Vec<edid::Mode>) {
    match edid::parse(edid) {
        Ok(monitor) => {
            let modes = monitor.modes.clone();
            (Some(monitor), modes)
        }
        Err(_) => (None, alloc::vec![edid::cvt(fallback.0, fallback.1, 60, edid::Source::Standard)]),
    }
}

/// Опросить virtio-gpu: выходы с мониторами, рядом слева направо.
pub fn probe_virtio() -> Vec<Output> {
    let mut out = Vec::new();
    let mut x = 0i32;
    for head in crate::virtio::gpu::heads() {
        // Подключён — включён, показан или отдаёт паспорт монитора.
        if !head.enabled && head.shown.is_none() && head.edid.is_empty() {
            continue;
        }
        let (monitor, modes) = modes_from(&head.edid, head.preferred);
        let current = head
            .shown
            .map(|(w, h)| (w, h, monitor.as_ref().and_then(|m| m.find(w, h, 60_000)).map_or(60_000, edid::Mode::refresh_mhz)));
        let name = monitor
            .as_ref()
            .map(|m| if m.name.is_empty() { String::from(m.maker_str()) } else { m.name.clone() })
            .unwrap_or_else(|| alloc::format!("Virtual output {}", head.scanout + 1));
        let width = head.shown.map_or(head.preferred.0, |s| s.0) as i32;
        out.push(Output {
            backend: Backend::Virtio(head.scanout),
            name,
            monitor,
            modes,
            current,
            x,
            y: 0,
            enabled: head.enabled || head.shown.is_some(),
            primary: head.scanout == 0,
            surface: None,
        });
        x += width;
    }
    out
}

/// Режим, в котором выход включается, если его не выбирали: выставленный,
/// родной режим монитора из EDID, первый из списка — и 1024×768 на крайний
/// случай.
#[must_use]
pub fn default_mode(o: &Output) -> (u32, u32, u32) {
    if let Some(current) = o.current.filter(|c| c.0 != 0 && c.1 != 0) {
        return current;
    }
    let best = o.monitor.as_ref().and_then(edid::Monitor::preferred).or_else(|| o.modes.first());
    best.map_or((1024, 768, 60_000), |m| (m.width, m.height, m.refresh_mhz()))
}

/// Подключить к столу остальные мониторы virtio-gpu (16.11): каждому
/// подключённому выходу, кроме основного, — картинка в его режиме, место
/// справа от уже стоящих. Возвращает, сколько выходов включено.
///
/// Подключён — значит, хост отдал паспорт монитора или сам включил выход:
/// у QEMU с `max_outputs=2` второй выход «выключен», пока гость не поставит
/// на него картинку, но EDID у него есть.
///
/// NVIDIA здесь не нужна: её выходы приходят в реестр уже с картинками
/// ([`nvidia_lit`]).
pub fn attach_secondary() -> usize {
    let mut attached = 0;
    for o in list() {
        let Backend::Virtio(scanout) = o.backend else { continue };
        if o.primary || o.surface.is_some() {
            continue;
        }
        if !o.enabled && o.monitor.is_none() {
            continue;
        }
        let (w, h, refresh) = default_mode(&o);
        match crate::virtio::gpu::take_scanout(scanout, w, h) {
            Ok(fb) => {
                let x = {
                    let outputs = OUTPUTS.lock();
                    outputs
                        .iter()
                        .filter(|other| other.backend != o.backend && (other.enabled || other.surface.is_some()))
                        .map(|other| other.x + other.size().0 as i32)
                        .max()
                        .unwrap_or(0)
                };
                update(o.backend, |out| {
                    out.current = Some((w, h, refresh));
                    out.surface = Some(fb);
                    out.enabled = true;
                    out.x = x;
                    out.y = 0;
                });
                crate::kprintln!("  outputs     : {:?} attached, {w}x{h} at {x},0", o.backend);
                attached += 1;
            }
            Err(err) => crate::kprintln!("  outputs     : {:?} could not show {w}x{h}: {err}", o.backend),
        }
    }
    attached
}

/// Выставить режим одному выходу через его драйвер и отдать картинку, в
/// которую рисовать. Основной экран у адаптеров без нескольких выходов
/// (stdvga, VMSVGA, ramfb, Intel) меняет [`super::set_mode`], не это.
pub fn set_output_mode(backend: Backend, w: u32, h: u32, refresh_mhz: u32) -> Result<boot_info::Framebuffer, super::DisplayError> {
    if w < 320 || h < 200 || w > 4096 || h > 4096 {
        return Err(super::DisplayError::Refused("mode outside 320x200..4096x4096"));
    }
    match backend {
        Backend::Virtio(scanout) => {
            let fb = crate::virtio::gpu::take_scanout(scanout, w, h)?;
            update(backend, |out| {
                let refresh = out.monitor.as_ref().and_then(|m| m.find(w, h, if refresh_mhz == 0 { 60_000 } else { refresh_mhz })).map_or(60_000, edid::Mode::refresh_mhz);
                out.current = Some((w, h, refresh));
                out.surface = Some(fb);
                out.enabled = true;
            });
            Ok(fb)
        }
        Backend::Nvidia(id) => {
            let _ = super::nvidia_disp::modeset(&[(id, w, h, refresh_mhz)]);
            list()
                .into_iter()
                .find(|o| o.backend == backend)
                .and_then(|o| o.surface)
                .ok_or(super::DisplayError::Refused("the NVIDIA output is not lit"))
        }
        Backend::Firmware => Err(super::DisplayError::NoDriver("the firmware picture has one fixed mode")),
    }
}

/// Погасить выход (у virtio-gpu — снять картинку и вернуть память).
pub fn release_output(backend: Backend) {
    if let Backend::Virtio(scanout) = backend {
        crate::virtio::gpu::release_scanout(scanout);
    }
    update(backend, |out| {
        out.enabled = false;
        out.surface = None;
        out.current = None;
    });
}

/// Описание выходов для журнала загрузки.
pub fn log_outputs(outputs: &[Output]) {
    for o in outputs {
        let (w, h, r) = o.current.unwrap_or((0, 0, 0));
        let best = o.monitor.as_ref().and_then(edid::Monitor::preferred);
        crate::kprintln!(
            "  outputs     : {:?} '{}'{}: {}x{}@{}.{:03} at {},{}, {} mode(s){}",
            o.backend,
            o.name,
            if o.primary { " (primary)" } else { "" },
            w,
            h,
            r / 1000,
            r % 1000,
            o.x,
            o.y,
            o.modes.len(),
            best.map_or(String::new(), |m| alloc::format!(", native {}x{}@{}", m.width, m.height, m.refresh_hz()))
        );
    }
}

/// NVIDIA зажгла выход `id` головкой `head` в режиме `mode`; картинка лежит по
/// адресу `base` (окно BAR1). Выход появляется в реестре или обновляется.
/// `monitor` — паспорт монитора: по нему «Параметры» дают список режимов, а
/// сохранённая раскладка узнаёт монитор (ключ [`Output::key`]).
pub fn nvidia_lit(id: u32, head: u32, mode: edid::Mode, base: usize, monitor: &edid::Monitor) {
    let pitch = (mode.width * 4).div_ceil(64) * 64;
    let surface = boot_info::Framebuffer {
        base: base as u64,
        size: u64::from(pitch) * u64::from(mode.height),
        width: mode.width,
        height: mode.height,
        stride: pitch / 4,
        format: boot_info::PixelFormat::Bgr,
    };
    let mut outputs = OUTPUTS.lock();
    let x = outputs.iter().filter(|o| o.backend != Backend::Nvidia(id)).map(|o| o.x + o.size().0 as i32).max().unwrap_or(0);
    let current = Some((mode.width, mode.height, mode.refresh_mhz()));
    if let Some(o) = outputs.iter_mut().find(|o| o.backend == Backend::Nvidia(id)) {
        o.current = current;
        o.surface = Some(surface);
        o.enabled = true;
        return;
    }
    let primary = !outputs.iter().any(|o| o.primary && o.enabled);
    let name = if monitor.name.is_empty() { alloc::format!("NVIDIA output {id:#x} (head {head})") } else { monitor.name.clone() };
    outputs.push(Output {
        backend: Backend::Nvidia(id),
        name,
        monitor: Some(monitor.clone()),
        modes: if monitor.modes.is_empty() { alloc::vec![mode] } else { monitor.modes.clone() },
        current,
        x,
        y: 0,
        enabled: true,
        primary,
        surface: Some(surface),
    });
}
