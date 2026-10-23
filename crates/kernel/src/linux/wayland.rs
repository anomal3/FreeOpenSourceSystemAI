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
//! `wl_output`, `wl_seat` с указателем и клавиатурой. Пиксели копируются: буфер
//! возвращается клиенту (`wl_buffer.release`) сразу после копии, а окно
//! держит свою. Форматы — `ARGB8888` и `XRGB8888`; альфа учитывается только
//! у подповерхностей.
//!
//! # Клавиатура
//!
//! Окна сервера просят у стола коды клавиш (`ui::raw_keys_window`): каждое
//! нажатие и отпускание приходит кодом evdev, символы клиент считает сам по
//! раскладке XKB, которую получает от сервера (`wayland.xkb`, US, через
//! `memfd`). Модификаторы сервер считает по тем же кодам и сообщает
//! `wl_keyboard.modifiers`.
//!
//! Чего нет: всплывающих окон (`xdg_popup`
//! получает `configure`, но не показывается), `dmabuf`, буфера обмена,
//! масштаба кроме 1.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

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
const GLOBALS: [(u32, &str, u32); 6] = [
    (1, "wl_compositor", 4),
    (2, "wl_subcompositor", 1),
    (3, "wl_shm", 1),
    (4, "xdg_wm_base", 2),
    (5, "wl_seat", 5),
    (6, "wl_output", 2),
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
const XDG_TOPLEVEL_ACTIVATED: u32 = 4;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

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
    offset: (i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    None,
    /// Окно верхнего уровня: номер `xdg_surface`.
    Toplevel(u32),
    /// Всплывающее: номер `xdg_surface`.
    Popup(u32),
    /// Подповерхность: родитель и место в нём.
    Sub { parent: u32, x: i32, y: i32 },
}

struct SurfaceState {
    /// `attach` до `commit`: `Some(None)` — снять буфер.
    pending: Option<Option<u32>>,
    frames: Vec<u32>,
    /// Своя копия показанного: `ARGB8888` построчно.
    pixels: Vec<u32>,
    width: u32,
    height: u32,
    alpha: bool,
    role: Role,
    children: Vec<u32>,
}

impl SurfaceState {
    fn new() -> Self {
        Self { pending: None, frames: Vec::new(), pixels: Vec::new(), width: 0, height: 0, alpha: false, role: Role::None, children: Vec::new() }
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
    next_slot: u32,
    serial: u32,
    /// `wl_callback` кадров, которые отвечают на следующем витке — после
    /// того как кадр показан.
    frames_due: Vec<u32>,
    pointer_on: Option<u32>,
    /// Поверхность с фокусом клавиатуры.
    keyboard_on: Option<u32>,
    /// Нажатые модификаторы и включённые замки (маски `MOD_*`).
    mods_down: u32,
    mods_locked: u32,
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
            next_slot: FIRST_SLOT,
            serial: 0,
            frames_due: Vec::new(),
            pointer_on: None,
            keyboard_on: None,
            mods_down: 0,
            mods_locked: 0,
            seat_version: 1,
            broken: false,
        }
    }

    fn send(&mut self, msg: Msg) {
        let data = msg.finish();
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
        for callback in core::mem::take(&mut self.frames_due) {
            if self.objects.contains_key(&callback) {
                self.send(Msg::new(callback, 0).u32(time));
                self.delete(callback);
            }
        }
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
            if self.request(id, opcode, &body).is_err() {
                self.fail(id, 1, "malformed request");
            }
        }
        if !self.broken {
            self.window_events(time);
            for index in 0..self.wins.len() {
                if self.wins[index].dirty {
                    self.present(index);
                }
            }
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
            Obj::Subcompositor => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    let surface = args.u32()?;
                    let parent = args.u32()?;
                    if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                        state.role = Role::Sub { parent, x: 0, y: 0 };
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
                            if let Role::Sub { parent, .. } = state.role {
                                state.role = Role::Sub { parent, x, y };
                            }
                        }
                    }
                    _ => {}
                }
            }
            Obj::Shm => {
                if opcode == 0 {
                    let new = args.u32()?;
                    let _size = args.i32()?;
                    let Some(fd) = self.fds.pop_front() else {
                        self.fail(id, 2, "create_pool without a descriptor");
                        return Ok(());
                    };
                    match fd.shared_memory() {
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
                            if win.open.is_some() {
                                let _ = crate::ui::title_window(self.pid, win.slot, &title);
                            }
                        }
                    }
                    _ => {}
                }
            }
            Obj::Popup => {
                if opcode == 0 {
                    self.delete(id);
                }
            }
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
                self.unparent(id);
                self.delete(id);
            }
            1 => {
                let buffer = args.u32()?;
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
                });
            }
            2 => {
                let new = args.u32()?;
                let _parent = args.u32()?;
                let positioner = args.u32()?;
                self.objects.insert(new, Obj::Popup);
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                    state.role = Role::Popup(id);
                }
                let place = match self.objects.get(&positioner) {
                    Some(Obj::Positioner(place)) => *place,
                    _ => Positioner::default(),
                };
                let (ax, ay, aw, ah) = place.anchor;
                let x = ax + aw / 2 + place.offset.0;
                let y = ay + ah + place.offset.1;
                self.send(Msg::new(new, 0).i32(x).i32(y).i32(place.width.max(1)).i32(place.height.max(1)));
                let serial = self.next_serial();
                self.send(Msg::new(id, 0).u32(serial));
            }
            3 => {
                let (x, y, w, h) = (args.i32()?, args.i32()?, args.i32()?, args.i32()?);
                if let Some(win) = self.wins.iter_mut().find(|win| win.xdg == id) {
                    if w > 0 && h > 0 {
                        win.geometry = Some((x, y, w as u32, h as u32));
                    }
                }
            }
            _ => {}
        }
        Ok(())
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
        match pending {
            Some(Some(buffer)) => {
                let copied = match self.objects.get(&buffer) {
                    Some(Obj::Buffer(buf)) => copy_buffer(buf),
                    _ => None,
                };
                if let Some((pixels, width, height, alpha)) = copied {
                    if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
                        state.pixels = pixels;
                        (state.width, state.height, state.alpha) = (width, height, alpha);
                    }
                }
                // Копия снята — буфер клиенту больше не нужен нам.
                self.send(Msg::new(buffer, 0));
            }
            Some(None) => {
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
                    state.pixels.clear();
                    (state.width, state.height) = (0, 0);
                }
            }
            None => {}
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
            return;
        };
        if !self.wins[index].configured {
            // Первый `commit` без буфера — просьба о `configure`: размер
            // выбирает клиент.
            self.wins[index].configured = true;
            let (toplevel, xdg) = (self.wins[index].toplevel, self.wins[index].xdg);
            self.send(Msg::new(toplevel, 0).i32(0).i32(0).bytes(&XDG_TOPLEVEL_ACTIVATED.to_ne_bytes()));
            let serial = self.next_serial();
            self.send(Msg::new(xdg, 0).u32(serial));
        }
        self.present(index);
    }

    /// Свести поверхность окна с подповерхностями и показать на столе.
    fn present(&mut self, index: usize) {
        let surface = self.wins[index].surface;
        let (width, height) = match self.objects.get(&surface) {
            Some(Obj::Surface(state)) => (state.width, state.height),
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
        let mut canvas = vec![BACKDROP; (gw * gh) as usize];
        self.draw_tree(surface, -gx, -gy, &mut canvas, gw, gh, 0);
        let win = &mut self.wins[index];
        let pid = self.pid;
        win.dirty = true;
        match win.open {
            None => {
                let Some(pixels) = Surface::new(gw, gh, theme::window_bg()) else {
                    return;
                };
                if crate::ui::open_window(pid, win.slot, &win.title, pixels).is_err() {
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
                win.open = Some((gw, gh));
            }
            Some(_) => {}
        }
        let swap = mini_ui::format_code() == user_abi::PIXEL_RGB;
        let area = Rect::new(0, 0, gw, gh);
        let painted = crate::ui::paint_window(pid, win.slot, area, |target| {
            let width = target.width().min(gw) as usize;
            for y in 0..target.height().min(gh) {
                let src = &canvas[(y * gw) as usize..][..width];
                for (out, pixel) in target.row_mut(y)[..width].iter_mut().zip(src) {
                    *out = if swap { (pixel & 0xff00) | ((pixel >> 16) & 0xff) | ((pixel & 0xff) << 16) } else { pixel & 0xff_ffff };
                }
            }
        });
        win.dirty = painted.is_err();
    }

    /// Нарисовать поверхность и её подповерхности в `canvas` со сдвигом.
    #[allow(clippy::too_many_arguments)]
    fn draw_tree(&self, id: u32, dx: i32, dy: i32, canvas: &mut [u32], cw: u32, ch: u32, depth: u32) {
        let Some(Obj::Surface(state)) = self.objects.get(&id) else {
            return;
        };
        let blend = depth > 0 && state.alpha;
        for y in 0..state.height as i32 {
            let ty = y + dy;
            if ty < 0 || ty >= ch as i32 {
                continue;
            }
            for x in 0..state.width as i32 {
                let tx = x + dx;
                if tx < 0 || tx >= cw as i32 {
                    continue;
                }
                let src = state.pixels[(y as u32 * state.width + x as u32) as usize];
                let at = (ty as u32 * cw + tx as u32) as usize;
                canvas[at] = if blend { over(src, canvas[at]) } else { src };
            }
        }
        if depth >= 8 {
            return;
        }
        for child in &state.children {
            if let Some(Obj::Surface(SurfaceState { role: Role::Sub { x, y, .. }, .. })) = self.objects.get(child) {
                self.draw_tree(*child, dx + x, dy + y, canvas, cw, ch, depth + 1);
            }
        }
    }

    // ---- окна стола -------------------------------------------------------------

    fn close_win_only(&mut self, index: usize) {
        let win = &mut self.wins[index];
        if win.open.take().is_some() {
            for _ in 0..50 {
                match crate::ui::close_window(self.pid, win.slot) {
                    Err(crate::ui::WindowError::Busy) => crate::sched::sleep_ms(2),
                    _ => break,
                }
            }
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
        while !self.wins.is_empty() {
            self.close_win(0);
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
                    user_abi::WIN_MOVE => self.pointer_motion(surface, x, y, time),
                    user_abi::WIN_POINTER | user_abi::WIN_RELEASE => {
                        self.pointer_motion(surface, x, y, time);
                        let button = if event.code == 2 { BTN_RIGHT } else { BTN_LEFT };
                        let state = u32::from(event.kind == user_abi::WIN_POINTER);
                        self.pointer_send(|pointer, serial| Msg::new(pointer, 3).u32(serial).u32(time).u32(button).u32(state));
                    }
                    user_abi::WIN_WHEEL => {
                        // У Wayland «больше нуля» — вниз; щелчок — 10 единиц.
                        let clicks = event.x;
                        self.pointer_send(|pointer, _| Msg::new(pointer, 8).u32(0).i32(-clicks));
                        self.pointer_send(|pointer, _| Msg::new(pointer, 4).u32(time).u32(0).fixed(-clicks * 10));
                    }
                    user_abi::WIN_LEAVE => {
                        if self.pointer_on == Some(surface) {
                            self.pointer_on = None;
                            self.pointer_send(|pointer, serial| Msg::new(pointer, 1).u32(serial).u32(surface));
                        }
                    }
                    user_abi::WIN_RESIZE => {
                        let (toplevel, xdg) = (self.wins[index].toplevel, self.wins[index].xdg);
                        self.send(Msg::new(toplevel, 0).i32(event.x).i32(event.y).bytes(&XDG_TOPLEVEL_ACTIVATED.to_ne_bytes()));
                        let serial = self.next_serial();
                        self.send(Msg::new(xdg, 0).u32(serial));
                    }
                    user_abi::WIN_FOCUS => {
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
                        let (code, pressed) = (event.code, event.x != 0);
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
        if self.seat_version >= 4 {
            self.send(Msg::new(keyboard, 5).i32(25).i32(600));
        }
        if let Some(surface) = self.keyboard_on {
            let serial = self.next_serial();
            self.send(Msg::new(keyboard, 1).u32(serial).u32(surface).bytes(&[]));
            self.send(Msg::new(keyboard, 4).u32(serial).u32(self.mods_down).u32(0).u32(self.mods_locked).u32(0));
        }
    }

    fn keyboard_enter(&mut self, surface: u32) {
        if let Some(old) = self.keyboard_on.filter(|old| *old != surface) {
            self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 2).u32(serial).u32(old));
        }
        self.keyboard_on = Some(surface);
        self.mods_down = 0;
        let locked = self.mods_locked;
        self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 1).u32(serial).u32(surface).bytes(&[]));
        self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 4).u32(serial).u32(0).u32(0).u32(locked).u32(0));
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
            let (down, locked) = (self.mods_down, self.mods_locked);
            self.keyboard_send(|keyboard, serial| Msg::new(keyboard, 4).u32(serial).u32(down).u32(0).u32(locked).u32(0));
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

    fn pointer_motion(&mut self, surface: u32, x: i32, y: i32, time: u32) {
        if self.pointer_on != Some(surface) {
            if let Some(old) = self.pointer_on {
                self.pointer_send(|pointer, serial| Msg::new(pointer, 1).u32(serial).u32(old));
            }
            self.pointer_on = Some(surface);
            self.pointer_send(|pointer, serial| Msg::new(pointer, 0).u32(serial).u32(surface).fixed(x).fixed(y));
        }
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

/// Скопировать буфер `wl_shm` к себе: `(пиксели, ширина, высота, альфа)`.
fn copy_buffer(buffer: &Buffer) -> Option<(Vec<u32>, u32, u32, bool)> {
    let (width, height) = (buffer.width, buffer.height.min(4096));
    let width = width.min(4096);
    let mut pixels = vec![0u32; (width * height) as usize];
    let mut row = vec![0u8; width as usize * 4];
    for y in 0..height {
        let at = buffer.offset + u64::from(y) * u64::from(buffer.stride);
        if !buffer.mem.read(at, &mut row) {
            return None;
        }
        let out = &mut pixels[(y * width) as usize..][..width as usize];
        for (pixel, bytes) in out.iter_mut().zip(row.chunks_exact(4)) {
            *pixel = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
    }
    let alpha = buffer.format == SHM_ARGB8888;
    if !alpha {
        for pixel in &mut pixels {
            *pixel |= 0xff00_0000;
        }
    }
    Some((pixels, width, height, alpha))
}

/// `src` поверх `dst` (альфа заранее умножена, как у Wayland).
fn over(src: u32, dst: u32) -> u32 {
    let alpha = src >> 24;
    if alpha == 0xff {
        return src;
    }
    if alpha == 0 {
        return dst;
    }
    let inverse = 255 - alpha;
    let channel = |shift: u32| {
        let s = (src >> shift) & 0xff;
        let d = (dst >> shift) & 0xff;
        (s + d * inverse / 255).min(255) << shift
    };
    channel(0) | channel(8) | channel(16) | 0xff00_0000
}
