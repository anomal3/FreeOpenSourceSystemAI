//! Сервер Wayland в ядре (фаза 71i): программы Linux рисуют в окна стола.
//!
//! # Где живёт
//!
//! Не в отдельном процессе, а здесь, в слое Linux: программе Linux родные
//! вызовы окон недоступны, а мост «процесс-композитор ↔ ядро» стоил бы ещё
//! одного протокола. Ядро слушает AF_UNIX-имя [`SOCKET`] (встаёт по первому
//! `connect` к нему), соединения разбирает задача ядра `wayland`, каждое
//! окно верхнего уровня (`xdg_toplevel`) — обычное окно стола
//! (`ui::open_window`) с ключом `(pid клиента, свой номер)`.
//!
//! # Что умеет
//!
//! Ядро протокола (`wl_display`, `wl_registry`, `wl_callback`),
//! `wl_compositor` с поверхностями и областями, `wl_subcompositor`
//! (подповерхности сводятся в окно родителя), `wl_shm` (буферы — прямо из
//! кадров `memfd` клиента, [`SharedMem`]), `xdg_wm_base` с `xdg_toplevel`,
//! `wl_output`, `wl_seat` с указателем и клавиатурой. Окно рисуется строками
//! прямо из памяти клиента, без промежуточных копий (полноэкранное окно — 8
//! МиБ на копию, а куча ядра — 64 МиБ); буфер возвращается клиенту
//! (`wl_buffer.release`), когда его сменит следующий. Форматы — `ARGB8888` и
//! `XRGB8888`; альфа учитывается только у подповерхностей.
//!
//! # Клавиатура
//!
//! Окна сервера просят у стола коды клавиш (`ui::raw_keys_window`): каждое
//! нажатие и отпускание приходит кодом evdev, символы клиент считает сам по
//! раскладке XKB, которую получает от сервера (`wayland.xkb`, US, через
//! `memfd`). Модификаторы сервер считает по тем же кодам и сообщает
//! `wl_keyboard.modifiers`.
//!
//! # Всплывающие окна
//!
//! `xdg_popup` (меню, выпадающие списки, подсказки) — всплывающее окно стола
//! над окном верхнего уровня (`ui::open_popup`). Место считается по
//! `xdg_positioner`: точка прямоугольника привязки, направление, сдвиг, а у
//! края экрана — переворот и сдвиг внутрь, если клиент их разрешил. Щелчок
//! мимо меню, взявшего захват (`grab`), или уход фокуса закрывают меню
//! (`popup_done`), и сам щелчок до окна не доходит — как у любого сервера с
//! захватом.
//!
//! Чего нет: `dmabuf`, буфера обмена, масштаба кроме 1.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use mini_ui::{Rect, Surface, theme};

/// Фон окна там, где клиент ничего не нарисовал (`ARGB8888`).
const BACKDROP: u32 = 0xff20_2020;

use super::ipc::{KernelConn, KernelListener};
use crate::kprintln;
use crate::sync::Mutex;
use crate::user::files::Handle;
use crate::user::shm::SharedMem;

/// Имя сокета: `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` у программ Linux
/// (`XDG_RUNTIME_DIR=/run/user/0`, `WAYLAND_DISPLAY=wayland-0`).
pub const SOCKET: &[u8] = b"/run/user/0/wayland-0";

static STARTED: AtomicBool = AtomicBool::new(false);
static LISTENER: Mutex<Option<KernelListener>> = Mutex::new(None);

// ---- буфер обмена ---------------------------------------------------------------
//
// Выделение (`wl_data_device.set_selection`) — одно на всех клиентов: какой
// клиент им владеет, его `wl_data_source` и форматы. Текст сервер у себя не
// держит: клиенту, попросившему вставку (`wl_data_offer.receive`), владелец
// пишет его сам — в канал, который сервер ему передаёт (`wl_data_source.send`).
// Так устроено у любого сервера Wayland, и так Chromium и Rider (Java)
// обмениваются текстом (Роман, 27.10: «не могу скопировать из браузера и
// вставить в Rider»).

/// Чьё выделение и что в нём.
struct Selection {
    owner: u64,
    source: u32,
    mimes: Vec<String>,
}

static SELECTION: Mutex<Option<Selection>> = Mutex::new(None);
/// Растёт при каждой смене выделения: клиент, у которого своё число
/// отстало, получает новое предложение.
static SELECTION_GEN: AtomicU32 = AtomicU32::new(1);

/// Просьба к клиенту-владельцу, которую он выполнит на своём шаге.
enum ToOwner {
    /// `wl_data_source.send(mime, fd)`: написать данные в этот канал.
    Send { source: u32, mime: String, fd: Handle },
    /// `wl_data_source.cancelled`: выделение перешло к другому.
    Cancelled { source: u32 },
}

static TO_OWNER: Mutex<Vec<(u64, ToOwner)>> = Mutex::new(Vec::new());
/// Номер клиента для буфера обмена: у одного процесса бывает несколько
/// соединений, и номер процесса их не различает.
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// Печатать запросы клиентов и события сервера (`kernel wltrace on`).
static TRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Включить или выключить трассировку протокола.
pub fn set_trace(on: bool) {
    TRACE.store(on, Ordering::Relaxed);
}

/// Поднять сервер, если программа подключается к [`SOCKET`].
pub fn serve_if_named(name: &[u8]) {
    if name != SOCKET || STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let Some(listener) = KernelListener::bind(SOCKET) else {
        return;
    };
    *LISTENER.lock() = Some(listener);
    match crate::sched::spawn_daemon("wayland", run) {
        Ok(_) => kprintln!("  wayland     : serving {}", String::from_utf8_lossy(SOCKET)),
        Err(_) => {
            *LISTENER.lock() = None;
            STARTED.store(false, Ordering::Release);
        }
    }
}

/// Задача сервера: новые соединения, запросы, события окон — по кругу.
fn run() {
    let Some(listener) = LISTENER.lock().take() else {
        return;
    };
    let mut clients: Vec<Client> = Vec::new();
    loop {
        let seen = crate::io_events::generation();
        while let Some(conn) = listener.accept() {
            let client = Client::new(conn);
            kprintln!("  wayland     : client #{} connected", client.pid);
            clients.push(client);
        }
        let now = crate::time::uptime_ms();
        clients.retain_mut(|client| {
            let alive = client.step(now);
            if !alive {
                client.close_all();
                kprintln!("  wayland     : client #{} gone", client.pid);
            }
            alive
        });
        // События окон приходят без уведомления — опрос раз в кадр.
        crate::io_events::wait(seen, Some(now + 16));
    }
}

// ---- провод ---------------------------------------------------------------------

/// Сборка сообщения: заголовок `id`, `(размер << 16) | код`, аргументы.
struct Msg(Vec<u8>);

impl Msg {
    fn new(id: u32, opcode: u16) -> Self {
        let mut buf = Vec::with_capacity(32);
        buf.extend_from_slice(&id.to_ne_bytes());
        buf.extend_from_slice(&u32::from(opcode).to_ne_bytes());
        Self(buf)
    }
    fn u32(mut self, value: u32) -> Self {
        self.0.extend_from_slice(&value.to_ne_bytes());
        self
    }
    fn i32(self, value: i32) -> Self {
        self.u32(value as u32)
    }
    /// `wl_fixed_t`: 24.8.
    fn fixed(self, value: i32) -> Self {
        self.i32(value.saturating_mul(256))
    }
    fn bytes(mut self, data: &[u8]) -> Self {
        self.0.extend_from_slice(&(data.len() as u32).to_ne_bytes());
        self.0.extend_from_slice(data);
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
        self
    }
    fn str(self, text: &str) -> Self {
        let mut data = Vec::with_capacity(text.len() + 1);
        data.extend_from_slice(text.as_bytes());
        data.push(0);
        self.bytes(&data)
    }
    fn finish(mut self) -> Vec<u8> {
        let size = self.0.len() as u32;
        let word = u32::from_ne_bytes([self.0[4], self.0[5], self.0[6], self.0[7]]) | (size << 16);
        self.0[4..8].copy_from_slice(&word.to_ne_bytes());
        self.0
    }
}

/// Разбор аргументов запроса.
struct Args<'a> {
    data: &'a [u8],
    at: usize,
}

/// Запрос разобран не до конца — ошибка протокола.
struct Bad;

impl Args<'_> {
    fn u32(&mut self) -> Result<u32, Bad> {
        let word = self.data.get(self.at..self.at + 4).ok_or(Bad)?;
        self.at += 4;
        Ok(u32::from_ne_bytes([word[0], word[1], word[2], word[3]]))
    }
    fn i32(&mut self) -> Result<i32, Bad> {
        self.u32().map(|value| value as i32)
    }
    fn bytes(&mut self) -> Result<&[u8], Bad> {
        let len = self.u32()? as usize;
        let data = self.data.get(self.at..self.at + len).ok_or(Bad)?;
        self.at += len.div_ceil(4) * 4;
        Ok(data)
    }
    fn string(&mut self) -> Result<String, Bad> {
        let data = self.bytes()?;
        let text = data.strip_suffix(&[0]).unwrap_or(data);
        Ok(String::from_utf8_lossy(text).into_owned())
    }
}

// ---- объекты --------------------------------------------------------------------

/// Глобальные объекты: номер в реестре, интерфейс, версия.
const GLOBALS: [(u32, &str, u32); 8] = [
    (1, "wl_compositor", 4),
    (2, "wl_subcompositor", 1),
    (3, "wl_shm", 1),
    // xdg_wm_base 6 и wl_output 4 (12.11): `winewayland` GE-Proton не
    // привязывается к более старым — `bind` даже не уходит, и Wine остаётся без
    // окон («compositor doesn't support xdg_wm_base»). Новое в этих версиях
    // для сервера необязательно: события `configure_bounds`,
    // `wm_capabilities`, `name`, `description` можно не слать, а новые
    // запросы (`set_reactive`, `reposition`, `release`) принимаются молча.
    (4, "xdg_wm_base", 6),
    (5, "wl_seat", 5),
    (6, "wl_output", 4),
    // Буфер обмена и перетаскивание. Пока — объекты, принимающие запросы, и
    // ни одного предложения данных: Java (AWT на Wayland) без этого глобала не
    // заводит окно вовсе («Can't bind to the wl_data_device_manager»), а
    // настоящий обмен — следующий шаг.
    (7, "wl_data_device_manager", 3),
    // Размер поверхности, отличный от буфера. Последний из шести глобалов,
    // без которых AWT на Wayland не заводится (их список — строки «Can't bind
    // to the … interface» в `libawt_wlawt.so`). Масштаба экрана у нас нет —
    // окно приходит в размер буфера, и `set_destination` его не меняет.
    (8, "wp_viewporter", 1),
];

