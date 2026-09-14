//! Диспетчер устройств: что стоит в машине, чем оно обслуживается и у чего
//! драйвера нет.
//!
//! # Зачем
//!
//! Правило вехи v0.7b: человек с Windows не должен выяснять расспросами, почему
//! «сеть не работает». До этой фазы ответ жил только в журнале загрузки —
//! строками вроде `network : no virtio-net card attached`, — а журнала на чужой
//! машине нет вовсе. Окно показывает то же самое словами: каждое устройство на
//! шине PCI, на USB и каждый диск, драйвер ядра и его состояние.
//!
//! # Что здесь из ядра
//!
//! Всё — вызовом `SYS_DEVICES`, строками. Ядро обходит шину PCI заново на каждый
//! вопрос и спрашивает драйверы, что они подняли; окно только раскладывает это
//! по группам и переводит классы на русский.
//!
//! # Чего окно не умеет и говорит об этом
//!
//! Устанавливать и отключать драйверы: драйверы живут в ядре, и подгружать их
//! нечем. Для устройства без драйвера строка состояния так и говорит —
//! «функция запланирована», по правилу вехи.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::glyphicon::Icon;
use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};
use user_progs::{
    Args, SYSINFO_DARK, SysInfo, WIN_CLOSE, WIN_KEY, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME,
    WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_UP, WIN_POINTER, Window, devices, exit,
    monotonic_ms, nanosleep, println, sysinfo,
};

/// Имя окна. Латиницей: по нему стенд наводит мышь.
const TITLE: &str = "Device Manager";

/// Как часто перечитывать перепись. Устройства меняются редко — горячим
/// подключением, — и чаще спрашивать шину незачем.
const PERIOD_MS: u64 = 3_000;
const POLL_NS: u32 = 30_000_000;
const DEVICES_LIMIT: usize = 16 * 1024;
const OPEN_WAIT_MS: u64 = 30_000;
const WAIT_GRAPHICS_MS: u64 = 10_000;

const QUIT: u32 = 'q' as u32;
const REFRESH: u32 = 'r' as u32;

// ---------------------------------------------------------------------------
// Перепись
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bus {
    Pci,
    Usb,
    Disk,
}

impl Bus {
    const ALL: [Self; 3] = [Self::Pci, Self::Usb, Self::Disk];

    fn parse(text: &str) -> Option<Self> {
        match text {
            "pci" => Some(Self::Pci),
            "usb" => Some(Self::Usb),
            "disk" => Some(Self::Disk),
            _ => None,
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Pci => "pci",
            Self::Usb => "usb",
            Self::Disk => "disk",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Pci => "ШИНА PCI",
            Self::Usb => "USB",
            Self::Disk => "ДИСКИ",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Active,
    Idle,
    Missing,
    NotNeeded,
}

impl State {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "active" => Some(Self::Active),
            "idle" => Some(Self::Idle),
            "none" => Some(Self::Missing),
            "not-needed" => Some(Self::NotNeeded),
            _ => None,
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Missing => "none",
            Self::NotNeeded => "not-needed",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Active => "Работает",
            Self::Idle => "Драйвер есть, не занят",
            Self::Missing => "Нет драйвера",
            Self::NotNeeded => "Драйвер не нужен",
        }
    }

    const fn tone(self) -> Tone {
        match self {
            Self::Active => Tone::Ok,
            Self::Idle => Tone::Accent,
            Self::Missing => Tone::Warn,
            Self::NotNeeded => Tone::Muted,
        }
    }
}

struct Device {
    bus: Bus,
    place: String,
    id: String,
    what: String,
    driver: Option<String>,
    state: State,
}

