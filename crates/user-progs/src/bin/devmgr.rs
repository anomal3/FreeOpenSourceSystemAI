//! Диспетчер устройств: что стоит в машине, чем оно обслуживается и у чего
//! драйвера нет (с фазы 69d — по макету Романа).
//!
//! # Зачем
//!
//! Правило вехи v0.7b: человек с Windows не должен выяснять расспросами, почему
//! «сеть не работает». Окно показывает словами каждое устройство на шине PCI,
//! на USB, каждый диск и системные устройства, драйвер ядра и его состояние.
//!
//! # Как устроено с 69d
//!
//! Как в макете: сверху панель (обновить, вид «по типу» / «по шине», поиск),
//! слева фильтры-пилюли и дерево (машина → группы → устройства), справа
//! карточка выбранного с вкладками «Общие», «Драйвер», «Ресурсы», «Сведения»,
//! «События», снизу выдвижной «Журнал ядра».
//!
//! # Что здесь из ядра
//!
//! Всё — `SYS_DEVICES`, строками; с 69d в строке PCI ещё способ прерывания
//! (`MSI-X x4`, `INTA -> GSI 11`), а системные устройства — процессоры, память,
//! прошивка, кадровый буфер — приходят строками `sys`. «События» и журнал —
//! `dmesg` через `SYS_KERNEL_COMMAND`, только у root: журнал видит всю машину.
//!
//! # Чего окно не умеет и говорит об этом
//!
//! Отключать, перезапускать и отсоединять устройства из макета нельзя:
//! драйверы живут в ядре, и выгрузить их нечем. Кнопок под это нет — кнопка,
//! которая ничего не делает, хуже отсутствующей. Драйверы, которых нет в ядре,
//! ищет служба `drvd`; вкладка «Драйвер» так и говорит.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface};
use user_progs::app::{self, App, Spec};
use user_progs::{
    SysInfo, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_RIGHT, WIN_KEY_UP, devices, kernel_command, println,
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
const VIEW: u32 = 'v' as u32;
const LOG: u32 = 'j' as u32;
const FIND: u32 = '/' as u32;
const TAB: u32 = '\t' as u32;
const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;

const TOOLBAR_H: u32 = 52;
const CHIPS_H: u32 = 46;
const TREE_ROW_H: u32 = 32;
const TABS_H: u32 = 42;
const DRAWER_HEAD_H: u32 = 32;
const DRAWER_H: u32 = 150;
const STATUS_H: u32 = 30;

// ---------------------------------------------------------------------------
// Перепись
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bus {
    Pci,
    Usb,
    Disk,
    Sys,
}

impl Bus {
    const ALL: [Self; 4] = [Self::Sys, Self::Pci, Self::Usb, Self::Disk];

    fn parse(text: &str) -> Option<Self> {
        match text {
            "pci" => Some(Self::Pci),
            "usb" => Some(Self::Usb),
            "disk" => Some(Self::Disk),
            "sys" => Some(Self::Sys),
            _ => None,
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Pci => "pci",
            Self::Usb => "usb",
            Self::Disk => "disk",
            Self::Sys => "sys",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Pci => "Шина PCI",
            Self::Usb => "USB",
            Self::Disk => "Диски",
            Self::Sys => "Система",
        }
    }

    const fn long(self) -> &'static str {
        match self {
            Self::Pci => "PCI / PCIe",
            Self::Usb => "USB",
            Self::Disk => "Блочное устройство",
            Self::Sys => "Системная плата",
        }
    }

    const fn icon(self) -> Fi {
        match self {
            Self::Pci => Fi::Layers,
            Self::Usb => Fi::Usb,
            Self::Disk => Fi::Drive,
            Self::Sys => Fi::Cpu,
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

/// Группа дерева «по типу» — как в диспетчере устройств Windows.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Cpu,
    System,
    Display,
    Network,
    Storage,
    Disks,
    Sound,
    UsbControllers,
    UsbDevices,
    Input,
    Other,
}

impl Kind {
    const ALL: [Kind; 11] = [
        Kind::Cpu,
        Kind::System,
        Kind::Display,
        Kind::Network,
        Kind::Storage,
        Kind::Disks,
        Kind::Sound,
        Kind::UsbControllers,
        Kind::UsbDevices,
        Kind::Input,
        Kind::Other,
    ];

    const fn title(self) -> &'static str {
        match self {
            Kind::Cpu => "Процессоры",
            Kind::System => "Системные устройства",
            Kind::Display => "Видеоадаптеры",
            Kind::Network => "Сетевые адаптеры",
            Kind::Storage => "Контроллеры хранения",
            Kind::Disks => "Дисковые устройства",
            Kind::Sound => "Звук и мультимедиа",
            Kind::UsbControllers => "Контроллеры USB",
            Kind::UsbDevices => "Устройства USB",
            Kind::Input => "Клавиатуры и мыши",
            Kind::Other => "Прочие устройства",
        }
    }

    const fn icon(self) -> Fi {
        match self {
            Kind::Cpu => Fi::Cpu,
            Kind::System => Fi::Layers,
            Kind::Display => Fi::Monitor,
            Kind::Network => Fi::Globe,
            Kind::Storage => Fi::Server,
            Kind::Disks => Fi::Drive,
            Kind::Sound => Fi::Bell,
            Kind::UsbControllers | Kind::UsbDevices => Fi::Usb,
            Kind::Input => Fi::Keyboard,
            Kind::Other => Fi::Warning,
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
    /// Чем устройство прерывает (только PCI, с 69d).
    signal: Option<String>,
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
        let signal = fields.next().filter(|text| !text.is_empty()).map(String::from);
        Some(Self { bus, place, id, what, driver: (driver != "-").then(|| driver.to_string()), state, signal })
    }

    fn kind(&self) -> Kind {
        let what = self.what.as_str();
        match self.bus {
            Bus::Sys if what == "processor" => Kind::Cpu,
            Bus::Sys if what == "framebuffer" => Kind::Display,
            Bus::Sys => Kind::System,
            Bus::Disk => Kind::Disks,
            Bus::Usb if what.contains("keyboard") || what.contains("mouse") || what.contains("tablet") || what.contains("pointer") => Kind::Input,
            Bus::Usb if what.contains("storage") => Kind::Disks,
            Bus::Usb if what.contains("net") => Kind::Network,
            Bus::Usb => Kind::UsbDevices,
            Bus::Pci if self.state == State::Missing && !what.contains("bridge") => Kind::Other,
            Bus::Pci if what.contains("usb") => Kind::UsbControllers,
            Bus::Pci if what.contains("network") || what.contains("ethernet") => Kind::Network,
            Bus::Pci if what.contains("display") || what.contains("vga") => Kind::Display,
            Bus::Pci if ["storage", "sata", "nvme", "ide"].iter().any(|word| what.contains(word)) => Kind::Storage,
            Bus::Pci if what.contains("multimedia") || what.contains("audio") => Kind::Sound,
            Bus::Pci => Kind::System,
        }
    }

    fn icon(&self) -> Fi {
        match self.kind() {
            Kind::Input if self.what.contains("keyboard") => Fi::Keyboard,
            Kind::Input => Fi::Mouse,
            Kind::System if self.what == "memory" => Fi::Memory,
            Kind::System if self.what == "firmware" => Fi::Layers,
            kind => kind.icon(),
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

    /// Где стоит — строкой для карточки: «PCI · 00:04.0».
    fn location(&self) -> String {
        match self.bus {
            Bus::Pci => format!("PCI · {}", self.place),
            Bus::Usb => format!("USB · {}", self.place),
            Bus::Disk => self.place.clone(),
            Bus::Sys => String::from("Системная плата"),
        }
    }
}

/// Класс словами — по-русски. Неизвестное слово показывается как есть.
fn russian(what: &str) -> String {
    let text = match what {
        "usb controller (xhci)" => "USB xHCI контроллер",
        "usb controller (ohci)" => "USB OHCI контроллер",
        "usb controller (ehci)" => "USB EHCI контроллер",
        "usb controller (uhci)" => "USB UHCI контроллер",
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
        "keyboard" => "Клавиатура USB",
        "mouse" => "Мышь USB",
        "tablet" => "Планшет USB",
        "disk" => "Диск",
        "processor" => "Процессор",
        "memory" => "Оперативная память",
        "firmware" => "Микропрограмма UEFI",
        "framebuffer" => "Кадровый буфер",
        other => return other.to_string(),
    };
    text.to_string()
}

/// Имя устройства в дереве: класс словами, у процессора — его номер, у диска
/// — номер носителя.
fn display_name(device: &Device) -> String {
    match device.bus {
        Bus::Sys if device.what == "processor" => format!("Процессор {} · {}", device.place.trim_start_matches("cpu "), device.id),
        Bus::Disk => format!("{} {}", russian(&device.what), device.place),
        _ => russian(&device.what),
    }
}

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
    text.lines().filter_map(Device::parse).collect()
}

/// `dmesg [слово]` ядра. Только root.
fn kernel_log(word: &str) -> Result<Vec<String>, String> {
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let line = if word.is_empty() { String::from("dmesg") } else { format!("dmesg {word}") };
    let got = kernel_command(&line, &mut buffer);
    if got < 0 {
        return Err(match got {
            user_abi::ERR_PERMISSION => String::from("Журнал ядра виден только администратору (root)."),
            user_abi::ERR_AGAIN => String::from("Ядро занято другой командой — R, чтобы повторить."),
            code => format!("Журнал не прочитан: код {code}."),
        });
    }
    let text = core::str::from_utf8(&buffer[..got as usize]).unwrap_or("");
    Ok(text.lines().map(|line| line.trim_end().to_string()).filter(|line| !line.is_empty()).collect())
}

// ---------------------------------------------------------------------------
// Окно
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    ByType,
    ByBus,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Working,
    Attention,
    Quiet,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::Working, Filter::Attention, Filter::Quiet];

    fn title(self) -> &'static str {
        match self {
            Filter::All => "Все",
            Filter::Working => "Работают",
            Filter::Attention => "Внимание",
            Filter::Quiet => "Не нужен",
        }
    }

    fn passes(self, device: &Device) -> bool {
        match self {
            Filter::All => true,
            Filter::Working => device.state == State::Active,
            Filter::Attention => matches!(device.state, State::Missing | State::Idle),
            Filter::Quiet => device.state == State::NotNeeded,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Driver,
    Resources,
    Details,
    Events,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::General, Tab::Driver, Tab::Resources, Tab::Details, Tab::Events];

    fn title(self) -> &'static str {
        match self {
            Tab::General => "Общие",
            Tab::Driver => "Драйвер",
            Tab::Resources => "Ресурсы",
            Tab::Details => "Сведения",
            Tab::Events => "События",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }
}

/// Строка дерева.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Root(usize),
    Group(usize, usize),
    Device(usize),
}

