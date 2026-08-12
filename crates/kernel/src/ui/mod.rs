//! Рабочий стол: окна, панель задач, меню запуска и оконный менеджер.
//!
//! # Чем это не является
//!
//! Ни X11, ни Wayland. Здесь нет ни протокола, ни клиентов, ни сервера: окно —
//! это структура в том же ядре, у него есть поверхность в памяти и место на
//! экране. Смысл всей затеи ровно в этом: оконная система, которая помещается в
//! голову целиком, а не в несколько сотен тысяч строк.
//!
//! # Из чего состоит стол
//!
//! * [`compositor`] — порядок слоёв, учёт изменённого, сборка кадра, фон;
//! * [`window`] — окно с рамкой, заголовком, кнопкой закрытия и содержимым;
//! * [`panel`] — панель задач и меню запуска;
//! * [`files`] — файловый менеджер;
//! * этот модуль — глобальное состояние и **оконный менеджер**: он первым видит
//!   каждое нажатие и решает, кому оно достанется.
//!
//! # Кто получает клавишу
//!
//! Порядок разбора, сверху вниз:
//!
//! 1. открытое меню — пока оно открыто, оно забирает всё;
//! 2. сочетания оконного менеджера (Meta, Tab, Ctrl+W, Ctrl+стрелки);
//! 3. активное окно, если оно умеет обрабатывать клавиши (файловый менеджер);
//! 4. оболочка — но только если активно именно её окно.
//!
//! Последний пункт — то, чего до этой фазы не было вовсе: раньше ввод всегда
//! уходил в оболочку, а «переключение окон» переставляло их по глубине, ничего
//! не меняя во вводе. Теперь фокус — это фокус.
//!
//! # Почему менеджер живёт здесь, а не в задаче оболочки
//!
//! Потому что оболочка — одна из программ, а не хозяин экрана. Задача оболочки
//! отдаёт каждое событие в [`dispatch`] и получает обратно либо `None` («стол
//! разобрался сам»), либо событие, которое действительно предназначено ей.
//! Когда появится пользовательское пространство, на месте этого вызова окажется
//! доставка события процессу — а не переписанный оконный менеджер.

pub mod compositor;
pub mod files;
pub mod panel;
pub mod pointer;
pub mod theme;
pub mod window;

use alloc::string::String;
use core::fmt::Write as _;

use mini_ui::{Rect, Screen};

use crate::input::{Buttons, KeyCode, KeyEvent, Modifiers, PointerEvent};
use crate::sync::SpinLock;
use crate::{arch, kprintln, mm};

use compositor::Compositor;
use panel::{PanelHit, Status};
pub use window::App;
use window::{Hit, Window};

/// Насколько сдвигается окно за одно нажатие Ctrl+стрелка.
///
/// Крупный шаг, а не пиксель: без мыши перетаскивание — это серия нажатий, и
/// шагом в точку окно двигали бы минуту. Каждый шаг — это два прямоугольника
/// перерисовки, то есть цена тоже не нулевая.
const MOVE_STEP: i32 = 32;

static DESKTOP: SpinLock<Option<Compositor>> = SpinLock::new(None);

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

/// Поднять рабочий стол на этом фреймбуфере.
///
/// Возвращает `false`, если экрана нет или памяти под окна не хватило. Ядро в
/// этом случае продолжает работать с оболочкой в серийной консоли — графика не
/// является условием работы системы.
pub fn init(fb: &boot_info::Framebuffer) -> bool {
    let Some(screen) = Screen::new(fb) else {
        return false;
    };

    let scale = theme::scale_for(screen.width());
    let mut desktop = Compositor::new(screen, scale);

    // Окно оболочки обязательно, остальные — нет: без второго окна система
    // работает, поэтому отказ выделения памяти под него не повод отказываться
    // от графики целиком.
    let Some(terminal) = build(&desktop, App::Terminal) else {
        return false;
    };
    // Окно состояния сознательно перекрывает окно оболочки — без перекрытия
    // композитор не отличить от двух независимых прямоугольников. Порядок
    // добавления и есть порядок по глубине: состояние уходит вниз, оболочка
    // кладётся поверх.
    if let Some(status) = build(&desktop, App::System) {
        desktop.push(status);
    }
    desktop.push(terminal);

    desktop.refresh_panel(&status_now());
    desktop.present();

    kprintln!(
        "  desktop     : {}x{}, glyph scale {}, panel {} px",
        desktop.screen_width(),
        desktop.screen_height(),
        desktop.scale(),
        desktop.screen_height() as i32 - desktop.work_bottom(),
    );
    for (app, focused) in desktop.buttons() {
        log_window(&desktop, app, focused);
    }

    *DESKTOP.lock() = Some(desktop);
    true
}