const SHM_ARGB8888: u32 = 0;
const SHM_XRGB8888: u32 = 1;
const SEAT_POINTER: u32 = 1;
const SEAT_KEYBOARD: u32 = 2;
/// Раскладка для `wl_keyboard.keymap` (формат `XKB_V1`): самодостаточная,
/// без `include` — у клиента может не быть каталога раскладок.
const KEYMAP: &str = include_str!("wayland.xkb");
const KEYMAP_XKB_V1: u32 = 1;
/// Маски модификаторов этой раскладки: `Shift`, `Lock`, `Control`, `Mod1`
/// (Alt), `Mod2` (NumLock), `Mod4` (Super).
const MOD_SHIFT: u32 = 1;
const MOD_LOCK: u32 = 2;
const MOD_CONTROL: u32 = 4;
const MOD_ALT: u32 = 8;
const MOD_NUM: u32 = 16;
const MOD_SUPER: u32 = 64;
const XDG_TOPLEVEL_MAXIMIZED: u32 = 1;
const XDG_TOPLEVEL_ACTIVATED: u32 = 4;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

#[derive(Clone)]
struct Buffer {
    mem: Arc<SharedMem>,
    offset: u64,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
}

#[derive(Default, Clone, Copy)]
struct Positioner {
    width: i32,
    height: i32,
    anchor: (i32, i32, i32, i32),
    /// Точка прямоугольника привязки (`xdg_positioner.anchor`).
    edge: u32,
    /// Куда окно растёт от этой точки (`xdg_positioner.gravity`).
    gravity: u32,
    /// Что можно сделать у края экрана (`constraint_adjustment`).
    adjust: u32,
    offset: (i32, i32),
}

/// `xdg_positioner.anchor` и `gravity`: 1 верх, 2 низ, 3 лево, 4 право,
/// 5–8 углы (верх-лево, низ-лево, верх-право, низ-право).
const fn is_top(edge: u32) -> bool {
    matches!(edge, 1 | 5 | 7)
}
const fn is_bottom(edge: u32) -> bool {
    matches!(edge, 2 | 6 | 8)
}
const fn is_left(edge: u32) -> bool {
    matches!(edge, 3 | 5 | 6)
}
const fn is_right(edge: u32) -> bool {
    matches!(edge, 4 | 7 | 8)
}

/// То же направление, отражённое по вертикали или по горизонтали.
const fn flip_vertical(edge: u32) -> u32 {
    match edge {
        1 => 2,
        2 => 1,
        5 => 6,
        6 => 5,
        7 => 8,
        8 => 7,
        other => other,
    }
}
const fn flip_horizontal(edge: u32) -> u32 {
    match edge {
        3 => 4,
        4 => 3,
        5 => 7,
        7 => 5,
        6 => 8,
        8 => 6,
        other => other,
    }
}

const ADJUST_SLIDE_X: u32 = 1;
const ADJUST_SLIDE_Y: u32 = 2;
const ADJUST_FLIP_X: u32 = 4;
const ADJUST_FLIP_Y: u32 = 8;

impl Positioner {
    /// Левый верхний угол окна от угла родителя — без учёта края экрана.
    fn place(&self) -> (i32, i32) {
        let (ax, ay, aw, ah) = self.anchor;
        let px = if is_left(self.edge) {
            ax
        } else if is_right(self.edge) {
            ax + aw
        } else {
            ax + aw / 2
        };
        let py = if is_top(self.edge) {
            ay
        } else if is_bottom(self.edge) {
            ay + ah
        } else {
            ay + ah / 2
        };
        let (w, h) = (self.width.max(1), self.height.max(1));
        let x = if is_left(self.gravity) {
            px - w
        } else if is_right(self.gravity) {
            px
        } else {
            px - w / 2
        };
        let y = if is_top(self.gravity) {
            py - h
        } else if is_bottom(self.gravity) {
            py
        } else {
            py - h / 2
        };
        (x + self.offset.0, y + self.offset.1)
    }

    /// Место с учётом экрана: `parent` — где на экране угол родителя,
    /// `screen` — размер экрана. Не влезающее переворачивается к другой
    /// стороне привязки, а если и так не влезает — сдвигается внутрь, если
    /// клиент это разрешил.
    fn constrain(&self, parent: (i32, i32), screen: (i32, i32)) -> (i32, i32) {
        let (w, h) = (self.width.max(1), self.height.max(1));
        let (mut x, mut y) = self.place();
        let out_x = |x: i32| parent.0 + x < 0 || parent.0 + x + w > screen.0;
        let out_y = |y: i32| parent.1 + y < 0 || parent.1 + y + h > screen.1;
        if self.adjust & ADJUST_FLIP_X != 0 && out_x(x) {
            let flipped = Self {
                edge: flip_horizontal(self.edge),
                gravity: flip_horizontal(self.gravity),
                offset: (-self.offset.0, self.offset.1),
                ..*self
            };
            let (fx, _) = flipped.place();
            if !out_x(fx) {
                x = fx;
            }
        }
        if self.adjust & ADJUST_FLIP_Y != 0 && out_y(y) {
            let flipped = Self {
                edge: flip_vertical(self.edge),
                gravity: flip_vertical(self.gravity),
                offset: (self.offset.0, -self.offset.1),
                ..*self
            };
            let (_, fy) = flipped.place();
            if !out_y(fy) {
                y = fy;
            }
        }
        if self.adjust & ADJUST_SLIDE_X != 0 && out_x(x) {
            x = (screen.0 - w).min(parent.0 + x).max(0) - parent.0;
        }
        if self.adjust & ADJUST_SLIDE_Y != 0 && out_y(y) {
            y = (screen.1 - h).min(parent.1 + y).max(0) - parent.1;
        }
        (x, y)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    None,
    /// Окно верхнего уровня: номер `xdg_surface`.
    Toplevel(u32),
    /// Всплывающее: номер `xdg_surface`.
    Popup(u32),
    /// Подповерхность: родитель, место в нём и лежит ли она **под** ним
    /// (`wl_subsurface.place_below` родителя). Порядок среди сестёр — порядок
    /// в `children` родителя.
    Sub { parent: u32, x: i32, y: i32, below: bool },
}

struct SurfaceState {
    /// `attach` до `commit`: `Some(None)` — снять буфер.
    pending: Option<Option<u32>>,
    frames: Vec<u32>,
    /// Показанный буфер: номер `wl_buffer` и его память. Копии нет — окно
    /// рисуется прямо из памяти клиента, а буфер возвращается ему
    /// (`release`), когда его сменит следующий (фаза 71i: копии полноэкранного
    /// окна исчерпывали кучу ядра).
    shown: Option<(u32, Buffer)>,
    role: Role,
    children: Vec<u32>,
    /// Что клиент объявил изменившимся (`damage`, `damage_buffer`) до
    /// `commit`, в точках поверхности.
    damage: Rect,
    /// Изменившееся с прошлой отрисовки окна: стол перерисовывает только это
    /// (до 24.10 окно Chromium копировалось целиком на каждый кадр, даже на
    /// мигание курсора).
    ready: Rect,
    /// Перерисовать целиком: новый буфер без объявленных изменений или
    /// другого размера.
    full: bool,
}

impl SurfaceState {
    fn new() -> Self {
        Self {
            pending: None,
            frames: Vec::new(),
            shown: None,
            role: Role::None,
            children: Vec::new(),
            damage: Rect::EMPTY,
            ready: Rect::EMPTY,
            full: true,
        }
    }
}

/// Окно стола за `xdg_toplevel`.
struct Win {
    surface: u32,
    xdg: u32,
    toplevel: u32,
    slot: u32,
    title: String,
    geometry: Option<(i32, i32, u32, u32)>,
    /// Размер окна на столе; `None` — ещё не открыто.
    open: Option<(u32, u32)>,
    configured: bool,
    /// Перерисовка не прошла (стол был занят) — повторить.
    dirty: bool,
    /// В окне есть прозрачные точки (тень и скругления, которые рисует
    /// программа) — так сказано столу (`ui::alpha_window`).
    see_through: bool,
    /// У окна фокус стола (состояние `activated` в `configure`).
    active: bool,
    /// Заголовок не дошёл до стола (он был занят) — повторить.
    title_due: bool,
}

/// Всплывающее окно стола за `xdg_popup`.
struct Pop {
    surface: u32,
    xdg: u32,
    popup: u32,
    slot: u32,
    /// Номер окна стола верхнего уровня, над которым оно всплывает.
    owner: u32,
    /// Место от угла окна верхнего уровня.
    at: (i32, i32),
    geometry: Option<(i32, i32, u32, u32)>,
    open: Option<(u32, u32)>,
    dirty: bool,
    /// Есть прозрачные точки (скруглённая подсказка JBR) — сказано столу.
    see_through: bool,
    /// Меню взяло захват: щелчок мимо закрывает его.
    grab: bool,
    /// Клиенту уже сказано `popup_done`.
    done: bool,
}

enum Obj {
    Display,
    Registry,
    Callback,
    Compositor,
    Subcompositor,
    Region,
    Shm,
    Pool(Arc<SharedMem>),
    Buffer(Buffer),
    Surface(SurfaceState),
    Subsurface(u32),
    WmBase,
    Positioner(Positioner),
    XdgSurface(u32),
    Toplevel,
    Popup,
    Seat,
    Pointer,
    Keyboard,
    Output,
    DataDeviceManager,
    /// Источник данных клиента и форматы, которые он предложил.
    DataSource(Vec<String>),
    DataDevice,
    /// Предложение выделения, которое сервер прислал клиенту.
    DataOffer,
    Viewporter,
    Viewport,
}

impl Obj {
    /// Имя интерфейса — для `wltrace`.
    fn name(&self) -> &'static str {
        match self {
            Self::Display => "wl_display",
            Self::Registry => "wl_registry",
            Self::Callback => "wl_callback",
            Self::Compositor => "wl_compositor",
            Self::Subcompositor => "wl_subcompositor",
            Self::Region => "wl_region",
            Self::Shm => "wl_shm",
            Self::Pool(_) => "wl_shm_pool",
            Self::Buffer(_) => "wl_buffer",
            Self::Surface(_) => "wl_surface",
            Self::Subsurface(_) => "wl_subsurface",
            Self::WmBase => "xdg_wm_base",
            Self::Positioner(_) => "xdg_positioner",
            Self::XdgSurface(_) => "xdg_surface",
            Self::Toplevel => "xdg_toplevel",
            Self::Popup => "xdg_popup",
            Self::Seat => "wl_seat",
            Self::Pointer => "wl_pointer",
            Self::Keyboard => "wl_keyboard",
            Self::Output => "wl_output",
            Self::DataDeviceManager => "wl_data_device_manager",
            Self::DataSource(_) => "wl_data_source",
            Self::DataDevice => "wl_data_device",
            Self::DataOffer => "wl_data_offer",
            Self::Viewporter => "wp_viewporter",
            Self::Viewport => "wp_viewport",
        }
    }
}

/// Номера окон стола у клиентов Wayland — отдельно от номеров родных окон
/// той же задачи.
const FIRST_SLOT: u32 = 0x1000;

