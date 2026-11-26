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

/// `zwp_linux_dmabuf_v1` (NVK, веха M7) — отдельным файлом.
#[path = "wayland_dmabuf.rs"]
mod dmabuf;

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

// ---- one clipboard with native programs (24.11.2026) ----------------------------
//
// The selection's owner `NATIVE` is the system clipboard
// ([`crate::ui::clipboard`]): what the terminal, Notepad or Files copied is
// offered to Wayland clients, and the server itself writes it into the pipe of
// `wl_data_offer.receive`. A client's own selection is read at once when it is
// set - the server asks the client (`wl_data_source.send`) to write the text
// and the `text/uri-list` into kernel pipes and puts what arrives on the
// system clipboard (Roman, 23.11: text copied in Notepad++ under Proton did
// not paste into the terminal).

/// Owner number of the system clipboard (clients are numbered from 1).
const NATIVE: u64 = 0;

/// The system clipboard changed: the server takes the selection over on its
/// next round ([`take_native`]). A flag, not a lock - the desktop calls this
/// holding its spin lock.
static NATIVE_DIRTY: AtomicBool = AtomicBool::new(false);

/// What a client's selection is being read as.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchKind {
    Text,
    Files,
}

/// A client's selection being read through a pipe.
struct Fetch {
    reader: crate::user::pipe::Reader,
    kind: FetchKind,
    data: Vec<u8>,
    done: bool,
}

/// Reads of the current client selection (all of one `set_selection`) and when
/// to give up on them.
static FETCHES: Mutex<(Vec<Fetch>, u64)> = Mutex::new((Vec::new(), 0));

/// The system clipboard being written to a client's `receive` pipe.
struct Feed {
    fd: Handle,
    data: Vec<u8>,
    at: usize,
    until: u64,
}

static FEEDS: Mutex<Vec<Feed>> = Mutex::new(Vec::new());

/// How long a client gets to write its selection (and to read ours).
const CLIP_WAIT_MS: u64 = 5_000;

/// The system clipboard changed (text or files from a native program): offer
/// it to Wayland clients. Safe to call under any lock.
pub fn native_clipboard_changed() {
    NATIVE_DIRTY.store(true, Ordering::Release);
    crate::io_events::notify();
}

/// The system clipboard as a selection; `None` - it is empty.
fn native_selection() -> Option<Selection> {
    let mimes = crate::ui::clipboard::offered_mimes();
    (!mimes.is_empty()).then_some(Selection { owner: NATIVE, source: 0, mimes })
}

/// The selection becomes the system clipboard's (the previous owner hears
/// `cancelled`); reads of the old client selection are dropped.
fn take_native() {
    let mut selection = SELECTION.lock();
    if let Some(old) = selection.take() {
        if old.owner != NATIVE {
            TO_OWNER.lock().push((old.owner, ToOwner::Cancelled { source: old.source }));
        }
    }
    *selection = native_selection();
    SELECTION_GEN.fetch_add(1, Ordering::AcqRel);
    drop(selection);
    FETCHES.lock().0.clear();
}

/// Pipes of the clipboard bridge, one round: read what clients write about
/// their selection, write ours to clients that asked. `true` - still busy.
fn pump_clipboard(now: u64) -> bool {
    if NATIVE_DIRTY.swap(false, Ordering::AcqRel) {
        take_native();
    }
    let mut busy = false;
    {
        let mut fetches = FETCHES.lock();
        if !fetches.0.is_empty() {
            let deadline = fetches.1;
            let mut chunk = [0u8; 4096];
            for fetch in fetches.0.iter_mut().filter(|fetch| !fetch.done) {
                loop {
                    match fetch.reader.read(&mut chunk, false) {
                        Ok(0) => {
                            fetch.done = true;
                            break;
                        }
                        Ok(n) => {
                            if fetch.data.len() + n > user_abi::CLIP_MAX {
                                fetch.done = true;
                                break;
                            }
                            fetch.data.extend_from_slice(&chunk[..n]);
                        }
                        Err(_) => break,
                    }
                }
            }
            let all = fetches.0.iter().all(|fetch| fetch.done);
            if all || now >= deadline {
                let list = core::mem::take(&mut fetches.0);
                drop(fetches);
                let mut text = None;
                let mut files = None;
                for fetch in list.into_iter().filter(|fetch| fetch.done) {
                    match fetch.kind {
                        FetchKind::Text => text = Some(String::from_utf8_lossy(&fetch.data).replace('\0', "")),
                        FetchKind::Files => files = Some(crate::ui::clipboard::parse_uri_list(&String::from_utf8_lossy(&fetch.data))),
                    }
                }
                let files = files.filter(|files| !files.is_empty());
                if text.is_some() || files.is_some() {
                    kprintln!(
                        "  wayland     : clipboard from a client: {} chars, {} file(s)",
                        text.as_ref().map_or(0, |text| text.chars().count()),
                        files.as_ref().map_or(0, Vec::len)
                    );
                    crate::ui::clipboard::set_from_wayland(text, files);
                } else if !all {
                    kprintln!("  wayland     : clipboard from a client: nothing came in {CLIP_WAIT_MS} ms");
                }
            } else {
                busy = true;
            }
        }
    }
    let mut feeds = FEEDS.lock();
    feeds.retain_mut(|feed| {
        while feed.at < feed.data.len() {
            match feed.fd.write_now(&feed.data[feed.at..]) {
                Ok(0) | Err(true) => return now < feed.until,
                Ok(n) => feed.at += n,
                Err(false) => return false,
            }
        }
        false
    });
    busy || !feeds.is_empty()
}
/// Номер клиента для буфера обмена: у одного процесса бывает несколько
/// соединений, и номер процесса их не различает.
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// Печатать запросы клиентов и события сервера (`kernel wltrace on`).
static TRACE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Темп ответов на `frame` (P8) и `ping` клиентам (P4) — включаемы
/// командами `wlpace`/`wlping`: сравнить с прежним поведением на железе.
static PACE: AtomicBool = AtomicBool::new(true);
static PING: AtomicBool = AtomicBool::new(true);
/// Непрозрачные строки смешиваемых слоёв — копией (19.11.2026);
/// `wlopaque off` — через `over` по точкам, как раньше.
pub static OPAQUE_ROWS: AtomicBool = AtomicBool::new(true);


/// Частота темпа, заданная руками (`wlpace <Гц>`, мГц); 0 — по монитору.
static PACE_FORCED_MHZ: AtomicU64 = AtomicU64::new(0);
/// Частота самого быстрого включённого монитора (мГц) и когда её смотрели.
static PACE_OUTPUT_MHZ: AtomicU64 = AtomicU64::new(60_000);
static PACE_CHECKED_MS: AtomicU64 = AtomicU64::new(0);

/// `wlpace on|off|unlimited|auto|<Гц>` (D4, 20.11.2026): темп ответов на
/// `frame`. `auto` (`on`) — частота монитора (у RTX 3060 с монитором на
/// 144 Гц — 144, у GOP и virtio — 60), `<Гц>` — своя, `off`/`unlimited` — без
/// темпа (замеры: кадры столько, сколько стол успевает показать). Ответ —
/// строка для терминала.
///
/// Темп держит только того, кто его просит (D7, 21.11.2026): `frame` ждёт
/// клиент с VSync (Vulkan FIFO, DXVK `SyncInterval 1`, GL swap interval 1).
/// С выключенной VSync Mesa WSI (`MAILBOX`, `IMMEDIATE` через
/// `wp_tearing_control_v1`) `frame` не просит и ждёт только
/// `wl_buffer.release` — сервер возвращает сменённый буфер сразу и
/// показывает последний кадр с частотой монитора, так что игра рисует
/// столько, сколько успевает GPU. `off` — принудительно без темпа и для FIFO.
pub fn pace_command(argument: &str) -> String {
    match argument.trim() {
        "on" | "auto" => {
            PACE_FORCED_MHZ.store(0, Ordering::Relaxed);
            PACE.store(true, Ordering::Relaxed);
        }
        "off" | "unlimited" => PACE.store(false, Ordering::Relaxed),
        "" => {}
        hz => match hz.parse::<u64>() {
            Ok(hz @ 10..=1000) => {
                PACE_FORCED_MHZ.store(hz * 1000, Ordering::Relaxed);
                PACE.store(true, Ordering::Relaxed);
            }
            _ => return String::from("  wlpace: usage `wlpace [on|auto|off|unlimited|<10..1000 Hz>]`"),
        },
    }
    PACE_CHECKED_MS.store(0, Ordering::Relaxed);
    if !PACE.load(Ordering::Relaxed) {
        return String::from("  wlpace: off (unlimited)");
    }
    let mhz = pace_mhz();
    let forced = PACE_FORCED_MHZ.load(Ordering::Relaxed) != 0;
    alloc::format!(
        "  wlpace: on, {}.{:03} Hz ({}) for vsync clients (FIFO); vsync-off clients (MAILBOX/IMMEDIATE) are not paced",
        mhz / 1000,
        mhz % 1000,
        // G3: without a forced rate each window is paced by its own monitor;
        // this number is the fastest one (windows not placed yet).
        if forced { "forced" } else { "fastest monitor; each window follows its own monitor" }
    )
}

/// Частота темпа в мГц: заданная руками или самого быстрого включённого
/// монитора (реестр выходов смотрится не чаще раза в секунду).
fn pace_mhz() -> u64 {
    let forced = PACE_FORCED_MHZ.load(Ordering::Relaxed);
    if forced != 0 {
        return forced;
    }
    let now = crate::time::uptime_ms();
    let checked = PACE_CHECKED_MS.load(Ordering::Relaxed);
    if checked == 0 || now.saturating_sub(checked) >= 1000 {
        PACE_CHECKED_MS.store(now.max(1), Ordering::Relaxed);
        let best = crate::display::outputs::list()
            .iter()
            .filter(|output| output.enabled)
            .filter_map(|output| output.current.map(|(_, _, mhz)| u64::from(mhz)))
            .filter(|mhz| (10_000..=1_000_000).contains(mhz))
            .max()
            .unwrap_or(60_000);
        PACE_OUTPUT_MHZ.store(best, Ordering::Relaxed);
    }
    PACE_OUTPUT_MHZ.load(Ordering::Relaxed)
}

/// Кадр экрана в микросекундах (темп ответов на `frame`).
fn frame_us() -> u64 {
    1_000_000_000 / pace_mhz().max(1)
}

/// G3 (21.11): the enabled monitor under the centre of `rect` (desktop
/// coordinates of the output registry) and its refresh in mHz.
fn monitor_of(rect: Rect) -> Option<(Rect, u64)> {
    let list = crate::display::outputs::list();
    let lit = || list.iter().filter(|o| o.enabled && o.current.is_some());
    // The desktop starts at the corner of the box around all monitors; the
    // registry keeps its own origin (the primary monitor).
    let left = lit().map(|o| o.x).min()?;
    let top = lit().map(|o| o.y).min()?;
    let (cx, cy) = (rect.x + rect.w as i32 / 2 + left, rect.y + rect.h as i32 / 2 + top);
    lit().find_map(|o| {
        let (w, h, mhz) = o.current?;
        let inside = cx >= o.x && cy >= o.y && cx < o.x + w as i32 && cy < o.y + h as i32;
        (inside && (10_000..=1_000_000).contains(&mhz)).then(|| (Rect::new(o.x - left, o.y - top, w, h), u64::from(mhz)))
    })
}

/// How often the monitor of a window is looked up again (it can be dragged
/// to another monitor).
const PACE_RECHECK_MS: u64 = 500;
/// 25.11: how long a show refused by the busy desktop is retried every
/// millisecond (then the 16 ms poll, as for a window the desktop lost).
const BUSY_FAST_RETRY_MS: u64 = 250;
pub fn set_ping(on: bool) {
    PING.store(on, Ordering::Relaxed);
}
#[must_use]
pub fn ping() -> bool {
    PING.load(Ordering::Relaxed)
}

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
        // G5: monitors switched on/off or moved - clients hear it this round.
        refresh_outputs(now);
        clients.retain_mut(|client| {
            let alive = client.step(now);
            if !alive {
                client.close_all();
                kprintln!("  wayland     : client #{} gone", client.pid);
            }
            alive
        });
        // G3: buffers of gone clients that a display scanned out - freed
        // once it does not any more.
        let graves = {
            let mut grave = SCAN_GRAVE.lock();
            grave.retain(|buf| scanned(buf));
            !grave.is_empty()
        };
        // События окон приходят без уведомления — опрос раз в кадр; раньше —
        // если у клиента отложен ответ на `frame` (темп экрана, P8).
        let mut due = clients.iter().filter_map(Client::frames_due_at).min().unwrap_or(u64::MAX);
        if graves {
            due = 0;
        }
        // Clipboard bridge (24.11): native changes, pipes to and from clients.
        if pump_clipboard(now) {
            due = due.min(now + 4);
        }
        crate::io_events::wait(seen, Some(due.clamp(now + 1, now + 16)));
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
const GLOBALS: [(u32, &str, u32); 18] = [
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
    // `wl_output` (v4) is not here: one global per lit monitor, names from
    // [`FIRST_OUTPUT_GLOBAL`], added and removed with the layout (G5).
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
    // Буферы dma-buf (18.11.2026): аппаратный Vulkan (Mesa NVK) показывает
    // кадры только так (`wayland_dmabuf.rs`).
    (9, dmabuf::INTERFACE, dmabuf::VERSION),
    // Вертикальная синхронизация — решение игры (D7, 21.11.2026). Mesa WSI
    // на Wayland даёт `VK_PRESENT_MODE_IMMEDIATE_KHR` только при этом
    // глобале (MAILBOX и FIFO — всегда): DXVK/vkd3d с выключенной VSync
    // выбирают IMMEDIATE (подсказка `async`), иначе MAILBOX.
    (10, "wp_tearing_control_manager_v1", 1),
    // Время показа кадра (`wp_presentation_feedback`): Mesa отвечает по нему
    // на `vkWaitForPresentKHR` (DXVK ждёт им кадр в полёте) — без него ждать
    // было нечем, кроме `frame`.
    (11, "wp_presentation", 1),
    // G3 (21.11): mouse for games. Wine (hw-log13: "relative motion won't
    // work", "pointer locking/confining won't work") turns cursor clipping
    // and mouse-look into these: raw device motion, and the pointer locked
    // (hidden, does not move) or confined to the window.
    (12, "zwp_relative_pointer_manager_v1", 1),
    (13, "zwp_pointer_constraints_v1", 1),
    // Window activation (SetForegroundWindow under Wine) and the preferred
    // scale of surfaces: our Wayland coordinates are screen pixels
    // (`wl_output.scale` 1), so the scale announced is 1.0 (120/120).
    (14, "xdg_activation_v1", 1),
    (15, "wp_fractional_scale_manager_v1", 1),
    // 22.11: video players and games keep the screensaver and screen off
    // away while an inhibitor lives (`power::inhibit`).
    (16, "zwp_idle_inhibit_manager_v1", 1),
    // G5 (22.11): logical position and size of each monitor. Wine
    // (winewayland) places monitors by these - without them all of them sat
    // at 0,0 and it kept only one.
    (17, "zxdg_output_manager_v1", 3),
    // Clipboard managers (24.11): Wine's winewayland syncs the Windows
    // clipboard through data control ("compositor doesn't support optional
    // ext_data_control_manager_v1 (clipboard functionality will be
    // limited)"); a data-control device sees the selection without keyboard
    // focus and may set it. Both names, the same objects.
    (18, "ext_data_control_manager_v1", 1),
    (19, "zwlr_data_control_manager_v1", 2),
];

// ---- мониторы (G5, 22.11) ----------------------------------------------------------
//
// One `wl_output` global per lit monitor of the output registry
// (`display::outputs`), in its coordinates - the primary monitor at 0,0 like
// Windows: Wine takes the output at 0,0 as the primary one and builds
// EnumDisplayMonitors from the logical positions (`zxdg_output_v1`). The
// table is looked up again every [`OUTPUT_RECHECK_MS`]; a monitor switched
// on or off in Settings -> Display adds or removes its global
// (`wl_registry.global` / `global_remove`), a moved or re-moded one sends
// new `geometry`/`mode`/logical position followed by `done`.

/// Registry names of output globals start here (the static ones are below).
const FIRST_OUTPUT_GLOBAL: u32 = 64;
const OUTPUT_RECHECK_MS: u64 = 250;
/// How often surfaces' monitors (`wl_surface.enter`/`leave`) are looked up.
const ENTER_RECHECK_MS: u64 = 300;

/// One monitor as clients see it.
#[derive(Clone, PartialEq, Eq)]
struct OutputInfo {
    global: u32,
    /// Driver output and monitor passport: the same monitor keeps its global.
    key: String,
    /// Registry coordinates: the primary monitor at 0,0.
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    mhz: u32,
    mm: (i32, i32),
    make: String,
    model: String,
    /// Connector-like name (`wl_output.name`, v4): `Virtual-1`, `DP-2`.
    name: String,
    description: String,
    primary: bool,
    /// 24.11: the number of the monitor — its place in the registry plus
    /// one, as «Параметры → Дисплей» and the «Определить» plates show it.
    number: u32,
}