impl Device {
    /// Строка ответа `SYS_DEVICES`. Строка другого вида пропускается, а не
    /// роняет окно: ядро новее программы вправе дописать поле.
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let bus = Bus::parse(fields.next()?)?;
        let place = fields.next()?.to_string();
        let id = fields.next()?.to_string();
        let what = fields.next()?.to_string();
        let driver = fields.next()?;
        let state = State::parse(fields.next()?)?;
        Some(Self {
            bus,
            place,
            id,
            what,
            driver: (driver != "-").then(|| driver.to_string()),
            state,
        })
    }

    fn icon(&self) -> Icon {
        let what = self.what.as_str();
        match self.bus {
            Bus::Usb if what == "keyboard" => Icon::Keyboard,
            Bus::Usb => Icon::Devices,
            Bus::Disk => Icon::Disk,
            Bus::Pci if what.contains("usb") => Icon::Devices,
            Bus::Pci if what.contains("network") || what.contains("ethernet") => Icon::Network,
            Bus::Pci if what.contains("display") || what.contains("vga") => Icon::Display,
            Bus::Pci
                if ["storage", "sata", "nvme", "ide"].iter().any(|word| what.contains(word)) =>
            {
                Icon::Disk
            }
            Bus::Pci => Icon::Pci,
        }
    }

    /// Строка журнала — по ней стенд проверяет перепись.
    fn log_line(&self) -> String {
        format!(
            "devmgr: {} {} {} {}: {} {}",
            self.bus.tag(),
            self.place,
            self.id,
            self.what,
            self.driver.as_deref().unwrap_or("-"),
            self.state.tag()
        )
    }
}

/// Класс словами — по-русски. Неизвестное слово показывается как есть:
/// ядро новее программы не должно превращать устройство в пустую строку.
fn russian(what: &str) -> String {
    let text = match what {
        "usb controller (xhci)" => "USB-контроллер (xHCI)",
        "usb controller (ohci)" => "USB-контроллер (OHCI)",
        "usb controller (ehci)" => "USB-контроллер (EHCI)",
        "usb controller (uhci)" => "USB-контроллер (UHCI)",
        "usb controller" => "USB-контроллер",
        "sata controller (ahci)" => "SATA-контроллер (AHCI)",
        "sata controller" => "SATA-контроллер",
        "nvme controller" => "NVMe-контроллер",
        "scsi storage controller" => "Контроллер хранения",
        "ide controller" => "IDE-контроллер",
        "storage controller" => "Контроллер хранения",
        "ethernet controller" => "Сетевой адаптер Ethernet",
        "network controller" => "Сетевой адаптер",
        "vga display" => "Видеоадаптер VGA",
        "display controller" => "Видеоадаптер",
        "multimedia controller" => "Мультимедиа",
        "memory controller" => "Контроллер памяти",
        "host bridge" => "Главный мост",
        "isa bridge" => "Мост ISA",
        "pci bridge" => "Мост PCI",
        "bridge" => "Мост",
        "communication controller" => "Контроллер связи",
        "system peripheral" => "Системное устройство",
        "input controller" => "Контроллер ввода",
        "smbus controller" => "Контроллер SMBus",
        "serial bus controller" => "Контроллер шины",
        "unclassified device" => "Неопознанное устройство",
        "other device" => "Прочее устройство",
        "keyboard" => "Клавиатура",
        "mouse" => "Мышь",
        "disk" => "Диск",
        other => return other.to_string(),
    };
    text.to_string()
}

/// «1 устройство», «3 устройства», «10 устройств».
fn devices_word(count: usize) -> &'static str {
    match (count % 10, count % 100) {
        (1, rest) if rest != 11 => "устройство",
        (2..=4, rest) if !(12..=14).contains(&rest) => "устройства",
        _ => "устройств",
    }
}