struct Plan {
    toolbar: Rect,
    refresh: Rect,
    by_type: Rect,
    by_bus: Rect,
    search: Rect,
    left: Rect,
    chips: Vec<Rect>,
    tree: Rect,
    right: Rect,
    tabs: Vec<Rect>,
    detail: Rect,
    drawer: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect, drawer_open: bool) -> Self {
        let status = Rect::new(area.x, area.bottom() - ctx.px(STATUS_H) as i32, area.w, ctx.px(STATUS_H));
        let drawer_h = if drawer_open { ctx.px(DRAWER_H) } else { ctx.px(DRAWER_HEAD_H) };
        let drawer = Rect::new(area.x, status.y - drawer_h as i32, area.w, drawer_h);
        let toolbar = Rect::new(area.x, area.y, area.w, ctx.px(TOOLBAR_H));
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = toolbar.y + (toolbar.h as i32 - bh as i32) / 2;
        let refresh = Rect::new(toolbar.x + pad, by, ctx.face(Role::Body).width("Обновить") + ctx.px(46), bh);
        let seg_w = ctx.face(Role::Body).width("По шине") + ctx.px(28);
        let by_type = Rect::new(refresh.right() + ctx.px(14) as i32, by, ctx.face(Role::Body).width("По типу") + ctx.px(28), bh);
        let by_bus = Rect::new(by_type.right(), by, seg_w, bh);
        let search_w = ctx.px(230).min(toolbar.w / 3);
        let search = Rect::new(toolbar.right() - pad - search_w as i32, by, search_w, bh);

        let body_top = toolbar.bottom();
        let body_h = (drawer.y - body_top).max(0) as u32;
        let left_w = (area.w * 2 / 5).clamp(ctx.px(260).min(area.w / 2), ctx.px(400));
        let left = Rect::new(area.x, body_top, left_w, body_h);
        let mut chips = Vec::new();
        let mut cx = left.x + pad;
        let mut cy = left.y + ctx.px(10) as i32;
        for filter in Filter::ALL {
            let w = ctx.face(Role::Caption).width(filter.title()) + ctx.px(48);
            if cx + w as i32 > left.right() - pad {
                cx = left.x + pad;
                cy += ctx.px(32) as i32;
            }
            chips.push(Rect::new(cx, cy, w, ctx.px(26)));
            cx += (w + ctx.px(6)) as i32;
        }
        let chips_bottom = cy + ctx.px(CHIPS_H - 10) as i32;
        let tree = Rect::new(left.x, chips_bottom, left.w, (left.bottom() - chips_bottom).max(0) as u32);
        let right = Rect::new(left.right(), body_top, area.w.saturating_sub(left_w), body_h);
        let tabs_y = right.y + ctx.px(84) as i32;
        let mut tabs = Vec::new();
        let mut tx = right.x + ctx.px(20) as i32;
        for tab in Tab::ALL {
            let w = ctx.face(Role::Body).width(tab.title()) + ctx.px(28);
            tabs.push(Rect::new(tx, tabs_y, w, ctx.px(TABS_H)));
            tx += w as i32;
        }
        let detail_y = tabs_y + ctx.px(TABS_H) as i32;
        let detail = Rect::new(right.x, detail_y, right.w, (right.bottom() - detail_y).max(0) as u32);
        Self { toolbar, refresh, by_type, by_bus, search, left, chips, tree, right, tabs, detail, drawer, status }
    }
}

