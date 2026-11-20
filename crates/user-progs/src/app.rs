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
use alloc::string::String;

use mini_ui::paint::Ctx;
use mini_ui::{Rect, Scale, Surface, theme, typeface};

use crate::{
    SYSINFO_DARK, SysInfo, WIN_CLOSE, WIN_KEY, WIN_POINTER, WIN_RESIZE, WIN_WHEEL, Window, exit, monotonic_ms, nanosleep,
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

/// Код средней кнопки в событии `WIN_POINTER` (ядро присылает её с 20.11).
pub const MIDDLE_BUTTON: u32 = 4;

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

    /// Клавиша с модификаторами (`WIN_MOD_*`) и латинской буквой клавиши —
    /// для сочетаний (Shift+стрелки, Ctrl+Shift+S). По умолчанию — [`App::key`].
    fn key_with(&mut self, code: u32, _mods: i32, _latin: u32) -> bool {
        self.key(code)
    }

    /// Окно просят закрыть (крестик, Alt+F4). `false` — не сейчас: программа
    /// спросит «Сохранить изменения?» и закроется сама ([`App::wants_quit`]).
    fn close_requested(&mut self) -> bool {
        true
    }

    /// Программа решила закрыться (ответ на свой вопрос перед закрытием).
    fn wants_quit(&self) -> bool {
        false
    }

    /// Шаг фоновой работы — на каждом витке цикла (~30 мс), а не раз в
    /// период: копирование файлов в «Файлах» идёт кусками, и ход его виден.
    /// `true` — перерисовать.
    fn step(&mut self) -> bool {
        false
    }

    /// Щелчок левой кнопкой в точке окна.
    fn click(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        false
    }

    /// Щелчок левой кнопкой с модификаторами (`WIN_MOD_*`): Ctrl+щелчок,
    /// Shift+щелчок. По умолчанию — [`App::click`].
    fn click_with(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32, _mods: i32) -> bool {
        self.click(area, ctx, x, y)
    }

    /// Указатель движется с зажатой левой кнопкой (перетаскивание).
    fn drag(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        false
    }

    /// Левую кнопку отпустили (конец перетаскивания), точка — от угла окна.
    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
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

    /// Колесо мыши: положительное — от себя (фаза 69c).
    fn wheel(&mut self, _clicks: i32) -> bool {
        false
    }

    /// Колесо вместе с раскладкой окна (20.11): колесо двигает **вид**, а не
    /// выделение, и чтобы не уехать за конец, надо знать высоту вида. По
    /// умолчанию — [`App::wheel`].
    fn wheel_at(&mut self, _area: Rect, _ctx: Ctx, clicks: i32) -> bool {
        self.wheel(clicks)
    }

    /// Указатель движется без нажатых кнопок — подсветить полосу прокрутки,
    /// вкладку под ним.
    fn hover(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        false
    }

    /// Указатель ушёл с окна: погасить подсветку.
    fn leave(&mut self) -> bool {
        false
    }

    /// Щелчок средней кнопкой ([`MIDDLE_BUTTON`]).
    fn click_middle(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        false
    }

    /// Имя окна сейчас. `None` — то, что в паспорте ([`Spec::title`]).
    ///
    /// Форма — `"Имя\x1fПодпись"`: имя остаётся именем окна для журнала и
    /// стенда (`focus 'Files'`), подпись стол показывает в заголовке, на
    /// панели задач и в миниатюрах. Без `\x1f` строка — и то и другое.
    ///
    /// «Файлы» зовут окно открытой папкой (20.11): два окна «Files» на
    /// панели задач и в миниатюрах не различить. Каркас сверяет имя после
    /// каждого кадра и меняет его только когда оно сменилось.
    fn title(&self) -> Option<String> {
        None
    }

    /// После всякого разобранного ввода — «Файлы» печатают здесь, куда ушло
    /// выделение.
    fn after_input(&mut self) {}

    /// Можно ли закрыть окно клавишей `q` прямо сейчас. Нельзя, пока `q` —
    /// это буква: в поле переименования, в открытом меню.
    fn quits_on_q(&self) -> bool {
        true
    }

    /// Можно ли тянуть окно за край и разворачивать. Программы каркаса
    /// получают размер окна в каждом вызове (`area`) и раскладку считают
    /// заново, поэтому умеют это все; отказывается тот, у кого раскладка
    /// рассчитана на один размер (калькулятор).
    fn resizable(&self) -> bool {
        true
    }

    /// Особый вид окна — флаги `user_abi::WIN_STYLE_*` сверх
    /// [`App::resizable`] (20.11): `WIN_STYLE_DIALOG` — маленький диалог с
    /// одним крестиком, `WIN_STYLE_AT_START` — открыть у кнопки «Пуск», как
    /// «Выполнить» в Windows.
    fn window_style(&self) -> u32 {
        0
    }
}

/// Самое маленькое окно, до которого программу каркаса можно сжать.
const MIN_W: u32 = 320;
const MIN_H: u32 = 200;

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