fn list_devices() -> Vec<Device> {
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(DEVICES_LIMIT).is_err() {
        return Vec::new();
    }
    buffer.resize(DEVICES_LIMIT, 0u8);
    let got = devices(&mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    let text = core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("");
    let mut out: Vec<Device> = text.lines().filter_map(Device::parse).collect();
    // Группы — в порядке шин; внутри группы порядок ядра: адреса по шине.
    out.sort_by_key(|device| Bus::ALL.iter().position(|bus| *bus == device.bus).unwrap_or(0));
    out
}

// ---------------------------------------------------------------------------
// Окно
// ---------------------------------------------------------------------------

/// Строка списка: заголовок группы или устройство.
#[derive(Clone, Copy)]
enum Row {
    Header(Bus, usize),
    Device(usize),
}

struct Manager {
    devices: Vec<Device>,
    rows: Vec<Row>,
    selected: usize,
    /// Что было напечатано в журнал в прошлый раз — чтобы печатать только
    /// перемены, а не одно и то же каждые три секунды.
    last_log: String,
}

impl Manager {
    fn new() -> Self {
        let mut manager = Self { devices: Vec::new(), rows: Vec::new(), selected: 0, last_log: String::new() };
        manager.refresh();
        manager
    }

    fn refresh(&mut self) {
        self.devices = list_devices();
        self.rows.clear();
        for bus in Bus::ALL {
            let count = self.devices.iter().filter(|device| device.bus == bus).count();
            if count == 0 {
                continue;
            }
            self.rows.push(Row::Header(bus, count));
            for (index, device) in self.devices.iter().enumerate() {
                if device.bus == bus {
                    self.rows.push(Row::Device(index));
                }
            }
        }
        self.selected = self.selected.min(self.devices.len().saturating_sub(1));

        let mut log = String::new();
        for device in &self.devices {
            log.push_str(&device.log_line());
            log.push('\n');
        }
        let count = |bus: Bus| self.devices.iter().filter(|device| device.bus == bus).count();
        let missing = self.devices.iter().filter(|device| device.state == State::Missing).count();
        log.push_str(&format!(
            "devmgr: {} device(s): {} pci, {} usb, {} disk(s); {missing} without a driver",
            self.devices.len(),
            count(Bus::Pci),
            count(Bus::Usb),
            count(Bus::Disk)
        ));
        if log != self.last_log {
            for line in log.lines() {
                println(line);
            }
            self.last_log = log;
        }
    }

    fn select(&mut self, index: usize) {
        let index = index.min(self.devices.len().saturating_sub(1));
        if index != self.selected {
            self.selected = index;
            if let Some(device) = self.devices.get(index) {
                println(&format!("devmgr: selected {} {} '{}'", device.bus.tag(), device.place, device.what));
            }
        }
    }

    fn key(&mut self, code: u32) -> bool {
        let last = self.devices.len().saturating_sub(1);
        match code {
            WIN_KEY_UP => self.select(self.selected.saturating_sub(1)),
            WIN_KEY_DOWN => self.select(self.selected + 1),
            WIN_KEY_PAGE_UP => self.select(self.selected.saturating_sub(10)),
            WIN_KEY_PAGE_DOWN => self.select(self.selected + 10),
            WIN_KEY_HOME => self.select(0),
            WIN_KEY_END => self.select(last),
            REFRESH => self.refresh(),
            _ => return false,
        }
        true
    }

    /// Номер строки списка, с которой начинается видимая часть: выбранное
    /// устройство всегда на экране.
    fn first_visible(&self, visible: usize) -> usize {
        let at = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Device(index) if *index == self.selected))
            .unwrap_or(0);
        if visible == 0 || at < visible { 0 } else { at + 1 - visible }
    }

    /// Сколько строк списка показывается: если все не помещаются, последняя
    /// отдана пометке о скрытых.
    fn shown(&self, plan: &Plan) -> usize {
        let visible = plan.visible();
        if self.rows.len() > visible { visible.saturating_sub(1) } else { visible }
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        if !plan.list.contains(x, y) {
            return false;
        }
        let offset = ((y - plan.list.y) / plan.row_h.max(1) as i32).max(0) as usize;
        let shown = self.shown(&plan);
        if offset >= shown {
            return false;
        }
        let first = self.first_visible(shown);
        match self.rows.get(first + offset) {
            Some(Row::Device(index)) => {
                self.select(*index);
                true
            }
            _ => false,
        }
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = layout(ctx, area);

        // Панель: что это и сколько всего.
        s.fill(plan.toolbar, ctx.flat(p.panel));
        draw::hline(s, plan.toolbar.x, plan.toolbar.bottom() - 1, plan.toolbar.w, p.line.color, p.line.alpha);
        let bar = ctx.on(theme::panel_bg());
        let pad = ctx.px(16);
        paint::text_clipped(
            bar,
            s,
            Role::Title,
            plan.toolbar.x + pad as i32,
            paint::baseline(bar, Role::Title, plan.toolbar),
            plan.toolbar.w / 2,
            "Устройства этого компьютера",
            p.ink2,
        );
        let missing = self.devices.iter().filter(|device| device.state == State::Missing).count();
        let total = self.devices.len();
        let count = if missing == 0 {
            format!("{total} {}", devices_word(total))
        } else {
            format!("{total} {}, без драйвера: {missing}", devices_word(total))
        };
        paint::text_right(
            bar,
            s,
            Role::Mono,
            plan.toolbar.right() - pad as i32,
            paint::baseline(bar, Role::Mono, plan.toolbar),
            &count,
            if missing == 0 { p.ink3 } else { p.bad_ink },
        );

        // Шапка столбцов.
        let cols = plan.columns(ctx);
        let y = paint::baseline(ctx, Role::MonoCaps, plan.header);
        for (rect, title) in cols.iter().zip(["УСТРОЙСТВО", "ДРАЙВЕР", "СОСТОЯНИЕ", "МЕСТО", "ИДЕНТИФИКАТОР"]) {
            if rect.w > 0 {
                paint::caps(ctx, s, rect.x, y, title);
            }
        }
        draw::hline(
            s,
            plan.header.x + ctx.px(12) as i32,
            plan.header.bottom() - 1,
            plan.header.w.saturating_sub(ctx.px(24)),
            p.line.color,
            p.line.alpha,
        );

        let visible = self.shown(&plan);
        let first = self.first_visible(visible);
        for (offset, row) in self.rows.iter().skip(first).take(visible).enumerate() {
            let rect = plan.row_rect(ctx, offset);
            match *row {
                Row::Header(bus, count) => {
                    let text = format!("{} · {count}", bus.title());
                    paint::caps(ctx, s, rect.x + ctx.px(6) as i32, paint::baseline(ctx, Role::MonoCaps, rect), &text);
                }
                Row::Device(index) => {
                    let Some(device) = self.devices.get(index) else {
                        continue;
                    };
                    let selected = index == self.selected;
                    paint::row(ctx, s, rect, if selected { RowState::Selected } else { RowState::Idle });
                    let side = ctx.px(22);
                    let tile = Rect::new(rect.x + ctx.px(6) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
                    paint::icon_tile(ctx, s, tile, device.icon(), device.state.tone(), false);
                    let state_ink = match device.state {
                        State::Active => p.ok_ink,
                        State::Missing => p.bad_ink,
                        State::Idle => p.ink3,
                        State::NotNeeded => p.ink4,
                    };
                    let cells = [
                        (russian(&device.what), if selected { Role::Title } else { Role::Body }, if selected { p.ink } else { p.ink2 }),
                        (device.driver.clone().unwrap_or_else(|| String::from("—")), Role::Mono, p.ink3),
                        (device.state.title().to_string(), Role::Body, state_ink),
                        (device.place.clone(), Role::Mono, p.ink4),
                        (device.id.clone(), Role::Mono, p.ink4),
                    ];
                    for (column, (col, (text, role, ink))) in cols.iter().zip(cells).enumerate() {
                        if col.w == 0 {
                            continue;
                        }
                        let x = if column == 0 { tile.right() + ctx.px(10) as i32 } else { col.x };
                        let room = (col.right() - x - ctx.px(8) as i32).max(0) as u32;
                        paint::text_clipped(ctx, s, role, x, paint::baseline(ctx, role, rect), room, &text, ink);
                    }
                }
            }
        }
        // Строки за краем названы, а не молча отрезаны: на снимке x86 группа
        // «ДИСКИ · 1» стояла последней видимой строкой, а сам диск — под краем,
        // и окно читалось как «диск не найден».
        if self.rows.len() > plan.visible() {
            let below = self.rows.len().saturating_sub(first + visible);
            let rect = plan.row_rect(ctx, visible);
            let text = format!("строк выше: {first}, ниже: {below} — стрелки, PageUp, PageDown");
            paint::text_clipped(
                ctx,
                s,
                Role::Caption,
                rect.x + ctx.px(6) as i32,
                paint::baseline(ctx, Role::Caption, rect),
                rect.w,
                &text,
                p.ink3,
            );
        }
        if self.devices.is_empty() {
            let rect = plan.row_rect(ctx, 0);
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                rect.x + ctx.px(10) as i32,
                paint::baseline(ctx, Role::Body, rect),
                rect.w,
                "Перепись пуста — ядро не ответило",
                p.bad_ink,
            );
        }

        // Строка состояния: выбранное устройство одной фразой.
        s.fill(plan.status, ctx.flat(p.panel));
        draw::hline(s, plan.status.x, plan.status.y, plan.status.w, p.line2.color, p.line2.alpha);
        let text = match self.devices.get(self.selected) {
            Some(device) => {
                let serves = match (device.state, device.driver.as_deref()) {
                    (State::Active, Some(driver)) => format!("обслуживает драйвер {driver}"),
                    (State::Idle, Some(driver)) => format!("драйвер {driver} есть, но этим устройством не занят"),
                    (State::NotNeeded, _) => String::from("настраивается прошивкой, драйвер не нужен"),
                    _ => String::from("драйвера в системе нет; установка драйверов — функция запланирована"),
                };
                format!("{} · {} · {serves}", russian(&device.what), device.place)
            }
            None => String::from("Стрелки — выбрать устройство    R — обновить"),
        };
        let pad = ctx.px(14);
        paint::text_clipped(
            bar,
            s,
            Role::MonoSmall,
            plan.status.x + pad as i32,
            paint::baseline(bar, Role::MonoSmall, plan.status),
            plan.status.w.saturating_sub(pad * 2),
            &text,
            p.ink4,
        );
    }
}