struct Manager {
    devices: Vec<Device>,
    view: View,
    filter: Filter,
    query: String,
    searching: bool,
    collapsed: Vec<(View, usize)>,
    /// Выбранное устройство — номер в `devices`.
    selected: usize,
    tab: Tab,
    drawer: bool,
    log: Result<Vec<String>, String>,
    events: Option<(usize, Result<Vec<String>, String>)>,
    last_log: String,
    logged_selection: bool,
    hostname: String,
}

impl Manager {
    fn new(screen_h: u32) -> Self {
        let mut manager = Self {
            devices: Vec::new(),
            view: View::ByType,
            filter: Filter::All,
            query: String::new(),
            searching: false,
            collapsed: Vec::new(),
            selected: 0,
            tab: Tab::General,
            // На экране ниже тысячи точек окно — две трети, около 530, и
            // открытый журнал оставил бы дереву пять строк: он свёрнут, пока
            // его не попросят (J или щелчок по заголовку).
            drawer: screen_h >= 1000,
            log: Ok(Vec::new()),
            events: None,
            last_log: String::new(),
            logged_selection: false,
            hostname: String::from("freeos"),
        };
        manager.refresh();
        manager.read_log();
        manager
    }

    fn refresh(&mut self) {
        let previous = self.devices.get(self.selected).map(|device| (device.bus, device.place.clone()));
        self.devices = list_devices();
        // Выбор держится за устройство, а не за номер строки: перепись
        // перечитывается каждые три секунды, и строка могла переехать.
        self.selected = previous
            .and_then(|(bus, place)| self.devices.iter().position(|device| device.bus == bus && device.place == place))
            .or_else(|| self.order().first().copied())
            .unwrap_or(0);

        let mut log = String::new();
        for device in self.devices.iter().filter(|device| device.bus != Bus::Sys) {
            log.push_str(&device.log_line());
            log.push('\n');
        }
        let count = |bus: Bus| self.devices.iter().filter(|device| device.bus == bus).count();
        let missing = self.devices.iter().filter(|device| device.state == State::Missing).count();
        log.push_str(&format!(
            "devmgr: {} device(s): {} pci, {} usb, {} disk(s); {missing} without a driver",
            self.devices.len() - count(Bus::Sys),
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

    fn read_log(&mut self) {
        self.log = kernel_log("").map(|lines| {
            let skip = lines.len().saturating_sub(200);
            lines.into_iter().skip(skip).collect()
        });
    }

    fn passes(&self, device: &Device) -> bool {
        if !self.filter.passes(device) {
            return false;
        }
        if self.query.is_empty() {
            return true;
        }
        let query = self.query.to_lowercase();
        [display_name(device), device.what.clone(), device.id.clone(), device.driver.clone().unwrap_or_default()]
            .iter()
            .any(|text| text.to_lowercase().contains(&query))
    }

    /// Группы текущего вида: заголовок и номера устройств.
    fn groups(&self) -> Vec<(usize, Vec<usize>)> {
        let mut out = Vec::new();
        match self.view {
            View::ByType => {
                for (index, kind) in Kind::ALL.into_iter().enumerate() {
                    let members: Vec<usize> = (0..self.devices.len())
                        .filter(|at| self.devices[*at].kind() == kind && self.passes(&self.devices[*at]))
                        .collect();
                    if !members.is_empty() {
                        out.push((index, members));
                    }
                }
            }
            View::ByBus => {
                for (index, bus) in Bus::ALL.into_iter().enumerate() {
                    let members: Vec<usize> = (0..self.devices.len())
                        .filter(|at| self.devices[*at].bus == bus && self.passes(&self.devices[*at]))
                        .collect();
                    if !members.is_empty() {
                        out.push((index, members));
                    }
                }
            }
        }
        out
    }

    fn rows(&self) -> Vec<Row> {
        let groups = self.groups();
        let total = groups.iter().map(|(_, members)| members.len()).sum();
        let mut out = vec![Row::Root(total)];
        for (group, members) in groups {
            out.push(Row::Group(group, members.len()));
            if !self.collapsed.contains(&(self.view, group)) {
                out.extend(members.into_iter().map(Row::Device));
            }
        }
        out
    }

    /// Устройства в порядке дерева — по ним ходят стрелки.
    fn order(&self) -> Vec<usize> {
        self.rows()
            .into_iter()
            .filter_map(|row| match row {
                Row::Device(index) => Some(index),
                _ => None,
            })
            .collect()
    }

    fn select(&mut self, index: usize) {
        if index == self.selected || index >= self.devices.len() {
            return;
        }
        self.selected = index;
        self.events = None;
        // Печатается **первый** выбор и только он: стенду нужна одна строка,
        // а человеку строка на каждое нажатие стрелки заливает терминал.
        if !self.logged_selection {
            if let Some(device) = self.devices.get(index) {
                println(&format!("devmgr: selected {} {} '{}'", device.bus.tag(), device.place, device.what));
            }
            self.logged_selection = true;
        }
        if self.tab == Tab::Events {
            self.read_events();
        }
    }

    fn step(&mut self, delta: isize) {
        let order = self.order();
        if order.is_empty() {
            return;
        }
        let at = order.iter().position(|index| *index == self.selected).unwrap_or(0);
        let next = (at as isize + delta).clamp(0, order.len() as isize - 1) as usize;
        self.select(order[next]);
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            if tab == Tab::Events {
                self.read_events();
            }
        }
    }

    /// Строки журнала о выбранном устройстве: по имени драйвера, а у
    /// устройства без драйвера — по месту на шине.
    fn read_events(&mut self) {
        let Some(device) = self.devices.get(self.selected) else {
            return;
        };
        let word = device.driver.clone().unwrap_or_else(|| device.place.clone());
        let word = word.split_whitespace().next().unwrap_or("").to_string();
        self.events = Some((self.selected, kernel_log(&word)));
    }

    fn first_row(&self, rows: &[Row], shown: usize) -> usize {
        let at = rows.iter().position(|row| *row == Row::Device(self.selected)).unwrap_or(0);
        kit::first_visible(at, shown.max(1))
    }

    fn group_title(&self, group: usize) -> (&'static str, Fi) {
        match self.view {
            View::ByType => {
                let kind = Kind::ALL[group];
                (kind.title(), kind.icon())
            }
            View::ByBus => {
                let bus = Bus::ALL[group];
                (bus.title(), bus.icon())
            }
        }
    }

    // -----------------------------------------------------------------------
    // Отрисовка
    // -----------------------------------------------------------------------

    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить");
        // Переключатель вида — две половины одной пилюли.
        let both = Rect::new(plan.by_type.x, plan.by_type.y, plan.by_type.w + plan.by_bus.w, plan.by_type.h);
        paint::sunk(bar, s, both, ctx.px(6));
        for (rect, label, on) in [(plan.by_type, "По типу", self.view == View::ByType), (plan.by_bus, "По шине", self.view == View::ByBus)] {
            if on {
                let inner = Rect::new(rect.x + 3, rect.y + 3, rect.w - 6, rect.h - 6);
                draw::rounded(s, inner, ctx.px(5), bar.flat(p.btn), 255);
                draw::rounded_stroke(s, inner, ctx.px(5), p.accedge, 255);
            }
            paint::text_centered(bar, s, Role::Body, rect, label, if on { p.ink } else { p.ink3 });
        }
        let field = plan.search;
        paint::sunk(bar, s, field, ctx.px(6));
        if self.searching {
            draw::rounded_stroke(s, field, ctx.px(6), p.acc, 255);
        }
        let g = ctx.px(16);
        iconset::draw(s, Fi::Search, field.x + ctx.px(10) as i32, field.y + (field.h - g) as i32 / 2, g, p.ink4, 255);
        let tx = field.x + ctx.px(34) as i32;
        let room = (field.right() - tx - ctx.px(8) as i32).max(0) as u32;
        if self.query.is_empty() && !self.searching {
            paint::text_clipped(bar, s, Role::Body, tx, paint::baseline(ctx, Role::Body, field), room, "Поиск устройства", p.ink5);
        } else {
            paint::text_clipped(bar, s, Role::Body, tx, paint::baseline(ctx, Role::Body, field), room, &self.query, p.ink);
            if self.searching {
                let w = ctx.face(Role::Body).width(&self.query).min(room);
                s.fill(Rect::new(tx + w as i32 + 1, field.y + ctx.px(8) as i32, ctx.px(2), field.h.saturating_sub(ctx.px(16))), p.acc);
            }
        }
    }

    fn draw_left(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        draw::vline(s, plan.left.right() - 1, plan.left.y, plan.left.h, p.line.color, p.line.alpha);
        // ── Пилюли-фильтры ──────────────────────────────────────────────────
        for (filter, rect) in Filter::ALL.into_iter().zip(&plan.chips) {
            let count = self.devices.iter().filter(|device| filter.passes(device)).count();
            let on = filter == self.filter;
            let r = rect.h / 2;
            if on {
                draw::rounded(s, *rect, r, ctx.flat(p.acctint), 255);
                draw::rounded_stroke(s, *rect, r, p.accedge, 255);
            } else {
                draw::rounded_stroke(s, *rect, r, p.line3.color, p.line3.alpha);
            }
            let dot = match filter {
                Filter::All => p.ink4,
                Filter::Working => p.ok,
                Filter::Attention => p.warn,
                Filter::Quiet => p.ink5,
            };
            draw::circle(s, rect.x + ctx.px(12) as i32, rect.y + rect.h as i32 / 2, ctx.px(3), dot, 255);
            let tx = rect.x + ctx.px(22) as i32;
            let w = paint::text(ctx, s, Role::Caption, tx, paint::baseline(ctx, Role::Caption, *rect), filter.title(), if on { p.ink } else { p.ink2 });
            paint::text(ctx, s, Role::MonoSmall, tx + w as i32 + ctx.px(6) as i32, paint::baseline(ctx, Role::MonoSmall, *rect), &format!("{count}"), p.ink4);
        }
        draw::hline(s, plan.tree.x, plan.tree.y - 1, plan.tree.w.saturating_sub(1), p.line.color, p.line.alpha);

        // ── Дерево ──────────────────────────────────────────────────────────
        let rows = self.rows();
        let row_h = ctx.px(TREE_ROW_H);
        let shown = (plan.tree.h.saturating_sub(ctx.px(8)) / row_h) as usize;
        let first = self.first_row(&rows, shown);
        let g = ctx.px(18);
        for (offset, row) in rows.iter().skip(first).take(shown).enumerate() {
            let rect = Rect::new(plan.tree.x + ctx.px(8) as i32, plan.tree.y + ctx.px(6) as i32 + (offset as u32 * row_h) as i32, plan.tree.w.saturating_sub(ctx.px(16)), row_h);
            let mid = rect.y + (rect.h - g) as i32 / 2;
            match *row {
                Row::Root(count) => {
                    let x = rect.x + ctx.px(6) as i32;
                    iconset::draw(s, Fi::Laptop, x, mid, g, p.ink2, 255);
                    let w = paint::text(ctx, s, Role::Strong, x + (g + ctx.px(10)) as i32, paint::baseline(ctx, Role::Strong, rect), &self.hostname, p.ink);
                    paint::text(ctx, s, Role::Body, x + (g + ctx.px(18)) as i32 + w as i32, paint::baseline(ctx, Role::Body, rect), &format!("({count})"), p.ink4);
                }
                Row::Group(group, count) => {
                    let collapsed = self.collapsed.contains(&(self.view, group));
                    let x = rect.x + ctx.px(14) as i32;
                    let chevron = if collapsed { Fi::ChevronRight } else { Fi::ChevronDown };
                    iconset::draw(s, chevron, x, rect.y + (rect.h - ctx.px(14)) as i32 / 2, ctx.px(14), p.ink4, 255);
                    let (title, icon) = self.group_title(group);
                    iconset::draw(s, icon, x + ctx.px(22) as i32, mid, g, p.ink2, 255);
                    let tx = x + ctx.px(22) as i32 + (g + ctx.px(8)) as i32;
                    paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), (rect.right() - tx - ctx.px(36) as i32).max(0) as u32, title, p.ink);
                    paint::text_right(ctx, s, Role::MonoSmall, rect.right() - ctx.px(8) as i32, paint::baseline(ctx, Role::MonoSmall, rect), &format!("{count}"), p.ink4);
                }
                Row::Device(index) => {
                    let device = &self.devices[index];
                    let selected = index == self.selected;
                    if selected {
                        draw::rounded(s, rect, ctx.px(5), ctx.flat(p.acctint), 255);
                    }
                    let x = rect.x + ctx.px(36) as i32;
                    let warn = device.state == State::Missing;
                    let tint = if warn { p.warn } else { p.ink2 };
                    iconset::draw(s, device.icon(), x, mid, g, tint, 255);
                    let tx = x + (g + ctx.px(8)) as i32;
                    let room = (rect.right() - tx - ctx.px(30) as i32).max(0) as u32;
                    paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), room, &display_name(device), if warn { p.warn } else { p.ink });
                    if warn {
                        iconset::draw(s, Fi::Warning, rect.right() - ctx.px(24) as i32, mid, g, p.warn, 255);
                    }
                }
            }
        }
        if rows.len() <= 1 {
            let rect = Rect::new(plan.tree.x + ctx.px(20) as i32, plan.tree.y + ctx.px(6) as i32 + row_h as i32, plan.tree.w, row_h);
            let text = if self.devices.is_empty() { "Перепись пуста — ядро не ответило" } else { "Ничего не подходит под фильтр" };
            paint::text(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, rect), text, p.ink4);
        }
    }

    fn draw_right(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let Some(device) = self.devices.get(self.selected) else {
            return;
        };
        let pad = ctx.px(20) as i32;
        // ── Карточка ────────────────────────────────────────────────────────
        let tile = Rect::new(plan.right.x + pad, plan.right.y + ctx.px(18) as i32, ctx.px(48), ctx.px(48));
        draw::rounded(s, tile, ctx.px(10), ctx.flat(p.btn), 255);
        draw::rounded_stroke(s, tile, ctx.px(10), p.btnline.color, p.btnline.alpha);
        let g = ctx.px(26);
        iconset::draw(s, device.icon(), tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, p.ink, 255);
        let pill_label = device.state.title();
        let pill_w = ctx.face(Role::Caption).width(pill_label) + ctx.px(34);
        let pill = Rect::new(plan.right.right() - pad - pill_w as i32, tile.y + ctx.px(10) as i32, pill_w, ctx.px(26));
        let (fill, edge, ink) = match device.state.tone() {
            Tone::Ok => (p.okbg, p.okline, p.ok_ink),
            Tone::Warn => (p.warnbg, p.warnline, p.warn),
            Tone::Accent => (p.acctint, p.accedge, p.acc_ink),
            _ => (p.ghost, p.line3.color, p.ink3),
        };
        draw::rounded(s, pill, pill.h / 2, ctx.flat(fill), 255);
        draw::rounded_stroke(s, pill, pill.h / 2, edge, 255);
        draw::circle(s, pill.x + ctx.px(13) as i32, pill.y + pill.h as i32 / 2, ctx.px(3), ink, 255);
        paint::text(ctx, s, Role::Caption, pill.x + ctx.px(22) as i32, paint::baseline(ctx, Role::Caption, pill), pill_label, ink);
        let tx = tile.right() + ctx.px(16) as i32;
        let room = (pill.x - tx - ctx.px(12) as i32).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Heading, tx, tile.y, room, &display_name(device), p.ink);
        paint::text_clipped(ctx, s, Role::Mono, tx, tile.y + line_h(ctx, Role::Heading) + ctx.px(2) as i32, room, &device.location().replace(" · ", "  ·  "), p.ink4);

        // ── Вкладки ─────────────────────────────────────────────────────────
        let line_y = plan.tabs.first().map_or(plan.detail.y, |rect| rect.bottom()) - 1;
        draw::hline(s, plan.right.x, line_y, plan.right.w, p.line.color, p.line.alpha);
        for (tab, rect) in Tab::ALL.into_iter().zip(&plan.tabs) {
            let on = tab == self.tab;
            paint::text_centered(ctx, s, Role::Body, *rect, tab.title(), if on { p.ink } else { p.ink3 });
            if on {
                s.fill(Rect::new(rect.x + ctx.px(6) as i32, rect.bottom() - ctx.px(2) as i32, rect.w.saturating_sub(ctx.px(12)), ctx.px(2)), p.acc);
            }
        }

        // ── Содержимое вкладки ──────────────────────────────────────────────
        let area = Rect::new(plan.detail.x + pad, plan.detail.y + ctx.px(18) as i32, plan.detail.w.saturating_sub(pad as u32 * 2), plan.detail.h.saturating_sub(ctx.px(18)));
        match self.tab {
            Tab::General => {
                let (text, tone) = match device.state {
                    State::Active => (String::from("Устройство работает нормально."), Tone::Ok),
                    State::Idle => (format!("Драйвер {} есть в ядре, но этим устройством не занят.", device.driver.as_deref().unwrap_or("—")), Tone::Accent),
                    State::Missing => (String::from("Драйвера для этого устройства в системе нет."), Tone::Warn),
                    State::NotNeeded => (String::from("Устройство настраивает прошивка — драйвер не нужен."), Tone::Muted),
                };
                let mut y = banner(ctx, s, area, &text, tone);
                let kind = device.kind().title();
                let facts = [
                    ("Тип", kind.to_string()),
                    ("Шина", device.bus.long().to_string()),
                    ("Расположение", device.location()),
                    ("Источник", String::from("FossAi")),
                    ("Состояние", device.state.title().to_string()),
                ];
                y = facts_table(ctx, s, area, y, &facts);
                let _ = y;
            }
            Tab::Driver => {
                let facts: Vec<(&str, String)> = match &device.driver {
                    Some(driver) => vec![
                        ("Драйвер", driver.clone()),
                        ("Где живёт", String::from("Встроен в ядро FossAi")),
                        ("Состояние", device.state.title().to_string()),
                        ("Выгрузка", String::from("Нельзя: драйверы ядра не выгружаются")),
                    ],
                    None if device.state == State::NotNeeded => vec![("Драйвер", String::from("Не нужен")), ("Почему", String::from("Устройство настроила прошивка"))],
                    None => vec![
                        ("Драйвер", String::from("Нет")),
                        ("Поиск", String::from("Служба drvd спрашивает его в репозитории")),
                        ("Идентификатор", device.id.clone()),
                    ],
                };
                facts_table(ctx, s, area, area.y, &facts);
            }
            Tab::Resources => {
                let mut facts: Vec<(&str, String)> = Vec::new();
                match device.bus {
                    Bus::Pci => {
                        facts.push(("Прерывание", device.signal.clone().unwrap_or_else(|| String::from("—"))));
                        facts.push(("Адрес на шине", device.place.clone()));
                    }
                    Bus::Usb => facts.push(("Порт", device.place.clone())),
                    Bus::Disk => facts.push(("Объём", device.id.clone())),
                    Bus::Sys => facts.push(("Сведения", device.id.clone())),
                }
                facts_table(ctx, s, area, area.y, &facts);
            }
            Tab::Details => {
                let facts = [
                    ("Идентификатор", device.id.clone()),
                    ("Класс", device.what.clone()),
                    ("Шина", device.bus.tag().to_string()),
                    ("Место", device.place.clone()),
                    ("Строка переписи", device.log_line().trim_start_matches("devmgr: ").to_string()),
                ];
                facts_table(ctx, s, area, area.y, &facts);
            }
            Tab::Events => match &self.events {
                Some((_, Ok(lines))) if lines.is_empty() => {
                    paint::text(ctx, s, Role::Body, area.x, area.y, "В журнале ядра о нём ничего нет.", p.ink4);
                }
                Some((_, Ok(lines))) => log_lines(ctx, s, area, lines, lines.len().saturating_sub(1)),
                Some((_, Err(error))) => {
                    paint::text_clipped(ctx, s, Role::Body, area.x, area.y, area.w, error, p.ink3);
                }
                None => {}
            },
        }
    }

    fn draw_drawer(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        s.fill(plan.drawer, fill);
        draw::hline(s, plan.drawer.x, plan.drawer.y, plan.drawer.w, p.line.color, p.line.alpha);
        let head = Rect::new(plan.drawer.x, plan.drawer.y, plan.drawer.w, ctx.px(DRAWER_HEAD_H));
        let g = ctx.px(14);
        iconset::draw(s, if self.drawer { Fi::ChevronDown } else { Fi::ChevronUp }, head.x + ctx.px(14) as i32, head.y + (head.h - g) as i32 / 2, g, p.ink3, 255);
        let tx = head.x + ctx.px(38) as i32;
        let w = paint::text(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, head), "Журнал ядра", p.ink);
        let count = self.log.as_ref().map_or(0, Vec::len);
        paint::text(ctx, s, Role::MonoSmall, tx + w as i32 + ctx.px(10) as i32, paint::baseline(ctx, Role::MonoSmall, head), &format!("{count}"), p.ink4);
        if !self.drawer {
            return;
        }
        let area = Rect::new(plan.drawer.x + ctx.px(14) as i32, head.bottom() + ctx.px(2) as i32, plan.drawer.w.saturating_sub(ctx.px(28)), plan.drawer.h.saturating_sub(head.h + ctx.px(6)));
        match &self.log {
            Ok(lines) => log_lines(ctx.on(fill), s, area, lines, lines.len().saturating_sub(1)),
            Err(error) => {
                paint::text_clipped(ctx, s, Role::Caption, area.x, area.y + ctx.px(4) as i32, area.w, error, p.ink4);
            }
        }
    }
}