/// Масштаб стола из `SysInfo` (проценты, дробный — с 20.11) и ряд шрифта
/// под него. Один вход для всех программ: и каркаса [`run`], и тех, у кого
/// цикл свой (`dotnet`), — чтобы масштаб считался одинаково.
#[must_use]
pub fn ui_scale(info: &SysInfo) -> Scale {
    let (w, h) = (info.screen_w.max(1), info.screen_h.max(1));
    let scale = theme::geometry_scale_for(user_abi::sysinfo_scale_percent(info.flags), w, h);
    if !matches!(theme::form_for(w, h), theme::Form::Mobile) {
        load_font_pack(scale);
    }
    scale
}

/// Ряд шрифта для дробного масштаба (`/usr/share/fonts/fossai/ui-NNN.fnt`,
/// см. `mini_ui::typeface::install_pack`): читается один раз за жизнь
/// программы. Нет файла — ближайший встроенный ряд, без ошибки.
fn load_font_pack(scale: Scale) {
    let pct = scale.pct();
    if !typeface::pack_wanted(pct) {
        return;
    }
    let Some(path) = typeface::pack_path(pct) else {
        return;
    };
    let fd = crate::open(&path);
    if fd < 0 {
        return;
    }
    let size = crate::file_size(fd);
    let mut bytes = alloc::vec::Vec::new();
    if size > 0 && size <= 2 * 1024 * 1024 && bytes.try_reserve_exact(size as usize).is_ok() {
        bytes.resize(size as usize, 0u8);
        let mut got = 0usize;
        while got < bytes.len() {
            let step = crate::read(fd, &mut bytes[got..]);
            if step <= 0 {
                break;
            }
            got += step as usize;
        }
        bytes.truncate(got);
    }
    crate::close(fd);
    if !bytes.is_empty() {
        let _ = typeface::install_pack(alloc::boxed::Box::leak(bytes.into_boxed_slice()));
    }
}

/// Желаемые размеры окон системных программ в точках макета (множитель 1).
///
/// # Почему числом, а не долей экрана
///
/// До 20.11 диспетчеры и «Файлы» брали две трети экрана, калькулятор —
/// своё число, а каждая следующая программа — что придумала. Роман: на 1280×720
/// и на 2560×1440 калькулятор занимал полэкрана, а окна разных программ
/// открывались совсем разными. Доля экрана растёт вместе с экраном, а
/// содержимое — нет: на мониторе 2560 таблица в две трети ширины — это пустое
/// поле справа. Число же, умноженное на множитель геометрии, даёт **один и тот
/// же вид** на обоих экранах: на 2560 при 200 % окно вдвое больше в точках и
/// такое же на глаз. Маленький экран окно не переполнит — [`window_size`]
/// ограничивает его долей рабочей области.
pub mod size {
    /// «Файлы».
    pub const FILES: (u32, u32) = (1000, 640);
    /// Блокнот.
    pub const NOTEPAD: (u32, u32) = (900, 600);
    /// Диспетчер задач.
    pub const TASKMGR: (u32, u32) = (900, 620);
    /// Диспетчер устройств.
    pub const DEVMGR: (u32, u32) = (760, 560);
    /// Калькулятор — узкий и невысокий, как «Обычный» у Windows.
    pub const CALC: (u32, u32) = (320, 460);
}

/// Размер окна при открытии: желаемый размер в точках макета `logical`
/// (см. [`size`]), умноженный на множитель геометрии (тот же, что у рамки
/// стола и шрифта), но не больше доли рабочей области — экрана без панели
/// задач и заголовка окна — из [`theme::window_share`] (20.11: на 2560 было
/// 85 %, и «Файлы» занимали почти весь экран; теперь 56 × 66 %, на 1280 —
/// 82 × 86 %) и не меньше самого маленького окна каркаса.
#[must_use]
pub fn window_size(info: &SysInfo, logical: (u32, u32)) -> (u32, u32) {
    let (screen_w, screen_h) = (info.screen_w.max(1), info.screen_h.max(1));
    let scale = ui_scale(info);
    let work_h = screen_h.saturating_sub((theme::PANEL_H + theme::TITLE_H) * scale);
    let (width, height) = theme::default_window(logical, scale, (screen_w, work_h), screen_w);
    let width = width.max(MIN_W.min(screen_w));
    let height = height.max(MIN_H.min(work_h.max(1)));
    (width, height)
}

