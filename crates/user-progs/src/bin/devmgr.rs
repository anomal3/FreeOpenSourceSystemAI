//! Диспетчер устройств: что стоит в машине, чем оно обслуживается и у чего
//! драйвера нет.
//!
//! # Зачем
//!
//! Правило вехи v0.7b: человек с Windows не должен выяснять расспросами, почему
//! «сеть не работает». До фазы С7 ответ жил только в журнале загрузки —
//! строками вроде `network : no virtio-net card attached`, — а журнала на чужой
//! машине нет вовсе. Окно показывает то же самое словами: каждое устройство на
//! шине PCI, на USB и каждый диск, драйвер ядра и его состояние.
//!
//! # Что здесь из ядра
//!
//! Всё — вызовом `SYS_DEVICES`, строками. Ядро обходит шину PCI заново на каждый
//! вопрос и спрашивает драйверы, что они подняли; окно только раскладывает это
//! по группам и переводит классы на русский. Окно и цикл — у `user_progs::app`,
//! панели, таблица и пометка о строках за краем — у `mini_ui::kit` (фаза С8).
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
use mini_ui::kit::{self, Frame, Table};
use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface};
use user_progs::app::{self, App, Spec};
use user_progs::{
    SysInfo, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_UP, devices, println,
};

const SPEC: Spec = Spec {
    name: "devmgr",
    title: "Device Manager",
    // Устройства меняются редко — горячим подключением, — и чаще спрашивать
    // шину незачем.
    period_ms: 3_000,
};

const DEVICES_LIMIT: usize = 16 * 1024;
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

/// Столбцы по важности: узкое окно теряет сначала место и идентификатор, а не
/// состояние. Снимок AArch64 с окном в 682 точки показал обратное — исчезал
/// именно «СОСТОЯНИЕ», ради которого окно и открывают.
const TITLES: [&str; 5] = ["УСТРОЙСТВО", "ДРАЙВЕР", "СОСТОЯНИЕ", "МЕСТО", "ИДЕНТИФИКАТОР"];

fn columns(ctx: Ctx, table: &Table) -> Vec<Rect> {
    table.columns(ctx, &[0, ctx.px(100), ctx.px(190), ctx.px(120), ctx.px(110)], ctx.px(200))
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

    /// Первая показанная строка списка: выбранное устройство всегда на экране.
    fn first_row(&self, shown: usize) -> usize {
        let at = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Device(index) if *index == self.selected))
            .unwrap_or(0);
        kit::first_visible(at, shown)
    }
}

impl App for Manager {
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

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let table = Table::new(ctx, Frame::new(ctx, area).body).with_row(ctx.px(30));
        let shown = table.shown(self.rows.len());
        let Some(offset) = table.offset_at(x, y, shown) else {
            return false;
        };
        match self.rows.get(self.first_row(shown) + offset) {
            Some(Row::Device(index)) => {
                self.select(*index);
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        self.refresh();
        true
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, mini_ui::theme::window_bg());
        let frame = Frame::new(ctx, area);

        // Панель: что это и сколько всего.
        let bar = kit::toolbar(ctx, s, frame.toolbar);
        kit::toolbar_title(bar, s, frame.toolbar, "Устройства этого компьютера");
        let total = self.devices.len();
        let missing = self.devices.iter().filter(|device| device.state == State::Missing).count();
        let count = if missing == 0 {
            format!("{total} {}", devices_word(total))
        } else {
            format!("{total} {}, без драйвера: {missing}", devices_word(total))
        };
        paint::text_right(
            bar,
            s,
            Role::Mono,
            frame.toolbar.right() - ctx.px(16) as i32,
            paint::baseline(bar, Role::Mono, frame.toolbar),
            &count,
            if missing == 0 { p.ink3 } else { p.bad_ink },
        );

        let table = Table::new(ctx, frame.body).with_row(ctx.px(30));
        let cols = columns(ctx, &table);
        table.draw_header(ctx, s, &cols, &TITLES);

        let shown = table.shown(self.rows.len());
        let first = self.first_row(shown);
        for (offset, row) in self.rows.iter().skip(first).take(shown).enumerate() {
            let rect = table.row_rect(ctx, offset);
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
                        let x = if column == 0 { tile.right() + ctx.px(10) as i32 } else { col.x };
                        kit::cell(ctx, s, *col, x, rect, role, &text, ink);
                    }
                }
            }
        }
        table.draw_overflow(ctx, s, self.rows.len(), first, shown);
        if self.devices.is_empty() {
            let rect = table.row_rect(ctx, 0);
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
        kit::status_bar(ctx, s, frame.status, &text);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::two_thirds(&info);
    app::run(&SPEC, &info, size, |_, _| Manager::new())
}