/// Работает ли графика.
#[must_use]
pub fn is_active() -> bool {
    DESKTOP.lock().is_some()
}

/// Поработать со столом — **вне** замка.
///
/// Стол вынимается из-под замка на время работы, а не удерживается под ним, и
/// это не стилистика. [`SpinLock`] держится с запрещёнными прерываниями, а
/// сборка кадра — это до полутора миллионов записей в память устройства, то есть
/// сотни миллисекунд в отладочной сборке. Рисование под замком означало бы
/// ровно такую задержку прерываний.
///
/// Чем это вылезло: после Ctrl+W клавиша Ctrl оставалась «нажатой», и следующая
/// набранная команда молча пропадала. Причина — не в клавиатуре: пока стол
/// перерисовывал экран целиком, отпускание Ctrl ждало в контроллере, а байты из
/// UART пришли позже, но по вектору с бо́льшим приоритетом, и обогнали его.
/// Порядок событий ввода оказался не тем, в котором они произошли.
///
/// Пока стол вынут, [`is_active`] отвечает «нет», и вывод уходит только в
/// серийную линию. Это безопасно: стол трогают из одной задачи, а обработчики
/// прерываний к нему не обращаются вовсе.
fn with_desktop<R>(action: impl FnOnce(&mut Compositor) -> R) -> Option<R> {
    let mut desktop = DESKTOP.lock().take()?;
    let result = action(&mut desktop);
    *DESKTOP.lock() = Some(desktop);
    Some(result)
}

/// Где на экране стоит окно программы.
fn layout(desktop: &Compositor, app: App) -> Rect {
    // Раскладка считается от размера экрана, а не задана числами: прошивка
    // вправе дать и 800×600, и 1920×1080, и окно, не помещающееся на экран,
    // выглядело бы как испорченная графика.
    let width = desktop.screen_width();
    let work = desktop.work_bottom().max(1) as u32;
    let margin = width / 24;

    match app {
        App::Terminal => Rect::new(
            margin as i32,
            (work / 16) as i32,
            width * 5 / 8,
            work * 3 / 4,
        ),
        App::System => {
            let x = (margin + width * 9 / 16) as i32;
            let w = width.saturating_sub(x as u32 + margin);
            Rect::new(x, (work / 3) as i32, w, work / 2)
        }
        App::Files => Rect::new(
            (width / 6) as i32,
            (work / 8) as i32,
            width * 2 / 3,
            work * 2 / 3,
        ),
        App::About => {
            let w = (width / 2).max(320);
            let h = (work / 2).max(200);
            Rect::new(
                ((width - w) / 2) as i32,
                ((work.saturating_sub(h)) / 2) as i32,
                w,
                h,
            )
        }
    }
}

/// Создать окно программы.
fn build(desktop: &Compositor, app: App) -> Option<Window> {
    let rect = layout(desktop, app);
    let scale = desktop.scale();
    match app {
        App::Files => Window::files(rect, scale),
        App::About => {
            let mut window = Window::text(App::About, rect, scale)?;
            window.write_str(&about_text(desktop));
            Some(window)
        }
        other => Window::text(other, rect, scale),
    }
}

/// Текст окна «о системе».
///
/// Он же — единственное место, где сочетания клавиш стола записаны для
/// пользователя. Меню их не показывает: меню отвечает на вопрос «что можно
/// запустить», а не «как этим управлять».
fn about_text(desktop: &Compositor) -> String {
    let mut text = String::new();
    let _ = write!(
        text,
        "FreeOS {}\n\
         An operating system written from scratch in Rust.\n\n\
         architecture  {}\n\
         screen        {}x{}\n\n\
         Meta or F1    start menu\n\
         Tab           next window\n\
         Ctrl+W        close window\n\
         Ctrl+arrows   move window\n",
        env!("CARGO_PKG_VERSION"),
        arch::ARCH_NAME,
        desktop.screen_width(),
        desktop.screen_height(),
    );
    text
}

/// Что показывает панель справа.
///
/// Считается **до** захвата замка стола: счётчики памяти живут за своим замком,
/// и брать два замка во вложенном порядке — это способ однажды получить
/// взаимную блокировку.
fn status_now() -> Status {
    let frames = mm::frame::stats();
    Status {
        clock: crate::time::clock_text(),
        uptime_ms: crate::time::uptime_ms(),
        free_mib: (frames.free_bytes() / (1024 * 1024)) as u64,
        total_mib: (frames.total_bytes() / (1024 * 1024)) as u64,
    }
}