/// Окно в две трети экрана — прежний размер диспетчеров.
///
/// Устарело (20.11): размер зависел от экрана, а не от содержимого, и на
/// 2560×1440 окно выходило вдвое больше нужного. Новое — [`window_size`].
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
    let mut scale = ui_scale(info);

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

    let mut area = Rect::new(0, 0, width, height);
    println(&format!("{}: window '{}' opened, {width}x{height}", spec.name, spec.title));
    let mut app = make(Ctx::scaled(scale), area);
    // Тянуть за край и разворачивать — как всякое окно (Роман, 27.10:
    // «Файлы» нельзя было растянуть, а терминал можно).
    let style = app.window_style();
    if style != 0 {
        let resizable = if app.resizable() { user_abi::WIN_STYLE_RESIZABLE } else { 0 };
        let _ = window.set_style(style | resizable);
    } else if app.resizable() {
        let _ = window.set_resizable(true);
    }

    let mut dark = info.flags & SYSINFO_DARK != 0;
    let mut next = monotonic_ms() + spec.period_ms;
    let mut dirty = true;
    let mut frames = 0u64;
    // Модификаторы последнего нажатия кнопки мыши (`WIN_MODS` перед ним).
    let mut pointer_mods = 0i32;
    // Имя, которое окно носит сейчас: сначала — из паспорта.
    let mut shown_title: Option<String> = None;
    let reason;

    'live: loop {
        while let Some(event) = window.next_event() {
            let ctx = Ctx::scaled(scale);
            let handled = match event.kind {
                WIN_CLOSE if app.close_requested() => {
                    reason = "request";
                    break 'live;
                }
                WIN_CLOSE => true,
                WIN_KEY if event.code == QUIT && app.quits_on_q() => {
                    reason = "'q'";
                    break 'live;
                }
                WIN_KEY => app.key_with(event.code, event.x, event.y.max(0) as u32),
                WIN_POINTER if event.code == RIGHT_BUTTON => {
                    pointer_mods = 0;
                    app.click_right(area, ctx, event.x, event.y)
                }
                WIN_POINTER if event.code == MIDDLE_BUTTON => {
                    pointer_mods = 0;
                    app.click_middle(area, ctx, event.x, event.y)
                }
                user_abi::WIN_MODS => {
                    pointer_mods = event.code as i32;
                    false
                }
                WIN_POINTER => app.click_with(area, ctx, event.x, event.y, core::mem::take(&mut pointer_mods)),
                WIN_WHEEL => app.wheel_at(area, ctx, event.x),
                user_abi::WIN_MOVE if event.code & 1 != 0 => app.drag(area, ctx, event.x, event.y),
                user_abi::WIN_MOVE if event.code == 0 => app.hover(area, ctx, event.x, event.y),
                user_abi::WIN_LEAVE => app.leave(),
                user_abi::WIN_RELEASE if event.code == 1 => app.release(area, ctx, event.x, event.y),
                // Новый размер от стола: поверхность выдаётся заново, прежние
                // пиксели после ответа недействительны.
                WIN_RESIZE => {
                    let (w, h) = ((event.x.max(0) as u32).max(MIN_W), (event.y.max(0) as u32).max(MIN_H));
                    if window.resize(w, h).is_ok() {
                        let base = window.pixels().as_mut_ptr();
                        // SAFETY: ядро только что отобразило ровно `w * h`
                        // точек по этому адресу; прежняя поверхность больше не
                        // используется — она заменяется здесь же.
                        if let Some(fresh) = unsafe { Surface::from_raw(base, w, h) } {
                            surface = fresh;
                            area = Rect::new(0, 0, w, h);
                        }
                    }
                    true
                }
                _ => false,
            };
            if handled {
                app.after_input();
                dirty = true;
            }
            if app.wants_quit() {
                reason = "request";
                break 'live;
            }
        }

        if app.step() {
            dirty = true;
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
                // Масштаб стола сменился на ходу (20.11): ядро отдаёт его
                // в `flags`, и содержимое рисуется в том же, что и рамка.
                let fresh_scale = ui_scale(&fresh);
                if fresh_scale != scale {
                    let old = scale;
                    scale = fresh_scale;
                    // Окно, которое тянуть нельзя (калькулятор, «Выполнить»),
                    // стол не перекраивает — его раскладка на один размер.
                    // Размер меняет сама программа: тот же в точках макета.
                    if !app.resizable() {
                        let (w, h) = (scale.rescale(area.w, old), scale.rescale(area.h, old));
                        if window.resize(w, h).is_ok() {
                            let base = window.pixels().as_mut_ptr();
                            // SAFETY: как у WIN_RESIZE выше — ядро только что
                            // отобразило ровно `w * h` точек по этому адресу.
                            if let Some(fresh) = unsafe { Surface::from_raw(base, w, h) } {
                                surface = fresh;
                                area = Rect::new(0, 0, w, h);
                            }
                        }
                    }
                    println(&format!("{}: repainted at ui scale {scale}", spec.name));
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
            if let Some(title) = app.title() {
                if shown_title.as_deref() != Some(title.as_str()) && window.set_title(&title) >= 0 {
                    // «Имя\x1fПодпись» (договор K2, 20.11): в журнал — читаемо.
                    println(&format!("{}: window title '{}'", spec.name, title.replace('\x1f', " | ")));
                    shown_title = Some(title);
                }
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