impl App for Manager {
    fn key(&mut self, code: u32) -> bool {
        if self.searching {
            match code {
                ESCAPE => {
                    self.searching = false;
                    self.query.clear();
                }
                ENTER => self.searching = false,
                BACKSPACE => {
                    self.query.pop();
                }
                WIN_KEY_UP => self.step(-1),
                WIN_KEY_DOWN => self.step(1),
                code if code < 0x0100_0000 => {
                    if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                        if self.query.len() < 32 {
                            self.query.push(ch);
                        }
                    }
                }
                _ => return false,
            }
            if let Some(first) = self.order().first().copied() {
                if !self.order().contains(&self.selected) {
                    self.select(first);
                }
            }
            return true;
        }
        match code {
            WIN_KEY_UP => self.step(-1),
            WIN_KEY_DOWN => self.step(1),
            WIN_KEY_PAGE_UP => self.step(-10),
            WIN_KEY_PAGE_DOWN => self.step(10),
            WIN_KEY_HOME => self.step(isize::MIN / 2),
            WIN_KEY_END => self.step(isize::MAX / 2),
            WIN_KEY_LEFT => self.set_tab(Tab::ALL[self.tab.index().saturating_sub(1)]),
            WIN_KEY_RIGHT | TAB => self.set_tab(Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()]),
            REFRESH => {
                self.refresh();
                self.read_log();
                if self.tab == Tab::Events {
                    self.read_events();
                }
            }
            VIEW => self.view = if self.view == View::ByType { View::ByBus } else { View::ByType },
            LOG => self.drawer = !self.drawer,
            FIND => self.searching = true,
            ESCAPE => {
                self.query.clear();
                self.filter = Filter::All;
            }
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area, self.drawer);
        self.searching = false;
        if plan.refresh.contains(x, y) {
            self.refresh();
            self.read_log();
            return true;
        }
        if plan.by_type.contains(x, y) {
            self.view = View::ByType;
            return true;
        }
        if plan.by_bus.contains(x, y) {
            self.view = View::ByBus;
            return true;
        }
        if plan.search.contains(x, y) {
            self.searching = true;
            return true;
        }
        for (filter, rect) in Filter::ALL.into_iter().zip(&plan.chips) {
            if rect.contains(x, y) {
                self.filter = filter;
                if let Some(first) = self.order().first().copied() {
                    if !self.order().contains(&self.selected) {
                        self.select(first);
                    }
                }
                return true;
            }
        }
        for (tab, rect) in Tab::ALL.into_iter().zip(&plan.tabs) {
            if rect.contains(x, y) {
                self.set_tab(tab);
                return true;
            }
        }
        let head = Rect::new(plan.drawer.x, plan.drawer.y, plan.drawer.w, ctx.px(DRAWER_HEAD_H));
        if head.contains(x, y) {
            self.drawer = !self.drawer;
            return true;
        }
        if plan.tree.contains(x, y) {
            let rows = self.rows();
            let row_h = ctx.px(TREE_ROW_H);
            let shown = (plan.tree.h.saturating_sub(ctx.px(8)) / row_h) as usize;
            let first = self.first_row(&rows, shown);
            let offset = ((y - plan.tree.y - ctx.px(6) as i32).max(0) as u32 / row_h) as usize;
            match rows.get(first + offset) {
                Some(Row::Device(index)) => self.select(*index),
                Some(Row::Group(group, _)) => {
                    let key = (self.view, *group);
                    if let Some(at) = self.collapsed.iter().position(|other| *other == key) {
                        self.collapsed.remove(at);
                    } else {
                        self.collapsed.push(key);
                    }
                }
                _ => return false,
            }
            return true;
        }
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        self.step(if clicks > 0 { -3 } else { 3 });
        true
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        self.refresh();
        true
    }

    fn quits_on_q(&self) -> bool {
        !self.searching
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, mini_ui::theme::window_bg());
        let plan = Plan::new(ctx, area, self.drawer);
        self.draw_toolbar(s, ctx, &plan);
        self.draw_left(s, ctx, &plan);
        self.draw_right(s, ctx, &plan);
        self.draw_drawer(s, ctx, &plan);

        let total = self.devices.len();
        let attention = self.devices.iter().filter(|device| matches!(device.state, State::Missing)).count();
        let left = format!("{total} {} · без драйвера: {attention}", devices_word(total));
        kit::status_bar(ctx, s, plan.status, &left);
        let right = match self.devices.get(self.selected) {
            Some(device) => format!("{} · {}", device.id, device.location()),
            None => String::from("R — обновить    V — вид    / — поиск"),
        };
        paint::text_right(ctx, s, Role::Mono, plan.status.right() - ctx.px(14) as i32, paint::baseline(ctx, Role::Mono, plan.status), &right, p.ink4);
    }
}