// ---------------------------------------------------------------------------
// Вывод программ
// ---------------------------------------------------------------------------

/// Напечатать в окно оболочки и собрать кадр.
pub fn write(text: &str) {
    with_window(App::Terminal, |window| window.write_str(text));
}

/// Заменить содержимое окна состояния.
pub fn set_status(text: &str) {
    with_window(App::System, |window| {
        window.clear();
        window.write_str(text);
    });
}

/// Очистить окно оболочки.
pub fn clear_shell() {
    with_window(App::Terminal, Window::clear);
}

/// Показывать ли курсор в окне оболочки.
pub fn set_cursor(visible: bool) {
    with_window(App::Terminal, |window| window.set_cursor(visible));
}

/// Сделать что-нибудь с окном программы и вывести изменения на экран.
///
/// Окна может не быть: пользователь вправе его закрыть. Тогда вывод оболочки
/// уходит только в серийную линию — это не отказ, а последствие закрытия окна.
fn with_window(app: App, action: impl FnOnce(&mut Window)) {
    with_desktop(|desktop| {
        if let Some(window) = desktop.find(app) {
            action(window);
        }
        desktop.present();
    });
}

/// Размер окна оболочки в символах.
#[must_use]
pub fn shell_size() -> (u32, u32) {
    with_desktop(|desktop| match desktop.find(App::Terminal) {
        Some(window) => window.size_in_cells(),
        None => (0, 0),
    })
    .unwrap_or((0, 0))
}

/// Где сейчас указатель и виден ли он.
///
/// `None`, если графики нет. Существует ради команды `ui` в оболочке, и это не
/// украшение вывода: положение курсора — единственное, что мышь меняет в
/// системе видимым снаружи образом, и без этой строки проверить драйвер можно
/// было бы только глазами по снимку экрана.
#[must_use]
pub fn pointer_state() -> Option<(i32, i32, bool)> {
    with_desktop(|desktop| {
        let (x, y) = desktop.pointer_position();
        (x, y, desktop.pointer_visible())
    })
}

/// Кадры, прямоугольники и число окон.
#[must_use]
pub fn stats() -> (u64, u64, usize) {
    with_desktop(|desktop| desktop.stats()).unwrap_or((0, 0, 0))
}

/// Обновить панель задач: часы и память.
///
/// Вызывается задачей оболочки раз в полсекунды — там же, где обновляется окно
/// состояния. Отдельного таймера у стола нет намеренно: перерисовка из
/// обработчика прерывания означала бы рисование под замком, взятым в
/// произвольном месте.
pub fn tick() {
    let status = status_now();
    with_desktop(|desktop| {
        desktop.refresh_panel(&status);
        desktop.present();
    });
}

// ---------------------------------------------------------------------------
// Оконный менеджер
// ---------------------------------------------------------------------------

/// Разобрать событие ввода.
///
/// Возвращает `None`, если событие забрал рабочий стол, и само событие — если
/// оно предназначено оболочке. Когда графики нет, события проходят насквозь:
/// система в серийной консоли работает ровно как раньше.
#[must_use]
pub fn dispatch(event: KeyEvent) -> Option<KeyEvent> {
    let status = status_now();
    // Графики нет: события идут прямо в оболочку, как и до появления стола.
    with_desktop(|desktop| dispatch_on(desktop, event, &status)).unwrap_or(Some(event))
}

