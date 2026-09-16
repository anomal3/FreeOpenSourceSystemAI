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
//! 2. сочетания оконного менеджера (Meta, Alt+Tab, Ctrl+W, Ctrl+стрелки);
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

// Словарь элементов (`paint`) и палитра (`theme`) с фазы 47c живут в
// `mini_ui`, а не здесь. Переезд не косметический: окно перестало быть частью
// ядра, и рисовать теми же цветами обязаны обе стороны границы привилегий —
// иначе файловый менеджер выглядит на этом столе чужой программой. Зависимостей
// от ядра у них не было ни одной, так что переезд свёлся к переносу двух файлов.
pub mod compositor;
pub mod context;
pub mod dialog;
pub mod icons;
pub mod panel;
pub mod pointer;
pub mod prefs;
pub mod settings;
pub mod flight;
pub mod keyboard;
pub mod shade;
pub mod tray;
pub mod statusbar;
pub mod term;
pub mod window;

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mini_ui::theme;
use mini_ui::{Rect, Screen, Surface};
use user_abi::{WIN_CLOSE, WIN_KEY, WIN_LEAVE, WIN_MOVE, WIN_POINTER, WinEvent};

use crate::input::keymap;
use crate::input::{Buttons, KeyCode, KeyEvent, Modifiers, PointerEvent};
use crate::sync::SpinLock;
use crate::{arch, kprintln, mm};

use compositor::Compositor;
use panel::{NetState, PanelHit, Status, TrayItem};
use settings::Section;
pub use window::App;
use dialog::{AboutFacts, Answer, Dialog};
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

/// Фреймбуфер, на котором стол рисует сейчас: от прошивки или от последней
/// смены режима. Драйвер сверяет с ним, тот ли адаптер показывает картинку.
static FRAMEBUFFER: SpinLock<boot_info::Framebuffer> = SpinLock::new(boot_info::Framebuffer::NONE);