// ---------------------------------------------------------------------------
// Кирпичи
// ---------------------------------------------------------------------------

fn line_h(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Ghost, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, p.ink2, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink);
}

/// Плашка состояния — зелёная «работает нормально», жёлтая «нет драйвера».
fn banner(ctx: Ctx, s: &mut Surface, area: Rect, text: &str, tone: Tone) -> i32 {
    let p = ctx.palette;
    let rect = Rect::new(area.x, area.y, area.w, ctx.px(46));
    let (fill, edge, ink, icon) = match tone {
        Tone::Ok => (p.okbg, p.okline, p.ok_ink, Fi::Check),
        Tone::Warn => (p.warnbg, p.warnline, p.warn, Fi::Warning),
        Tone::Accent => (p.acctint, p.accedge, p.acc_ink, Fi::Info),
        _ => (p.ghost, p.line3.color, p.ink3, Fi::Info),
    };
    draw::rounded(s, rect, ctx.px(8), ctx.flat(fill), 255);
    draw::rounded_stroke(s, rect, ctx.px(8), edge, 255);
    let g = ctx.px(18);
    iconset::draw(s, icon, rect.x + ctx.px(16) as i32, rect.y + (rect.h - g) as i32 / 2, g, ink, 255);
    let tx = rect.x + ctx.px(46) as i32;
    paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), (rect.right() - tx - ctx.px(10) as i32).max(0) as u32, text, p.ink);
    rect.bottom() + ctx.px(20) as i32
}

