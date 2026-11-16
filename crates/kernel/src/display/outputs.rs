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
            Some(m) => alloc::format!("{}{:04x}{:08x}", m.maker_str(), m.product, m.serial),
            None => alloc::format!("{:?}", self.backend),
        }
    }
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
pub fn nvidia_lit(id: u32, head: u32, mode: edid::Mode, base: usize) {
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
    outputs.push(Output {
        backend: Backend::Nvidia(id),
        name: alloc::format!("NVIDIA output {id:#x} (head {head})"),
        monitor: None,
        modes: alloc::vec![mode],
        current,
        x,
        y: 0,
        enabled: true,
        primary,
        surface: Some(surface),
    });
}