impl OutputInfo {
    fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }
}

struct OutputTable {
    list: Vec<OutputInfo>,
    next_global: u32,
    /// Connector names handed out by key: a monitor switched off and on
    /// again keeps its name (its global is new).
    names: Vec<(String, String)>,
    checked_ms: u64,
    generation: u32,
}

static OUTPUT_TABLE: Mutex<OutputTable> =
    Mutex::new(OutputTable { list: Vec::new(), next_global: FIRST_OUTPUT_GLOBAL, names: Vec::new(), checked_ms: 0, generation: 0 });

/// Look the monitors up again (at most every [`OUTPUT_RECHECK_MS`]).
fn refresh_outputs(now: u64) {
    {
        let table = OUTPUT_TABLE.lock();
        if table.checked_ms != 0 && now.saturating_sub(table.checked_ms) < OUTPUT_RECHECK_MS {
            return;
        }
    }
    let registry = crate::display::outputs::list();
    let lit: Vec<&crate::display::outputs::Output> = registry.iter().filter(|o| o.enabled && o.current.is_some()).collect();
    // The registry keeps the primary at 0,0; normalised anyway.
    let origin = lit.iter().find(|o| o.primary).map_or((0, 0), |o| (o.x, o.y));
    let number_of = |o: &crate::display::outputs::Output| registry.iter().position(|r| core::ptr::eq(r, o)).map_or(1, |i| i as u32 + 1);
    let mut table = OUTPUT_TABLE.lock();
    table.checked_ms = now.max(1);
    let mut list: Vec<OutputInfo> = Vec::new();
    for o in &lit {
        let Some((w, h, mhz)) = o.current else { continue };
        let key = match &o.monitor {
            Some(m) => alloc::format!("{:?}/{}", o.backend, crate::display::outputs::monitor_key(m)),
            None => alloc::format!("{:?}", o.backend),
        };
        let name = match table.names.iter().find(|(k, _)| *k == key) {
            Some((_, name)) => name.clone(),
            None => {
                let (prefix, number) = match o.backend {
                    crate::display::outputs::Backend::Virtio(n) => ("Virtual", n + 1),
                    crate::display::outputs::Backend::Nvidia(_) => {
                        ("DP", table.names.iter().filter(|(_, n)| n.starts_with("DP-")).count() + 1)
                    }
                    crate::display::outputs::Backend::Firmware => ("FB", 1),
                };
                let mut name = alloc::format!("{prefix}-{number}");
                while table.names.iter().any(|(_, n)| *n == name) {
                    name.push('b');
                }
                table.names.push((key.clone(), name.clone()));
                name
            }
        };
        let (make, model, mm) = match &o.monitor {
            Some(m) => {
                let model = if m.name.is_empty() { alloc::format!("{:04X}", m.product) } else { m.name.clone() };
                let mm = if m.width_mm > 0 && m.height_mm > 0 { (m.width_mm as i32, m.height_mm as i32) } else { mm_at_96dpi(w, h) };
                (String::from(m.maker_str()), model, mm)
            }
            None => (String::from("FossAi"), o.name.clone(), mm_at_96dpi(w, h)),
        };
        let description = alloc::format!("{} {} ({name})", make, model);
        let global = table.list.iter().find(|old| old.key == key).map_or(0, |old| old.global);
        list.push(OutputInfo {
            global,
            key,
            x: o.x - origin.0,
            y: o.y - origin.1,
            w,
            h,
            mhz: if (10_000..=1_000_000).contains(&mhz) { mhz } else { 60_000 },
            mm,
            make,
            model,
            name,
            description,
            primary: o.primary,
            number: number_of(o),
        });
    }
    // No registry (one firmware screen): the primary screen as it is.
    if list.is_empty() {
        let (w, h) = crate::ui::screen_size();
        if w > 0 && h > 0 {
            let global = table.list.iter().find(|old| old.key == "screen").map_or(0, |old| old.global);
            list.push(OutputInfo {
                global,
                key: String::from("screen"),
                x: 0,
                y: 0,
                w,
                h,
                mhz: PACE_OUTPUT_MHZ.load(Ordering::Relaxed) as u32,
                mm: mm_at_96dpi(w, h),
                make: String::from("FossAi"),
                model: String::from("Screen"),
                name: String::from("FB-1"),
                description: String::from("FossAi Screen (FB-1)"),
                primary: true,
                number: 1,
            });
        }
    }
    for info in &mut list {
        if info.global == 0 {
            info.global = table.next_global;
            table.next_global += 1;
        }
    }
    if list != table.list {
        let lines: Vec<String> = list
            .iter()
            .map(|o| alloc::format!("{} '{}' {}x{} at {},{}{}", o.global, o.name, o.w, o.h, o.x, o.y, if o.primary { " primary" } else { "" }))
            .collect();
        kprintln!("  wayland     : outputs: {}", lines.join("; "));
        table.list = list;
        table.generation = table.generation.wrapping_add(1);
    }
}

/// Monitors as `read` of `/dev/fossai-capture` hands them (OBS «Захват
/// экрана FossAi», 24.11): one line per monitor, the primary first,
/// `x\ty\tw\th\tprimary\tname\tdescription\tnumber\n` - the place in
/// desktop (capture) coordinates, `primary` 1/0, the same connector name as
/// `wl_output.name` (what Qt and OBS call the screen) and (24.11) the number
/// «Параметры → Дисплей» and the «Определить» plates show (`read` of a
/// shell keeps it in the last variable with the description).
#[must_use]
pub fn monitor_lines() -> String {
    refresh_outputs(crate::time::uptime_ms());
    let table = OUTPUT_TABLE.lock();
    let left = table.list.iter().map(|o| o.x).min().unwrap_or(0);
    let top = table.list.iter().map(|o| o.y).min().unwrap_or(0);
    let mut order: Vec<&OutputInfo> = table.list.iter().collect();
    order.sort_by_key(|o| (!o.primary, o.x, o.y));
    let mut text = String::new();
    for o in order {
        text.push_str(&alloc::format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            o.x - left,
            o.y - top,
            o.w,
            o.h,
            u8::from(o.primary),
            o.name,
            o.description.replace(['\t', '\n'], " "),
            o.number
        ));
    }
    text
}

/// Physical size of a `w`x`h` screen at 96 dots per inch, mm.
fn mm_at_96dpi(w: u32, h: u32) -> (i32, i32) {
    ((w * 254 / 960) as i32, (h * 254 / 960) as i32)
}

/// The middle of the monitor of output global `global`, in desktop
/// coordinates (they start at the corner of the box around all monitors).
fn output_point(global: u32) -> Option<(i32, i32)> {
    let table = OUTPUT_TABLE.lock();
    let o = table.list.iter().find(|o| o.global == global)?;
    let left = table.list.iter().map(|o| o.x).min().unwrap_or(0);
    let top = table.list.iter().map(|o| o.y).min().unwrap_or(0);
    Some((o.x - left + o.w as i32 / 2, o.y - top + o.h as i32 / 2))
}

/// A window state request of `xdg_toplevel` (opcode: 9/10 maximize/unset,
/// 11 fullscreen - on the monitor at `at` if given, 12 unset, 13 minimize)
/// to the desktop. `true` - the desktop was busy: ask again.
fn window_op(pid: u32, slot: u32, op: u16, at: Option<(i32, i32)>) -> bool {
    let done = match op {
        9 => crate::ui::maximize_window(pid, slot, true),
        10 => crate::ui::maximize_window(pid, slot, false),
        11 => match at {
            Some(at) => crate::ui::fullscreen_window_at(pid, slot, at),
            None => crate::ui::fullscreen_window(pid, slot, true),
        },
        12 => crate::ui::fullscreen_window(pid, slot, false),
        13 => crate::ui::minimize_window(pid, slot),
        _ => Ok(()),
    };
    if let Err(err) = &done {
        kprintln!("  wayland     : #{pid} xdg_toplevel request {op}: desktop says {err:?}");
    }
    matches!(done, Err(crate::ui::WindowError::Busy))
}

/// The output table now: generation and monitors.
fn outputs_now() -> (u32, Vec<OutputInfo>) {
    let table = OUTPUT_TABLE.lock();
    (table.generation, table.list.clone())
}

/// `zwp_pointer_constraints_v1.lifetime`: `persistent` - the constraint
/// comes back each time its conditions hold again (oneshot ends for good).
const CONSTRAINT_PERSISTENT: u32 = 2;
/// `wp_fractional_scale_v1.preferred_scale`: 120 = 1.0.
const FRACTIONAL_ONE: u32 = 120;

/// A pointer constraint of a client (`zwp_locked_pointer_v1` or
/// `zwp_confined_pointer_v1`), G3.
struct PointerConstraint {
    id: u32,
    surface: u32,
    lock: bool,
    /// Region in surface coordinates (bounding box of the `wl_region`).
    region: Option<Rect>,
    persistent: bool,
    /// Asked of the desktop for the window (again after a focus change or a
    /// new region).
    asked: bool,
    /// `locked`/`confined` sent and not yet `unlocked`/`unconfined`.
    active: bool,
    /// Oneshot constraint that ended: inert until destroyed.
    defunct: bool,
}

/// `wp_tearing_control_v1.presentation_hint`: `async` — можно с разрывом.
const TEARING_ASYNC: u32 = 1;
/// `wp_presentation.clock_id`: время в `presented` — `CLOCK_MONOTONIC`
/// (`time::uptime_ns`, его же отдаёт `clock_gettime`).
const PRESENTATION_CLOCK: u32 = 1;

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
// Темп ответов на `wl_surface.frame` (P8): клиент в режиме FIFO (DXVK, Mesa
// WSI) рисует не чаще, чем стол показывает. До 20.11 — постоянные 16 мс
// (потолок ~62 к/с и на мониторе 144 Гц, hw-log12); теперь — кадр самого
// быстрого монитора ([`frame_us`]), `wlpace` меняет.
/// Дольше этого ответ на `frame` не откладывается, даже если окно ещё не
/// показано (стол занят): клиент не должен стоять.
const FRAME_HOLD_MAX_MS: u64 = 100;
/// Окно не закрылось за столько после просьбы — стол спросит «Программа не
/// отвечает» (P4, 18.11.2026; как `HUNG_AFTER_MS` у родных окон).
const CLOSE_GRACE_MS: u64 = 10_000;
/// `xdg_wm_base.ping` — раз в столько, пока у клиента есть окна…
const PING_EVERY_MS: u64 = 5_000;
/// …и столько ждать `pong`: дольше — клиент не разбирает свою очередь.
const PING_PATIENCE_MS: u64 = 5_000;
const XDG_TOPLEVEL_MAXIMIZED: u32 = 1;
const XDG_TOPLEVEL_FULLSCREEN: u32 = 2;
const XDG_TOPLEVEL_ACTIVATED: u32 = 4;
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Кнопка evdev по коду кнопки в событии окна: 1 левая, 2 правая, 4 средняя
/// (средняя приходит с 20.11).
const fn evdev_button(code: u32) -> u32 {
    match code {
        2 => BTN_RIGHT,
        4 => BTN_MIDDLE,
        _ => BTN_LEFT,
    }
}

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
    /// `area` — монитор, в пределах которого всплывать (G5, 21.11: тот, на
    /// котором привязка, — прежде размер основного от угла стола, и меню
    /// окна на правом мониторе уезжали на левый). Не влезающее
    /// переворачивается к другой стороне привязки, а если и так не влезает —
    /// сдвигается внутрь, если клиент это разрешил.
    fn constrain(&self, parent: (i32, i32), area: Rect) -> (i32, i32) {
        let (w, h) = (self.width.max(1), self.height.max(1));
        let (mut x, mut y) = self.place();
        let (left, top) = (area.x, area.y);
        let (right, bottom) = (area.x + area.w as i32, area.y + area.h as i32);
        let out_x = |x: i32| parent.0 + x < left || parent.0 + x + w > right;
        let out_y = |y: i32| parent.1 + y < top || parent.1 + y + h > bottom;
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
            x = (right - w).min(parent.0 + x).max(left) - parent.0;
        }
        if self.adjust & ADJUST_SLIDE_Y != 0 && out_y(y) {
            y = (bottom - h).min(parent.1 + y).max(top) - parent.1;
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
    /// `wp_viewport` поверхности: действующий и ждущий `commit`.
    view: View,
    view_next: Option<View>,
    /// `wp_tearing_control_v1` поверхности (одна на поверхность) и подсказка
    /// `async`: действующая и ждущая `commit` (D7).
    tearing: Option<u32>,
    tearing_async: bool,
    tearing_next: Option<bool>,
    /// `wp_presentation_feedback`, ждущие `commit`.
    feedback: Vec<u32>,
    /// Как клиент показывает кадры — уже сказано в журнал (0 — нет, 1 — с
    /// темпом `frame`, 2 — без него); сколько кадров подряд так же.
    told: u8,
    run: u8,
    run_unpaced: bool,
}

/// Кусок буфера и размер поверхности (`wp_viewport`, D4 20.11.2026): Wine
/// изображает режим игры ниже монитора буфером 1280x720 с
/// `set_destination(2560, 1440)` — раньше это игнорировалось, и кадр
/// ложился 1:1 в угол («четверть экрана», hw-log12).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct View {
    /// `set_source`: x, y, w, h в `wl_fixed` (24.8); `None` — весь буфер.
    src: Option<(i32, i32, i32, i32)>,
    /// `set_destination`: размер поверхности; `None` — по куску буфера.
    dst: Option<(u32, u32)>,
}

impl View {
    /// Кусок буфера `(x, y, w, h)` в точках буфера, обрезанный по нему.
    fn source(&self, buf: &Buffer) -> (u32, u32, u32, u32) {
        let Some((x, y, w, h)) = self.src else {
            return (0, 0, buf.width, buf.height);
        };
        let x = ((x.max(0) as u32) >> 8).min(buf.width.saturating_sub(1));
        let y = ((y.max(0) as u32) >> 8).min(buf.height.saturating_sub(1));
        let w = ((w.max(256) as u32 + 255) >> 8).clamp(1, buf.width - x);
        let h = ((h.max(256) as u32 + 255) >> 8).clamp(1, buf.height - y);
        (x, y, w, h)
    }

    /// Размер поверхности с этим буфером.
    fn size(&self, buf: &Buffer) -> (u32, u32) {
        match self.dst {
            Some((w, h)) => (w.clamp(1, 8192), h.clamp(1, 8192)),
            None => {
                let (_, _, w, h) = self.source(buf);
                (w, h)
            }
        }
    }
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
            view: View::default(),
            view_next: None,
            tearing: None,
            tearing_async: false,
            tearing_next: None,
            feedback: Vec::new(),
            told: 0,
            run: 0,
            run_unpaced: false,
        }
    }

    /// Размер поверхности: буфер через `wp_viewport`; без буфера — `None`.
    fn size(&self) -> Option<(u32, u32)> {
        self.shown.as_ref().map(|(_, buf)| self.view.size(buf))
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
    /// Когда стол попросил окно закрыться (мс работы системы); ноль — не
    /// просил или уже спрошено «не отвечает» (P4).
    close_asked: u64,
    /// `set_fullscreen` пришёл до того, как окно открылось (20.11): исполнить
    /// при открытии.
    want_full: bool,
    /// G5: the output global `set_fullscreen` named (Wine: the monitor the
    /// game is on in its coordinates) - fullscreen goes there.
    full_output: Option<u32>,
    /// G5: a state request (maximize, fullscreen, minimize) the busy desktop
    /// did not take yet: `xdg_toplevel` opcode and fullscreen monitor point.
    op_due: Option<(u16, Option<(i32, i32)>)>,
    /// Буфер, который окно стола показывает без копии (D4, 20.11.2026):
    /// пиксели окна — прямо память dma-buf клиента. Держит память живой и
    /// возвращается клиенту, когда окно перейдёт на другой.
    adopted: Option<(u32, Buffer)>,
    /// Собственные пиксели окна, отложенные на время показа без копии.
    stash: Option<Surface>,
    /// Когда окно последний раз показано (мкс) и до какого мига отложен
    /// показ кадра, пришедшего без темпа (`MAILBOX`/`IMMEDIATE`, D7): такие
    /// кадры показываются с частотой монитора, последний побеждает.
    presented_us: u64,
    hold_until_us: u64,
    /// 25.11: uptime (ms) of the first show the busy desktop refused in a
    /// row; 0 - the last show went through.
    busy_ms: u64,
    /// 25.11: since when (us) the show waits for the GPU frame fence; 0 -
    /// not waiting.
    fence_us: u64,
    /// G3: frame of the monitor the window is on (us) and when it was looked
    /// up (ms): pacing follows the window's own monitor.
    pace_us: u64,
    pace_at_ms: u64,
    /// 24.11 (WM SDK): `xdg_toplevel.set_min_size` / `set_max_size` in
    /// pixels, `(min, max)`; zero — no limit. `limits_due` — the desktop has
    /// not taken them yet (busy, or the window is not open).
    limits: ((u32, u32), (u32, u32)),
    limits_due: bool,
    /// 24.11 (Rupor): the desktop draws the frame (title band, buttons) -
    /// for clients that never draw one: rootful Xwayland ([`framed_app`]).
    framed: bool,
}