struct Plan {
    toolbar: Rect,
    header: Rect,
    list: Rect,
    status: Rect,
    row_h: u32,
}

impl Plan {
    fn visible(&self) -> usize {
        (self.list.h / self.row_h.max(1)) as usize
    }

    fn row_rect(&self, ctx: Ctx, offset: usize) -> Rect {
        let pad = ctx.px(12);
        Rect::new(
            self.list.x + pad as i32,
            self.list.y + (offset as u32 * self.row_h) as i32,
            self.list.w.saturating_sub(pad * 2),
            self.row_h.saturating_sub(ctx.px(2)),
        )
    }

    /// Столбцы: имя тянется, остальные — по ширине содержимого; узкое окно
    /// теряет столбцы справа налево, как у диспетчера задач.
    fn columns(&self, ctx: Ctx) -> [Rect; 5] {
        // Порядок — по важности: первыми уходят место и идентификатор, а не
        // состояние. Снимок AArch64 с окном в 682 точки показал обратное —
        // исчезал именно «СОСТОЯНИЕ», ради которого окно и открывают.
        let widths = [0, ctx.px(100), ctx.px(190), ctx.px(120), ctx.px(110)];
        let left = self.list.x + ctx.px(24) as i32;
        let right = self.list.right() - ctx.px(22) as i32;
        let name_min = ctx.px(200);
        let mut used = 0u32;
        let mut shown = 1;
        for width in widths.iter().skip(1) {
            if left + (name_min + used + width) as i32 > right {
                break;
            }
            used += width;
            shown += 1;
        }
        let mut out = [Rect::EMPTY; 5];
        let mut x = right - used as i32;
        out[0] = Rect::new(left, self.list.y, (x - left).max(0) as u32, self.list.h);
        for index in 1..shown {
            out[index] = Rect::new(x, self.list.y, widths[index], self.list.h);
            x += widths[index] as i32;
        }
        out
    }
}