/// Пары «подпись — значение» в две колонки.
fn facts_table(ctx: Ctx, s: &mut Surface, area: Rect, y: i32, facts: &[(&str, String)]) -> i32 {
    let p = ctx.palette;
    let label_w = (area.w * 2 / 5).min(ctx.px(230));
    let step = line_h(ctx, Role::Body) + ctx.px(10) as i32;
    let mut y = y;
    for (label, value) in facts {
        if y + step > area.bottom() {
            break;
        }
        paint::text_clipped(ctx, s, Role::Body, area.x, y, label_w, label, p.ink3);
        paint::text_clipped(ctx, s, Role::Body, area.x + label_w as i32, y, area.w.saturating_sub(label_w), value, p.ink);
        y += step;
    }
    y
}

/// Строки журнала ядра: время, тег цветом акцента, текст. Последняя видна.
fn log_lines(ctx: Ctx, s: &mut Surface, area: Rect, lines: &[String], last: usize) {
    let p = ctx.palette;
    let step = line_h(ctx, Role::Mono) + ctx.px(3) as i32;
    let shown = (area.h as i32 / step).max(1) as usize;
    let first = (last + 1).saturating_sub(shown);
    for (offset, line) in lines.iter().skip(first).take(shown).enumerate() {
        let y = area.y + offset as i32 * step;
        let trimmed = line.trim_start();
        let (tag, rest) = match trimmed.split_once(" : ") {
            Some((tag, rest)) if tag.len() <= 14 => (tag.trim_end(), rest),
            _ => ("", trimmed),
        };
        if tag.is_empty() {
            paint::text_clipped(ctx, s, Role::Mono, area.x, y, area.w, rest, p.ink3);
        } else {
            let tag_w = ctx.px(110).min(area.w / 3);
            paint::text_clipped(ctx, s, Role::Mono, area.x, y, tag_w, tag, p.acc_ink);
            paint::text_clipped(ctx, s, Role::Mono, area.x + tag_w as i32, y, area.w.saturating_sub(tag_w), &format!(": {rest}"), p.ink2);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::two_thirds(&info);
    let screen_h = info.screen_h;
    app::run(&SPEC, &info, size, move |_, _| Manager::new(screen_h))
}
