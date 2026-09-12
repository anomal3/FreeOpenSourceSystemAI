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
pub mod context;
pub mod files;
pub mod icons;
pub mod paint;
pub mod panel;
pub mod pointer;
pub mod settings;
pub mod term;
pub mod theme;
pub mod window;

use alloc::string::String;
use core::fmt::Write as _;

use mini_ui::{Rect, Screen, Surface};
use user_abi::{WIN_CLOSE, WIN_KEY, WIN_POINTER, WinEvent};

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

/// За сколько миллисекунд два щелчка считаются двойным.
///
/// Полсекунды — то, к чему человек привык за тридцать лет чужих рабочих столов.
/// Меньше — и двойной щелчок не засчитывается у того, кто не торопится; больше —
/// и два отдельных щелчка по одному значку случайно открывают программу.
const DOUBLE_CLICK_MS: u64 = 500;

static DESKTOP: SpinLock<Option<Compositor>> = SpinLock::new(None);

/// Когда был прошлый щелчок по значку и по какому именно.
///
/// Обычные статики, а не поле стола: стол вынимается из-под замка на время
/// работы, и класть в него состояние, которое нужно **между** двумя вызовами,
/// значит гадать, тот ли это стол.
static LAST_ICON_CLICK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static LAST_ICON: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(u32::MAX);

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

    let scale = theme::geometry_scale(screen.width());
    // Буфер кадра — условие работы стола, а не украшение: без него собирать
    // картинку негде. Не хватило памяти — система работает в серийной линии,
    // ровно как на машине без фреймбуфера.
    let Some(mut desktop) = Compositor::new(screen, scale) else {
        return false;
    };

    // Окно оболочки обязательно: без него системе негде принять команду.
    //
    // Второго окна ядро больше не заводит. До фазы 47b здесь открывалось окно
    // состояния — оно перекрывало оболочку и тем показывало, что композитор
    // складывает слои, а не рисует два независимых прямоугольника. Теперь
    // монитор системы это программа (`/bin/sysmon`), её поднимает супервизор, и
    // перекрытие получается то же самое — только окно приходит снаружи ядра.
    let Some(terminal) = build(&desktop, App::Terminal) else {
        return false;
    };
    desktop.push(terminal);

    desktop.refresh_panel(&status_now());
    desktop.present();

    kprintln!(
        "  desktop     : {}x{}, ui scale {}, panel {} px",
        desktop.screen_width(),
        desktop.screen_height(),
        desktop.scale(),
        desktop.screen_height() as i32 - desktop.work_bottom(),
    );
    for entry in desktop.buttons() {
        log_window(&desktop, entry.app, entry.focused);
    }
    log_icons(&desktop);
    // Сколько программ нашлось в `/bin` — единственное, чем список меню видно
    // снаружи: сам он рисуется, а нарисованное доказательством не считается.
    // Два числа, а не одно, и печатаются они **всегда**, даже когда совпадают.
    //
    // Сначала здесь стоял хвост «столько-то не поместилось», дописываемый по
    // условию, — и это была ошибка: строка получалась разной на разных машинах,
    // потому что помещается на них разное. На AArch64 с его разрешением влезают
    // все, на x86-64 — нет, и сценарий, один на обе архитектуры, проверить такое
    // не может. Два числа рядом читаются одинаково везде, а неполнота списка
    // видна их несовпадением.
    let listed = desktop.menu_programs();
    kprintln!(
        "  desktop     : start menu lists {listed} of {} programs from /bin",
        listed + desktop.menu_dropped()
    );

    // Размер сетки запоминается один раз: окна не меняют размера, а спрашивают
    // его программы — в том числе тогда, когда стол занят перерисовкой.
    let cells = match desktop.find(App::Terminal) {
        Some(window) => window.size_in_cells(),
        None => (0, 0),
    };
    SHELL_CELLS.store(
        (u64::from(cells.0) << 32) | u64::from(cells.1),
        core::sync::atomic::Ordering::Relaxed,
    );

    *DESKTOP.lock() = Some(desktop);
    GRAPHICS.store(true, core::sync::atomic::Ordering::Relaxed);
    true
}

/// Доступен ли стол **прямо сейчас**.
///
/// Отвечает «нет» и тогда, когда стол вынут из-под замка на время работы (см.
/// [`with_desktop`]), — то есть это вопрос «можно ли сию секунду нарисовать», а
/// не «есть ли на машине графика». На второй отвечает [`graphics`], и путать их
/// нельзя: вывод, отданный в окно в момент перерисовки, просто пропал бы.
#[must_use]
pub fn is_active() -> bool {
    DESKTOP.lock().is_some()
}