/// Clients whose toplevel gets the desktop's frame, by `xdg_toplevel.set_app_id`
/// (24.11, the Linux client of Rupor): rootful Xwayland draws no decorations
/// without `-decorate` (libdecor), and its window is a whole X screen - a
/// program window, not a client-side-decorated one like Chromium or Qt.
///
/// VLC (24.11, `vlc` - fossai-vlc sets it): Qt 5 on wl_shm alone draws no
/// decorations either (`QWaylandDisplay::supportsWindowDecoration` wants an
/// EGL buffer integration), and its window had no title bar to drag or close.
fn framed_app(app_id: &str) -> bool {
    matches!(app_id, "org.freedesktop.Xwayland" | "vlc")
}

/// Hand the size limits of a toplevel to the desktop. `true` — still due.
fn send_limits(pid: u32, win: &Win) -> bool {
    let (min, max) = win.limits;
    let limits = crate::ui::window::SizeLimits { min, max, logical: false };
    matches!(crate::ui::limit_window(pid, win.slot, limits), Err(crate::ui::WindowError::Busy))
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
    /// G5: where the parent (the window or a parent menu) is from that
    /// corner - `xdg_popup.reposition` places from it again.
    base: (i32, i32),
    /// G5: a new place not yet given to the desktop (it was busy).
    place_due: bool,
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
    /// `wl_output` bound to the global `global` (G5), with its version.
    Output { global: u32, version: u32 },
    /// `zxdg_output_manager_v1` and `zxdg_output_v1` of the `wl_output`
    /// object `output` (G5).
    XdgOutputManager(u32),
    XdgOutput { output: u32, version: u32 },
    DataDeviceManager,
    /// Источник данных клиента и форматы, которые он предложил.
    DataSource(Vec<String>),
    DataDevice,
    /// Предложение выделения, которое сервер прислал клиенту.
    DataOffer,
    /// Data control (24.11): `ext_data_control_manager_v1` /
    /// `zwlr_data_control_manager_v1` (`true` - the wlr one), its source with
    /// the offered types, device and offer.
    DcManager(bool),
    DcSource(Vec<String>),
    DcDevice,
    DcOffer,
    Viewporter,
    /// `wp_viewport` поверхности с этим номером.
    Viewport(u32),
    Dmabuf,
    DmabufParams(dmabuf::Params),
    /// `zwp_linux_dmabuf_feedback_v1` (версия 4, узел AMD).
    DmabufFeedback,
    TearingManager,
    /// `wp_tearing_control_v1` поверхности с этим номером.
    Tearing(u32),
    Presentation,
    Feedback,
    RelativeManager,
    RelativePointer,
    ConstraintsManager,
    /// `zwp_locked_pointer_v1` / `zwp_confined_pointer_v1` (G3).
    Locked,
    Confined,
    Activation,
    /// `xdg_activation_token_v1`.
    ActivationToken,
    FractionalManager,
    /// `wp_fractional_scale_v1` of this surface.
    Fractional(u32),
    IdleInhibitManager,
    /// `zwp_idle_inhibitor_v1` (22.11).
    IdleInhibitor,
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
            Self::Output { .. } => "wl_output",
            Self::XdgOutputManager(_) => "zxdg_output_manager_v1",
            Self::XdgOutput { .. } => "zxdg_output_v1",
            Self::DataDeviceManager => "wl_data_device_manager",
            Self::DataSource(_) => "wl_data_source",
            Self::DataDevice => "wl_data_device",
            Self::DataOffer => "wl_data_offer",
            Self::DcManager(false) => "ext_data_control_manager_v1",
            Self::DcManager(true) => "zwlr_data_control_manager_v1",
            Self::DcSource(_) => "ext_data_control_source_v1",
            Self::DcDevice => "ext_data_control_device_v1",
            Self::DcOffer => "ext_data_control_offer_v1",
            Self::Viewporter => "wp_viewporter",
            Self::Viewport(_) => "wp_viewport",
            Self::Dmabuf => "zwp_linux_dmabuf_v1",
            Self::DmabufParams(_) => "zwp_linux_buffer_params_v1",
            Self::DmabufFeedback => "zwp_linux_dmabuf_feedback_v1",
            Self::TearingManager => "wp_tearing_control_manager_v1",
            Self::Tearing(_) => "wp_tearing_control_v1",
            Self::Presentation => "wp_presentation",
            Self::Feedback => "wp_presentation_feedback",
            Self::RelativeManager => "zwp_relative_pointer_manager_v1",
            Self::RelativePointer => "zwp_relative_pointer_v1",
            Self::ConstraintsManager => "zwp_pointer_constraints_v1",
            Self::Locked => "zwp_locked_pointer_v1",
            Self::Confined => "zwp_confined_pointer_v1",
            Self::Activation => "xdg_activation_v1",
            Self::ActivationToken => "xdg_activation_token_v1",
            Self::FractionalManager => "wp_fractional_scale_manager_v1",
            Self::Fractional(_) => "wp_fractional_scale_v1",
            Self::IdleInhibitManager => "zwp_idle_inhibit_manager_v1",
            Self::IdleInhibitor => "zwp_idle_inhibitor_v1",
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
    /// The same for data-control devices (24.11): they hear every change,
    /// focus or not.
    control_seen: u32,
    /// Следующий номер объекта, созданного сервером (`wl_data_offer`):
    /// сервер нумерует свои с `0xff000000`.
    next_server_id: u32,
    /// Нажатые клавиши (коды evdev): повтор ядра отличается от нажатия.
    keys_down: Vec<u32>,
    seat_version: u32,
    /// G5: version of the client's `xdg_wm_base` (`wm_capabilities` from 5).
    wm_version: u32,
    broken: bool,
    /// `xdg_wm_base` клиента — ему идут `ping` (P4).
    wm_base: Option<u32>,
    /// Посланный и ещё не отвеченный `ping`: номер и когда послан.
    ping: Option<(u32, u64)>,
    /// Когда послан последний `ping`.
    ping_sent: u64,
    /// О молчании на `ping` уже сказано в журнале.
    ping_silence_told: bool,
    /// Когда клиенту последний раз ответили на `frame` (темп, P8; мкс) и с
    /// какого мига ответ отложен (мс).
    frame_answered_us: u64,
    frames_held_since: u64,
    /// `wp_presentation_feedback` после `commit`: (объект, поверхность,
    /// поверхность окна) — `presented`, когда окно показано, `discarded`, если
    /// кадр сменили раньше (D7).
    feedbacks: Vec<(u32, u32, u32)>,
    /// Счётчик показов для `presented` (`seq`).
    present_seq: u64,
    /// G3: bounding boxes of the client's `wl_region`s (surface coordinates).
    regions: BTreeMap<u32, Rect>,
    /// G3: pointer constraints (`zwp_pointer_constraints_v1`).
    constraints: Vec<PointerConstraint>,
    /// G3: frame pace of this client (us): the refresh of the monitor its
    /// paced window is on, not of the fastest monitor; 0 - not known yet.
    pace_us: u64,
    /// G3: activation tokens handed out.
    tokens: u32,
    /// G3: buffers replaced on screen that a display may still scan out (a
    /// page flip not on screen yet): `wl_buffer.release` waits for it.
    scan_held: Vec<(u32, Buffer)>,
    /// G5: the monitors as last told to this client (globals announced and
    /// what the bound `wl_output`s were sent), and the table generation.
    outputs_told: Vec<OutputInfo>,
    outputs_gen: u32,
    /// G5: `wl_surface.enter` sent and not left: (surface, `wl_output`
    /// object); when the monitors under surfaces were last looked up (ms).
    entered: Vec<(u32, u32)>,
    enter_checked_ms: u64,
}