/// Фреймбуфер, на котором стол рисует сейчас (для снимка экрана по кабелю).
#[must_use]
pub fn framebuffer() -> boot_info::Framebuffer {
    *FRAMEBUFFER.lock()
}

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
    *FRAMEBUFFER.lock() = *fb;

    // Форма машины запоминается **до** масштаба и до палитры: от неё зависят и
    // множитель геометрии, и раскладка стола, и то, как открываются окна.
    // Спрашивается она у экрана, а не у настройки: человек, включивший систему
    // на телефоне, не должен сначала объяснять ей, что это телефон.
    theme::set_form(theme::form_for(screen.width(), screen.height()));
    let scale = theme::geometry_scale(screen.width(), screen.height());
    // Тема читается **до** создания композитора: палитра выбирается один раз,
    // при сборке первого кадра, и тема, применённая после, потребовала бы
    // перекрасить всё заново — то есть показать человеку вспышку чужого цвета
    // на каждой загрузке.
    prefs::adopt();
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
    // С фазы С3 в меню только то, у чего есть окно, а остальное из `/bin`
    // запускается в терминале. Оба числа в одной строке: «в меню семь строк»
    // без числа программ не говорит, сколько осталось за его пределами.
    kprintln!(
        "  desktop     : start menu lists {} items; /bin holds {} programs, the rest run from the terminal",
        desktop.menu_items(),
        desktop.menu_bin_programs()
    );

    // Размер сетки хранится отдельно от стола: спрашивают его программы — в том
    // числе тогда, когда стол занят перерисовкой. Обновляется он при каждом
    // возврате стола (см. `remember_shell_size`): окно можно растянуть.
    let cells = match desktop.find(App::Terminal) {
        Some(window) => window.size_in_cells(),
        None => (0, 0),
    };
    SHELL_CELLS.store(
        (u64::from(cells.0) << 32) | u64::from(cells.1),
        core::sync::atomic::Ordering::Relaxed,
    );
    SCREEN_SIZE.store(
        (u64::from(desktop.screen_width()) << 32) | u64::from(desktop.screen_height()),
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

/// Размер экрана в точках, упакованный в одно слово. Нули — графики нет.
///
/// Живёт здесь по той же причине, что и [`SHELL_CELLS`], и спрашивают его те
/// же: программа, открывающая окно, выбирает по нему и свой размер, и
/// множитель геометрии. Спросить его через захват стола значило бы отдать нули
/// тому, кто спросил во время перерисовки, — то есть окно в ноль точек, и
/// виновата была бы гонка, а не программа.
static SCREEN_SIZE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Размер экрана в точках. `(0, 0)` — графики на этой машине нет.
#[must_use]
pub fn screen_size() -> (u32, u32) {
    let packed = SCREEN_SIZE.load(core::sync::atomic::Ordering::Relaxed);
    ((packed >> 32) as u32, packed as u32)
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
    // Вывод, отложенный, пока стол был у другого, — раньше своего действия:
    // иначе строки программы встали бы на экран не в том порядке, в каком она
    // их напечатала.
    flush_deferred(&mut desktop);
    let result = action(&mut desktop);
    // Окно оболочки могли растянуть или развернуть — а всё, что двигает окна,
    // проходит здесь.
    remember_shell_size(&mut desktop);
    // Возвращать стол можно только с пустой очередью, и проверка идёт под тем
    // же замком, под которым пишущий кладёт в неё текст (см. [`write`]). Иначе
    // текст, положенный между «очередь пуста» и «стол на месте», ждал бы в
    // очереди до следующего кадра — а следующего могло и не быть.
    loop {
        let mut slot = DESKTOP.lock();
        let text = core::mem::take(&mut *DEFERRED.lock());
        if text.is_empty() {
            *slot = Some(desktop);
            return Some(result);
        }
        drop(slot);
        term::feed(desktop.find(App::Terminal), &text);
        desktop.present();
    }
}

/// Вывод в окно оболочки, пришедший, пока стол был вынут другой задачей.
///
/// # Дефект, ради которого очередь заведена (фаза С9)
///
/// [`write`] раньше в этом случае только разбирал текст — управляющие
/// последовательности применялись к состоянию терминала, а печатаемое
/// **пропадало**. Пока программы перерисовывали экран целиком на каждое
/// нажатие, потерю закрывал следующий кадр, и её никто не видел. `mc` фазы С9
/// шлёт только изменившиеся строки — и на x86_64 в отладочной сборке, где кадр
/// стола рисуется дольше, от панелей на экране оставалось три строки из
/// тридцати, при зелёном стенде.
///
/// Ждать стол в [`write`] нельзя: печатающий держит замок вывода оболочки и
/// просьбу не вытеснять его, и на одном процессоре задача, которая стол
/// вернёт, так и не получила бы времени. Поэтому текст кладётся сюда, а вливает
/// его тот, кто стол возвращает.
static DEFERRED: SpinLock<String> = SpinLock::new(String::new());

/// Сколько отложенного вывода держится, прежде чем лишнее отбрасывается.
///
/// Мегабайт — десятки полных экранов. Больше может набраться, только если стол
/// не возвращается вовсе, и тогда держать память бесполезно.
const DEFERRED_MAX: usize = 1024 * 1024;

/// Сколько байт вывода пришлось отбросить — с начала работы.
static DROPPED_OUTPUT: AtomicU64 = AtomicU64::new(0);

/// Запомнить размер окна оболочки в знаках — и назвать его, если он изменился.
///
/// # Дефект, ради которого это здесь (фаза С9)
///
/// Размер запоминался один раз, при запуске стола: «окна не меняют размера».
/// С тех пор окна научились и растягиваться за угол, и разворачиваться, а
/// [`shell_size`] продолжал отдавать размер от загрузки — `mc`, открытый в
/// развёрнутом терминале, рисовал панели в его левой трети.
fn remember_shell_size(desktop: &mut Compositor) {
    let cells = desktop.find(App::Terminal).map_or((0, 0), |window| window.size_in_cells());
    let packed = (u64::from(cells.0) << 32) | u64::from(cells.1);
    let before = SHELL_CELLS.swap(packed, Ordering::Relaxed);
    if before != packed && before != 0 && packed != 0 {
        kprintln!("  term        : window is now {}x{} cells", cells.0, cells.1);
    }
}

/// Влить отложенный вывод в окно оболочки.
fn flush_deferred(desktop: &mut Compositor) {
    loop {
        let text = core::mem::take(&mut *DEFERRED.lock());
        if text.is_empty() {
            return;
        }
        term::feed(desktop.find(App::Terminal), &text);
        desktop.present();
    }
}

/// Где на экране стоит окно программы.
fn layout(desktop: &Compositor, app: App) -> Rect {
    // Раскладка считается от размера экрана, а не задана числами: прошивка
    // вправе дать и 800×600, и 1920×1080, и окно, не помещающееся на экран,
    // выглядело бы как испорченная графика.
    let width = desktop.screen_width();
    let work = desktop.work_bottom().max(1) as u32;
    let margin = width / 24;

    // На телефоне окно занимает экран целиком — от строки состояния до дока.
    //
    // Не «так красивее»: окно в половину экрана на телефоне нельзя ни читать,
    // ни двигать. Двигать нечем — пальцем за полосу заголовка шириной в палец
    // же; читать нечего — в четверти экрана помещается три строки. Все
    // мобильные системы пришли к одному и тому же, и пришли не от вкуса.
    if theme::is_mobile() {
        let ctx = mini_ui::paint::Ctx::scaled(desktop.scale());
        let top = ctx.px(theme::M_STATUS_H + theme::M_INSET) as i32;
        let inset = ctx.px(theme::M_INSET);
        let w = width.saturating_sub(inset * 2).max(1);
        let h = (work - top.max(0) as u32).max(1);
        return Rect::new(inset as i32, top, w, h);
    }

    match app {
        App::Terminal => Rect::new(
            margin as i32,
            (work / 16) as i32,
            width * 5 / 8,
            work * 3 / 4,
        ),
        // «Параметры» — окно из двух колонок: разделы слева и настройки
        // справа. Ширина считается от них, а не долей экрана: боковая колонка
        // 240, содержимому нужно место под подпись, пояснение и элемент
        // справа, а предпросмотр стола — ещё 340. Окно уже этой суммы
        // показывало бы содержимое в щели между колонкой и краем.
        App::Settings => {
            let w = (width * 7 / 8).clamp(560, width.saturating_sub(margin).max(560));
            // Девять десятых рабочей области, а не четыре пятых, и это не вкус.
            // С фазы 48 разделов восемь, и у трёх из них содержимое высокое:
            // список часовых поясов, два поля сети с подписями, перечень томов.
            // На прежней высоте нижняя строка каждого из них уезжала за край —
            // клавиатурой она достижима (попадания считаются независимо от
            // видимости), а мышью нет вовсе. Раздел, которым нельзя
            // воспользоваться мышью, — это раздел, о котором никто не узнает.
            let h = (work * 9 / 10).max(360);
            Rect::new(
                ((width.saturating_sub(w)) / 2) as i32,
                (work / 24) as i32,
                w,
                h,
            )
        }
        // Размер диалога — от его содержимого, а не доля экрана: текст и
        // кнопки те же на любом мониторе, и на 1920 окно в половину ширины
        // было бы пустым полем с кнопками в дальнем углу.
        App::About => {
            let scale = desktop.scale();
            let w = (540 * scale).min(width);
            let h = (440 * scale).min(work);
            Rect::new(
                ((width - w) / 2) as i32,
                ((work.saturating_sub(h)) / 2) as i32,
                w,
                h,
            )
        }
        // Подтверждение встаёт по центру и заметно меньше остальных: это вопрос
        // с двумя кнопками, и окно размером с терминал выглядело бы как ещё одна
        // программа, а не как «система ждёт от вас ответа».
        App::Shutdown | App::Restart => {
            let scale = desktop.scale();
            let w = (500 * scale).min(width);
            let h = (260 * scale).min(work);
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
        App::Program(..) => {
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
        App::Settings => Window::settings(
            rect,
            scale,
            (desktop.screen_width(), desktop.screen_height()),
        ),
        App::About => Window::dialog(App::About, rect, scale, Dialog::About(about_facts(desktop))),
        App::Shutdown | App::Restart => {
            Window::dialog(app, rect, scale, Dialog::Power { restart: app == App::Restart })
        }
        // Окно программы строится не здесь, и построить его столу нечем: ему
        // нужны кадры, которые выдаёт [`crate::user`]. Сюда приходят с меню и с
        // панели задач, а там окно программы к этому времени уже существует —
        // кнопка на панели без окна не рисуется.
        App::Program(..) => None,
        other => Window::text(other, rect, scale),
    }
}

/// Память на момент последнего обновления панели, в МиБ.
///
/// Копия, а не вопрос к пулу кадров: окно строится под замком стола, а пул
/// кадров живёт за своим замком, и под ним бывает вывод, который доходит до
/// терминала на столе. Спросить пул отсюда — значит взять два замка в порядке,
/// обратном тому, в каком их берёт вывод. [`status_now`] спрашивает его вне
/// замка стола при каждом обновлении панели и оставляет числа здесь.
static LAST_FREE_MIB: AtomicU64 = AtomicU64::new(0);
static LAST_TOTAL_MIB: AtomicU64 = AtomicU64::new(0);

/// Сведения для окна «О системе» — на момент его открытия.
fn about_facts(desktop: &Compositor) -> AboutFacts {
    AboutFacts {
        version: crate::VERSION,
        arch: arch::ARCH_NAME,
        screen: (desktop.screen_width(), desktop.screen_height()),
        free_mib: LAST_FREE_MIB.load(Ordering::Relaxed),
        total_mib: LAST_TOTAL_MIB.load(Ordering::Relaxed),
        uptime_ms: crate::time::uptime_ms(),
    }
}


/// Что показывает панель справа.
///
/// Считается **до** захвата замка стола: счётчики памяти живут за своим замком,
/// и брать два замка во вложенном порядке — это способ однажды получить
/// взаимную блокировку.
/// Сколько памяти свободно и сколько её всего, в мегабайтах.
///
/// Из атомиков, а не из [`mm::frame::stats`], и это важно: спрашивают их при
/// сборке кадра, то есть под замком стола, а `stats` берёт свой. Числа кладёт
/// сюда [`status_now`], который вызывается снаружи всех замков стола.
#[must_use]
pub fn memory_mib() -> (u64, u64) {
    (
        LAST_FREE_MIB.load(Ordering::Relaxed),
        LAST_TOTAL_MIB.load(Ordering::Relaxed),
    )
}

/// Часы, а если их нет — время работы.
///
/// Машина без часов реального времени (телефон — ровно такая: прошивка времени
/// не отдаёт) знает только, сколько она работает. Показать это честнее, чем
/// оставить пустое место или нарисовать выдуманное время.
#[must_use]
pub fn clock_or_uptime() -> String {
    crate::time::clock_text().unwrap_or_else(|| statusbar::uptime_text(crate::time::uptime_ms()))
}

fn status_now() -> Status {
    let frames = mm::frame::stats();
    let free_mib = (frames.free_bytes() / (1024 * 1024)) as u64;
    let total_mib = (frames.total_bytes() / (1024 * 1024)) as u64;
    LAST_FREE_MIB.store(free_mib, Ordering::Relaxed);
    LAST_TOTAL_MIB.store(total_mib, Ordering::Relaxed);
    Status {
        clock: crate::time::clock_text(),
        uptime_ms: crate::time::uptime_ms(),
        net: net_state(),
    }
}

/// Сеть для значка в трее — по двум флагам без замка.
///
/// Первая версия спрашивала `net::status()` и брала замок сети на каждое
/// событие ввода. С сетевой картой машина вставала намертво посреди набора
/// адреса в «Параметрах»: замок сети держит и сетевая задача, и встреча двух
/// задач на нём с выключенными прерываниями — это остановка всего.
fn net_state() -> NetState {
    if !crate::net::is_present() {
        NetState::Absent
    } else if crate::net::has_address() {
        NetState::Up
    } else {
        NetState::NoAddress
    }
}

/// Клавиша с логотипом нажата, и с тех пор не нажимали ничего другого.
///
/// Меню открывает её **отпускание**, как в Windows: нажатие само по себе
/// ничего не значит, потому что за ним может идти Пробел (раскладка) — и
/// открывать меню, чтобы тут же закрыть, значило бы мигать им на каждое
/// Win+Пробел.
static META_ARMED: AtomicBool = AtomicBool::new(false);

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
    let shown = with_desktop(|desktop| {
        term::feed(desktop.find(App::Terminal), text);
        desktop.present();
    });
    if shown.is_some() {
        return;
    }

    // Стол у другой задачи: текст ждёт в очереди, и вольёт его тот, кто стол
    // вернёт (см. [`DEFERRED`]). Проверка «стол всё ещё вынут» и запись в
    // очередь — под одним замком стола: если стол успели вернуть, пока мы шли
    // сюда, очередь уже никто не опустошит, и надо рисовать самим.
    let slot = DESKTOP.lock();
    if slot.is_some() {
        drop(slot);
        write(text);
        return;
    }
    let mut queue = DEFERRED.lock();
    if queue.len() + text.len() <= DEFERRED_MAX && queue.try_reserve(text.len()).is_ok() {
        queue.push_str(text);
        return;
    }
    drop(queue);
    drop(slot);
    // Разбор всё равно идёт: терминал обязан помнить, что ему сказали, иначе
    // последовательность, разрезанная потерей, испортила бы и следующий вывод.
    term::feed(None, text);
    let before = DROPPED_OUTPUT.fetch_add(text.len() as u64, Ordering::Relaxed);
    if before == 0 {
        // Прямо в серийную линию: `kprintln!` при поднятой графике попробовал бы
        // ещё и нарисовать — а стол сейчас у другого.
        crate::serial::_print(format_args!(
            "  term        : dropped output while the desktop was away (queue full)\n"
        ));
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

/// Во что обходятся кадры: кадры, полосы, наносекунды сборки и вывода, точки.
///
/// # Почему атомики, а не поля стола
///
/// Потому что спрашивают об этом **тогда, когда стол занят**, и только тогда
/// это и интересно. [`with_desktop`] стол не одалживает, а **вынимает** из-под
/// замка на всё время работы — пока идёт разбор касания и сборка кадра, любой
/// другой спрашивающий получает `None`. Первая версия замера жила полями
/// композитора, и по кабелю она отвечала «стола на этой машине нет» ровно в те
/// мгновения, ради которых её и заводили.
///
/// Те же грабли уже обошли числа памяти в строке состояния — см.
/// [`memory_mib`].
static FRAMES: AtomicU64 = AtomicU64::new(0);
static BANDS: AtomicU64 = AtomicU64::new(0);
static DRAW_NS: AtomicU64 = AtomicU64::new(0);
static BLIT_NS: AtomicU64 = AtomicU64::new(0);
static POINTS: AtomicU64 = AtomicU64::new(0);

/// Записать, во что обошлась одна полоса кадра. Зовётся из композитора.
pub fn note_band(draw_ns: u64, blit_ns: u64, points: u64) {
    BANDS.fetch_add(1, Ordering::Relaxed);
    DRAW_NS.fetch_add(draw_ns, Ordering::Relaxed);
    BLIT_NS.fetch_add(blit_ns, Ordering::Relaxed);
    POINTS.fetch_add(points, Ordering::Relaxed);
}

/// Записать, что кадр собран.
pub fn note_frame() {
    FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// Во что обошлись слои одной полосы: обои, значки, окна (из них тени), верх.
static WALL_NS: AtomicU64 = AtomicU64::new(0);
static ICONS_NS: AtomicU64 = AtomicU64::new(0);
static WINDOWS_NS: AtomicU64 = AtomicU64::new(0);
static SHADOW_NS: AtomicU64 = AtomicU64::new(0);
static TOP_NS: AtomicU64 = AtomicU64::new(0);

/// Записать, во что обошлись слои полосы.
pub fn note_layers(wall: u64, icons: u64, windows: u64, shadow: u64, top: u64) {
    WALL_NS.fetch_add(wall, Ordering::Relaxed);
    ICONS_NS.fetch_add(icons, Ordering::Relaxed);
    WINDOWS_NS.fetch_add(windows, Ordering::Relaxed);
    SHADOW_NS.fetch_add(shadow, Ordering::Relaxed);
    TOP_NS.fetch_add(top, Ordering::Relaxed);
}

/// Кадры целиком, кадры по кускам, переполнения учёта и сумма кусков.
static FULL_FRAMES: AtomicU64 = AtomicU64::new(0);
static PART_FRAMES: AtomicU64 = AtomicU64::new(0);
static OVERFLOWS: AtomicU64 = AtomicU64::new(0);
static PART_RECTS: AtomicU64 = AtomicU64::new(0);

/// Кадр перерисовал весь экран.
pub fn note_full_frame() {
    FULL_FRAMES.fetch_add(1, Ordering::Relaxed);
}

/// Кадр перерисовал `rects` кусков.
pub fn note_partial_frame(rects: u64) {
    PART_FRAMES.fetch_add(1, Ordering::Relaxed);
    PART_RECTS.fetch_add(rects, Ordering::Relaxed);
}

/// Площадь одного куска изменений — чтобы отличать «много мелких» от
/// «несколько во весь экран». Сумма точек уже есть, но она считается по
/// полосам, а полоса всегда во всю ширину экрана: по ней не видно, был ли сам
/// прямоугольник узким.
static RECT_POINTS: AtomicU64 = AtomicU64::new(0);

/// Записать площадь куска изменений.
pub fn note_rect(points: u64) {
    RECT_POINTS.fetch_add(points, Ordering::Relaxed);
}

/// Сколько точек всего пришлось на куски изменений.
#[must_use]
pub fn rect_points() -> u64 {
    RECT_POINTS.load(Ordering::Relaxed)
}

/// Учёт изменённого переполнился — дальше только полная перерисовка.
pub fn note_overflow() {
    OVERFLOWS.fetch_add(1, Ordering::Relaxed);
}

/// Полные кадры, частичные, переполнения, всего кусков в частичных.
#[must_use]
pub fn damage_timing() -> (u64, u64, u64, u64) {
    (
        FULL_FRAMES.load(Ordering::Relaxed),
        PART_FRAMES.load(Ordering::Relaxed),
        OVERFLOWS.load(Ordering::Relaxed),
        PART_RECTS.load(Ordering::Relaxed),
    )
}

/// Сколько полос кадра действительно задел док.
static DOCK_BANDS: AtomicU64 = AtomicU64::new(0);

/// Док скопирован в полосу.
pub fn note_dock_band() {
    DOCK_BANDS.fetch_add(1, Ordering::Relaxed);
}

/// Во что обошёлся док в этой полосе.
static DOCK_NS: AtomicU64 = AtomicU64::new(0);

/// Записать время дока.
pub fn note_dock_ns(ns: u64) {
    DOCK_NS.fetch_add(ns, Ordering::Relaxed);
}

/// Наносекунды, ушедшие на док.
#[must_use]
pub fn dock_ns() -> u64 {
    DOCK_NS.load(Ordering::Relaxed)
}

/// Сколько раз док копировался в полосу.
#[must_use]
pub fn dock_bands() -> u64 {
    DOCK_BANDS.load(Ordering::Relaxed)
}

/// Слои по отдельности — обои, значки, окна, из них тени, верхние слои.
#[must_use]
pub fn layer_timing() -> (u64, u64, u64, u64, u64) {
    (
        WALL_NS.load(Ordering::Relaxed),
        ICONS_NS.load(Ordering::Relaxed),
        WINDOWS_NS.load(Ordering::Relaxed),
        SHADOW_NS.load(Ordering::Relaxed),
        TOP_NS.load(Ordering::Relaxed),
    )
}

/// Кадры, полосы, наносекунды сборки и вывода, точки.
#[must_use]
pub fn timing() -> (u64, u64, u64, u64, u64) {
    (
        FRAMES.load(Ordering::Relaxed),
        BANDS.load(Ordering::Relaxed),
        DRAW_NS.load(Ordering::Relaxed),
        BLIT_NS.load(Ordering::Relaxed),
        POINTS.load(Ordering::Relaxed),
    )
}

/// Худший кадр с последнего сброса, в наносекундах — от начала `present` до
/// вывода последней полосы.
///
/// Среднее прячет ровно то, что человек называет «лагом»: сто кадров по 3 мс и
/// один на 200 мс дают в среднем 5 мс, а палец чувствует именно двести.
static WORST_FRAME_NS: AtomicU64 = AtomicU64::new(0);

/// Записать длительность кадра, если он хуже прежнего худшего.
pub fn note_frame_ns(ns: u64) {
    WORST_FRAME_NS.fetch_max(ns, Ordering::Relaxed);
}

/// Худший кадр, наносекунды.
#[must_use]
pub fn worst_frame_ns() -> u64 {
    WORST_FRAME_NS.load(Ordering::Relaxed)
}

/// Помечать переезд окна разностью, а не двумя прямоугольниками целиком.
///
/// Переключатель, а не просто новый код: сравнить оба пути надо на одной
/// прошивке и одном жесте (`oem ui move 0|1`, затем `oem drag`), иначе каждая
/// половина замера стоит перепрошивки и нажатия питания.
static EXACT_MOVES: AtomicBool = AtomicBool::new(true);

/// Включён ли новый учёт переезда.
#[must_use]
pub fn exact_moves() -> bool {
    EXACT_MOVES.load(Ordering::Relaxed)
}

/// Выбрать учёт переезда (`oem ui move 0|1`).
pub fn set_exact_moves(on: bool) {
    EXACT_MOVES.store(on, Ordering::Relaxed);
}

/// Идёт ли анимация — оболочке надо будить стол чаще (см. `shell::task`).
///
/// Атомиком, а не вопросом к столу: спрашивают на каждом витке цикла ввода,
/// а стол в этот миг может быть вынут из-под замка другой задачей.
static ANIMATING: AtomicBool = AtomicBool::new(false);

/// Идёт ли анимация прямо сейчас.
#[must_use]
pub fn animating() -> bool {
    ANIMATING.load(Ordering::Relaxed)
}

/// Отметить, идёт ли анимация. Зовёт композитор после каждого кадра.
pub fn set_animating(on: bool) {
    ANIMATING.store(on, Ordering::Relaxed);
}

/// Обнулить все счётчики кадров (`oem ui reset`).
///
/// Без этого соседние замеры смешиваются: счётчики накопительные, и жест,
/// измеренный вторым, делится на кадры первого.
pub fn reset_timing() {
    for counter in [
        &FRAMES, &BANDS, &DRAW_NS, &BLIT_NS, &POINTS, &WALL_NS, &ICONS_NS, &WINDOWS_NS,
        &SHADOW_NS, &TOP_NS, &FULL_FRAMES, &PART_FRAMES, &OVERFLOWS, &PART_RECTS,
        &RECT_POINTS, &DOCK_BANDS, &DOCK_NS, &WORST_FRAME_NS,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
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
    // Следующее событие уже в очереди — кадр под это собирать незачем: его
    // никто не увидит, следующий всё равно соберётся поверх. Без этого каждое
    // нажатие в «Параметрах» стоило полной сборки окна 1120×588 вместе с
    // обоями под ним, в отладочной сборке дольше, чем стенд ждёт между
    // клавишами. Очередь росла, «Применить» доезжало через полминуты, и на
    // x86_64 сценарий `settings` выглядел зависшим — снимок регистров показал
    // процессор в `draw_background` при включённых прерываниях.
    //
    // Последнее событие очереди кадр собирает само, даже если ничего не
    // нарисовало: отложенное предыдущими иначе ждало бы полсекунды до
    // [`tick`]. Так бывает всякий раз — за нажатием едет его отпускание, а
    // отпускание окна не перерисовывает.
    let defer = crate::input::has_events();
    for attempt in 0..INPUT_TRIES {
        let answer = with_desktop(|desktop| {
            desktop.set_deferred(defer);
            let answer = dispatch_on(desktop, event, &status);
            desktop.set_deferred(false);
            if !defer {
                desktop.present();
            }
            answer
        });
        if let Some(answer) = answer {
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
    // Alt+Shift — раскладка, и раньше всего остального: сочетание из двух
    // модификаторов не значит ничего ни для меню, ни для окна, а трей обязан
    // показать новую раскладку в том же кадре.
    if keymap::observe(event) {
        desktop.refresh_panel(status);
        desktop.present();
        return None;
    }

    // Клавиша с логотипом — см. [`META_ARMED`]. Разбирается до отпусканий:
    // работает именно отпускание.
    if matches!(event.code, KeyCode::LeftMeta | KeyCode::RightMeta) {
        if event.pressed {
            META_ARMED.store(true, Ordering::Relaxed);
        } else if META_ARMED.swap(false, Ordering::Relaxed) {
            toggle_menu(desktop, status);
        }
        return None;
    }

    // Отпускания стол не использует: все его действия происходят по нажатию.
    // Пропускать их дальше всё равно нужно — редактор строки различает нажатие
    // и отпускание сам, и молчаливая потеря половины событий однажды вылезет.
    if !event.pressed {
        return route(desktop, event, status);
    }
    META_ARMED.store(false, Ordering::Relaxed);

    // Win+Пробел — второе сочетание раскладки, то, к которому привыкли в
    // Windows 8 и позже. Alt+Shift выше — то, к которому привыкли раньше.
    if event.code == KeyCode::Space && event.mods.contains(Modifiers::META) {
        keymap::toggle_layout();
        desktop.refresh_panel(status);
        desktop.present();
        return None;
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
        // F1 — то же, что кнопка «FreeOS» на панели и клавиша с логотипом.
        // Продублирована не для удобства: Meta доходит не с каждой клавиатуры и
        // не через каждый эмулятор, а стол без меню — это набор окон.
        KeyCode::F1 => {
            toggle_menu(desktop, status);
            return None;
        }
        // Alt+Tab по кругу поднимает окна. До фазы N7h это делал голый Tab, и
        // окну программы он не доставался вовсе: форма WinForms не могла
        // перейти от поля к кнопке — первое, что пробует человек, пришедший из
        // Windows. Там окна переключает Alt+Tab, и здесь теперь так же, а Tab
        // без Alt идёт окну, как любая другая клавиша.
        KeyCode::Tab if event.mods.contains(Modifiers::ALT) => {
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

/// Сказать окну программы, что над его содержимым ходит указатель (фаза N7h).
///
/// Окну под указателем, а не окну в фокусе: подсказка у кнопки неактивного окна
/// всплывает и в Windows. Пока тащат окно или открыто меню стола, указатель
/// принадлежит им, и программе не достаётся ничего — иначе подсказка всплывала
/// бы под открытым меню запуска.
fn hover(desktop: &mut Compositor, x: i32, y: i32, buttons: Buttons) {
    let busy = desktop.dragging().is_some() || desktop.menu_open() || desktop.context_open();
    let under = if busy { None } else { desktop.program_under(x, y) };
    let now = under.map(|(app, _, _)| app);
    if let Some(was) = desktop.hovered().filter(|was| Some(*was) != now) {
        if let Some(window) = desktop.find(was) {
            window.push_event(WinEvent { kind: WIN_LEAVE, code: 0, x: 0, y: 0 });
        }
    }
    desktop.set_hovered(now);
    if let Some((app, local_x, local_y)) = under {
        let mut mask = 0;
        if buttons.contains(Buttons::LEFT) {
            mask |= 1;
        }
        if buttons.contains(Buttons::RIGHT) {
            mask |= 2;
        }
        if let Some(window) = desktop.find(app) {
            window.push_move(WinEvent { kind: WIN_MOVE, code: mask, x: local_x, y: local_y });
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
    if dx != 0 || dy != 0 {
        hover(desktop, x, y, event.buttons);
    }

    if event.pressed(Buttons::LEFT) {
        press(desktop, x, y, status);
    }
    // Правая кнопка: меню стола там, где щёлкнули. Пока оно открыто, левая
    // кнопка выбирает в нём пункт — этим и занимается `press`.
    if event.pressed(Buttons::RIGHT) {
        // Пункты зависят от того, во что целились. «Удалить», предложенное
        // тогда, когда ничего не выбрано, относилось бы неизвестно к чему —
        // а на столе это означало бы удалённый наугад файл. `None` — целились
        // в окно или в кнопку панели: у них своего меню пока нет.
        let target: Option<(&[context::Action], &str)> = if desktop.context_open() {
            desktop.close_context();
            None
        } else if let Some(hit) = desktop.panel_at(x, y) {
            match hit {
                PanelHit::Tray(_) | PanelHit::Empty => Some((&context::Action::ON_TRAY[..], "tray")),
                PanelHit::Menu
                | PanelHit::Window(_)
                | PanelHit::Missing(_)
                | PanelHit::Stack => None,
            }
        } else if let Some((index, hit)) = desktop.window_at(x, y) {
            // Правая кнопка внутри окна программы доходит до неё событием с
            // кодом 2 — так у файлового менеджера появляется своё меню. Окно
            // при этом поднимается, как и от левой: меню над окном, которое
            // не в фокусе, читалось бы как меню чужого окна.
            if hit == Hit::Body {
                desktop.raise(index);
                log_focus(desktop);
                let scale = desktop.scale();
                if let Some(window) = desktop.focused_mut() {
                    if window.is_program() {
                        let local = (
                            x - window.rect.x,
                            y - window.rect.y - Window::title_height(scale) as i32,
                        );
                        window.push_event(WinEvent {
                            kind: WIN_POINTER,
                            code: 2,
                            x: local.0,
                            y: local.1,
                        });
                    }
                }
                desktop.refresh_panel(status);
                desktop.present();
            }
            None
        } else {
            Some(match desktop.icon_at(x, y) {
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
            })
        };
        if let Some((items, what)) = target {
            // Открытое меню запуска закрывается: два меню разом — это два
            // ответа на один щелчок.
            if desktop.menu_open() {
                if let Some(menu) = desktop.menu_mut() {
                    menu.close();
                }
                kprintln!("  desktop     : menu closed");
                desktop.mark_menu_area();
                desktop.refresh_panel(status);
            }
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
        desktop.keyboard_release();
        // Размер, отложенный до кадра, применяется до того, как спросят, где и
        // какого размера окно: иначе отпускание описывало бы окно на шаг назад.
        desktop.settle_drag();
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

/// Напечатать то, что нажато на экранной клавиатуре.
///
/// Клавиши уходят в общую очередь ввода нажатием и отпусканием — так же, как их
/// кладёт USB-клавиатура (см. [`keyboard`]). Shift, нужный знаку, оборачивает
/// нажатие и отпускается сразу за ним: оставленный нажатым, он сделал бы
/// заглавными все следующие буквы с физической клавиатуры тоже.
fn type_on_screen(desktop: &mut Compositor, action: keyboard::Action) {
    use crate::input::{self, KeyCode};
    let tap = |code: KeyCode, shift: bool| {
        if shift {
            input::post(KeyCode::LeftShift, true);
        }
        input::post(code, true);
        input::post(code, false);
        if shift {
            input::post(KeyCode::LeftShift, false);
        }
    };
    match action {
        keyboard::Action::Key { code, shift } => {
            let letter = (KeyCode::A as u8..=KeyCode::Z as u8).contains(&(code as u8));
            tap(code, shift || (letter && desktop.keyboard_shifted()));
            if letter {
                desktop.keyboard_consume_shift();
            }
        }
        keyboard::Action::Backspace => tap(KeyCode::Backspace, false),
        keyboard::Action::Space => tap(KeyCode::Space, false),
        keyboard::Action::Enter => tap(KeyCode::Enter, false),
        keyboard::Action::Tab => tap(KeyCode::Tab, false),
        keyboard::Action::Chip(word) => {
            for letter in word.chars() {
                if let Some(code) = keyboard::code_for(letter) {
                    tap(code, false);
                }
            }
            kprintln!("  keyboard    : typed '{word}'");
        }
        // Состояние самой клавиатуры — уже учтено в `Keyboard::press`.
        keyboard::Action::Shift | keyboard::Action::Page => {}
    }
}

/// Разобрать нажатие левой кнопки.
///
/// Порядок проверок — сверху вниз по слоям кадра, и он обязан совпадать с
/// порядком рисования: щелчок должен доставаться тому, кого человек видит под
/// стрелкой. Обратный порядок означал бы, что кнопка, накрытая меню,
/// срабатывает сквозь него.
fn press(desktop: &mut Compositor, x: i32, y: i32, status: &Status) {
    // 0а. Шторка — поверх всего остального, и разбирается первой.
    //
    // Открытая шторка съедает нажатие целиком: нажатие внутрь пока ничего не
    // переключает (переключать нечего — см. `ui::shade`), а нажатие мимо её
    // закрывает. Пропустить его дальше значило бы открыть окно под шторкой,
    // которую человек всего лишь хотел убрать.
    if desktop.shade_open() {
        if !desktop.shade_contains(x, y) {
            desktop.toggle_shade();
            kprintln!("  desktop     : shade closed");
            desktop.present();
        }
        return;
    }

    // 0а''. Лист «Свёрнутые программы» — над доком. Нажатие мимо закрывает
    // его и дальше не идёт: иначе человек, просто убирающий лист, открыл бы
    // то, что под ним.
    if desktop.tray_open() {
        match desktop.tray_hit(x, y) {
            Some(tray::Hit::Restore(app)) => {
                desktop.close_tray();
                launch(desktop, app);
            }
            Some(tray::Hit::Close(app)) => {
                desktop.close_tray();
                let name = name_of(desktop, app);
                if request_close(desktop, app) {
                    kprintln!("  desktop     : close requested of '{name}'");
                } else if desktop.close(app) {
                    kprintln!("  desktop     : closed '{name}'");
                }
                log_focus(desktop);
            }
            Some(tray::Hit::RestoreAll) => {
                desktop.close_tray();
                for app in desktop.minimized_apps() {
                    launch(desktop, app);
                }
            }
            Some(tray::Hit::Inside) => return,
            None => {
                desktop.close_tray();
                kprintln!("  desktop     : minimized list closed");
            }
        }
        desktop.refresh_panel(status);
        desktop.present();
        return;
    }

    // 0а'. Экранная клавиатура: нажатие в неё — клавиша, и дальше не идёт.
    if desktop.keyboard_contains(x, y) {
        if let Some(action) = desktop.keyboard_press(x, y) {
            type_on_screen(desktop, action);
        }
        desktop.present();
        return;
    }

    // 0б. Строка состояния открывает шторку. Жеста «потянуть сверху» у нас
    // пока нет — жестов нет вовсе, — а нажатие в ту же полосу делает то же
    // самое и доступно с первого дня.
    if theme::is_mobile() {
        let bar = statusbar::bounds(desktop.screen_width(), desktop.scale());
        if bar.contains(x, y) {
            if desktop.toggle_shade() == Some(true) {
                kprintln!("  desktop     : shade opened");
                desktop.present();
            }
            return;
        }
    }

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
            PanelHit::Tray(item) => tray_click(desktop, item, status),
            // Кнопка есть, программы за ней нет. Говорится это вслух и
            // называется своим именем: «нажатие никуда не привело» человек
            // прочитает как поломку, а не как отсутствующую возможность.
            // Стопка свёрнутых: показать их списком. Пока список — это меню
            // запуска, где свёрнутые окна и так перечислены; отдельное окно
            // «Свёрнутые программы» из макета будет следующим шагом.
            PanelHit::Stack => {
                if desktop.toggle_tray() {
                    kprintln!("  desktop     : minimized list opened");
                }
            }
            PanelHit::Missing(what) => {
                kprintln!("  desktop     : {what} -- no program for it on this machine yet");
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
        // Пальцем открывают с одного раза. Двойной щелчок придуман мышью и для
        // мыши: там указатель стоит на месте между нажатиями, и второе попадает
        // туда же само собой. Палец между двумя нажатиями уходит с экрана, и
        // повторить попадание в ту же точку за полсекунды — задача, которую
        // человек не должен решать, чтобы открыть программу.
        if theme::is_mobile() || (same && now.saturating_sub(last) <= DOUBLE_CLICK_MS) {
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
                // Ответ диалога забирается здесь, а действует ниже: окно сейчас
                // заимствовано у стола, и закрыть его изнутри `match` нечем.
                let mut answer = None;
                let mut title_changed = false;
                let mut mode = None;
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
                        // Единица — левая кнопка; правая приходит двойкой из
                        // разбора правой кнопки ниже.
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
                        answer = window.take_answer();
                        let title = window.took_title_change();
                        let look = window.took_theme_change();
                        title_changed = title;
                        mode = window.took_mode_request();
                        look
                    }
                    None => false,
                };
                if let (Some(answer), Some(app)) = (answer, app) {
                    answer_dialog(desktop, app, answer);
                    log_focus(desktop);
                }
                // Смена вида — единственное, что окно меняет за своими
                // границами. Перекрашивать стол изнутри окна оно не может:
                // остальные окна, панель и обои принадлежат композитору,
                // и окно о них не знает.
                apply_look_change(desktop, changed, title_changed, status);
                if let Some(mode) = mode {
                    change_mode(desktop, mode, status);
                }
            }
        }
        desktop.refresh_panel(status);
    }
}

/// Ответить диалогу: закрыть окно и, если это было «да» на вопрос о питании,
/// поднять просьбу выключиться.
///
/// Строки журнала те же, что были у ответа буквами: по ним стенд узнаёт,
/// что окно закрыто и чем.
///
/// Само выключение здесь **не** происходит. Эта функция работает под замком
/// рабочего стола, взятым с запрещёнными прерываниями, а выключение сбрасывает
/// том на диск и ждёт ответа контроллера — то есть ждёт прерывания, которого в
/// этом состоянии не будет. Поэтому здесь поднимается просьба, а гасит систему
/// задача (см. [`crate::power`]).
fn answer_dialog(desktop: &mut Compositor, app: App, answer: Answer) {
    if desktop.close(app) {
        kprintln!("  desktop     : closed '{}'", app.title());
    }
    match (app.confirms_power(), answer) {
        (Some(restart), Answer::Confirm) => {
            crate::power::request(restart, crate::power::Source::Desktop);
        }
        (Some(_), Answer::Close) => kprintln!("  desktop     : '{}' cancelled", app.title()),
        (None, _) => {}
    }
}

/// Чем эта клавиша приедет в окно программы, и приедет ли вообще.
///
/// Символ — если он у клавиши есть; иначе имя из договора — если оно ей выдано.
/// `None` означает «эта клавиша программам не обещана», и таких большинство.
///
/// Порядок именно такой: сначала символ. Обратный сделал бы `Delete` именем
/// даже там, где клавиатура шлёт за него `0x7F`, — и программа получала бы одну
/// и ту же клавишу то так, то эдак, в зависимости от того, пришла она с
/// терминала или с настоящей клавиатуры.
fn program_key(event: KeyEvent) -> Option<u32> {
    if let Some(symbol) = event.to_char() {
        return Some(symbol as u32);
    }
    // Ctrl с цифрой управляющего символа не даёт, а сочетание `Ctrl+1` в
    // программах есть. Символом идёт сама цифра; что Ctrl был зажат, программа
    // узнаёт из маски (фаза N7h).
    if event.mods.contains(Modifiers::CTRL) {
        if let Some(plain) = keymap::latin(event.code) {
            return Some(plain as u32);
        }
    }
    Some(match event.code {
        // Backspace и Escape раскладка символом не отдаёт — намеренно, см.
        // `keymap`, — а договор окон обещает их программам символами `0x08`
        // и `0x1B`: так их шлёт всякий терминал, и так их ждёт «Файлы». До
        // фазы С4 обе клавиши здесь терялись, и «вверх» по Backspace в окне
        // программы не работал ни разу — заметно это стало только со стенда.
        KeyCode::Backspace => 0x08,
        KeyCode::Escape => 0x1B,
        KeyCode::Left => user_abi::WIN_KEY_LEFT,
        KeyCode::Right => user_abi::WIN_KEY_RIGHT,
        KeyCode::Up => user_abi::WIN_KEY_UP,
        KeyCode::Down => user_abi::WIN_KEY_DOWN,
        KeyCode::Home => user_abi::WIN_KEY_HOME,
        KeyCode::End => user_abi::WIN_KEY_END,
        KeyCode::PageUp => user_abi::WIN_KEY_PAGE_UP,
        KeyCode::PageDown => user_abi::WIN_KEY_PAGE_DOWN,
        KeyCode::Delete => user_abi::WIN_KEY_DELETE,
        KeyCode::Menu => user_abi::WIN_KEY_MENU,
        _ => return None,
    })
}

/// Модификаторы клавиши так, как их обещает договор окон.
fn program_mods(mods: Modifiers) -> i32 {
    let mut mask = 0;
    if mods.contains(Modifiers::SHIFT) {
        mask |= user_abi::WIN_MOD_SHIFT;
    }
    if mods.contains(Modifiers::CTRL) {
        mask |= user_abi::WIN_MOD_CTRL;
    }
    if mods.contains(Modifiers::ALT) {
        mask |= user_abi::WIN_MOD_ALT;
    }
    mask
}

/// Отдать событие активному окну.
fn route(desktop: &mut Compositor, event: KeyEvent, status: &Status) -> Option<KeyEvent> {
    // Диалог разбирает клавиши раньше остальных окон: набирать в нём нечего, а
    // ответ закрывает окно, и закрыть его может только стол.
    if let Some(app) = desktop.focused_app() {
        if app == App::About || app.confirms_power().is_some() {
            if event.pressed {
                // `Y` и `N` у вопроса о питании остались с тех пор, когда кнопок
                // не было: на них стоит стенд, а человеку они не мешают.
                let answer = match event.code {
                    KeyCode::Y if app.confirms_power().is_some() => Some(Answer::Confirm),
                    KeyCode::N if app.confirms_power().is_some() => Some(Answer::Close),
                    code => desktop.focused_mut().and_then(|window| {
                        window.handle_key(code);
                        window.take_answer()
                    }),
                };
                if let Some(answer) = answer {
                    answer_dialog(desktop, app, answer);
                    log_focus(desktop);
                    desktop.refresh_panel(status);
                }
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
        // Клавише без символа даётся имя из договора — но только той, которой
        // это имя выдано: остальные (F-ряд, цифровой блок) событий по-прежнему
        // не дают. См. [`WinEvent::code`], там сказано, почему это честнее.
        Some(app @ App::Program(..)) => {
            if event.pressed {
                if let Some(code) = program_key(event) {
                    // Модификаторы и буква клавиши — фаза N7h: без них сочетание
                    // `Ctrl+H` не отличить от Backspace, а `Ctrl+O` в русской
                    // раскладке — от `Ctrl+щ`. См. [`WinEvent::y`].
                    let latin = keymap::latin(event.code).map_or(0, |ch| ch as i32);
                    let key = WinEvent { kind: WIN_KEY, code, x: program_mods(event.mods), y: latin };
                    if let Some(window) = desktop.find(app) {
                        window.push_event(key);
                    }
                }
            }
            None
        }
        Some(_) => {
            if event.pressed {
                let outcome = desktop.focused_mut().map(|window| {
                    let handled = window.handle_key(event.code);
                    (
                        handled,
                        window.took_theme_change(),
                        window.took_title_change(),
                        window.took_mode_request(),
                    )
                });
                if let Some((handled, look, title, mode)) = outcome {
                    // С клавиатуры вид меняется так же, как мышью: раньше
                    // тема, выбранная стрелками и Enter, перекрашивала одно
                    // окно, а стол оставался прежним до следующего щелчка.
                    apply_look_change(desktop, look, title, status);
                    if let Some(mode) = mode {
                        change_mode(desktop, mode, status);
                    }
                    if handled {
                        desktop.present();
                    }
                }
            }
            None
        }
    }
}

/// Сменить режим экрана по просьбе «Параметров» (фаза С6a) и сказать окну,
/// чем кончилось.
fn change_mode(desktop: &mut Compositor, (width, height): (u32, u32), status: &Status) {
    let outcome = switch_mode(desktop, width, height, status);
    if let Err(why) = &outcome {
        kprintln!("  display     : {width}x{height} not set now: {why}");
    }
    if let Some(window) = desktop.find(App::Settings) {
        window.mode_applied((width, height), outcome);
    }
    desktop.present();
}

/// Переключить адаптер и перевезти на новый экран стол.
///
/// Порядок — от того, что может отказать без последствий, к тому, что
/// отменить нельзя: слои нового размера, адаптер, переезд стола.
fn switch_mode(desktop: &mut Compositor, width: u32, height: u32, status: &Status) -> Result<(), String> {
    let layers = desktop
        .prepare_screen(width, height)
        .ok_or_else(|| String::from("no memory for the new frame buffer"))?;
    let current = *FRAMEBUFFER.lock();
    let fb = crate::display::set_mode(&current, width, height).map_err(|err| alloc::format!("{err}"))?;
    // Размер буфера посчитан драйвером от того же режима, поэтому отказ здесь —
    // ошибка в драйвере, а не свойство машины. Сказать о ней всё равно надо.
    let Some(screen) = Screen::new(&fb) else {
        return Err(String::from("the new frame buffer does not add up"));
    };
    *FRAMEBUFFER.lock() = fb;
    crate::console::adopt(&fb);
    desktop.adopt_screen(screen, layers, status);

    let cells = match desktop.find(App::Terminal) {
        Some(window) => window.size_in_cells(),
        None => (0, 0),
    };
    SHELL_CELLS.store(
        (u64::from(cells.0) << 32) | u64::from(cells.1),
        core::sync::atomic::Ordering::Relaxed,
    );
    SCREEN_SIZE.store(
        (u64::from(width) << 32) | u64::from(height),
        core::sync::atomic::Ordering::Relaxed,
    );

    kprintln!("  display     : {width}x{height} set now by {}", crate::display::DRIVER);
    // Та же строка, что при запуске: по последней такой стенд наводит мышь.
    kprintln!(
        "  desktop     : {}x{}, ui scale {}, panel {} px",
        desktop.screen_width(),
        desktop.screen_height(),
        desktop.scale(),
        desktop.screen_height() as i32 - desktop.work_bottom(),
    );
    for entry in desktop.buttons() {
        log_window(desktop, entry.app, entry.focused);
    }
    Ok(())
}

/// Перекрасить стол после того, как «Параметры» сменили тему, акцент, обои или
/// высоту заголовка.
fn apply_look_change(desktop: &mut Compositor, look: bool, title: bool, status: &Status) {
    if title {
        kprintln!("  desktop     : title bar {} px", theme::title_h());
        desktop.retitle_all(status);
    } else if look {
        kprintln!(
            "  desktop     : theme {}, accent {}, wallpaper {}",
            if theme::is_dark() { "dark" } else { "light" },
            theme::accent().tag(),
            theme::wallpaper().tag()
        );
        desktop.restyle(status);
    }
}

/// Открыть или закрыть меню запуска.
fn toggle_menu(desktop: &mut Compositor, status: &Status) {
    // Открывается — значит пересобирается (фаза N8): пакет, поставленный после
    // запуска стола, обязан появиться в «Пуске» при следующем открытии, а не
    // после перезагрузки.
    if !desktop.menu_open() {
        desktop.reload_menu();
    }
    let Some(menu) = desktop.menu_mut() else {
        return;
    };
    let opened = menu.toggle();
    let packages = if opened { menu.package_names() } else { String::new() };
    kprintln!(
        "  desktop     : menu {}",
        if opened { "opened" } else { "closed" }
    );
    if !packages.is_empty() {
        kprintln!("  desktop     : start menu offers packages: {packages}");
    }
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
/// Окно стола открывается здесь же, а программа третьего кольца запускается.
///
/// Терминал при этом **не** поднимается. До фазы С3 поднимался: в меню стояли
/// и консольные программы, которые говорят строками, и без поднятого терминала
/// их ответа не было видно. Теперь в меню только программы с окном, и терминал,
/// выскочивший поверх их окна, выглядел бы как ошибка. Ждать программу нельзя —
/// этот код работает внутри разбора события ввода.
fn run_choice(desktop: &mut Compositor, choice: panel::Choice) {
    match choice {
        panel::Choice::App(app) => launch(desktop, app),
        panel::Choice::Program(name) => {
            let path = alloc::format!("/bin/{name}");
            match crate::user::spawn(&path, crate::user::session::credentials()) {
                Ok(id) => kprintln!("  desktop     : started '{path}' as {id}"),
                Err(err) => kprintln!("  desktop     : cannot start '{path}': {err}"),
            }
        }
        // Строка пакета — командная строка целиком (`/bin/dotnet /opt/…`), с
        // правами того же сеанса, что и у программ из `/bin`.
        panel::Choice::Command(line) => match crate::user::spawn(&line, crate::user::session::credentials()) {
            Ok(id) => kprintln!("  desktop     : started '{line}' as {id}"),
            Err(err) => kprintln!("  desktop     : cannot start '{line}': {err}"),
        },
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

/// Открыть значок: системный — окном ядра, программа и файл — запуском.
///
/// Отдельная функция, а не ветка внутри разбора щелчка, потому что открывают
/// значок двумя дорогами — двойным щелчком и пунктом «Open», — и разошедшиеся
/// дороги к одному действию расходятся окончательно в тот день, когда одну из
/// них поправят.
fn open_icon(desktop: &mut Compositor, index: usize) {
    match desktop.icon_kind(index) {
        Some(icons::Kind::App(app)) => launch(desktop, app),
        // Значок-запуск: команда лежит в пути записи. Так на столе живёт
        // файловый менеджер с фазы 47c — он программа, и открывать его окном
        // ядра больше нечем.
        Some(icons::Kind::Program(_)) => {
            let Some(command) = desktop.icon_path(index) else {
                return;
            };
            start(desktop, &command);
        }
        // Файл или каталог со стола. До 47c здесь поднималось окно ядра и ему
        // говорили `reveal`; теперь путь уезжает **аргументом** программе.
        // Разбор на слова делает сама задача, кавычек он не знает — имя с
        // пробелом приедет двумя аргументами, и это названный предел, а не
        // недосмотр: заводить разбор кавычек ради стола значит заводить его во
        // всей системе.
        Some(_) => {
            let Some(path) = desktop.icon_path(index) else {
                return;
            };
            start(desktop, &alloc::format!("/bin/files {path}"));
            kprintln!("  desktop     : opened '{path}'");
        }
        None => {}
    }
}

/// Запустить программу третьего кольца по готовой командной строке.
///
/// Ждать её нельзя — этот код работает внутри разбора события ввода, — поэтому
/// «получилось» здесь означает «задача заведена», а не «окно появилось». Окно
/// программа откроет сама, и сама же о нём скажет.
fn start(desktop: &mut Compositor, command: &str) {
    match crate::user::spawn(command, crate::user::session::credentials()) {
        Ok(id) => kprintln!("  desktop     : started '{command}' as {id}"),
        Err(err) => {
            kprintln!("  desktop     : cannot start '{command}': {err}");
            // Отказ виден только в журнале, а человек смотрит на стол. Поднять
            // оболочку — единственное, что можно сделать отсюда: там он
            // прочитает ту же строку.
            launch(desktop, App::Terminal);
        }
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
            // Записывается здесь же, а не «когда-нибудь потом»: тема, выбранная
            // из меню и не пережившая перезагрузку, — та же ошибка, что тема,
            // выбранная из окна и не пережившая её. Отказ записи не отменяет
            // смену: вид уже другой, и молчать о том, что он не запомнен,
            // нельзя, а откатывать — значит спорить с человеком.
            let saved = prefs::store_theme(dark);
            kprintln!(
                "  desktop     : theme {}{}",
                if dark { "dark" } else { "light" },
                match saved {
                    Ok(()) => "",
                    Err(_) => " (not saved: the root is read-only)",
                }
            );
            desktop.restyle(status);
        }
        context::Action::DisplaySettings => {
            desktop.close_context();
            open_settings(desktop, Section::Display, status);
        }
        context::Action::NetworkSettings => {
            desktop.close_context();
            open_settings(desktop, Section::Network, status);
        }
        context::Action::ClockSettings => {
            desktop.close_context();
            open_settings(desktop, Section::Clock, status);
        }
        context::Action::SwitchLayout => {
            desktop.close_context();
            keymap::toggle_layout();
            desktop.refresh_panel(status);
        }
        context::Action::TaskManager => {
            desktop.close_context();
            run_choice(desktop, panel::Choice::Program("taskmgr"));
            desktop.refresh_panel(status);
        }
        context::Action::Refresh => {
            desktop.close_context();
            // «Обновить» на рабочем столе — это перечитать каталог, а не только
            // перерисовать пиксели: файл, созданный оболочкой, иначе появлялся
            // бы на столе неизвестно когда.
            desktop.reload_icons();
            log_icons(desktop);
            desktop.repaint_all();
            kprintln!("  desktop     : repainted");
        }
    }
}

/// Щелчок по значку трея.
///
/// Язык переключается щелчком, как в Windows; сеть и часы открывают свой
/// раздел «Параметров» — ровно тот, а не окно с первого раздела.
fn tray_click(desktop: &mut Compositor, item: TrayItem, status: &Status) {
    match item {
        TrayItem::Layout => {
            keymap::toggle_layout();
            desktop.refresh_panel(status);
        }
        TrayItem::Network => open_settings(desktop, Section::Network, status),
        TrayItem::Clock => open_settings(desktop, Section::Clock, status),
    }
}

/// Открыть «Параметры» на заданном разделе.
fn open_settings(desktop: &mut Compositor, section: Section, status: &Status) {
    launch(desktop, App::Settings);
    if let Some(window) = desktop.find(App::Settings) {
        window.show_settings(section);
    }
    kprintln!("  desktop     : settings opened on {section:?}");
    desktop.refresh_panel(status);
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
    /// Окно с этим номером у задачи уже есть — см. [`App::Program`].
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
    if !matches!(app, App::Program(..)) {
        return false;
    }
    let Some(window) = desktop.find(app) else {
        return false;
    };
    window.push_event(WinEvent { kind: WIN_CLOSE, code: 0, x: 0, y: 0 });
    true
}

/// Завести задаче `task` окно номер `slot` поверх её поверхности.
///
/// Пиксели принадлежат не окну: [`Surface`] здесь заимствованная, построенная
/// поверх кадров, которые выдал и которыми владеет [`crate::user`]. Окно живёт
/// ровно столько же, сколько они, и снимает их тот же путь, что снимает окно.
pub fn open_window(task: u32, slot: u32, title: &str, pixels: Surface) -> Result<(), WindowError> {
    let app = App::Program(task, slot);
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
pub fn commit_window(task: u32, slot: u32, area: Option<Rect>) -> Result<(), WindowError> {
    let app = App::Program(task, slot);
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

/// Сменить окну программы размер (фаза N7d): `pixels` — новая поверхность той же
/// программы. Прежнюю [`crate::user`] снимает уже после этого вызова: до его
/// конца композитор вправе читать старые пиксели.
pub fn resize_window(task: u32, slot: u32, pixels: Surface) -> Result<(), WindowError> {
    let app = App::Program(task, slot);
    with_desktop(|desktop| {
        let focused = desktop.focused_app() == Some(app);
        let Some(window) = desktop.find(app) else {
            return Err(WindowError::NoWindow);
        };
        if !window.replace_pixels(pixels, focused) {
            return Err(WindowError::NoMemory);
        }
        log_window(desktop, app, focused);
        // Окно могло сжаться: открывшееся под ним место рисуется заново.
        desktop.repaint_all();
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
pub fn next_window_event(task: u32, slot: u32) -> Option<WinEvent> {
    with_desktop(|desktop| desktop.find(App::Program(task, slot))?.pop_event()).flatten()
}

/// Убрать окно задачи со стола.
///
/// Пиксели после этого никто не читает — и только поэтому кадры под ними можно
/// возвращать в пул. Порядок обязателен: сначала окно уходит со стола, потом
/// освобождается память, а не наоборот.
pub fn close_window(task: u32, slot: u32) -> Result<(), WindowError> {
    let app = App::Program(task, slot);
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
        App::Program(..) => desktop.caption_of(app).unwrap_or_else(|| String::from(app.title())),
        _ => String::from(app.title()),
    }
}

/// Записать в журнал, где стоит окно.
///
/// Координаты нужны не человеку: по ним автоматический прогон наводит мышь.
/// Без них сценарий с мышью пришлось бы писать в числах, подобранных под один
/// размер экрана, — то есть отдельно под каждую архитектуру, потому что режимы
/// у них разные (на 14.09.2026 загрузчик ставит 1280×720 на OVMF и 1024×768 на
/// ramfb), а с фазы С6a режим ещё и меняется на ходу.
fn log_window(desktop: &Compositor, app: App, focused: bool) {
    let Some(rect) = desktop.rect_of(app) else {
        return;
    };
    // У окна программы имя своё, и в журнал едет именно оно: [`App::title`]
    // знает только родовое слово «Program», а стенд наводит мышь по имени.
    let own = match app {
        App::Program(..) => desktop.caption_of(app),
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