/// Поднята ли графика вообще.
///
/// В отличие от [`is_active`], ответ не зависит от того, занят ли стол
/// перерисовкой. Разница появилась в Phase 29 и она существенная: разбор
/// управляющих последовательностей — состояние **терминала**, а не окна, и
/// пропустить `ESC [ 2 J` потому, что в этот момент рисовался кадр, значит
/// получить программу, которая ведёт себя по-разному в зависимости от того,
/// успел ли стол.
#[must_use]
pub fn graphics() -> bool {
    GRAPHICS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Поднята ли графика. Ставится один раз, при запуске стола.
static GRAPHICS: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Размер окна оболочки в знаках, упакованный в одно слово.
///
/// Живёт отдельно от стола намеренно: это **свойство**, а не операция, и
/// спрашивать его через захват стола значило бы возвращать нули всякий раз,
/// когда стол занят перерисовкой. Программа, спросившая размер окна и
/// получившая `0x0`, нарисовала бы рамку шириной ноль — и виновата была бы
/// гонка, а не программа.
static SHELL_CELLS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

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
        App::Files => Rect::new(
            (width / 6) as i32,
            (work / 8) as i32,
            width * 2 / 3,
            work * 2 / 3,
        ),
        // «Параметры» — окно из двух колонок: разделы слева и настройки
        // справа. Ширина считается от них, а не долей экрана: боковая колонка
        // 240, содержимому нужно место под подпись, пояснение и элемент
        // справа, а предпросмотр стола — ещё 340. Окно уже этой суммы
        // показывало бы содержимое в щели между колонкой и краем.
        App::Settings => {
            let w = (width * 7 / 8).clamp(560, width.saturating_sub(margin).max(560));
            let h = (work * 4 / 5).max(360);
            Rect::new(
                ((width.saturating_sub(w)) / 2) as i32,
                (work / 12) as i32,
                w,
                h,
            )
        }
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
        // Подтверждение встаёт по центру и заметно меньше остальных: это вопрос
        // на две строки ответа, и окно размером с терминал выглядело бы как
        // ещё одна программа, а не как «система ждёт от вас слова».
        App::Shutdown | App::Restart => {
            let w = (width / 2).max(360).min(width);
            let h = (work / 3).max(180);
            Rect::new(
                ((width.saturating_sub(w)) / 2) as i32,
                ((work.saturating_sub(h)) / 2) as i32,
                w,
                h,
            )
        }
        // Окно программы сюда не приходит: его прямоугольник считается от
        // поверхности, которую заказала сама программа ([`open_window`]), а не
        // от размера экрана. Ветка стоит ради полноты разбора, и ответ у неё
        // осмысленный, а не нулевой, — на случай, если однажды придёт.
        App::Program(_) => {
            let w = (width / 2).max(320);
            let h = (work / 2).max(200);
            Rect::new(
                ((width.saturating_sub(w)) / 2) as i32,
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
        App::Settings => Window::settings(
            rect,
            scale,
            (desktop.screen_width(), desktop.screen_height()),
        ),
        App::About => {
            let mut window = Window::text(App::About, rect, scale)?;
            window.write_str(&about_text(desktop));
            Some(window)
        }
        App::Shutdown | App::Restart => {
            let mut window = Window::text(app, rect, scale)?;
            window.write_str(&confirm_text(app));
            Some(window)
        }
        // Окно программы строится не здесь, и построить его столу нечем: ему
        // нужны кадры, которые выдаёт [`crate::user`]. Сюда приходят с меню и с
        // панели задач, а там окно программы к этому времени уже существует —
        // кнопка на панели без окна не рисуется.
        App::Program(_) => None,
        other => Window::text(other, rect, scale),
    }
}

/// Текст окна подтверждения.
///
/// Оно объясняет не только выбор, но и последствие: «том будет закрыт» — это
/// то, ради чего порядок действий при выключении вообще существует, и человеку
/// стоит знать, что систему нельзя гасить кнопкой на корпусе просто так.
fn confirm_text(app: App) -> String {
    let mut text = String::new();
    // Строки короткие не случайно: окно подтверждения вдвое уже терминала, а
    // перенос посреди фразы выглядит как испорченный вывод. Сорок знаков
    // помещаются и на 800×600, и на 1280×800.
    let (what, then) = if app == App::Restart {
        ("Перезагрузить машину?", "Она запустится снова с того же диска.")
    } else {
        ("Выключить машину?", "Включать её придётся руками.")
    };
    let _ = write!(
        text,
        "{what}\n\n\
         Корневой том закрывается первым,\n\
         чтобы следующая загрузка нашла его целым.\n\
         {then}\n\n\
         Y   да, выключаем\n\
         N   нет, передумал (Esc, Ctrl+W)\n",
    );
    text
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
         Операционная система, написанная на Rust с пустого места.\n\n\
         архитектура   {}\n\
         экран         {}x{}\n\n\
         Meta или F1   меню запуска\n\
         Tab           следующее окно\n\
         Ctrl+W        закрыть окно\n\
         Ctrl+стрелки  подвинуть окно\n",
        crate::VERSION,
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
///
/// Текст проходит через разбор управляющих последовательностей: с Phase 29
/// программа вправе не только печатать, но и управлять терминалом — двигать
/// курсор, очищать экран, менять цвет. Всё, что не последовательность,
/// попадает в сетку символов как раньше.
pub fn write(text: &str) {
    // Разбор идёт в обоих случаях, а рисование — только если стол свободен.
    // Терминал обязан помнить, что ему сказали, даже когда показать это некуда:
    // иначе последовательность, пришедшая в момент перерисовки, потерялась бы
    // наполовину, и следующая за ней выглядела бы испорченной.
    let shown = with_desktop(|desktop| {
        term::feed(desktop.find(App::Terminal), text);
        desktop.present();
    });
    if shown.is_none() {
        term::feed(None, text);
    }
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

/// Размер окна оболочки в символах. Нули — графики нет.
#[must_use]
pub fn shell_size() -> (u32, u32) {
    let packed = SHELL_CELLS.load(core::sync::atomic::Ordering::Relaxed);
    ((packed >> 32) as u32, packed as u32)
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
    for attempt in 0..INPUT_TRIES {
        if let Some(answer) = with_desktop(|desktop| dispatch_on(desktop, event, &status)) {
            return answer;
        }
        // Графики нет вовсе — ждать нечего, событие идёт прямо в оболочку, как
        // и до появления стола.
        if !graphics() {
            return Some(event);
        }
        // Стол вынут из-под замка: кто-то собирает кадр. Ждём — см.
        // [`INPUT_TRIES`], — но только между попытками, а не после последней.
        if attempt + 1 < INPUT_TRIES {
            crate::sched::sleep_ms(INPUT_PAUSE_MS);
        }
    }
    // Не дождались. Событие всё-таки уходит оболочке: она есть всегда, а
    // потерять нажатие молча — худшее из возможного.
    Some(event)
}

/// Сколько раз ждать освободившийся стол, прежде чем отдать событие оболочке.
///
/// # Почему ждать вообще приходится
///
/// Потому что [`with_desktop`] отвечает `None` на **два разных** вопроса:
/// «графики нет» и «стол сейчас занят сборкой кадра». До фазы 47b разница не
/// проявлялась — фокус почти всегда был у оболочки, и «отдать оболочке»
/// случайно совпадало с правильным ответом.
///
/// С окнами программ совпадение кончилось. Монитор системы перерисовывает своё
/// окно раз в секунду, то есть стол занят заметную долю времени, и клавиша,
/// попавшая в этот промежуток, доставалась **не тому окну**: программа своих
/// событий не получала вовсе, а в командной строке появлялись буквы, которых
/// туда никто не набирал. В журнале это выглядело как `awinshow: waited 20011
/// ms, 0 event(s)` — буква `a`, отражённая эхом оболочки, приклеенная к строке
/// программы, которая так ничего и не дождалась.
///
/// Двести пятьдесят попыток по [`INPUT_PAUSE_MS`] — это полсекунды. Ста
/// миллисекунд, стоявших здесь сначала, не хватило: стол держат не только
/// сборкой кадра, но и **открытием окна** — выделить поверхность, нарисовать
/// украшения, переставить панель, вывести всё на экран, — а в отладочной сборке
/// это сотни миллисекунд. Щелчок, пришедшийся на такой момент, терялся.
const INPUT_TRIES: u32 = 250;

/// Пауза между попытками достучаться до стола.
const INPUT_PAUSE_MS: u64 = 2;

fn dispatch_on(desktop: &mut Compositor, event: KeyEvent, status: &Status) -> Option<KeyEvent> {
    // Отпускания стол не использует: все его действия происходят по нажатию.
    // Пропускать их дальше всё равно нужно — редактор строки различает нажатие
    // и отпускание сам, и молчаливая потеря половины событий однажды вылезет.
    if !event.pressed {
        return route(desktop, event, status);
    }

    // Меню стола выше меню запуска и выше сочетаний оконного менеджера: пока в
    // нём набирают имя, Ctrl+W и Tab означают буквы этого имени, а не действия
    // над окнами.
    if desktop.context_open() {
        if handle_context_key(desktop, event, status) {
            return None;
        }
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
            // Окно программы закрывает сама программа, и Ctrl+W для неё —
            // такая же просьба, как крестик. Иначе сочетание отнимало бы у неё
            // и окно, и возможность спросить «сохранить?».
            let focused = desktop.focused_app();
            // Имя берётся до всякого закрытия — по той же причине, что и у
            // крестика: у окна программы оно живёт в самом окне.
            let name = focused.map(|app| name_of(desktop, app));
            if focused.is_some_and(|app| request_close(desktop, app)) {
                if let Some(name) = &name {
                    kprintln!("  desktop     : close requested of '{name}'");
                }
            } else if desktop.close_focused().is_some() {
                if let Some(name) = &name {
                    kprintln!("  desktop     : closed '{name}'");
                }
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

    route(desktop, event, status)
}

/// Разобрать отчёт мыши.
///
/// Отдельный вход, а не общий с клавиатурой, потому что событие принципиально
/// другое: у клавиши есть код и адресат — активное окно, у указателя есть место
/// на экране, и адресата он выбирает сам, попадая в него. Общая точка входа
/// заставила бы одну из двух моделей притворяться другой.
pub fn dispatch_pointer(event: PointerEvent) {
    let status = status_now();
    // Ждут **все** события указателя, а не только нажатия, и это выяснилось
    // дорого. Казалось, что потерянное движение ничего не стоит: следующее
    // придёт через миллисекунды. На деле движения складываются — курсор ведут
    // приращениями, — и потерянное означает, что указатель не доехал. Щелчок
    // после этого приходит исправно, но мимо: в журнале это выглядит как
    // «не нажалась кнопка меню», а не как «потерялось движение мыши».
    let tries = INPUT_TRIES;

    for attempt in 0..tries {
        if with_desktop(|desktop| pointer_on(desktop, event, &status)).is_some() {
            return;
        }
        if !graphics() {
            return;
        }
        // Спим только между попытками, а не после последней. Разница
        // выглядит придиркой и стоила красного прогона: у движений попытка
        // одна, и сон в конце тела означал две миллисекунды сна на **каждое**
        // потерянное движение. У мыши их поток, задача ввода отставала — и
        // команда, набранная в серийную линию, терялась целиком, потому что
        // читает эту линию та же задача.
        if attempt + 1 < tries {
            crate::sched::sleep_ms(INPUT_PAUSE_MS);
        }
    }
}

fn pointer_on(desktop: &mut Compositor, event: PointerEvent, status: &Status) {
    // На сколько сдвинулся курсор. У мыши это приехало в отчёте, у планшета
    // приходится вычесть одно положение из другого: устройство сообщило точку,
    // а окно можно тащить только на разницу.
    let (dx, dy) = match event.absolute {
        Some((x, y)) => desktop.move_pointer_to(x, y),
        None if event.dx != 0 || event.dy != 0 => {
            desktop.move_pointer(event.dx, event.dy);
            (event.dx, event.dy)
        }
        None => (0, 0),
    };

    if dx != 0 || dy != 0 {
        // Перетаскивание — это движение окна вслед за указателем на то же
        // приращение. Запоминать смещение точки захвата не нужно: приращения
        // складываются сами, а окно, упёршееся в край экрана, отстаёт от
        // курсора ровно на столько, на сколько его не пустили.
        if desktop.dragging().is_some() && event.buttons.contains(Buttons::LEFT) {
            desktop.drag_by(dx, dy);
        }
    }

    let (x, y) = desktop.pointer_position();

    if event.pressed(Buttons::LEFT) {
        press(desktop, x, y, status);
    }
    // Правая кнопка: меню стола там, где щёлкнули. Пока оно открыто, левая
    // кнопка выбирает в нём пункт — этим и занимается `press`.
    if event.pressed(Buttons::RIGHT) {
        if desktop.context_open() {
            desktop.close_context();
        } else if desktop.window_at(x, y).is_none() && desktop.panel_at(x, y).is_none() {
            // Пункты зависят от того, во что целились. «Удалить», предложенное
            // тогда, когда ничего не выбрано, относилось бы неизвестно к чему —
            // а на столе это означало бы удалённый наугад файл.
            let (items, what) = match desktop.icon_at(x, y) {
                Some(index) => {
                    desktop.select_icon(Some(index));
                    match desktop.icon_kind(index) {
                        Some(icons::Kind::App(_)) => (&context::Action::ON_APP[..], "icon"),
                        Some(_) => (&context::Action::ON_ENTRY[..], "entry"),
                        None => (&context::Action::ON_DESKTOP[..], "desktop"),
                    }
                }
                None => {
                    desktop.select_icon(None);
                    (&context::Action::ON_DESKTOP[..], "desktop")
                }
            };
            desktop.open_context(x, y, items);
            // Печатается **место, куда меню встало**, а не точка щелчка: у
            // края экрана оно сдвигается, чтобы не выехать, и стенд, целящийся
            // по точке щелчка, попадал бы мимо пунктов.
            if let Some(rect) = desktop.context_rect() {
                kprintln!(
                    "  desktop     : context menu at {},{} {}x{} for {what}",
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h
                );
            }
        }
    }
    if event.released(Buttons::LEFT) {
        // Где окно оказалось — в журнал: это единственное видимое снаружи
        // последствие перетаскивания, и без него проверить его можно было бы
        // только глазами по снимку экрана.
        //
        // Строка печатается, **только если окно действительно переехало**.
        // Щелчок по заголовку проходит через тот же захват и отпускание, и
        // раньше он писал «переехало туда, где стояло». Стенд ждёт переезда по
        // этой строке — и принимал за него щелчок, после чего целился в кнопку
        // закрытия по старому месту окна и промахивался мимо всего окна.
        if let Some(app) = desktop.dragging().filter(|_| desktop.drag_moved()) {
            if let Some(rect) = desktop.rect_of(app) {
                kprintln!(
                    "  desktop     : moved '{}' to {},{}",
                    name_of(desktop, app),
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
    // 0. Меню стола — оно поверх всего, включая меню запуска.
    if desktop.context_open() {
        // Пока в меню набирают имя или отвечают на вопрос об удалении, щелчок
        // внутрь него не пункт: пунктов там сейчас не нарисовано. Щелчок мимо
        // — отказ, как и Esc.
        if desktop.context_editing() {
            if !desktop.context_contains(x, y) {
                desktop.close_context();
                kprintln!("  desktop     : context menu closed");
            }
            return;
        }
        match desktop.context_action_at(x, y) {
            Some(action) => {
                desktop.context_select(action);
                context_action(desktop, action, status);
                return;
            }
            None => {
                desktop.close_context();
                kprintln!("  desktop     : context menu closed");
                return;
            }
        }
    }

    // 1. Открытое меню.
    if desktop.menu_open() {
        let choice = desktop.menu_mut().and_then(|menu| menu.choice_at(x, y));
        match choice {
            Some(choice) => {
                if let Some(menu) = desktop.menu_mut() {
                    menu.select_at(x, y);
                    menu.close();
                }
                kprintln!("  desktop     : menu closed");
                desktop.mark_menu_area();
                run_choice(desktop, choice);
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
                // Щелчок по кнопке активного окна сворачивает его — так ведёт
                // себя панель задач везде, где человек её видел, и другого
                // способа свернуть окно без мыши у кнопки нет.
                if desktop.focused_app() == Some(app) && !desktop.is_minimized(app) {
                    let name = name_of(desktop, app);
                    if desktop.minimize(app) {
                        kprintln!("  desktop     : minimized '{name}'");
                    }
                    log_focus(desktop);
                } else {
                    launch(desktop, app);
                }
                desktop.refresh_panel(status);
            }
            PanelHit::Empty => {}
        }
        return;
    }

    // 3. Значки на столе — ниже окон, но выше фона.
    if let Some(index) = desktop.icon_at(x, y) {
        desktop.select_icon(Some(index));
        // Второй щелчок по тому же значку в пределах [`DOUBLE_CLICK_MS`]
        // открывает его. Порог во времени, а не «щелчок с Shift» и не
        // «одиночный открывает»: так это работает у всех, кто видел стол.
        let now = crate::time::uptime_ms();
        let last = LAST_ICON_CLICK.swap(now, core::sync::atomic::Ordering::Relaxed);
        let key = index as u32;
        let same = LAST_ICON.swap(key, core::sync::atomic::Ordering::Relaxed) == key;
        if same && now.saturating_sub(last) <= DOUBLE_CLICK_MS {
            open_icon(desktop, index);
            desktop.refresh_panel(status);
        }
        return;
    }
    // Щелчок по пустому столу снимает выделение: выбранным обязано оставаться
    // то, во что человек целился последним, а не то, что он выбрал минуту назад
    // и уже забыл.
    desktop.select_icon(None);

    // 4. Окна.
    if let Some((index, hit)) = desktop.window_at(x, y) {
        let app = desktop.app_at(index);
        desktop.raise(index);
        log_focus(desktop);
        match hit {
            Hit::Close => {
                if let Some(app) = app {
                    // Имя спрашивается **до** закрытия: у окна программы оно
                    // живёт в самом окне, и после того, как окно убрано со
                    // стола, узнать его уже негде.
                    let name = name_of(desktop, app);
                    if request_close(desktop, app) {
                        kprintln!("  desktop     : close requested of '{name}'");
                    } else if desktop.close(app) {
                        kprintln!("  desktop     : closed '{name}'");
                    }
                    log_focus(desktop);
                }
            }
            Hit::Minimize => {
                if let Some(app) = app {
                    let name = name_of(desktop, app);
                    if desktop.minimize(app) {
                        kprintln!("  desktop     : minimized '{name}'");
                    }
                    log_focus(desktop);
                }
            }
            Hit::Maximize => {
                if let Some(app) = app {
                    let name = name_of(desktop, app);
                    if desktop.toggle_maximize(app) {
                        kprintln!("  desktop     : resized '{name}'");
                        // Новый прямоугольник — следом: без него снаружи не
                        // видно, куда уехало окно, и попасть в его кнопку
                        // второй раз можно только наугад.
                        log_window(desktop, app, true);
                    }
                }
            }
            Hit::Resize => {
                if let Some(app) = app {
                    desktop.set_resize_drag(app);
                    kprintln!("  desktop     : resizing '{}'", name_of(desktop, app));
                }
            }
            Hit::Title => {
                desktop.set_drag(app);
                if let Some(app) = app {
                    kprintln!("  desktop     : drag '{}'", name_of(desktop, app));
                }
            }
            // Щелчок по содержимому: его разбирает само содержимое — в
            // «Параметрах» им выбирают раздел и нажимают пункты.
            Hit::Body => {
                let scale = desktop.scale();
                let changed = match desktop.focused_mut() {
                    // Окно программы получает щелчок событием, а не
                    // перерисовкой: что нарисовать в ответ, решает она.
                    Some(window) if window.is_program() => {
                        // Координаты переводятся в систему поверхности: окно
                        // двигают по столу, и программа, считающая в экранных,
                        // промахивалась бы мимо собственных кнопок всякий раз,
                        // когда окно сдвинули.
                        let local = (
                            x - window.rect.x,
                            y - window.rect.y - Window::title_height(scale) as i32,
                        );
                        // Единица — левая кнопка, и других здесь не бывает:
                        // правая открывает меню стола и до окна не доходит.
                        window.push_event(WinEvent {
                            kind: WIN_POINTER,
                            code: 1,
                            x: local.0,
                            y: local.1,
                        });
                        false
                    }
                    Some(window) => {
                        window.handle_click(x, y);
                        window.took_theme_change()
                    }
                    None => false,
                };
                // Смена темы — единственное, что окно меняет за своими
                // границами. Перекрашивать стол изнутри окна оно не может:
                // остальные окна, панель и обои принадлежат композитору,
                // и окно о них не знает.
                if changed {
                    kprintln!(
                        "  desktop     : theme {}",
                        if theme::is_dark() { "dark" } else { "light" }
                    );
                    desktop.restyle(status);
                }
            }
        }
        desktop.refresh_panel(status);
    }
}

/// Ответить на вопрос окна подтверждения.
///
/// Возвращает `true`, если клавиша была ответом: всё остальное окно
/// подтверждения игнорирует — набирать в нём нечего.
///
/// Само выключение здесь **не** происходит. Эта функция работает под замком
/// рабочего стола, взятым с запрещёнными прерываниями, а выключение сбрасывает
/// том на диск и ждёт ответа контроллера — то есть ждёт прерывания, которого в
/// этом состоянии не будет. Поэтому здесь поднимается просьба, а гасит систему
/// задача (см. [`crate::power`]).
fn confirm_key(desktop: &mut Compositor, app: App, restart: bool, code: KeyCode) -> bool {
    match code {
        KeyCode::Y => {
            if desktop.close(app) {
                kprintln!("  desktop     : closed '{}'", app.title());
            }
            crate::power::request(restart, crate::power::Source::Desktop);
            true
        }
        KeyCode::N | KeyCode::Escape => {
            if desktop.close(app) {
                kprintln!("  desktop     : closed '{}'", app.title());
            }
            kprintln!("  desktop     : '{}' cancelled", app.title());
            true
        }
        _ => false,
    }
}

/// Отдать событие активному окну.
fn route(desktop: &mut Compositor, event: KeyEvent, status: &Status) -> Option<KeyEvent> {
    // Окно подтверждения разбирает клавиши само и раньше остальных: у него нет
    // содержимого, которое стоило бы прокручивать, зато есть ровно два ответа.
    if let Some(app) = desktop.focused_app() {
        if let Some(restart) = app.confirms_power() {
            if event.pressed && confirm_key(desktop, app, restart, event.code) {
                log_focus(desktop);
                desktop.refresh_panel(status);
                desktop.present();
            }
            return None;
        }
    }

    match desktop.focused_app() {
        // Окна оболочки может не быть вовсе (его закрыли) — тогда ввод всё
        // равно уходит ей: иначе система осталась бы без единственного места,
        // где можно набрать команду.
        Some(App::Terminal) | None => Some(event),
        // Окно программы получает символ, а не код клавиши, и только нажатия.
        // Клавиша без символа — стрелки, F-ряд — события не даёт вовсе: класть
        // им ноль значило бы сделать их все одной клавишей (см. договор
        // [`WinEvent::code`]).
        Some(app @ App::Program(_)) => {
            if event.pressed {
                if let Some(symbol) = event.to_char() {
                    let key = WinEvent { kind: WIN_KEY, code: symbol as u32, x: 0, y: 0 };
                    if let Some(window) = desktop.find(app) {
                        window.push_event(key);
                    }
                }
            }
            None
        }
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
        // Влево-вправо ходят между столбцами: слева окна стола, справа
        // программы из `/bin`. Обход обоих списков одними стрелками вверх-вниз
        // означал бы двадцать нажатий на дорогу от последней программы обратно
        // к «Терминалу».
        KeyCode::Right => {
            if !menu.switch_column(true) {
                return;
            }
        }
        KeyCode::Left => {
            if !menu.switch_column(false) {
                return;
            }
        }
        KeyCode::Enter => {
            launching = menu.selection();
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
    if let Some(choice) = launching {
        run_choice(desktop, choice);
    }
    desktop.refresh_panel(status);
    desktop.present();
}

/// Выполнить то, что выбрали в меню запуска.
///
/// Окно стола открывается здесь же, а программа третьего кольца запускается и
/// **поднимает окно оболочки**: она разговаривает строками, и оставить её
/// говорить в закрытое окно значило бы запустить программу, ответа которой
/// нигде не видно. Ждать её нельзя — этот код работает внутри разбора события
/// ввода.
fn run_choice(desktop: &mut Compositor, choice: panel::Choice) {
    match choice {
        panel::Choice::App(app) => launch(desktop, app),
        panel::Choice::Program(name) => {
            let path = alloc::format!("/bin/{name}");
            match crate::user::spawn(&path, crate::user::session::credentials()) {
                Ok(id) => kprintln!("  desktop     : started '{path}' as {id}"),
                Err(err) => kprintln!("  desktop     : cannot start '{path}': {err}"),
            }
            launch(desktop, App::Terminal);
        }
    }
}

/// Запустить программу: поднять её окно или создать новое.
fn launch(desktop: &mut Compositor, app: App) {
    if let Some(index) = desktop.index_of(app) {
        // Свёрнутое окно возвращается на экран, а не просто поднимается: иначе
        // кнопка в панели задач у свёрнутого окна не делала бы ничего видимого.
        if desktop.restore(app) {
            kprintln!("  desktop     : restored '{}'", app.title());
        }
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

/// Открыть значок: системный — окном программы, файл и каталог — менеджером.
///
/// Отдельная функция, а не ветка внутри разбора щелчка, потому что открывают
/// значок двумя дорогами — двойным щелчком и пунктом «Open», — и разошедшиеся
/// дороги к одному действию расходятся окончательно в тот день, когда одну из
/// них поправят.
fn open_icon(desktop: &mut Compositor, index: usize) {
    match desktop.icon_kind(index) {
        Some(icons::Kind::App(app)) => launch(desktop, app),
        Some(kind) => {
            let Some(path) = desktop.icon_path(index) else {
                return;
            };
            let directory = kind == icons::Kind::Folder;
            launch(desktop, App::Files);
            if let Some(window) = desktop.find(App::Files) {
                window.reveal(&path, directory);
            }
            kprintln!("  desktop     : opened '{path}'");
        }
        None => {}
    }
}

/// Показать в открытом файловом менеджере то, что изменилось на диске.
fn refresh_files_window(desktop: &mut Compositor) {
    if let Some(window) = desktop.find(App::Files) {
        window.refresh_files();
    }
}

/// Выполнить пункт меню стола.
///
/// Меню остаётся открытым и показывает ответ: «создал папку» и «отказано в
/// правах» — оба ответа человек обязан увидеть, а закрывшееся меню не сказало
/// бы ни того, ни другого.
fn context_action(desktop: &mut Compositor, action: context::Action, status: &Status) {
    match action {
        context::Action::Open => {
            desktop.close_context();
            if let Some(index) = desktop.icon_selection() {
                open_icon(desktop, index);
            }
            desktop.refresh_panel(status);
        }
        // «Переименовать» и «удалить» ничего не делают сразу: первое просит
        // имя, второе — подтверждения. Действие происходит в ответе на
        // клавишу, см. [`handle_context_key`].
        context::Action::Rename => match selected_entry(desktop) {
            Some((_, label)) => desktop.context_rename(&label),
            None => desktop.context_note("nothing is selected"),
        },
        context::Action::Delete => match selected_entry(desktop) {
            Some((_, label)) => desktop.context_confirm(&label),
            None => desktop.context_note("nothing is selected"),
        },
        context::Action::NewFolder | context::Action::NewTextFile => {
            let directory = action == context::Action::NewFolder;
            match context::create_entry(directory) {
                Ok(name) => {
                    kprintln!(
                        "  desktop     : created '{}' in {}",
                        name,
                        context::desktop_dir()
                    );
                    desktop.context_note(&alloc::format!("created {name}"));
                    // Созданное обязано появиться и на столе, и в открытом
                    // менеджере: иначе «создал» видно только на слово.
                    desktop.reload_icons();
                    log_icons(desktop);
                    refresh_files_window(desktop);
                }
                Err(err) => {
                    kprintln!("  desktop     : cannot create: {err}");
                    desktop.context_note(&err);
                }
            }
        }
        context::Action::Theme => {
            desktop.close_context();
            let dark = !theme::is_dark();
            theme::set_dark(dark);
            kprintln!(
                "  desktop     : theme {}",
                if dark { "dark" } else { "light" }
            );
            desktop.restyle(status);
        }
        context::Action::DisplaySettings => {
            desktop.close_context();
            launch(desktop, App::Settings);
            if let Some(window) = desktop.find(App::Settings) {
                window.show_display_settings();
            }
            desktop.refresh_panel(status);
        }
        context::Action::Refresh => {
            desktop.close_context();
            // «Обновить» на рабочем столе — это перечитать каталог, а не только
            // перерисовать пиксели: файл, созданный оболочкой, иначе появлялся
            // бы на столе неизвестно когда.
            desktop.reload_icons();
            log_icons(desktop);
            refresh_files_window(desktop);
            desktop.repaint_all();
            kprintln!("  desktop     : repainted");
        }
    }
}

/// Что выбрано на столе, если это файл или каталог: путь и подпись.
///
/// Системный значок сюда не попадает: у него нет пути, а переименовать
/// «Settings» нечем.
fn selected_entry(desktop: &Compositor) -> Option<(String, String)> {
    let index = desktop.icon_selection()?;
    let path = desktop.icon_path(index)?;
    let label = desktop.icon_label(index)?;
    Some((path, label))
}

/// Разобрать клавишу, пока открыто меню стола.
///
/// Возвращает `true`, если клавиша меню понадобилась. `false` означает, что её
/// разберёт кто-нибудь ещё, — тогда меню остаётся открытым, и это намеренно:
/// закрывать его на всякую незнакомую клавишу значило бы терять набранное имя
/// от случайного нажатия.
fn handle_context_key(desktop: &mut Compositor, event: KeyEvent, status: &Status) -> bool {
    match desktop.context_key(event) {
        context::Reply::Ignored => false,
        context::Reply::Handled => {
            desktop.present();
            true
        }
        context::Reply::Close => {
            desktop.close_context();
            kprintln!("  desktop     : context menu closed");
            desktop.present();
            true
        }
        context::Reply::Run(action) => {
            context_action(desktop, action, status);
            desktop.refresh_panel(status);
            desktop.present();
            true
        }
        context::Reply::Rename(name) => {
            match selected_entry(desktop) {
                Some((path, _)) => match context::rename_entry(&path, &name) {
                    Ok(target) => {
                        let new_name = context::base_name(&target);
                        kprintln!("  desktop     : renamed '{path}' to '{new_name}'");
                        desktop.context_note(&alloc::format!("renamed to {new_name}"));
                        desktop.reload_icons();
                        // Выделение остаётся на том же файле под новым именем:
                        // иначе следующий пункт меню — «удалить» — относился бы
                        // к пустоте, и человек, переименовавший файл и решивший
                        // его убрать, получил бы «ничего не выбрано».
                        desktop.select_icon_path(&target);
                        log_icons(desktop);
                        refresh_files_window(desktop);
                    }
                    Err(err) => {
                        kprintln!("  desktop     : cannot rename '{path}': {err}");
                        desktop.context_note(&err);
                    }
                },
                None => desktop.context_note("nothing is selected"),
            }
            desktop.present();
            true
        }
        context::Reply::Delete => {
            match selected_entry(desktop) {
                Some((path, label)) => match context::delete_entry(&path) {
                    Ok(()) => {
                        kprintln!("  desktop     : deleted '{path}'");
                        desktop.context_note(&alloc::format!("deleted {label}"));
                        desktop.reload_icons();
                        log_icons(desktop);
                        refresh_files_window(desktop);
                    }
                    Err(err) => {
                        kprintln!("  desktop     : cannot delete '{path}': {err}");
                        desktop.context_note(&err);
                    }
                },
                None => desktop.context_note("nothing is selected"),
            }
            desktop.present();
            true
        }
    }
}

/// Записать в журнал, что сейчас лежит на столе.
///
/// Не украшение вывода: содержимое каталога стола видно только глазами, а
/// снимок экрана доказательством не считается — он показывает последний
/// нарисованный кадр. Эта строка — единственный способ проверить, что созданный
/// файл появился на столе, а удалённый исчез.
fn log_icons(desktop: &Compositor) {
    let (total, entries) = desktop.icon_counts();
    kprintln!(
        "  desktop     : icons {total}, {entries} from {}",
        context::desktop_dir()
    );
}

/// Записать в журнал, какое окно стало активным.
///
/// Не украшение вывода: рабочий стол — единственная часть системы, у которой
/// нет собственного текстового вывода, и без этих строк проверить её мог бы
/// только человек, глядящий на экран. Снимок экрана доказательством не является
/// — он показывает последний нарисованный кадр, а не текущее состояние.
fn log_focus(desktop: &Compositor) {
    if let Some(app) = desktop.focused_app() {
        kprintln!("  desktop     : focus '{}'", name_of(desktop, app));
    }
}

// ---------------------------------------------------------------------------
// Окна пользовательских программ (фаза 47a)
// ---------------------------------------------------------------------------

/// Почему окно программе не досталось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowError {
    /// Графики на машине нет вовсе — окну негде быть.
    NoDesktop,
    /// Стол занят сборкой кадра. Ответ временный: стоит попробовать ещё раз.
    ///
    /// Отдельно от [`WindowError::NoDesktop`] намеренно: «окна не будет
    /// никогда» и «окна нет сию секунду» — разные ответы, и программа,
    /// получившая на второй первый, зря закончила бы работу.
    Busy,
    /// У этой задачи окно уже есть. Окно у программы одно — см. [`App::Program`].
    Exists,
    /// Окна с таким номером нет.
    NoWindow,
    /// Не хватило памяти под поверхность окна.
    NoMemory,
}

/// Чем ответить, когда стол не дался.
fn unavailable() -> WindowError {
    if graphics() { WindowError::Busy } else { WindowError::NoDesktop }
}

/// Попросить окно программы закрыться. `true` — просьба ушла в очередь.
///
/// `false` означает «это не окно программы», а не «не получилось»: обычное окно
/// закрывают на месте, спрашивать там некого.
fn request_close(desktop: &mut Compositor, app: App) -> bool {
    if !matches!(app, App::Program(_)) {
        return false;
    }
    let Some(window) = desktop.find(app) else {
        return false;
    };
    window.push_event(WinEvent { kind: WIN_CLOSE, code: 0, x: 0, y: 0 });
    true
}

/// Завести окно задаче `task` поверх её поверхности.
///
/// Пиксели принадлежат не окну: [`Surface`] здесь заимствованная, построенная
/// поверх кадров, которые выдал и которыми владеет [`crate::user`]. Окно живёт
/// ровно столько же, сколько они, и снимает их тот же путь, что снимает окно.
pub fn open_window(task: u32, title: &str, pixels: Surface) -> Result<(), WindowError> {
    let app = App::Program(task);
    let (w, h) = (pixels.width(), pixels.height());
    with_desktop(|desktop| {
        if desktop.index_of(app).is_some() {
            return Err(WindowError::Exists);
        }
        let scale = desktop.scale();
        // Окно выше поверхности на полосу заголовка: программа просит место
        // **под содержимое**, а полосу рисует ядро. Считать иначе значило бы
        // отдавать ей окно, в котором её же рисунок наполовину под заголовком.
        let full_h = h.saturating_add(Window::title_height(scale));
        let screen = desktop.screen_width();
        let work = desktop.work_bottom().max(1) as u32;
        let rect = Rect::new(
            ((screen.saturating_sub(w)) / 2) as i32,
            ((work.saturating_sub(full_h)) / 2) as i32,
            w,
            full_h,
        );
        let window =
            Window::program(app, rect, scale, title, pixels).ok_or(WindowError::NoMemory)?;
        desktop.push(window);
        kprintln!(
            "  desktop     : opened '{title}' for {}",
            crate::sched::TaskId::new(task)
        );
        log_window(desktop, app, true);
        desktop.refresh_panel(&status_now());
        desktop.present();
        Ok(())
    })
    .unwrap_or_else(|| Err(unavailable()))
}

/// Показать на экране то, что программа нарисовала в своей поверхности.
///
/// `area` — в координатах поверхности, `None` — «всё окно». Вылезающее за её
/// край обрезается, а не отвергается: программа считает в своих координатах и о
/// полосе заголовка не знает ничего.
pub fn commit_window(task: u32, area: Option<Rect>) -> Result<(), WindowError> {
    let app = App::Program(task);
    with_desktop(|desktop| {
        let Some(window) = desktop.find(app) else {
            return Err(WindowError::NoWindow);
        };
        window.commit(area);
        desktop.present();
        Ok(())
    })
    .unwrap_or_else(|| Err(unavailable()))
}

/// Забрать у окна самое старое событие.
///
/// `None` — событий нет **или** стол сейчас занят. Разница здесь неважна и
/// потому не возвращается: событие в очереди никуда не денется, а программа
/// спросит снова — она и так спрашивает в цикле.
pub fn next_window_event(task: u32) -> Option<WinEvent> {
    with_desktop(|desktop| desktop.find(App::Program(task))?.pop_event()).flatten()
}

/// Убрать окно задачи со стола.
///
/// Пиксели после этого никто не читает — и только поэтому кадры под ними можно
/// возвращать в пул. Порядок обязателен: сначала окно уходит со стола, потом
/// освобождается память, а не наоборот.
pub fn close_window(task: u32) -> Result<(), WindowError> {
    let app = App::Program(task);
    with_desktop(|desktop| {
        if !desktop.close(app) {
            return Err(WindowError::NoWindow);
        }
        kprintln!(
            "  desktop     : closed the window of {}",
            crate::sched::TaskId::new(task)
        );
        log_focus(desktop);
        desktop.refresh_panel(&status_now());
        desktop.present();
        Ok(())
    })
    .unwrap_or_else(|| Err(unavailable()))
}

/// Как окно называется в журнале.
///
/// У окон ядра это [`App::title`] — латиница, потому что журнал читает
/// разработчик, и по нему же наводит мышь автоматический стенд. У окна программы
/// имени в перечислении нет вовсе: оно своё и приехало из самой программы, а
/// печатать вместо него родовое «Program» значило бы, что два разных окна в
/// журнале неразличимы — и что стенд, целящийся по имени, не найдёт ни одного.
fn name_of(desktop: &Compositor, app: App) -> String {
    match app {
        App::Program(_) => desktop.caption_of(app).unwrap_or_else(|| String::from(app.title())),
        _ => String::from(app.title()),
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
    // У окна программы имя своё, и в журнал едет именно оно: [`App::title`]
    // знает только родовое слово «Program», а стенд наводит мышь по имени.
    let own = match app {
        App::Program(_) => desktop.caption_of(app),
        _ => None,
    };
    kprintln!(
        "  window      : '{}' at {},{} {}x{}{}",
        own.as_deref().unwrap_or(app.title()),
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        if focused { " (focused)" } else { "" }
    );
}