fn dispatch_on(desktop: &mut Compositor, event: KeyEvent, status: &Status) -> Option<KeyEvent> {
    // Отпускания стол не использует: все его действия происходят по нажатию.
    // Пропускать их дальше всё равно нужно — редактор строки различает нажатие
    // и отпускание сам, и молчаливая потеря половины событий однажды вылезет.
    if !event.pressed {
        return route(desktop, event);
    }

    if desktop.menu_open() {
        handle_menu(desktop, event.code, status);
        return None;
    }

    match event.code {
        // Клавиша с логотипом — то же, что кнопка «FreeOS» на панели. F1
        // продублирован не для удобства: Meta доходит не с каждой клавиатуры и
        // не через каждый эмулятор, а стол без меню — это набор окон.
        KeyCode::LeftMeta | KeyCode::RightMeta | KeyCode::F1 => {
            toggle_menu(desktop, status);
            return None;
        }
        // Tab по кругу поднимает окна. У обычной оболочки на этой клавише
        // дополнение имён, но дополнять пока нечего, а переключать окна нужно.
        KeyCode::Tab => {
            desktop.focus_next();
            log_focus(desktop);
            desktop.refresh_panel(status);
            desktop.present();
            return None;
        }
        KeyCode::W if event.mods.contains(Modifiers::CTRL) => {
            if let Some(closed) = desktop.close_focused() {
                kprintln!("  desktop     : closed '{}'", closed.title());
            }
            log_focus(desktop);
            desktop.refresh_panel(status);
            desktop.present();
            return None;
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            if event.mods.contains(Modifiers::CTRL) =>
        {
            let (dx, dy) = match event.code {
                KeyCode::Left => (-MOVE_STEP, 0),
                KeyCode::Right => (MOVE_STEP, 0),
                KeyCode::Up => (0, -MOVE_STEP),
                _ => (0, MOVE_STEP),
            };
            desktop.move_focused(dx, dy);
            desktop.present();
            return None;
        }
        _ => {}
    }

    route(desktop, event)
}

/// Разобрать отчёт мыши.
///
/// Отдельный вход, а не общий с клавиатурой, потому что событие принципиально
/// другое: у клавиши есть код и адресат — активное окно, у указателя есть место
/// на экране, и адресата он выбирает сам, попадая в него. Общая точка входа
/// заставила бы одну из двух моделей притворяться другой.
pub fn dispatch_pointer(event: PointerEvent) {
    let status = status_now();
    with_desktop(|desktop| pointer_on(desktop, event, &status));
}

fn pointer_on(desktop: &mut Compositor, event: PointerEvent, status: &Status) {
    if event.dx != 0 || event.dy != 0 {
        desktop.move_pointer(event.dx, event.dy);
        // Перетаскивание — это движение окна вслед за указателем на то же
        // приращение. Запоминать смещение точки захвата не нужно: приращения
        // складываются сами, а окно, упёршееся в край экрана, отстаёт от
        // курсора ровно на столько, на сколько его не пустили.
        if desktop.dragging().is_some() && event.buttons.contains(Buttons::LEFT) {
            desktop.drag_by(event.dx, event.dy);
        }
    }

    let (x, y) = desktop.pointer_position();

    if event.pressed(Buttons::LEFT) {
        press(desktop, x, y, status);
    }
    if event.released(Buttons::LEFT) {
        // Где окно оказалось — в журнал: это единственное видимое снаружи
        // последствие перетаскивания, и без него проверить его можно было бы
        // только глазами по снимку экрана.
        if let Some(app) = desktop.dragging() {
            if let Some(rect) = desktop.rect_of(app) {
                kprintln!(
                    "  desktop     : moved '{}' to {},{}",
                    app.title(),
                    rect.x,
                    rect.y
                );
            }
        }
        desktop.set_drag(None);
    }

    desktop.present();
}

/// Разобрать нажатие левой кнопки.
///
/// Порядок проверок — сверху вниз по слоям кадра, и он обязан совпадать с
/// порядком рисования: щелчок должен доставаться тому, кого человек видит под
/// стрелкой. Обратный порядок означал бы, что кнопка, накрытая меню,
/// срабатывает сквозь него.
fn press(desktop: &mut Compositor, x: i32, y: i32, status: &Status) {
    // 1. Открытое меню.
    if desktop.menu_open() {
        let index = desktop.menu_mut().and_then(|menu| menu.index_at(x, y));
        match index {
            Some(index) => {
                if let Some(menu) = desktop.menu_mut() {
                    menu.select(index);
                    menu.close();
                }
                kprintln!("  desktop     : menu closed");
                desktop.mark_menu_area();
                launch(desktop, App::LAUNCHABLE[index]);
                desktop.refresh_panel(status);
                return;
            }
            // Щелчок мимо меню закрывает его и на этом заканчивается: это то,
            // чего человек ждёт от щелчка вне открытого меню, и заодно
            // единственный способ закрыть его мышью.
            None => {
                if let Some(menu) = desktop.menu_mut() {
                    menu.close();
                }
                kprintln!("  desktop     : menu closed");
                desktop.mark_menu_area();
                desktop.refresh_panel(status);
                return;
            }
        }
    }

    // 2. Панель задач.
    if let Some(hit) = desktop.panel_at(x, y) {
        match hit {
            PanelHit::Menu => toggle_menu(desktop, status),
            PanelHit::Window(app) => {
                launch(desktop, app);
                desktop.refresh_panel(status);
            }
            PanelHit::Empty => {}
        }
        return;
    }

    // 3. Окна.
    if let Some((index, hit)) = desktop.window_at(x, y) {
        let app = desktop.app_at(index);
        desktop.raise(index);
        log_focus(desktop);
        match hit {
            Hit::Close => {
                if let Some(app) = app {
                    if desktop.close(app) {
                        kprintln!("  desktop     : closed '{}'", app.title());
                    }
                    log_focus(desktop);
                }
            }
            Hit::Title => {
                desktop.set_drag(app);
                if let Some(app) = app {
                    kprintln!("  desktop     : drag '{}'", app.title());
                }
            }
            Hit::Body => {}
        }
        desktop.refresh_panel(status);
    }
}

/// Отдать событие активному окну.
fn route(desktop: &mut Compositor, event: KeyEvent) -> Option<KeyEvent> {
    match desktop.focused_app() {
        // Окна оболочки может не быть вовсе (его закрыли) — тогда ввод всё
        // равно уходит ей: иначе система осталась бы без единственного места,
        // где можно набрать команду.
        Some(App::Terminal) | None => Some(event),
        Some(_) => {
            if event.pressed {
                let handled = desktop
                    .focused_mut()
                    .is_some_and(|window| window.handle_key(event.code));
                if handled {
                    desktop.present();
                }
            }
            None
        }
    }
}

/// Открыть или закрыть меню запуска.
fn toggle_menu(desktop: &mut Compositor, status: &Status) {
    let Some(menu) = desktop.menu_mut() else {
        return;
    };
    let opened = menu.toggle();
    kprintln!(
        "  desktop     : menu {}",
        if opened { "opened" } else { "closed" }
    );
    if !opened {
        // Закрытое меню надо стереть: под ним фон и окна, которые никто не
        // перерисовывал, — они не «изменились», но их снова видно.
        desktop.mark_menu_area();
    }
    desktop.refresh_panel(status);
    desktop.present();
}

/// Разобрать клавишу, пока меню открыто.
fn handle_menu(desktop: &mut Compositor, code: KeyCode, status: &Status) {
    let Some(menu) = desktop.menu_mut() else {
        return;
    };

    let mut launching = None;
    let mut closed = false;
    match code {
        KeyCode::Up => menu.move_selection(false),
        KeyCode::Down => menu.move_selection(true),
        KeyCode::Enter => {
            launching = Some(menu.selection());
            menu.close();
            closed = true;
        }
        KeyCode::Escape | KeyCode::LeftMeta | KeyCode::RightMeta | KeyCode::F1 => {
            menu.close();
            closed = true;
        }
        _ => return,
    }

    if closed {
        kprintln!("  desktop     : menu closed");
        desktop.mark_menu_area();
    }
    if let Some(app) = launching {
        launch(desktop, app);
    }
    desktop.refresh_panel(status);
    desktop.present();
}

/// Запустить программу: поднять её окно или создать новое.
fn launch(desktop: &mut Compositor, app: App) {
    if let Some(index) = desktop.index_of(app) {
        desktop.raise(index);
        log_focus(desktop);
        return;
    }
    // Памяти под ещё одно окно может не хватить, и это не повод останавливать
    // систему: меню просто не откроет программу.
    if let Some(window) = build(desktop, app) {
        desktop.push(window);
        kprintln!("  desktop     : opened '{}'", app.title());
        log_window(desktop, app, true);
        log_focus(desktop);
    } else {
        kprintln!("  desktop     : not enough memory for '{}'", app.title());
    }
}

/// Записать в журнал, какое окно стало активным.
///
/// Не украшение вывода: рабочий стол — единственная часть системы, у которой
/// нет собственного текстового вывода, и без этих строк проверить её мог бы
/// только человек, глядящий на экран. Снимок экрана доказательством не является
/// — он показывает последний нарисованный кадр, а не текущее состояние.
fn log_focus(desktop: &Compositor) {
    if let Some(app) = desktop.focused_app() {
        kprintln!("  desktop     : focus '{}'", app.title());
    }
}

/// Записать в журнал, где стоит окно.
///
/// Координаты нужны не человеку: по ним автоматический прогон наводит мышь.
/// Без них сценарий с мышью пришлось бы писать в числах, подобранных под один
/// размер экрана, — то есть отдельно под каждую архитектуру, потому что OVMF
/// даёт 1280×800, а ramfb на `virt` — 800×600.
fn log_window(desktop: &Compositor, app: App, focused: bool) {
    let Some(rect) = desktop.rect_of(app) else {
        return;
    };
    kprintln!(
        "  window      : '{}' at {},{} {}x{}{}",
        app.title(),
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if focused { " (focused)" } else { "" }
    );
}
