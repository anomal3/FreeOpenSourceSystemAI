//! Каркас программы с окном (фаза С8).
//!
//! # Откуда он взялся
//!
//! Монитор системы, «Файлы», диспетчер задач и диспетчер устройств начинались
//! одинаково: дождаться графики, выставить формат точки и тему, терпеливо
//! попросить окно, обернуть его пиксели поверхностью, крутить цикл событий,
//! раз в несколько секунд спрашивать счётчики и следить, не сменилась ли тема,
//! а закрывшись — сказать об этом в журнал. Четыре копии одного и того же, и
//! копии уже разошлись: монитор ждал графики по одному признаку, остальные — по
//! двум, и печатал о закрытии не так, как соседи.
//!
//! Здесь это одно место. Программа описывает только своё — что рисовать и что
//! делать с клавишей и щелчком ([`App`]); окно и цикл — у [`run`].
//!
//! # Два правила цикла, купленные дефектами
//!
//! * Отказ `commit` — **не** сбой. Занятый стол отвечает `ERR_AGAIN`, а
//!   пропущенный кадр ничего не стоит: следующий виток нарисует то же самое.
//!   Первая версия монитора считала это фатальным, падала, супервизор поднимал
//!   её снова, новое окно забирало фокус — и стол превращался в чехарду.
//! * Контекст рисования пересобирается на каждый кадр: он держит палитру, а
//!   палитра меняется вместе с темой.

use alloc::format;

use mini_ui::paint::Ctx;
use mini_ui::{Rect, Surface, theme};

use crate::{
    SYSINFO_DARK, SysInfo, WIN_CLOSE, WIN_KEY, WIN_POINTER, Window, exit, monotonic_ms, nanosleep,
    println, sysinfo,
};

/// Как часто заглядывать в очередь событий.
const POLL_NS: u32 = 30_000_000;

/// Сколько ждать окна при запуске.
///
/// Полминуты. Стол при загрузке занят: система много печатает, а каждая строка
/// в окне оболочки — перерисовка, на которую стол берут целиком. Программа,
/// сдавшаяся на второй секунде, падала и поднималась супервизором по кругу.
const OPEN_WAIT_MS: u64 = 30_000;

/// Сколько ждать графики. Программу запускает служба, стол поднимает ядро, и
/// порядок между ними не обещан никем.
const WAIT_GRAPHICS_MS: u64 = 10_000;

/// Клавиша, закрывающая окно.
const QUIT: u32 = 'q' as u32;

/// Код правой кнопки в событии `WIN_POINTER`.
pub const RIGHT_BUTTON: u32 = 2;

/// Что программа с окном умеет сама.
///
/// Методы, кроме [`App::draw`], необязательны. Возвращаемое `true` значит
/// «перерисовать».
pub trait App {
    /// Нарисовать окно целиком.
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx);

    /// Клавиша: символ или имя из договора (`WIN_KEY_*`).
    fn key(&mut self, _code: u32) -> bool {
        false
    }

    /// Щелчок левой кнопкой в точке окна.
    fn click(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        false
    }

    /// Щелчок правой кнопкой.
    fn click_right(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        false
    }

    /// Раз в период: свежие счётчики системы. Здесь перечитывают данные.
    fn tick(&mut self, _info: &SysInfo) -> bool {
        false
    }

    /// После всякого разобранного ввода — «Файлы» печатают здесь, куда ушло
    /// выделение.
    fn after_input(&mut self) {}

    /// Можно ли закрыть окно клавишей `q` прямо сейчас. Нельзя, пока `q` —
    /// это буква: в поле переименования, в открытом меню.
    fn quits_on_q(&self) -> bool {
        true
    }
}

/// Паспорт программы.
pub struct Spec {
    /// Имя в журнале: `files`, `taskmgr`.
    pub name: &'static str,
    /// Имя окна. Латиницей: по нему стенд наводит мышь.
    pub title: &'static str,
    /// Период [`App::tick`] и проверки темы.
    pub period_ms: u64,
}

/// Дождаться графики и приготовить рисование: формат точки и тему.
///
/// Машина без графики — не сбой: система работает в серийной линии, и
/// показывать окно просто негде, поэтому программа выходит с нулём.
///
/// Формат точки и тему приходится спрашивать: у программы своё адресное
/// пространство и свои экземпляры обоих счётчиков, пустые. Без формата окно
/// вышло бы сплошь чёрным при исправной отрисовке, без темы — светлым на тёмном
/// столе; обе ошибки глазами ищут долго.
pub fn start(spec: &Spec) -> SysInfo {
    let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
    let info = loop {
        match sysinfo() {
            Some(info) if info.pixel_format != 0 && info.screen_w != 0 => break info,
            Some(_) if monotonic_ms() < deadline => {
                nanosleep(0, POLL_NS);
            }
            _ => {
                println(&format!("{}: no graphics on this machine, nothing to show", spec.name));
                exit(0)
            }
        }
    };
    mini_ui::use_raw_format(info.pixel_format);
    theme::set_dark(info.flags & SYSINFO_DARK != 0);
    info
}