fn layout(ctx: Ctx, area: Rect) -> Plan {
    let toolbar_h = ctx.px(theme::TOOLBAR_H).min(area.h);
    let toolbar = Rect::new(area.x, area.y, area.w, toolbar_h);
    let status_h = ctx.px(theme::STATUS_H);
    let status_y = (area.bottom() - status_h as i32).max(toolbar.bottom());
    let status = Rect::new(area.x, status_y, area.w, status_h);
    let body_h = (status.y - toolbar.bottom()).max(0) as u32;
    let header_h = (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14)).min(body_h);
    let header = Rect::new(area.x, toolbar.bottom(), area.w, header_h);
    let list = Rect::new(area.x, header.bottom(), area.w, body_h.saturating_sub(header_h));
    Plan { toolbar, header, list, status, row_h: ctx.px(30) }
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно в том виде, в каком их положило
    // ядро.
    let _args = unsafe { Args::new(argc, argv) };

    let Some(info) = wait_for_graphics() else {
        println("devmgr: no graphics on this machine, nothing to show");
        exit(0)
    };
    mini_ui::use_raw_format(info.pixel_format);
    theme::set_dark(info.flags & SYSINFO_DARK != 0);

    let width = (info.screen_w * 2 / 3).clamp(320, info.screen_w.max(320));
    let height = (info.screen_h * 2 / 3).clamp(240, info.screen_h.max(240));
    let scale = theme::geometry_scale(info.screen_w.max(1));

    let Some(mut window) = open_patiently(width, height) else {
        println("devmgr: FAILED the desktop never freed up; no window");
        exit(1)
    };
    let base = window.pixels().as_mut_ptr();
    // SAFETY: ядро отобразило ровно `width * height` точек по этому адресу и
    // держит их, пока живо окно; второй ссылки на них нет.
    let Some(mut surface) = (unsafe { Surface::from_raw(base, width, height) }) else {
        println("devmgr: FAILED the surface the kernel gave makes no sense");
        exit(1)
    };

    let area = Rect::new(0, 0, width, height);
    let ctx = Ctx::scaled(scale);
    println(&format!("devmgr: window '{TITLE}' opened, {width}x{height}"));
    let mut manager = Manager::new();

    let mut dark = info.flags & SYSINFO_DARK != 0;
    let mut next = monotonic_ms() + PERIOD_MS;
    let mut dirty = true;
    let reason;

    'live: loop {
        while let Some(event) = window.next_event() {
            match event.kind {
                WIN_CLOSE => {
                    reason = "request";
                    break 'live;
                }
                WIN_KEY if event.code == QUIT => {
                    reason = "'q'";
                    break 'live;
                }
                WIN_KEY => {
                    if manager.key(event.code) {
                        dirty = true;
                    }
                }
                WIN_POINTER if event.code != 2 => {
                    if manager.click(area, ctx, event.x, event.y) {
                        dirty = true;
                    }
                }
                _ => {}
            }
        }

        let now = monotonic_ms();
        if now >= next {
            next = now + PERIOD_MS;
            manager.refresh();
            if let Some(fresh) = sysinfo() {
                let fresh_dark = fresh.flags & SYSINFO_DARK != 0;
                if fresh_dark != dark {
                    dark = fresh_dark;
                    theme::set_dark(dark);
                }
            }
            dirty = true;
        }

        if dirty {
            manager.draw(&mut surface, area, Ctx::scaled(scale));
            if window.commit() >= 0 {
                dirty = false;
            }
        }
        nanosleep(0, POLL_NS);
    }

    println(&format!("devmgr: closing on {reason}"));
    window.close();
    exit(0)
}

fn open_patiently(width: u32, height: u32) -> Option<Window> {
    let deadline = monotonic_ms() + OPEN_WAIT_MS;
    loop {
        match Window::open(TITLE, width, height) {
            Ok(window) => return Some(window),
            Err(code) => {
                if code != user_progs::ERR_AGAIN {
                    println(&format!("devmgr: FAILED opening the window: {code}"));
                    return None;
                }
            }
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}

fn wait_for_graphics() -> Option<SysInfo> {
    let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
    loop {
        let info = sysinfo()?;
        if info.pixel_format != 0 && info.screen_w != 0 {
            return Some(info);
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}