struct Client {
    conn: KernelConn,
    pid: u32,
    input: Vec<u8>,
    fds: VecDeque<Handle>,
    out: Vec<u8>,
    out_fds: Vec<Handle>,
    objects: BTreeMap<u32, Obj>,
    wins: Vec<Win>,
    pops: Vec<Pop>,
    /// Нажатие, закрывшее меню, съедено — съесть и его отпускание.
    swallow_release: bool,
    next_slot: u32,
    serial: u32,
    /// `wl_callback` кадров, которые отвечают на следующем витке — после
    /// того как кадр показан.
    frames_due: Vec<u32>,
    /// Буферы, уже возвращённые клиенту (`wl_buffer.release`) — второй раз
    /// не отправлять, пока клиент не прикрепит их снова (13.11).
    released: Vec<u32>,
    pointer_on: Option<u32>,
    /// Где указатель на ней был в последнем `motion` — чтобы не слать
    /// движения, которого не было.
    pointer_at: Option<(i32, i32)>,
    /// Поверхность с фокусом клавиатуры.
    keyboard_on: Option<u32>,
    /// Нажатые модификаторы и включённые замки (маски `MOD_*`).
    mods_down: u32,
    mods_locked: u32,
    /// Группа раскладки, о которой знает клиент: 0 — английская, 1 —
    /// русская (`wayland.xkb`). Сверяется с языком системы на каждой клавише.
    group: u32,
    /// Номер клиента для буфера обмена ([`NEXT_KEY`]).
    key: u64,
    /// Какое поколение выделения клиент уже видел ([`SELECTION_GEN`]).
    selection_seen: u32,
    /// Следующий номер объекта, созданного сервером (`wl_data_offer`):
    /// сервер нумерует свои с `0xff000000`.
    next_server_id: u32,
    /// Нажатые клавиши (коды evdev): повтор ядра отличается от нажатия.
    keys_down: Vec<u32>,
    seat_version: u32,
    broken: bool,
}

impl Client {
    fn new(conn: KernelConn) -> Self {
        let pid = conn.peer_pid();
        let mut objects = BTreeMap::new();
        objects.insert(1, Obj::Display);
        Self {
            conn,
            pid,
            input: Vec::new(),
            fds: VecDeque::new(),
            out: Vec::new(),
            out_fds: Vec::new(),
            objects,
            wins: Vec::new(),
            pops: Vec::new(),
            swallow_release: false,
            next_slot: FIRST_SLOT,
            serial: 0,
            frames_due: Vec::new(),
            released: Vec::new(),
            pointer_on: None,
            pointer_at: None,
            keyboard_on: None,
            mods_down: 0,
            mods_locked: 0,
            group: 0,
            key: NEXT_KEY.fetch_add(1, Ordering::Relaxed),
            selection_seen: SELECTION_GEN.load(Ordering::Acquire),
            next_server_id: 0xff00_0000,
            keys_down: Vec::new(),
            seat_version: 1,
            broken: false,
        }
    }