/// Окно в две трети экрана — размер диспетчеров.
#[must_use]
pub fn two_thirds(info: &SysInfo) -> (u32, u32) {
    let width = (info.screen_w * 2 / 3).clamp(320, info.screen_w.max(320));
    let height = (info.screen_h * 2 / 3).clamp(240, info.screen_h.max(240));
    (width, height)
}

/// Открыть окно размера `size` и крутить его, пока не закроют.
///
/// Состояние программы строится **после** открытия окна (`make`): строка
/// «окно открыто» стоит в журнале раньше строк, которые печатает программа,
/// и стенд ждёт их в этом порядке.
pub fn run<A: App>(spec: &Spec, info: &SysInfo, size: (u32, u32), make: impl FnOnce(Ctx, Rect) -> A) -> ! {
    let (width, height) = size;
    // Форма и множитель берутся от экрана целиком, а не от одной ширины: на
    // телефоне порог по ширине не срабатывает вовсе, и содержимое окна
    // рисовалось бы вдвое мельче рамки, которую вокруг него рисует стол.
    theme::set_form(theme::form_for(info.screen_w.max(1), info.screen_h.max(1)));
    let scale = theme::geometry_scale(info.screen_w.max(1), info.screen_h.max(1));

    let Some(mut window) = open_patiently(spec, width, height) else {
        println(&format!("{}: FAILED the desktop never freed up; no window", spec.name));
        exit(1)
    };
    let base = window.pixels().as_mut_ptr();
    // SAFETY: ядро отобразило ровно `width * height` точек по этому адресу и
    // держит их, пока живо окно; второй ссылки на них нет — `window` больше
    // пикселей никому не отдаёт.
    let Some(mut surface) = (unsafe { Surface::from_raw(base, width, height) }) else {
        println(&format!("{}: FAILED the surface the kernel gave makes no sense", spec.name));
        exit(1)
    };

    let area = Rect::new(0, 0, width, height);
    println(&format!("{}: window '{}' opened, {width}x{height}", spec.name, spec.title));
    let mut app = make(Ctx::scaled(scale), area);

    let mut dark = info.flags & SYSINFO_DARK != 0;
    let mut next = monotonic_ms() + spec.period_ms;
    let mut dirty = true;
    let mut frames = 0u64;
    let reason;

    'live: loop {
        while let Some(event) = window.next_event() {
            let ctx = Ctx::scaled(scale);
            let handled = match event.kind {
                WIN_CLOSE => {
                    reason = "request";
                    break 'live;
                }
                WIN_KEY if event.code == QUIT && app.quits_on_q() => {
                    reason = "'q'";
                    break 'live;
                }
                WIN_KEY => app.key(event.code),
                WIN_POINTER if event.code == RIGHT_BUTTON => app.click_right(area, ctx, event.x, event.y),
                WIN_POINTER => app.click(area, ctx, event.x, event.y),
                _ => false,
            };
            if handled {
                app.after_input();
                dirty = true;
            }
        }

        let now = monotonic_ms();
        if now >= next {
            next = now + spec.period_ms;
            if let Some(fresh) = sysinfo() {
                let fresh_dark = fresh.flags & SYSINFO_DARK != 0;
                if fresh_dark != dark {
                    dark = fresh_dark;
                    theme::set_dark(dark);
                    println(&format!(
                        "{}: repainted for the {} theme",
                        spec.name,
                        if dark { "dark" } else { "light" }
                    ));
                    dirty = true;
                }
                if app.tick(&fresh) {
                    dirty = true;
                }
            }
        }

        if dirty {
            app.draw(&mut surface, area, Ctx::scaled(scale));
            if window.commit() >= 0 {
                dirty = false;
                frames += 1;
            }
        }
        nanosleep(0, POLL_NS);
    }

    println(&format!("{}: closing on {reason} after {frames} frame(s)", spec.name));
    window.close();
    exit(0)
}

/// Попросить окно столько раз, сколько нужно. Всё, кроме «попробуйте ещё»,
/// окончательно: окна такого размера не дадут, сколько ни проси.
fn open_patiently(spec: &Spec, width: u32, height: u32) -> Option<Window> {
    let deadline = monotonic_ms() + OPEN_WAIT_MS;
    loop {
        match Window::open(spec.title, width, height) {
            Ok(window) => return Some(window),
            Err(code) if code != crate::ERR_AGAIN => {
                println(&format!("{}: FAILED opening the window: {code}", spec.name));
                return None;
            }
            Err(_) => {}
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}