/// A client gone with idle inhibitors alive: they end with it (22.11).
impl Drop for Client {
    fn drop(&mut self) {
        for _ in self.objects.values().filter(|obj| matches!(obj, Obj::IdleInhibitor)) {
            crate::power::inhibit(false);
        }
    }
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
            control_seen: SELECTION_GEN.load(Ordering::Acquire),
            next_server_id: 0xff00_0000,
            keys_down: Vec::new(),
            seat_version: 1,
            wm_version: 1,
            broken: false,
            wm_base: None,
            ping: None,
            ping_sent: 0,
            ping_silence_told: false,
            frame_answered_us: 0,
            frames_held_since: 0,
            feedbacks: Vec::new(),
            present_seq: 0,
            regions: BTreeMap::new(),
            constraints: Vec::new(),
            pace_us: 0,
            tokens: 0,
            scan_held: Vec::new(),
            outputs_told: Vec::new(),
            outputs_gen: 0,
            entered: Vec::new(),
            enter_checked_ms: 0,
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
        self.release_scanned();
        self.answer_frames(now);
        // G5: before requests, so a registry made now lists what is current.
        self.sync_outputs();
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
            self.sync_control();
            self.serve_owner();
            self.window_events(time);
            self.popup_events(time);
            if !self.constraints.is_empty() {
                self.sync_constraints();
            }
            self.sync_enter(now);
            let now_us = crate::time::uptime_ns() / 1000;
            for index in 0..self.wins.len() {
                // Отложенный кадр без темпа (D7) — к своему сроку.
                if self.wins[index].dirty && now_us >= self.wins[index].hold_until_us {
                    self.present(index);
                }
                let win = &mut self.wins[index];
                if let (Some((op, at)), Some(_)) = (win.op_due, win.open) {
                    win.op_due = window_op(self.pid, win.slot, op, at).then_some((op, at));
                }
                if win.title_due && win.open.is_some() {
                    win.title_due = crate::ui::title_window(self.pid, win.slot, &win.title).is_err();
                }
                if win.limits_due && win.open.is_some() {
                    win.limits_due = send_limits(self.pid, win);
                }
            }
            for index in 0..self.pops.len() {
                if self.pops[index].dirty {
                    self.present_popup(index);
                }
            }
            self.watch_answers(now);
            // Кадры этого витка показаны — ответить, но не чаще кадра
            // экрана (P8).
            self.answer_frames(now);
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
                        let version = if name == 9 { dmabuf::version() } else { version };
                        self.send(Msg::new(registry, 0).u32(name).str(interface).u32(version));
                    }
                    let outputs: Vec<u32> = self.outputs_told.iter().map(|o| o.global).collect();
                    for name in outputs {
                        self.send(Msg::new(registry, 0).u32(name).str("wl_output").u32(4));
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
            // `destroy`, `get_tearing_control(id, surface)` (D7).
            Obj::TearingManager => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    let surface = args.u32()?;
                    match self.objects.get_mut(&surface) {
                        Some(Obj::Surface(state)) if state.tearing.is_some_and(|other| other != new) => {
                            self.fail(id, 0, "the surface already has a tearing control");
                            return Ok(());
                        }
                        Some(Obj::Surface(state)) => state.tearing = Some(new),
                        _ => {}
                    }
                    self.objects.insert(new, Obj::Tearing(surface));
                }
                _ => {}
            },
            // `set_presentation_hint(hint)` (с `commit`), `destroy` — снова
            // `vsync`.
            Obj::Tearing(surface) => {
                let surface = *surface;
                let hint = match opcode {
                    0 => args.u32()? == TEARING_ASYNC,
                    1 => {
                        self.delete(id);
                        if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                            state.tearing = None;
                        }
                        false
                    }
                    _ => return Ok(()),
                };
                if let Some(Obj::Surface(state)) = self.objects.get_mut(&surface) {
                    state.tearing_next = Some(hint);
                }
            }
            // `destroy`, `feedback(surface, callback)`: ответ — после показа
            // кадра следующего `commit` этой поверхности.
            Obj::Presentation => match opcode {
                0 => self.delete(id),
                1 => {
                    let surface = args.u32()?;
                    let callback = args.u32()?;
                    self.objects.insert(callback, Obj::Feedback);
                    match self.objects.get_mut(&surface) {
                        Some(Obj::Surface(state)) => state.feedback.push(callback),
                        _ => self.feedback_end(callback, None),
                    }
                }
                _ => {}
            },
            Obj::Feedback => {}
            // G3: `wl_region.add(x, y, w, h)` / `subtract` - kept as the
            // bounding box of what was added (pointer confinement needs no
            // more: games confine to one rectangle).
            Obj::Region if opcode == 1 || opcode == 2 => {
                let (x, y, w, h) = (args.i32()?, args.i32()?, args.i32()?, args.i32()?);
                if opcode == 1 && w > 0 && h > 0 {
                    let rect = Rect::new(x, y, w as u32, h as u32);
                    let next = self.regions.get(&id).map_or(rect, |old| old.union(&rect));
                    self.regions.insert(id, next);
                }
            }
            Obj::Region if opcode == 0 => {
                self.regions.remove(&id);
                self.delete(id);
            }
            // G3: `destroy`, `get_relative_pointer(id, pointer)`.
            Obj::RelativeManager => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::RelativePointer);
                }
                _ => {}
            },
            Obj::RelativePointer => {
                if opcode == 0 {
                    self.delete(id);
                }
            }
            // G3: `destroy`, `lock_pointer` / `confine_pointer(id, surface,
            // pointer, region, lifetime)`.
            Obj::ConstraintsManager => match opcode {
                0 => self.delete(id),
                1 | 2 => {
                    let (new, surface, _pointer, region, lifetime) = (args.u32()?, args.u32()?, args.u32()?, args.u32()?, args.u32()?);
                    if self.constraints.iter().any(|c| c.surface == surface) {
                        self.fail(id, 1, "the surface already has a pointer constraint");
                        return Ok(());
                    }
                    let lock = opcode == 1;
                    self.objects.insert(new, if lock { Obj::Locked } else { Obj::Confined });
                    let region = (region != 0).then(|| self.regions.get(&region).copied().unwrap_or(Rect::EMPTY));
                    kprintln!(
                        "  wayland     : #{} asks to {} the pointer on surface {surface}{}",
                        self.pid,
                        if lock { "lock" } else { "confine" },
                        if lifetime == CONSTRAINT_PERSISTENT { " (persistent)" } else { "" }
                    );
                    self.constraints.push(PointerConstraint {
                        id: new,
                        surface,
                        lock,
                        region,
                        persistent: lifetime == CONSTRAINT_PERSISTENT,
                        asked: false,
                        active: false,
                        defunct: false,
                    });
                    self.sync_constraints();
                }
                _ => {}
            },
            // `zwp_locked_pointer_v1`: `destroy`, `set_cursor_position_hint(x,
            // y)`, `set_region(region)`; `zwp_confined_pointer_v1`:
            // `destroy`, `set_region(region)`.
            Obj::Locked | Obj::Confined => {
                let lock = matches!(object, Obj::Locked);
                match (lock, opcode) {
                    (_, 0) => {
                        self.drop_constraint(id);
                        self.delete(id);
                    }
                    (true, 1) => {
                        let (x, y) = (args.i32()? >> 8, args.i32()? >> 8);
                        if let Some((slot, (gx, gy))) = self.constraint_window(id) {
                            crate::ui::constraint_hint(self.pid, slot, x - gx, y - gy);
                        }
                    }
                    (true, 2) | (false, 1) => {
                        let region = args.u32()?;
                        let region = (region != 0).then(|| self.regions.get(&region).copied().unwrap_or(Rect::EMPTY));
                        if let Some(c) = self.constraints.iter_mut().find(|c| c.id == id) {
                            c.region = region;
                            c.asked = false;
                        }
                        self.sync_constraints();
                    }
                    _ => {}
                }
            }
            // G3: `destroy`, `get_activation_token(id)`, `activate(token,
            // surface)`.
            Obj::Activation => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::ActivationToken);
                }
                2 => {
                    let _token = args.string()?;
                    let surface = args.u32()?;
                    let root = self.root_of(surface);
                    if let Some(slot) = self.wins.iter().find(|win| win.surface == root && win.open.is_some()).map(|win| win.slot) {
                        let _ = crate::ui::activate_window(self.pid, slot);
                    }
                }
                _ => {}
            },
            // `set_serial`, `set_app_id`, `set_surface`, `commit` (-> `done`
            // with the token), `destroy`.
            Obj::ActivationToken => match opcode {
                3 => {
                    self.tokens = self.tokens.wrapping_add(1);
                    let token = alloc::format!("fossai-{}-{}", self.pid, self.tokens);
                    self.send(Msg::new(id, 0).str(&token));
                }
                4 => self.delete(id),
                _ => {}
            },
            // G3: `destroy`, `get_fractional_scale(id, surface)`: the scale is
            // 1.0 - surface coordinates are screen pixels here.
            Obj::FractionalManager => match opcode {
                0 => self.delete(id),
                1 => {
                    let (new, surface) = (args.u32()?, args.u32()?);
                    if self.objects.values().any(|obj| matches!(obj, Obj::Fractional(of) if *of == surface)) {
                        self.fail(id, 0, "the surface already has a fractional scale");
                        return Ok(());
                    }
                    self.objects.insert(new, Obj::Fractional(surface));
                    self.send(Msg::new(new, 0).u32(FRACTIONAL_ONE));
                }
                _ => {}
            },
            Obj::Fractional(_) => {
                if opcode == 0 {
                    self.delete(id);
                }
            }
            // 22.11: `destroy`, `create_inhibitor(id, surface)`; the
            // inhibitor's only request is `destroy`.
            Obj::IdleInhibitManager => match opcode {
                0 => self.delete(id),
                1 => {
                    let (new, _surface) = (args.u32()?, args.u32()?);
                    self.objects.insert(new, Obj::IdleInhibitor);
                    crate::power::inhibit(true);
                    kprintln!("  wayland     : client #{} inhibits the screensaver", self.pid);
                }
                _ => {}
            },
            Obj::IdleInhibitor => {
                if opcode == 0 {
                    self.delete(id);
                    crate::power::inhibit(false);
                    kprintln!("  wayland     : client #{} lets the screensaver run again", self.pid);
                }
            }
            Obj::Callback | Obj::Region | Obj::Output { .. } | Obj::Keyboard => {
                // `destroy`/`release` у тех, у кого он есть, — нулевой.
                let destroys = match object {
                    Obj::Region => opcode == 0,
                    Obj::Output { .. } | Obj::Keyboard => opcode == 0,
                    _ => false,
                };
                if destroys {
                    self.entered.retain(|&(_, output)| output != id);
                    self.delete(id);
                }
            }
            // G5: `destroy`, `get_xdg_output(id, output)`.
            Obj::XdgOutputManager(version) => {
                let version = *version;
                match opcode {
                    0 => self.delete(id),
                    1 => {
                        let (new, output) = (args.u32()?, args.u32()?);
                        self.objects.insert(new, Obj::XdgOutput { output, version });
                        let info = match self.objects.get(&output) {
                            Some(Obj::Output { global, .. }) => self.outputs_told.iter().find(|o| o.global == *global).cloned(),
                            _ => None,
                        };
                        if let Some(info) = info {
                            self.send_xdg_output(new, output, version, &info, true);
                        }
                    }
                    _ => {}
                }
            }
            Obj::XdgOutput { .. } => {
                if opcode == 0 {
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
                    self.receive_selection(mime, fd);
                }
                2 => self.delete(id),
                _ => {}
            },
            // Data control (24.11): `create_data_source(id)`,
            // `get_data_device(id, seat)`, `destroy`.
            Obj::DcManager(_) => match opcode {
                0 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::DcSource(Vec::new()));
                }
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::DcDevice);
                    // The current selection on the next round.
                    self.control_seen = SELECTION_GEN.load(Ordering::Acquire).wrapping_sub(1);
                }
                _ => self.delete(id),
            },
            // `offer(mime)`, `destroy`.
            Obj::DcSource(_) => match opcode {
                0 => {
                    let mime = args.string()?;
                    if let Some(Obj::DcSource(mimes)) = self.objects.get_mut(&id) {
                        if mimes.len() < 32 && !mimes.contains(&mime) {
                            mimes.push(mime);
                        }
                    }
                }
                _ => {
                    self.forget_source(id);
                    self.delete(id);
                }
            },
            // `set_selection(source or null)`, `destroy`,
            // `set_primary_selection` (no primary selection here).
            Obj::DcDevice => match opcode {
                0 => {
                    let source = args.u32()?;
                    self.set_selection(source);
                }
                1 => self.delete(id),
                _ => {}
            },
            // `receive(mime, fd)`, `destroy`.
            Obj::DcOffer => match opcode {
                0 => {
                    let mime = args.string()?;
                    let Some(fd) = self.fds.pop_front() else {
                        self.fail(id, 2, "receive without a descriptor");
                        return Ok(());
                    };
                    self.receive_selection(mime, fd);
                }
                _ => self.delete(id),
            },
            // `destroy`, `get_viewport(id, surface)`.
            Obj::Viewporter => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    let surface = args.u32()?;
                    self.objects.insert(new, Obj::Viewport(surface));
                }
                _ => {}
            },
            // `destroy`, `set_source(x, y, w, h)` (`wl_fixed`, все -1 — снять),
            // `set_destination(w, h)` (-1, -1 — снять). Действуют с `commit`
            // поверхности (D4, 20.11.2026).
            Obj::Viewport(surface) => {
                let surface = *surface;
                let current = match self.objects.get(&surface) {
                    Some(Obj::Surface(state)) => state.view_next.unwrap_or(state.view),
                    _ => View::default(),
                };
                let next = match opcode {
                    0 => {
                        self.delete(id);
                        Some(View::default())
                    }
                    1 => {
                        let (x, y, w, h) = (args.i32()?, args.i32()?, args.i32()?, args.i32()?);
                        let src = if (x, y, w, h) == (-256, -256, -256, -256) {
                            None
                        } else if x < 0 || y < 0 || w <= 0 || h <= 0 {
                            self.fail(id, 0, "bad source rectangle");
                            return Ok(());
                        } else {
                            Some((x, y, w, h))
                        };
                        Some(View { src, ..current })
                    }
                    2 => {
                        let (w, h) = (args.i32()?, args.i32()?);
                        let dst = if (w, h) == (-1, -1) {
                            None
                        } else if w <= 0 || h <= 0 {
                            self.fail(id, 0, "bad destination size");
                            return Ok(());
                        } else {
                            Some((w as u32, h as u32))
                        };
                        Some(View { dst, ..current })
                    }
                    _ => None,
                };
                if let (Some(next), Some(Obj::Surface(state))) = (next, self.objects.get_mut(&surface)) {
                    state.view_next = Some(next);
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
            // `destroy`, `create_params(id)`.
            Obj::Dmabuf => match opcode {
                0 => self.delete(id),
                1 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::DmabufParams(dmabuf::Params::default()));
                }
                // `get_default_feedback(id)` и `get_surface_feedback(id, surface)` (версия 4).
                2 | 3 => {
                    let new = args.u32()?;
                    self.objects.insert(new, Obj::DmabufFeedback);
                    self.dmabuf_feedback(new);
                }
                _ => {}
            },
            Obj::DmabufFeedback => {
                if opcode == 0 {
                    self.delete(id);
                }
            }
            // `destroy`, `add(fd, plane, offset, stride, mod_hi, mod_lo)`,
            // `create(w, h, format, flags)`, `create_immed(id, w, h, format, flags)`.
            Obj::DmabufParams(_) => match opcode {
                0 => self.delete(id),
                1 => {
                    let Some(fd) = self.fds.pop_front() else {
                        self.fail(id, dmabuf::ERR_INCOMPLETE, "add without a descriptor");
                        return Ok(());
                    };
                    let (plane, offset, stride) = (args.u32()?, args.u32()?, args.u32()?);
                    let modifier = (u64::from(args.u32()?) << 32) | u64::from(args.u32()?);
                    let added = match self.objects.get_mut(&id) {
                        Some(Obj::DmabufParams(params)) => params.add(&fd, plane, offset, stride, modifier),
                        _ => Ok(()),
                    };
                    if let Err(dmabuf::Fail(code, text)) = added {
                        self.fail(id, code, text);
                    }
                }
                2 | 3 => {
                    let new = if opcode == 3 { Some(args.u32()?) } else { None };
                    let (width, height, format, flags) = (args.i32()?, args.i32()?, args.u32()?, args.u32()?);
                    let made = match self.objects.get_mut(&id) {
                        Some(Obj::DmabufParams(params)) => params.create(width, height, format, flags),
                        _ => return Ok(()),
                    };
                    if let Ok(buffer) = &made {
                        kprintln!(
                            "  wayland     : client #{} dmabuf buffer {}x{} stride {} format {:#x}",
                            self.pid,
                            buffer.width,
                            buffer.height,
                            buffer.stride,
                            format
                        );
                    }
                    match (made, new) {
                        (Ok(buffer), Some(new)) => {
                            self.objects.insert(new, Obj::Buffer(buffer));
                        }
                        // `create`: буфер с номером сервера и событие `created`.
                        (Ok(buffer), None) => {
                            let server = self.next_server_id;
                            self.next_server_id = self.next_server_id.wrapping_add(1).max(0xff00_0000);
                            self.objects.insert(server, Obj::Buffer(buffer));
                            self.send(Msg::new(id, 0).u32(server));
                        }
                        (Err(dmabuf::Fail(code, text)), None) if dmabuf::soft_failure(code) => {
                            kprintln!("  wayland     : client #{} dmabuf import failed: {text}", self.pid);
                            self.send(Msg::new(id, 1));
                        }
                        (Err(dmabuf::Fail(code, text)), _) => self.fail(id, code, text),
                    }
                }
                _ => {}
            },
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
                // `pong(serial)` (P4).
                3 => {
                    let serial = args.u32()?;
                    if self.ping.is_some_and(|(sent, _)| sent == serial) {
                        if self.ping_silence_told {
                            kprintln!("  wayland     : client #{} answers ping again", self.pid);
                        }
                        self.ping = None;
                        self.ping_silence_told = false;
                    }
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
                    // 11/12 — `set_fullscreen`/`unset_fullscreen` (20.11):
                    // настоящий полноэкранный режим — весь монитор, панель не
                    // поверх, в `configure` состояние FULLSCREEN (прежде — как
                    // «развернуть», и Wine, не видя состояния, крутил set/unset).
                    // До открытия окна — запомнить.
                    11 | 12 if !self.wins.iter().any(|win| win.toplevel == id && win.open.is_some()) => {
                        let output = if opcode == 11 { self.output_global(args.u32()?) } else { None };
                        if let Some(win) = self.wins.iter_mut().find(|win| win.toplevel == id) {
                            win.want_full = opcode == 11;
                            win.full_output = output;
                        }
                    }
                    // 24.11 (WM SDK): `set_max_size` (7) / `set_min_size` (8)
                    // — the desktop keeps the window inside them on resize,
                    // snap and maximize. Before the window opens — kept and
                    // handed over when it opens.
                    7 | 8 => {
                        let (w, h) = (args.i32()?.max(0) as u32, args.i32()?.max(0) as u32);
                        let pid = self.pid;
                        if let Some(win) = self.wins.iter_mut().find(|win| win.toplevel == id) {
                            let changed = if opcode == 7 {
                                core::mem::replace(&mut win.limits.1, (w, h)) != (w, h)
                            } else {
                                core::mem::replace(&mut win.limits.0, (w, h)) != (w, h)
                            };
                            if changed || win.limits_due {
                                win.limits_due = win.open.is_none() || send_limits(pid, win);
                            }
                        }
                    }
                    // G5: `set_fullscreen(output)` - on the monitor asked
                    // (Wine: the one the game is on in its coordinates).
                    11 => {
                        let output = self.output_global(args.u32()?);
                        let pid = self.pid;
                        if let Some(win) = self.wins.iter_mut().find(|win| win.toplevel == id && win.open.is_some()) {
                            let at = output.and_then(output_point);
                            win.op_due = window_op(pid, win.slot, 11, at).then_some((11, at));
                        }
                    }
                    // `set_app_id` (24.11): before the window opens - whether
                    // the desktop frames it.
                    3 => {
                        let app_id = args.string()?;
                        if let Some(win) = self.wins.iter_mut().find(|win| win.toplevel == id && win.open.is_none()) {
                            win.framed = framed_app(&app_id);
                        }
                    }
                    5 | 6 | 9 | 10 | 12 | 13 => {
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
                        let pid = self.pid;
                        if let Some(win) = self.wins.iter_mut().find(|win| win.toplevel == id && win.open.is_some()) {
                            let slot = win.slot;
                            match opcode {
                                5 => {
                                    let _ = crate::ui::begin_move(pid, slot);
                                }
                                6 => {
                                    let _ = crate::ui::begin_resize(pid, slot, edges);
                                }
                                // G5 (22.11): the desktop busy (a flight, a
                                // frame) - asked again on the next rounds, not
                                // lost (a second set_minimized right after a
                                // restore was dropped in `wayland-multihead`).
                                _ => win.op_due = window_op(pid, slot, opcode, None).then_some((opcode, None)),
                            }
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
                2 => {
                    let (positioner, token) = (args.u32()?, args.u32()?);
                    self.reposition_popup(id, positioner, token);
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
            Obj::Pointer => match opcode {
                // G3: `set_cursor(serial, surface, hx, hy)`: a null surface
                // hides the pointer over the window (games drawing their own
                // cursor - then a flipped output needs no cursor at all); a
                // surface shows ours again (cursor images are not taken yet).
                0 => {
                    let _serial = args.u32()?;
                    let surface = args.u32()?;
                    let root = self.pointer_on.map(|on| self.root_of(on));
                    if let Some(win) = root.and_then(|root| self.wins.iter().find(|win| win.surface == root && win.open.is_some())) {
                        crate::ui::client_cursor(self.pid, win.slot, surface == 0);
                    }
                }
                1 => self.delete(id),
                _ => {}
            },
        }
        Ok(())
    }

    fn bind(&mut self, name: u32, interface: &str, version: u32, new: u32) {
        // G5: a monitor. One removed meanwhile (its `global_remove` still on
        // the way) gets an inert object, as the protocol asks.
        if name >= FIRST_OUTPUT_GLOBAL {
            if interface != "wl_output" {
                self.fail(1, 0, "interface does not match the global");
                return;
            }
            let version = version.clamp(1, 4);
            self.objects.insert(new, Obj::Output { global: name, version });
            if let Some(info) = self.outputs_told.iter().find(|o| o.global == name).cloned() {
                self.send_output(new, version, &info, true);
                // Already on screen: its surfaces are on this monitor too.
                self.enter_checked_ms = 0;
            }
            return;
        }
        let Some((_, global, ours)) = GLOBALS.iter().find(|(number, _, _)| *number == name) else {
            self.fail(1, 0, "no such global");
            return;
        };
        if *global != interface {
            self.fail(1, 0, "interface does not match the global");
            return;
        }
        let ours = if name == 9 { dmabuf::version() } else { *ours };
        let version = version.min(ours);
        let object = match name {
            1 => Obj::Compositor,
            2 => Obj::Subcompositor,
            3 => Obj::Shm,
            4 => Obj::WmBase,
            5 => Obj::Seat,
            7 => Obj::DataDeviceManager,
            8 => Obj::Viewporter,
            9 => Obj::Dmabuf,
            10 => Obj::TearingManager,
            11 => Obj::Presentation,
            12 => Obj::RelativeManager,
            13 => Obj::ConstraintsManager,
            14 => Obj::Activation,
            15 => Obj::FractionalManager,
            16 => Obj::IdleInhibitManager,
            18 => Obj::DcManager(false),
            19 => Obj::DcManager(true),
            _ => Obj::XdgOutputManager(version),
        };
        self.objects.insert(new, object);
        if name == 4 {
            self.wm_base = Some(new);
            self.wm_version = version;
        }
        match name {
            11 => self.send(Msg::new(new, 0).u32(PRESENTATION_CLOCK)),
            3 => {
                self.send(Msg::new(new, 0).u32(SHM_ARGB8888));
                self.send(Msg::new(new, 0).u32(SHM_XRGB8888));
            }
            // Версия 3: `format` (устаревшее, но его ждут старые клиенты) и
            // `modifier` — только LINEAR.
            9 => {
                // С версии 4 форматы приходят только обратной связью (`get_default_feedback`).
                for (format, _) in dmabuf::FORMATS.into_iter().filter(|_| version < 4) {
                    self.send(Msg::new(new, 0).u32(format));
                    if version >= 3 {
                        let linear = dmabuf::MOD_LINEAR;
                        self.send(Msg::new(new, 1).u32(format).u32((linear >> 32) as u32).u32(linear as u32));
                    }
                }
            }
            5 => {
                self.seat_version = version;
                self.send(Msg::new(new, 0).u32(SEAT_POINTER | SEAT_KEYBOARD));
                if version >= 2 {
                    self.send(Msg::new(new, 1).str("seat0"));
                }
            }
            _ => {}
        }
    }

    /// G5: `wl_output` events of a monitor - `geometry` (place in the
    /// registry's desktop, size in mm and make/model from EDID), `mode`
    /// (current + preferred, the real refresh: Wine and DXVK see 144 Hz),
    /// `scale` 1, and on bind `name`/`description` (v4); then `done`
    /// unless an `zxdg_output_v1` update follows.
    fn send_output(&mut self, id: u32, version: u32, info: &OutputInfo, first: bool) {
        self.send(
            Msg::new(id, 0)
                .i32(info.x)
                .i32(info.y)
                .i32(info.mm.0)
                .i32(info.mm.1)
                .i32(0)
                .str(&info.make)
                .str(&info.model)
                .i32(0),
        );
        self.send(Msg::new(id, 1).u32(3).i32(info.w as i32).i32(info.h as i32).i32(info.mhz as i32));
        if version >= 2 {
            self.send(Msg::new(id, 3).i32(1));
        }
        if version >= 4 {
            if first {
                self.send(Msg::new(id, 4).str(&info.name));
            }
            self.send(Msg::new(id, 5).str(&info.description));
        }
        if version >= 2 {
            self.send(Msg::new(id, 2));
        }
    }

    /// G5: `zxdg_output_v1` events - logical position and size (scale 1:
    /// the mode), `name`/`description` (v2), then `done` (v1/v2) or, from
    /// v3, `wl_output.done` of its output (`done_too`).
    fn send_xdg_output(&mut self, id: u32, output: u32, version: u32, info: &OutputInfo, done_too: bool) {
        self.send(Msg::new(id, 0).i32(info.x).i32(info.y));
        self.send(Msg::new(id, 1).i32(info.w as i32).i32(info.h as i32));
        if version >= 2 {
            self.send(Msg::new(id, 3).str(&info.name));
            self.send(Msg::new(id, 4).str(&info.description));
        }
        if version < 3 {
            self.send(Msg::new(id, 2));
        } else if done_too && matches!(self.objects.get(&output), Some(Obj::Output { version, .. }) if *version >= 2) {
            self.send(Msg::new(output, 2));
        }
    }

    /// The output global of a `wl_output` object of this client (0 - none).
    fn output_global(&self, object: u32) -> Option<u32> {
        match self.objects.get(&object) {
            Some(Obj::Output { global, .. }) => Some(*global),
            _ => None,
        }
    }

    /// G5: tell the client what changed in the monitors - new globals,
    /// removed globals, and new geometry/mode/position of bound outputs.
    fn sync_outputs(&mut self) {
        let generation = OUTPUT_TABLE.lock().generation;
        if generation == self.outputs_gen {
            return;
        }
        let (generation, now) = outputs_now();
        self.outputs_gen = generation;
        let old = core::mem::replace(&mut self.outputs_told, now.clone());
        let registries: Vec<u32> = self.objects.iter().filter(|(_, obj)| matches!(obj, Obj::Registry)).map(|(id, _)| *id).collect();
        for gone in old.iter().filter(|o| !now.iter().any(|n| n.global == o.global)) {
            for &registry in &registries {
                self.send(Msg::new(registry, 1).u32(gone.global));
            }
        }
        for added in now.iter().filter(|n| !old.iter().any(|o| o.global == n.global)) {
            for &registry in &registries {
                self.send(Msg::new(registry, 0).u32(added.global).str("wl_output").u32(4));
            }
        }
        // Bound outputs whose monitor moved or changed mode.
        let bound: Vec<(u32, u32, u32)> = self
            .objects
            .iter()
            .filter_map(|(id, obj)| match obj {
                Obj::Output { global, version } => Some((*id, *global, *version)),
                _ => None,
            })
            .collect();
        for (id, global, version) in bound {
            let (Some(before), Some(after)) = (old.iter().find(|o| o.global == global), now.iter().find(|n| n.global == global)) else {
                continue;
            };
            if before == after {
                continue;
            }
            let after = after.clone();
            let xdg: Vec<(u32, u32)> = self
                .objects
                .iter()
                .filter_map(|(xid, obj)| match obj {
                    Obj::XdgOutput { output, version } if *output == id => Some((*xid, *version)),
                    _ => None,
                })
                .collect();
            // `done` once, after the xdg_output events.
            self.send(
                Msg::new(id, 0)
                    .i32(after.x)
                    .i32(after.y)
                    .i32(after.mm.0)
                    .i32(after.mm.1)
                    .i32(0)
                    .str(&after.make)
                    .str(&after.model)
                    .i32(0),
            );
            self.send(Msg::new(id, 1).u32(3).i32(after.w as i32).i32(after.h as i32).i32(after.mhz as i32));
            if version >= 4 && before.description != after.description {
                self.send(Msg::new(id, 5).str(&after.description));
            }
            for (xid, xversion) in xdg {
                self.send_xdg_output(xid, id, xversion, &after, false);
            }
            if version >= 2 {
                self.send(Msg::new(id, 2));
            }
        }
        // Surfaces may be on other monitors now.
        self.enter_checked_ms = 0;
    }

    /// G5: `wl_surface.enter`/`leave` - every surface of a window (and of a
    /// menu) is on the monitors its window overlaps, looked up every
    /// [`ENTER_RECHECK_MS`] (windows are dragged between monitors).
    fn sync_enter(&mut self, now: u64) {
        if self.enter_checked_ms != 0 && now.saturating_sub(self.enter_checked_ms) < ENTER_RECHECK_MS {
            return;
        }
        let outputs: Vec<(u32, u32)> = self
            .objects
            .iter()
            .filter_map(|(id, obj)| match obj {
                Obj::Output { global, .. } => Some((*id, *global)),
                _ => None,
            })
            .collect();
        if outputs.is_empty() && self.entered.is_empty() {
            self.enter_checked_ms = now.max(1);
            return;
        }
        // Desktop coordinates start at the corner of the box around all
        // monitors; the table's at the primary monitor.
        let left = self.outputs_told.iter().map(|o| o.x).min().unwrap_or(0);
        let top = self.outputs_told.iter().map(|o| o.y).min().unwrap_or(0);
        let mut roots: Vec<(u32, u32)> = self.wins.iter().filter(|win| win.open.is_some()).map(|win| (win.surface, win.slot)).collect();
        roots.extend(self.pops.iter().filter(|pop| pop.open.is_some()).map(|pop| (pop.surface, pop.slot)));
        let mut want: Vec<(u32, u32)> = Vec::new();
        let mut root_ids: Vec<u32> = Vec::new();
        for (root, slot) in roots {
            root_ids.push(root);
            let mut tree = vec![root];
            let mut at = 0;
            while at < tree.len() && tree.len() < 64 {
                if let Some(Obj::Surface(state)) = self.objects.get(&tree[at]) {
                    tree.extend(state.children.iter().copied());
                }
                at += 1;
            }
            let Some(rect) = crate::ui::window_rect(self.pid, slot) else {
                // The desktop was busy (or the window not there yet): what
                // was entered stays until the next look.
                want.extend(self.entered.iter().copied().filter(|(surface, _)| tree.contains(surface)));
                continue;
            };
            let rect = rect.translate(left, top);
            let on: Vec<u32> = outputs
                .iter()
                .filter(|(_, global)| self.outputs_told.iter().any(|o| o.global == *global && !o.rect().intersect(&rect).is_empty()))
                .map(|(id, _)| *id)
                .collect();
            for surface in tree {
                for &output in &on {
                    want.push((surface, output));
                }
            }
        }
        self.enter_checked_ms = now.max(1);
        let alive = |objects: &BTreeMap<u32, Obj>, (surface, output): (u32, u32)| {
            matches!(objects.get(&surface), Some(Obj::Surface(_))) && matches!(objects.get(&output), Some(Obj::Output { .. }))
        };
        let entered = core::mem::take(&mut self.entered);
        for pair in entered.iter().copied() {
            if alive(&self.objects, pair) && !want.contains(&pair) {
                self.send(Msg::new(pair.0, 1).u32(pair.1));
            }
        }
        for pair in want.iter().copied() {
            if !entered.contains(&pair) {
                self.send(Msg::new(pair.0, 0).u32(pair.1));
                let global = self.output_global(pair.1).unwrap_or(0);
                if let Some(o) = self.outputs_told.iter().find(|o| o.global == global).filter(|_| root_ids.contains(&pair.0)) {
                    kprintln!("  wayland     : #{} surface {} entered output '{}'", self.pid, pair.0, o.name);
                }
            }
        }
        self.entered = want;
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
                // Кадры поверхности уже не покажутся.
                let unsent = match self.objects.get_mut(&id) {
                    Some(Obj::Surface(state)) => core::mem::take(&mut state.feedback),
                    _ => Vec::new(),
                };
                for feedback in unsent {
                    self.feedback_end(feedback, None);
                }
                self.feedback_discard(|surface, _| surface == id);
                self.entered.retain(|&(surface, _)| surface != id);
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

    /// Where a popup goes from its parent's corner (`base` from the owner's
    /// pixels): bounds - the monitor under the anchor rectangle (its middle),
    /// counted from the owner's pixels like the popup itself
    /// (`Compositor::popup_rect`; G5).
    fn popup_place(&self, owner: u32, base: (i32, i32), place: &Positioner) -> (i32, i32) {
        let (ax, ay, aw, ah) = place.anchor;
        let point = (base.0 + ax + aw / 2, base.1 + ay + ah / 2);
        let (origin, area) = crate::ui::popup_bounds(self.pid, owner, point).unwrap_or_else(|| {
            let (sw, sh) = crate::ui::screen_size();
            ((0, 0), Rect::new(0, 0, sw, sh))
        });
        place.constrain((origin.0 + base.0, origin.1 + base.1), area)
    }

    /// G5 (22.11): `xdg_popup.reposition(positioner, token)` (xdg_wm_base 3+):
    /// placed again by the new positioner - `repositioned(token)`, then
    /// `configure` with the new place and size and `xdg_surface.configure`;
    /// an open menu moves at once, a new size comes with the client's commit.
    fn reposition_popup(&mut self, popup: u32, positioner: u32, token: u32) {
        let Some(index) = self.pops.iter().position(|pop| pop.popup == popup) else {
            return;
        };
        let place = match self.objects.get(&positioner) {
            Some(Obj::Positioner(place)) => *place,
            _ => return,
        };
        let (owner, base, xdg) = (self.pops[index].owner, self.pops[index].base, self.pops[index].xdg);
        let (x, y) = self.popup_place(owner, base, &place);
        let at = (base.0 + x, base.1 + y);
        let pid = self.pid;
        let pop = &mut self.pops[index];
        if pop.at != at {
            pop.at = at;
            // The desktop busy - placed with the next paint.
            pop.place_due = pop.open.is_some() && crate::ui::place_window(pid, pop.slot, at).is_err();
            pop.dirty |= pop.place_due;
        }
        kprintln!("  wayland     : #{pid} popup repositioned to {},{} {}x{}", at.0, at.1, place.width, place.height);
        self.send(Msg::new(popup, 2).u32(token));
        self.send(Msg::new(popup, 0).i32(x).i32(y).i32(place.width.max(1)).i32(place.height.max(1)));
        let serial = self.next_serial();
        self.send(Msg::new(xdg, 0).u32(serial));
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
                // G5 (21.11): `wm_capabilities` (xdg_wm_base 5+) before the
                // first configure - maximize (2), fullscreen (3), minimize (4).
                // GE-Proton's winewayland takes "configured, no capabilities"
                // as "no minimize" and keeps a minimized window mapped: TMNF,
                // minimized (its corner button, focus loss of a fullscreen
                // game), stayed on the desktop as a small window showing the
                // last frame, and the first click on it restored the game to
                // fullscreen - "windowed TMNF can't be dragged" (hw-log13).
                if self.wm_version >= 5 {
                    let mut caps = Vec::with_capacity(12);
                    for cap in [2u32, 3, 4] {
                        caps.extend_from_slice(&cap.to_ne_bytes());
                    }
                    self.send(Msg::new(new, 3).bytes(&caps));
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
                    close_asked: 0,
                    want_full: false,
                    full_output: None,
                    op_due: None,
                    adopted: None,
                    stash: None,
                    presented_us: 0,
                    hold_until_us: 0,
                    busy_ms: 0,
                    fence_us: 0,
                    pace_us: 0,
                    pace_at_ms: 0,
                    limits: ((0, 0), (0, 0)),
                    limits_due: false,
                    framed: false,
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
                let (x, y) = self.popup_place(owner, base, &place);
                let slot = self.next_slot;
                self.next_slot += 1;
                self.pops.push(Pop {
                    surface,
                    xdg: id,
                    popup: new,
                    slot,
                    owner,
                    at: (base.0 + x, base.1 + y),
                    base,
                    place_due: false,
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
        super::perfdiag::WL_COMMITS.fetch_add(1, Ordering::Relaxed);
        let (pending, frames, feedback) = match self.objects.get_mut(&id) {
            Some(Obj::Surface(state)) => (state.pending.take(), core::mem::take(&mut state.frames), core::mem::take(&mut state.feedback)),
            _ => return,
        };
        let paced = !frames.is_empty();
        self.frames_due.extend(frames);
        let attached = pending.is_some();
        // Кадр без `frame` с новым буфером — клиент не ждёт темпа экрана
        // (Mesa WSI в `MAILBOX`/`IMMEDIATE`: VSync в игре выключена) — или
        // прямо разрешил разрыв (`async`). Такой кадр не тормозится, а
        // показывается не чаще монитора; последний побеждает (D7).
        let mut unpaced = false;
        if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
            if let Some(hint) = state.tearing_next.take() {
                state.tearing_async = hint;
            }
            if attached && pending != Some(None) {
                unpaced = state.tearing_async || !paced;
                // В журнал — когда видно, что это поток кадров (30 подряд), а
                // не редкая перерисовка обычной программы.
                state.run = if unpaced == state.run_unpaced { state.run.saturating_add(1) } else { 1 };
                state.run_unpaced = unpaced;
                let mode = if unpaced { 2 } else { 1 };
                if state.run == 30 && state.told != mode {
                    state.told = mode;
                    let how = match (unpaced, state.tearing_async) {
                        (true, true) => "without vsync (tearing allowed, IMMEDIATE): frames are not paced",
                        (true, false) => "without frame callbacks (MAILBOX): frames are not paced",
                        _ => "with frame callbacks (FIFO, vsync): paced by the monitor",
                    };
                    kprintln!("  wayland     : #{} surface {id} presents {how}", self.pid);
                    // Темп и где стоит окно (21.11, журнал 13: D2R на мониторе
                    // 60 Гц, второй — 180 Гц; темп FIFO — по самому быстрому
                    // монитору, показ — с частотой монитора окна).
                    if let Some(win) = self.wins.iter().find(|win| win.surface == id) {
                        let rect = crate::ui::window_rect(self.pid, win.slot);
                        let monitor = rect.and_then(monitor_of);
                        // G3: the pace is the window's own monitor (forced
                        // `wlpace <Hz>` wins).
                        let mhz = if PACE_FORCED_MHZ.load(Ordering::Relaxed) != 0 { pace_mhz() } else { monitor.map_or_else(pace_mhz, |m| m.1) };
                        match monitor {
                            Some((o, hz_mhz)) => kprintln!(
                                "  wayland     : #{} surface {id}: window on the monitor at {},{} ({}.{:03} Hz), pace {}.{:03} Hz (the window's monitor)",
                                self.pid, o.x, o.y, hz_mhz / 1000, hz_mhz % 1000, mhz / 1000, mhz % 1000
                            ),
                            None => kprintln!(
                                "  wayland     : #{} surface {id}: FIFO pace {}.{:03} Hz (window monitor unknown)",
                                self.pid, mhz / 1000, mhz % 1000
                            ),
                        }
                    }
                }
            }
        }
        // Прежний кадр этой поверхности, так и не показанный, сменён этим.
        self.feedback_discard(|surface, _| surface == id);
        if let Some(Obj::Surface(state)) = self.objects.get_mut(&id) {
            let damage = core::mem::replace(&mut state.damage, Rect::EMPTY);
            state.ready = state.ready.union(&damage);
            // Новый буфер без объявленных изменений — клиент не сказал, что
            // в нём другое; перерисовать целиком безопаснее.
            if attached && damage.is_empty() {
                state.full = true;
            }
            if let Some(view) = state.view_next.take() {
                if view != state.view {
                    state.view = view;
                    state.full = true;
                }
            }
            // Изменения у масштабируемой поверхности — в точках буфера, а
            // окно — в точках поверхности: проще перерисовать целиком.
            if state.view != View::default() {
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
            // Прежний буфер больше не читается — вернуть его клиенту. Кроме
            // буфера, который окно показывает без копии (D4): его вернёт
            // `present`, когда окно перейдёт на следующий.
            if let Some((old, _)) = old {
                if Some(old) != next && !self.wins.iter().any(|win| win.adopted.as_ref().is_some_and(|(adopted, _)| *adopted == old)) {
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
        self.feedbacks.extend(feedback.into_iter().map(|callback| (callback, id, root)));
        let Some(index) = self.wins.iter().position(|win| win.surface == root) else {
            if let Some(index) = self.pops.iter().position(|pop| pop.surface == root) {
                self.present_popup(index);
            }
            // Меню показывается сразу; поверхность без окна не покажется.
            let shown = self.pops.iter().any(|pop| pop.surface == root && pop.open.is_some() && !pop.dirty);
            if shown {
                self.feedback_presented(root, 0);
            } else {
                self.feedback_discard(|_, of| of == root);
            }
            return;
        };
        if !self.wins[index].configured {
            // Первый `commit` без буфера — просьба о `configure`: размер
            // выбирает клиент.
            self.wins[index].configured = true;
            self.configure(index, 0, 0);
        }
        // Кадр без темпа пришёл раньше кадра монитора после прошлого показа
        // — показать его (или более новый) к сроку; прежний буфер уже
        // вернулся клиенту выше, и рисовать ему есть во что (D7).
        // G3: pace by the monitor this window is on (a 60 Hz and a 180 Hz
        // monitor side by side, hw-log13), not by the fastest one.
        let win_us = self.win_frame_us(index);
        if paced {
            self.pace_us = win_us;
        }
        if unpaced && PACE.load(Ordering::Relaxed) && self.wins[index].open.is_some() {
            let now_us = crate::time::uptime_ns() / 1000;
            let due = self.wins[index].presented_us.saturating_add(win_us);
            if now_us < due {
                let win = &mut self.wins[index];
                win.dirty = true;
                win.hold_until_us = due;
                super::perfdiag::WL_COALESCED.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        self.present(index);
    }

    /// Ответ на `wp_presentation_feedback`: `presented` (с временем
    /// показа) или `discarded`; объект уничтожается сервером.
    fn feedback_end(&mut self, callback: u32, presented: Option<(u64, u32)>) {
        if !matches!(self.objects.get(&callback), Some(Obj::Feedback)) {
            return;
        }
        match presented {
            Some((ns, flags)) => {
                let secs = ns / 1_000_000_000;
                let refresh = (self.client_frame_us() * 1000).min(u64::from(u32::MAX)) as u32;
                let seq = self.present_seq;
                self.send(
                    Msg::new(callback, 1)
                        .u32((secs >> 32) as u32)
                        .u32(secs as u32)
                        .u32((ns % 1_000_000_000) as u32)
                        .u32(refresh)
                        .u32((seq >> 32) as u32)
                        .u32(seq as u32)
                        .u32(flags),
                );
            }
            None => self.send(Msg::new(callback, 2)),
        }
        self.delete(callback);
    }

    /// `discarded` ждущим показа кадрам, отобранным `which(поверхность, окно)`.
    fn feedback_discard(&mut self, which: impl Fn(u32, u32) -> bool) {
        if !self.feedbacks.iter().any(|&(_, surface, root)| which(surface, root)) {
            return;
        }
        let (gone, kept): (Vec<_>, Vec<_>) = core::mem::take(&mut self.feedbacks).into_iter().partition(|&(_, surface, root)| which(surface, root));
        self.feedbacks = kept;
        for (callback, _, _) in gone {
            self.feedback_end(callback, None);
        }
    }

    /// Окно с поверхностью `root` показано — `presented` его кадрам.
    fn feedback_presented(&mut self, root: u32, flags: u32) {
        if !self.feedbacks.iter().any(|&(_, _, of)| of == root) {
            return;
        }
        self.present_seq += 1;
        let now = crate::time::uptime_ns();
        let (done, kept): (Vec<_>, Vec<_>) = core::mem::take(&mut self.feedbacks).into_iter().partition(|&(_, _, of)| of == root);
        self.feedbacks = kept;
        for (callback, _, _) in done {
            self.feedback_end(callback, Some((now, flags)));
        }
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
        if crate::ui::window_fullscreen(self.pid, slot) {
            states.extend_from_slice(&XDG_TOPLEVEL_FULLSCREEN.to_ne_bytes());
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
            Some((w, h)) if crate::ui::window_maximized(self.pid, win.slot) || crate::ui::window_fullscreen(self.pid, win.slot) => {
                (w as i32, h as i32)
            }
            _ => (0, 0),
        }
    }

    /// Вернуть буфер, который окно показывало без копии, — если поверхность
    /// не показывает его и сейчас (тогда его вернёт обычный путь).
    ///
    /// G3: a buffer a display may still scan out (page flip not on screen
    /// yet) is held - released by [`Self::release_scanned`] once the display
    /// is done with it; no tearing, no reuse under the scanout.
    fn release_unless_shown(&mut self, _surface: u32, buffer: u32) {
        let shown = self.objects.values().any(|obj| matches!(obj, Obj::Surface(SurfaceState { shown: Some((id, _)), .. }) if *id == buffer));
        if shown {
            return;
        }
        if let Some(Obj::Buffer(buf)) = self.objects.get(&buffer) {
            if scanned(buf) {
                if !self.scan_held.iter().any(|(id, _)| *id == buffer) {
                    let buf = buf.clone();
                    self.scan_held.push((buffer, buf));
                }
                return;
            }
        }
        self.release(buffer);
    }

    /// G3: release the held buffers the displays no longer read.
    fn release_scanned(&mut self) {
        if self.scan_held.is_empty() {
            return;
        }
        let mut index = 0;
        while index < self.scan_held.len() {
            if scanned(&self.scan_held[index].1) {
                index += 1;
                continue;
            }
            let (id, _) = self.scan_held.remove(index);
            // Attached again meanwhile - the usual path releases it later.
            let shown = self.objects.values().any(|obj| matches!(obj, Obj::Surface(SurfaceState { shown: Some((shown, _)), .. }) if *shown == id));
            if !shown {
                self.release(id);
            }
        }
    }

    /// G3: the desktop window of a pointer constraint and the offset of its
    /// surface coordinates (`set_window_geometry`).
    fn constraint_window(&self, id: u32) -> Option<(u32, (i32, i32))> {
        let c = self.constraints.iter().find(|c| c.id == id)?;
        let root = self.root_of(c.surface);
        let win = self.wins.iter().find(|win| win.surface == root && win.open.is_some())?;
        Some((win.slot, win.geometry.map_or((0, 0), |(x, y, _, _)| (x, y))))
    }

    /// G3: ask the desktop for the constraints and tell the client when they
    /// start (`locked`/`confined`) and end (`unlocked`/`unconfined`; a
    /// oneshot one is then over). The desktop decides: focus, menus,
    /// Alt+Tab, Win, the pointer over the window.
    fn sync_constraints(&mut self) {
        for index in 0..self.constraints.len() {
            let (id, lock, region, asked, active, defunct) = {
                let c = &self.constraints[index];
                (c.id, c.lock, c.region, c.asked, c.active, c.defunct)
            };
            if defunct {
                continue;
            }
            let Some((slot, (gx, gy))) = self.constraint_window(id) else {
                if active {
                    self.end_constraint(index, None);
                }
                continue;
            };
            if !asked {
                crate::ui::constrain_pointer(self.pid, slot, Some((lock, region.map(|r| r.translate(-gx, -gy)))));
                self.constraints[index].asked = true;
            }
            match crate::ui::pointer_constrained(self.pid, slot) {
                Some(true) if !active => {
                    self.constraints[index].active = true;
                    kprintln!("  wayland     : #{} pointer {}", self.pid, if lock { "locked" } else { "confined" });
                    self.send(Msg::new(id, 0));
                }
                Some(false) if active => self.end_constraint(index, Some(slot)),
                _ => {}
            }
        }
    }

    /// G3: `unlocked`/`unconfined`; a oneshot constraint is over for good.
    fn end_constraint(&mut self, index: usize, slot: Option<u32>) {
        let c = &mut self.constraints[index];
        c.active = false;
        let (id, lock) = (c.id, c.lock);
        if !c.persistent {
            c.defunct = true;
            if let Some(slot) = slot {
                crate::ui::constrain_pointer(self.pid, slot, None);
            }
        }
        kprintln!("  wayland     : #{} pointer {}", self.pid, if lock { "unlocked" } else { "unconfined" });
        self.send(Msg::new(id, 1));
    }

    /// G3: the client destroyed a constraint - the desktop forgets it.
    fn drop_constraint(&mut self, id: u32) {
        let window = self.constraint_window(id);
        let Some(index) = self.constraints.iter().position(|c| c.id == id) else {
            return;
        };
        let c = self.constraints.remove(index);
        if let (Some((slot, _)), true) = (window, c.asked && !c.defunct) {
            crate::ui::constrain_pointer(self.pid, slot, None);
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
    ///
    /// Темп (P8, 18.11.2026): не чаще кадра экрана ([`FRAME_MS`]) и только
    /// когда окно действительно показано (перерисовка не отложена занятым
    /// столом), но не дольше [`FRAME_HOLD_MAX_MS`]. Раньше ответ уходил на
    /// каждом витке: игра в меню рисовала сотни кадров в секунду, и каждый
    /// копировался и собирался столом, а показывались из них шестьдесят.
    fn answer_frames(&mut self, now: u64) {
        if self.frames_due.is_empty() {
            self.frames_held_since = 0;
            return;
        }
        if self.frames_held_since == 0 {
            self.frames_held_since = now;
        }
        let held = now.saturating_sub(self.frames_held_since);
        let unshown = self.wins.iter().any(|win| win.dirty) || self.pops.iter().any(|pop| pop.dirty);
        // Срок — в микросекундах: кадр 144 Гц — 6,94 мс, миллисекундами
        // темп сбивался бы на целую.
        let now_us = crate::time::uptime_ns() / 1000;
        let frame = self.client_frame_us();
        let early = now_us < self.frame_answered_us.saturating_add(frame);
        if PACE.load(Ordering::Relaxed) && held < FRAME_HOLD_MAX_MS && (unshown || early) {
            return;
        }
        let time = now as u32;
        for callback in core::mem::take(&mut self.frames_due) {
            if self.objects.contains_key(&callback) {
                self.send(Msg::new(callback, 0).u32(time));
                self.delete(callback);
                super::perfdiag::WL_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
        }
        // Сетка кадров, а не «от последнего ответа» (D7): опоздание задачи
        // сервера (очередь готовых в QEMU) не копится, и FIFO держит частоту
        // монитора, а не 52-56 к/с при 60 Гц. Опоздание больше кадра —
        // сетка заново от «сейчас».
        let ideal = self.frame_answered_us.saturating_add(frame);
        self.frame_answered_us = if now_us >= ideal && now_us - ideal < frame { ideal } else { now_us };
        self.frames_held_since = 0;
    }

    /// G3: frame of this client's pace (us): its paced window's monitor;
    /// a forced `wlpace <Hz>` or no window yet - the global pace.
    fn client_frame_us(&self) -> u64 {
        if self.pace_us == 0 || PACE_FORCED_MHZ.load(Ordering::Relaxed) != 0 {
            frame_us()
        } else {
            self.pace_us
        }
    }

    /// G3: frame (us) of the monitor window `index` is on, looked up again
    /// every [`PACE_RECHECK_MS`] (windows move between monitors).
    fn win_frame_us(&mut self, index: usize) -> u64 {
        if PACE_FORCED_MHZ.load(Ordering::Relaxed) != 0 {
            return frame_us();
        }
        let now = crate::time::uptime_ms();
        let pid = self.pid;
        let Some(win) = self.wins.get_mut(index) else {
            return frame_us();
        };
        if win.pace_us == 0 || now.saturating_sub(win.pace_at_ms) >= PACE_RECHECK_MS {
            win.pace_at_ms = now;
            let mhz = crate::ui::window_rect(pid, win.slot).and_then(monitor_of).map(|(_, mhz)| mhz);
            win.pace_us = mhz.map_or_else(frame_us, |mhz| 1_000_000_000 / mhz.max(1));
        }
        win.pace_us
    }

    /// Когда этому клиенту пора ответить на отложенные `frame` (мс, вверх) —
    /// чтобы задача сервера проснулась к сроку, а не на следующем опросе.
    fn frames_due_at(&self) -> Option<u64> {
        let frames = (!self.frames_due.is_empty()).then(|| self.frame_answered_us.saturating_add(self.client_frame_us()).div_ceil(1000));
        // И к сроку отложенного показа кадра без темпа (D7).
        // 25.11 (hw-log19: 'busy' 2000-4300 per 30 s in GOH/UoT): a show the
        // busy desktop refused (dirty, no hold) is tried again next
        // millisecond - it waited for the next client request or the 16 ms
        // poll, and a FIFO game waited with it for its frame callback.
        // Only for a while after the first refusal: a window the desktop
        // lost keeps the old 16 ms poll.
        let now_ms = crate::time::uptime_ms();
        let mut held = self
            .wins
            .iter()
            .filter(|win| {
                win.dirty && (win.hold_until_us != 0 || win.fence_us != 0 || (win.busy_ms != 0 && now_ms.saturating_sub(win.busy_ms) < BUSY_FAST_RETRY_MS))
            })
            .map(|win| win.hold_until_us.div_ceil(1000))
            .min();
        // G3: a buffer held for a flip - look again next millisecond.
        if !self.scan_held.is_empty() {
            held = Some(0);
        }
        match (frames, held) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// `ping` клиенту и «не отвечает» столу (P4, 18.11.2026).
    ///
    /// Просьба закрыть окно (крестик, Alt+F4, меню панели) уходит клиенту
    /// событием `xdg_toplevel.close` — а дальше решает программа. TrackMania
    /// на ПК Романа просьбу трижды пропустила, и снять игру можно было только
    /// из Диспетчера задач, по процессу. Теперь: окно не закрылось за
    /// [`CLOSE_GRACE_MS`] — или клиент молчит на `ping` дольше
    /// [`PING_PATIENCE_MS`] после просьбы — стол спрашивает «Программа не
    /// отвечает — Завершить / Подождать». Молчание на `ping` без просьбы
    /// закрыть только пишется в журнал: игра, компилирующая шейдеры, —
    /// не повод для вопроса.
    fn watch_answers(&mut self, now: u64) {
        let Some(base) = self.wm_base.filter(|_| PING.load(Ordering::Relaxed)) else {
            return;
        };
        if self.wins.is_empty() {
            return;
        }
        let asked_since_ping = self.wins.iter().any(|win| win.close_asked > self.ping_sent);
        if self.ping.is_none() && (now.saturating_sub(self.ping_sent) >= PING_EVERY_MS || asked_since_ping) {
            let serial = self.next_serial();
            self.send(Msg::new(base, 0).u32(serial));
            self.ping = Some((serial, now));
            self.ping_sent = now;
        }
        let silent = self.ping.map_or(0, |(_, sent)| now.saturating_sub(sent));
        if silent >= PING_PATIENCE_MS && !self.ping_silence_told {
            self.ping_silence_told = true;
            kprintln!("  wayland     : client #{} does not answer ping for {} s", self.pid, silent / 1000);
        }
        for index in 0..self.wins.len() {
            let win = &self.wins[index];
            if win.close_asked == 0 || win.open.is_none() {
                continue;
            }
            let waited = now.saturating_sub(win.close_asked);
            if waited >= CLOSE_GRACE_MS || (silent >= PING_PATIENCE_MS && waited >= PING_PATIENCE_MS) {
                kprintln!(
                    "  wayland     : #{} window '{}' still open {} s after close was asked{}",
                    self.pid,
                    win.title,
                    waited / 1000,
                    if silent > 0 { alloc::format!(", ping unanswered for {} s", silent / 1000) } else { String::new() },
                );
                crate::ui::report_unresponsive(self.pid, win.slot, waited / 1000);
                // Следующий вопрос — не раньше чем через столько же
                // («Подождать» в диалоге).
                self.wins[index].close_asked = now;
            }
        }
    }

    /// Показать окно ([`Self::present_now`]); показано — запомнить когда и
    /// ответить `presented` его кадрам (D7).
    fn present(&mut self, index: usize) {
        // 25.11: the GPU still draws this frame (its fence was noted at
        // present instead of waited for in the client) - show it when done;
        // the server looks again next millisecond (`frames_due_at`).
        if self.fence_pending(index) {
            let win = &mut self.wins[index];
            win.dirty = true;
            if win.fence_us == 0 {
                win.fence_us = (crate::time::uptime_ns() / 1000).max(1);
                FENCE_HELD.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if let Some(win) = self.wins.get_mut(index) {
            if win.fence_us != 0 {
                FENCE_HELD_US.fetch_add((crate::time::uptime_ns() / 1000).saturating_sub(win.fence_us), Ordering::Relaxed);
                win.fence_us = 0;
            }
        }
        self.present_now(index);
        let Some(win) = self.wins.get_mut(index) else {
            return;
        };
        // 25.11: since when the desktop refuses this show (busy) - the
        // server tries again next millisecond for a while (`frames_due_at`).
        if win.dirty && win.open.is_some() {
            if win.busy_ms == 0 {
                win.busy_ms = crate::time::uptime_ms().max(1);
            }
        } else {
            win.busy_ms = 0;
        }
        if win.dirty || win.open.is_none() {
            return;
        }
        // Отложенный показ — по сетке кадров (опоздание задачи не копится).
        let now_us = crate::time::uptime_ns() / 1000;
        let due = win.hold_until_us;
        let frame = if win.pace_us != 0 { win.pace_us } else { frame_us() };
        win.presented_us = if due != 0 && now_us >= due && now_us - due < frame { due } else { now_us };
        win.hold_until_us = 0;
        // `ZERO_COPY` (8): окно показывает буфер клиента без копии.
        let (root, flags) = (win.surface, if win.adopted.is_some() { 8 } else { 0 });
        self.feedback_presented(root, flags);
    }

    /// 25.11: does the buffer of window `index` wait for its GPU frame fence
    /// (`drm::frame_pending`, dma-bufs only)?
    fn fence_pending(&self, index: usize) -> bool {
        let Some(win) = self.wins.get(index) else {
            return false;
        };
        match self.objects.get(&win.surface) {
            Some(Obj::Surface(state)) => state
                .shown
                .as_ref()
                .and_then(|(_, buf)| buf.mem.frame(0))
                .is_some_and(|first| crate::linux::drm::frame_pending(first.0)),
            _ => false,
        }
    }

    /// Свести поверхность окна с подповерхностями и показать на столе.
    fn present_now(&mut self, index: usize) {
        let surface = self.wins[index].surface;
        let (width, height) = match self.objects.get(&surface) {
            Some(Obj::Surface(state)) => state.size().unwrap_or((0, 0)),
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
        // Один буфер на всё окно (до отбора видимых): только такой можно
        // вернуть клиенту сразу после копии.
        let single = layers.len() == 1;
        cull_hidden(&mut layers, gw, gh);
        let damaged = self.damaged(surface, -gx, -gy, 0);
        // Окно сменит размер — пиксели окна станут новыми, показ без копии
        // кончается (буфер вернётся клиенту ниже).
        let mut freed = None;
        if self.wins[index].open.is_some_and(|size| size != (gw, gh)) {
            let win = &mut self.wins[index];
            win.stash = None;
            freed = win.adopted.take().map(|(id, _)| id);
        }
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
                if crate::ui::open_window_framed(pid, win.slot, &win.title, pixels, win.framed).is_err() {
                    return;
                }
                let _ = crate::ui::style_window(pid, win.slot, user_abi::WIN_STYLE_RESIZABLE);
                let _ = crate::ui::raw_keys_window(pid, win.slot);
                kprintln!("  wayland     : #{pid} window '{}' {gw}x{gh}", win.title);
                win.open = Some((gw, gh));
                if win.limits_due {
                    win.limits_due = send_limits(pid, win);
                }
                // G5: `wl_surface.enter` for its monitor right away.
                self.enter_checked_ms = 0;
                if core::mem::take(&mut win.want_full) {
                    let _ = match win.full_output.take().and_then(output_point) {
                        Some(at) => crate::ui::fullscreen_window_at(pid, win.slot, at),
                        None => crate::ui::fullscreen_window(pid, win.slot, true),
                    };
                }
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
            if let Some(id) = freed {
                self.release_unless_shown(surface, id);
            }
            return;
        }
        // Без копии (D4, 20.11.2026): кадр dma-buf во всё окно становится
        // пикселями окна; стол переносит его на поверхность окна сам.
        let shown_id = layers.first().map(|layer| layer.id);
        let win = &mut self.wins[index];
        if let (Some(ptr), Some(id)) = (zero_copy_source(&layers, gw, gh, swap), shown_id) {
            // SAFETY: `ptr` — `gw * gh` точек подряд (`zero_copy_source`:
            // строка = ширина, кадры подряд), память держит `Buffer` в
            // `win.adopted`, пока окно её показывает; клиент не пишет в
            // буфер до `release`, а он уходит только после смены пикселей.
            if let Some(pixels) = unsafe { Surface::from_raw(ptr, gw, gh) } {
                let started = crate::time::uptime_ns();
                let (desk, flush, direct) = (crate::ui::timing(), crate::ui::flush_ns(), crate::ui::direct_stats().1);
                // G3: the buffer is contiguous frames of the direct map - its
                // physical address lets a display scan it out directly.
                let phys = (ptr as u64).checked_sub(crate::mm::PHYS_MAP_BASE as u64);
                match crate::ui::swap_window_pixels(pid, win.slot, pixels, area, phys) {
                    Ok(old) => {
                        // Сборка, вывод полос и сброс кадра стола внутри
                        // показа (21.11): журнал 13 видел 3,3-4 мс на показ
                        // без копии — это и есть они (кадр 2560x1440 стол
                        // собирает и выводит целиком), у пути с копией та же
                        // цена не входила в `copy ms/paint`. G3: plus the
                        // direct copy of a fullscreen window (no compose).
                        let (desk2, flush2, direct2) = (crate::ui::timing(), crate::ui::flush_ns(), crate::ui::direct_stats().1);
                        super::perfdiag::WL_ZERO_COPY_PRESENT_NS.fetch_add(
                            desk2.2.saturating_sub(desk.2)
                                + desk2.3.saturating_sub(desk.3)
                                + flush2.saturating_sub(flush)
                                + direct2.saturating_sub(direct),
                            Ordering::Relaxed,
                        );
                        if win.adopted.is_none() {
                            win.stash = Some(old);
                            kprintln!("  wayland     : #{pid} window '{}' shows its {gw}x{gh} dma-buf without a copy", win.title);
                        }
                        let buffer = layers[0].buf.clone();
                        let previous = win.adopted.replace((id, buffer)).map(|(old, _)| old);
                        win.dirty = false;
                        super::perfdiag::WL_ZERO_COPY.fetch_add(1, Ordering::Relaxed);
                        super::perfdiag::WL_ZERO_COPY_NS.fetch_add(crate::time::uptime_ns().saturating_sub(started), Ordering::Relaxed);
                        if win.see_through && crate::ui::alpha_window(pid, win.slot, false, win.geometry.is_none()).is_ok() {
                            win.see_through = false;
                        }
                        self.clear_damage(surface, 0);
                        for old in [previous, freed].into_iter().flatten() {
                            if old != id {
                                self.release_unless_shown(surface, old);
                            }
                        }
                    }
                    Err(_) => {
                        win.dirty = true;
                        super::perfdiag::WL_BUSY.fetch_add(1, Ordering::Relaxed);
                    }
                }
                return;
            }
        }
        // Окно показывало буфер без копии, а этот кадр так нельзя: вернуть
        // окну свои пиксели (в них сейчас и нарисуем).
        if let Some((old, old_buf)) = win.adopted.take() {
            let own = match win.stash.take() {
                Some(own) if (own.width(), own.height()) == (gw, gh) => Some(own),
                _ => Surface::new(gw, gh, theme::window_bg()),
            };
            match own.map(|own| crate::ui::swap_window_pixels(pid, win.slot, own, Rect::EMPTY, None)) {
                Some(Ok(_borrowed)) => {
                    whole = true;
                    self.release_unless_shown(surface, old);
                }
                back => {
                    // Стол занят (или нет памяти) — снова на следующем витке;
                    // буфер, который окно показывает, пока держим.
                    if let Some(Err(own)) = back {
                        win.stash = Some(own);
                    }
                    win.adopted = Some((old, old_buf));
                    win.dirty = true;
                    super::perfdiag::WL_BUSY.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
        if let Some(id) = freed {
            self.release_unless_shown(surface, id);
        }
        let area = if whole { bounds } else { area };
        let win = &mut self.wins[index];
        let alpha = has_alpha(&layers);
        let mut clear = false;
        // Копия буфера клиента в окно стола — отдельно от сборки и вывода,
        // которые `paint_window` делает тут же (P1: `perf-diag`).
        let mut copy_ns = 0u64;
        let (desk, flush, direct) = (crate::ui::timing(), crate::ui::flush_ns(), crate::ui::direct_stats().1);
        let painted = crate::ui::paint_window(pid, win.slot, area, |target| {
            let started = crate::time::uptime_ns();
            clear = paint(target, &layers, swap, area, alpha);
            copy_ns = crate::time::uptime_ns().saturating_sub(started);
        });
        if painted.is_ok() {
            // Показ после копии (сборка + вывод + сброс), 21.11 — для честного
            // сравнения с показом без копии (G3: и прямой вывод).
            let (desk2, flush2, direct2) = (crate::ui::timing(), crate::ui::flush_ns(), crate::ui::direct_stats().1);
            super::perfdiag::WL_PAINT_PRESENT_NS.fetch_add(
                desk2.2.saturating_sub(desk.2)
                    + desk2.3.saturating_sub(desk.3)
                    + flush2.saturating_sub(flush)
                    + direct2.saturating_sub(direct),
                Ordering::Relaxed,
            );
            super::perfdiag::WL_PAINTS.fetch_add(1, Ordering::Relaxed);
            super::perfdiag::WL_PAINT_NS.fetch_add(copy_ns, Ordering::Relaxed);
            let px = u64::from(area.w) * u64::from(area.h);
            super::perfdiag::WL_PAINT_PX.fetch_add(px, Ordering::Relaxed);
            // Лучшая копия — только по крупным (от 0,1 Мточки): у мелких
            // кусков цена вызова больше цены точек.
            if px >= 100_000 {
                super::perfdiag::WL_PAINT_BEST.fetch_min(copy_ns * 1_000_000 / px, Ordering::Relaxed);
            }
        } else {
            super::perfdiag::WL_BUSY.fetch_add(1, Ordering::Relaxed);
        }
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
                if let Some(buffer) = layers.first().map(|layer| layer.id) {
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
            Some(Obj::Surface(state)) => state.size().unwrap_or((0, 0)),
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
                self.enter_checked_ms = 0;
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
        if pop.place_due {
            pop.place_due = crate::ui::place_window(pid, pop.slot, pop.at).is_err();
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
                if let Some((id, buf)) = &state.shown {
                    if out.try_reserve(1).is_err() {
                        return;
                    }
                    // Поверх того, что под ней, поверхность смешивается по
                    // альфе; нижний слой окна — копией, как было.
                    let blend = (depth > 0 || !out.is_empty()) && buf.format == SHM_ARGB8888;
                    let (w, h) = state.view.size(buf);
                    let src = state.view.source(buf);
                    out.push(Layer { id: *id, buf: buf.clone(), x: dx, y: dy, w, h, src, blend });
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
        win.hold_until_us = 0;
        let root = win.surface;
        self.feedback_discard(|_, of| of == root);
        let win = &mut self.wins[index];
        // Окна больше нет — и пикселей из буфера клиента тоже.
        win.stash = None;
        if let Some((buffer, _)) = win.adopted.take() {
            self.release(buffer);
        }
        let win = &mut self.wins[index];
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
        // G3: memory a display may still scan out stays alive after the
        // client (the desktop flips back to its frame buffer next frame).
        {
            let mut grave = SCAN_GRAVE.lock();
            for (_, buf) in self.scan_held.drain(..) {
                grave.push(buf);
            }
            for win in &self.wins {
                if let Some((_, buf)) = &win.adopted {
                    if scanned(buf) {
                        grave.push(buf.clone());
                    }
                }
            }
        }
        // G3: the pointer comes back if a lock of this client was active.
        let ids: Vec<u32> = self.constraints.iter().map(|c| c.id).collect();
        for id in ids {
            self.drop_constraint(id);
        }
        while !self.pops.is_empty() {
            self.close_popup(0);
        }
        while !self.wins.is_empty() {
            self.close_win(0);
        }
        // Ушедший клиент не напишет своё выделение никому.
        // The system clipboard keeps what was read from it (24.11) and takes
        // the selection over.
        let mut selection = SELECTION.lock();
        if selection.as_ref().is_some_and(|held| held.owner == self.key) {
            *selection = native_selection();
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
            Some(Obj::DataSource(mimes) | Obj::DcSource(mimes)) => Some(mimes.clone()),
            _ => None,
        };
        let mut selection = SELECTION.lock();
        if mimes.is_none() && selection.as_ref().is_some_and(|held| held.owner != self.key) {
            // Чужое выделение снимать «ничем» нельзя.
            return;
        }
        let same = selection.as_ref().is_some_and(|old| old.owner == self.key && old.source == source);
        if let Some(old) = selection.take() {
            if !(old.owner == self.key && old.source == source) && old.owner != NATIVE {
                TO_OWNER.lock().push((old.owner, ToOwner::Cancelled { source: old.source }));
            }
        }
        // Cleared - the system clipboard (what was read from the client)
        // stays offered.
        *selection = match &mimes {
            Some(mimes) => Some(Selection { owner: self.key, source, mimes: mimes.clone() }),
            None => native_selection(),
        };
        SELECTION_GEN.fetch_add(1, Ordering::AcqRel);
        drop(selection);
        if let Some(mimes) = mimes.filter(|_| !same) {
            self.fetch_selection(source, &mimes);
        }
        crate::io_events::notify();
    }

    /// The client's new selection onto the system clipboard (24.11): ask it to
    /// write the text and the file list into kernel pipes; [`pump_clipboard`]
    /// reads them.
    fn fetch_selection(&mut self, source: u32, mimes: &[String]) {
        let text = crate::ui::clipboard::TEXT_MIMES.iter().find_map(|want| mimes.iter().find(|mime| mime.eq_ignore_ascii_case(want)));
        let uris = mimes.iter().find(|mime| mime.as_str() == "text/uri-list");
        // `send` is event 1 of wl_data_source, 0 of a data-control source.
        let send = if matches!(self.objects.get(&source), Some(Obj::DcSource(_))) { 0 } else { 1 };
        let mut fetches = FETCHES.lock();
        fetches.0.clear();
        fetches.1 = crate::time::uptime_ms() + CLIP_WAIT_MS;
        for (mime, kind) in [(text, FetchKind::Text), (uris, FetchKind::Files)] {
            let Some(mime) = mime.cloned() else {
                continue;
            };
            let Ok((reader, writer)) = crate::user::pipe::create() else {
                continue;
            };
            self.out_fds.push(Handle::of_pipe_write(writer));
            self.send(Msg::new(source, send).str(&mime));
            fetches.0.push(Fetch { reader, kind, data: Vec::new(), done: false });
        }
        if !fetches.0.is_empty() {
            kprintln!("  wayland     : reading the selection of client #{} ({})", self.pid, mimes.join(", "));
        }
    }

    /// Источник уничтожен — если выделение было на нём, оно пропадает
    /// (остаётся системный буфер — то, что из него прочитано).
    fn forget_source(&mut self, source: u32) {
        let mut selection = SELECTION.lock();
        if selection.as_ref().is_some_and(|held| held.owner == self.key && held.source == source) {
            *selection = native_selection();
            SELECTION_GEN.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Выделение сменилось — каждому `wl_data_device` клиента новое
    /// предложение (`data_offer`, форматы, `selection`) или «пусто».
    ///
    /// Только клиенту с фокусом клавиатуры (как велит протокол: `selection`
    /// приходит перед `wl_keyboard.enter` и пока фокус у клиента), остальным —
    /// при входе фокуса ([`Self::keyboard_enter`]). Qt 6 (OBS, 23.11) падал
    /// на `selection` в первом обмене: его обработчик зовёт буфер обмена
    /// платформы, которой ещё нет.
    fn sync_selection(&mut self) {
        let generation = SELECTION_GEN.load(Ordering::Acquire);
        if generation == self.selection_seen || self.keyboard_on.is_none() {
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

    /// `receive(mime, fd)` of an offer (wl_data_offer or data control):
    /// the pipe goes to the selection's owner; the system clipboard is written
    /// by the server itself; no owner - the pipe closes here (empty read).
    fn receive_selection(&mut self, mime: String, fd: Handle) {
        let owner = SELECTION.lock().as_ref().map(|selection| (selection.owner, selection.source));
        match owner {
            Some((NATIVE, _)) => {
                if let Some(data) = crate::ui::clipboard::bytes_for(&mime) {
                    kprintln!("  wayland     : clipboard to client #{} as {mime}: {} bytes", self.pid, data.len());
                    FEEDS.lock().push(Feed { fd, data, at: 0, until: crate::time::uptime_ms() + CLIP_WAIT_MS });
                    crate::io_events::notify();
                }
            }
            Some(owner) => {
                TO_OWNER.lock().push((owner.0, ToOwner::Send { source: owner.1, mime, fd }));
                crate::io_events::notify();
            }
            None => {}
        }
    }

    /// The selection changed - every data-control device of the client hears
    /// it at once, focus or not (24.11): `data_offer`, the offer's types,
    /// `selection`.
    fn sync_control(&mut self) {
        let generation = SELECTION_GEN.load(Ordering::Acquire);
        if generation == self.control_seen {
            return;
        }
        self.control_seen = generation;
        let devices: Vec<u32> = self.objects.iter().filter(|(_, obj)| matches!(obj, Obj::DcDevice)).map(|(id, _)| *id).collect();
        if devices.is_empty() {
            return;
        }
        let mimes = SELECTION.lock().as_ref().map(|selection| selection.mimes.clone());
        for device in devices {
            match &mimes {
                Some(mimes) => {
                    let offer = self.next_server_id;
                    self.next_server_id = self.next_server_id.wrapping_add(1).max(0xff00_0000);
                    self.objects.insert(offer, Obj::DcOffer);
                    self.send(Msg::new(device, 0).u32(offer));
                    for mime in mimes {
                        self.send(Msg::new(offer, 0).str(mime));
                    }
                    self.send(Msg::new(device, 1).u32(offer));
                }
                None => self.send(Msg::new(device, 1).u32(0)),
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
                ToOwner::Send { source, mime, fd } => match self.objects.get(&source) {
                    Some(Obj::DataSource(_)) => {
                        self.out_fds.push(fd);
                        self.send(Msg::new(source, 1).str(&mime));
                    }
                    Some(Obj::DcSource(_)) => {
                        self.out_fds.push(fd);
                        self.send(Msg::new(source, 0).str(&mime));
                    }
                    _ => {}
                },
                ToOwner::Cancelled { source } => match self.objects.get(&source) {
                    Some(Obj::DataSource(_)) => self.send(Msg::new(source, 2)),
                    Some(Obj::DcSource(_)) => self.send(Msg::new(source, 1)),
                    _ => {}
                },
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
                        let button = evdev_button(event.code);
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
                    // G3: raw mouse motion (device delta, no acceleration) to
                    // every `zwp_relative_pointer_v1` of the client.
                    crate::ui::WIN_RELMOTION => {
                        let (dx, dy) = (event.x, event.y);
                        let us = crate::time::uptime_ns() / 1000;
                        let relatives: Vec<u32> =
                            self.objects.iter().filter(|(_, obj)| matches!(obj, Obj::RelativePointer)).map(|(id, _)| *id).collect();
                        for relative in relatives {
                            self.send(
                                Msg::new(relative, 0).u32((us >> 32) as u32).u32(us as u32).fixed(dx).fixed(dy).fixed(dx).fixed(dy),
                            );
                        }
                    }
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
                            // G3: back in front - ask for its pointer
                            // constraints again (another window may have
                            // taken the desktop's one meanwhile).
                            for index in 0..self.constraints.len() {
                                if self.root_of(self.constraints[index].surface) == surface {
                                    self.constraints[index].asked = false;
                                }
                            }
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
                        // Отсчёт «не отвечает» — с первой просьбы (P4).
                        if self.wins[index].close_asked == 0 {
                            self.wins[index].close_asked = crate::time::uptime_ms().max(1);
                            kprintln!("  wayland     : #{} asked to close '{}'", self.pid, self.wins[index].title);
                        }
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
                        let button = evdev_button(event.code);
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

    /// События `zwp_linux_dmabuf_feedback_v1`: таблица форматов (fd), главное устройство, одна партия
    /// (все форматы таблицы, без флагов), `done`.
    fn dmabuf_feedback(&mut self, feedback: u32) {
        kprintln!("  wayland     : client #{} asked dmabuf feedback: main device differs from the renderer, Mesa copies frames into a linear buffer", self.pid);
        let table = dmabuf::format_table();
        let mem = SharedMem::new(0);
        let node = Arc::new(crate::user::shm::ShmNode(Arc::clone(&mem)));
        if crate::vfs::Node::write_at(&*node, 0, &table).is_err() {
            return;
        }
        self.out_fds.push(Handle::of_node(node, false));
        let device = dmabuf::MAIN_DEVICE.to_ne_bytes();
        let mut indices = Vec::new();
        for index in 0..dmabuf::FORMATS.len() as u16 {
            indices.extend_from_slice(&index.to_ne_bytes());
        }
        self.send(Msg::new(feedback, 1).u32(table.len() as u32));
        self.send(Msg::new(feedback, 2).bytes(&device));
        self.send(Msg::new(feedback, 4).bytes(&device));
        self.send(Msg::new(feedback, 6).u32(0));
        self.send(Msg::new(feedback, 5).bytes(&indices));
        self.send(Msg::new(feedback, 3));
        self.send(Msg::new(feedback, 0));
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
        // Выделение — перед `enter` (см. `sync_selection`).
        self.sync_selection();
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
        if let Some((w, h)) = state.size() {
            if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
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
            // `wl_pointer.frame` есть с версии 5 `wl_seat`: клиент, который связался с меньшей версией
            // (vkcube), слушателя `frame` не ставит, и libwayland убивает его
            // «listener function for opcode 5 of wl_pointer is NULL» (24.11, RADV на RX 590).
            if self.seat_version >= 5 {
                self.send(Msg::new(pointer, 5));
            }
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
    /// Номер `wl_buffer`.
    id: u32,
    buf: Buffer,
    x: i32,
    y: i32,
    /// Размер слоя в окне (`wp_viewport`: может отличаться от буфера).
    w: u32,
    h: u32,
    /// Кусок буфера, который ложится в `w`×`h`: x, y, ширина, высота.
    src: (u32, u32, u32, u32),
    blend: bool,
}

impl Layer {
    /// Буфер ложится 1:1 (без масштаба).
    fn unscaled(&self) -> bool {
        (self.src.2, self.src.3) == (self.w, self.h)
    }

    /// Слой непрозрачен и закрывает всё окно `w`×`h`.
    fn covers(&self, w: u32, h: u32) -> bool {
        !self.blend
            && self.buf.format == SHM_XRGB8888
            && self.x <= 0
            && self.y <= 0
            && self.x + self.w as i32 >= w as i32
            && self.y + self.h as i32 >= h as i32
    }
}

/// Слои под непрозрачным слоем во всё окно не видны — убрать (D4,
/// 20.11.2026). Окно игры Wine — поверхность ARGB во весь размер и над ней
/// подповерхность с кадром Vulkan (dma-buf XRGB): раньше на каждый кадр
/// заливалась подложка, копировалась нижняя поверхность и поверх неё —
/// кадр игры с проходом по точкам (альфа); 0 строк одной копией в hw-log12.
fn cull_hidden(layers: &mut Vec<Layer>, w: u32, h: u32) {
    if let Some(top) = layers.iter().rposition(|layer| layer.covers(w, h)) {
        layers.drain(..top);
    }
}

/// Может ли окно быть прозрачным: нижний слой с альфой. Нижний XRGB
/// закрывает всё, что над ним смешивается.
fn has_alpha(layers: &[Layer]) -> bool {
    layers.first().is_some_and(|layer| layer.buf.format == SHM_ARGB8888)
}

/// Показ окна без копии (D4, 20.11.2026; `wlzerocopy off` — как раньше).
pub static ZERO_COPY: AtomicBool = AtomicBool::new(true);

/// 25.11: shows that waited for the GPU frame fence, and the time waited.
static FENCE_HELD: AtomicU64 = AtomicU64::new(0);
static FENCE_HELD_US: AtomicU64 = AtomicU64::new(0);

/// 25.11: `(shows held for the frame fence, us waited in all)` since boot.
#[must_use]
pub fn fence_held() -> (u64, u64) {
    (FENCE_HELD.load(Ordering::Relaxed), FENCE_HELD_US.load(Ordering::Relaxed))
}

/// G3: buffers of gone clients a display may still scan out.
static SCAN_GRAVE: crate::sync::SpinLock<Vec<Buffer>> = crate::sync::SpinLock::new(Vec::new());

/// G3: may a display still read this buffer (a page flip to it, or away
/// from it, not on screen yet)? Only physically contiguous buffers are
/// ever scanned out.
fn scanned(buf: &Buffer) -> bool {
    if !crate::display::flip_any() {
        return false;
    }
    let len = u64::from(buf.stride) * u64::from(buf.height);
    let Some(ptr) = buf.mem.contiguous(buf.offset, len) else {
        return false;
    };
    let Some(phys) = (ptr as u64).checked_sub(crate::mm::PHYS_MAP_BASE as u64) else {
        return false;
    };
    crate::display::flip_scanning(phys, len)
}

/// Кадр, который окно может показать прямо из памяти клиента: один
/// непрозрачный слой ровно в размер окна, 1:1, без перестановки каналов,
/// строки подряд, кадры буфера подряд (буфер DRM одним куском). Ответ —
/// адрес первой точки.
fn zero_copy_source(layers: &[Layer], w: u32, h: u32, swap: bool) -> Option<*mut u32> {
    let [layer] = layers else { return None };
    if swap || !ZERO_COPY.load(Ordering::Relaxed) || layer.blend || layer.buf.format != SHM_XRGB8888 {
        return None;
    }
    let buf = &layer.buf;
    if (layer.x, layer.y) != (0, 0) || (layer.w, layer.h) != (w, h) || layer.src != (0, 0, w, h) || buf.stride != w * 4 || buf.offset % 4 != 0 {
        return None;
    }
    let len = u64::from(buf.stride) * u64::from(h);
    buf.mem.contiguous(buf.offset, len).map(|ptr| ptr.cast::<u32>())
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
            && layer.x + layer.w as i32 >= ax1
            && layer.y + layer.h as i32 >= ay1
    };
    if !covered {
        for y in ay0..ay1 {
            target.row_mut(y as u32)[ax0 as usize..ax1 as usize].fill(fill);
        }
    }
    // Точки, а не байты: строку смешиваемого слоя проверяем на прозрачность
    // словами (см. ниже), и ей нужно выравнивание `u32`.
    let mut row: Vec<u32> = Vec::new();
    if row.try_reserve_exact(area.w as usize).is_err() {
        return false;
    }
    // Строк, ушедших одной копией (P8, `perf-diag`).
    let mut direct = 0u64;
    row.resize(area.w as usize, 0u32);
    for layer in layers {
        let buf = &layer.buf;
        // `wp_viewport` с масштабом (D4) — своим путём.
        if !layer.unscaled() {
            if !paint_scaled(target, layer, area, swap, keep) {
                return false;
            }
            super::perfdiag::WL_SCALED.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let x0 = layer.x.max(ax0);
        let x1 = (layer.x + layer.w as i32).min(ax1);
        if x0 >= x1 {
            continue;
        }
        let count = (x1 - x0) as usize;
        let first_column = (x0 - layer.x) as u64 + u64::from(layer.src.0);
        // Только строки слоя, попадающие в изменившуюся область.
        let sy0 = (ay0 - layer.y).max(0) as u32;
        let sy1 = ((ay1 - layer.y).max(0) as u32).min(layer.h);
        for sy in sy0..sy1 {
            let ty = sy as i32 + layer.y;
            if ty < ay0 || ty >= ay1 {
                continue;
            }
            let at = buf.offset + u64::from(sy + layer.src.1) * u64::from(buf.stride) + first_column * 4;
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
                if !swap && !alpha {
                    // Непрозрачное окно без перестановки каналов (P8,
                    // 18.11.2026): старший байт стол у такого окна не читает
                    // (`stack` копирует, `over` красит только цвет), и
                    // проход по точкам ради него — второй проход по кадру
                    // (14,7 МБ на 2560x1440). Строка — одной копией.
                    direct += 1;
                    continue;
                }
                if swap || or != 0 || keep != u32::MAX {
                    fix_row(out, swap, or, keep);
                }
                continue;
            }
            let src_row = &mut row[..count];
            // SAFETY: байты тех же `count` точек.
            let bytes = unsafe { core::slice::from_raw_parts_mut(src_row.as_mut_ptr().cast::<u8>(), count * 4) };
            if !buf.mem.read(at, bytes) {
                return false;
            }
            let out = &mut target.row_mut(ty as u32)[x0 as usize..x1 as usize];
            // Строка смешиваемого слоя целиком непрозрачна — `over` вернул бы
            // её саму: копия и тот же проход, что у нижнего слоя (19.11.2026).
            // Так winewayland кладёт кадр OpenGL (wined3d) — подповерхностью
            // ARGB над окном, — и по точкам через `over` шёл каждый кадр:
            // 12,5 мс на 0,9 Мточки даже в лучшем случае.
            if OPAQUE_ROWS.load(Ordering::Relaxed) && row_opaque(src_row) {
                out.copy_from_slice(src_row);
                if swap || keep != u32::MAX {
                    fix_row(out, swap, 0, keep);
                }
                continue;
            }
            for (pixel, &src) in out.iter_mut().zip(src_row.iter()) {
                let mut src = u32::from_le(src);
                if buf.format == SHM_XRGB8888 {
                    src |= 0xff00_0000;
                }
                let src = to_screen(src);
                *pixel = if layer.blend { mini_ui::over(src, *pixel) } else { src } & keep;
            }
        }
    }
    if direct > 0 {
        super::perfdiag::WL_DIRECT.fetch_add(1, Ordering::Relaxed);
    }
    alpha && (ay0..ay1).any(|y| !row_opaque(&target.row(y as u32)[ax0 as usize..ax1 as usize]))
}

/// Слой с масштабом (`wp_viewport`, D4 20.11.2026): кусок буфера `src`
/// ложится в `w`×`h` по ближайшей точке. Wine так растягивает кадр игры в
/// режиме ниже монитора (1280x720 → 2560x1440). Строка буфера читается один
/// раз; одинаковые строки окна (растяжение по высоте) — копией соседней.
/// `false` — память буфера не прочиталась.
fn paint_scaled(target: &mut Surface, layer: &Layer, area: Rect, swap: bool, keep: u32) -> bool {
    let buf = &layer.buf;
    let (sx, sy, sw, sh) = layer.src;
    let bounds = Rect::new(layer.x, layer.y, layer.w, layer.h);
    let clip = area.intersect(&bounds);
    if clip.is_empty() || sw == 0 || sh == 0 {
        return true;
    }
    let (cx0, cx1) = (clip.x, clip.right());
    // Шаг 16.16 по обеим осям; точка окна — середина, как у растяжения GPU.
    let step_x = (u64::from(sw) << 16) / u64::from(layer.w.max(1));
    let step_y = (u64::from(sh) << 16) / u64::from(layer.h.max(1));
    let column = |tx: i32| -> u32 { ((((tx - layer.x) as u64) * step_x + step_x / 2) >> 16).min(u64::from(sw - 1)) as u32 };
    // Столбцы буфера, нужные этой полосе окна.
    let (src_x0, src_x1) = (column(cx0), column(cx1 - 1) + 1);
    let width = (src_x1 - src_x0) as usize;
    let mut row: Vec<u32> = Vec::new();
    if row.try_reserve_exact(width).is_err() {
        return false;
    }
    row.resize(width, 0);
    // Номер столбца буфера для каждой точки полосы — один раз.
    let mut map: Vec<u32> = Vec::new();
    if map.try_reserve_exact((cx1 - cx0) as usize).is_err() {
        return false;
    }
    map.extend((cx0..cx1).map(|tx| column(tx) - src_x0));
    let or = if buf.format == SHM_XRGB8888 { 0xff00_0000 } else { 0 };
    // Последняя собранная строка окна (непрозрачный слой): соседняя строка
    // окна с той же строкой буфера — её копия.
    let mut scaled: Vec<u32> = Vec::new();
    if !layer.blend && scaled.try_reserve_exact(map.len()).is_err() {
        return false;
    }
    let mut last: Option<u32> = None;
    for ty in clip.y..clip.bottom() {
        let line = ((((ty - layer.y) as u64) * step_y + step_y / 2) >> 16).min(u64::from(sh - 1)) as u32;
        if !layer.blend && last == Some(line) {
            target.row_mut(ty as u32)[cx0 as usize..cx1 as usize].copy_from_slice(&scaled);
            continue;
        }
        let at = buf.offset + u64::from(sy + line) * u64::from(buf.stride) + u64::from(sx + src_x0) * 4;
        // SAFETY: байты тех же `width` точек.
        let bytes = unsafe { core::slice::from_raw_parts_mut(row.as_mut_ptr().cast::<u8>(), width * 4) };
        if !buf.mem.read(at, bytes) {
            return false;
        }
        let out = &mut target.row_mut(ty as u32)[cx0 as usize..cx1 as usize];
        for (pixel, &index) in out.iter_mut().zip(map.iter()) {
            let src = u32::from_le(row[index as usize]) | or;
            let src = if swap { (src & 0xff00_ff00) | ((src >> 16) & 0xff) | ((src & 0xff) << 16) } else { src };
            *pixel = if layer.blend { mini_ui::over(src, *pixel) } else { src } & keep;
        }
        if !layer.blend {
            scaled.clear();
            scaled.extend_from_slice(out);
            last = Some(line);
        }
    }
    true
}

/// Привести строку клиента к виду стола: старший байт (`or`, `keep`) и, при
/// `swap`, красный с синим местами.
///
/// Точки — по две за раз (19.11.2026): ядро без векторов, и проход по одной
/// точке — вдвое больше шагов; в QEMU без ускорения каждый шаг дорог.
fn fix_row(row: &mut [u32], swap: bool, or: u32, keep: u32) {
    let or2 = u64::from(or) * 0x1_0000_0001;
    let keep2 = u64::from(keep) * 0x1_0000_0001;
    let swap2 = |p: u64| (p & 0xff00_ff00_ff00_ff00) | ((p >> 16) & 0x0000_00ff_0000_00ff) | ((p & 0x0000_00ff_0000_00ff) << 16);
    let swap1 = |p: u32| (p & 0xff00_ff00) | ((p >> 16) & 0xff) | ((p & 0xff) << 16);
    // SAFETY: `u64` — две точки `u32` подряд; `align_to_mut` берёт только
    // выровненную середину, края (по точке, не больше) остаются точками.
    let (head, middle, tail) = unsafe { row.align_to_mut::<u64>() };
    if swap {
        for pair in middle.iter_mut() {
            *pair = swap2(*pair | or2) & keep2;
        }
        for pixel in head.iter_mut().chain(tail.iter_mut()) {
            *pixel = swap1(*pixel | or) & keep;
        }
    } else {
        for pair in middle.iter_mut() {
            *pair = (*pair | or2) & keep2;
        }
        for pixel in head.iter_mut().chain(tail.iter_mut()) {
            *pixel = (*pixel | or) & keep;
        }
    }
}

/// Все ли точки строки непрозрачны (альфа 0xff) — по две за раз.
fn row_opaque(row: &[u32]) -> bool {
    // SAFETY: см. [`fix_row`].
    let (head, middle, tail) = unsafe { row.align_to::<u64>() };
    const ALPHA2: u64 = 0xff00_0000_ff00_0000;
    head.iter().chain(tail).all(|pixel| pixel >> 24 == 0xff) && middle.iter().all(|pair| pair & ALPHA2 == ALPHA2)
}