    fn send(&mut self, msg: Msg) {
        let data = msg.finish();
        if TRACE.load(Ordering::Relaxed) && data.len() >= 8 {
            let id = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
            let opcode = u32::from_ne_bytes([data[4], data[5], data[6], data[7]]) & 0xffff;
            let words: Vec<u32> = data[8..].chunks_exact(4).take(4).map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]])).collect();
            let kind = self.objects.get(&id).map_or("?", Obj::name);
            kprintln!("  wltrace #{} <- {id} {kind}.{opcode} {words:?}", self.pid);
        }
        self.out.extend_from_slice(&data);
    }

    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    /// Ошибка протокола: сообщить и закрыть соединение.
    fn fail(&mut self, object: u32, code: u32, text: &str) {
        kprintln!("  wayland     : client #{} protocol error on object {object}: {text}", self.pid);
        self.send(Msg::new(1, 0).u32(object).u32(code).str(text));
        self.broken = true;
    }

    /// Убрать объект, который клиент уничтожил, и вернуть ему номер.
    fn delete(&mut self, id: u32) {
        self.objects.remove(&id);
        self.send(Msg::new(1, 1).u32(id));
    }

    /// Один виток: прочитать, разобрать, отправить. `false` — соединения нет.
    fn step(&mut self, now: u64) -> bool {
        let time = now as u32;
        self.answer_frames(time);
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match self.conn.recv(&mut buf) {
                Ok((0, rights)) if rights.is_empty() => return false,
                Ok((count, rights)) => {
                    self.input.extend_from_slice(&buf[..count]);
                    self.fds.extend(rights);
                }
                Err(_) => break,
            }
        }
        while !self.broken && self.input.len() >= 8 {
            let id = u32::from_ne_bytes([self.input[0], self.input[1], self.input[2], self.input[3]]);
            let word = u32::from_ne_bytes([self.input[4], self.input[5], self.input[6], self.input[7]]);
            let (size, opcode) = ((word >> 16) as usize, (word & 0xffff) as u16);
            if size < 8 {
                self.fail(id, 1, "bad message size");
                break;
            }
            if self.input.len() < size {
                break;
            }
            let body: Vec<u8> = self.input[8..size].to_vec();
            self.input.drain(..size);
            if TRACE.load(Ordering::Relaxed) {
                let words: Vec<u32> = body.chunks_exact(4).take(6).map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]])).collect();
                let kind = self.objects.get(&id).map_or("?", Obj::name);
                kprintln!("  wltrace #{} -> {id} {kind}.{opcode} {words:?}", self.pid);
            }
            if self.request(id, opcode, &body).is_err() {
                self.fail(id, 1, "malformed request");
            }
        }
        if !self.broken {
            self.sync_selection();
            self.serve_owner();
            self.window_events(time);
            self.popup_events(time);
            for index in 0..self.wins.len() {
                if self.wins[index].dirty {
                    self.present(index);
                }
                let win = &mut self.wins[index];
                if win.title_due && win.open.is_some() {
                    win.title_due = crate::ui::title_window(self.pid, win.slot, &win.title).is_err();
                }
            }
            for index in 0..self.pops.len() {
                if self.pops[index].dirty {
                    self.present_popup(index);
                }
            }
            // Кадры этого витка показаны — ответить сразу.
            self.answer_frames(time);
        }
        self.flush();
        !self.broken || !self.out.is_empty()
    }

    fn flush(&mut self) {
        while !self.out.is_empty() {
            let rights = core::mem::take(&mut self.out_fds);
            match self.conn.send(&self.out, rights) {
                Ok(sent) if sent > 0 => {
                    self.out.drain(..sent);
                }
                Ok(_) => break,
                Err(err) if err == -super::e::EAGAIN => break,
                Err(_) => {
                    self.out.clear();
                    self.broken = true;
                    break;
                }
            }
        }
    }

    // ---- запросы ----------------------------------------------------------------

    fn request(&mut self, id: u32, opcode: u16, body: &[u8]) -> Result<(), Bad> {
        let mut args = Args { data: body, at: 0 };
        let Some(object) = self.objects.get(&id) else {
            // Объект уже уничтожен нами (`delete_id` в пути) — запрос молча
            // теряется, как у любого сервера.
            return Ok(());
        };
        match object {
            Obj::Display => match opcode {
                0 => {
                    let callback = args.u32()?;
                    let serial = self.next_serial();
                    self.send(Msg::new(callback, 0).u32(serial));
                    self.send(Msg::new(1, 1).u32(callback));
                }
                1 => {
                    let registry = args.u32()?;
                    self.objects.insert(registry, Obj::Registry);
                    for (name, interface, version) in GLOBALS {
                        self.send(Msg::new(registry, 0).u32(name).str(interface).u32(version));
                    }
                }
                _ => {}
            },
            Obj::Registry => {
                if opcode == 0 {
                    let name = args.u32()?;
                    let interface = args.string()?;
                    let version = args.u32()?;
                    let new = args.u32()?;
                    self.bind(name, &interface, version, new);
                }
            }
            Obj::Callback | Obj::Region | Obj::Output | Obj::Keyboard => {
                // `destroy`/`release` у тех, у кого он есть, — нулевой.
                let destroys = match object {
                    Obj::Region => opcode == 0,
                    Obj::Output | Obj::Keyboard => opcode == 0,
                    _ => false,
                };
                if destroys {
                    self.delete(id);
                }
            }
            Obj::Compositor => {
                let new = args.u32()?;
                let made = if opcode == 0 { Obj::Surface(SurfaceState::new()) } else { Obj::Region };
                self.objects.insert(new, made);
            }
            // `create_data_source(id)`, `get_data_device(id, seat)`.
            Obj::DataDeviceManager => {
                let new = args.u32()?;
                if opcode == 0 {
                    self.objects.insert(new, Obj::DataSource(Vec::new()));
                } else {
                    self.objects.insert(new, Obj::DataDevice);
                    // Новому устройству — нынешнее выделение на ближайшем шаге.
                    self.selection_seen = SELECTION_GEN.load(Ordering::Acquire).wrapping_sub(1);
                }
            }
            // `offer(mime)`, `destroy`, `set_actions`.
            Obj::DataSource(_) => match opcode {
                0 => {
                    let mime = args.string()?;
                    if let Some(Obj::DataSource(mimes)) = self.objects.get_mut(&id) {
                        if mimes.len() < 32 && !mimes.contains(&mime) {
                            mimes.push(mime);
                        }
                    }
                }
                1 => {
                    self.forget_source(id);
                    self.delete(id);
                }
                _ => {}
            },
            // `start_drag`, `set_selection(source, serial)`, `release`.
            Obj::DataDevice => match opcode {
                1 => {
                    let source = args.u32()?;
                    self.set_selection(source);
                }
                2 => self.delete(id),
                _ => {}
            },
            // `accept`, `receive(mime, fd)`, `destroy`, `finish`, `set_actions`.
            Obj::DataOffer => match opcode {
                1 => {
                    let mime = args.string()?;
                    let Some(fd) = self.fds.pop_front() else {
                        self.fail(id, 2, "receive without a descriptor");
                        return Ok(());
                    };
                    // Канал — владельцу выделения; нет владельца — канал
                    // закроется здесь, и клиент прочтёт пустоту.
                    if let Some(owner) = SELECTION.lock().as_ref().map(|selection| (selection.owner, selection.source)) {
                        TO_OWNER.lock().push((owner.0, ToOwner::Send { source: owner.1, mime, fd }));
                        crate::io_events::notify();
                    }
                }
                2 => self.delete(id),
                _ => {}
            },
            // `destroy`, `get_viewport(id, surface)`.
            Obj::Viewporter => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::Viewport);
                }
                _ => {}
            },
            // `destroy`, `set_source`, `set_destination`: окно стола — в размер
            // буфера, масштаба нет.
            Obj::Viewport => {
                if opcode == 0 {
                    self.delete(id);
                }
            }
            Obj::Subcompositor => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    let surface = args.u32()?;
                    let parent = args.u32()?;
                    if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                        state.role = Role::Sub { parent, x: 0, y: 0, below: false };
                    }
                    if let Some(Obj::Surface(state)) = self.objects.get_mut(&parent) {
                        state.children.push(surface);
                    }
                    self.objects.insert(new, Obj::Subsurface(surface));
                }
                _ => {}
            },
            Obj::Subsurface(surface) => {
                let surface = *surface;
                match opcode {
                    0 => {
                        self.unparent(surface);
                        self.delete(id);
                    }
                    1 => {
                        let (x, y) = (args.i32()?, args.i32()?);
                        if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                            if let Role::Sub { parent, below, .. } = state.role {
                                state.role = Role::Sub { parent, x, y, below };
                            }
                        }
                    }
                    // `place_above(sibling)`, `place_below(sibling)`. Java
                    // (AWT на Wayland) кладёт тень окна подповерхностью **под**
                    // окно; без этого полупрозрачная тень ложилась поверх
                    // содержимого, и окно выглядело как под тёмной плёнкой
                    // (Rider в VirtualBox, 27.10).
                    2 | 3 => {
                        let sibling = args.u32()?;
                        self.restack(surface, sibling, opcode == 3);
                    }
                    _ => {}
                }
            }
            Obj::Shm => {
                if opcode == 0 {
                    let new = args.u32()?;
                    let size = args.i32()?;
                    let Some(fd) = self.fds.pop_front() else {
                        self.fail(id, 2, "create_pool without a descriptor");
                        return Ok(());
                    };
                    // `memfd` и `/dev/shm` — сами общая память; обычный файл
                    // (Wine 11: файл раздела) — через общий объект файла.
                    match fd.shared_memory().or_else(|| fd.file_memory(size.max(0) as usize)) {
                        Some(mem) => {
                            self.objects.insert(new, Obj::Pool(mem));
                        }
                        None => self.fail(id, 2, "pool descriptor is not shared memory"),
                    }
                }
            }
            Obj::Pool(mem) => match opcode {
                0 => {
                    let mem = Arc::clone(mem);
                    let new = args.u32()?;
                    let offset = args.i32()?;
                    let (width, height, stride) = (args.i32()?, args.i32()?, args.i32()?);
                    let format = args.u32()?;
                    if offset < 0 || width <= 0 || height <= 0 || stride < width * 4 || !matches!(format, SHM_ARGB8888 | SHM_XRGB8888) {
                        self.fail(id, 0, "unsupported buffer");
                        return Ok(());
                    }
                    let buffer = Buffer { mem, offset: offset as u64, width: width as u32, height: height as u32, stride: stride as u32, format };
                    self.objects.insert(new, Obj::Buffer(buffer));
                }
                1 => self.delete(id),
                _ => {}
            },
            Obj::Buffer(_) => {
                if opcode == 0 {
                    self.delete(id);
                }
            }
            Obj::Surface(_) => self.surface_request(id, opcode, &mut args)?,
            Obj::WmBase => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::Positioner(Positioner::default()));
                }
                2 => {
                    let new = args.u32()?;
                    let surface = args.u32()?;
                    self.objects.insert(new, Obj::XdgSurface(surface));
                }
                _ => {}
            },
            Obj::Positioner(old) => {
                let mut next = *old;
                match opcode {
                    0 => {
                        self.delete(id);
                        return Ok(());
                    }
                    1 => (next.width, next.height) = (args.i32()?, args.i32()?),
                    2 => next.anchor = (args.i32()?, args.i32()?, args.i32()?, args.i32()?),
                    3 => next.edge = args.u32()?,
                    4 => next.gravity = args.u32()?,
                    5 => next.adjust = args.u32()?,
                    6 => next.offset = (args.i32()?, args.i32()?),
                    _ => {}
                }
                self.objects.insert(id, Obj::Positioner(next));
            }
            Obj::XdgSurface(surface) => {
                let surface = *surface;
                self.xdg_surface_request(id, surface, opcode, &mut args)?;
            }
            Obj::Toplevel => {
                match opcode {
                    0 => {
                        if let Some(index) = self.wins.iter().position(|win| win.toplevel == id) {
                            self.close_win(index);
                        }
                        self.delete(id);
                    }
                    2 => {
                        let title = args.string()?;
                        if let Some(win) = self.wins.iter_mut().find(|win| win.toplevel == id) {
                            win.title = title.clone();
                            // Стол занят — заголовок не теряется, а уходит на
                            // следующем витке: Chromium шлёт его один раз.
                            win.title_due = win.open.is_some() && crate::ui::title_window(self.pid, win.slot, &title).is_err();
                        }
                    }
                    // Рамку Chromium рисует сам (окно без рамки стола), и о
                    // перетаскивании, размере и кнопках просит стол он.
                    5 | 6 | 9 | 10 | 13 => {
                        // `resize(seat, serial, edges)`: у Wayland верх 1, низ 2,
                        // лево 4, право 8 — у стола свои биты.
                        let edges = if opcode == 6 {
                            let (_seat, _serial, wl) = (args.u32()?, args.u32()?, args.u32()?);
                            let mut edges = 0u8;
                            for (bit, ours) in [
                                (1, crate::ui::window::EDGE_TOP),
                                (2, crate::ui::window::EDGE_BOTTOM),
                                (4, crate::ui::window::EDGE_LEFT),
                                (8, crate::ui::window::EDGE_RIGHT),
                            ] {
                                if wl & bit != 0 {
                                    edges |= ours;
                                }
                            }
                            edges
                        } else {
                            0
                        };
                        if let Some(win) = self.wins.iter().find(|win| win.toplevel == id && win.open.is_some()) {
                            let (pid, slot) = (self.pid, win.slot);
                            let _ = match opcode {
                                5 => crate::ui::begin_move(pid, slot),
                                6 => crate::ui::begin_resize(pid, slot, edges),
                                9 => crate::ui::maximize_window(pid, slot, true),
                                10 => crate::ui::maximize_window(pid, slot, false),
                                _ => crate::ui::minimize_window(pid, slot),
                            };
                        }
                    }
                    _ => {}
                }
            }
            Obj::Popup => match opcode {
                0 => {
                    if let Some(index) = self.pops.iter().position(|pop| pop.popup == id) {
                        self.close_popup(index);
                    }
                    self.delete(id);
                }
                1 => {
                    if let Some(pop) = self.pops.iter_mut().find(|pop| pop.popup == id) {
                        pop.grab = true;
                    }
                }
                _ => {}
            },
            Obj::Seat => match opcode {
                0 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::Pointer);
                }
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::Keyboard);
                    self.keyboard_start(new);
                }
                3 => self.delete(id),
                _ => {}
            },
            Obj::Pointer => {
                if opcode == 1 {
                    self.delete(id);
                }
            }
        }
        Ok(())
    }

    fn bind(&mut self, name: u32, interface: &str, version: u32, new: u32) {
        let Some((_, global, ours)) = GLOBALS.iter().find(|(number, _, _)| *number == name) else {
            self.fail(1, 0, "no such global");
            return;
        };
        if *global != interface {
            self.fail(1, 0, "interface does not match the global");
            return;
        }
        let version = version.min(*ours);
        let object = match name {
            1 => Obj::Compositor,
            2 => Obj::Subcompositor,
            3 => Obj::Shm,
            4 => Obj::WmBase,
            5 => Obj::Seat,
            7 => Obj::DataDeviceManager,
            8 => Obj::Viewporter,
            _ => Obj::Output,
        };
        self.objects.insert(new, object);
        match name {
            3 => {
                self.send(Msg::new(new, 0).u32(SHM_ARGB8888));
                self.send(Msg::new(new, 0).u32(SHM_XRGB8888));
            }
            5 => {
                self.seat_version = version;
                self.send(Msg::new(new, 0).u32(SEAT_POINTER | SEAT_KEYBOARD));
                if version >= 2 {
                    self.send(Msg::new(new, 1).str("seat0"));
                }
            }
            6 => {
                let (width, height) = crate::ui::screen_size();
                // Физический размер — как у экрана 96 точек на дюйм.
                let (mm_w, mm_h) = ((width * 254 / 960) as i32, (height * 254 / 960) as i32);
                self.send(Msg::new(new, 0).i32(0).i32(0).i32(mm_w).i32(mm_h).i32(0).str("FossAi").str("Screen").i32(0));
                self.send(Msg::new(new, 1).u32(3).i32(width as i32).i32(height as i32).i32(60_000));
                if version >= 2 {
                    self.send(Msg::new(new, 3).i32(1));
                    self.send(Msg::new(new, 2));
                }
            }
            _ => {}
        }
    }

    fn surface_request(&mut self, id: u32, opcode: u16, args: &mut Args<'_>) -> Result<(), Bad> {
        match opcode {
            0 => {
                if let Some(index) = self.wins.iter().position(|win| win.surface == id) {
                    self.close_win(index);
                }
                if let Some(index) = self.pops.iter().position(|pop| pop.surface == id) {
                    self.close_popup(index);
                }
                self.unparent(id);
                if let Some(Obj::Surface(SurfaceState { shown: Some((buffer, _)), .. })) = self.objects.get(&id) {
                    let buffer = *buffer;
                    self.release(buffer);
                }
                self.delete(id);
            }
            1 => {
                let buffer = args.u32()?;
                // Прикреплён снова — снова занят сервером до `release`.
                self.released.retain(|&other| other != buffer);
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
                    state.pending = Some((buffer != 0).then_some(buffer));
                }
            }
            3 => {
                let callback = args.u32()?;
                self.objects.insert(callback, Obj::Callback);
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
                    state.frames.push(callback);
                }
            }
            // `damage` и `damage_buffer`: при масштабе 1 координаты одни.
            2 | 9 => {
                let (x, y, w, h) = (args.i32()?, args.i32()?, args.i32()?, args.i32()?);
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
                    let (x0, y0) = (x.max(0), y.max(0));
                    let (x1, y1) = (x.saturating_add(w).min(8192), y.saturating_add(h).min(8192));
                    if x1 > x0 && y1 > y0 {
                        let rect = Rect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
                        state.damage = state.damage.union(&rect);
                    }
                }
            }
            6 => self.commit(id),
            _ => {}
        }
        Ok(())
    }

    fn xdg_surface_request(&mut self, id: u32, surface: u32, opcode: u16, args: &mut Args<'_>) -> Result<(), Bad> {
        match opcode {
            0 => self.delete(id),
            1 => {
                let new = args.u32()?;
                self.objects.insert(new, Obj::Toplevel);
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                    state.role = Role::Toplevel(id);
                }
                let slot = self.next_slot;
                self.next_slot += 1;
                self.wins.push(Win {
                    surface,
                    xdg: id,
                    toplevel: new,
                    slot,
                    title: String::from("Wayland"),
                    geometry: None,
                    open: None,
                    configured: false,
                    dirty: false,
                    see_through: false,
                    active: true,
                    title_due: false,
                });
            }
            2 => {
                let new = args.u32()?;
                let parent = args.u32()?;
                let positioner = args.u32()?;
                self.objects.insert(new, Obj::Popup);
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                    state.role = Role::Popup(id);
                }
                let place = match self.objects.get(&positioner) {
                    Some(Obj::Positioner(place)) => *place,
                    _ => Positioner::default(),
                };
                // Родитель — окно или другое меню (подменю): место считается от
                // угла окна верхнего уровня, над которым всё это всплывает.
                let owner = if let Some(win) = self.wins.iter().find(|win| win.xdg == parent) {
                    Some((win.slot, (0, 0)))
                } else {
                    self.pops.iter().find(|pop| pop.xdg == parent).map(|pop| (pop.owner, pop.at))
                };
                let Some((owner, base)) = owner.or_else(|| self.wins.first().map(|win| (win.slot, (0, 0)))) else {
                    // Всплывать не над чем — сказать, что меню закрыто.
                    self.send(Msg::new(new, 1));
                    return Ok(());
                };
                let origin = crate::ui::window_rect(self.pid, owner).map_or((0, 0), |rect| (rect.x, rect.y));
                let (sw, sh) = crate::ui::screen_size();
                let (x, y) = place.constrain((origin.0 + base.0, origin.1 + base.1), (sw as i32, sh as i32));
                let slot = self.next_slot;
                self.next_slot += 1;
                self.pops.push(Pop {
                    surface,
                    xdg: id,
                    popup: new,
                    slot,
                    owner,
                    at: (base.0 + x, base.1 + y),
                    geometry: None,
                    open: None,
                    dirty: false,
                    see_through: false,
                    grab: false,
                    done: false,
                });
                self.send(Msg::new(new, 0).i32(x).i32(y).i32(place.width.max(1)).i32(place.height.max(1)));
                let serial = self.next_serial();
                self.send(Msg::new(id, 0).u32(serial));
            }
            3 => {
                let (x, y, w, h) = (args.i32()?, args.i32()?, args.i32()?, args.i32()?);
                if w > 0 && h > 0 {
                    let geometry = Some((x, y, w as u32, h as u32));
                    if let Some(win) = self.wins.iter_mut().find(|win| win.xdg == id) {
                        win.geometry = geometry;
                    } else if let Some(pop) = self.pops.iter_mut().find(|pop| pop.xdg == id) {
                        pop.geometry = geometry;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `wl_subsurface.place_above`/`place_below`: переставить подповерхность
    /// над или под `sibling` — родителем или сестрой. Над родителем — значит
    /// первой из лежащих над ним, под родителем — последней из лежащих под.
    fn restack(&mut self, surface: u32, sibling: u32, below: bool) {
        let Some(Obj::Surface(SurfaceState { role: Role::Sub { parent, x, y, .. }, .. })) = self.objects.get(&surface) else {
            return;
        };
        let (parent, x, y) = (*parent, *x, *y);
        let sibling_below = match self.objects.get(&sibling) {
            Some(Obj::Surface(SurfaceState { role: Role::Sub { parent: other, below, .. }, .. })) if *other == parent => Some(*below),
            _ => None,
        };
        // Сестра — встать рядом с ней на её сторону от родителя; родитель —
        // на выбранную сторону.
        let (side, anchor) = match sibling_below {
            Some(side) if sibling != parent => (side, Some(sibling)),
            _ => (below, None),
        };
        if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
            state.role = Role::Sub { parent, x, y, below: side };
        }
        let sides: Vec<(u32, bool)> = match self.objects.get(&parent) {
            Some(Obj::Surface(state)) => state
                .children
                .iter()
                .map(|child| {
                    let side = match self.objects.get(child) {
                        Some(Obj::Surface(SurfaceState { role: Role::Sub { below, .. }, .. })) => *below,
                        _ => false,
                    };
                    (*child, side)
                })
                .collect(),
            _ => return,
        };
        let Some(Obj::Surface(state)) = self.objects.get_mut(&parent) else {
            return;
        };
        state.children.retain(|child| *child != surface);
        let at = match anchor {
            Some(anchor) => {
                let index = state.children.iter().position(|child| *child == anchor).unwrap_or(state.children.len());
                if below { index } else { index + 1 }
            }
            // Сразу над родителем — первой из лежащих над ним; сразу под —
            // последней из лежащих под.
            None => sides.iter().filter(|(child, side)| *child != surface && *side).count(),
        };
        let at = at.min(state.children.len());
        state.children.insert(at, surface);
    }

    /// Отцепить подповерхность от родителя.
    fn unparent(&mut self, surface: u32) {
        let parent = match self.objects.get(&surface) {
            Some(Obj::Surface(SurfaceState { role: Role::Sub { parent, .. }, .. })) => *parent,
            _ => return,
        };
        if let Some(Obj::Surface(state)) = self.objects.get_mut(&parent) {
            state.children.retain(|child| *child != surface);
        }
        if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
            state.role = Role::None;
        }
    }

    fn commit(&mut self, id: u32) {
        let (pending, frames) = match self.objects.get_mut(&id) {
            Some(Obj::Surface(state)) => (state.pending.take(), core::mem::take(&mut state.frames)),
            _ => return,
        };
        self.frames_due.extend(frames);
        let attached = pending.is_some();
        if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
            let damage = core::mem::replace(&mut state.damage, Rect::EMPTY);
            state.ready = state.ready.union(&damage);
            // Новый буфер без объявленных изменений — клиент не сказал, что
            // в нём другое; перерисовать целиком безопаснее.
            if attached && damage.is_empty() {
                state.full = true;
            }
        }
        if let Some(next) = pending {
            let shown = next.and_then(|buffer| match self.objects.get(&buffer) {
                Some(Obj::Buffer(buf)) => Some((buffer, buf.clone())),
                _ => None,
            });
            let old = match self.objects.get_mut(&id) {
                Some(Obj::Surface(state)) => {
                    let size = |shown: &Option<(u32, Buffer)>| shown.as_ref().map(|(_, buf)| (buf.width, buf.height));
                    if size(&state.shown) != size(&shown) {
                        state.full = true;
                    }
                    core::mem::replace(&mut state.shown, shown)
                }
                _ => None,
            };
            // Прежний буфер больше не читается — вернуть его клиенту.
            if let Some((old, _)) = old {
                if Some(old) != next {
                    self.release(old);
                }
            }
        }
        // Окно, которому принадлежит поверхность (через цепочку родителей).
        let mut root = id;
        for _ in 0..16 {
            match self.objects.get(&root) {
                Some(Obj::Surface(SurfaceState { role: Role::Sub { parent, .. }, .. })) => root = *parent,
                _ => break,
            }
        }
        let Some(index) = self.wins.iter().position(|win| win.surface == root) else {
            if let Some(index) = self.pops.iter().position(|pop| pop.surface == root) {
                self.present_popup(index);
            }
            return;
        };
        if !self.wins[index].configured {
            // Первый `commit` без буфера — просьба о `configure`: размер
            // выбирает клиент.
            self.wins[index].configured = true;
            self.configure(index, 0, 0);
        }
        self.present(index);
    }

    /// `xdg_toplevel.configure` с размером (0 — «выбери сам») и состояниями
    /// окна стола, затем `xdg_surface.configure`.
    fn configure(&mut self, index: usize, width: i32, height: i32) {
        let (toplevel, xdg, slot, active) = {
            let win = &self.wins[index];
            (win.toplevel, win.xdg, win.slot, win.active)
        };
        let mut states = Vec::new();
        if crate::ui::window_maximized(self.pid, slot) {
            states.extend_from_slice(&XDG_TOPLEVEL_MAXIMIZED.to_ne_bytes());
        }
        if active {
            states.extend_from_slice(&XDG_TOPLEVEL_ACTIVATED.to_ne_bytes());
        }
        self.send(Msg::new(toplevel, 0).i32(width).i32(height).bytes(&states));
        let serial = self.next_serial();
        self.send(Msg::new(xdg, 0).u32(serial));
    }

    /// Размер, который окно сейчас занимает на столе: развёрнутому — ровно он
    /// (иначе клиент вправе выбрать другой), обычному — 0, «как было».
    fn current_size(&self, index: usize) -> (i32, i32) {
        let win = &self.wins[index];
        match win.open {
            Some((w, h)) if crate::ui::window_maximized(self.pid, win.slot) => (w as i32, h as i32),
            _ => (0, 0),
        }
    }

    /// `wl_buffer.release`, если буфер ещё жив.
    fn release(&mut self, buffer: u32) {
        if self.released.contains(&buffer) {
            return;
        }
        if matches!(self.objects.get(&buffer), Some(Obj::Buffer(_))) {
            self.send(Msg::new(buffer, 0));
            self.released.push(buffer);
        }
    }

    /// Ответить на `wl_surface.frame` всех показанных кадров — сразу, а не на
    /// следующем витке (13.11): клиент в режиме FIFO (DXVK, Mesa WSI) ждал
    /// на каждом кадре сон сервера в 16 мс.
    fn answer_frames(&mut self, time: u32) {
        for callback in core::mem::take(&mut self.frames_due) {
            if self.objects.contains_key(&callback) {
                self.send(Msg::new(callback, 0).u32(time));
                self.delete(callback);
            }
        }
    }

    /// Свести поверхность окна с подповерхностями и показать на столе.
    fn present(&mut self, index: usize) {
        let surface = self.wins[index].surface;
        let (width, height) = match self.objects.get(&surface) {
            Some(Obj::Surface(SurfaceState { shown: Some((_, buf)), .. })) => (buf.width, buf.height),
            Some(Obj::Surface(_)) => (0, 0),
            _ => return,
        };
        if width == 0 || height == 0 {
            if self.wins[index].open.is_some() {
                self.close_win_only(index);
            }
            return;
        }
        let (gx, gy, gw, gh) = self.wins[index].geometry.unwrap_or((0, 0, width, height));
        let (gw, gh) = (gw.clamp(1, 4096), gh.clamp(1, 4096));
        let mut layers = Vec::new();
        self.layers(surface, -gx, -gy, 0, &mut layers);
        let damaged = self.damaged(surface, -gx, -gy, 0);
        let win = &mut self.wins[index];
        let pid = self.pid;
        let mut whole = win.dirty;
        win.dirty = true;
        match win.open {
            None => {
                whole = true;
                let Some(pixels) = Surface::new(gw, gh, theme::window_bg()) else {
                    return;
                };
                // Без рамки стола: заголовок, вкладки и кнопки рисует клиент
                // (Chromium) — иначе выходит «окно в окне».
                if crate::ui::open_window_framed(pid, win.slot, &win.title, pixels, false).is_err() {
                    return;
                }
                let _ = crate::ui::style_window(pid, win.slot, user_abi::WIN_STYLE_RESIZABLE);
                let _ = crate::ui::raw_keys_window(pid, win.slot);
                kprintln!("  wayland     : #{pid} window '{}' {gw}x{gh}", win.title);
                win.open = Some((gw, gh));
            }
            Some(size) if size != (gw, gh) => {
                let Some(pixels) = Surface::new(gw, gh, theme::window_bg()) else {
                    return;
                };
                if crate::ui::resize_window(pid, win.slot, pixels).is_err() {
                    return;
                }
                whole = true;
                win.open = Some((gw, gh));
            }
            Some(_) => {}
        }
        let swap = mini_ui::format_code() == user_abi::PIXEL_RGB;
        let bounds = Rect::new(0, 0, gw, gh);
        // Только изменившееся — кроме первого кадра, смены размера и повтора
        // после занятого стола (тогда неизвестно, что уже нарисовано).
        let area = match damaged {
            _ if whole => bounds,
            None => bounds,
            Some(rect) => rect.intersect(&bounds),
        };
        if area.is_empty() {
            win.dirty = false;
            return;
        }
        let alpha = has_alpha(&layers);
        let single = layers.len() == 1;
        let mut clear = false;
        let painted = crate::ui::paint_window(pid, win.slot, area, |target| clear = paint(target, &layers, swap, area, alpha));
        win.dirty = painted.is_err();
        if painted.is_ok() {
            // Прозрачность помнится до перерисовки целиком: частичная видит
            // только свой кусок.
            let see_through = clear || (win.see_through && area != bounds);
            if see_through != win.see_through {
                // Тень и форму рисует программа, если не сказала, где тело
                // окна (`set_window_geometry`): иначе поля вокруг — её тень.
                let shaped = win.geometry.is_none();
                if crate::ui::alpha_window(pid, win.slot, see_through, shaped).is_ok() {
                    win.see_through = see_through;
                }
            }
            self.clear_damage(surface, 0);
            // Кадр скопирован на стол — буфер свободен уже сейчас (13.11): Mesa
            // WSI с тремя-четырьмя буферами иначе ждала следующего кадра, чтобы
            // получить прежний обратно. Только без подповерхностей: их кадр
            // сводится из нескольких буферов при каждой перерисовке.
            if single {
                if let Some(Obj::Surface(SurfaceState { shown: Some((buffer, _)), .. })) = self.objects.get(&surface) {
                    let buffer = *buffer;
                    self.release(buffer);
                }
            }
        }
    }

    /// Изменившееся в поверхности и её подповерхностях с прошлой отрисовки,
    /// в координатах окна. `None` — перерисовать целиком.
    fn damaged(&self, id: u32, dx: i32, dy: i32, depth: u32) -> Option<Rect> {
        let Some(Obj::Surface(state)) = self.objects.get(&id) else {
            return Some(Rect::EMPTY);
        };
        if state.full || depth >= 8 {
            return None;
        }
        let mut area = if state.ready.is_empty() { Rect::EMPTY } else { state.ready.translate(dx, dy) };
        for child in &state.children {
            if let Some(Obj::Surface(SurfaceState { role: Role::Sub { x, y, .. }, .. })) = self.objects.get(child) {
                area = area.union(&self.damaged(*child, dx + x, dy + y, depth + 1)?);
            }
        }
        Some(area)
    }

    /// Нарисовано — забыть изменения поверхности и подповерхностей.
    fn clear_damage(&mut self, id: u32, depth: u32) {
        let children = match self.objects.get_mut(&id) {
            Some(Obj::Surface(state)) => {
                state.ready = Rect::EMPTY;
                state.full = false;
                state.children.clone()
            }
            _ => return,
        };
        if depth < 8 {
            for child in children {
                self.clear_damage(child, depth + 1);
            }
        }
    }

    /// Показать всплывающее окно: открыть над окном-владельцем при первом
    /// буфере, сменить размер, нарисовать.
    fn present_popup(&mut self, index: usize) {
        let surface = self.pops[index].surface;
        let (width, height) = match self.objects.get(&surface) {
            Some(Obj::Surface(SurfaceState { shown: Some((_, buf)), .. })) => (buf.width, buf.height),
            Some(Obj::Surface(_)) => (0, 0),
            _ => return,
        };
        if width == 0 || height == 0 {
            self.close_popup_window(index);
            return;
        }
        let (gx, gy, gw, gh) = self.pops[index].geometry.unwrap_or((0, 0, width, height));
        let (gw, gh) = (gw.clamp(1, 4096), gh.clamp(1, 4096));
        let mut layers = Vec::new();
        self.layers(surface, -gx, -gy, 0, &mut layers);
        let pid = self.pid;
        let pop = &mut self.pops[index];
        pop.dirty = true;
        match pop.open {
            None => {
                let Some(pixels) = Surface::new(gw, gh, theme::window_bg()) else {
                    return;
                };
                if crate::ui::open_popup(pid, pop.slot, "Wayland", pixels, pop.owner, pop.at).is_err() {
                    return;
                }
                kprintln!("  wayland     : #{pid} popup {gw}x{gh} at {},{}", pop.at.0, pop.at.1);
                pop.open = Some((gw, gh));
            }
            Some(size) if size != (gw, gh) => {
                let Some(pixels) = Surface::new(gw, gh, theme::window_bg()) else {
                    return;
                };
                if crate::ui::resize_window(pid, pop.slot, pixels).is_err() {
                    return;
                }
                pop.open = Some((gw, gh));
            }
            Some(_) => {}
        }
        let swap = mini_ui::format_code() == user_abi::PIXEL_RGB;
        let area = Rect::new(0, 0, gw, gh);
        let alpha = has_alpha(&layers);
        let mut clear = false;
        let painted = crate::ui::paint_window(pid, pop.slot, area, |target| clear = paint(target, &layers, swap, area, alpha));
        pop.dirty = painted.is_err();
        // Подсказка со скруглёнными углами: тень и форма — её, прямоугольная
        // тень стола под ней была бы видна.
        if painted.is_ok() && clear != pop.see_through && crate::ui::alpha_window(pid, pop.slot, clear, true).is_ok() {
            pop.see_through = clear;
        }
    }

    /// Убрать окно стола всплывающего, оставив его запись (буфер сняли).
    fn close_popup_window(&mut self, index: usize) {
        let pid = self.pid;
        let pop = &mut self.pops[index];
        pop.dirty = false;
        if pop.open.take().is_some() {
            close_slot(pid, pop.slot);
            kprintln!("  wayland     : #{pid} popup closed");
        }
        if self.pointer_on == Some(pop.surface) {
            self.pointer_on = None;
        }
    }

    /// Всплывающее уничтожено.
    fn close_popup(&mut self, index: usize) {
        self.close_popup_window(index);
        self.pops.remove(index);
    }

    /// Сказать всем открытым меню `popup_done` — от верхних к нижним.
    /// `grabbed_only` — только тем, что взяли захват (подсказки щелчок мимо
    /// не закрывает). `true`, если сказано хоть одному.
    fn dismiss_popups(&mut self, grabbed_only: bool) -> bool {
        let mut done = Vec::new();
        for pop in self.pops.iter_mut().rev() {
            if !pop.done && (pop.grab || !grabbed_only) {
                pop.done = true;
                done.push(pop.popup);
            }
        }
        for popup in &done {
            self.send(Msg::new(*popup, 1));
        }
        !done.is_empty()
    }

    /// Слои окна снизу вверх: буфер поверхности и её подповерхностей с их
    /// местом в окне.
    fn layers(&self, id: u32, dx: i32, dy: i32, depth: u32, out: &mut Vec<Layer>) {
        let Some(Obj::Surface(state)) = self.objects.get(&id) else {
            return;
        };
        // Сначала лежащие под поверхностью (тень окна Java), потом она сама,
        // потом лежащие над ней — каждая группа в порядке `children`.
        for under in [true, false] {
            if !under {
                if let Some((_, buf)) = &state.shown {
                    if out.try_reserve(1).is_err() {
                        return;
                    }
                    // Поверх того, что под ней, поверхность смешивается по
                    // альфе; нижний слой окна — копией, как было.
                    let blend = (depth > 0 || !out.is_empty()) && buf.format == SHM_ARGB8888;
                    out.push(Layer { buf: buf.clone(), x: dx, y: dy, blend });
                }
            }
            if depth >= 8 {
                continue;
            }
            for child in &state.children {
                if let Some(Obj::Surface(SurfaceState { role: Role::Sub { x, y, below, .. }, .. })) = self.objects.get(child) {
                    if *below == under {
                        self.layers(*child, dx + x, dy + y, depth + 1, out);
                    }
                }
            }
        }
    }

    // ---- окна стола -------------------------------------------------------------

    fn close_win_only(&mut self, index: usize) {
        // Меню над этим окном закрываются вместе с ним.
        let slot = self.wins[index].slot;
        while let Some(at) = self.pops.iter().position(|pop| pop.owner == slot && pop.open.is_some()) {
            self.close_popup_window(at);
        }
        let win = &mut self.wins[index];
        if win.open.take().is_some() {
            close_slot(self.pid, win.slot);
        }
        if self.pointer_on == Some(win.surface) {
            self.pointer_on = None;
        }
        if self.keyboard_on == Some(win.surface) {
            self.keyboard_on = None;
        }
    }

    fn close_win(&mut self, index: usize) {
        self.close_win_only(index);
        self.wins.remove(index);
    }

    fn close_all(&mut self) {
        while !self.pops.is_empty() {
            self.close_popup(0);
        }
        while !self.wins.is_empty() {
            self.close_win(0);
        }
        // Ушедший клиент не напишет своё выделение никому.
        let mut selection = SELECTION.lock();
        if selection.as_ref().is_some_and(|held| held.owner == self.key) {
            *selection = None;
            SELECTION_GEN.fetch_add(1, Ordering::AcqRel);
        }
        drop(selection);
        TO_OWNER.lock().retain(|(owner, _)| *owner != self.key);
    }

    /// `wl_data_device.set_selection(source)`: выделение — этого клиента
    /// (или ничьё, если `source` ноль). Прежний владелец узнаёт, что его
    /// источник больше не нужен.
    fn set_selection(&mut self, source: u32) {
        let mimes = match self.objects.get(&source) {
            Some(Obj::DataSource(mimes)) => Some(mimes.clone()),
            _ => None,
        };
        let mut selection = SELECTION.lock();
        if mimes.is_none() && selection.as_ref().is_some_and(|held| held.owner != self.key) {
            // Чужое выделение снимать «ничем» нельзя.
            return;
        }
        if let Some(old) = selection.take() {
            if !(old.owner == self.key && old.source == source) {
                TO_OWNER.lock().push((old.owner, ToOwner::Cancelled { source: old.source }));
            }
        }
        *selection = mimes.map(|mimes| Selection { owner: self.key, source, mimes });
        SELECTION_GEN.fetch_add(1, Ordering::AcqRel);
        drop(selection);
        crate::io_events::notify();
    }

    /// Источник уничтожен — если выделение было на нём, оно пропадает.
    fn forget_source(&mut self, source: u32) {
        let mut selection = SELECTION.lock();
        if selection.as_ref().is_some_and(|held| held.owner == self.key && held.source == source) {
            *selection = None;
            SELECTION_GEN.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Выделение сменилось — каждому `wl_data_device` клиента новое
    /// предложение (`data_offer`, форматы, `selection`) или «пусто».
    fn sync_selection(&mut self) {
        let generation = SELECTION_GEN.load(Ordering::Acquire);
        if generation == self.selection_seen {
            return;
        }
        self.selection_seen = generation;
        let devices: Vec<u32> = self.objects.iter().filter(|(_, obj)| matches!(obj, Obj::DataDevice)).map(|(id, _)| *id).collect();
        if devices.is_empty() {
            return;
        }
        let mimes = SELECTION.lock().as_ref().map(|selection| selection.mimes.clone());
        for device in devices {
            match &mimes {
                Some(mimes) => {
                    let offer = self.next_server_id;
                    self.next_server_id = self.next_server_id.wrapping_add(1).max(0xff00_0000);
                    self.objects.insert(offer, Obj::DataOffer);
                    self.send(Msg::new(device, 0).u32(offer));
                    for mime in mimes {
                        self.send(Msg::new(offer, 0).str(mime));
                    }
                    self.send(Msg::new(device, 5).u32(offer));
                }
                None => self.send(Msg::new(device, 5).u32(0)),
            }
        }
    }

    /// Просьбы к этому клиенту как к владельцу выделения: написать данные в
    /// канал (`send`) или забыть источник (`cancelled`).
    fn serve_owner(&mut self) {
        let mine: Vec<ToOwner> = {
            let mut queue = TO_OWNER.lock();
            if !queue.iter().any(|(owner, _)| *owner == self.key) {
                return;
            }
            let all: Vec<(u64, ToOwner)> = queue.drain(..).collect();
            let (mine, rest): (Vec<_>, Vec<_>) = all.into_iter().partition(|(owner, _)| *owner == self.key);
            *queue = rest;
            mine.into_iter().map(|(_, request)| request).collect()
        };
        for request in mine {
            match request {
                ToOwner::Send { source, mime, fd } => {
                    if matches!(self.objects.get(&source), Some(Obj::DataSource(_))) {
                        self.out_fds.push(fd);
                        self.send(Msg::new(source, 1).str(&mime));
                    }
                }
                ToOwner::Cancelled { source } => {
                    if matches!(self.objects.get(&source), Some(Obj::DataSource(_))) {
                        self.send(Msg::new(source, 2));
                    }
                }
            }
        }
    }

    /// События столовых окон — клиенту.
    fn window_events(&mut self, time: u32) {
        for index in 0..self.wins.len() {
            let (slot, surface, geometry) = {
                let win = &self.wins[index];
                if win.open.is_none() {
                    continue;
                }
                (win.slot, win.surface, win.geometry)
            };
            let (gx, gy) = geometry.map_or((0, 0), |(x, y, _, _)| (x, y));
            for _ in 0..64 {
                let Some(event) = crate::ui::next_window_event(self.pid, slot) else {
                    break;
                };
                let (x, y) = (event.x + gx, event.y + gy);
                match event.kind {
                    user_abi::WIN_MOVE => {
                        self.pointer_motion_at(surface, x, y, time);
                    }
                    // Щелчок мимо открытого меню закрывает его и дальше не
                    // идёт — как у сервера с захватом указателя.
                    user_abi::WIN_POINTER if self.dismiss_popups(true) => self.swallow_release = true,
                    user_abi::WIN_RELEASE if self.swallow_release => self.swallow_release = false,
                    user_abi::WIN_POINTER | user_abi::WIN_RELEASE => {
                        let target = self.pointer_motion_at(surface, x, y, time);
                        let button = if event.code == 2 { BTN_RIGHT } else { BTN_LEFT };
                        let state = u32::from(event.kind == user_abi::WIN_POINTER);
                        // Нажатие — в журнал: «щелчок не доходит до программы»
                        // иначе снаружи не разобрать (крестик диалога Java,
                        // 27.10).
                        if state == 1 {
                            kprintln!("  wayland     : #{} press at {x},{y} of surface {target}", self.pid);
                        }
                        self.pointer_send(|pointer, serial| Msg::new(pointer, 3).u32(serial).u32(time).u32(button).u32(state));
                    }
                    user_abi::WIN_WHEEL => {
                        // У Wayland «больше нуля» — вниз; щелчок — 10 единиц.
                        let clicks = event.x;
                        self.pointer_send(|pointer, _| Msg::new(pointer, 8).u32(0).i32(-clicks));
                        self.pointer_send(|pointer, _| Msg::new(pointer, 4).u32(time).u32(0).fixed(-clicks * 10));
                    }
                    user_abi::WIN_LEAVE => {
                        // Указатель мог стоять на подповерхности этого окна.
                        if let Some(on) = self.pointer_on.filter(|on| self.root_of(*on) == surface) {
                            self.pointer_on = None;
                            self.pointer_send(|pointer, serial| Msg::new(pointer, 1).u32(serial).u32(on));
                        }
                    }
                    user_abi::WIN_RESIZE => self.configure(index, event.x, event.y),
                    user_abi::WIN_FOCUS => {
                        // Рамка Chromium у неактивного окна бледнее — ему
                        // нужно знать о фокусе.
                        self.wins[index].active = event.code != 0;
                        if event.code == 0 {
                            self.dismiss_popups(false);
                        }
                        let (w, h) = self.current_size(index);
                        self.configure(index, w, h);
                        if event.code != 0 {
                            self.keyboard_enter(surface);
                        } else if self.keyboard_on == Some(surface) {
                            self.keyboard_on = None;
                            self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 2).u32(serial).u32(surface));
                        }
                    }
                    crate::ui::WIN_RAWKEY => {
                        if self.keyboard_on != Some(surface) {
                            self.keyboard_enter(surface);
                        }
                        self.sync_group();
                        let (code, pressed) = (event.code, event.x != 0);
                        let held = self.keys_down.contains(&code);
                        if pressed && held {
                            // Повтор ядра: для клиента — отпустили и нажали.
                            self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 3).u32(serial).u32(time).u32(code).u32(0));
                        }
                        if !pressed && !held {
                            // Отпускание клавиши, нажатой до фокуса, — клиенту
                            // оно ни к чему.
                            continue;
                        }
                        if pressed {
                            if !held {
                                self.keys_down.push(code);
                            }
                        } else {
                            self.keys_down.retain(|key| *key != code);
                        }
                        let state = u32::from(pressed);
                        self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 3).u32(serial).u32(time).u32(code).u32(state));
                        self.key_modifiers(code, pressed);
                    }
                    user_abi::WIN_CLOSE => {
                        let toplevel = self.wins[index].toplevel;
                        self.send(Msg::new(toplevel, 1));
                    }
                    _ => {}
                }
            }
        }
    }

    /// События всплывающих окон: только указатель — клавиатура остаётся у
    /// окна верхнего уровня.
    fn popup_events(&mut self, time: u32) {
        for index in 0..self.pops.len() {
            let (slot, surface, geometry) = {
                let pop = &self.pops[index];
                if pop.open.is_none() {
                    continue;
                }
                (pop.slot, pop.surface, pop.geometry)
            };
            let (gx, gy) = geometry.map_or((0, 0), |(x, y, _, _)| (x, y));
            for _ in 0..64 {
                let Some(event) = crate::ui::next_window_event(self.pid, slot) else {
                    break;
                };
                let (x, y) = (event.x + gx, event.y + gy);
                match event.kind {
                    user_abi::WIN_MOVE => {
                        self.pointer_motion_at(surface, x, y, time);
                    }
                    user_abi::WIN_POINTER | user_abi::WIN_RELEASE => {
                        let target = self.pointer_motion_at(surface, x, y, time);
                        let button = if event.code == 2 { BTN_RIGHT } else { BTN_LEFT };
                        let state = u32::from(event.kind == user_abi::WIN_POINTER);
                        // Нажатие — в журнал: «щелчок не доходит до программы»
                        // иначе снаружи не разобрать (крестик диалога Java,
                        // 27.10).
                        if state == 1 {
                            kprintln!("  wayland     : #{} press at {x},{y} of surface {target}", self.pid);
                        }
                        self.pointer_send(|pointer, serial| Msg::new(pointer, 3).u32(serial).u32(time).u32(button).u32(state));
                    }
                    user_abi::WIN_WHEEL => {
                        let clicks = event.x;
                        self.pointer_send(|pointer, _| Msg::new(pointer, 8).u32(0).i32(-clicks));
                        self.pointer_send(|pointer, _| Msg::new(pointer, 4).u32(time).u32(0).fixed(-clicks * 10));
                    }
                    user_abi::WIN_LEAVE => {
                        // Указатель мог стоять на подповерхности этого окна.
                        if let Some(on) = self.pointer_on.filter(|on| self.root_of(*on) == surface) {
                            self.pointer_on = None;
                            self.pointer_send(|pointer, serial| Msg::new(pointer, 1).u32(serial).u32(on));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Новый `wl_keyboard`: раскладка, повтор и — если фокус уже есть — вход.
    fn keyboard_start(&mut self, keyboard: u32) {
        let mem = SharedMem::new(0);
        let node = Arc::new(crate::user::shm::ShmNode(Arc::clone(&mem)));
        let mut text = Vec::with_capacity(KEYMAP.len() + 1);
        text.extend_from_slice(KEYMAP.as_bytes());
        text.push(0);
        if crate::vfs::Node::write_at(&*node, 0, &text).is_err() {
            return;
        }
        self.out_fds.push(Handle::of_node(node, false));
        self.send(Msg::new(keyboard, 0).u32(KEYMAP_XKB_V1).u32(text.len() as u32));
        // Повтора у клиента нет (частота 0): зажатую клавишу повторяет ядро,
        // как typematic PS/2, а сервер шлёт повтор отпусканием и нажатием.
        // Повтор клиента считает от его собственного таймера, и медленный
        // клиент (Chromium под эмулятором) успевал «дожать» букву два-двенадцать
        // раз, прежде чем прочитать отпускание.
        if self.seat_version >= 4 {
            self.send(Msg::new(keyboard, 5).i32(0).i32(600));
        }
        if let Some(surface) = self.keyboard_on {
            let serial = self.next_serial();
            self.send(Msg::new(keyboard, 1).u32(serial).u32(surface).bytes(&[]));
            self.send(Msg::new(keyboard, 4).u32(serial).u32(self.mods_down).u32(0).u32(self.mods_locked).u32(self.group));
        }
    }

    fn keyboard_enter(&mut self, surface: u32) {
        if let Some(old) = self.keyboard_on.filter(|old| *old != surface) {
            self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 2).u32(serial).u32(old));
        }
        self.keyboard_on = Some(surface);
        self.mods_down = 0;
        self.keys_down.clear();
        self.group = layout_group();
        let (locked, group) = (self.mods_locked, self.group);
        self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 1).u32(serial).u32(surface).bytes(&[]));
        self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 4).u32(serial).u32(0).u32(0).u32(locked).u32(group));
    }

    /// Язык системы сменился (Alt+Shift, трей) — сказать клиенту новую
    /// группу раскладки до клавиши, иначе Chromium печатал бы латиницей
    /// при включённой русской (Роман, 27.10).
    fn sync_group(&mut self) {
        let group = layout_group();
        if group == self.group {
            return;
        }
        self.group = group;
        let (down, locked) = (self.mods_down, self.mods_locked);
        self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 4).u32(serial).u32(down).u32(0).u32(locked).u32(group));
    }

    /// Пересчитать модификаторы после клавиши `code` и сообщить, если они
    /// изменились.
    fn key_modifiers(&mut self, code: u32, pressed: bool) {
        let (down, locked) = (self.mods_down, self.mods_locked);
        let held = match code {
            42 | 54 => MOD_SHIFT,
            29 | 97 => MOD_CONTROL,
            56 | 100 => MOD_ALT,
            125 | 126 => MOD_SUPER,
            _ => 0,
        };
        if held != 0 {
            self.mods_down = if pressed { down | held } else { down & !held };
        }
        if pressed {
            match code {
                58 => self.mods_locked ^= MOD_LOCK,
                69 => self.mods_locked ^= MOD_NUM,
                _ => {}
            }
        }
        if (self.mods_down, self.mods_locked) != (down, locked) {
            let (down, locked, group) = (self.mods_down, self.mods_locked, self.group);
            self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 4).u32(serial).u32(down).u32(0).u32(locked).u32(group));
        }
    }

    /// Событие каждому `wl_keyboard` клиента.
    fn keyboard_send(&mut self, make: impl Fn(u32, u32) -> Msg) {
        let keyboards: Vec<u32> = self.objects.iter().filter(|(_, obj)| matches!(obj, Obj::Keyboard)).map(|(id, _)| *id).collect();
        let serial = self.next_serial();
        for keyboard in keyboards {
            self.send(make(keyboard, serial));
        }
    }

    /// Указатель над окном `root` в точке `(x, y)`: событие — той поверхности,
    /// что под ним (подповерхность — в её координатах). Возвращает её.
    ///
    /// Подсказки Chromium — «сохранить пароль», «восстановить вкладки»,
    /// автозаполнение — не всплывающие окна, а подповерхности главного; пока
    /// указатель всегда шёл главной поверхности, щелчок по подсказке Chromium
    /// принимал за щелчок по странице под ней (Роман, 29.10).
    fn pointer_motion_at(&mut self, root: u32, x: i32, y: i32, time: u32) -> u32 {
        let (target, lx, ly) = self.surface_at(root, x, y, 0).unwrap_or((root, x, y));
        self.pointer_motion(target, lx, ly, time);
        target
    }

    /// Самая верхняя поверхность дерева `id` с буфером под точкой `(x, y)`
    /// (в координатах `id`): над ней лежащие подповерхности — сверху вниз,
    /// она сама, под ней лежащие.
    fn surface_at(&self, id: u32, x: i32, y: i32, depth: u32) -> Option<(u32, i32, i32)> {
        let Some(Obj::Surface(state)) = self.objects.get(&id) else {
            return None;
        };
        let child_hit = |under: bool| -> Option<(u32, i32, i32)> {
            if depth >= 8 {
                return None;
            }
            for child in state.children.iter().rev() {
                if let Some(Obj::Surface(SurfaceState { role: Role::Sub { x: cx, y: cy, below, .. }, .. })) = self.objects.get(child) {
                    if *below == under {
                        if let Some(hit) = self.surface_at(*child, x - cx, y - cy, depth + 1) {
                            return Some(hit);
                        }
                    }
                }
            }
            None
        };
        if let Some(hit) = child_hit(false) {
            return Some(hit);
        }
        if let Some((_, buf)) = &state.shown {
            if x >= 0 && y >= 0 && (x as u32) < buf.width && (y as u32) < buf.height {
                return Some((id, x, y));
            }
        }
        child_hit(true)
    }

    /// Корень дерева подповерхностей.
    fn root_of(&self, mut id: u32) -> u32 {
        for _ in 0..8 {
            match self.objects.get(&id) {
                Some(Obj::Surface(SurfaceState { role: Role::Sub { parent, .. }, .. })) => id = *parent,
                _ => break,
            }
        }
        id
    }

    fn pointer_motion(&mut self, surface: u32, x: i32, y: i32, time: u32) {
        if self.pointer_on != Some(surface) {
            if let Some(old) = self.pointer_on {
                self.pointer_send(|pointer, serial| Msg::new(pointer, 1).u32(serial).u32(old));
            }
            self.pointer_on = Some(surface);
            self.pointer_send(|pointer, serial| Msg::new(pointer, 0).u32(serial).u32(surface).fixed(x).fixed(y));
        } else if self.pointer_at == Some((x, y)) {
            // Указатель не сдвинулся — движения не было. Лишнее `motion`
            // между нажатием и отпусканием AWT (Java) считает перетаскиванием
            // и отменяет щелчок: крестик и кнопки окон Java не нажимались
            // (диалог лицензии Rider, 27.10).
            return;
        }
        self.pointer_at = Some((x, y));
        self.pointer_send(|pointer, _| Msg::new(pointer, 2).u32(time).fixed(x).fixed(y));
    }

    /// Событие каждому `wl_pointer` клиента, затем `frame`.
    fn pointer_send(&mut self, make: impl Fn(u32, u32) -> Msg) {
        let pointers: Vec<u32> = self.objects.iter().filter(|(_, obj)| matches!(obj, Obj::Pointer)).map(|(id, _)| *id).collect();
        if pointers.is_empty() {
            return;
        }
        let serial = self.next_serial();
        for pointer in pointers {
            self.send(make(pointer, serial));
            self.send(Msg::new(pointer, 5));
        }
    }
}

/// Группа `wayland.xkb` для языка системы: 0 — английская, 1 — русская.
fn layout_group() -> u32 {
    u32::from(crate::input::keymap::layout() == crate::input::keymap::Layout::Ru)
}

/// Закрыть окно стола, переждав занятый стол.
fn close_slot(pid: u32, slot: u32) {
    for _ in 0..50 {
        match crate::ui::close_window(pid, slot) {
            Err(crate::ui::WindowError::Busy) => crate::sched::sleep_ms(2),
            _ => break,
        }
    }
}

/// Буфер на своём месте в окне.
struct Layer {
    buf: Buffer,
    x: i32,
    y: i32,
    blend: bool,
}

/// Может ли окно быть прозрачным: нижний слой с альфой. Нижний XRGB
/// закрывает всё, что над ним смешивается.
fn has_alpha(layers: &[Layer]) -> bool {
    layers.first().is_some_and(|layer| layer.buf.format == SHM_ARGB8888)
}

/// Нарисовать слои в поверхность окна — строками прямо из памяти клиентов.
/// `swap` — у экрана красный в младшем байте (у `wl_shm` — синий).
///
/// `alpha` — альфа остаётся в старшем байте (цвет умножен на неё, как у
/// Wayland), под слоями — прозрачность, а не подложка: тень и скругления,
/// которые рисует программа, стол смешает с тем, что под окном. Ответ —
/// нашлась ли в `area` хоть одна не вполне непрозрачная точка.
fn paint(target: &mut Surface, layers: &[Layer], swap: bool, area: Rect, alpha: bool) -> bool {
    let (tw, th) = (target.width(), target.height());
    let area = area.intersect(&Rect::new(0, 0, tw, th));
    if area.is_empty() {
        return false;
    }
    let (ax0, ax1) = (area.x, area.right());
    let (ay0, ay1) = (area.y, area.bottom());
    let to_screen = |pixel: u32| if swap { (pixel & 0xff00_ff00) | ((pixel >> 16) & 0xff) | ((pixel & 0xff) << 16) } else { pixel };
    let keep = if alpha { u32::MAX } else { 0xff_ffff };
    let fill = if alpha { 0 } else { to_screen(BACKDROP) & keep };
    // Один непрозрачный слой закрывает всю область — подложка не видна, и
    // заливать её — лишний проход по кадру (13.11: DXVK, ~90 мс на кадр).
    let covered = layers.len() == 1 && {
        let layer = &layers[0];
        !layer.blend
            && layer.x <= ax0
            && layer.y <= ay0
            && layer.x + layer.buf.width as i32 >= ax1
            && layer.y + layer.buf.height as i32 >= ay1
    };
    if !covered {
        for y in ay0..ay1 {
            target.row_mut(y as u32)[ax0 as usize..ax1 as usize].fill(fill);
        }
    }
    let mut row = Vec::new();
    if row.try_reserve_exact(area.w as usize * 4).is_err() {
        return false;
    }
    row.resize(area.w as usize * 4, 0u8);
    for layer in layers {
        let buf = &layer.buf;
        let x0 = layer.x.max(ax0);
        let x1 = (layer.x + buf.width as i32).min(ax1);
        if x0 >= x1 {
            continue;
        }
        let count = (x1 - x0) as usize;
        let first_column = (x0 - layer.x) as u64;
        // Только строки слоя, попадающие в изменившуюся область.
        let sy0 = (ay0 - layer.y).max(0) as u32;
        let sy1 = ((ay1 - layer.y).max(0) as u32).min(buf.height);
        for sy in sy0..sy1 {
            let ty = sy as i32 + layer.y;
            if ty < ay0 || ty >= ay1 {
                continue;
            }
            let at = buf.offset + u64::from(sy) * u64::from(buf.stride) + first_column * 4;
            if !layer.blend {
                // Короткий путь (13.11): строка клиента — прямо на место в окне,
                // затем один простой проход (альфа, при нужде — красный с
                // синим), без промежуточного буфера и ветвлений в цикле.
                let out = &mut target.row_mut(ty as u32)[x0 as usize..x1 as usize];
                // SAFETY: те же байты, что у среза `u32` (выравнивание `u8` —
                // единица), длина — `count` точек по 4 байта.
                let bytes = unsafe { core::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<u8>(), count * 4) };
                if !buf.mem.read(at, bytes) {
                    return false;
                }
                let or = if buf.format == SHM_XRGB8888 { 0xff00_0000 } else { 0 };
                if swap {
                    for pixel in out.iter_mut() {
                        *pixel = to_screen(u32::from_le(*pixel) | or) & keep;
                    }
                } else if or != 0 || keep != u32::MAX {
                    for pixel in out.iter_mut() {
                        *pixel = (u32::from_le(*pixel) | or) & keep;
                    }
                }
                continue;
            }
            if !buf.mem.read(at, &mut row[..count * 4]) {
                return false;
            }
            let out = &mut target.row_mut(ty as u32)[x0 as usize..x1 as usize];
            for (pixel, bytes) in out.iter_mut().zip(row.chunks_exact(4)) {
                let mut src = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if buf.format == SHM_XRGB8888 {
                    src |= 0xff00_0000;
                }
                let src = to_screen(src);
                *pixel = if layer.blend { mini_ui::over(src, *pixel) } else { src } & keep;
            }
        }
    }
    alpha && (ay0..ay1).any(|y| target.row(y as u32)[ax0 as usize..ax1 as usize].iter().any(|pixel| pixel >> 24 != 0xff))
}

