//! Композитор: слои рабочего стола поверх фреймбуфера.
//!
//! Слоёв четыре, и порядок их вывода — это и есть весь рабочий стол:
//!
//! ```text
//!   меню запуска      (если открыто)
//!   панель задач      (всегда сверху окон)
//!   окна              снизу вверх, последнее — активное
//!   фон               градиент с разметкой, рисуется без единого байта памяти
//! ```
//!
//! # Учёт изменённого никуда не делся
//!
//! На экран выводится не всё, а прямоугольники, которые действительно
//! изменились. Это не оптимизация ради оптимизации: фреймбуфер — память
//! устройства, запись в него на порядок дороже записи в обычную память, а набор
//! одной строки в терминале меняет одну строку ячеек, а не полтора миллиона
//! пикселей. Перетаскивание окна порождает два прямоугольника (откуда ушло и
//! куда пришло), и именно поэтому их список длиннее одного.
//!
//! # Почему фон не хранится поверхностью
//!
//! Поверхность размером с экран — это четыре мегабайта при куче в шестнадцать.
//! Градиент считается по номеру строки, а разметка — по остатку от деления
//! координаты, поэтому фон рисуется из ничего, и его хватает на любой экран,
//! который отдаст прошивка.
//!
//! # Кадр собирается в памяти и выводится целиком
//!
//! Слои складываются не на экран, а в буфер, и на экран уходит уже готовая
//! картинка — одним копированием строк. Это даёт три разные вещи, и каждая
//! стоила бы отдельной работы:
//!
//! 1. **Ничего не мелькает.** Раньше прямоугольник сначала заливался фоном, а
//!    окно ложилось поверх — и между этими двумя действиями фон было видно.
//! 2. **В экран пишут один раз.** Фреймбуфер — память устройства; фон, значок и
//!    окно, легшие на одно место, стоили трёх записей в неё, а теперь одной.
//! 3. **Буфер можно читать.** На этом стоит сглаживание шрифта: полутон на
//!    ступеньке буквы смешивается с тем, что под ней, а прочитать «что под
//!    ней» на экране невозможно — чтение write-combining памяти сбрасывает
//!    буфер записи.
//!
//! Буфер — **полоса**, а не целый экран: экран 1920×1080 в четырёх байтах на
//! точку — это восемь мегабайт из шестнадцати, то есть половина кучи под одну
//! картинку. Полоса в полмегабайта даёт то же самое, а прямоугольник выше её
//! просто выводится в несколько заходов.

use alloc::vec::Vec;

use mini_ui::{Color, Rect, Screen, Surface, draw};

use super::context::{Action, ContextMenu, Reply};
use super::icons::{Icons, Kind};
use super::panel::{Menu, Panel, PanelHit, Status};
use super::pointer::Pointer;
use super::flight::{Flight, Motion};
use super::start::{self, Start};
use super::tray::{self, Sheet};
use super::keyboard::{self, Keyboard};
use super::shade::Shade;
use mini_ui::theme;
use super::window::{App, Hit, Window};
use crate::input::KeyCode;
use user_abi::WinEvent;

/// Сколько прямоугольников изменений композитор согласен помнить.
///
/// Двенадцать, а не восемь, как было до появления стола: перетаскивание окна
/// даёт сразу два прямоугольника, панель — третий, и запаса на обычную работу
/// (строка терминала, окно состояния) должно оставаться. Переполнение не
/// ошибка — оно стоит одной перерисовки экрана.
const MAX_DAMAGE: usize = 12;

/// Шаг разметки на фоне рабочего стола.
const DOT_STEP: u32 = 24;

/// Размер точки разметки.
const DOT_SIZE: u32 = 1;

/// На сколько тень плавающего слоя выходит за его края.
///
/// Тень — единственное, что отделяет панель задач от обоев там, где обои
/// светлые: однопиксельной обводки на светлой теме не видно, а без отделения
/// панель читается как полоса, нарисованная прямо на фоне.
const SHADOW_SPREAD: u32 = 18;

/// На сколько тень смещена вниз относительно самого слоя.
const SHADOW_DROP: u32 = 10;

/// Сколько мониторов стол ведёт одновременно — столько мест под их
/// прямоугольники лежит в самом столе (см. [`Compositor::out_rects`]).
pub const MAX_OUTPUTS: usize = 8;

/// G3 (21.11): fullscreen fast path on/off (`ui direct on|off`, for A/B).
pub static DIRECT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Куда прилипает окно, брошенное у края экрана (20.11, как Snap в Windows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Snap {
    /// Левая половина рабочей области монитора.
    Left,
    /// Правая половина.
    Right,
    /// Верхний край — развернуть.
    Top,
    /// Зона макета прилипания (20.11, как Snap Layouts).
    Layout,
}

impl Snap {
    /// Для журнала.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Snap::Left => "to the left half",
            Snap::Right => "to the right half",
            Snap::Top => "maximized",
            Snap::Layout => "to a layout zone",
        }
    }
}

/// «Определить» (24.11): number plates on the monitors and when they go.
struct Identify {
    until_ms: u64,
    /// Place on the desktop and the plate.
    plates: Vec<(Rect, Surface)>,
}

/// Монитор стола: экран, в который выводятся собранные полосы, и его место
/// на общем столе.
///
/// Координаты стола начинаются в левом верхнем углу прямоугольника, который
/// охватывает все включённые мониторы (а не на основном, как в реестре
/// [`crate::display::outputs`]): так полоса кадра и все прямоугольники
/// изменений неотрицательны, и буфер полосы просто шириной во весь стол.
pub struct OutputSlot {
    /// Экран монитора, в своих координатах (0..w, 0..h).
    pub screen: Screen,
    /// Место на столе.
    pub rect: Rect,
    pub backend: crate::display::outputs::Backend,
    /// Основной: на нём панель, меню, значки, уведомления и экран блокировки.
    pub primary: bool,
}

pub struct Compositor {
    /// Мониторы стола; на машине с одним экраном — один, в (0, 0).
    outputs: Vec<OutputSlot>,
    /// Прямоугольник, охватывающий все мониторы, — весь стол, (0, 0, w, h).
    union: Rect,
    /// Место основного монитора на столе.
    primary: Rect,
    /// Места мониторов подряд — копия из [`Self::outputs`] для указателя:
    /// он двигается на каждый отчёт мыши, и собирать список в куче на каждое
    /// движение незачем.
    out_rects: [Rect; MAX_OUTPUTS],
    /// Экран погашен по бездействию (см. [`Compositor::blank`]).
    blanked: bool,
    /// Панели остальных мониторов (20.11, «Панель задач на всех
    /// мониторах»): номер монитора и его панель. Пусто — панель только на
    /// основном.
    side_panels: Vec<(usize, Panel)>,
    /// Монитор, на кнопке «Пуск» которого «Пуск» открывается; `None` —
    /// основной.
    menu_output: Option<usize>,
    /// Макеты прилипания (20.11, как Snap Layouts в Windows 11).
    snap_pop: Option<super::snaplay::Popup>,
    /// С какого мига (мс) тащимое окно держат у верхнего края; 0 — не держат.
    snap_top_since: u64,
    /// Плавное появление и уход подсмотра (20.11): когда начато (мс) и
    /// появляется ли он (`true`) или уходит.
    peek_fade: Option<(u64, bool)>,
    /// Указатель стоит на «Развернуть»: окно, кнопка и с какого мига (мс) —
    /// через [`SNAP_HOVER_MS`] открываются макеты прилипания.
    snap_hover: Option<(App, Rect, u64)>,
    /// Снизу вверх: последнее окно поверх остальных.
    windows: Vec<Window>,
    /// Индекс активного окна.
    focus: usize,
    panel: Option<Panel>,
    menu: Option<Menu>,
    /// Указатель мыши — верхний слой кадра.
    pointer: Pointer,
    /// Стрелку показывает адаптер (фаза 67b): стол её не рисует, а движение
    /// мыши не пересобирает кадр.
    hw_cursor: bool,
    /// Где курсор адаптера стоит по последней команде.
    cursor_sent: (i32, i32),
    /// Какая картинка у курсора адаптера (форма или кадр круга, 72c).
    cursor_key: usize,
    /// Значки на самом столе — слой между фоном и окнами.
    icons: Icons,
    /// Меню по правому щелчку. Верхний слой, как и меню запуска.
    context: Option<ContextMenu>,
    /// Шторка телефона — слой поверх окон (см. [`super::shade`]).
    ///
    /// `None` на настольной машине: там её нет вовсе, а не «есть и закрыта».
    shade: Option<Shade>,
    /// Экранная клавиатура телефона — слой над окнами, под шторкой (см.
    /// [`super::keyboard`]).
    keyboard: Option<Keyboard>,
    /// Окно, летящее в стопку или из неё (см. [`super::flight`]).
    flight: Option<Flight>,
    /// Закрытое окно, которое ещё уходит в свою плитку: из списка окон оно уже
    /// вынуто, а поверхность его нужна полёту до последнего кадра.
    closing: Option<Window>,
    /// Лист «Свёрнутые программы» — открыт, пока `Some` (см. [`super::tray`]).
    tray: Option<Sheet>,
    /// Лист «Выключить телефон?» — открыт, пока `Some` (см. [`super::powersheet`]).
    power: Option<super::powersheet::Sheet>,
    /// Лист «Пуск» телефона — открыт, пока `Some` (см. [`super::start`]).
    start: Option<Start>,
    /// Что показано в карточке «Система» — по нему видно, пора ли её
    /// перерисовать (см. [`Compositor::refresh_panel`]).
    home: super::home::Facts,
    /// Готовая картинка всего, что лежит под окнами телефона: обои, строка
    /// состояния, карточки домашнего экрана, значки.
    ///
    /// # Зачем
    ///
    /// Измерено на телефоне (`oem ui`): пока окно летело в стопку, слой под
    /// окнами стоил 69 мс на кадр, и полёт уложился в два кадра. Каждая полоса
    /// кадра рисовала карточки заново целиком — скругления, стекло, текст, — а
    /// меняется эта картинка раз в минуту. Теперь она собирается один раз и
    /// копируется в полосы, как окно.
    under: Option<Surface>,
    /// Сцена под летящим окном на настольном столе (фаза 64): обои, значки и
    /// остальные окна с тенями, собранные один раз на первом кадре полёта.
    ///
    /// Без неё каждый кадр сворачивания и разворота рисовал всё это заново —
    /// для разворота почти весь экран: обои со свечением, тени окон по каждой
    /// полосе. Замер `lag` после фазы 63b: средний кадр 10–14 мс, худший
    /// 88–130 мс. Док, летящее окно, листы и указатель рисуются поверх сцены на
    /// каждом кадре, как раньше. Нет памяти под картинку (3,7 МБ на 1280×720) —
    /// кадры собираются по-старому.
    scene: Option<Surface>,
    /// Готовые обои настольного стола и то, для чего они нарисованы: ширина,
    /// высота, тёмная ли тема, два цвета выбранных обоев (фаза 64).
    ///
    /// Обои — функция только размера экрана, темы и выбранных обоев, а
    /// считались они на каждом
    /// кадре заново: три световых пятна на точку. Замер `lag` после фазы 63b —
    /// в среднем кадре анимации это самый дорогой слой (6–10 мс), а в сборке
    /// сцены под полётом — большая часть её 50–60 мс. Теперь — копирование.
    /// Ячейка, а не поле: рисуют обои из сборки полосы, где стол только
    /// читается.
    ///
    /// Мониторов несколько — картинок по одной на каждый **размер** монитора:
    /// два одинаковых монитора делят одну.
    wall: core::cell::RefCell<Vec<(Surface, WallKey)>>,
    /// Годна ли [`Self::under`]. Сбрасывается там, где под окнами что-то
    /// меняется (см. [`Self::invalidate_under`]).
    under_valid: bool,
    /// Полоса годной [`Self::under`], которая устарела: часы, карточка,
    /// выделенный значок. Перерисовывается только она.
    ///
    /// # Зачем
    ///
    /// Измерено на телефоне 17.09 (`oem log`, десять циклов свернуть —
    /// развернуть): полная пересборка стоила 30–130 мс и шла первым кадром
    /// после разворачивания, ровно когда человек начинает печатать, — а
    /// менялась при этом одна карточка «Последнее». Отсюда и «лаг, который
    /// то есть, то нет»: утечки не было, куча стояла на месте все десять раз.
    under_dirty: Rect,
    /// Текст часов, который нарисован в строке состояния.
    status_text: alloc::string::String,
    /// Что внизу экрана телефона нарисовано последним: `true` — полоска
    /// жеста (на экране программа), `false` — док (см. [`Self::app_in_front`]).
    front_shown: bool,
    /// Панель спрятана под окном во весь экран (см.
    /// [`Self::fullscreen_front`]) — так было в прошлом кадре.
    full_front_shown: bool,
    /// Где был указатель, когда окно взяли за заголовок: развёрнутое окно
    /// уменьшается, только когда указатель отошёл от этой точки (20.11).
    drag_press: (i32, i32),
    /// Тащимое окно было развёрнутым и уменьшилось под указателем.
    drag_unmaximized: bool,
    /// Окно, которое сейчас тащат за заголовок.
    ///
    /// Программа, а не индекс: порядок окон меняется при поднятии, и индекс,
    /// запомненный до щелчка, после него указывал бы на соседнее окно.
    drag: Option<App>,
    /// Где стояло окно в тот миг, когда его взяли.
    ///
    /// Нужно одному: отличить перетаскивание от щелчка по заголовку. Щелчок
    /// тоже проходит через захват и отпускание, и без этой точки в журнале
    /// появляется строка «окно переехало туда, где оно и было». Она не просто
    /// лишняя: стенд ждёт переезда по этой строке и принимает за него щелчок,
    /// после чего целится в окно по старому месту.
    drag_from: (i32, i32),
    /// Захват меняет размер окна, а не его место.
    drag_resizes: bool,
    /// Окно тащат к краю экрана (20.11, как Snap в Windows): куда оно
    /// прилипнет, если его отпустить, и где это место — его показывает
    /// полупрозрачная рамка под окном.
    snap: Option<(Snap, Rect)>,
    /// Окно, переехавшее с прошлого кадра, и где оно стояло **до первого**
    /// сдвига в этом кадре. См. [`Compositor::note_moved`].
    moved: Option<(App, Rect)>,
    /// Размер, который просит тянущий уголок, — применяется раз за кадр (см.
    /// [`Compositor::apply_pending_size`]).
    ///
    /// Числа со знаком и без ограничений: это «где сейчас палец», а пределы
    /// ставятся при применении. Так окно, упёршееся в наименьший размер, на
    /// обратном ходу снова следует за пальцем, а не отстаёт на накопленное.
    ///
    /// Прямоугольник целиком (`x, y, w, h`): тянуть можно за любой край, и у
    /// левого и верхнего вместе с размером едет угол окна (01.11).
    pending_size: Option<(App, i32, i32, i32, i32)>,
    /// Контур перетаскивания (20.11, «Оформление»): где окажется окно, если
    /// его отпустить. Само окно стоит на месте до отпускания.
    outline: Option<Rect>,
    /// Отпускание: отложенный размер применяется и в режиме контура.
    settling: bool,
    /// Какие края тянут ([`super::window::EDGE_LEFT`] и прочие).
    resize_edges: u8,
    /// Окно программы, над содержимым которого указатель был в прошлый раз
    /// (фаза N7h).
    ///
    /// Нужно одному: сказать окну, что указатель ушёл. Узнать это из текущего
    /// движения нельзя — оно достаётся уже другому окну или столу.
    hovered: Option<App>,
    /// Окно, показанное поверх всех, пока указатель стоит на его строке в
    /// списке окон панели задач (как «подсмотр» в Windows): порядок окон при
    /// этом не меняется. Свёрнутое тоже показывается — иначе не узнать,
    /// какое из одинаково названных окон какое.
    peek: Option<App>,
    /// Подложка подсмотра (20.11, как Aero Peek): весь стол с окнами,
    /// уменьшенный в [`PEEK_STEP`] раз, размытый и приглушённый. Собирается
    /// один раз, когда подсмотр начался; пока он идёт, всё под подсмотренным
    /// окном рисуется из неё растяжением.
    peek_back: Option<Surface>,
    /// Миниатюры окон над кнопкой панели (72e).
    thumbs: Option<super::thumbs::Thumbs>,
    /// Окно трея (72h-1).
    tray_pop: Option<super::traypop::TrayPop>,
    /// «Завершение работы» screen (24.11): over everything on every monitor.
    ending: Option<super::ending::Screen>,
    /// Экран блокировки (72h-4) и его готовая картинка во весь экран.
    lock: Option<(super::lock::Lock, Surface)>,
    /// Screensaver (22.11, `saver`): over everything, the lock screen too.
    saver: Option<super::saver::Shown>,
    /// Уведомление (72h-2) и пакеты, о которых стол уже знает.
    toast: Option<super::toast::Toast>,
    /// «Определить» (24.11, like Windows): number plates on the monitors
    /// until a moment.
    identify: Option<Identify>,
    /// Заставка запуска программы Windows (21.11, `ui::launch`).
    splash: super::launch::Splash,
    known_packages: Option<Vec<alloc::string::String>>,
    /// Активное окно, о котором программам уже сказано (фаза 62c, `WIN_FOCUS`).
    ///
    /// Сверяется с [`Self::focused_app`] после каждого действия со столом, а не
    /// в каждом месте, где фокус меняется: таких мест пять (поднятие, новое
    /// окно, сворачивание, закрытие, обход по кругу), и забытое шестое молча
    /// оставило бы меню WinForms висеть.
    reported_focus: Option<App>,
    /// Клавиши, нажатие которых ушло окну программы, — кому и каким событием
    /// (фаза 62c, `WIN_KEYUP`).
    ///
    /// Отпускание идёт туда же, куда нажатие, а не в нынешний фокус: после
    /// `Alt+Tab` нажатая буква отпускается уже над другим окном, и без таблицы
    /// одно окно осталось бы с «зажатой» клавишей, а другое получило бы
    /// отпускание того, чего не нажимали. Восьми мест хватает рукам; девятая
    /// одновременно зажатая клавиша своего отпускания не получит.
    held_keys: [Option<(KeyCode, App, WinEvent)>; 8],
    /// Окно программы, над содержимым которого нажали кнопку мыши, и какие
    /// кнопки ещё держат (1 левая, 2 правая) — захват указателя (фаза 62b).
    ///
    /// Пока кнопка держится, движения и отпускание достаются этому окну, где бы
    /// ни был указатель: иначе ползунок, который утащили за край окна, так и
    /// остался бы нажатым, а выделение текста не узнало бы, где кончилось.
    captured: Option<(App, u32)>,
    /// Масштаб глифа для новых окон.
    scale: mini_ui::Scale,
    /// Буфер, в котором собирается кадр, — полоса во всю ширину экрана.
    back: Surface,
    damage: [Rect; MAX_DAMAGE],
    damage_count: usize,
    /// Изменений накопилось больше, чем помещается: проще перерисовать всё.
    damage_overflow: bool,
    frames: u64,
    rects: u64,
    /// Кадр не собирать: за этим событием в очереди ввода уже лежит следующее.
    ///
    /// Изменения при этом не теряются — они копятся в прямоугольниках окон и в
    /// [`Self::damage`] и уходят на экран ближайшим несложенным кадром.
    deferred: bool,
    /// G3 (21.11): per output, the fullscreen program window shown straight
    /// from its pixels in the last frame (no compose, no back buffer).
    direct: [Option<App>; MAX_OUTPUTS],
    /// G3: per output, the client buffer (physical address) the display
    /// scans out instead of the output's frame buffer (NVIDIA page flip).
    flipped: [Option<u64>; MAX_OUTPUTS],
    /// G3: per direct output, the last logged flip state: `Some("")` - it
    /// flips, `Some(why)` - why it does not; `None` - not direct.
    flip_note: [Option<alloc::string::String>; MAX_OUTPUTS],
    /// G3: per direct output, a refusal of the display for the current
    /// window (copy until the window leaves the direct mode).
    flip_block: [Option<alloc::string::String>; MAX_OUTPUTS],
    /// G3: per direct output, the display had no cursor for it - the pointer
    /// over it blocks the flip (it is drawn into the frame buffer).
    flip_no_cursor: [bool; MAX_OUTPUTS],
    /// G3: per output, the display's cursor over the flipped output:
    /// `(image key, hot spot, top-left)` sent last; `None` - off.
    flip_cursor: [Option<(usize, (u32, u32), (i32, i32))>; MAX_OUTPUTS],
    /// G3: per output, the changed part of a flipped client buffer this
    /// frame (virtio-gpu transfers it).
    flip_touched: [Option<Rect>; MAX_OUTPUTS],
    /// G3: a flip the display could not take yet - next frame tries again.
    flip_retry: bool,
    /// G3: the program window that hid the pointer over itself
    /// (`wl_pointer.set_cursor` with a null surface).
    cursor_hidden_by: Option<App>,
    /// G3: pointer lock/confinement asked by a program window
    /// (`zwp_pointer_constraints_v1`).
    constraint: Option<Constraint>,
    /// G3: last tablet position (absolute pointing devices: QEMU, VirtualBox)
    /// - relative motion while the pointer is locked is counted from it.
    tablet_last: Option<(i32, i32)>,
}

/// G3: a pointer constraint of a program window.
#[derive(Clone, Copy)]
struct Constraint {
    app: App,
    /// Lock (the pointer stays and hides) or confine (stays inside).
    lock: bool,
    /// Confinement region in program-local coordinates; `None` - the window.
    region: Option<Rect>,
    /// Where to put the pointer when the lock ends (program-local).
    hint: Option<(i32, i32)>,
    active: bool,
}

/// Сколько раз картинка под окнами сбрасывалась — для разбора по журналу.
static UNDER_DROPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Сколько держать тащимое окно у верхнего края до макетов прилипания и
/// указатель на «Развернуть» (20.11, как в Windows 11).
const SNAP_HOLD_MS: u64 = 450;
const SNAP_HOVER_MS: u64 = 500;

/// Сколько длится появление и уход размытого подсмотра (20.11).
const PEEK_FADE_MS: u64 = 150;

/// Сколько памяти отдаётся под полосу, в которой собирается кадр.
///
/// Полмегабайта — это сотня строк на экране 1280 точек и семь десятков на 1920.
/// Больше не нужно: полоса влияет только на число заходов, а не на то, что
/// получится; меньше — и на каждый прямоугольник изменений приходилось бы по
/// десятку выводов на экран, каждый со своим обходом всех слоёв.
const BAND_BYTES: u32 = 512 * 1024;

/// Наименьшая и наибольшая высота полосы в строках.
const BAND_MIN: u32 = 16;
const BAND_MAX: u32 = 256;

/// Охватывающий прямоугольник, место основного монитора и места всех
/// мониторов подряд. `None` — мониторов нет или их больше [`MAX_OUTPUTS`].
fn layout_of(outputs: &[OutputSlot]) -> Option<(Rect, Rect, [Rect; MAX_OUTPUTS])> {
    let rects: Vec<Rect> = outputs.iter().map(|o| o.rect).collect();
    let primary = outputs.iter().position(|o| o.primary).unwrap_or(0);
    layout_of_rects(&rects, primary)
}

/// То же по одним прямоугольникам (`primary` — номер основного).
fn layout_of_rects(rects: &[Rect], primary: usize) -> Option<(Rect, Rect, [Rect; MAX_OUTPUTS])> {
    if rects.is_empty() || rects.len() > MAX_OUTPUTS {
        return None;
    }
    let mut union = rects[0];
    let mut out = [Rect::EMPTY; MAX_OUTPUTS];
    for (i, r) in rects.iter().enumerate() {
        union = union.union(r);
        out[i] = *r;
    }
    // Стол начинается в (0, 0): координаты мониторов приходят уже в нём.
    let union = Rect::new(0, 0, union.right().max(0) as u32, union.bottom().max(0) as u32);
    Some((union, rects.get(primary).copied().unwrap_or(rects[0]), out))
}

/// Монитор `p` без того, что панель отнимает у своей стороны (20.11).
fn cut_by_panel(p: Rect, panel: &Panel) -> Rect {
    let (edge, t) = panel.reserve();
    let t = t.min(p.w.min(p.h) / 2);
    match edge {
        super::deskcfg::Edge::Bottom => Rect::new(p.x, p.y, p.w, p.h - t),
        super::deskcfg::Edge::Top => Rect::new(p.x, p.y + t as i32, p.w, p.h - t),
        super::deskcfg::Edge::Left => Rect::new(p.x + t as i32, p.y, p.w - t, p.h),
        super::deskcfg::Edge::Right => Rect::new(p.x, p.y, p.w - t, p.h),
    }
}

/// Буфер полосы во всю ширину стола.
fn band_buffer(union: Rect) -> Option<Surface> {
    let rows = (BAND_BYTES / (union.w.max(1) * 4)).clamp(BAND_MIN, BAND_MAX);
    Surface::new(union.w, rows.min(union.h.max(1)), theme::palette().wall_top)
}

/// Слои стола, посчитанные от размера экрана, — собранные заранее под новый
/// режим (фаза С6a). См. [`Compositor::prepare_layout`].
pub struct Layers {
    back: Surface,
    panel: Option<Panel>,
    menu: Option<Menu>,
    context: Option<ContextMenu>,
}

impl Compositor {
    /// Поднять композитор на этом экране.
    ///
    /// `None`, если не хватило памяти под буфер кадра. Это не отказ системы:
    /// ядро в таком случае работает с оболочкой в серийной линии — ровно как
    /// тогда, когда прошивка не дала фреймбуфера вовсе. Отдельного пути «рисуем
    /// прямо на экран, без буфера» здесь намеренно нет: он означал бы две
    /// сборки кадра, из которых вторая проверялась бы только на машине, где не
    /// хватило памяти, — то есть никогда.
    pub fn new(outputs: Vec<OutputSlot>, scale: mini_ui::Scale) -> Option<Self> {
        let (union, primary, out_rects) = layout_of(&outputs)?;
        let mut pointer = Pointer::new(primary);
        pointer.set_scale(scale);
        let back = band_buffer(union)?;
        let mut compositor = Self {
            back,
            outputs,
            union,
            primary,
            out_rects,
            windows: Vec::new(),
            focus: 0,
            panel: None,
            menu: None,
            pointer,
            icons: Icons::new(scale),
            context: ContextMenu::new(scale),
            shade: None,
            keyboard: None,
            flight: None,
            closing: None,
            tray: None,
            power: None,
            start: None,
            home: super::home::Facts::default(),
            under: None,
            scene: None,
            wall: core::cell::RefCell::new(Vec::new()),
            under_valid: false,
            under_dirty: Rect::EMPTY,
            status_text: alloc::string::String::new(),
            front_shown: false,
            full_front_shown: false,
            drag_press: (0, 0),
            drag_unmaximized: false,
            drag: None,
            drag_from: (0, 0),
            drag_resizes: false,
            snap: None,
            moved: None,
            pending_size: None,
            outline: None,
            settling: false,
            resize_edges: 0,
            hovered: None,
            peek: None,
            peek_back: None,
            thumbs: None,
            tray_pop: None,
            ending: None,
            toast: None,
            identify: None,
            splash: super::launch::Splash::default(),
            known_packages: None,
            lock: None,
            saver: None,
            reported_focus: None,
            held_keys: [None; 8],
            captured: None,
            scale,
            damage: [Rect::EMPTY; MAX_DAMAGE],
            damage_count: 0,
            damage_overflow: true,
            frames: 0,
            rects: 0,
            deferred: false,
            blanked: false,
            side_panels: Vec::new(),
            menu_output: None,
            snap_pop: None,
            snap_top_since: 0,
            peek_fade: None,
            snap_hover: None,
            hw_cursor: false,
            cursor_sent: (0, 0),
            cursor_key: usize::MAX,
            direct: [None; MAX_OUTPUTS],
            flipped: [None; MAX_OUTPUTS],
            flip_note: [const { None }; MAX_OUTPUTS],
            flip_block: [const { None }; MAX_OUTPUTS],
            flip_no_cursor: [false; MAX_OUTPUTS],
            flip_cursor: [None; MAX_OUTPUTS],
            flip_touched: [None; MAX_OUTPUTS],
            flip_retry: false,
            cursor_hidden_by: None,
            constraint: None,
            tablet_last: None,
        };
        compositor.panel = Panel::new(compositor.primary, scale);
        compositor.build_side_panels();
        let panel_top = compositor.work_bottom();
        compositor.menu = compositor.new_menu();
        // Шторка заводится только на телефоне — [`Shade::new`] сам вернёт
        // `None` на настольной машине.
        compositor.shade = Shade::new(compositor.primary.w, panel_top, scale);
        compositor.keyboard = Keyboard::new(compositor.primary.w, compositor.primary.h, scale);
        // Длина столбца значков считается от рабочей области, а не от экрана:
        // ячейка, заехавшая под панель задач, щёлкается панелью, а не значком.
        // Сетка — на основном мониторе.
        // С 20.11 панель стоит у любой стороны: сетка — от угла рабочей
        // области, а не экрана.
        compositor.place_icon_grid();
        // Ширина экрана и верх сетки — телефону (см. [`Icons::set_span`]). На
        // столе вызов безвреден: мобильная раскладка спрашивает эти числа, а
        // настольная о них не знает.
        // На телефоне сетка стоит под карточкой «Система» домашнего экрана.
        compositor.icons.set_span(compositor.primary.w, super::home::grid_top(scale));
        compositor.icons.reload();
        Some(compositor)
    }

    /// Нижняя граница области, в которой живут окна: верх нижней панели.
    pub fn work_bottom(&self) -> i32 {
        self.primary_work().bottom()
    }

    /// Рабочая область основного монитора: экран без того, что панель
    /// отнимает у своей стороны (20.11: панель и Dock стоят у любой стороны,
    /// со скрытием — не отнимают ничего).
    #[must_use]
    pub fn primary_work(&self) -> Rect {
        let p = self.primary;
        let Some(panel) = self.panel.as_ref() else {
            return p;
        };
        if theme::is_mobile() {
            return Rect::new(p.x, p.y, p.w, (panel.rect.y - p.y).max(0) as u32);
        }
        cut_by_panel(p, panel)
    }

    /// Номер основного монитора в [`Self::outputs`].
    fn primary_index(&self) -> usize {
        self.outputs.iter().position(|o| o.primary).unwrap_or(0)
    }

    /// Пересобрать панели остальных мониторов (20.11, «на всех
    /// мониторах»); выключено — убрать их.
    fn build_side_panels(&mut self) {
        let old: Vec<Rect> = self.side_panels.drain(..).map(|(_, panel)| panel.rect).collect();
        for rect in old {
            self.mark_layer(rect);
        }
        let primary = self.primary_index();
        if let Some(panel) = self.panel.as_mut() {
            panel.output = primary;
        }
        if theme::is_mobile() || !super::deskcfg::taskbar().all {
            return;
        }
        let rects: Vec<Rect> = self.output_rects().to_vec();
        for (i, rect) in rects.into_iter().enumerate() {
            if i == primary {
                continue;
            }
            if let Some(mut panel) = Panel::new_side(rect, self.scale) {
                panel.output = i;
                self.mark_layer(panel.rect);
                self.side_panels.push((i, panel));
            }
        }
    }

    /// Панель монитора `output`: основная или своя у второго монитора.
    fn panel_on(&self, output: usize) -> Option<&Panel> {
        if output == self.primary_index() {
            return self.panel.as_ref();
        }
        self.side_panels.iter().find(|(o, _)| *o == output).map(|(_, panel)| panel)
    }

    /// Все панели стола: основная первой.
    fn all_panels(&self) -> impl Iterator<Item = &Panel> {
        self.panel.iter().chain(self.side_panels.iter().map(|(_, panel)| panel))
    }

    /// На кнопке какого монитора стоит окно в `rect`: монитор под его
    /// серединой, если на нём своя панель, иначе основной (20.11, как
    /// «Показывать кнопки на панели, где открыто окно» в Windows).
    fn panel_output_of(&self, rect: Rect) -> usize {
        let o = self.output_near(rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2);
        if self.panel_on(o).is_some() { o } else { self.primary_index() }
    }

    /// Поставить сетку значков (20.11): от угла основного монитора, столбец
    /// той же длины при любой стороне панели — иначе панель, переехавшая
    /// влево, меняла число строк, и значки перескакивали в другие столбцы.
    /// Сдвиг — только на ту часть, которую панель слева или сверху
    /// действительно закрывает.
    fn place_icon_grid(&mut self) {
        let work = self.primary_work();
        let p = self.primary;
        let Some(panel) = self.panel.as_ref().filter(|_| !theme::is_mobile()) else {
            self.icons.set_origin(work.x, work.y);
            self.icons.set_area(work.h as i32);
            return;
        };
        let cfg = panel.config();
        let ctx = mini_ui::paint::Ctx::scaled(self.scale);
        let margin = ctx.px(theme::ICON_MARGIN) as i32;
        let gap = ctx.px(6) as i32;
        let home = panel.home();
        let (mut ox, mut oy) = (p.x, p.y);
        if !cfg.autohide {
            match cfg.edge {
                super::deskcfg::Edge::Left => ox += (home.right() + gap - (p.x + margin)).max(0),
                super::deskcfg::Edge::Top => oy += (home.bottom() + gap - (p.y + margin)).max(0),
                _ => {}
            }
        }
        // Длина столбца — как у полосы снизу той же толщины.
        let bar = if cfg.autohide { 0 } else { Panel::thickness(super::deskcfg::Taskbar { edge: super::deskcfg::Edge::Bottom, ..cfg }, self.scale) };
        let height = (p.h.saturating_sub(bar) as i32 - (oy - p.y).saturating_sub(bar as i32).max(0)).max(0);
        self.icons.set_origin(ox, oy);
        self.icons.set_area(height);
    }

    /// Кнопка «Пуск» на экране (20.11: у неё открывается «Выполнить»);
    /// пусто — панели нет.
    #[must_use]
    pub fn start_button_rect(&self) -> Rect {
        self.panel.as_ref().map_or(Rect::EMPTY, |panel| panel.home())
    }

    /// «Пуск» под нынешнюю панель.
    fn new_menu(&self) -> Option<Menu> {
        // У «Пуска» монитора, на кнопке которого его открыли (20.11).
        let side = self.menu_output.and_then(|o| self.side_panels.iter().find(|(i, _)| *i == o)).map(|(_, panel)| panel);
        let (area, panel) = match side {
            Some(panel) => (panel.area(), Some(panel)),
            None => (self.primary, self.panel.as_ref()),
        };
        let (home, cfg) = panel.map_or((Rect::EMPTY, super::deskcfg::taskbar()), |panel| (panel.home(), panel.config()));
        Menu::new(area, home, cfg, self.scale)
    }

    /// «Пуск» откроется у панели под точкой `x, y` (20.11, панели на всех
    /// мониторах): закрытое меню пересобирается у её кнопки.
    pub fn aim_menu_at(&mut self, x: i32, y: i32) {
        let output = self.side_panels.iter().find(|(_, panel)| panel.rect.contains(x, y)).map(|(o, _)| *o);
        if output == self.menu_output || self.menu_open() {
            return;
        }
        self.menu_output = output;
        if let Some(menu) = self.new_menu() {
            self.menu = Some(menu);
        }
        crate::kprintln!("  desktop     : start menu on output {}", output.unwrap_or_else(|| self.primary_index()));
    }

    /// Пересобрать панель и «Пуск» под новые настройки (`deskcfg`, 20.11):
    /// сторона, вид, скрытие, размер значков. Окна вписываются в новую
    /// рабочую область, развёрнутые — занимают её целиком.
    pub fn rebuild_taskbar(&mut self) {
        if theme::is_mobile() {
            return;
        }
        let Some(panel) = Panel::new(self.primary, self.scale) else {
            return;
        };
        if let Some(old) = self.panel.as_ref() {
            let rect = old.rect;
            self.mark_layer(rect);
        }
        self.panel = Some(panel);
        self.build_side_panels();
        self.menu_output = None;
        if let Some(menu) = self.new_menu() {
            self.menu = Some(menu);
        }
        let work = self.primary_work();
        self.place_icon_grid();
        self.icons.reload();
        let works: Vec<Rect> = self.windows.iter().map(|window| self.work_rect_under(window.rect)).collect();
        for (window, work) in self.windows.iter_mut().zip(works) {
            if window.is_maximized() && !window.popup && window.resize(work.w, work.h) {
                window.rect.x = work.x;
                window.rect.y = work.y;
                continue;
            }
            let max_x = (work.right() - window.rect.w as i32).max(work.x);
            let max_y = (work.bottom() - window.rect.h as i32).max(work.y);
            window.rect.x = window.rect.x.clamp(work.x, max_x);
            window.rect.y = window.rect.y.clamp(work.y, max_y);
        }
        self.under_valid = false;
        self.repaint_all();
        crate::kprintln!("  desktop     : taskbar {} (work area {},{} {}x{})", super::deskcfg::describe(), work.x, work.y, work.w, work.h);
    }

    /// Сменить масштаб всего стола на ходу (20.11, как «Масштаб» в Windows):
    /// панель или Dock, «Пуск», значки, указатель, меню, рамки и содержимое
    /// окон ядра, шрифт терминала — всё от одного числа. Программам масштаб
    /// уходит через `SysInfo::flags` (его выставляет вызывающий). `false` —
    /// масштаб тот же, делать нечего.
    pub fn set_scale(&mut self, scale: mini_ui::Scale, status: &Status) -> bool {
        let scale = scale.min(mini_ui::Scale::TWO);
        if scale == self.scale || theme::is_mobile() {
            return false;
        }
        let old = self.scale;
        self.scale = scale;
        self.pointer.set_scale(scale);
        self.icons.set_scale(scale);
        self.context = ContextMenu::new(scale);
        // Всплывающее собрано в прежнем масштабе — закрыть.
        self.thumbs = None;
        self.tray_pop = None;
        self.peek = None;
        self.peek_back = None;
        self.toast = None;
        for window in &mut self.windows {
            window.set_scale(scale);
        }
        // Панель, «Пуск», сетка значков — заново; рабочая область — уже с
        // панелью нового размера (на 125 % она выше на 12 точек, и окно,
        // поджатое по прежней, уходило низом под неё).
        self.rebuild_taskbar();
        let works: Vec<Rect> = self.windows.iter().map(|window| self.work_rect_under(window.rect)).collect();
        for (window, work) in self.windows.iter_mut().zip(works) {
            if window.is_maximized() {
                continue;
            }
            // Выросшее окно не шире и не выше рабочей области своего монитора
            // и целиком в ней: правый и нижний край не за экраном.
            if window.rect.w > work.w || window.rect.h > work.h {
                let _ = window.resize(window.rect.w.min(work.w), window.rect.h.min(work.h));
            }
            let (w, h) = (window.rect.w as i32, window.rect.h as i32);
            window.rect.x = window.rect.x.min(work.right() - w).max(work.x);
            window.rect.y = window.rect.y.min(work.bottom() - h).max(work.y);
        }
        self.restyle(status);
        crate::kprintln!("  desktop     : ui scale {scale} (was {old}), {} window(s) rescaled", self.windows.len());
        true
    }

    /// Указатель сдвинулся: автоскрытие панели.
    pub fn taskbar_pointer(&mut self, x: i32, y: i32) {
        let busy = self.menu_open() || self.context_open() || self.thumbs_open() || self.tray_pop.is_some();
        let now = crate::time::uptime_ms();
        // Каждая панель — у края своего монитора (20.11).
        let panels = self.panel.iter_mut().chain(self.side_panels.iter_mut().map(|(_, panel)| panel));
        for panel in panels {
            panel.autohide_pointer(x, y, busy, now);
            if panel.autohide_pending() {
                super::set_animating(true);
            }
        }
    }

    /// Шаг автоскрытия перед кадром. `true` — ещё едет или ждёт срока.
    fn advance_taskbar(&mut self) -> bool {
        let busy = self.menu_open() || self.context_open() || self.thumbs_open() || self.tray_pop.is_some();
        let now = crate::time::uptime_ms();
        let mut pending = false;
        let mut moved = Vec::new();
        let panels = self.panel.iter_mut().chain(self.side_panels.iter_mut().map(|(_, panel)| panel));
        for panel in panels {
            if busy {
                panel.hold(now);
            }
            if let Some(rect) = panel.autohide_tick(now) {
                // Только в пределах своего монитора: уехавшая за край
                // плашка на соседнем не рисуется (20.11).
                moved.push(rect.intersect(&panel.area()));
            }
            pending |= panel.autohide_pending();
        }
        for rect in moved {
            self.mark_layer(rect);
        }
        pending
    }

    /// Закреплённая на панели программа под кнопкой `index`.
    #[must_use]
    pub fn panel_pin_choice(&self, index: usize) -> Option<super::panel::Choice> {
        self.panel.as_ref()?.pin_choice(index)
    }

    #[must_use]
    pub fn panel_pin_key(&self, index: usize) -> Option<alloc::string::String> {
        self.panel.as_ref()?.pin_key(index)
    }

    #[must_use]
    pub fn panel_pin_caption(&self, index: usize) -> alloc::string::String {
        self.panel.as_ref().map(|panel| alloc::string::String::from(panel.pin_caption(index))).unwrap_or_default()
    }

    /// Всплывающее у панели (трей, уведомление, миниатюры): поставленное
    /// «над нижней панелью» `rect` переносится к панели у другой стороны —
    /// под верхнюю, правее левой, левее правой; `anchor` — кнопка, к которой
    /// оно относится.
    #[must_use]
    pub fn beside_panel(&self, rect: Rect, anchor: Rect) -> Rect {
        // Панель монитора, на котором кнопка (20.11).
        let output = self.output_near(anchor.x + anchor.w as i32 / 2, anchor.y + anchor.h as i32 / 2);
        let Some(panel) = self.panel_on(output).or(self.panel.as_ref()).filter(|_| !theme::is_mobile()) else {
            return rect;
        };
        let home = panel.home();
        let gap = mini_ui::paint::Ctx::scaled(self.scale).px(12) as i32;
        let p = panel.area();
        let fit_y = |y: i32| y.clamp(p.y + gap, (p.bottom() - rect.h as i32 - gap).max(p.y + gap));
        let center_y = anchor.y + anchor.h as i32 / 2 - rect.h as i32 / 2;
        match panel.config().edge {
            super::deskcfg::Edge::Bottom => rect,
            super::deskcfg::Edge::Top => Rect::new(rect.x, home.bottom() + gap, rect.w, rect.h),
            super::deskcfg::Edge::Left => Rect::new(home.right() + gap, fit_y(center_y), rect.w, rect.h),
            super::deskcfg::Edge::Right => Rect::new(home.x - gap - rect.w as i32, fit_y(center_y), rect.w, rect.h),
        }
    }

    /// Нижняя граница окон: на телефоне — над полоской жеста (окно во весь
    /// экран, дока под ним нет, 04.11), на ПК — та же [`Self::work_bottom`].
    pub fn app_bottom(&self) -> i32 {
        if theme::is_mobile() && self.panel.is_some() {
            self.primary.bottom() - super::gesture::bar_height(self.scale) as i32
        } else {
            self.work_bottom()
        }
    }

    /// Куда окно разворачивается (05.11): на ПК — вся рабочая область, на
    /// телефоне — «на весь экран», от края до края между строкой состояния
    /// и полоской жеста.
    #[must_use]
    pub fn app_area(&self) -> Rect {
        let width = self.primary.w;
        if !theme::is_mobile() {
            let _ = width;
            return self.primary_work();
        }
        let top = mini_ui::paint::Ctx::scaled(self.scale).px(theme::M_STATUS_H) as i32;
        Rect::new(0, top, width, self.app_bottom().saturating_sub(top).max(1) as u32)
    }

    /// Окно телефона обычного размера: на поле [`theme::M_INSET`] от краёв,
    /// от строки состояния до полоски жеста (то, что [`Self::app_area`]
    /// разворачивает «на весь экран»).
    #[must_use]
    pub fn mobile_rect(&self) -> Rect {
        let ctx = mini_ui::paint::Ctx::scaled(self.scale);
        let top = ctx.px(theme::M_STATUS_H + theme::M_INSET) as i32;
        let inset = ctx.px(theme::M_INSET);
        let w = self.primary.w.saturating_sub(inset * 2).max(1);
        let bottom = self.app_bottom().max(1) as u32;
        let h = bottom.saturating_sub(top.max(0) as u32).max(1);
        Rect::new(inset as i32, top, w, h)
    }

    /// Программа на экране телефона: тогда внизу полоска жеста, а не док.
    ///
    /// Всплывающие окна не в счёт — они часть формы. Свёрнутое окно в полёте к
    /// стопке уже свёрнуто, и док появляется в начале полёта: стопке есть куда
    /// принять окно. Разворачиваемое — наоборот, в конце.
    #[must_use]
    pub fn app_in_front(&self) -> bool {
        theme::is_mobile() && self.windows.iter().any(|window| !window.minimized && !window.popup)
    }

    /// Окна на экране (не свёрнутые и не всплывающие), снизу вверх.
    #[must_use]
    pub fn front_apps(&self) -> Vec<App> {
        self.windows.iter().filter(|window| !window.minimized && !window.popup).map(|window| window.app).collect()
    }

    /// Свёрнутые окна, начиная со свёрнутого последним.
    #[must_use]
    pub fn recent_minimized(&self) -> Vec<App> {
        let mut minimized: Vec<&Window> =
            self.windows.iter().filter(|window| window.minimized && !window.popup).collect();
        minimized.sort_by(|a, b| b.minimized_at_ms.cmp(&a.minimized_at_ms));
        minimized.into_iter().map(|window| window.app).collect()
    }

    /// Сменить программу на экране без полёта: `from` уходит в стопку,
    /// `to` встаёт на его место (жест вбок по полоске, 04.11).
    ///
    /// Без полёта намеренно: полёт в стопку и обратно показал бы док между
    /// ними, а человек листает программы и дока не звал.
    pub fn switch_to(&mut self, from: App, to: App) -> bool {
        self.finish_flight();
        let now = crate::time::uptime_ms();
        if let Some(window) = self.find(from).filter(|window| !window.minimized) {
            window.minimized = true;
            window.minimized_at_ms = now;
        }
        let Some(index) = self.index_of(to) else {
            return false;
        };
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        window.minimized = false;
        window.damage = Rect::new(0, 0, window.rect.w, window.rect.h);
        self.raise(index);
        self.invalidate_recent();
        self.refresh_decorations();
        self.mark(self.union);
        true
    }

    /// Док или полоска жеста — по тому, есть ли на экране программа.
    /// Сменилось — пометить низ экрана.
    fn sync_front(&mut self) {
        // Окно во весь экран вышло вперёд или ушло назад — панель под ним
        // прячется или возвращается (20.11).
        let full = self.fullscreen_front();
        if full != self.full_front_shown {
            self.full_front_shown = full;
            let rects: Vec<Rect> = self.all_panels().map(|panel| panel.rect).collect();
            for rect in rects {
                self.mark_layer(rect);
            }
        }
        let front = self.app_in_front();
        if front == self.front_shown {
            return;
        }
        self.front_shown = front;
        let top = self.work_bottom().max(0);
        let p = self.primary;
        self.mark(Rect::new(p.x, top, p.w, (p.bottom() - top).max(0) as u32));
    }

    /// Ширина **основного** монитора: от неё — масштаб, форма машины, место
    /// новых окон. Весь стол — [`Self::desktop_size`].
    pub const fn screen_width(&self) -> u32 {
        self.primary.w
    }

    /// Высота основного монитора.
    pub const fn screen_height(&self) -> u32 {
        self.primary.h
    }

    /// Размер всего стола — прямоугольника, охватывающего все мониторы.
    pub const fn desktop_size(&self) -> (u32, u32) {
        (self.union.w, self.union.h)
    }

    /// Весь стол (0, 0, w, h).
    #[must_use]
    pub const fn union_rect(&self) -> Rect {
        self.union
    }

    /// Место основного монитора на столе.
    #[must_use]
    pub const fn primary_rect(&self) -> Rect {
        self.primary
    }

    /// Места мониторов на столе, по порядку.
    #[must_use]
    pub fn output_rects(&self) -> &[Rect] {
        &self.out_rects[..self.outputs.len().min(MAX_OUTPUTS)]
    }

    /// Монитор, на котором лежит точка.
    fn output_at(&self, x: i32, y: i32) -> Option<usize> {
        self.output_rects().iter().position(|r| r.contains(x, y))
    }

    /// Монитор под точкой, а если она в щели между ними — ближайший.
    pub fn output_near(&self, x: i32, y: i32) -> usize {
        self.output_at(x, y).unwrap_or_else(|| {
            let mut best = 0;
            let mut best_d = i64::MAX;
            for (i, r) in self.output_rects().iter().enumerate() {
                let dx = i64::from((r.x - x).max(0).max(x - (r.right() - 1)));
                let dy = i64::from((r.y - y).max(0).max(y - (r.bottom() - 1)));
                let d = dx * dx + dy * dy;
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            best
        })
    }

    /// Монитор `i` без того, что внизу отнимает панель (`bottom` — её верх
    /// или полоска жеста): у основного обрезается низ, у остальных — весь
    /// монитор.
    fn area_of(&self, i: usize, bottom: i32) -> Rect {
        let r = self.output_rects().get(i).copied().unwrap_or(self.primary);
        if r == self.primary && self.outputs.get(i).is_some_and(|o| o.primary) {
            if !theme::is_mobile() {
                return self.primary_work();
            }
            Rect::new(r.x, r.y, r.w, (bottom - r.y).max(0) as u32)
        } else {
            // Своя панель второго монитора отнимает своё (20.11).
            self.side_panels.iter().find(|(o, _)| *o == i).map_or(r, |(_, panel)| cut_by_panel(r, panel))
        }
    }

    /// Рабочая область монитора `i` для окон: до панели у основного.
    fn work_rect_of(&self, i: usize) -> Rect {
        self.area_of(i, self.app_bottom())
    }

    /// Рабочие области всех мониторов — пределы, в которых двигаются окна.
    fn work_rects(&self) -> ([Rect; MAX_OUTPUTS], usize) {
        let n = self.outputs.len().min(MAX_OUTPUTS);
        let mut areas = [Rect::EMPTY; MAX_OUTPUTS];
        for (i, area) in areas.iter_mut().enumerate().take(n) {
            *area = self.work_rect_of(i);
        }
        (areas, n)
    }

    /// Work area of the monitor under the point (or nearest to it) - where
    /// a window launched from there opens (25.11, `ui::origin`). The
    /// primary's when `at` is `None` and on the phone.
    #[must_use]
    pub fn work_at(&self, at: Option<(i32, i32)>) -> Rect {
        match at {
            Some((x, y)) if !theme::is_mobile() => self.work_rect_of(self.output_near(x, y)),
            _ => self.primary_work(),
        }
    }

    /// Centre of the monitor «Пуск» is open on (25.11: a program chosen in
    /// it with the keyboard opens there).
    #[must_use]
    pub fn menu_point(&self) -> Option<(i32, i32)> {
        let o = self.menu_output.unwrap_or_else(|| self.primary_index());
        self.output_rects().get(o).map(|r| (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2))
    }

    /// Where the window of task `task` stands that its new window (a dialog,
    /// a second window) opens over (25.11): the focused one of the task, or
    /// its topmost not minimized ordinary window.
    #[must_use]
    pub fn task_window_rect(&self, task: u32) -> Option<Rect> {
        let mine = |window: &&Window| matches!(window.app, App::Program(owner, _) if owner == task) && !window.minimized && !window.popup;
        self.windows.get(self.focus).filter(mine).or_else(|| self.windows.iter().rev().find(mine)).map(|window| window.rect)
    }

    /// Рабочая область монитора, на котором середина прямоугольника.
    fn work_rect_under(&self, rect: Rect) -> Rect {
        self.work_rect_of(self.output_near(rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2))
    }

    /// Куда разворачивается окно, стоящее в `rect`: на ПК — рабочая область
    /// монитора под его серединой, на телефоне — [`Self::app_area`].
    fn app_area_for(&self, rect: Rect) -> Rect {
        if theme::is_mobile() {
            return self.app_area();
        }
        self.work_rect_under(rect)
    }

    pub const fn scale(&self) -> mini_ui::Scale {
        self.scale
    }

    // -----------------------------------------------------------------------
    // Окна
    // -----------------------------------------------------------------------

    /// Добавить окно наверх и сделать активным.
    pub fn push(&mut self, window: Window) {
        let rect = window.rect;
        // Диалог затемняет весь стол под собой (макет 18) — перерисовать всё.
        if dims(window.app) {
            self.repaint_all();
        }
        self.windows.push(window);
        self.focus = self.windows.len() - 1;
        self.refresh_decorations();
        // Помечается площадь нового окна, а не весь экран: остальное как было,
        // так и осталось. Разница не косметическая — перерисовка экрана целиком
        // задерживает ввод настолько, что успевает измениться порядок событий.
        self.mark_layer(rect);
        // Окно вырастает из своей плитки — на ПК из значка на столе, а нет
        // значка, из середины своего места. До конца роста оно в списке и
        // принимает ввод, но на месте не рисуется (см. `hidden_by_flight`).
        let app = self.windows[self.focus].app;
        self.start_zoom(app, rect, true);
    }

    /// Добавить всплывающее окно программы над её окном `owner` (фаза 62e).
    ///
    /// Фокус не трогается: меню, открывшись, забрало бы ввод у формы, и стол
    /// сказал бы ей «ты больше не активна» — то есть закрыть это самое меню.
    /// Роста из плитки тоже нет: подсказка, вырастающая полсекунды, — это
    /// подсказка, которой не дождались.
    pub fn push_popup(&mut self, mut window: Window, owner: App) {
        window.owner = Some(owner);
        let rect = window.rect;
        self.windows.push(window);
        self.mark_layer(rect);
    }

    /// Всплывающее ли это окно.
    #[must_use]
    pub fn is_popup(&self, app: App) -> bool {
        self.windows.iter().any(|window| window.app == app && window.popup)
    }

    /// Где всплывающему окну стоять: от угла содержимого владельца, в пределах
    /// экрана. `None` — владельца нет.
    #[must_use]
    pub fn popup_rect(&self, owner: App, anchor: (i32, i32), w: u32, h: u32) -> Option<Rect> {
        let owner = self.windows.iter().find(|window| window.app == owner)?;
        // От угла пикселей владельца: в рамке стола (телефон) они стоят по
        // центру окна, а не под самым заголовком.
        let (ox, oy) = owner.content_origin();
        let x = ox.saturating_add(anchor.0);
        let y = oy.saturating_add(anchor.1);
        // В пределах экрана, а не формы: ради этого всплывающие окна и стали
        // окнами стола. Не влезающее сдвигается внутрь экрана.
        // Мониторов несколько — в пределах того, на котором середина окна
        // (G5: по углу меню, отвёрнутого влево у края монитора, оно
        // перескакивало на соседний).
        let o = self.output_rects()[self.output_near(x.saturating_add(w as i32 / 2), y.saturating_add(h as i32 / 2))];
        let max_x = (o.right() - w as i32).max(o.x);
        let max_y = (o.bottom() - h as i32).max(o.y);
        Some(Rect::new(x.clamp(o.x, max_x), y.clamp(o.y, max_y), w, h))
    }

    /// G5: the corner of `owner`'s pixels and the monitor under `point`
    /// (relative to that corner) - the bounds a Wayland popup is
    /// constrained to (`xdg_positioner`).
    #[must_use]
    pub fn popup_bounds(&self, owner: App, point: (i32, i32)) -> Option<((i32, i32), Rect)> {
        let owner = self.windows.iter().find(|window| window.app == owner)?;
        let (ox, oy) = owner.content_origin();
        let rects = self.output_rects();
        if rects.is_empty() {
            return None;
        }
        let o = rects[self.output_near(ox.saturating_add(point.0), oy.saturating_add(point.1))];
        Some(((ox, oy), o))
    }

    /// Переставить всплывающее окно: новое место от угла владельца (фаза 62e,
    /// `user_abi::SYS_WINPLACE`). `false` — это не всплывающее окно.
    pub fn place_popup(&mut self, app: App, anchor: (i32, i32)) -> bool {
        let Some(window) = self.windows.iter_mut().find(|window| window.app == app && window.popup) else {
            return false;
        };
        window.anchor = anchor;
        let owner = window.owner;
        if let Some(owner) = owner {
            self.follow_owner(owner);
        }
        true
    }

    /// Всплывающие окна владельца встают на свои места от его угла — после
    /// того, как владельца сдвинули, развернули или сменили ему размер.
    fn follow_owner(&mut self, owner: App) {
        let popups: Vec<(App, (i32, i32), u32, u32)> = self
            .windows
            .iter()
            .filter(|window| window.popup && window.owner == Some(owner))
            .map(|window| (window.app, window.anchor, window.rect.w, window.rect.h))
            .collect();
        for (app, anchor, w, h) in popups {
            let Some(rect) = self.popup_rect(owner, anchor, w, h) else {
                continue;
            };
            let Some(window) = self.find(app) else {
                continue;
            };
            let before = window.rect;
            if before == rect {
                continue;
            }
            window.rect = rect;
            self.mark_layer(before);
            self.mark_layer(rect);
        }
    }

    /// Начать рост окна из плитки (`open`) или уход в неё.
    fn start_zoom(&mut self, app: App, rect: Rect, open: bool) -> bool {
        self.finish_flight();
        if self.panel.is_none() || rect.is_empty() || !super::animations_allowed() || !super::look::anim(super::look::Flag::OpenClose) {
            return false;
        }
        // Плитки нет — окно растёт из середины своего места, с четырёх пятых.
        let to = self.icons.tile_of(app).unwrap_or_else(|| {
            let (w, h) = (rect.w * 4 / 5, rect.h * 4 / 5);
            Rect::new(rect.x + (rect.w - w) as i32 / 2, rect.y + (rect.h - h) as i32 / 2, w, h)
        });
        self.flight = Some(Flight::zoom(app, rect, to, open, crate::time::uptime_ns()));
        super::set_animating(true);
        true
    }

    /// Не рисуется ли окно на своём месте, потому что ещё растёт из плитки.
    fn hidden_by_flight(&self, window: &Window) -> bool {
        self.flight
            .as_ref()
            .is_some_and(|flight| flight.hides_window() && flight.app == window.app)
    }

    pub fn find(&mut self, app: App) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.app == app)
    }

    /// The window of `app`, read-only (23.11: the journal of content sizes).
    #[must_use]
    pub fn peek(&self, app: App) -> Option<&Window> {
        self.windows.iter().find(|window| window.app == app)
    }

    /// Окно стало прозрачным или непрозрачным (см. [`Window::translucent`]):
    /// перерисовать его место вместе с тенью — она появляется или уходит.
    pub fn set_alpha(&mut self, app: App, translucent: bool, shaped: bool) -> bool {
        let Some(window) = self.find(app) else {
            return false;
        };
        let shaped = translucent && shaped;
        if (window.translucent, window.shaped) != (translucent, shaped) {
            window.translucent = translucent;
            window.shaped = shaped;
            let rect = window.rect;
            self.mark_layer(rect);
        }
        true
    }

    /// Окно программы, над **содержимым** которого стоит точка, и эта точка
    /// внутри его поверхности. Заголовок и рамка — не содержимое: программа их
    /// не рисует и событий о них не ждёт.
    #[must_use]
    pub fn program_under(&self, x: i32, y: i32) -> Option<(App, i32, i32)> {
        let (index, hit) = self.window_at(x, y)?;
        let window = self.windows.get(index)?;
        if hit != Hit::Body || !window.is_program() {
            return None;
        }
        // Поле вокруг пикселей в рамке стола (телефон, 05.11) — не программы.
        if window.fit && !window.on_pixels(x, y) {
            return None;
        }
        let (lx, ly) = window.program_local(x, y);
        Some((window.app, lx, ly))
    }

    /// Окно ядра (не программы) под точкой — для колеса мыши (фаза 69b).
    pub fn window_under(&self, x: i32, y: i32) -> Option<App> {
        let (index, hit) = self.window_at(x, y)?;
        let window = self.windows.get(index)?;
        (hit == Hit::Body && !window.is_program()).then_some(window.app)
    }

    /// Подсветить кнопку заголовка под указателем (72b). `x, y` — точка
    /// экрана; вне кнопок подсветка снимается.
    pub fn hover_title(&mut self, x: i32, y: i32) {
        let under = self.window_at(x, y).and_then(|(index, hit)| {
            matches!(hit, Hit::Close | Hit::Maximize | Hit::Minimize).then(|| (self.windows[index].app, hit))
        });
        // Указатель на «Развернуть» — отсчёт до макетов прилипания (20.11).
        match under {
            Some((app, Hit::Maximize)) if !theme::is_mobile() && self.snap_pop.is_none() => {
                if self.snap_hover.map(|(was, _, _)| was) != Some(app) {
                    let side = Window::title_height(self.scale);
                    let top = self.rect_of(app).map_or(y, |rect| rect.y);
                    let button = Rect::new(x - side as i32 / 2, top, side, side);
                    self.snap_hover = Some((app, button, crate::time::uptime_ms()));
                    super::set_animating(true);
                }
            }
            _ => self.snap_hover = None,
        }
        let focus = self.focus;
        for (index, window) in self.windows.iter_mut().enumerate() {
            let hot = under.filter(|(app, _)| *app == window.app).map(|(_, hit)| hit);
            window.set_hot(hot, index == focus);
        }
    }

    /// См. [`Self::hovered`].
    pub const fn hovered(&self) -> Option<App> {
        self.hovered
    }

    pub fn set_hovered(&mut self, app: Option<App>) {
        self.hovered = app;
    }

    /// Показать окно `app` поверх всех (см. [`Self::peek`]); `None` — убрать.
    pub fn set_peek(&mut self, app: Option<App>) {
        if self.peek == app {
            return;
        }
        // Подсмотр начался или кончился: стол целиком меняет вид - размытый
        // под окном или обычный.
        match app {
            // Без «Прозрачности и размытия» (20.11) окно подсматривается
            // поверх как есть — без размытого стола под ним.
            Some(_) if self.peek_back.is_none() && super::look::on(super::look::Flag::Effects) => {
                self.build_peek_back();
                if self.peek_back.is_some() {
                    // Размытие наплывает за [`PEEK_FADE_MS`], а не встаёт
                    // разом (Роман, 20.11: «хоп и топорно»).
                    if super::animations_allowed() {
                        self.peek_fade = Some((crate::time::uptime_ms(), true));
                        super::set_animating(true);
                    }
                    self.mark_layer(self.union);
                }
            }
            // Ушло, не успев уйти, — возвращается с той же силы.
            Some(_) if self.peek_back.is_some() && matches!(self.peek_fade, Some((_, false))) => {
                let alpha = u64::from(self.peek_alpha());
                self.peek_fade = Some((crate::time::uptime_ms().saturating_sub(alpha * PEEK_FADE_MS / 255), true));
                super::set_animating(true);
                self.mark_layer(self.union);
            }
            None if self.peek_back.is_some() => {
                if super::animations_allowed() {
                    let alpha = u64::from(self.peek_alpha());
                    self.peek_fade = Some((crate::time::uptime_ms().saturating_sub((255 - alpha.min(255)) * PEEK_FADE_MS / 255), false));
                    super::set_animating(true);
                } else {
                    self.peek_back = None;
                    self.peek_fade = None;
                }
                self.mark_layer(self.union);
            }
            _ => {}
        }
        for old in [self.peek, app].into_iter().flatten() {
            if let Some(rect) = self.windows.iter().find(|window| window.app == old).map(|window| window.rect) {
                self.mark_layer(rect);
            }
        }
        self.peek = app;
    }

    /// Сила размытой подложки подсмотра сейчас (0..=255).
    fn peek_alpha(&self) -> u32 {
        match self.peek_fade {
            None => {
                if self.peek_back.is_some() { 255 } else { 0 }
            }
            Some((start, showing)) => {
                let t = (crate::time::uptime_ms().saturating_sub(start).min(PEEK_FADE_MS) * 255 / PEEK_FADE_MS) as u32;
                if showing { t } else { 255 - t }
            }
        }
    }

    /// Шаг наплыва подсмотра перед кадром. `true` — ещё идёт.
    fn advance_peek(&mut self) -> bool {
        let Some((start, showing)) = self.peek_fade else {
            return false;
        };
        self.mark(self.union);
        if crate::time::uptime_ms().saturating_sub(start) < PEEK_FADE_MS {
            return true;
        }
        self.peek_fade = None;
        if !showing {
            self.peek_back = None;
        }
        false
    }

    /// Запомнить, что нажатие `key` ушло окну `app` событием `event`.
    pub fn hold_key(&mut self, key: KeyCode, app: App, event: WinEvent) {
        // Повтор зажатой клавиши занимает то же место, а не новое.
        let slot = self
            .held_keys
            .iter()
            .position(|held| held.is_some_and(|(code, ..)| code == key))
            .or_else(|| self.held_keys.iter().position(Option::is_none));
        if let Some(slot) = slot {
            self.held_keys[slot] = Some((key, app, event));
        }
    }

    /// Клавишу отпустили: кому досталось её нажатие и каким событием.
    pub fn release_key(&mut self, key: KeyCode) -> Option<(App, WinEvent)> {
        let slot = self.held_keys.iter().position(|held| held.is_some_and(|(code, ..)| code == key))?;
        self.held_keys[slot].take().map(|(_, app, event)| (app, event))
    }

    /// Сменилось ли активное окно с прошлого раза: `Some((было, стало))`, и
    /// новое запоминается как сказанное (фаза 62c).
    pub fn take_focus_change(&mut self) -> Option<(Option<App>, Option<App>)> {
        let now = self.focused_app();
        if now == self.reported_focus {
            return None;
        }
        Some((core::mem::replace(&mut self.reported_focus, now), now))
    }

    /// См. [`Self::captured`].
    pub const fn captured(&self) -> Option<(App, u32)> {
        self.captured
    }

    /// Кнопку `button` нажали над содержимым окна `app`: указатель теперь его.
    pub fn capture(&mut self, app: App, button: u32) {
        match self.captured {
            Some((held, mask)) if held == app => self.captured = Some((app, mask | button)),
            // Вторая кнопка над другим окном захват не переносит: первая всё
            // ещё держится, и её отпускание обещано первому окну.
            Some(_) => {}
            None => self.captured = Some((app, button)),
        }
    }

    /// Снять захват указателя целиком: окно программы отдало кнопку столу
    /// (`ui::begin_move`, фаза 71i), отпускание ему уже не обещано.
    pub fn drop_capture(&mut self) {
        self.captured = None;
    }

    /// Кнопку `button` отпустили. Ответ — окно, которому об этом сказать; захват
    /// кончается, когда отпущены все кнопки.
    pub fn release_capture(&mut self, button: u32) -> Option<App> {
        let (app, mask) = self.captured?;
        if mask & button == 0 {
            return None;
        }
        let rest = mask & !button;
        self.captured = if rest == 0 { None } else { Some((app, rest)) };
        Some(app)
    }

    /// Точка экрана в координатах поверхности окна программы — в том числе за
    /// её краем: захваченному окну нужна и такая.
    #[must_use]
    pub fn program_point(&self, app: App, x: i32, y: i32) -> Option<(i32, i32)> {
        let window = self.windows.iter().find(|window| window.app == app)?;
        Some(window.program_local(x, y))
    }

    /// Где стоит окно программы.
    #[must_use]
    pub fn rect_of(&self, app: App) -> Option<Rect> {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .map(|window| window.rect)
    }

    /// Каскад (20.11): новое окно, чей угол пришёлся на угол уже открытого,
    /// сдвигается вниз-вправо на высоту заголовка — как у Windows, чтобы
    /// второе окно не легло ровно поверх первого. Сдвиг, выводящий окно за
    /// рабочую область, не делается.
    #[must_use]
    pub fn cascade(&self, rect: Rect) -> Rect {
        let step = Window::title_height(self.scale) as i32;
        // The monitor the window is on (25.11: windows open on the monitor
        // they were launched from, not only on the primary).
        let work = self.work_rect_under(rect);
        let mut out = rect;
        for _ in 0..16 {
            let taken = self
                .windows
                .iter()
                .any(|window| (window.rect.x - out.x).abs() < step / 2 && (window.rect.y - out.y).abs() < step / 2);
            if !taken {
                break;
            }
            let next = Rect::new(out.x + step, out.y + step, out.w, out.h);
            if next.right() > work.right() || next.bottom() > work.bottom() {
                break;
            }
            out = next;
        }
        out
    }

    /// Как называется окно этой программы.
    ///
    /// Нужно журналу: у окна программы имя своё, и [`App::title`] о нём не
    /// знает. Строка собственная, а не заимствованная, по той же причине, что и
    /// в [`Compositor::buttons`], — стол выходит из-под замка, окно может
    /// закрыться.
    #[must_use]
    pub fn caption_of(&self, app: App) -> Option<alloc::string::String> {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .map(|window| window.caption().into())
    }

    /// Какая программа живёт в окне с этим номером.
    #[must_use]
    pub fn app_at(&self, index: usize) -> Option<App> {
        self.windows.get(index).map(|window| window.app)
    }

    /// Номер окна программы в порядке по глубине.
    pub fn index_of(&self, app: App) -> Option<usize> {
        self.windows.iter().position(|window| window.app == app)
    }

    /// Активное окно — то, которому достаётся ввод.
    ///
    /// Свёрнутое окно активным **не бывает**, и это не придирка к слову. Ввод
    /// уходит туда, что вернёт эта функция ([`super::route`]), а `None` означает
    /// «в оболочку». Свёрнутое окно, названное активным, означает клавиши,
    /// уходящие туда, где их некому показать: строка, набранная человеком,
    /// пропадает без следа, и выглядит это как «система перестала принимать
    /// команды».
    ///
    /// Куплено красным прогоном 48a: сценарий `installed` сворачивал оба окна и
    /// после этого посылал команду в серийную линию. Она уезжала в свёрнутое
    /// окно монитора.
    pub fn focused_app(&self) -> Option<App> {
        let window = self.windows.get(self.focus)?;
        (!window.minimized).then_some(window.app)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Window> {
        self.windows.get_mut(self.focus)
    }

    /// Кнопки панели задач: порядок создания, а не порядок по глубине.
    ///
    /// Именно порядок создания: кнопка, переезжающая с места на место при каждом
    /// переключении окон, — это кнопка, в которую нельзя попасть.
    ///
    /// Список окон упорядочен по глубине — поднятое окно уезжает в его конец, —
    /// поэтому кнопки сортируются по [`Window::born`] (24.10: свёрнутые
    /// «Параметры» и «Файлы» менялись местами при разворачивании).
    pub fn buttons(&self) -> super::panel::Windows {
        let mut order: Vec<(usize, &Window)> = self.windows.iter().enumerate().collect();
        order.sort_by_key(|(_, window)| window.born);
        order
            .into_iter()
            // Всплывающему окну кнопка не положена: меню — часть формы.
            // Модальному подчинённому (20.11, «Сохранить как») — тоже: кнопка
            // одна, у владельца, и она активна, пока активен его вопрос.
            .filter(|(_, window)| !window.popup && !(window.modal && window.owner.is_some()))
            .map(|(index, window)| super::panel::Entry {
                app: window.app,
                caption: window.label(),
                focused: index == self.focus
                    || self.windows.get(self.focus).is_some_and(|top| top.modal && top.owner == Some(window.app)),
                minimized: window.minimized,
                art: window.icon_art(),
                group: if window.group.is_empty() { alloc::string::String::from(window.app.title()) } else { window.group.clone() },
            })
            .collect()
    }

    /// Кнопки панели монитора `output` (20.11): окна, чья кнопка на ней.
    fn buttons_on(&self, output: usize) -> super::panel::Windows {
        if self.side_panels.is_empty() {
            return self.buttons();
        }
        let mut entries = self.buttons();
        // «Показывать кнопки окон» (21.11, как в Windows): по умолчанию все
        // окна на всех панелях; активной кнопка горит только на панели
        // монитора, где окно открыто, — видно, где его искать.
        if !super::deskcfg::taskbar().own {
            for entry in &mut entries {
                if entry.focused && !self.rect_of(entry.app).is_some_and(|rect| self.panel_output_of(rect) == output) {
                    entry.focused = false;
                }
            }
            return entries;
        }
        entries.retain(|entry| self.rect_of(entry.app).is_some_and(|rect| self.panel_output_of(rect) == output));
        entries
    }

    /// Окна программ (не окна ядра).
    #[must_use]
    pub fn program_apps(&self) -> Vec<App> {
        self.windows.iter().map(|window| window.app).filter(|app| matches!(app, App::Program(..))).collect()
    }

    /// Окна той же программы, что `app`, — в порядке кнопок панели (одна
    /// кнопка на программу, когда панель без подписей).
    #[must_use]
    pub fn group_members(&self, app: App) -> Vec<App> {
        let entries = self.buttons();
        let Some(group) = entries.iter().find(|entry| entry.app == app).map(|entry| entry.group.clone()) else {
            return Vec::new();
        };
        entries.into_iter().filter(|entry| entry.group == group).map(|entry| entry.app).collect()
    }

    /// Поднять окно с номером `index` наверх и сделать активным.
    pub fn raise(&mut self, index: usize) {
        if index >= self.windows.len() {
            return;
        }
        // Всплывающее окно поднимается, но фокус остаётся, где был (фаза 62e):
        // щелчок по пункту меню — это щелчок формы, а не уход от неё.
        if self.windows[index].popup {
            let focused = self.windows.get(self.focus).map(|window| window.app);
            let window = self.windows.remove(index);
            let rect = window.rect;
            self.windows.push(window);
            if let Some(index) = focused.and_then(|app| self.index_of(app)) {
                self.focus = index;
            }
            self.mark_layer(rect);
            return;
        }
        // Кто теряет фокус — запоминается до перестановки: индексы после неё
        // съедут, а перекрасить надо ровно два заголовка. Перекрашивать все
        // окна здесь дорого не «в принципе», а измеримо: на 1920×1080 в
        // отладочной сборке переключение окон занимало столько, что второе
        // нажатие Tab не успевало быть прочитанным с клавиатуры и терялось.
        let losing = self.windows.get(self.focus).map(|window| window.app);
        // Перерисовать надо не всё поднятое окно, а только то, что его
        // закрывало: пиксели, видные и до, и после поднятия, не изменились.
        // Разница измерима — на 1920×1080 в отладочной сборке перерисовка окна
        // целиком занимала больше четверти секунды, и следующее нажатие
        // клавиши терялось: клавиатуру некому было опросить.
        let mut uncovered = [Rect::EMPTY; MAX_DAMAGE];
        let mut uncovered_count = 0;
        {
            let raised = self.windows[index].rect;
            for above in self.windows[index + 1..].iter().filter(|w| !w.minimized) {
                let overlap = raised.intersect(&above.rect);
                if !overlap.is_empty() && uncovered_count < MAX_DAMAGE {
                    uncovered[uncovered_count] = overlap;
                    uncovered_count += 1;
                }
            }
        }
        let window = self.windows.remove(index);
        self.windows.push(window);
        self.focus = self.windows.len() - 1;
        if let Some(app) = losing {
            if let Some(previous) = self.windows.iter_mut().find(|window| window.app == app) {
                previous.draw_decorations(false);
            }
        }
        if let Some(window) = self.windows.last_mut() {
            window.draw_decorations(true);
        }
        // Поднятое окно — единственное место, где порядок по глубине
        // изменился; всё остальное на экране осталось прежним.
        for index in 0..uncovered_count {
            self.mark_layer(uncovered[index]);
        }
        self.lift_owned();
    }

    /// Поднять над верхним окном его подчинённые, а над ними — их подчинённые
    /// (фаза 62d, `user_abi::SYS_WINOWNER`).
    ///
    /// Фокус остаётся у поднятого окна, если среди поднятых следом нет
    /// модального; есть — достаётся верхнему модальному: форма, ждущая ответа,
    /// сама ввода не примет, а вопрос под ней человек бы не нашёл.
    fn lift_owned(&mut self) {
        let Some(raised) = self.windows.last().map(|window| window.app) else {
            return;
        };
        let mut owner = raised;
        let mut modal = None;
        // Кольца в цепочке владельцев не бывает (см. [`Self::set_owner`]), а
        // предел по числу окон — на случай, если однажды оно всё же появится.
        for _ in 0..self.windows.len() {
            // Сначала — кто, потом — перестановка: окно, уехавшее в конец
            // списка, обход увидел бы второй раз.
            let owned: Vec<App> = self
                .windows
                .iter()
                .filter(|window| window.owner == Some(owner) && !window.minimized)
                .map(|window| window.app)
                .collect();
            if owned.is_empty() {
                break;
            }
            for app in owned {
                let Some(index) = self.index_of(app) else {
                    continue;
                };
                let window = self.windows.remove(index);
                if window.modal {
                    modal = Some(window.app);
                }
                let rect = window.rect;
                self.windows.push(window);
                self.mark_layer(rect);
            }
            let Some(top) = self.windows.last().map(|window| window.app) else {
                break;
            };
            owner = top;
        }
        if owner == raised {
            return;
        }
        let focused = modal.unwrap_or(raised);
        if let Some(index) = self.index_of(focused) {
            self.focus = index;
        }
        self.refresh_decorations();
    }

    /// Назначить окну `app` владельца (`None` — снять). Отказ — владелец не
    /// найден, совпадает с окном или сам ему подчинён: цепочка владельцев не
    /// бывает кольцом.
    pub fn set_owner(&mut self, app: App, owner: Option<App>, modal: bool) -> bool {
        if let Some(owner) = owner {
            let mut step = Some(owner);
            for _ in 0..=self.windows.len() {
                let Some(current) = step else {
                    break;
                };
                if current == app {
                    return false;
                }
                let Some(window) = self.windows.iter().find(|window| window.app == current) else {
                    return false;
                };
                step = window.owner;
            }
        }
        let Some(window) = self.find(app) else {
            return false;
        };
        window.owner = owner;
        window.modal = modal && owner.is_some();
        // Подчинённое окно сразу встаёт над владельцем: обычно оно и так
        // наверху — только что открыто, — но связь могли назначить и позже.
        if let Some(index) = owner.and_then(|owner| self.index_of(owner)) {
            if self.focus == index {
                self.raise(index);
            }
        }
        true
    }

    /// Есть ли у окна `app` открытое модальное подчинённое (20.11): пока оно
    /// открыто, владелец ввода не принимает — ни щелчков, ни колеса, ни кнопок
    /// заголовка, как окно под «Сохранить как» в Windows.
    #[must_use]
    pub fn has_modal(&self, app: App) -> bool {
        self.windows.iter().any(|window| window.owner == Some(app) && window.modal)
    }

    /// Поставить окно `app` посередине окна `owner` в пределах рабочей
    /// области (20.11, `WIN_OWNER_CENTER`). `true` — сдвинуто.
    pub fn center_over(&mut self, app: App, owner: App) -> bool {
        let Some(over) = self.rect_of(owner) else {
            return false;
        };
        let work = self.app_area_for(over);
        let Some(window) = self.find(app) else {
            return false;
        };
        let before = window.rect;
        let (w, h) = (before.w as i32, before.h as i32);
        let x = (over.x + (over.w as i32 - w) / 2).min(work.right() - w).max(work.x);
        let y = (over.y + (over.h as i32 - h) / 2).min(work.bottom() - h).max(work.y);
        if (x, y) == (before.x, before.y) {
            return false;
        }
        window.rect.x = x;
        window.rect.y = y;
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
        true
    }

    /// Поднять нижнее окно наверх — обход по кругу.
    pub fn focus_next(&mut self) {
        if self.windows.len() < 2 {
            return;
        }
        self.raise(0);
    }

    /// Закрыть активное окно.
    pub fn close_focused(&mut self) -> Option<App> {
        if self.windows.is_empty() {
            return None;
        }
        let window = self.windows.remove(self.focus);
        Self::forget_terminal(&window);
        self.focus = self.windows.len().saturating_sub(1);
        self.refresh_decorations();
        // На месте закрытого окна снова виден фон и то, что было под ним.
        self.mark_layer(window.rect);
        Some(window.app)
    }

    /// Сдвинуть активное окно.
    pub fn move_focused(&mut self, dx: i32, dy: i32) {
        let (areas, n) = self.work_rects();
        let Some(window) = self.windows.get_mut(self.focus) else {
            return;
        };
        if window.fullscreen {
            return;
        }
        let before = window.rect;
        window.move_within(dx, dy, &areas[..n]);
        if window.rect == before {
            return;
        }
        // Сдвинутое развёрнутое окно больше не развёрнуто (20.11): кнопка
        // снова «развернуть», а не «вернуть».
        window.forget_maximized(true);
        let after = window.rect;
        // Два прямоугольника, а не один объединяющий: при большом сдвиге
        // объединение — это почти весь экран, тогда как настоящих изменений два
        // куска по краям.
        self.mark_layer(before);
        self.mark_layer(after);
    }

    // -----------------------------------------------------------------------
    // Указатель
    // -----------------------------------------------------------------------

    /// Сдвинуть указатель и пометить изменившееся.
    ///
    /// Помечаются **два** прямоугольника — откуда стрелка ушла и куда пришла.
    /// Один объединяющий при быстром движении накрыл бы полэкрана, тогда как
    /// настоящих изменений два пятна размером с курсор.
    pub fn move_pointer(&mut self, dx: i32, dy: i32) {
        self.move_pointer_by(dx, dy);
        if self.cursor_hidden_by.is_some() {
            self.sync_client_cursor();
        }
    }

    fn move_pointer_by(&mut self, dx: i32, dy: i32) {
        let before = self.pointer.rect();
        let outs = self.out_rects;
        let moved = self.pointer.move_by(dx, dy, &outs[..self.outputs.len().min(MAX_OUTPUTS)]);
        let appeared = self.pointer.show();
        if !moved && !appeared {
            return;
        }
        if self.hand_cursor(appeared) {
            return;
        }
        if moved {
            self.mark(before);
        }
        let after = self.pointer.rect();
        self.mark(after);
    }

    /// Программа запускается: рядом со стрелкой крутится круг, пока она не
    /// откроет окно (или не выйдет срок `ms`).
    pub fn pointer_busy(&mut self, ms: u64) {
        self.pointer.set_busy(crate::time::uptime_ms() + ms);
        self.mark(self.pointer.rect());
        super::set_animating(true);
    }

    /// Окно появилось — круг больше не нужен.
    pub fn pointer_idle(&mut self) {
        if let Some(was) = self.pointer.clear_busy() {
            self.mark(was);
        }
    }

    /// Поставить указатель туда, куда показало устройство, и пометить
    /// изменившееся. Возвращает **фактическое** приращение положения.
    ///
    /// Приращение возвращается не для симметрии: за курсором тащится окно, а
    /// планшет о приращениях ничего не сообщает — единственный, кто их знает,
    /// это тот, кто хранит прежнее положение. Заодно из ответа исчезает та
    /// часть движения, которую съел край экрана.
    pub fn move_pointer_to(&mut self, x_fraction: u16, y_fraction: u16) -> (i32, i32) {
        let moved = self.move_pointer_at(x_fraction, y_fraction);
        if self.cursor_hidden_by.is_some() {
            self.sync_client_cursor();
        }
        moved
    }

    /// G3: a program window hides (`hidden`) or shows the pointer over
    /// itself (`wl_pointer.set_cursor`).
    pub fn set_client_cursor(&mut self, app: App, hidden: bool) {
        let before = self.cursor_hidden_by;
        if hidden {
            self.cursor_hidden_by = Some(app);
        } else if self.cursor_hidden_by == Some(app) {
            self.cursor_hidden_by = None;
        }
        if self.cursor_hidden_by != before {
            crate::kprintln!(
                "  desktop     : '{}' {} the pointer over its window",
                self.label(app),
                if hidden { "hides" } else { "shows" }
            );
        }
        self.sync_client_cursor();
    }

    /// G3: hide the pointer while it is over the window that hid it, show it
    /// elsewhere (a pointer lock hides it on its own).
    fn sync_client_cursor(&mut self) {
        if theme::is_mobile() || self.constraint.is_some_and(|c| c.active && c.lock) {
            return;
        }
        let (x, y) = self.pointer.position();
        let hide =self.cursor_hidden_by.is_some_and(|app| {
            self.window_at(x, y).is_some_and(|(index, _)| self.windows.get(index).is_some_and(|window| window.app == app))
        });
        let rect = self.pointer.rect();
        if hide {
            if self.pointer.hide() {
                if self.hw_cursor {
                    crate::display::cursor_visible(false, x, y);
                }
                self.mark(rect);
            }
        } else if self.pointer.show() {
            if self.hw_cursor {
                crate::display::cursor_visible(true, x, y);
                self.cursor_sent = (x, y);
            }
            self.mark(rect);
        }
    }

    fn move_pointer_at(&mut self, x_fraction: u16, y_fraction: u16) -> (i32, i32) {
        // Планшет показывает точку на основном мониторе: устройство одно, и
        // его поле ложится на тот экран, где панель.
        let p = self.primary;
        let (from_x, from_y) = self.pointer.position();
        let before = self.pointer.rect();

        let outs = self.out_rects;
        let moved = self.pointer.move_to(
            p.x + scale_fraction(x_fraction, p.w),
            p.y + scale_fraction(y_fraction, p.h),
            &outs[..self.outputs.len().min(MAX_OUTPUTS)],
        );
        let appeared = self.pointer.show();
        if !moved && !appeared {
            return (0, 0);
        }
        if self.hand_cursor(appeared) {
            let (to_x, to_y) = self.pointer.position();
            return (to_x - from_x, to_y - from_y);
        }
        if moved {
            self.mark(before);
        }
        let after = self.pointer.rect();
        self.mark(after);

        let (to_x, to_y) = self.pointer.position();
        (to_x - from_x, to_y - from_y)
    }

    /// Курсор адаптера (фаза 67b): `true` — стрелку показывает адаптер, и
    /// пересобирать под ней кадр не надо.
    ///
    /// Стрелка отдаётся адаптеру, когда указатель впервые появляется, — до
    /// этого её нет и на экране стола. Адаптер, который так не умеет
    /// (stdvga, ramfb), отвечает `false`, и всё идёт по-старому. На телефоне
    /// стрелки нет вовсе.
    fn hand_cursor(&mut self, appeared: bool) -> bool {
        // Мониторов несколько — стрелку рисует стол: курсор адаптера живёт на
        // одном выходе (у virtio-gpu — на нулевом), а полоса стола одна на
        // все мониторы и стрелку в ней покажет каждый, через который она идёт.
        if theme::is_mobile() || self.outputs.len() > 1 {
            return false;
        }
        let (x, y) = self.pointer.position();
        if !self.hw_cursor {
            if !appeared {
                return false;
            }
            let (image, hot) = self.pointer.image(crate::display::CURSOR_SIDE);
            self.hw_cursor = crate::display::cursor_show(&image, hot, x, y);
            if self.hw_cursor {
                self.cursor_sent = (x, y);
                self.cursor_key = self.pointer.image_key();
                crate::kprintln!("  desktop     : the pointer is drawn by the display adapter");
            }
            return self.hw_cursor;
        }
        // Другая форма или кадр круга — картинку отдать заново.
        let key = self.pointer.image_key();
        if key != self.cursor_key {
            let (image, hot) = self.pointer.image(crate::display::CURSOR_SIDE);
            crate::display::cursor_show(&image, hot, x, y);
            self.cursor_key = key;
            self.cursor_sent = (x, y);
        } else if self.cursor_sent != (x, y) {
            crate::display::cursor_move(x, y);
            self.cursor_sent = (x, y);
        }
        true
    }

    /// Сменить форму курсора (72c): стрелка, текст, растяжка…
    pub fn set_pointer_shape(&mut self, shape: super::pointer::Shape) {
        if !self.pointer.set_shape(shape) {
            return;
        }
        if self.hw_cursor {
            self.hand_cursor(false);
        } else {
            self.mark(self.pointer.rect());
        }
    }

    #[must_use]
    pub const fn pointer_position(&self) -> (i32, i32) {
        self.pointer.position()
    }

    #[must_use]
    pub const fn pointer_visible(&self) -> bool {
        self.pointer.is_visible()
    }

    /// Верхнее окно под точкой и то, во что она попала.
    #[must_use]
    pub fn window_at(&self, x: i32, y: i32) -> Option<(usize, Hit)> {
        // Сверху вниз: перекрытое окно щелчок получать не должно. Свёрнутого
        // окна на экране нет — щелчок проходит сквозь его прежнее место.
        // G5: the window holding the pointer constraint has no resize border
        // - the confined pointer rests on its edge, and that is the game's
        // pixel (motion, clicks), not a grip.
        let held = self.active_constraint().map(|(app, _)| app);
        self.windows
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, window)| !window.minimized)
            .find_map(|(index, window)| {
                window.hit(x, y).map(|hit| match hit {
                    Hit::Resize(_) if held == Some(window.app) => (index, Hit::Body),
                    hit => (index, hit),
                })
            })
    }

    /// Значок стола под точкой — если она не накрыта окном.
    ///
    /// Проверка «не накрыта окном» здесь, а не у вызывающего: значок лежит на
    /// столе, то есть ниже всех окон, и щелчок сквозь окно по значку под ним —
    /// это ровно та ошибка, которую пользователь заметит первой.
    #[must_use]
    pub fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        if self.window_at(x, y).is_some() {
            return None;
        }
        self.icons.at(x, y)
    }

    /// Что за значок стоит на этом месте сетки.
    #[must_use]
    pub fn icon_kind(&self, index: usize) -> Option<Kind> {
        self.icons.item(index).map(|item| item.kind.clone())
    }

    /// Подпись значка.
    #[must_use]
    pub fn icon_label(&self, index: usize) -> Option<alloc::string::String> {
        self.icons.item(index).map(|item| item.label.clone())
    }

    /// Путь к тому, что за значком, — только у файлов и каталогов стола.
    #[must_use]
    pub fn icon_path(&self, index: usize) -> Option<alloc::string::String> {
        self.icons.item(index).and_then(|item| item.path.clone())
    }

    /// Какой значок выбран сейчас.
    #[must_use]
    pub fn icon_selection(&self) -> Option<usize> {
        self.icons.selection()
    }

    /// Сколько значков на столе и сколько из них — файлы и каталоги.
    #[must_use]
    pub fn icon_counts(&self) -> (usize, usize) {
        self.icons.counts()
    }

    /// Выделить значок по пути — тем, кто только что переименовал файл.
    pub fn select_icon_path(&mut self, path: &str) {
        let index = self.icons.index_of_path(path);
        self.select_icon(index);
    }

    /// Выделить значок (или снять выделение).
    pub fn select_icon(&mut self, index: Option<usize>) {
        let damage = self.icons.select(index);
        self.icon_damage(damage);
    }

    /// Перерисовать то, что значки задели.
    fn icon_damage(&mut self, damage: Rect) {
        self.invalidate_under_rect(damage);
        if !damage.is_empty() {
            self.mark(damage);
        }
    }

    /// Ctrl+щелчок по значку: добавить к выделению или убрать.
    pub fn toggle_icon(&mut self, index: usize) {
        let damage = self.icons.toggle(index);
        self.icon_damage(damage);
    }

    /// Начать правку подписи значка на месте.
    pub fn start_icon_edit(&mut self, index: usize) {
        let damage = self.icons.start_edit(index);
        self.icon_damage(damage);
    }

    /// Правится ли подпись: номер значка и набранное.
    #[must_use]
    pub fn icon_editing(&self) -> Option<(usize, alloc::string::String)> {
        self.icons.editing().map(|(index, text)| (index, alloc::string::String::from(text)))
    }

    /// Клавиша в поле имени значка.
    pub fn icon_edit_key(&mut self, code: crate::input::KeyCode, ch: Option<char>, ctrl: bool) -> super::icons::EditReply {
        let reply = self.icons.edit_key(code, ch, ctrl);
        let damage = self.icons.edit_area();
        self.icon_damage(damage);
        reply
    }

    /// Закончить правку подписи.
    pub fn end_icon_edit(&mut self) {
        let damage = self.icons.end_edit();
        self.icon_damage(damage);
    }

    /// Сделать выделенный значок главным, не снимая остального выделения.
    pub fn focus_icon(&mut self, index: usize) {
        self.icons.focus(index);
    }

    /// Выделен ли значок.
    #[must_use]
    pub fn icon_marked(&self, index: usize) -> bool {
        self.icons.is_marked(index)
    }

    /// Номера выделенных значков.
    #[must_use]
    pub fn marked_icons(&self) -> alloc::vec::Vec<usize> {
        self.icons.marked()
    }

    /// Рамка выделения на столе (`None` — отпустили); `add` — что было
    /// выделено до неё и остаётся (с Ctrl).
    pub fn icon_band(&mut self, band: Option<Rect>, add: &[usize]) {
        let damage = self.icons.set_band(band, add);
        self.icon_damage(damage);
    }

    /// Нести выделенные значки: сдвиг от точки, где их взяли.
    pub fn lift_icons(&mut self, dx: i32, dy: i32) {
        let damage = self.icons.lift(dx, dy);
        self.icon_damage(damage);
    }

    /// Положить несомые значки по сетке и запомнить их места.
    pub fn drop_icons(&mut self) {
        let before = self.icons.bounds();
        let (damage, places) = self.icons.drop_lifted();
        self.icon_damage(damage.union(&before).union(&self.icons.bounds()));
        if let Some(places) = places {
            super::icons::set_places(&places);
            match super::prefs::store_icon_places(&places) {
                Ok(()) => crate::kprintln!("  desktop     : icons moved, places saved"),
                Err(err) => crate::kprintln!("  desktop     : icons moved, places not saved: {err}"),
            }
        }
    }

    /// Перечитать каталог стола и показать его заново.
    ///
    /// Помечается объединение сетки **до** и **после**: удалённый файл
    /// освобождает ячейку, которую иначе никто не стёр бы, и она осталась бы на
    /// экране до первой чужой перерисовки.
    /// Поставили или сняли пакет со значком — перечитать значки.
    pub fn reload_icons_if_packages_changed(&mut self) {
        // Что стоит сейчас — запоминается при первом взгляде, чтобы о уже
        // стоявших программах не уведомлять.
        if self.known_packages.is_none() {
            self.known_packages = Some(super::panel::list_packages().into_iter().map(|launcher| launcher.name).collect());
        }
        if self.icons.packages_changed() {
            super::deskcfg::packages_changed();
            self.reload_icons();
            let (total, entries) = self.icons.counts();
            crate::kprintln!("  desktop     : packages changed, {total} icons ({entries} from the desktop folder)");
            // Новая программа — уведомление «… установлен» (72h-2, макет 19).
            let packages = super::panel::list_packages();
            let known = self.known_packages.take().unwrap_or_default();
            let fresh = packages.iter().find(|launcher| !known.contains(&launcher.name) && !launcher.start.is_empty());
            if let Some(launcher) = fresh {
                let art = super::appicon::of_manifest(&launcher.icon);
                let panel_top = self.panel.as_ref().map_or(self.primary.bottom(), |panel| panel.home().y);
                if let Some(mut toast) = super::toast::Toast::new(
                    &launcher.caption,
                    &art,
                    launcher.start.clone(),
                    launcher.rights,
                    self.primary,
                    panel_top,
                    self.scale,
                ) {
                    toast.rect = self.beside_panel(toast.rect, toast.rect);
                    self.close_toast();
                    self.mark_layer(toast.rect);
                    self.toast = Some(toast);
                    crate::kprintln!("  desktop     : notification '{} установлен'", launcher.caption);
                }
            }
            self.known_packages = Some(packages.into_iter().map(|launcher| launcher.name).collect());
        }
    }

    /// Закрыть стол экраном блокировки (Win+L).
    pub fn lock_screen(&mut self) {
        if self.lock.is_some() {
            return;
        }
        // Картинка блокировки — размером с основной монитор: на нём часы и
        // поле пароля, остальные мониторы на это время чёрные.
        let (w, h) = (self.primary.w, self.primary.h);
        let Some(surface) = Surface::new(w, h, theme::palette().wall_top) else {
            return;
        };
        self.lock = Some((super::lock::Lock::new(), surface));
        self.redraw_lock();
    }

    /// Перерисовать экран блокировки: обои и то, что поверх.
    pub fn redraw_lock(&mut self) {
        let Some((mut lock, mut surface)) = self.lock.take() else {
            return;
        };
        let full = surface.bounds();
        // Обои основного монитора, в координатах самой картинки.
        if theme::is_mobile() {
            self.draw_background(&mut surface, full, 0);
        } else if !super::wallpaper::paint_lock(&mut surface) {
            // Свой фон экрана блокировки (24.11) не выбран или ещё готовится.
            self.draw_wall_on(&mut surface, full, full, 0);
        }
        lock.draw(&mut surface, self.scale);
        self.lock = Some((lock, surface));
        self.repaint_all();
    }

    #[must_use]
    pub fn locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Часы на экране блокировки устарели — перерисовать.
    pub fn refresh_lock_clock(&mut self) {
        let stale = self.lock.as_ref().is_some_and(|(lock, _)| lock.shown_clock != crate::time::clock_text().unwrap_or_default());
        if stale {
            self.redraw_lock();
        }
    }

    /// Клавиша на экране блокировки.
    pub fn lock_key(&mut self, ch: Option<char>, enter: bool, backspace: bool, escape: bool) -> super::lock::Outcome {
        let Some((lock, _)) = self.lock.as_mut() else {
            return super::lock::Outcome::Nothing;
        };
        let outcome = lock.key(ch, enter, backspace, escape);
        self.after_lock(outcome)
    }

    /// Щелчок на экране блокировки.
    pub fn lock_click(&mut self, x: i32, y: i32) -> super::lock::Outcome {
        let Some((lock, _)) = self.lock.as_mut() else {
            return super::lock::Outcome::Nothing;
        };
        let outcome = lock.click(x, y);
        self.after_lock(outcome)
    }

    fn after_lock(&mut self, outcome: super::lock::Outcome) -> super::lock::Outcome {
        match outcome {
            super::lock::Outcome::Redraw => self.redraw_lock(),
            super::lock::Outcome::Unlock => {
                self.lock = None;
                super::wallpaper::lock_done();
                self.repaint_all();
            }
            _ => {}
        }
        outcome
    }

    /// Show the «Завершение работы» / «Перезагрузка» screen (24.11) on every
    /// monitor; the pointer hides.
    pub fn begin_ending(&mut self, restart: bool) {
        if self.ending.is_some() {
            return;
        }
        self.ending = Some(super::ending::Screen::new(restart, self.scale));
        if self.hw_cursor {
            let (x, y) = self.pointer.position();
            crate::display::cursor_visible(false, x, y);
        }
        self.repaint_all();
    }

    /// Next spinner frame of the ending screen (and a new note under it).
    pub fn ending_tick(&mut self, note: Option<&str>) {
        let outputs: Vec<Rect> = self.outputs.iter().map(|out| out.rect).collect();
        let Some(screen) = self.ending.as_mut() else { return };
        screen.tick(note);
        let parts: Vec<Rect> = screen.damage(&outputs).collect();
        for part in parts {
            self.mark(part);
        }
    }

    #[must_use]
    pub const fn ending(&self) -> bool {
        self.ending.is_some()
    }

    /// Take the ending screen away (the machine refused to go down).
    pub fn cancel_ending(&mut self) {
        if self.ending.take().is_some() {
            if self.hw_cursor {
                let (x, y) = self.pointer.position();
                crate::display::cursor_visible(true, x, y);
            }
            self.repaint_all();
        }
    }

    /// Screensaver (22.11, `saver`): show `shown` (`None` - take it away);
    /// the pointer hides while it is up. Returns the one shown before.
    pub fn saver_set(&mut self, shown: Option<super::saver::Shown>) -> Option<super::saver::Shown> {
        let old = core::mem::replace(&mut self.saver, shown);
        if self.hw_cursor && !self.blanked {
            let (x, y) = self.pointer.position();
            crate::display::cursor_visible(self.saver.is_none(), x, y);
        }
        self.repaint_all();
        old
    }

    pub fn saver_mut(&mut self) -> Option<&mut super::saver::Shown> {
        self.saver.as_mut()
    }

    #[must_use]
    pub const fn saver_showing(&self) -> bool {
        self.saver.is_some()
    }

    /// The screensaver changed `rect` (its clock moved).
    pub fn saver_damage(&mut self, rect: Rect) {
        self.mark(rect);
    }

    /// Where a notification goes (22.11, `capture`): the primary monitor and
    /// the top of its panel.
    #[must_use]
    pub fn toast_place(&self) -> (Rect, i32) {
        (self.primary, self.panel.as_ref().map_or(self.primary.bottom(), |panel| panel.home().y))
    }

    /// Show a notification built outside (22.11, «Запись сохранена»).
    pub fn show_toast(&mut self, mut toast: super::toast::Toast) {
        let placed = self.beside_panel(toast.rect, toast.rect);
        toast.move_to(placed);
        self.close_toast();
        self.mark_layer(toast.rect);
        self.toast = Some(toast);
    }

    /// Убрать уведомление; вернуть, что оно запускало бы.
    pub fn close_toast(&mut self) -> Option<(alloc::string::String, fpk::Rights)> {
        let toast = self.toast.take()?;
        self.mark_layer(toast.rect);
        Some((toast.start, toast.rights))
    }

    /// «Определить» (24.11, like Windows): a big number plate in the lower
    /// left corner of every monitor for `ms`. `numbers` — `(backend, number,
    /// name)` of each monitor in the registry: the number is the one
    /// «Параметры → Дисплей» and OBS show, the name tells two monitors of
    /// the same model apart. `false` — no monitor to show it on.
    pub fn start_identify(&mut self, numbers: &[(crate::display::outputs::Backend, u32, alloc::string::String)], ms: u64) -> bool {
        self.close_identify();
        let ctx = mini_ui::paint::Ctx::scaled(self.scale);
        let mut plates = Vec::new();
        let mut lines = Vec::new();
        let outputs: Vec<(Rect, crate::display::outputs::Backend)> = self.outputs.iter().map(|out| (out.rect, out.backend)).collect();
        for (index, (mon, backend)) in outputs.into_iter().enumerate() {
            let (number, name) = match numbers.iter().find(|(b, ..)| *b == backend) {
                Some((_, number, name)) => (*number, name.clone()),
                None => (index as u32 + 1, alloc::format!("{backend:?}")),
            };
            let digits = alloc::format!("{number}");
            let size = (mon.h / 6).clamp(ctx.px(80), ctx.px(220));
            let face = mini_ui::typeface::display_face(size);
            let caption = alloc::format!("{name} · {}×{}", mon.w, mon.h);
            let cap_face = ctx.face(mini_ui::typeface::Role::Body);
            let pad = ctx.px(28);
            let w = face.width(&digits).max(cap_face.width(&caption)).max(size) + 2 * pad;
            let cap_h = u32::from(cap_face.line);
            let h = u32::from(face.line) + cap_h + ctx.px(6) + 2 * ctx.px(18);
            let work = self.work_rect_of(index);
            let margin = ctx.px(40) as i32;
            let x = mon.x + margin;
            let y = (work.bottom().min(mon.bottom()) - margin - h as i32).max(mon.y);
            let Some(mut surface) = Surface::new(w, h, Color::rgb(0x1B, 0x1E, 0x25)) else {
                continue;
            };
            // An accent strip on the left, the number white, the name grey.
            let accent = ctx.palette.acc;
            surface.fill(Rect::new(0, 0, ctx.px(6), h), accent);
            let white = Color::rgb(0xFF, 0xFF, 0xFF);
            let nx = (w as i32 - face.width(&digits) as i32) / 2;
            mini_ui::typeface::draw(&mut surface, face, nx, ctx.px(18) as i32, &digits, white, 255);
            let cx = (w as i32 - cap_face.width(&caption) as i32) / 2;
            let band = Rect::new(0, (ctx.px(18) + u32::from(face.line) + ctx.px(6)) as i32, w, cap_h);
            mini_ui::paint::text(ctx, &mut surface, mini_ui::typeface::Role::Body, cx, mini_ui::paint::baseline(ctx, mini_ui::typeface::Role::Body, band), &caption, Color::rgb(0xC8, 0xCC, 0xD4));
            let rect = Rect::new(x, y, w, h);
            self.mark_layer(rect);
            lines.push(alloc::format!("{number} '{name}' {}x{} at {},{}", mon.w, mon.h, mon.x, mon.y));
            plates.push((rect, surface));
        }
        if plates.is_empty() {
            return false;
        }
        crate::kprintln!("  desktop     : identify monitors for {} ms: {}", ms, lines.join("; "));
        self.identify = Some(Identify { until_ms: crate::time::uptime_ms() + ms, plates });
        true
    }

    /// Take the number plates off the monitors.
    pub fn close_identify(&mut self) {
        if let Some(identify) = self.identify.take() {
            for (rect, _) in identify.plates {
                self.mark_layer(rect);
            }
        }
    }

    /// The number plates are gone when their time is up.
    pub fn expire_identify(&mut self) {
        if self.identify.as_ref().is_some_and(|identify| crate::time::uptime_ms() >= identify.until_ms) {
            self.close_identify();
            crate::kprintln!("  desktop     : identify plates gone");
        }
    }

    /// Убрать уведомление, если вышел его срок.
    pub fn expire_toast(&mut self) {
        if self.toast.as_ref().is_some_and(|toast| crate::time::uptime_ms() >= toast.until_ms) {
            self.close_toast();
            crate::kprintln!("  desktop     : notification expired");
        }
    }

    #[must_use]
    /// Кадр заставки запуска (21.11): карточка посреди основного монитора,
    /// полоса бежит. `true` — анимация идёт.
    fn advance_splash(&mut self) -> bool {
        let area = self.primary_work();
        let (damage, moving) = self.splash.advance(area, self.scale);
        for rect in damage {
            self.mark_layer(rect);
        }
        moving
    }

    /// Что под точкой на заставке запуска.
    #[must_use]
    pub fn splash_hit(&self, x: i32, y: i32) -> Option<super::launch::Hit> {
        self.splash.hit(x, y)
    }

    /// Закрыть заставку запуска (крестик, «Закрыть»).
    pub fn splash_dismiss(&mut self) {
        let rect = self.splash.rect;
        self.splash.dismiss();
        self.mark_layer(rect);
    }

    pub fn toast_hit(&self, x: i32, y: i32) -> Option<super::toast::Hit> {
        self.toast.as_ref().and_then(|toast| toast.hit(x, y))
    }

    pub fn reload_icons(&mut self) {
        self.invalidate_under();
        let before = self.icons.bounds();
        self.icons.reload();
        let after = self.icons.bounds();
        self.mark_layer(before.union(&after));
    }

    /// Попадание в панель задач.
    #[must_use]
    pub fn panel_at(&self, x: i32, y: i32) -> Option<PanelHit> {
        // Под открытой клавиатурой дока нет — он спрятан, и нажимать его нельзя.
        // И под программой на телефоне: вместо дока там полоска жеста (04.11).
        if self.keyboard_visible() || self.app_in_front() || self.fullscreen_front() {
            return None;
        }
        if let Some(panel) = self.panel.as_ref().filter(|panel| panel.rect.contains(x, y) && panel.area().contains(x, y)) {
            return panel.hit(x, y);
        }
        // Панели остальных мониторов (20.11).
        let (_, panel) = self.side_panels.iter().find(|(_, panel)| panel.rect.contains(x, y) && panel.area().contains(x, y))?;
        if self.fullscreen_over(panel.rect) {
            return None;
        }
        panel.hit(x, y)
    }

    /// Начать или закончить перетаскивание окна.
    pub fn set_drag(&mut self, app: Option<App>) {
        // Контур прежнего захвата — стереть (отпускание уже перенесло окно в
        // `settle_drag`; здесь — отмена).
        self.set_outline(None);
        self.drag = app;
        self.snap_top_since = 0;
        self.snap_hover = None;
        if self.snap_pop.as_ref().is_some_and(|pop| pop.dragging) || app.is_some() {
            self.close_snap_layouts();
        }
        if let Some(app) = app {
            self.drag_press = self.pointer.position();
            self.drag_unmaximized = false;
            self.drag_from = self.rect_of(app).map_or((0, 0), |rect| (rect.x, rect.y));
        } else {
            self.drag_resizes = false;
        }
        if let Some((_, preview)) = self.snap.take() {
            self.mark_layer(preview);
        }
    }

    /// Переехало ли окно с тех пор, как его взяли.
    #[must_use]
    pub fn drag_moved(&self) -> bool {
        match self.drag.and_then(|app| self.rect_of(app)) {
            Some(rect) => (rect.x, rect.y) != self.drag_from,
            None => false,
        }
    }

    /// Начать изменение размера окна за края `edges`: тот же захват, другое
    /// действие.
    pub fn set_resize_drag(&mut self, app: App, edges: u8) {
        self.set_outline(None);
        self.drag = Some(app);
        self.drag_from = self.rect_of(app).map_or((0, 0), |rect| (rect.x, rect.y));
        self.drag_resizes = true;
        self.resize_edges = if edges == 0 { super::window::EDGE_RIGHT | super::window::EDGE_BOTTOM } else { edges };
    }

    /// За какие края тянут окно сейчас.
    #[must_use]
    pub const fn resize_edges(&self) -> u8 {
        self.resize_edges
    }

    /// Тянут ли окно за уголок (а не переносят).
    #[must_use]
    pub const fn resizing(&self) -> bool {
        self.drag.is_some() && self.drag_resizes
    }

    #[must_use]
    pub const fn dragging(&self) -> Option<App> {
        self.drag
    }

    /// Развёрнутое окно, взятое за заголовок, уже уменьшилось (20.11).
    #[must_use]
    pub const fn drag_unmaximized(&self) -> bool {
        self.drag_unmaximized
    }

    /// Захват меняет размер окна, а не двигает его.
    #[must_use]
    pub const fn drag_resizes(&self) -> bool {
        self.drag_resizes
    }

    /// Сдвинуть окно, которое тащат, — или изменить его размер.
    pub fn drag_by(&mut self, dx: i32, dy: i32) {
        let Some(app) = self.drag else {
            return;
        };
        let Some(index) = self.index_of(app) else {
            // Окно закрыли, не отпустив кнопку. Такое бывает, и тащить дальше
            // нечего.
            self.drag = None;
            self.drag_resizes = false;
            return;
        };
        let (areas, n) = self.work_rects();
        if self.drag_resizes {
            // Размер не меняется здесь, а запоминается: см. [`Self::pending_size`]
            // и [`Self::apply_pending_size`].
            let Some(before) = self.windows.get(index).map(|window| window.rect) else {
                return;
            };
            let (mut x, mut y, mut w, mut h) = match self.pending_size {
                Some((pending, x, y, w, h)) if pending == app => (x, y, w, h),
                _ => (before.x, before.y, before.w as i32, before.h as i32),
            };
            let edges = self.resize_edges;
            if edges & super::window::EDGE_LEFT != 0 {
                x += dx;
                w -= dx;
            } else if edges & super::window::EDGE_RIGHT != 0 {
                w += dx;
            }
            if edges & super::window::EDGE_TOP != 0 {
                y += dy;
                h -= dy;
            } else if edges & super::window::EDGE_BOTTOM != 0 {
                h += dy;
            }
            self.pending_size = Some((app, x, y, w, h));
            return;
        }
        // Развёрнутое окно, которое потащили, возвращается к прежнему
        // размеру под указателем (20.11, как в Windows), а не едет
        // развёрнутым. Во весь экран — не тащится вовсе.
        if self.windows.get(index).is_some_and(|window| window.fullscreen) {
            return;
        }
        // Так же прилепленное к половине монитора (20.11).
        if self.windows.get(index).is_some_and(|window| window.is_maximized() || window.is_snapped()) {
            self.unmaximize_dragged(index, app, &areas[..n]);
            self.update_snap();
            return;
        }
        // «Показывать контур при перетаскивании» (20.11): едет рамка, окно
        // переедет при отпускании ([`Self::settle_drag`]).
        if super::look::outline_drag() {
            if let Some(rect) = self.windows.get(index).map(|window| window.rect) {
                let ghost = self.outline.unwrap_or(rect).translate(dx, dy);
                self.set_outline(Some(ghost));
            }
            self.update_snap();
            return;
        }
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        let before = window.rect;
        window.move_within(dx, dy, &areas[..n]);
        let after = window.rect;
        if after != before {
            self.note_moved(app, before, after);
            self.follow_owner(app);
        }
        // Окно упирается в край рабочей области раньше указателя — к краю
        // экрана подходит указатель, и прилипание считается по нему.
        self.update_snap();
    }

    /// Указатель с тащимым окном у края экрана (20.11, как Snap в Windows):
    /// слева и справа — половина рабочей области этого монитора, сверху —
    /// развернуть. С 20.11 — у каждого края каждого монитора, и у верхнего
    /// края, если окно там подержать, открываются макеты прилипания.
    fn update_snap(&mut self) {
        let (x, y) = self.pointer.position();
        // Макеты открыты перетаскиванием: зона под указателем — место окна.
        if self.snap_pop.as_ref().is_some_and(|pop| pop.dragging) && self.drag.is_some() {
            let scale = self.scale;
            let keep = self.snap_pop.as_ref().is_some_and(|pop| pop.keeps(x, y, scale) || y <= pop.rect.y);
            if keep {
                let changed = self.snap_pop.as_mut().is_some_and(|pop| pop.hover(x, y));
                let zone = self.snap_pop.as_ref().and_then(|pop| pop.hot.and_then(|(l, c)| pop.target(l, c)));
                if changed {
                    if let Some(rect) = self.snap_pop.as_ref().map(|pop| pop.rect) {
                        self.mark(rect);
                    }
                }
                let next = match zone {
                    Some(rect) => Some((Snap::Layout, rect)),
                    None => self.snap_zone(x, y),
                };
                self.set_snap(next);
                return;
            }
            self.close_snap_layouts();
        }
        // A window of one size (24.11, WM SDK) does not snap, as in Windows.
        let snaps = self.drag.and_then(|app| self.index_of(app)).and_then(|index| self.windows.get(index)).is_some_and(|window| window.can_maximize());
        let next = if theme::is_mobile() || self.drag_resizes || self.drag.is_none() || !snaps {
            None
        } else {
            self.snap_zone(x, y)
        };
        // Держат у верхнего края — отсчёт до макетов ([`Self::advance_snap_layouts`]).
        if matches!(next, Some((Snap::Top, _))) {
            if self.snap_top_since == 0 {
                self.snap_top_since = crate::time::uptime_ms().max(1);
                super::set_animating(true);
            }
        } else {
            self.snap_top_since = 0;
        }
        self.set_snap(next);
    }

    /// Сменить показанное место прилипания.
    fn set_snap(&mut self, next: Option<(Snap, Rect)>) {
        if next == self.snap {
            return;
        }
        if let Some((_, old)) = self.snap {
            self.mark_layer(old);
        }
        if let Some((zone, rect)) = next {
            self.mark_layer(rect);
            crate::kprintln!("  desktop     : snap preview {} at {},{} {}x{}", zone.name(), rect.x, rect.y, rect.w, rect.h);
        }
        self.snap = next;
    }

    /// Куда прилипнет окно, если указатель у края `x, y`, и место для него.
    fn snap_zone(&self, x: i32, y: i32) -> Option<(Snap, Rect)> {
        let index = self.output_near(x, y);
        let monitor = *self.output_rects().get(index)?;
        // У каждого края каждого монитора, в том числе на стыке двух (20.11,
        // как в Windows): монитор — тот, что под указателем.
        let edge = (2u32 * self.scale) as i32;
        let zone = if x < monitor.x + edge {
            Snap::Left
        } else if x >= monitor.right() - edge {
            Snap::Right
        } else if y < monitor.y + edge {
            Snap::Top
        } else {
            return None;
        };
        let area = self.work_rect_of(index);
        if area.is_empty() {
            return None;
        }
        let half = area.w / 2;
        let rect = match zone {
            Snap::Top | Snap::Layout => area,
            Snap::Left => Rect::new(area.x, area.y, half, area.h),
            Snap::Right => Rect::new(area.x + half as i32, area.y, area.w - half, area.h),
        };
        Some((zone, rect))
    }

    /// Окно, которое тащили, отпустили (20.11): у края экрана оно прилипает
    /// к половине монитора или разворачивается. Ответ — куда прилипло.
    pub fn drop_snap(&mut self, app: App) -> Option<Snap> {
        self.snap_top_since = 0;
        if self.snap_pop.as_ref().is_some_and(|pop| pop.dragging) {
            self.close_snap_layouts();
        }
        let (zone, target) = self.snap.take()?;
        self.mark_layer(target);
        let (from_x, from_y) = self.drag_from;
        let before = self.rect_of(app)?;
        // Вернётся окно к размеру, с которым его взяли, и туда, откуда взяли.
        let previous = Rect::new(from_x, from_y, before.w, before.h);
        self.snap_window_to(app, target, previous, zone == Snap::Top).then_some(zone)
    }

    /// Поставить окно в `target` (половина, зона макета или весь монитор —
    /// `maximize`); `previous` — куда оно вернётся. Перетекает анимацией.
    fn snap_window_to(&mut self, app: App, target: Rect, previous: Rect, maximize: bool) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let focused = index == self.focus;
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        let before = window.rect;
        if !window.can_maximize() || !window.snap_to(target, previous, maximize) {
            return false;
        }
        window.draw_decorations(focused);
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
        self.follow_owner(app);
        self.finish_flight();
        if self.panel.is_some() && !before.is_empty() && !after.is_empty() && super::animations_allowed() && super::look::anim(super::look::Flag::MinMax) {
            self.flight = Some(Flight::morph(app, after, before, crate::time::uptime_ns()));
            super::set_animating(true);
        }
        true
    }

    /// Открыть макеты прилипания для окна `app` (20.11): `button` — кнопка
    /// «Развернуть», под которой они встают; `None` — окно держат у верхнего
    /// края, и они встают вверху монитора под указателем.
    fn open_snap_layouts(&mut self, app: App, button: Option<Rect>) {
        let Some(rect) = self.rect_of(app) else {
            return;
        };
        let (x, y) = self.pointer.position();
        let output = self.output_near(x, y);
        let monitor = self.output_rects().get(output).copied().unwrap_or(self.primary);
        let area = self.work_rect_of(output);
        let gap = (8 * self.scale) as i32;
        let pop = match button {
            Some(button) => super::snaplay::Popup::new(app, button.x + button.w as i32 / 2, button.bottom() + gap / 2, monitor, area, false, button, self.scale),
            None => super::snaplay::Popup::new(app, monitor.x + monitor.w as i32 / 2, area.y + gap, monitor, area, true, Rect::EMPTY, self.scale),
        };
        let _ = rect;
        crate::kprintln!(
            "  desktop     : snap layouts opened ({}) at {},{} {}x{}",
            if pop.dragging { "drag" } else { "button" },
            pop.rect.x,
            pop.rect.y,
            pop.rect.w,
            pop.rect.h
        );
        for spot in pop.spots() {
            crate::kprintln!("  desktop     : snap spot {spot}");
        }
        self.mark_layer(pop.rect);
        self.snap_pop = Some(pop);
    }

    /// Закрыть макеты прилипания.
    pub fn close_snap_layouts(&mut self) {
        if let Some(pop) = self.snap_pop.take() {
            self.mark_layer(pop.rect);
            crate::kprintln!("  desktop     : snap layouts closed");
        }
    }

    /// Открыты ли макеты, вызванные кнопкой «Развернуть».
    #[must_use]
    pub fn snap_layouts_open(&self) -> bool {
        self.snap_pop.as_ref().is_some_and(|pop| !pop.dragging)
    }

    /// Указатель сдвинулся при макетах от кнопки: подсветка зоны; ушёл и с
    /// плашки, и с кнопки — закрыть.
    pub fn snap_layouts_hover(&mut self, x: i32, y: i32) {
        let scale = self.scale;
        let Some(keeps) = self.snap_pop.as_ref().filter(|pop| !pop.dragging).map(|pop| pop.keeps(x, y, scale)) else {
            return;
        };
        if !keeps {
            self.close_snap_layouts();
            return;
        }
        let changed = self.snap_pop.as_mut().map(|pop| (pop.hover(x, y), pop.rect));
        if let Some((true, rect)) = changed {
            self.mark(rect);
        }
    }

    /// Щелчок при макетах от кнопки. `Some((окно, раскладка, зона))` — окно
    /// поставлено в зону; `None` — щелчок мимо, макеты закрыты.
    pub fn snap_layouts_press(&mut self, x: i32, y: i32) -> Option<(App, usize, usize)> {
        let pop = self.snap_pop.as_ref().filter(|pop| !pop.dragging)?;
        let hit = pop.zone_at(x, y).and_then(|(l, c)| pop.target(l, c).map(|rect| (pop.app, l, c, rect)));
        let inside = pop.rect.contains(x, y);
        if hit.is_none() && inside {
            return None;
        }
        self.close_snap_layouts();
        let (app, layout, cell, target) = hit?;
        let previous = self.windows.iter().find(|window| window.app == app).map(|window| window.drag_restore_rect().unwrap_or(window.rect))?;
        self.snap_window_to(app, target, previous, false).then_some((app, layout, cell))
    }

    /// Щелчок пришёлся в плашку макетов (мимо зон) — его съесть.
    #[must_use]
    pub fn snap_layouts_contains(&self, x: i32, y: i32) -> bool {
        self.snap_pop.as_ref().is_some_and(|pop| pop.rect.contains(x, y))
    }

    /// Такт макетов перед кадром: окно держат у верхнего края дольше
    /// [`SNAP_HOLD_MS`] — открыть их; указатель стоит на «Развернуть» дольше
    /// [`SNAP_HOVER_MS`] — тоже. `true` — ждём, нужны кадры.
    fn advance_snap_layouts(&mut self) -> bool {
        let now = crate::time::uptime_ms();
        if self.snap_pop.is_none() {
            if let Some(app) = self.drag.filter(|_| self.snap_top_since != 0 && !self.drag_resizes) {
                if now.saturating_sub(self.snap_top_since) >= SNAP_HOLD_MS {
                    self.snap_top_since = 0;
                    self.open_snap_layouts(app, None);
                    return false;
                }
                return true;
            }
            if let Some((app, button, since)) = self.snap_hover.filter(|_| self.drag.is_none()) {
                if now.saturating_sub(since) >= SNAP_HOVER_MS {
                    self.snap_hover = None;
                    self.open_snap_layouts(app, Some(button));
                    return false;
                }
                return true;
            }
        }
        false
    }

    /// Нарисовать место, куда прилипнет окно: полупрозрачная плашка цвета
    /// акцента с обводкой, на поле от краёв — как у Windows 11.
    fn draw_snap(&self, back: &mut Surface, band: Rect, dy: i32, rect: Rect) {
        let ctx = mini_ui::paint::Ctx::scaled(self.scale);
        let shown = rect.shrink(ctx.px(8));
        if shown.intersect(&band).is_empty() {
            return;
        }
        let p = theme::palette();
        let radius = ctx.px(theme::R_WINDOW);
        let at = shown.translate(0, dy);
        draw::rounded(back, at, radius, p.acc, 64);
        draw::rounded_stroke(back, at, radius, p.acc, 190);
    }

    /// Развёрнутое окно тащат за заголовок: как только указатель отошёл от
    /// точки нажатия дальше порога (дрожь руки при щелчке — не перенос),
    /// окно принимает прежний размер. Указатель остаётся на той же доле
    /// ширины заголовка и на той же высоте в нём — окно уменьшается «вокруг»
    /// руки, а не отпрыгивает из-под неё.
    fn unmaximize_dragged(&mut self, index: usize, app: App, areas: &[Rect]) {
        let (px, py) = self.pointer.position();
        let (ax, ay) = self.drag_press;
        let threshold = 4 * self.scale.pct() as i32 / 100;
        if (px - ax).abs() < threshold && (py - ay).abs() < threshold {
            return;
        }
        let focused = self.focus == index;
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        let before = window.rect;
        let Some(back) = window.drag_restore_rect() else {
            return;
        };
        let what = if window.is_snapped() { "snapped" } else { "maximized" };
        let along = i64::from((ax - before.x).clamp(0, before.w as i32));
        let x = ax - (along * i64::from(back.w) / i64::from(before.w.max(1))) as i32;
        let y = before.y;
        if !window.unmaximize_at(x, y, focused) {
            return;
        }
        // Дальше окно идёт за указателем от этого места: то, на что он
        // успел отойти от точки нажатия, — уже сдвиг.
        window.move_within(px - ax, py - ay, areas);
        crate::kprintln!("  desktop     : restored '{}' from {what} by dragging", window.caption());
        self.drag_unmaximized = true;
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
        self.follow_owner(app);
    }

    /// Свернуть окно: убрать с экрана, оставив в панели задач.
    pub fn minimize(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        if window.minimized {
            return false;
        }
        window.minimized = true;
        window.minimized_at_ms = crate::time::uptime_ms();
        // Карточка «Последнее» под окнами перечисляет свёрнутые, но картинка
        // сбрасывается при посадке (см. `finish_flight`): полёт идёт поверх
        // готовой, и треть секунды карточка показывает прежний список.
        let rect = window.rect;
        self.mark_layer(rect);
        // Куда летит — для стенда (20.11): кнопка на панели своего монитора.
        if let Some(button) = self.button_of(app) {
            let output = self.output_near(button.x + button.w as i32 / 2, button.y + button.h as i32 / 2);
            crate::kprintln!("  desktop     : minimizing '{}' to its button on output {output}", app.title());
        }
        self.start_flight(app, rect, false);
        // Модальное подчинённое (20.11, «Сохранить как») уходит вместе с
        // владельцем и вместе с ним вернётся (см. [`Self::restore`]).
        let mut owned = Vec::new();
        for other in self.windows.iter_mut().filter(|other| other.owner == Some(app) && other.modal && !other.minimized) {
            other.minimized = true;
            other.minimized_at_ms = crate::time::uptime_ms();
            owned.push(other.rect);
        }
        for rect in owned {
            self.mark_layer(rect);
        }
        // Фокус переезжает на **верхнее из оставшихся видимых**. Прежде здесь
        // стоял `focus_next`, а тот поднимает нулевое окно, не спрашивая, не
        // свёрнуто ли оно: свернув последнее видимое, система делала активным
        // соседнее свёрнутое. Ввод после этого уходил в никуда.
        //
        // Если видимых не осталось, фокус остаётся на этом окне, и это верно:
        // [`Compositor::focused_app`] назовёт его неактивным, то есть ввод
        // достанется оболочке — единственному месту, где команду ещё можно
        // набрать.
        if self.focus == index || self.windows.get(self.focus).is_some_and(|window| window.minimized) {
            if let Some(next) = self.windows.iter().rposition(|window| !window.minimized) {
                self.raise(next);
            }
        }
        true
    }

    /// Вернуть свёрнутое окно на экран.
    pub fn restore(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        if !window.minimized {
            return false;
        }
        // Поверхность цела, но экран под окном за это время перерисовали —
        // значит показать надо всё окно целиком, а не то, что в нём изменилось.
        window.damage = Rect::new(0, 0, window.rect.w, window.rect.h);
        let rect = window.rect;
        // Модальное подчинённое, свёрнутое вместе с владельцем (20.11), —
        // обратно сразу: полёт показывает только владельца.
        let mut owned = Vec::new();
        for other in self.windows.iter_mut().filter(|other| other.owner == Some(app) && other.modal && other.minimized) {
            other.minimized = false;
            other.damage = Rect::new(0, 0, other.rect.w, other.rect.h);
            owned.push(other.rect);
        }
        for rect in owned {
            self.mark_layer(rect);
        }
        // Окно возвращается полётом из стопки. Свёрнутым оно остаётся до конца
        // полёта: иначе на экране было бы сразу два окна — готовое на месте и
        // летящее к нему.
        if self.start_flight(app, rect, true) {
            return true;
        }
        if let Some(window) = self.windows.get_mut(index) {
            window.minimized = false;
        }
        self.invalidate_recent();
        // Вернувшееся окно приносит с собой и тень: без неё вокруг него
        // осталась бы светлая рамка от того, что лежало здесь, пока оно было
        // свёрнуто.
        self.mark_layer(rect);
        true
    }

    /// Свёрнуто ли окно этой программы.
    #[must_use]
    pub fn is_minimized(&self, app: App) -> bool {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .is_some_and(|window| window.minimized)
    }

    /// Развернуть окно на всю рабочую область или вернуть прежний размер.
    pub fn toggle_maximize(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        // Окно во весь экран не разворачивают и не возвращают: его размер —
        // монитор, пока программа сама не выйдет из этого режима.
        if self.windows.get(index).is_some_and(|window| window.fullscreen) {
            return false;
        }
        // «Развернуть» is greyed out for a window that cannot change size
        // (24.11): neither the button nor Win+Up nor a client request.
        if self.windows.get(index).is_some_and(|window| !window.is_maximized() && !window.can_maximize()) {
            return false;
        }
        let mut area = match self.windows.get(index) {
            Some(window) => self.app_area_for(window.rect),
            None => self.app_area(),
        };
        // Терминал под открытой клавиатурой телефона — до неё: развёрнутый
        // иначе спрятал бы под ней приглашение (клавиатура ужимает его только
        // при появлении и уходе, см. [`Self::sync_keyboard`]).
        // Так же окно программы, которому клавиатуру открыли кнопкой (05.11).
        let lifted = self.windows.get(index).is_some_and(|window| window.keyboard);
        if theme::is_mobile() && (app.is_terminal() || lifted) {
            if let Some(keyboard) = self.keyboard.as_ref().filter(|keyboard| keyboard.is_visible()) {
                let bottom = keyboard.rect.y.saturating_sub(mini_ui::paint::Ctx::scaled(self.scale).px(keyboard::GAP) as i32);
                area.h = bottom.saturating_sub(area.y).max(1) as u32;
            }
        }
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        let before = window.rect;
        if !window.toggle_maximize(area) {
            return false;
        }
        // Поверхность пересобрана неактивной (`Window::resize`): заголовок
        // — по настоящему фокусу.
        window.draw_decorations(index == self.focus);
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
        self.follow_owner(app);
        // Окно перетекает в новый размер. Поверхность уже нового размера и
        // готова: `Window::resize` перерисовал её.
        self.finish_flight();
        if self.panel.is_some() && !before.is_empty() && !after.is_empty() && super::animations_allowed() && super::look::anim(super::look::Flag::MinMax) {
            self.flight = Some(Flight::morph(app, after, before, crate::time::uptime_ns()));
            super::set_animating(true);
        }
        true
    }

    /// Открыть окно во весь монитор, на котором оно стоит, или вернуть
    /// (20.11, `xdg_toplevel.set_fullscreen` — игры под Wine). `true` —
    /// состояние изменилось.
    pub fn set_fullscreen(&mut self, app: App, on: bool) -> bool {
        self.set_fullscreen_at(app, on, None)
    }

    /// G5 (22.11): [`Self::set_fullscreen`] on the monitor under the desktop
    /// point `at` (Wayland `set_fullscreen(output)`: Wine names the monitor
    /// the game chose); `None` - the monitor under the window.
    pub fn set_fullscreen_at(&mut self, app: App, on: bool, at: Option<(i32, i32)>) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let Some(rect) = self.windows.get(index).map(|window| window.rect) else {
            return false;
        };
        let (px, py) = at.unwrap_or((rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2));
        let monitor = self.output_near(px, py);
        let area = self.output_rects().get(monitor).copied().unwrap_or(self.primary);
        let focused = index == self.focus;
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        let before = window.rect;
        let changed = if on { window.enter_fullscreen(area) } else { window.leave_fullscreen() };
        if !changed {
            return false;
        }
        window.draw_decorations(focused);
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
        let rects: Vec<Rect> = self.all_panels().map(|panel| panel.rect).collect();
        for rect in rects {
            self.mark_layer(rect);
        }
        self.follow_owner(app);
        true
    }

    /// Впереди всех окон — окно во весь экран, закрывающее панель: панель
    /// тогда не рисуется и щелчки не ловит (20.11). Открытый «Пуск»
    /// возвращает её — Win из игры ведёт к панели, как в Windows; Alt+Tab
    /// поднимает другое окно, и окно во весь экран перестаёт быть впереди.
    #[must_use]
    pub fn fullscreen_front(&self) -> bool {
        if self.start.is_some() || self.menu_open() {
            return false;
        }
        let Some(panel) = self.panel.as_ref() else {
            return false;
        };
        self.windows
            .iter()
            .rev()
            .find(|window| !window.minimized && !window.popup)
            .is_some_and(|window| window.fullscreen && !window.rect.intersect(&panel.rect).is_empty())
    }

    /// То же для панели второго монитора: впереди на её мониторе — окно во
    /// весь экран (20.11).
    fn fullscreen_over(&self, rect: Rect) -> bool {
        if self.start.is_some() || self.menu_open() {
            return false;
        }
        self.windows
            .iter()
            .rev()
            .filter(|window| !window.minimized && !window.popup)
            .find(|window| !window.rect.intersect(&rect).is_empty())
            .is_some_and(|window| window.fullscreen)
    }

    /// Закрыть окно программы. Возвращает `true`, если оно было.
    pub fn close(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        if dims(app) {
            self.repaint_all();
        }
        let window = self.windows.remove(index);
        Self::forget_terminal(&window);
        self.invalidate_recent();
        // Окно ниже активного уехало из списка — номер активного съехал на одно.
        // Раньше закрывали почти всегда верхнее, активное, окно; всплывающие
        // (фаза 62e) закрываются и тогда, когда активна форма под ними.
        if index < self.focus {
            self.focus -= 1;
        }
        // Подчинённые закрытого окна остаются без владельца, а не с номером
        // окна, которого нет: следующее окно с тем же номером их бы «усыновило».
        for other in self.windows.iter_mut().filter(|other| other.owner == Some(app)) {
            // Модальное окно другой программы (20.11, окно выбора файла)
            // без владельца не нужно никому: его просят закрыться.
            let foreign = matches!((other.app, app), (App::Program(mine, _), App::Program(theirs, _)) if mine != theirs);
            if foreign && other.modal {
                other.push_event(user_abi::WinEvent { kind: user_abi::WIN_CLOSE, code: 0, x: 0, y: 0 });
            }
            other.owner = None;
            other.modal = false;
        }
        if self.focus >= self.windows.len() {
            self.focus = self.windows.len().saturating_sub(1);
        }
        self.refresh_decorations();
        self.mark_layer(window.rect);
        if self.drag == Some(app) {
            self.drag = None;
        }
        // Окно закрыли, не отпустив кнопку: отпускание сказать уже некому, а
        // захват, оставшийся за несуществующим окном, съел бы движения всех
        // остальных до следующего нажатия.
        if self.captured.is_some_and(|(held, _)| held == app) {
            self.captured = None;
        }
        // Видимое окно уходит в свою плитку. Свёрнутое — нет: его на экране и
        // так не было.
        if !window.minimized && self.start_zoom(app, window.rect, false) {
            self.closing = Some(window);
        }
        true
    }

    /// Перерисовать украшения всех окон по текущему фокусу.
    fn refresh_decorations(&mut self) {
        let focus = self.focus;
        for (index, window) in self.windows.iter_mut().enumerate() {
            window.draw_decorations(index == focus);
        }
    }

    // -----------------------------------------------------------------------
    // Панель и меню
    // -----------------------------------------------------------------------

    pub fn menu_mut(&mut self) -> Option<&mut Menu> {
        self.menu.as_mut()
    }

    /// Сколько строк в меню запуска.
    #[must_use]
    pub fn menu_items(&self) -> usize {
        self.menu.as_ref().map_or(0, Menu::item_count)
    }

    /// Сколько программ лежит в `/bin`.
    #[must_use]
    pub fn menu_bin_programs(&self) -> usize {
        self.menu.as_ref().map_or(0, Menu::bin_programs)
    }

    pub fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(Menu::is_open)
    }

    /// Пересобрать закрытое меню запуска (фаза N8): строки пакетов читаются из
    /// реестра, а пакет могли поставить после запуска стола.
    ///
    /// Нехватка памяти под новую карточку оставляет прежнюю — меню без нового
    /// пакета лучше, чем меню, которое не открылось.
    pub fn reload_menu(&mut self) {
        if self.menu_open() {
            return;
        }
        if let Some(menu) = self.new_menu() {
            self.menu = Some(menu);
        }
    }

    /// Обновить панель задач.
    /// Перерисовать панель, даже если её содержимое не изменилось по
    /// отпечатку (значок громкости, 02.11).
    pub fn repaint_panel(&mut self, status: &Status) {
        if let Some(panel) = self.panel.as_mut() {
            panel.forget();
        }
        for (_, panel) in &mut self.side_panels {
            panel.forget();
        }
        self.refresh_panel(status);
    }

    pub fn refresh_panel(&mut self, status: &Status) {
        // Карточка «Система» перерисовывается, только когда её числа сменились:
        // аптайм меняется раз в секунду, а просят обновиться на каждое касание.
        if theme::is_mobile() && self.home != status.home {
            self.home = status.home;
            let card = super::home::system_card(self.primary.w, self.scale);
            self.invalidate_under_rect(card);
            self.mark(card);
        }
        if theme::is_mobile() {
            // Отпечаток строки — часы и батарея (03.11): заряд и зарядка
            // меняются без смены минуты, и без этого значок оставался пустым
            // до следующей.
            let mut text = super::clock_or_uptime();
            if let Some(state) = crate::battery::get() {
                text.push_str(&alloc::format!(" {}{}", state.percent, if state.charger { "+" } else { "" }));
            }
            // И Wi-Fi: значок появляется, когда он включился.
            text.push_str(&alloc::format!(" w{}", crate::wifi::generation()));
            // И сотовая связь (05.11): полоски и тип сети.
            text.push_str(&alloc::format!(" c{}", crate::cellular::generation()));
            if text != self.status_text {
                self.status_text = text;
                let bar = super::statusbar::bounds(self.primary.w, self.scale);
                self.invalidate_under_rect(bar);
                self.mark(bar);
            }
        }
        // Настройки панели сменились («Параметры», 20.11) — пересобрать.
        let rebuilt = !theme::is_mobile() && self.panel.as_ref().is_some_and(|panel| panel.config() != super::deskcfg::taskbar());
        if rebuilt {
            self.rebuild_taskbar();
        }
        let menu_open = self.menu_open();
        // У каждой панели — кнопки окон своего монитора (20.11); панель
        // одна — все окна у неё.
        let primary = self.primary_index();
        let buttons = self.buttons_on(primary);
        let menu_here = menu_open && self.menu_output.is_none();
        if let Some(panel) = self.panel.as_mut() {
            panel.redraw(&buttons, menu_here, status);
        }
        let sides: Vec<(usize, super::panel::Windows)> = self.side_panels.iter().map(|(o, _)| (*o, self.buttons_on(*o))).collect();
        for (o, buttons) in sides {
            let menu_here = menu_open && self.menu_output == Some(o);
            if let Some((_, panel)) = self.side_panels.iter_mut().find(|(i, _)| *i == o) {
                panel.redraw(&buttons, menu_here, status);
            }
        }
        // Где теперь кнопки окон (20.11): стенд наводит на них указатель,
        // чтобы открыть миниатюры у панели сверху или сбоку.
        if rebuilt {
            for app in self.windows.iter().filter(|window| !window.popup).map(|window| window.app).collect::<Vec<App>>() {
                if let Some(button) = self.button_of(app) {
                    crate::kprintln!("  desktop     : button of '{}' at {},{} {}x{}", app.title(), button.x, button.y, button.w, button.h);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Сборка кадра
    // -----------------------------------------------------------------------

    fn mark(&mut self, rect: Rect) {
        if rect.is_empty() || self.damage_overflow {
            return;
        }
        if self.damage_count == MAX_DAMAGE {
            self.damage_overflow = true;
            // Переполнение — отдельный счётчик: «весь экран перерисован» имеет
            // две причины, и лечатся они по-разному. Двенадцать накопившихся
            // прямоугольников — это учёт, который не справляется; смена режима
            // или темы — это честная полная перерисовка, которой так и надо.
            super::note_overflow();
            return;
        }
        self.damage[self.damage_count] = rect;
        self.damage_count += 1;
    }

    /// Пометить изменившимся **слой целиком**: вместе с его тенью.
    ///
    /// Отдельно от [`Compositor::mark`], а не вместо него, из-за цены. Тень
    /// выходит за границы слоя, и уехавшее окно оставило бы за собой её след, —
    /// но расширять так каждый прямоугольник значило бы платить за это и на
    /// каждой напечатанной в терминале букве. Буква тени не отбрасывает; слой
    /// отбрасывает. Поэтому расширяются только те прямоугольники, которые
    /// описывают появление, исчезновение или переезд слоя.
    fn mark_layer(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        // Ореол вокруг слоя — место под его тень: стирая слой, стереть надо и
        // её.
        let halo = self.halo();
        self.mark(Rect::new(
            rect.x - halo as i32,
            rect.y - halo as i32,
            rect.w + halo * 2,
            rect.h + halo * 2,
        ));
    }

    /// Начать полёт окна. `false` — анимации не будет, и окно надо показать
    /// или спрятать сразу.
    ///
    /// Полёт один: второе сворачивание, пришедшее во время первого, сначала
    /// доводит первое до конца — два окна, летящие в одну стопку, человек не
    /// разглядит, а держать список ради этого незачем.
    fn start_flight(&mut self, app: App, rect: Rect, restore: bool) -> bool {
        self.finish_flight();
        if self.panel.is_none() || !super::animations_allowed() || !super::look::anim(super::look::Flag::MinMax) {
            return false;
        }
        self.flight = Some(Flight::new(app, rect, restore, crate::time::uptime_ns()));
        super::set_animating(true);
        true
    }

    /// Куда летит окно: стопка в доке телефона, кнопка окна на панели задач.
    fn flight_target(&self, flight: &Flight) -> Rect {
        if let Some(to) = flight.to {
            return to;
        }
        let app = flight.app;
        let fallback = Rect::new(self.primary.x + self.primary.w as i32 / 2, self.primary.bottom(), 1, 1);
        // Кнопка на панели монитора окна (20.11): окно левого монитора не
        // летит через весь стол на панель правого.
        self.button_of(app).unwrap_or(fallback)
    }

    /// Шаг полёта перед кадром: пометить, что стёрто и что нарисовано.
    fn advance_flight(&mut self) {
        let now = crate::time::uptime_ns();
        let Some(flight) = self.flight.as_ref() else {
            return;
        };
        let to = self.flight_target(flight);
        let current = flight.bounds(to, flight.phase(now));
        let last = flight.last;
        let done = flight.done(now);
        self.mark(last.union(&current));
        if let Some(flight) = self.flight.as_mut() {
            flight.last = current;
            flight.frames += 1;
        }
        if done {
            self.finish_flight();
        }
    }

    /// Довести полёт до конца: развернувшееся окно — на место.
    fn finish_flight(&mut self) {
        let Some(flight) = self.flight.take() else {
            return;
        };
        super::set_animating(false);
        // Сцена под полётом — снимок его начала. Что под ней успело
        // измениться за полёт (терминал допечатал строку, программа
        // перерисовалась), собиралось из снимка; один полный кадр после
        // посадки возвращает экрану правду.
        // Сел диалог, который затемняет стол, — затемнение по всему экрану.
        if self.scene.take().is_some() || dims(flight.app) {
            self.mark(self.union);
        }
        self.mark(flight.last);
        if flight.motion == Motion::Morph {
            crate::kprintln!(
                "  desktop     : {} morph of '{}': {} frames in {} ms",
                if flight.from.w >= flight.to.map_or(0, |to| to.w) { "maximize" } else { "unmaximize" },
                flight.app.title(),
                flight.frames,
                flight.elapsed_ms(crate::time::uptime_ns()),
            );
            self.mark_layer(flight.from);
            return;
        }
        if flight.motion == Motion::Zoom {
            self.closing = None;
            crate::kprintln!(
                "  desktop     : {} zoom of '{}': {} frames in {} ms",
                if flight.restore { "open" } else { "close" },
                flight.app.title(),
                flight.frames,
                flight.elapsed_ms(crate::time::uptime_ns()),
            );
            if flight.restore {
                self.mark_layer(flight.from);
            }
            return;
        }
        self.invalidate_recent();
        if !flight.restore {
            // Список «Последнего» поменялся — на экран выходит его карточка.
            // Прежде здесь выходил весь экран, а картинка под ним собиралась
            // заново целиком.
            let card = super::home::recent_card(self.primary.w, self.scale, self.icons.bounds().bottom());
            self.mark(card);
        }
        crate::kprintln!(
            "  desktop     : {} flight of '{}': {} frames in {} ms",
            if flight.restore { "restore" } else { "minimize" },
            flight.app.title(),
            flight.frames,
            flight.elapsed_ms(crate::time::uptime_ns()),
        );
        if flight.restore {
            if let Some(index) = self.index_of(flight.app) {
                if let Some(window) = self.windows.get_mut(index) {
                    window.minimized = false;
                }
                self.invalidate_recent();
                self.mark_layer(flight.from);
                self.refresh_decorations();
            }
        }
    }

    /// Карточки домашнего экрана телефона — в полосу кадра, под значками.
    fn draw_home(&self, back: &mut Surface, band: Rect, dy: i32) {
        let width = self.primary.w;
        super::home::draw_system(back, band, dy, width, self.scale, &self.home);
        let minimized: Vec<(App, alloc::string::String, u64)> = self
            .windows
            .iter()
            .filter(|window| window.minimized)
            .map(|window| (window.app, alloc::string::String::from(window.caption()), window.minimized_at_ms))
            .collect();
        let rows = super::home::recent_rows(&minimized);
        let card = super::home::recent_card(width, self.scale, self.icons.bounds().bottom());
        super::home::draw_recent(back, band, dy, card, self.scale, &rows, rows.len());
    }

    /// Картинка под окнами устарела — собрать заново к ближайшему кадру.
    pub fn invalidate_under(&mut self) {
        if self.under_valid {
            UNDER_DROPS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        self.under_valid = false;
        self.under_dirty = Rect::EMPTY;
    }

    /// Устарела полоса картинки под окнами — перерисовать только её строки.
    pub fn invalidate_under_rect(&mut self, rect: Rect) {
        if self.under_valid && !rect.is_empty() {
            self.under_dirty = self.under_dirty.union(&rect);
        }
    }

    /// Карточка «Последнее» телефона: меняется, когда окно свернули, вернули
    /// или закрыли. Карточка может появиться и исчезнуть — место её то же.
    fn invalidate_recent(&mut self) {
        let card = super::home::recent_card(self.primary.w, self.scale, self.icons.bounds().bottom());
        self.invalidate_under_rect(card);
    }

    /// Собрать картинку под окнами телефона, если она устарела.
    fn rebuild_under(&mut self) {
        if !theme::is_mobile() || (self.under_valid && self.under_dirty.is_empty()) {
            return;
        }
        // Не во время полёта: сборка стоит десятки миллисекунд, и в кадре
        // полёта она становилась рывком. Под окном — можно и нужно: тогда к
        // сворачиванию картинка уже готова. Первая версия откладывала сборку и
        // под окном, и на телефоне первый же полёт шёл медленным путём (27 мс
        // на кадр, худший 177 мс).
        if self.flight.is_some() {
            return;
        }
        if self.under_valid {
            self.redraw_under_strip();
            return;
        }
        let (width, height) = (self.union.w, self.union.h);
        let fresh = match self.under.take() {
            Some(surface) if surface.width() == width && surface.height() == height => Some(surface),
            _ => Surface::new(width, height, theme::palette().wall_top),
        };
        let Some(mut surface) = fresh else {
            // Памяти не хватило — рисуем в полосы, как раньше.
            return;
        };
        let started = crate::time::uptime_ns();
        let all = Rect::new(0, 0, width, height);
        self.draw_background(&mut surface, all, 0);
        super::statusbar::draw(&mut surface, all, 0, width, self.scale);
        self.draw_home(&mut surface, all, 0);
        self.icons.draw(&mut surface, all, 0);
        self.under = Some(surface);
        self.under_valid = true;
        self.under_dirty = Rect::EMPTY;
        crate::kprintln!(
            "  desktop     : under-window picture rebuilt in {} ms ({} drops so far)",
            crate::time::uptime_ns().wrapping_sub(started) / 1_000_000,
            UNDER_DROPS.load(core::sync::atomic::Ordering::Relaxed)
        );
    }

    /// Перерисовать устаревшие строки картинки под окнами.
    ///
    /// Строки собираются в отдельный буфер во всю ширину экрана — так же, как
    /// полоса кадра, — и копируются в картинку целиком. Прямо в картинку
    /// рисовать нельзя: карточка, задетая полосой краем, рисуется вся, и её
    /// полупрозрачное стекло легло бы вторым слоем поверх прежнего.
    fn redraw_under_strip(&mut self) {
        let width = self.primary.w;
        let dirty = self.under_dirty.intersect(&self.union);
        self.under_dirty = Rect::EMPTY;
        if dirty.is_empty() {
            return;
        }
        let Some(mut under) = self.under.take() else {
            self.under_valid = false;
            return;
        };
        let started = crate::time::uptime_ns();
        let band = Rect::new(0, dirty.y, width, dirty.h);
        if let Some(mut strip) = Surface::new(width, dirty.h, theme::palette().wall_top) {
            let dy = -dirty.y;
            self.draw_background(&mut strip, band, dy);
            super::statusbar::draw(&mut strip, band, dy, width, self.scale);
            self.draw_home(&mut strip, band, dy);
            self.icons.draw(&mut strip, band, dy);
            under.blit_from(&strip, (0, dirty.y), Rect::new(0, 0, width, dirty.h));
            // На экран — тоже отсюда. Тот, кто сбросил полосу, свою область
            // уже пометил, но кадр с ней мог уйти раньше этой перерисовки — со
            // старой картинкой: так после посадки полёта не появлялась карточка
            // «Последнее» (снимок `04-stack`).
            self.mark(dirty);
            crate::kprintln!(
                "  desktop     : under-window rows {}..{} redrawn in {} ms",
                dirty.y,
                dirty.bottom(),
                crate::time::uptime_ns().wrapping_sub(started) / 1_000_000
            );
        } else {
            // Памяти под полосу не хватило — соберём картинку целиком.
            self.under_valid = false;
        }
        self.under = Some(under);
        if !self.under_valid {
            self.rebuild_under();
        }
    }

    /// Открыт ли «Пуск» телефона.
    #[must_use]
    pub const fn start_open(&self) -> bool {
        self.start.is_some()
    }

    /// Ждёт ли поле поиска «Пуска» набора.
    #[must_use]
    pub fn start_focused(&self) -> bool {
        self.start.as_ref().is_some_and(Start::focused)
    }

    /// Где низ листа «Пуск»: над клавиатурой, если она будет видна, иначе над
    /// доком (поле как у листа свёрнутых).
    fn start_bottom(&self, keyboard_up: bool) -> i32 {
        let ctx = mini_ui::paint::Ctx::scaled(self.scale);
        match self.keyboard.as_ref() {
            Some(keyboard) if keyboard_up => keyboard.rect.y - ctx.px(keyboard::GAP) as i32,
            _ => self.primary.h as i32 - ctx.px(100) as i32,
        }
    }

    /// Открыть «Пуск»: `focused` — сразу с полем поиска в фокусе.
    pub fn open_start(&mut self, focused: bool) -> bool {
        self.close_start();
        let user = start::User {
            name: crate::user::session::with_name(|name| alloc::string::String::from(name)),
            uid: crate::user::session::credentials().uid,
            slot: self.home.slot,
        };
        let bottom = self.start_bottom(focused);
        self.start = Start::open(self.primary.w, bottom, self.scale, user, focused);
        match self.start.as_ref() {
            Some(sheet) => {
                let rect = sheet.rect;
                self.mark_layer(rect);
                true
            }
            None => false,
        }
    }

    /// Закрыть «Пуск», если открыт.
    pub fn close_start(&mut self) {
        if let Some(sheet) = self.start.take() {
            self.mark_layer(sheet.rect);
        }
    }

    /// Во что попало нажатие в «Пуск».
    #[must_use]
    pub fn start_hit(&self, x: i32, y: i32) -> Option<start::Hit> {
        self.start.as_ref()?.hit(x, y)
    }

    /// Поле поиска получило фокус — клавиатура появится ближайшим кадром.
    pub fn focus_start(&mut self) {
        if let Some(sheet) = self.start.as_mut() {
            sheet.focus();
        }
    }

    /// Набор в поле поиска.
    pub fn start_type(&mut self, ch: char) {
        if let Some(sheet) = self.start.as_mut() {
            sheet.type_char(ch);
        }
    }

    pub fn start_backspace(&mut self) {
        if let Some(sheet) = self.start.as_mut() {
            sheet.backspace();
        }
    }

    /// Что набрано в поле поиска.
    #[must_use]
    pub fn start_query(&self) -> Option<alloc::string::String> {
        self.start.as_ref().map(|sheet| alloc::string::String::from(sheet.query()))
    }

    /// Первое найденное — то, что запустит ↵.
    #[must_use]
    pub fn start_first(&self) -> Option<start::Target> {
        self.start.as_ref()?.first()
    }

    /// Открыт ли лист «Свёрнутые программы».
    #[must_use]
    pub const fn tray_open(&self) -> bool {
        self.tray.is_some()
    }

    /// Открыть лист свёрнутых над доком — или закрыть, если открыт.
    /// Возвращает, открыт ли он теперь.
    pub fn toggle_tray(&mut self) -> bool {
        if let Some(sheet) = self.tray.take() {
            self.mark_layer(sheet.rect);
            return false;
        }
        self.open_tray(false)
    }

    /// Открыть переключатель программ — тот же лист, другие слова (жест
    /// полоски, 04.11). Окна к этому мигу уже свёрнуты. Сколько в нём строк;
    /// ноль — не открылся.
    pub fn open_switcher(&mut self) -> usize {
        self.close_tray();
        // С 05.11 — карточки со снимками самих окон, а не строки листа:
        // свёрнутое окно хранит поверхность, она и уменьшается.
        let mut minimized: Vec<&Window> =
            self.windows.iter().filter(|window| window.minimized && !window.popup).collect();
        minimized.sort_by(|a, b| b.minimized_at_ms.cmp(&a.minimized_at_ms));
        let cards: Vec<tray::Card> = minimized
            .iter()
            .map(|window| tray::Card {
                app: window.app,
                title: alloc::string::String::from(window.caption()),
                art: window.icon_art(),
                shot: window.surface(),
            })
            .collect();
        let count = cards.len();
        self.tray = Sheet::cards(self.primary.w, self.primary.h, self.scale, &cards);
        drop(cards);
        let Some(rect) = self.tray.as_ref().map(|sheet| sheet.rect) else {
            // Экран мал для карточек — прежний лист строк.
            return if self.open_tray(true) { self.recent_minimized().len() } else { 0 };
        };
        self.mark_layer(rect);
        count
    }

    /// Собрать лист свёрнутых окон.
    fn open_tray(&mut self, switcher: bool) -> bool {
        let now = crate::time::uptime_ms();
        let mut minimized: Vec<&Window> = self.windows.iter().filter(|window| window.minimized).collect();
        minimized.sort_by(|a, b| b.minimized_at_ms.cmp(&a.minimized_at_ms));
        let rows: Vec<tray::Row> = minimized
            .iter()
            .map(|window| tray::Row {
                app: window.app,
                title: alloc::string::String::from(window.caption()),
                note: match window.app {
                    App::Terminal | App::TermWin(_) => alloc::string::String::from("оболочка"),
                    App::Settings => alloc::string::String::from("параметры системы"),
                    App::About => alloc::string::String::from("о системе"),
                    App::Shutdown | App::Restart => alloc::string::String::from("вопрос о питании"),
                    App::OpenWith => alloc::string::String::from("выбор программы"),
                    App::Hung => alloc::string::String::from("программа не отвечает"),
                    App::DeleteAsk => alloc::string::String::from("удаление"),
                    App::Program(task, _) => alloc::format!("задача #{task}"),
                },
                age: tray::age_text(now.saturating_sub(window.minimized_at_ms)),
            })
            .collect();
        self.tray = Sheet::open(self.primary.w, self.primary.h, self.scale, &rows, switcher);
        match self.tray.as_ref() {
            Some(sheet) => {
                let rect = sheet.rect;
                self.mark_layer(rect);
                true
            }
            None => false,
        }
    }

    /// Открыт ли лист питания.
    #[must_use]
    pub const fn power_open(&self) -> bool {
        self.power.is_some()
    }

    /// Открыть лист питания над доком. `false` — не хватило памяти.
    pub fn open_power(&mut self) -> bool {
        self.close_power();
        let can_off = crate::power::can_power_off();
        self.power = super::powersheet::Sheet::open(self.primary.w, self.primary.h, self.scale, can_off);
        match self.power.as_ref() {
            Some(sheet) => {
                let rect = sheet.rect;
                self.mark_layer(rect);
                true
            }
            None => false,
        }
    }

    /// Закрыть лист питания, если открыт.
    pub fn close_power(&mut self) {
        if let Some(sheet) = self.power.take() {
            self.mark_layer(sheet.rect);
        }
    }

    /// Во что попало нажатие в лист питания. `None` — мимо листа.
    #[must_use]
    pub fn power_hit(&self, x: i32, y: i32) -> Option<super::powersheet::Hit> {
        self.power.as_ref()?.hit(x, y)
    }

    /// Закрыть лист свёрнутых, если открыт.
    pub fn close_tray(&mut self) {
        if let Some(sheet) = self.tray.take() {
            self.mark_layer(sheet.rect);
        }
    }

    /// Во что попало нажатие в лист свёрнутых. `None` — мимо листа.
    #[must_use]
    pub fn tray_hit(&self, x: i32, y: i32) -> Option<tray::Hit> {
        self.tray.as_ref()?.hit(x, y)
    }

    /// Свёрнутые окна — для «Развернуть все».
    #[must_use]
    pub fn minimized_apps(&self) -> Vec<App> {
        self.windows.iter().filter(|window| window.minimized).map(|window| window.app).collect()
    }

    /// Видна ли экранная клавиатура.
    #[must_use]
    pub fn keyboard_visible(&self) -> bool {
        self.keyboard.as_ref().is_some_and(Keyboard::is_visible)
    }

    /// Попадает ли точка в экранную клавиатуру.
    #[must_use]
    pub fn keyboard_contains(&self, x: i32, y: i32) -> bool {
        self.keyboard.as_ref().is_some_and(|keyboard| keyboard.contains(x, y))
    }

    /// Нажатие в клавиатуре: что делает кнопка под пальцем.
    pub fn keyboard_press(&mut self, x: i32, y: i32) -> Option<keyboard::Action> {
        self.keyboard.as_mut()?.press(x, y)
    }

    /// Палец отпустили — снять подсветку кнопки и узнать, чем было касание
    /// буквы или пробела: нажатием, словом или сменой языка.
    pub fn keyboard_release(&mut self) -> keyboard::Release {
        self.keyboard.as_mut().map_or(keyboard::Release::Nothing, Keyboard::release)
    }

    /// Палец ведут по клавиатуре. `true` — идёт жест.
    pub fn keyboard_stroke_to(&mut self, x: i32, y: i32) -> bool {
        self.keyboard.as_mut().is_some_and(|keyboard| keyboard.stroke_to(x, y))
    }

    /// Нажата подсказка: `(вставленное, новое, с заглавной)`.
    pub fn keyboard_suggest(&mut self, index: usize) -> Option<(&'static str, &'static str, bool)> {
        self.keyboard.as_mut()?.suggest(index)
    }

    /// Буква напечатана — снять одноразовый Shift.
    pub fn keyboard_consume_shift(&mut self) {
        if let Some(keyboard) = self.keyboard.as_mut() {
            keyboard.consume_shift();
        }
    }

    /// Показать клавиатуру, если активно окно, в которое печатают, и спрятать,
    /// если нет. Возвращает новое состояние, если оно изменилось.
    ///
    /// Вызывается на каждом кадре, а не в местах, где меняется фокус: мест этих
    /// больше десятка (открытие, закрытие, сворачивание, щелчок, Alt+Tab,
    /// программа, открывшая окно сама), и забытое одно оставило бы клавиатуру
    /// висеть над окном, в которое не печатают.
    ///
    /// Вместе с клавиатурой меняется высота окна терминала: низ окна встаёт над
    /// клавиатурой, а когда она уходит — возвращается к доку. Иначе нижние
    /// строки терминала, то есть строка ввода, оказались бы под клавишами.
    ///
    /// Окну программы клавиатуру открывают кнопкой в заголовке (05.11): стол
    /// не знает, ждёт ли программа текста. Пока такое окно активно, клавиатура
    /// видна, а окно стоит над ней (см. [`Self::lift_program_windows`]); ушёл
    /// фокус — просьба снимается.
    pub fn sync_keyboard(&mut self) -> Option<bool> {
        self.keyboard.as_ref()?;
        let focused = self.focused_app();
        for window in self.windows.iter_mut().filter(|window| window.keyboard && Some(window.app) != focused) {
            window.set_keyboard(false, false);
            crate::kprintln!("  desktop     : keyboard released by '{}'", window.caption());
        }
        let program_typing = self.windows.get(self.focus).is_some_and(|window| window.keyboard && !window.minimized);
        // И «Параметры» с открытым полем ввода — пароль Wi-Fi (03.11).
        let settings_typing = focused == Some(App::Settings)
            && self.windows.iter().any(|window| window.app == App::Settings && window.wants_text());
        let want = focused.is_some_and(App::is_terminal) || self.start_focused() || settings_typing || program_typing;
        let keyboard = self.keyboard.as_ref()?;
        if keyboard.is_visible() == want {
            self.lift_program_windows();
            return None;
        }
        let rect = keyboard.rect;
        self.keyboard.as_mut()?.set_visible(want);
        // Вместе с промежутком над клавиатурой: он не принадлежит ни окну, ни
        // клавиатуре, и без этого на ушедшей клавиатуре оставалась полоса
        // прежнего кадра — нижний край карточки «Последнее» срезало ровно по ней.
        let gap = mini_ui::paint::Ctx::scaled(self.scale).px(keyboard::GAP);
        self.mark_layer(Rect::new(rect.x, rect.y - gap as i32, rect.w, rect.h + gap));
        if let Some(panel) = self.panel.as_ref() {
            let dock = panel.rect;
            self.mark(Rect::new(0, dock.y, self.primary.w, self.primary.h.saturating_sub(dock.y.max(0) as u32)));
        }

        // «Пуск» переезжает вместе с клавиатурой: встаёт над ней или опускается
        // к доку.
        let start_bottom = self.start_bottom(want);
        if let Some(sheet) = self.start.as_mut() {
            let before = sheet.move_bottom(start_bottom);
            let after = sheet.rect;
            self.mark_layer(before.union(&after));
        }
        let bottom = if want {
            rect.y - mini_ui::paint::Ctx::scaled(self.scale).px(keyboard::GAP) as i32
        } else {
            self.app_bottom()
        };
        if let Some(index) = self.index_of(App::Terminal) {
            if let Some(window) = self.windows.get_mut(index).filter(|window| !window.minimized) {
                // Свёрнутый терминал размера не меняет. Иначе каждое сворачивание
                // растягивало бы его на весь экран (клавиатура ушла), а
                // разворачивание — ужимало обратно: две поверхности по 3,4 МБ и
                // две перекладки сетки ровно в кадрах полёта. На телефоне это
                // был худший кадр в 55 мс — рывок, который Роман и почувствовал.
                let before = window.rect;
                let height = (bottom - before.y).max(0) as u32;
                if height != before.h && window.resize(before.w, height) {
                    let after = window.rect;
                    self.note_moved(App::Terminal, before, after);
                    // Пересборка поверхности рисует рамку неактивной (см.
                    // `Window::rebuild`): без этого активный терминал с
                    // появлением клавиатуры выглядел погасшим.
                    self.refresh_decorations();
                }
            }
        }
        self.lift_program_windows();
        Some(want)
    }

    /// Кнопка клавиатуры в заголовке окна программы (телефон, 05.11): открыть
    /// клавиатуру для окна или убрать. Возвращает новое состояние; само
    /// появление и сдвиг окна — на ближайшем кадре ([`Self::sync_keyboard`]).
    pub fn toggle_window_keyboard(&mut self, app: App) -> bool {
        let focus = self.focus;
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        let on = !window.keyboard;
        window.set_keyboard(on, index == focus);
        on
    }

    /// Окна программ над клавиатурой (05.11): активное окно, которому её
    /// открыли, встаёт низом над ней, как терминал, — умеющая менять размер
    /// программа получает [`user_abi::WIN_RESIZE`] и раскладывается заново.
    /// Клавиатура ушла или окно её отпустило — низ возвращается к полоске
    /// жеста. Свёрнутое окно ждёт: его размер меняется, когда оно вернётся.
    ///
    /// Каждый кадр, но дёшево: размер меняется, только когда низ окна не там.
    fn lift_program_windows(&mut self) {
        let gap = mini_ui::paint::Ctx::scaled(self.scale).px(keyboard::GAP) as i32;
        let above = self
            .keyboard
            .as_ref()
            .filter(|keyboard| keyboard.is_visible())
            .map(|keyboard| keyboard.rect.y - gap);
        let free = self.app_bottom();
        let focus = self.focus;
        for index in 0..self.windows.len() {
            let Some(window) = self.windows.get_mut(index) else {
                continue;
            };
            if window.minimized || !window.fit {
                continue;
            }
            let bottom = match (window.keyboard && index == focus, above) {
                (true, Some(top)) => top,
                _ if window.lifted => free,
                _ => continue,
            };
            let before = window.rect;
            if before.bottom() == bottom {
                window.lifted = bottom != free;
                continue;
            }
            let height = (bottom - before.y).max(1) as u32;
            if !window.resize(before.w, height) {
                continue;
            }
            window.lifted = bottom != free;
            window.draw_decorations(index == focus);
            let after = window.rect;
            let app = window.app;
            crate::kprintln!("  desktop     : '{}' {} the keyboard at {}x{}", window.caption(), if window.lifted { "above" } else { "back from" }, after.w, after.h);
            self.note_moved(app, before, after);
        }
    }

    /// Довести отложенное изменение размера — палец отпустили.
    pub fn settle_drag(&mut self) {
        // Контур (20.11): окно переезжает туда, где рамку отпустили.
        if let Some(ghost) = self.outline {
            self.set_outline(None);
            if let (Some(app), false) = (self.drag, self.drag_resizes) {
                let (areas, n) = self.work_rects();
                if let Some(window) = self.windows.iter_mut().find(|window| window.app == app) {
                    let before = window.rect;
                    window.move_within(ghost.x - before.x, ghost.y - before.y, &areas[..n]);
                    let after = window.rect;
                    if after != before {
                        self.note_moved(app, before, after);
                        self.follow_owner(app);
                    }
                }
            }
        }
        self.settling = true;
        self.apply_pending_size();
        self.settling = false;
    }

    /// Толщина контура перетаскивания.
    fn outline_width(&self) -> u32 {
        (4 * self.scale.pct() / 100).max(2)
    }

    /// Поставить или убрать контур; перерисовываются только его полосы.
    fn set_outline(&mut self, ghost: Option<Rect>) {
        if self.outline == ghost {
            return;
        }
        let t = self.outline_width();
        for rect in [self.outline, ghost].into_iter().flatten() {
            for strip in super::look::outline_strips(rect, t) {
                self.mark(strip);
            }
        }
        self.outline = ghost;
    }

    /// Применить размер, который просил уголок, — один раз за кадр.
    ///
    /// # Почему не на каждый сдвиг пальца
    ///
    /// Изменение размера — это новая поверхность во весь размер окна (у окна во
    /// весь экран телефона три с половиной мегабайта), заново выложенная сетка
    /// терминала и окно, перерисованное целиком. Сдвиги пальца приходят чаще,
    /// чем это успевает сделаться, и каждый честно делал всю работу для
    /// размера, который на экран не попадал: окно тянулось плавно, но с
    /// опозданием — Роман так и описал. Последний размер к моменту кадра — это
    /// всё, что человек увидит.
    fn apply_pending_size(&mut self) {
        // Контур (20.11): пока тянут, меняется только рамка; размер —
        // при отпускании ([`Self::settle_drag`]).
        if !self.settling && self.drag_resizes && self.drag.is_some() && super::look::outline_drag() {
            if let Some((app, x, y, w, h)) = self.pending_size {
                // The outline obeys the size limits too (24.11): it stops
                // where the window would, the fixed side stays put.
                let edges = self.resize_edges;
                let outline = match self.index_of(app).and_then(|index| self.windows.get(index)) {
                    Some(window) => {
                        let before = window.rect;
                        let (cw, ch) = window.clamp_size(w.max(1) as u32, h.max(1) as u32);
                        let cx = if edges & super::window::EDGE_LEFT != 0 { before.right() - cw as i32 } else { x };
                        let cy = if edges & super::window::EDGE_TOP != 0 { before.bottom() - ch as i32 } else { y };
                        Rect::new(cx, cy, cw, ch)
                    }
                    None => Rect::new(x, y, w.max(1) as u32, h.max(1) as u32),
                };
                self.set_outline(Some(outline));
            }
            return;
        }
        let Some((app, _, _, w, h)) = self.pending_size.take() else {
            return;
        };
        let (areas, n) = self.work_rects();
        let edges = self.resize_edges;
        let Some(index) = self.index_of(app) else {
            return;
        };
        let Some(before) = self.windows.get(index).map(|window| window.rect) else {
            return;
        };
        // Пределы — монитора, на котором окно (по его середине).
        let work = self.work_rect_under(before);
        let focused = index == self.focus;
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        let (left, top) = (edges & super::window::EDGE_LEFT != 0, edges & super::window::EDGE_TOP != 0);
        // Левый и верхний края тянут к краю экрана, правый и нижний — к
        // правому краю и панели; противоположная сторона стоит на месте.
        let limit_w = if left {
            (before.right() - work.x).max(0) as u32
        } else {
            (work.right() - before.x.max(work.x)).max(0) as u32
        };
        let limit_h = if top {
            (before.bottom() - work.y).max(0) as u32
        } else {
            (work.bottom() - before.y).max(0) as u32
        };
        let w = (w.max(0) as u32).min(limit_w);
        let h = (h.max(0) as u32).min(limit_h);
        if !window.resize(w, h) {
            return;
        }
        // `resize` рисует заголовок неактивным — по настоящему фокусу
        // (классический заголовок иначе серел у активного окна, 20.11).
        window.draw_decorations(focused);
        // Окно могло упереться в наименьший размер: угол ставится от
        // неподвижной стороны по тому размеру, что получился.
        if left || top {
            let new_x = if left { before.right() - window.rect.w as i32 } else { before.x };
            let new_y = if top { before.bottom() - window.rect.h as i32 } else { before.y };
            window.move_within(new_x - window.rect.x, new_y - window.rect.y, &areas[..n]);
        }
        if window.rect == before {
            return;
        }
        let after = window.rect;
        self.note_moved(app, before, after);
        self.follow_owner(app);
    }

    /// Запомнить переезд окна до сборки кадра.
    ///
    /// # Почему не пометить сразу
    ///
    /// Пальцем окно тащат десятками сдвигов, и между двумя кадрами их
    /// набегает несколько. Помечая каждый, кадр собирал бы окно столько раз,
    /// сколько было сдвигов, — а на экран из этих положений попадает только
    /// последнее. Помнится первое «откуда»; «куда» спрашивается у окна в миг
    /// сборки.
    fn note_moved(&mut self, app: App, before: Rect, after: Rect) {
        if !super::exact_moves() {
            self.mark_layer(before);
            self.mark_layer(after);
            return;
        }
        match self.moved {
            Some((moving, _)) if moving == app => {}
            Some((other, from)) => {
                // Второе окно в том же кадре — редкость; прежнее помечается
                // сразу, чтобы не держать список.
                let to = self.rect_of(other).unwrap_or(Rect::EMPTY);
                self.mark_moved(from, to);
                self.moved = Some((app, before));
            }
            None => self.moved = Some((app, before)),
        }
    }

    /// Пометить переезд слоя: новое место целиком, из старого — только то, что
    /// новым не накрыто.
    ///
    /// Прежде помечались оба прямоугольника целиком, а при сдвиге на палец они
    /// совпадают почти полностью — кадр собирал окно дважды. Открывшаяся часть
    /// старого места — не больше двух полос шириной в сдвиг.
    ///
    /// Разность считается от прямоугольников **вместе с ореолом тени**: тень
    /// едет вместе со слоем, и её след стирается так же.
    fn mark_moved(&mut self, before: Rect, after: Rect) {
        let halo = self.halo();
        let grow = |rect: Rect| {
            if rect.is_empty() {
                rect
            } else {
                Rect::new(
                    rect.x - halo as i32,
                    rect.y - halo as i32,
                    rect.w + halo * 2,
                    rect.h + halo * 2,
                )
            }
        };
        let (from, to) = (grow(before), grow(after));
        self.mark(to);
        for strip in uncovered(from, to) {
            self.mark(strip);
        }
    }

    /// Ширина ореола вокруг слоя — места под его тень.
    fn halo(&self) -> u32 {
        // На телефоне тени нет (см. [`Self::drop_shadow`]), и ореол там
        // означал бы прямоугольник на 56 точек шире нужного с каждой стороны —
        // два таких накрывают экран целиком, учёт изменённого переполняется, и
        // кадр перерисовывает всё.
        if theme::is_mobile() || !super::look::on(super::look::Flag::Shadows) {
            0
        } else {
            (SHADOW_SPREAD + SHADOW_DROP) * self.scale
        }
    }

    /// Перенести накопленные слоями изменения в общий список.
    fn collect(&mut self) {
        if let Some((app, from)) = self.moved.take() {
            // Окно могли закрыть или свернуть после сдвига: его место тогда уже
            // помечено целиком, и «куда» пусто.
            let to = self
                .windows
                .iter()
                .find(|window| window.app == app && !window.minimized)
                .map_or(Rect::EMPTY, |window| window.rect);
            self.mark_moved(from, to);
        }
        for index in 0..self.windows.len() {
            let Some(window) = self.windows.get_mut(index) else {
                continue;
            };
            let damage = core::mem::replace(&mut window.damage, Rect::EMPTY);
            if damage.is_empty() {
                continue;
            }
            let origin = (window.rect.x, window.rect.y);
            self.mark(damage.translate(origin.0, origin.1));
        }

        if let Some(panel) = self.panel.as_mut() {
            let damage = panel.take_damage();
            // Dock сменил длину — старое место стереть вместе с тенью.
            let vacated = panel.take_vacated();
            let whole = panel.rect;
            if !damage.is_empty() {
                let rect = damage.translate(panel.rect.x, panel.rect.y);
                self.mark(rect);
            }
            if !vacated.is_empty() {
                self.mark_layer(vacated.union(&whole));
            }
        }
        let mut side = Vec::new();
        for (_, panel) in &mut self.side_panels {
            let damage = panel.take_damage();
            let vacated = panel.take_vacated();
            if !damage.is_empty() {
                side.push((damage.translate(panel.rect.x, panel.rect.y), false));
            }
            if !vacated.is_empty() {
                side.push((vacated.union(&panel.rect), true));
            }
        }
        for (rect, layer) in side {
            if layer {
                self.mark_layer(rect);
            } else {
                self.mark(rect);
            }
        }

        if let Some(menu) = self.menu.as_mut() {
            let damage = menu.take_damage();
            // Подменю классического «Пуска» (22.11) — места на экране.
            let sub = menu.take_sub_damage();
            if !damage.is_empty() {
                let rect = damage.translate(menu.rect.x, menu.rect.y);
                self.mark(rect);
            }
            if !sub.is_empty() {
                self.mark_layer(sub);
            }
        }

        if let Some(menu) = self.context.as_mut() {
            let damage = menu.take_damage();
            let sub = menu.take_sub_damage();
            if !damage.is_empty() {
                let rect = damage.translate(menu.rect.x, menu.rect.y);
                self.mark(rect);
            }
            // Подменю появляется и уходит целиком — вместе с тенью.
            if !sub.is_empty() {
                self.mark_layer(sub);
            }
        }

        if let Some(sheet) = self.start.as_mut() {
            let damage = sheet.take_damage();
            if !damage.is_empty() {
                let rect = damage.translate(sheet.rect.x, sheet.rect.y);
                self.mark(rect);
            }
        }
        if let Some(keyboard) = self.keyboard.as_mut() {
            let damage = keyboard.take_damage();
            if !damage.is_empty() && keyboard.is_visible() {
                let rect = damage.translate(keyboard.rect.x, keyboard.rect.y);
                self.mark(rect);
            }
        }
        if let Some(shade) = self.shade.as_mut() {
            let damage = shade.take_damage();
            if !damage.is_empty() {
                let rect = damage.translate(shade.rect.x, shade.rect.y);
                self.mark(rect);
            }
        }
    }

    /// Открыть или закрыть шторку. Возвращает новое состояние.
    ///
    /// `None` — шторки на этой машине нет (см. [`Shade::new`]).
    pub fn toggle_shade(&mut self) -> Option<bool> {
        let open = self.shade.as_mut()?.toggle();
        // Дальше её ведёт `advance_shade` кадр за кадром; первый кадр — сразу.
        super::set_animating(true);
        Some(open)
    }

    /// Шаг выезда шторки: стереть прошлое место, пометить новое. Возвращает,
    /// едет ли она ещё (тогда стол просит следующий кадр).
    fn advance_shade(&mut self) -> bool {
        let Some(shade) = self.shade.as_mut() else { return false };
        if !shade.is_moving() {
            return false;
        }
        let (was, now) = shade.advance(crate::time::uptime_ns());
        let moving = shade.is_moving();
        // Под прошлым местом — стол и окна, которые никто не перерисовывал:
        // они не «изменились», но их снова видно. Тень шире самой шторки.
        let pad = mini_ui::paint::Ctx::scaled(self.scale).px(24) as i32;
        let grow = |rect: Rect| if rect.is_empty() { rect } else { Rect::new(rect.x - pad, rect.y - pad, rect.w + pad as u32 * 2, rect.h + pad as u32 * 2) };
        self.mark_layer(grow(was));
        self.mark_layer(grow(now));
        moving
    }

    /// Нажатие в открытую шторку — что сделать (см. [`super::shade::Action`]).
    pub fn shade_press(&mut self, x: i32, y: i32) -> Option<super::shade::Action> {
        let shade = self.shade.as_mut()?;
        let action = shade.press(x, y);
        // «Не беспокоить» перерисовало шторку изнутри.
        let damage = shade.take_damage();
        if !damage.is_empty() {
            let rect = damage.translate(shade.rect.x, shade.rect.y);
            self.mark(rect);
        }
        action
    }

    /// Перерисовать открытую шторку (сменилась громкость, Wi-Fi).
    pub fn refresh_shade(&mut self) {
        if let Some(shade) = self.shade.as_mut() {
            shade.refresh();
        }
    }

    /// Громкость под пальцем на полосе звука шторки (протяжка).
    #[must_use]
    pub fn shade_volume_at(&self, x: i32) -> Option<u8> {
        self.shade.as_ref()?.volume_at(x)
    }

    #[must_use]
    pub fn shade_open(&self) -> bool {
        self.shade.as_ref().is_some_and(Shade::is_open)
    }

    /// Попадает ли точка в открытую шторку.
    #[must_use]
    pub fn shade_contains(&self, x: i32, y: i32) -> bool {
        self.shade.as_ref().is_some_and(|shade| shade.contains(x, y))
    }

    /// Меню стола: открыть в точке, закрыть, спросить пункт под указателем.
    pub fn open_context(&mut self, x: i32, y: i32, items: &[Action]) {
        // В пределах монитора под точкой; на основном — над панелью.
        let area = self.area_of(self.output_near(x, y), self.work_bottom());
        let rect = self.context.as_mut().map(|menu| {
            menu.open_at(x, y, area, items);
            menu.rect
        });
        // Вместе с тенью: метка самого меню — только его прямоугольник, и
        // тень в первом кадре ложилась не везде (полоска под меню, 02.11).
        if let Some(rect) = rect {
            self.mark_layer(rect);
        }
    }

    pub fn context_open(&self) -> bool {
        self.context.as_ref().is_some_and(ContextMenu::is_open)
    }

    /// Занято ли меню набором имени или вопросом об удалении.
    pub fn context_editing(&self) -> bool {
        self.context
            .as_ref()
            .is_some_and(|menu| menu.is_open() && menu.is_editing())
    }

    /// Где стоит меню стола — если оно открыто.
    pub fn context_rect(&self) -> Option<Rect> {
        self.context
            .as_ref()
            .filter(|menu| menu.is_open())
            .map(|menu| menu.rect)
    }

    /// Накрывает ли меню стола эту точку.
    pub fn context_contains(&self, x: i32, y: i32) -> bool {
        self.context.as_ref().is_some_and(|menu| menu.contains(x, y))
    }

    /// Погасить пункты открытого меню стола.
    pub fn context_set_off(&mut self, off: alloc::vec::Vec<Action>) {
        if let Some(menu) = self.context.as_mut() {
            menu.set_off(off);
        }
    }

    /// Нажимается ли пункт открытого меню стола.
    #[must_use]
    pub fn context_enabled(&self, action: Action) -> bool {
        self.context.as_ref().is_some_and(|menu| menu.is_enabled(action))
    }

    pub fn context_action_at(&self, x: i32, y: i32) -> Option<Action> {
        self.context.as_ref().and_then(|menu| menu.action_at(x, y))
    }

    /// Отдать клавишу меню стола.
    pub fn context_key(&mut self, event: crate::input::KeyEvent) -> Reply {
        match self.context.as_mut() {
            Some(menu) => menu.handle_key(event),
            None => Reply::Ignored,
        }
    }

    /// Подсветить в меню пункт, по которому щёлкнули.
    pub fn context_select(&mut self, action: Action) {
        if let Some(menu) = self.context.as_mut() {
            menu.select_action(action);
        }
    }

    /// Программы «Открыть с помощью» для следующего открытия меню.
    pub fn context_handlers(&mut self, handlers: alloc::vec::Vec<(alloc::string::String, alloc::string::String)>) {
        if let Some(menu) = self.context.as_mut() {
            menu.set_handlers(handlers);
        }
    }

    /// Команда программы «Открыть с помощью» номер `index`.
    #[must_use]
    pub fn context_handler(&self, index: u8) -> Option<alloc::string::String> {
        self.context.as_ref().and_then(|menu| menu.handler(index)).map(alloc::string::String::from)
    }

    /// Где на экране кнопка окна на панели задач.
    #[must_use]
    pub fn button_of(&self, app: App) -> Option<Rect> {
        let own = self.rect_of(app).and_then(|rect| self.panel_on(self.panel_output_of(rect))).and_then(|panel| panel.anchor(app));
        own.or_else(|| self.all_panels().find_map(|panel| panel.anchor(app)))
    }

    /// Ярлык программы ли это (из пакета, со стрелкой).
    #[must_use]
    pub fn icon_is_shortcut(&self, index: usize) -> bool {
        self.icons.item(index).is_some_and(|item| item.shortcut.is_some())
    }

    /// Показать в меню ответ действия — оно остаётся открытым.
    pub fn context_note(&mut self, note: &str) {
        if let Some(menu) = self.context.as_mut() {
            menu.set_note(note);
        }
    }

    /// Открыть миниатюры окон `apps` над кнопкой окна `anchor` (72e).
    /// Возвращает место полосы.
    pub fn open_thumbs(&mut self, apps: Vec<App>, anchor: App) -> Option<Rect> {
        self.open_thumbs_at(apps, anchor, None)
    }

    /// То же, но над кнопкой той панели, на которой стоит точка `at` —
    /// указатель (22.11, Роман: у терминала и окон Wine миниатюры
    /// всплывали на другом мониторе). Кнопка окна ищется по монитору, где
    /// лежит окно, а наводят на неё на панели своего монитора: у окна
    /// Wayland/терминала первое окно группы стоит не там (или вне экрана),
    /// и полоса уезжала к чужой панели. Панель под указателем знает кнопку
    /// наверняка — сначала спрашивается она.
    pub fn open_thumbs_at(&mut self, apps: Vec<App>, anchor: App, at: Option<(i32, i32)>) -> Option<Rect> {
        let hovered = at.and_then(|(x, y)| {
            let panel = self.all_panels().find(|panel| panel.rect.contains(x, y))?;
            core::iter::once(anchor).chain(apps.iter().copied()).find_map(|app| panel.anchor(app))
        });
        let button = match hovered {
            Some(button) => button,
            None => self.button_of(anchor)?,
        };
        // Монитор кнопки и его панель (20.11, панели на всех мониторах).
        let output = self.output_near(button.x + button.w as i32 / 2, button.y + button.h as i32 / 2);
        let (area, panel_top) = match self.panel_on(output) {
            Some(panel) => (panel.area(), panel.rect.y),
            None => (self.primary, self.panel.as_ref().map_or(self.primary.bottom(), |panel| panel.rect.y)),
        };
        self.close_thumbs();
        let mut thumbs = super::thumbs::Thumbs::new(apps, button, area, panel_top, self.scale);
        // Панель сверху или сбоку (20.11) — полоса под ней или рядом с ней.
        let placed = self.beside_panel(thumbs.rect, button);
        thumbs.move_to(placed.x, placed.y);
        let rect = thumbs.rect;
        self.thumbs = Some(thumbs);
        self.mark_layer(rect);
        Some(rect)
    }

    /// Открыть окно трея над значком `item`. `false` — не вышло.
    pub fn open_tray_pop(&mut self, kind: super::traypop::Kind, item: super::panel::TrayItem) -> bool {
        let Some(panel) = self.panel.as_ref() else { return false };
        // An icon kept under the chevron (24.11) opens its window above the
        // chevron.
        let Some(anchor) = panel.tray_rect(item).or_else(|| panel.tray_rect(super::panel::TrayItem::Overflow)) else { return false };
        let panel_top = panel.home().y;
        let Some(mut pop) = super::traypop::TrayPop::new(kind, anchor, self.primary, panel_top, self.scale) else {
            return false;
        };
        pop.rect = self.beside_panel(pop.rect, anchor);
        self.mark_layer(pop.rect);
        super::panel::set_overflow_open(kind == super::traypop::Kind::Overflow);
        self.tray_pop = Some(pop);
        true
    }

    pub fn close_tray_pop(&mut self) {
        if let Some(pop) = self.tray_pop.take() {
            self.mark_layer(pop.rect);
        }
        super::panel::set_overflow_open(false);
    }

    #[must_use]
    pub fn tray_pop_kind(&self) -> Option<super::traypop::Kind> {
        self.tray_pop.as_ref().map(|pop| pop.kind)
    }

    #[must_use]
    pub fn tray_pop_hit(&self, x: i32, y: i32) -> Option<super::traypop::Action> {
        self.tray_pop.as_ref().and_then(|pop| pop.hit(x, y))
    }

    /// Громкость под `x` ползунка открытого окна трея.
    #[must_use]
    pub fn tray_pop_volume_at(&self, x: i32) -> Option<u8> {
        self.tray_pop.as_ref().and_then(|pop| pop.volume_at(x))
    }

    /// Перерисовать открытое окно трея.
    pub fn tray_pop_refresh(&mut self) {
        let scale = self.scale;
        let rect = match self.tray_pop.as_mut() {
            Some(pop) => {
                pop.refresh(scale);
                pop.rect
            }
            None => return,
        };
        self.mark(rect);
    }

    /// Tray icons of the main panel as `tray:<item> at X,Y WxH` (24.11).
    #[must_use]
    pub fn tray_spots(&self) -> Vec<alloc::string::String> {
        self.panel.as_ref().map(super::panel::Panel::tray_spots).unwrap_or_default()
    }

    /// Cells of the open tray popup as `traypop:<n> at X,Y WxH` (24.11).
    #[must_use]
    pub fn tray_pop_spots(&self) -> Vec<alloc::string::String> {
        self.tray_pop.as_ref().map(super::traypop::TrayPop::spots).unwrap_or_default()
    }

    #[must_use]
    pub fn tray_pop_contains(&self, x: i32, y: i32) -> bool {
        self.tray_pop.as_ref().is_some_and(|pop| pop.rect.contains(x, y))
    }

    /// Окно терминала, в полосе которого вкладка сессии `session` (20.11:
    /// окон терминала может быть несколько - вкладки уносят в новые окна).
    pub fn terminal_of(&mut self, session: usize) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.app.is_terminal() && window.tab_sessions().contains(&session))
    }

    /// Какое окно терминала держит вкладку `session`.
    #[must_use]
    pub fn terminal_app_of(&self, session: usize) -> Option<App> {
        self.windows.iter().find(|window| window.app.is_terminal() && window.tab_sessions().contains(&session)).map(|window| window.app)
    }

    /// Сессии всех вкладок всех окон терминала.
    #[must_use]
    pub fn terminal_sessions(&self) -> Vec<usize> {
        self.windows.iter().filter(|window| window.app.is_terminal()).flat_map(|window| window.tab_sessions().iter().copied()).collect()
    }

    /// Окно терминала, ближайшее к человеку: активное, если это терминал,
    /// иначе верхнее из окон терминала.
    #[must_use]
    pub fn front_terminal(&self) -> Option<App> {
        match self.focused_app() {
            Some(app) if app.is_terminal() => Some(app),
            _ => self.windows.iter().rev().find(|window| window.app.is_terminal() && !window.minimized).map(|window| window.app),
        }
    }

    /// Окно терминала, на полосу вкладок которого указывает точка, - если
    /// оно верхнее в этой точке, не считая окна `except` (его и несут).
    #[must_use]
    pub fn terminal_strip_at(&self, x: i32, y: i32, except: App) -> Option<App> {
        let window = self
            .windows
            .iter()
            .rev()
            .filter(|window| window.app != except && !window.minimized && !window.popup)
            .find(|window| window.rect.contains(x, y))?;
        (window.app.is_terminal() && window.strip_contains(x, y)).then_some(window.app)
    }

    /// Окна терминала уходят со стола: их вкладки - тоже (оболочки сессий,
    /// кроме первой, которая живёт всегда, просят закрыться).
    fn forget_terminal(window: &Window) {
        if window.app.is_terminal() {
            for &session in window.tab_sessions() {
                if session != 0 {
                    super::request_tab(super::TabRequest::Close(session));
                }
            }
        }
    }

    /// Собрать подложку подсмотра ([`Self::peek_back`]): стол полосами по
    /// [`PEEK_STRIP`] строк - обои и значки отдельно, окна отдельно, - каждая
    /// полоса сразу уменьшается. Окна на подложке - вполовину прозрачные
    /// (среднее с обоями), всё размыто и приглушено. Полный кадр во весь стол
    /// не держится: на 2560x1440 это 14 МБ ради одного наведения.
    fn build_peek_back(&mut self) {
        if theme::is_mobile() {
            return;
        }
        let bounds = self.union;
        let (sw, sh) = (bounds.w.div_ceil(PEEK_STEP), bounds.h.div_ceil(PEEK_STEP));
        let started = crate::time::uptime_ns();
        let (Some(mut small), Some(mut wall), Some(mut strip)) = (
            Surface::new(sw, sh, Color::rgb(0, 0, 0)),
            Surface::new(sw, sh, Color::rgb(0, 0, 0)),
            Surface::new(bounds.w, PEEK_STRIP, theme::palette().wall_top),
        ) else {
            return;
        };
        let radius = super::look::corner(theme::R_WINDOW * self.scale);
        let mut top = bounds.y;
        while top < bounds.bottom() {
            let h = PEEK_STRIP.min((bounds.bottom() - top) as u32);
            let band = Rect::new(bounds.x, top, bounds.w, h);
            let dy = -top;
            self.draw_background(&mut strip, band, dy);
            super::statusbar::draw(&mut strip, band, dy, self.primary.w, self.scale);
            self.icons.draw(&mut strip, band, dy);
            let row = ((top - bounds.y) as u32) / PEEK_STEP;
            shrink_into(&strip, h, &mut wall, row);
            for window in self.windows.iter().filter(|window| !window.minimized && !window.popup) {
                if !window.shaped {
                    self.drop_shadow(&mut strip, window.rect, band, dy, radius);
                }
                self.stack_window(&mut strip, window, band, dy, radius);
            }
            shrink_into(&strip, h, &mut small, row);
            top += h as i32;
        }
        // Окна - вполовину сквозь (среднее с обоями под ними), всё темнее на
        // четверть: подсмотренное окно читается сразу.
        for y in 0..sh {
            let under = wall.row(y);
            let line = small.row_mut(y);
            for (pixel, &back) in line.iter_mut().zip(under) {
                let mixed = ((*pixel >> 1) & 0x7F7F_7F7F) + ((back >> 1) & 0x7F7F_7F7F);
                *pixel = darken(mixed) | 0xFF00_0000;
            }
        }
        box_blur(&mut small, 3);
        box_blur(&mut small, 3);
        crate::kprintln!(
            "  desktop     : peek backdrop {}x{} built in {} us",
            sw,
            sh,
            crate::time::uptime_ns().wrapping_sub(started) / 1000
        );
        self.peek_back = Some(small);
    }

    /// Подложка подсмотра в полосу кадра: растяжение в [`PEEK_STEP`] раз с
    /// линейной интерполяцией - сначала по вертикали в строку, потом по
    /// горизонтали.
    fn draw_peek_back(&self, back: &mut Surface, band: Rect, dy: i32, small: &Surface, alpha: u32) {
        let bounds = self.union;
        let (sw, sh) = (small.width(), small.height());
        if sw == 0 || sh == 0 {
            return;
        }
        let step = PEEK_STEP as i32;
        let mut line = Vec::new();
        if line.try_reserve_exact(sw as usize).is_err() {
            return;
        }
        line.resize(sw as usize, 0u32);
        for y in band.y..band.bottom() {
            let ty = back.height() as i32;
            let out_y = y + dy;
            if out_y < 0 || out_y >= ty {
                continue;
            }
            // Центр точки подложки - в середине её клетки.
            let v = (((y - bounds.y) * 256 + 128) / step - 128).max(0);
            let (r0, fy) = ((v >> 8) as u32, (v & 255) as u32);
            let r0 = r0.min(sh - 1);
            let r1 = (r0 + 1).min(sh - 1);
            let (a, b) = (small.row(r0), small.row(r1));
            for (index, slot) in line.iter_mut().enumerate() {
                *slot = lerp(a[index], b[index], fy);
            }
            // Буфер полосы - в координатах стола по горизонтали (см.
            // [`Self::compose`]).
            let out = back.row_mut(out_y as u32);
            let from = band.x.max(0);
            let to = band.right().min(out.len() as i32);
            for x in from..to {
                let u = (((x - bounds.x) * 256 + 128) / step - 128).max(0);
                let (c0, fx) = ((u >> 8) as u32, (u & 255) as u32);
                let c0 = c0.min(sw - 1);
                let c1 = (c0 + 1).min(sw - 1);
                let value = lerp(line[c0 as usize], line[c1 as usize], fx);
                out[x as usize] = if alpha >= 255 { value } else { lerp(out[x as usize], value, alpha) };
            }
        }
    }

    /// Убрать миниатюры.
    pub fn close_thumbs(&mut self) {
        if let Some(thumbs) = self.thumbs.take() {
            self.mark_layer(thumbs.rect);
        }
        self.set_peek(None);
    }

    #[must_use]
    pub fn thumbs_open(&self) -> bool {
        self.thumbs.is_some()
    }

    /// Место полосы и кнопка под ней.
    #[must_use]
    pub fn thumbs_area(&self) -> Option<(Rect, Rect)> {
        self.thumbs.as_ref().map(|thumbs| (thumbs.rect, thumbs.button))
    }

    /// Окна в полосе по порядку.
    #[must_use]
    pub fn thumbs_apps(&self) -> Vec<App> {
        self.thumbs.as_ref().map(|thumbs| thumbs.apps.clone()).unwrap_or_default()
    }

    /// Окно карточки под точкой и попала ли точка в крестик.
    #[must_use]
    pub fn thumbs_hit(&self, x: i32, y: i32) -> Option<(App, bool)> {
        let thumbs = self.thumbs.as_ref()?;
        let (index, cross) = thumbs.hit(x, y, self.scale)?;
        Some((thumbs.apps[index], cross))
    }

    /// Указатель над полосой: подсветить карточку и показать её окно поверх
    /// всех («подсмотр»).
    pub fn thumbs_hover(&mut self, x: i32, y: i32) {
        let scale = self.scale;
        let Some(thumbs) = self.thumbs.as_mut() else {
            return;
        };
        let hit = thumbs.hit(x, y, scale);
        let (hovered, cross) = (hit.map(|(index, _)| index), hit.is_some_and(|(_, cross)| cross));
        if thumbs.hovered == hovered && thumbs.cross == cross {
            return;
        }
        thumbs.hovered = hovered;
        thumbs.cross = cross;
        let rect = thumbs.rect;
        let peek = hovered.map(|index| thumbs.apps[index]);
        self.mark(rect);
        self.set_peek(peek);
    }

    /// Нарисовать полосу миниатюр в полосе кадра.
    fn draw_thumbs(&self, back: &mut Surface, band: Rect, dy: i32, thumbs: &super::thumbs::Thumbs) {
        use super::thumbs::{HEAD_H, PAD, SHOT_H, SHOT_W, Thumbs};
        use mini_ui::glyphicon::{self, Icon};
        use mini_ui::typeface::Role;
        let ctx = mini_ui::paint::Ctx::scaled(self.scale);
        let p = ctx.palette;
        let px = |value: u32| value * self.scale;
        let radius = px(theme::R_WINDOW);
        self.drop_shadow(back, thumbs.rect, band, dy, radius);
        let strip = thumbs.rect.translate(0, dy);
        let fill = p.tb1.over(p.tb2.color);
        draw::rounded(back, strip, radius, fill, 255);
        draw::rounded_stroke(back, strip, radius, p.line3.color, p.line3.alpha);
        let ctx = ctx.on(fill);
        for (index, card) in thumbs.cards.iter().enumerate() {
            let Some(window) = self.windows.iter().find(|window| window.app == thumbs.apps[index]) else {
                continue;
            };
            let hovered = thumbs.hovered == Some(index);
            let at = card.translate(0, dy);
            if hovered {
                draw::rounded(back, at, px(6), p.btnhov.over(fill), 255);
                draw::rounded_stroke(back, at, px(6), p.acc, 255);
            }
            // Заголовок: значок 16, имя окна, у наведённой — крестик.
            let icon = Rect::new(at.x + px(PAD) as i32, at.y + px(4 + 4) as i32, px(16), px(16));
            window.icon_art().draw(ctx, back, icon);
            let cross = Thumbs::cross_rect(*card, self.scale).translate(0, dy);
            let text_x = icon.right() + px(8) as i32;
            let room = (cross.x - px(4) as i32 - text_x).max(0) as u32;
            let head = Rect::new(at.x, at.y + px(4) as i32, at.w, px(HEAD_H));
            mini_ui::paint::text_clipped(ctx, back, Role::Body, text_x, mini_ui::paint::baseline(ctx, Role::Body, head), room, &window.label(), p.ink);
            if hovered {
                let plate = if thumbs.cross { Color::rgb(0xE8, 0x3B, 0x2C) } else { p.bad };
                draw::rounded(back, cross, px(4), plate, 255);
                let glyph = px(10);
                glyphicon::draw(
                    back,
                    Icon::Close,
                    cross.x + (cross.w as i32 - glyph as i32) / 2,
                    cross.y + (cross.h as i32 - glyph as i32) / 2,
                    glyph,
                    Color::rgb(0xFF, 0xFF, 0xFF),
                    255,
                );
            }
            // Миниатюра: окно, вписанное в 200×124 с сохранением пропорций.
            let shot = Rect::new(at.x + px(PAD) as i32, at.y + px(PAD + HEAD_H) as i32, px(SHOT_W), px(SHOT_H));
            back.fill(shot, p.sunk.over(fill));
            let src = window.surface();
            let (sw, sh) = (src.width().max(1), src.height().max(1));
            let (dw, dh) = if u64::from(sw) * u64::from(shot.h) > u64::from(sh) * u64::from(shot.w) {
                (shot.w, (u64::from(sh) * u64::from(shot.w) / u64::from(sw)).max(1) as u32)
            } else {
                ((u64::from(sw) * u64::from(shot.h) / u64::from(sh)).max(1) as u32, shot.h)
            };
            let ox = shot.x + (shot.w as i32 - dw as i32) / 2;
            let oy = shot.y + (shot.h as i32 - dh as i32) / 2;
            for ty in 0..dh as i32 {
                let target = oy + ty;
                if target < 0 || target >= back.height() as i32 {
                    continue;
                }
                let sy = (ty as u64 * u64::from(sh) / u64::from(dh)) as u32;
                let line: Vec<u32> = (0..dw).map(|tx| src.row(sy)[(u64::from(tx) * u64::from(sw) / u64::from(dw)) as usize]).collect();
                let row = back.row_mut(target as u32);
                for (tx, pixel) in line.into_iter().enumerate() {
                    let x = ox + tx as i32;
                    if x >= 0 && (x as usize) < row.len() {
                        row[x as usize] = pixel;
                    }
                }
            }
        }
    }

    /// Снять подсветку пункта меню стола.
    pub fn context_select_none(&mut self) {
        if let Some(menu) = self.context.as_mut() {
            menu.select_none();
        }
    }

    /// Закрыть меню стола и стереть его с экрана.
    pub fn close_context(&mut self) {
        self.set_peek(None);
        let rect = self.context.as_ref().map(|menu| menu.rect);
        if let Some(menu) = self.context.as_mut() {
            menu.close();
        }
        if let Some(rect) = rect {
            self.mark_layer(rect);
        }
    }

    /// Перекрасить стол в другую тему.
    ///
    /// Перекрашивается всё и сразу: тема — это цвет каждой точки, и оставить
    /// хоть один слой в прежней палитре значит показать человеку наполовину
    /// перекрашенный стол, который он примет за поломку.
    ///
    /// Поверхности при этом не пересоздаются: размеры от темы не зависят, а
    /// пересоздание стоило бы отказа выделения там, где ничего выделять не
    /// нужно.
    pub fn restyle(&mut self, status: &Status) {
        self.invalidate_under();
        let focus = self.focus;
        for (index, window) in self.windows.iter_mut().enumerate() {
            window.restyle();
            window.draw_decorations(index == focus);
        }
        self.icons.restyle();
        if let Some(menu) = self.context.as_mut() {
            menu.restyle();
        }
        // Панель рисует себя заново только при изменившемся содержимом (см.
        // [`Panel::redraw`]), а здесь меняются цвета — их отпечаток не видит.
        if let Some(panel) = self.panel.as_mut() {
            panel.forget();
        }
        for (_, panel) in &mut self.side_panels {
            panel.forget();
        }
        self.refresh_panel(status);
        if let Some(menu) = self.menu.as_mut() {
            menu.restyle();
        }
        self.repaint_all();
    }

    /// Пересобрать все окна под новую высоту заголовка и перекрасить стол.
    ///
    /// Окно, которому после этого не хватило рабочей области, поджимается к
    /// её низу: заголовок вырос, и нижняя кромка иначе ушла бы под панель.
    pub fn retitle_all(&mut self, status: &Status) {
        // Классика меняет и толщину панели задач (22.11: 28 точек) —
        // панель и рабочая область пересобираются до окон.
        if self.panel.as_ref().is_some_and(Panel::stale) {
            self.rebuild_taskbar();
            crate::kprintln!("  desktop     : taskbar rebuilt, panel {} px", self.primary.bottom() - self.work_bottom());
        }
        let works: Vec<Rect> = self.windows.iter().map(|window| self.work_rect_under(window.rect)).collect();
        for (window, work) in self.windows.iter_mut().zip(works) {
            window.retitle();
            let overflow = window.rect.bottom() - work.bottom();
            if overflow > 0 {
                window.rect.y = (window.rect.y - overflow).max(work.y);
            }
            // 23.11: a framed program keeps its content size (or is asked for
            // the new one, see `Window::retitle`) - the journal says which.
            if window.is_program() && !window.frameless && !window.popup {
                let (w, h) = window.program_size();
                crate::kprintln!(
                    "  desktop     : retitled '{}' {}x{}, content {w}x{h} frame {}",
                    window.caption(),
                    window.rect.w,
                    window.rect.h,
                    window.frame()
                );
            }
        }
        self.restyle(status);
    }

    /// Собрать слои под новую раскладку мониторов (или экран другого
    /// размера), ничего пока не меняя: `rects` — места мониторов на столе (от
    /// (0, 0) охватывающего прямоугольника), `primary` — номер основного.
    ///
    /// Отдельно от [`Self::adopt_outputs`], и порядок важен: память
    /// спрашивается **до** того, как переключён адаптер. Не хватило — `None`,
    /// адаптер не тронут, экран прежний. В обратном порядке отказ означал бы
    /// экран нового размера и стол, которому нечем на нём рисовать.
    pub fn prepare_layout(&self, rects: &[Rect], primary: usize) -> Option<Layers> {
        let (union, primary, _) = layout_of_rects(rects, primary)?;
        let back = band_buffer(union)?;
        let panel = Panel::new(primary, self.scale);
        let (home, cfg) = panel.as_ref().map_or((Rect::EMPTY, super::deskcfg::taskbar()), |panel| (panel.home(), panel.config()));
        let menu = Menu::new(primary, home, cfg, self.scale);
        let context = ContextMenu::new(self.scale);
        Some(Layers { back, panel, menu, context })
    }

    /// Перейти на новую раскладку мониторов или экран нового размера — смена
    /// режима на ходу. `layers` собраны [`Self::prepare_layout`] под эти же
    /// места.
    ///
    /// Всё, что посчитано от размера экрана, заменяется собранным заранее.
    /// Окна переезжают, а не строятся заново: в них состояние — строки
    /// терминала, память программы, — и пересоздать окно значило бы его
    /// потерять. Окно, не влезающее в свой монитор, поджимается, если умеет
    /// менять размер, и в любом случае прижимается внутрь: окно программы
    /// размера не меняет (см. `Window::rebuild`).
    ///
    /// Масштаб интерфейса остаётся прежним. Смена масштаба пересобрала бы сетку
    /// терминала, и строки, уже набранные человеком, переехали бы — это другая
    /// работа, и в эту фазу она не входит.
    pub fn adopt_outputs(&mut self, outputs: Vec<OutputSlot>, layers: Layers, status: &Status) {
        let Some((union, primary, out_rects)) = layout_of(&outputs) else {
            return;
        };
        // Мониторов стало несколько — курсор адаптера прячется, стрелку
        // дальше рисует стол (см. [`Self::hand_cursor`]). Остался один —
        // курсор адаптера снова заведётся при первом движении мыши.
        if self.hw_cursor && outputs.len() > 1 {
            let (x, y) = self.pointer.position();
            crate::display::cursor_visible(false, x, y);
            self.hw_cursor = false;
        }
        self.outputs = outputs;
        self.union = union;
        self.primary = primary;
        self.out_rects = out_rects;
        // Обои мониторов, которых в новой раскладке нет, — лишние мегабайты.
        let sizes: Vec<(u32, u32)> = self.output_rects().iter().map(|r| (r.w, r.h)).collect();
        self.wall.borrow_mut().retain(|(_, made)| sizes.contains(&(made.width, made.height)));
        let (width, height) = (primary.w, primary.h);
        self.back = layers.back;
        self.panel = layers.panel;
        self.menu = layers.menu;
        self.context = layers.context;
        // Клавиатура собирается заново под новый экран: её ширина и место — от
        // него. Видимость вернёт ближайший кадр (см. [`Self::sync_keyboard`]).
        self.keyboard = Keyboard::new(width, height, self.scale);
        self.pointer = Pointer::new(self.primary);
        self.pointer.set_scale(self.scale);
        self.under = None;
        self.under_valid = false;
        self.scene = None;
        self.drag = None;
        self.side_panels.clear();
        self.menu_output = None;
        self.snap_pop = None;
        self.build_side_panels();
        self.place_icon_grid();
        self.icons.reload();
        // Развёрнутое на телефоне окно («на весь экран», 05.11) и после
        // смены режима — во весь новый экран, а не прежнего размера.
        let area = theme::is_mobile().then(|| self.app_area());
        // Каждое окно вписывается в рабочую область монитора, на котором
        // оно оказалось (по середине; в щели — ближайшего).
        let works: Vec<Rect> = self.windows.iter().map(|window| self.work_rect_under(window.rect)).collect();
        for (window, work) in self.windows.iter_mut().zip(works) {
            if let Some(area) = area.filter(|_| window.is_maximized() && !window.popup) {
                if window.resize(area.w, area.h) {
                    window.rect.x = area.x;
                    window.rect.y = area.y;
                    continue;
                }
            }
            let fit_w = window.rect.w.min(work.w);
            let fit_h = window.rect.h.min(work.h.max(1));
            if (fit_w, fit_h) != (window.rect.w, window.rect.h) {
                // Отказ — окно программы или нехватка памяти: окно остаётся
                // своего размера и только прижимается ниже.
                let _ = window.resize(fit_w, fit_h);
            }
            let max_x = (work.right() - window.rect.w as i32).max(work.x);
            let max_y = (work.bottom() - window.rect.h as i32).max(work.y);
            window.rect.x = window.rect.x.clamp(work.x, max_x);
            window.rect.y = window.rect.y.clamp(work.y, max_y);
        }
        self.restyle(status);
        self.damage_count = 0;
        self.damage_overflow = true;
    }

    /// Перерисовать весь экран — то, что делает пункт «Refresh».
    pub fn repaint_all(&mut self) {
        let all = self.union;
        self.mark(all);
    }

    /// Пометить область меню как изменившуюся — при закрытии его надо стереть.
    pub fn mark_menu_area(&mut self) {
        if let Some(menu) = self.menu.as_ref() {
            // Вместе с подменю классического «Пуска» (22.11).
            let rect = menu.full_rect();
            self.mark_layer(rect);
        }
    }

    /// Откладывать ли сборку кадров — см. [`Self::deferred`].
    pub fn set_deferred(&mut self, deferred: bool) {
        self.deferred = deferred;
    }

    /// Погасить экран: чёрное поле и ни одного кадра, пока не разбудят.
    ///
    /// Заливка идёт **мимо** обычного вывода, прямо по экрану: собирать кадр
    /// ради одного цвета незачем, а слои при этом остаются нетронутыми — им
    /// предстоит вернуться на экран такими же, какими были.
    ///
    /// С 03.11 на телефоне гаснет и подсветка (`arch::backlight` — яркость 0
    /// командой панели 0x51): было «чёрное окно с горящими пикселями». У ПК
    /// подсветкой ведает монитор, DPMS у нас нет — там картинка чёрная, лампа
    /// горит, как раньше.
    pub fn blank(&mut self) {
        for out in &self.outputs {
            out.screen.fill(out.screen.bounds(), Color::rgb(0, 0, 0));
            crate::display::flush_output(out.backend, out.primary, 0, 0, out.rect.w, out.rect.h);
        }
        crate::arch::backlight(false);
        if self.hw_cursor {
            let (x, y) = self.pointer.position();
            crate::display::cursor_visible(false, x, y);
        }
        self.blanked = true;
    }

    /// Погашен ли экран.
    #[must_use]
    pub const fn blanked(&self) -> bool {
        self.blanked
    }

    /// Зажечь обратно: весь экран объявляется изменившимся и собирается заново.
    pub fn unblank(&mut self) {
        self.blanked = false;
        crate::arch::backlight(true);
        if self.hw_cursor {
            let (x, y) = self.pointer.position();
            crate::display::cursor_visible(true, x, y);
            self.cursor_sent = (x, y);
        }
        self.repaint_all();
    }

    /// Собрать кадр: вывести на экран всё, что изменилось.
    pub fn present(&mut self) {
        // Погашенный экран — не то же самое, что отложенный кадр: отложенный
        // соберётся следующим витком, а этот не соберётся, пока не придёт
        // человек. Помеченное за это время не теряется — [`Self::unblank`]
        // объявляет изменившимся весь экран.
        if self.deferred || self.blanked {
            return;
        }
        self.apply_pending_size();
        // Screen recorder (22.11): the captured rectangle once, to fill its mirror.
        if super::capture::active() {
            if let Some(rect) = super::capture::take_repaint() {
                self.mark(rect);
            }
        }
        // Указатель мог сдвинуться не мышью — смена режима ставит его в
        // середину нового экрана. Курсор адаптера догоняет здесь.
        if self.hw_cursor {
            self.hand_cursor(false);
        }
        self.rebuild_under();
        // Клавиатура — до полёта: иначе на медленной машине полёт успевал
        // кончиться в том же кадре, и порядок строк журнала «клавиатура ушла» и
        // «окно долетело» зависел от скорости кадра.
        if let Some(shown) = self.sync_keyboard() {
            crate::kprintln!("  keyboard    : {}", if shown { "shown" } else { "hidden" });
        }
        self.advance_flight();
        let shade_moving = self.advance_shade();
        let taskbar_moving = self.advance_taskbar();
        // A Start popup fading in (24.11).
        let taskbar_moving = taskbar_moving | self.menu.as_mut().is_some_and(Menu::tick_anim);
        // Наплыв подсмотра и отсчёт до макетов прилипания (20.11).
        let peek_moving = self.advance_peek();
        let snap_waiting = self.advance_snap_layouts();
        let splash_moving = self.advance_splash();
        self.build_scene();
        // След пальца по клавиатуре и язык её подписей. Анимация идёт, пока
        // летит окно или тлеет след: оболочка будит стол по этому признаку.
        let mut trail_damage = Rect::EMPTY;
        let mut trail = false;
        if let Some(keyboard) = self.keyboard.as_mut() {
            keyboard.sync_lang();
            let now = crate::time::uptime_ms();
            keyboard.tick(now);
            trail_damage = keyboard.advance_trail(now);
            trail = keyboard.trail_visible() || keyboard.holding();
        }
        self.mark(trail_damage);
        // Круг «программа запускается» крутится, пока не выйдет срок: на
        // каждый кадр — его место заново.
        let now = crate::time::uptime_ms();
        let spinning = self.pointer.is_busy(now);
        if spinning {
            self.mark(self.pointer.rect());
        } else if let Some(was) = self.pointer.clear_busy() {
            self.mark(was);
        }
        super::check_icon_hold(self);
        self.sync_front();
        // Палец держат на полоске жеста — задержку считает `ui::tick`, и
        // будить его надо кадрами, как и долгое нажатие на значок.
        super::set_animating(
            self.flight.is_some() || trail || spinning || shade_moving || taskbar_moving || peek_moving || snap_waiting || splash_moving || super::icon_holding() || super::gesture_holding() || self.flip_retry,
        );
        self.collect();
        // G3: outputs fully covered by a fullscreen program window go to the
        // screen straight from its pixels; entering and leaving that mode
        // repaints the output once.
        let plan = self.direct_plan();
        self.apply_direct_plan(&plan);
        let flips = self.flip_plan(&plan);
        // Screen recorder (22.11): the pointer is in the composed frame only
        // when the compositor draws it and no output shows a window directly.
        if super::capture::active() {
            let drawn = !self.hw_cursor && !theme::is_mobile() && plan.iter().all(Option::is_none);
            super::capture::note_pointer(&self.pointer, drawn);
        }
        if !self.damage_overflow && self.damage_count == 0 {
            return;
        }
        let started = crate::time::uptime_ns();
        self.frames += 1;
        // И наружу, в атомик: среднее на кадр считает тот, кто спрашивает по
        // кабелю, а стол к тому времени уже занят (см. [`super::timing`]).
        super::note_frame();

        if self.damage_overflow {
            let all = self.union;
            super::note_full_frame();
            self.compose_planned(all, &plan, &flips);
            self.finish_flips(&plan, &flips);
            self.rects += 1;
            self.damage_overflow = false;
            self.damage_count = 0;
            let spent = crate::time::uptime_ns().wrapping_sub(started);
            super::note_frame_ns(spent);
            super::note_screen_cost(spent, 1, 1);
            return;
        }
        super::note_partial_frame(self.damage_count as u64);

        let mut points = 0u64;
        for index in 0..self.damage_count {
            let rect = self.damage[index].intersect(&self.union);
            if !rect.is_empty() {
                super::note_rect(u64::from(rect.w) * u64::from(rect.h));
                points += u64::from(rect.w) * u64::from(rect.h);
                self.compose_planned(rect, &plan, &flips);
                self.rects += 1;
            }
        }
        self.finish_flips(&plan, &flips);
        self.damage_count = 0;
        let spent = crate::time::uptime_ns().wrapping_sub(started);
        super::note_frame_ns(spent);
        // Площадь мониторов, а не охватывающего прямоугольника: щели между
        // ними не показываются и в честную долю кадра не входят.
        let screen: u64 = self.output_rects().iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum();
        super::note_screen_cost(spent, points, screen);
    }

    // -----------------------------------------------------------------------
    // G3 (21.11): fullscreen fast path
    // -----------------------------------------------------------------------

    /// Which output shows a program window straight from its pixels this
    /// frame: the topmost window on it is fullscreen, opaque, exactly the
    /// output, and nothing of the desktop lies over it (menus, toasts, the
    /// launch card, panels, thumbnails, snap, peek, flight, lock...). Only the
    /// pointer may be over it - it is patched in ([`Self::blit_direct`]).
    /// The answer is a window index per output.
    fn direct_plan(&self) -> [Option<usize>; MAX_OUTPUTS] {
        let mut plan = [None; MAX_OUTPUTS];
        if !DIRECT.load(core::sync::atomic::Ordering::Relaxed)
            || theme::is_mobile()
            || self.lock.is_some()
            || self.ending.is_some()
            || self.flight.is_some()
            || self.scene.is_some()
            || self.peek.is_some()
            || self.peek_fade.is_some()
            || self.outline.is_some()
            || self.snap.is_some()
            || self.start.is_some()
            || self.tray.is_some()
            || self.power.is_some()
            || self.thumbs.is_some()
            || self.tray_pop.is_some()
            || self.snap_pop.is_some()
            || self.identify.is_some()
            || self.menu_open()
            || self.context.as_ref().is_some_and(|menu| menu.is_open())
            || self.keyboard.as_ref().is_some_and(|keyboard| keyboard.is_visible())
            || self.shade.as_ref().is_some_and(|shade| shade.is_visible())
            || super::look::depth() != 32
        {
            return plan;
        }
        let halo = ((SHADOW_SPREAD + SHADOW_DROP) * self.scale) as i32;
        let grow = |r: Rect| Rect::new(r.x - halo, r.y - halo, r.w + halo as u32 * 2, r.h + halo as u32 * 2);
        let full = self.fullscreen_front();
        for (index, out) in self.outputs.iter().enumerate().take(MAX_OUTPUTS) {
            let o = out.rect;
            if self.toast.as_ref().is_some_and(|toast| !grow(toast.rect).intersect(&o).is_empty())
                || self.splash.layer().is_some_and(|(_, rect)| !grow(rect).intersect(&o).is_empty())
            {
                continue;
            }
            if self.panel.as_ref().is_some_and(|panel| !full && !panel.rect.intersect(&panel.area()).intersect(&o).is_empty()) {
                continue;
            }
            if self.side_panels.iter().any(|(_, panel)| !panel.rect.intersect(&o).is_empty() && !self.fullscreen_over(panel.rect)) {
                continue;
            }
            // The topmost window touching this output (with its shadow).
            let top = self
                .windows
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, window)| !window.minimized)
                .find(|(_, window)| !grow(window.rect).intersect(&o).is_empty());
            let Some((at, window)) = top else {
                continue;
            };
            if window.fullscreen && window.rect == o && !dims(window.app) && window.direct_pixels().is_some() {
                plan[index] = Some(at);
            }
        }
        plan
    }

    /// Switch outputs into and out of the direct mode: a window leaving it
    /// gets its surface refreshed from the pixels, and the output is
    /// repainted whole once either way.
    fn apply_direct_plan(&mut self, plan: &[Option<usize>; MAX_OUTPUTS]) {
        for index in 0..self.outputs.len().min(MAX_OUTPUTS) {
            let now = plan[index].and_then(|at| self.windows.get(at)).map(|window| window.app);
            let was = self.direct[index];
            if now == was {
                continue;
            }
            if let Some(app) = was {
                if let Some(window) = self.find(app) {
                    window.refresh_stale();
                    window.damage = Rect::EMPTY;
                }
            }
            crate::kprintln!(
                "  desktop     : output {index} {} (window '{}')",
                if now.is_some() { "shows a fullscreen window directly" } else { "composes again" },
                now.or(was).map_or_else(|| alloc::string::String::from("-"), |app| self.label(app))
            );
            self.direct[index] = now;
            // A new window (or none) - the display may flip it again.
            self.flip_block[index] = None;
            self.flip_no_cursor[index] = false;
            let rect = self.outputs[index].rect;
            self.mark(rect);
        }
    }

    /// [`Self::compose`] with the direct outputs of `plan` taken out: their
    /// part of `rect` is copied from the window pixels.
    fn compose_planned(&mut self, rect: Rect, plan: &[Option<usize>; MAX_OUTPUTS], flips: &[Option<(u64, u32)>; MAX_OUTPUTS]) {
        if plan.iter().all(Option::is_none) {
            self.compose(rect);
            return;
        }
        for index in 0..self.outputs.len().min(MAX_OUTPUTS) {
            let part = rect.intersect(&self.outputs[index].rect);
            if part.is_empty() {
                continue;
            }
            match plan[index] {
                // The display shows the client buffer itself: nothing to copy
                // (virtio-gpu transfers the changed part after the flip).
                Some(window) if flips[index].is_some() => {
                    let o = self.outputs[index].rect;
                    let local = Rect::new(part.x - o.x, part.y - o.y, part.w, part.h);
                    self.flip_touched[index] = Some(self.flip_touched[index].map_or(local, |r| r.union(&local)));
                    // Screen recorder (22.11): the frame is the client buffer.
                    if super::capture::active() {
                        if let Some(shown) = self.windows.get(window) {
                            if let Some(pixels) = shown.direct_pixels() {
                                super::capture::put(part, pixels, part.x - shown.rect.x, part.y - shown.rect.y);
                            }
                        }
                    }
                }
                Some(window) => self.blit_direct(index, window, part),
                None => self.compose(part),
            }
        }
    }

    /// Outputs whose display scans the client buffer out directly (NVIDIA
    /// page flip, virtio-gpu stand-in; opt-in `wldirect flip on`): a direct
    /// output of a display that can flip, a contiguous client buffer, and no
    /// pointer over it unless the display draws it ([`Self::flip_cursor_update`]).
    /// `(physical address, pitch)`. Why an output does not flip is logged
    /// once per change. An output that stops flipping gets its frame buffer
    /// repainted whole first ([`Self::finish_flips`] switches back after).
    fn flip_plan(&mut self, plan: &[Option<usize>; MAX_OUTPUTS]) -> [Option<(u64, u32)>; MAX_OUTPUTS] {
        let mut flips = [None; MAX_OUTPUTS];
        let on = crate::display::nvidia_disp::FLIP.load(core::sync::atomic::Ordering::Relaxed);
        let stopped = crate::display::nvidia_disp::FLIP_STOPPED.load(core::sync::atomic::Ordering::Relaxed);
        for index in 0..self.outputs.len().min(MAX_OUTPUTS) {
            let Some(at) = plan[index] else {
                self.flip_note[index] = None;
                continue;
            };
            let out = &self.outputs[index];
            let pointer_over = !self.hw_cursor && self.pointer.is_visible() && !self.pointer.rect().intersect(&out.rect).is_empty();
            let source = self.windows.get(at).map_or(Err("no window"), Window::scanout_source);
            let decided: Result<(u64, u32), alloc::string::String> = if !on {
                Err("flip is off (`wldirect flip on`)".into())
            } else if stopped {
                Err("flips stopped after a display error (`wldirect flip on` tries again)".into())
            } else if let Some(why) = crate::display::flip_unsupported(out.backend) {
                Err(why.into())
            } else if let Some(why) = &self.flip_block[index] {
                Err(why.clone())
            } else if pointer_over && (self.flip_no_cursor[index] || !crate::display::flip_has_cursor(out.backend)) {
                Err("the pointer is over the output and the display has no cursor for it".into())
            } else {
                source.map_err(Into::into)
            };
            match decided {
                Ok(flip) => {
                    flips[index] = Some(flip);
                    if self.flip_note[index].as_deref() != Some("") && self.flipped[index].is_some() {
                        self.flip_note[index] = Some(alloc::string::String::new());
                    }
                }
                Err(why) => {
                    if self.flip_note[index].as_deref() != Some(why.as_str()) {
                        crate::kprintln!("  desktop     : output {index} does not flip: {why}");
                        self.flip_note[index] = Some(why);
                    }
                }
            }
        }
        for index in 0..self.outputs.len().min(MAX_OUTPUTS) {
            let rect = self.outputs[index].rect;
            if self.flipped[index].is_some() && flips[index].is_none() {
                self.mark(rect);
            } else if flips[index].is_some_and(|(phys, _)| self.flipped[index] != Some(phys)) {
                // A flip to do (or to try again): the frame must run even
                // without other damage; the flipped output composes nothing.
                self.mark(Rect::new(rect.x, rect.y, 1, 1));
            }
        }
        flips
    }

    /// Point the displays at the planned buffers (or back at their frame
    /// buffers, already repainted this frame). A flip the display cannot take
    /// yet is tried again next frame; one it refuses falls back to the copy
    /// for this window (logged).
    fn finish_flips(&mut self, plan: &[Option<usize>; MAX_OUTPUTS], flips: &[Option<(u64, u32)>; MAX_OUTPUTS]) {
        self.flip_retry = false;
        for index in 0..self.outputs.len().min(MAX_OUTPUTS) {
            let backend = self.outputs[index].backend;
            let touched = self.flip_touched[index].take();
            let want = flips[index].map(|(phys, _)| phys);
            if want == self.flipped[index] {
                if let (Some(_), Some(r)) = (want, touched) {
                    crate::display::flip_refresh(backend, r.x, r.y, r.w, r.h);
                    super::note_flip();
                }
            } else {
                let pitch = flips[index].map_or(0, |(_, pitch)| pitch);
                match crate::display::flip(backend, want, pitch) {
                    Ok(()) => {
                        if want.is_some() != self.flipped[index].is_some() {
                            crate::kprintln!(
                                "  desktop     : output {index} {}",
                                if want.is_some() { "scans the fullscreen window's buffer out directly (no copy)" } else { "scans its frame buffer out again" }
                            );
                        }
                        if want.is_some() {
                            self.flip_note[index] = Some(alloc::string::String::new());
                            super::note_flip();
                        }
                        self.flipped[index] = want;
                    }
                    Err(crate::display::FlipError::Pending(_)) => self.flip_retry = true,
                    Err(crate::display::FlipError::Refused(why)) => {
                        if want.is_some() {
                            crate::kprintln!("  desktop     : output {index} does not flip: the display refused - {why}");
                            self.flip_note[index] = Some(why.clone());
                            self.flip_block[index] = Some(why);
                            if self.flipped[index].is_none() {
                                // The display shows the frame buffer: copy now.
                                if let Some(at) = plan[index] {
                                    let rect = self.outputs[index].rect;
                                    self.blit_direct(index, at, rect);
                                }
                            } else {
                                // It still shows the previous client buffer:
                                // next frame goes back to the frame buffer.
                                let rect = self.outputs[index].rect;
                                self.mark(rect);
                                self.flip_retry = true;
                            }
                        } else {
                            crate::kprintln!("  desktop     : output {index} could not scan its frame buffer out again: {why}");
                            self.flipped[index] = None;
                        }
                    }
                }
            }
            self.flip_cursor_update(index);
        }
    }

    /// The pointer over a flipped output is the display's cursor (nothing is
    /// composed there): show, move, change or hide it; off when the output
    /// stops flipping. A display without a cursor blocks the flip while the
    /// pointer is over the output ([`Self::flip_no_cursor`]).
    fn flip_cursor_update(&mut self, index: usize) {
        let backend = self.outputs[index].backend;
        let o = self.outputs[index].rect;
        if self.flipped[index].is_none() || self.hw_cursor {
            if self.flip_cursor[index].take().is_some() {
                crate::display::flip_cursor_off(backend);
            }
            return;
        }
        let shown = self.pointer.is_visible() && !self.pointer.rect().intersect(&o).is_empty();
        let (key, hot, image) = match self.flip_cursor[index] {
            // Hidden and never enabled: games hiding the pointer never touch
            // the cursor hardware.
            None if !shown => return,
            Some((key, hot, _)) if shown && key == self.pointer.image_key() => (key, hot, None),
            Some((usize::MAX, hot, _)) if !shown => (usize::MAX, hot, None),
            _ if !shown => (usize::MAX, (0, 0), Some(alloc::vec![0u32; (crate::display::FLIP_CURSOR_SIDE * crate::display::FLIP_CURSOR_SIDE) as usize])),
            _ => {
                let (image, hot) = self.pointer.image(crate::display::FLIP_CURSOR_SIDE);
                (self.pointer.image_key(), hot, Some(image))
            }
        };
        let (x, y) = self.pointer.position();
        let at = if shown { (x - hot.0 as i32 - o.x, y - hot.1 as i32 - o.y) } else { self.flip_cursor[index].map_or((0, 0), |(_, _, at)| at) };
        if image.is_none() && self.flip_cursor[index].is_some_and(|(_, _, sent)| sent == at) {
            return;
        }
        let answer = crate::display::flip_cursor(backend, image.as_deref(), at);
        if answer == Some(true) {
            self.flip_cursor[index] = Some((key, hot, at));
        } else if answer.is_none() {
            // A lock is busy: the next frame sends it again.
            self.flip_retry = true;
            if let Some(o) = self.outputs.get(index).map(|out| out.rect) {
                self.mark(Rect::new(o.x, o.y, 1, 1));
            }
        } else {
            self.flip_no_cursor[index] = true;
            if shown {
                // Compose the pointer: the next frame stops the flip.
                self.mark(o);
                self.flip_retry = true;
            }
        }
    }

    /// One copy from the window pixels into the output, the pointer patched
    /// over it through a small buffer (the only layer allowed above).
    fn blit_direct(&mut self, index: usize, at: usize, part: Rect) {
        let started = crate::time::uptime_ns();
        let (Some(out), Some(window)) = (self.outputs.get(index), self.windows.get(at)) else {
            return;
        };
        let Some(pixels) = window.direct_pixels() else {
            return;
        };
        let o = out.rect;
        let w = window.rect;
        out.screen.blit(pixels, (part.x - o.x, part.y - o.y), Rect::new(part.x - w.x, part.y - w.y, part.w, part.h));
        // Screen recorder (22.11): without the pointer patch (it is patched
        // into grabbed frames, see `capture::note_pointer`).
        if super::capture::active() {
            super::capture::put(part, pixels, part.x - w.x, part.y - w.y);
        }
        if !self.hw_cursor && self.pointer.is_visible() {
            let spot = self.pointer.rect().intersect(&part);
            if !spot.is_empty() {
                if let Some(mut patch) = Surface::new(spot.w, spot.h, Color::rgb(0, 0, 0)) {
                    patch.blit_from(pixels, (0, 0), Rect::new(spot.x - w.x, spot.y - w.y, spot.w, spot.h));
                    self.pointer.draw_at(&mut patch, -spot.x, -spot.y);
                    out.screen.blit(&patch, (spot.x - o.x, spot.y - o.y), patch.bounds());
                }
            }
        }
        // The flush is counted in the direct time (not in `note_flush`).
        crate::display::flush_output(out.backend, out.primary, part.x - o.x, part.y - o.y, part.w, part.h);
        super::note_direct(crate::time::uptime_ns().wrapping_sub(started), u64::from(part.w) * u64::from(part.h));
    }

    /// Window name for the log (G3).
    fn label(&self, app: App) -> alloc::string::String {
        self.caption_of(app).unwrap_or_else(|| alloc::string::String::from(app.title()))
    }

    /// The program changed `area` of its pixels (`None` - all): copy into
    /// the window surface, or - while the window is shown directly - only
    /// note the damage (G3).
    pub fn commit_program(&mut self, app: App, area: Option<Rect>) {
        let direct = self.direct.contains(&Some(app));
        let Some(window) = self.find(app) else {
            return;
        };
        if direct && window.direct_pixels().is_some() {
            window.defer_commit(area);
        } else if window.stale {
            window.refresh_stale();
        } else {
            window.commit(area);
        }
    }

    // -----------------------------------------------------------------------
    // G3: pointer constraints (`zwp_pointer_constraints_v1`)
    // -----------------------------------------------------------------------

    /// Ask (or with `None` drop) a lock/confinement of the pointer for a
    /// program window. It becomes active when the window has the focus, no
    /// desktop menu is open and the pointer is over it (see
    /// [`Self::update_constraint`]).
    pub fn set_constraint(&mut self, app: App, wanted: Option<(bool, Option<Rect>)>) {
        let same = self.constraint.is_some_and(|c| c.app == app);
        match wanted {
            Some((lock, region)) => {
                if let Some(c) = self.constraint.as_mut().filter(|_| same) {
                    if c.lock == lock {
                        c.region = region;
                        return;
                    }
                }
                self.end_constraint();
                self.constraint = Some(Constraint { app, lock, region, hint: None, active: false });
            }
            None if same => {
                self.end_constraint();
                self.constraint = None;
            }
            None => {}
        }
        self.update_constraint();
    }

    /// Where the pointer goes when the lock ends (program-local).
    pub fn set_constraint_hint(&mut self, app: App, x: i32, y: i32) {
        if let Some(c) = self.constraint.as_mut().filter(|c| c.app == app) {
            c.hint = Some((x, y));
        }
    }

    /// Is the constraint of `app` active (`None` - none asked)?
    #[must_use]
    pub fn constraint_active(&self, app: App) -> Option<bool> {
        self.constraint.filter(|c| c.app == app).map(|c| c.active)
    }

    /// The active constraint: the window and whether it locks.
    #[must_use]
    pub fn active_constraint(&self) -> Option<(App, bool)> {
        self.constraint.filter(|c| c.active).map(|c| (c.app, c.lock))
    }

    /// Where the pointer may be while the confinement is active (screen).
    fn confine_rect(&self) -> Option<Rect> {
        let c = self.constraint.filter(|c| c.active && !c.lock)?;
        let window = self.windows.iter().find(|window| window.app == c.app)?;
        let (ox, oy) = window.content_origin();
        let body = Rect::new(ox, oy, window.rect.w.saturating_sub((ox - window.rect.x) as u32), window.rect.h.saturating_sub((oy - window.rect.y) as u32));
        let area = match c.region {
            Some(region) => region.translate(ox, oy).intersect(&body),
            None => body,
        };
        (!area.is_empty()).then_some(area)
    }

    /// Activate or end the constraint: active only while its window is
    /// focused and in front of the desktop menus (Alt+Tab, Win, Start, a
    /// context menu or the lock screen end it); it starts when the pointer is
    /// over the window (inside the confinement region).
    pub fn update_constraint(&mut self) {
        let Some(c) = self.constraint else {
            return;
        };
        let window = self.windows.iter().find(|window| window.app == c.app && !window.minimized);
        let eligible = window.is_some()
            && self.focused_app() == Some(c.app)
            && self.lock.is_none()
            && self.start.is_none()
            && !self.menu_open()
            && !self.context.as_ref().is_some_and(|menu| menu.is_open())
            && self.thumbs.is_none()
            && self.tray_pop.is_none()
            && self.drag.is_none();
        if c.active && !eligible {
            self.end_constraint();
            return;
        }
        if c.active || !eligible {
            return;
        }
        let Some(window) = window else {
            return;
        };
        let (px, py) = self.pointer.position();
        let (ox, oy) = window.content_origin();
        let inside = window.rect.contains(px, py)
            && c.region.is_none_or(|region| region.contains(px - ox, py - oy));
        if let Some(active) = self.constraint.as_mut() {
            active.active = true;
        }
        // G5 (21.11, hw: the pointer left the TMNF window): the focused
        // window's constraint starts at once, like ClipCursor on Windows -
        // a pointer outside (on the other monitor, beside the window) is
        // brought in: into the confinement area, or to the middle of the
        // window for a lock. Before, it waited for the pointer to come over
        // the window, and until then the pointer roamed both monitors and
        // its clicks went past the game.
        if !inside {
            let area = if c.lock {
                None
            } else {
                self.confine_rect()
            };
            let target = match area {
                Some(area) => (px.clamp(area.x, area.right() - 1), py.clamp(area.y, area.bottom() - 1)),
                None => {
                    let r = self.windows.iter().find(|window| window.app == c.app).map_or(Rect::EMPTY, |w| w.rect);
                    (r.x + r.w as i32 / 2, r.y + r.h as i32 / 2)
                }
            };
            let outs = self.out_rects;
            let before = self.pointer.rect();
            if self.pointer.move_to(target.0, target.1, &outs[..self.outputs.len().min(MAX_OUTPUTS)]) && !self.hand_cursor(false) {
                self.mark(before);
                let after = self.pointer.rect();
                self.mark(after);
            }
        }
        crate::kprintln!("  desktop     : pointer {} by '{}'", if c.lock { "locked" } else { "confined" }, self.label(c.app));
        if c.lock {
            let rect = self.pointer.rect();
            if self.pointer.hide() {
                if self.hw_cursor {
                    crate::display::cursor_visible(false, px, py);
                }
                self.mark(rect);
            }
        }
    }

    /// The constraint stops being active: the pointer comes back (at the
    /// hint of a lock, if the program gave one).
    fn end_constraint(&mut self) {
        let Some(c) = self.constraint.filter(|c| c.active) else {
            return;
        };
        if let Some(active) = self.constraint.as_mut() {
            active.active = false;
        }
        crate::kprintln!("  desktop     : pointer {} by '{}'", if c.lock { "unlocked" } else { "unconfined" }, self.label(c.app));
        if !c.lock {
            return;
        }
        if let (Some((hx, hy)), Some(window)) = (c.hint, self.windows.iter().find(|window| window.app == c.app)) {
            let (ox, oy) = window.content_origin();
            let outs = self.out_rects;
            let before = self.pointer.rect();
            if self.pointer.move_to(ox + hx, oy + hy, &outs[..self.outputs.len().min(MAX_OUTPUTS)]) {
                self.mark(before);
            }
        }
        self.pointer.show();
        let (x, y) = self.pointer.position();
        if self.hw_cursor {
            crate::display::cursor_visible(true, x, y);
            self.cursor_sent = (x, y);
        }
        let rect = self.pointer.rect();
        self.mark(rect);
    }

    /// Mouse motion under a constraint (G3). `Some(delta)` - what the desktop
    /// pointer should move by instead of the device delta: zero while locked,
    /// cut to the region while confined. `None` - no active constraint.
    pub fn constrain_motion(&mut self, dx: i32, dy: i32) -> Option<(i32, i32)> {
        self.update_constraint();
        let c = self.constraint.filter(|c| c.active)?;
        if c.lock {
            return Some((0, 0));
        }
        let area = self.confine_rect()?;
        let (x, y) = self.pointer.position();
        let tx = (x + dx).clamp(area.x, area.right() - 1);
        let ty = (y + dy).clamp(area.y, area.bottom() - 1);
        Some((tx - x, ty - y))
    }

    /// Absolute pointing device (tablet): the step from its last position,
    /// in desktop pixels, and remember the new one.
    pub fn tablet_step(&mut self, x_fraction: u16, y_fraction: u16) -> (i32, i32) {
        let p = self.primary;
        let at = (p.x + scale_fraction(x_fraction, p.w), p.y + scale_fraction(y_fraction, p.h));
        let step = self.tablet_last.map_or((0, 0), |last| (at.0 - last.0, at.1 - last.1));
        self.tablet_last = Some(at);
        step
    }

    /// Собрать сцену под летящим окном, если она нужна и её ещё нет (фаза 64).
    fn build_scene(&mut self) {
        if self.flight.is_none() || self.scene.is_some() || theme::is_mobile() {
            return;
        }
        let bounds = self.union;
        let Some(mut scene) = Surface::new(bounds.w, bounds.h, theme::palette().wall_top) else {
            return;
        };
        let started = crate::time::uptime_ns();
        self.draw_lower(&mut scene, bounds, 0);
        crate::kprintln!(
            "  desktop     : scene under the flight built in {} us",
            crate::time::uptime_ns().wrapping_sub(started) / 1000
        );
        self.scene = Some(scene);
    }

    /// Нижние слои полосы: обои, строка состояния, значки, окна с тенями.
    /// Общее у кадра и у сцены под полётом — чтобы сцена не разошлась с тем,
    /// что кадр нарисовал бы сам.
    fn draw_lower(&self, back: &mut Surface, band: Rect, dy: i32) {
        let radius = super::look::corner(theme::R_WINDOW * self.scale);
        self.draw_background(back, band, dy);
        super::statusbar::draw(back, band, dy, self.primary.w, self.scale);
        self.icons.draw(back, band, dy);
        for window in self.windows.iter().filter(|window| !window.minimized && !self.hidden_by_flight(window)) {
            // Развёрнутое и полноэкранное окно — углами в углы экрана (20.11).
            let r = if window.popup || window.fullscreen || (window.is_maximized() && !theme::is_mobile()) { 0 } else { radius };
            if !window.shaped {
                self.drop_shadow(back, window.rect, band, dy, r);
            }
            self.stack_window(back, window, band, dy, r);
        }
    }

    /// Собрать прямоугольник экрана и вывести его целиком.
    ///
    /// Прямоугольник выше полосы буфера разрезается на несколько: каждая полоса
    /// собирается со всеми слоями и уходит на экран одним выводом.
    ///
    /// # Почему здесь нет короткого пути
    ///
    /// Он был: если верхнее окно закрывает прямоугольник целиком, можно вывести
    /// только его и не трогать ни фон, ни окна под ним. Выглядит бесспорно —
    /// набор текста меняет ячейку внутри окна, и это самый частый случай.
    ///
    /// На практике он давал неверное перекрытие: содержимое нижнего окна
    /// оказывалось поверх верхнего. Причину найти не удалось — трассировка
    /// показывала правильный порядок вывода, а на экране был обратный, — и
    /// оптимизация убрана целиком. Оптимизация, работающая не всегда, хуже её
    /// отсутствия: она превращает картинку в лотерею.
    fn compose(&mut self, rect: Rect) {
        let rect = rect.intersect(&self.union);
        if rect.is_empty() {
            return;
        }
        // Буфер вынимается на время сборки — иначе он занят изменяемой
        // ссылкой, пока читаются окна, значки и панель, то есть весь остальной
        // композитор. Тот же приём, что и у самого стола под замком.
        let mut back = core::mem::replace(&mut self.back, Surface::EMPTY);
        let band_h = back.height().max(1);
        let mut top = rect.y;
        while top < rect.bottom() {
            let height = band_h.min((rect.bottom() - top) as u32);
            let band = Rect::new(rect.x, top, rect.w, height);

            // Сборка и вывод меряются **порознь**, и это не любопытство.
            // Медленная сборка и медленный вывод лечатся противоположным:
            // первая — рисующим кодом, второй — числом точек, уезжающих в
            // некэшируемую память панели. Одна общая цифра не различает их
            // вовсе, а различить надо прежде, чем что-то менять.
            let t0 = crate::time::uptime_ns();
            self.compose_band(&mut back, band);
            // Глубина цвета меньше 32 бит (20.11, «Дисплей»): полоса
            // сводится к палитре перед выводом.
            super::look::quantize(&mut back, band.x, band.w, band.h, band.y);
            let t1 = crate::time::uptime_ns();
            // Буфер во всю ширину стола, поэтому по горизонтали координаты
            // совпадают со столом, и сдвигать надо только начало по
            // вертикали. Каждому монитору — его кусок полосы, в его же
            // координатах; щели между мониторами не выводятся никуда.
            for (index, out) in self.outputs.iter().enumerate() {
                let part = band.intersect(&out.rect);
                if part.is_empty() {
                    continue;
                }
                let started = crate::time::uptime_ns();
                out.screen.blit(
                    &back,
                    (part.x - out.rect.x, part.y - out.rect.y),
                    Rect::new(part.x, part.y - band.y, part.w, part.h),
                );
                // Screen recorder (22.11): the same band into its mirror.
                if super::capture::active() {
                    super::capture::put(part, &back, part.x, part.y - band.y);
                }
                // По монитору порознь (P1): сканаут во VRAM и в обычной
                // памяти стоят по-разному.
                super::note_output_blit(
                    index,
                    crate::time::uptime_ns().wrapping_sub(started),
                    u64::from(part.w) * u64::from(part.h),
                );
            }
            let t2 = crate::time::uptime_ns();
            // Наружу, в атомики: спрашивают эти числа по кабелю, пока стол
            // занят, а занятый стол вынут из-под замка целиком (см.
            // [`super::timing`]).
            super::note_band(
                t1.wrapping_sub(t0),
                t2.wrapping_sub(t1),
                u64::from(band.w) * u64::from(band.h),
            );

            top += height as i32;
        }
        self.back = back;
        // У virtio-gpu картинка — в обычной памяти, и хост узнаёт об
        // изменении только так (фаза 67a); у stdvga и ramfb это ничего не
        // делает.
        let started = crate::time::uptime_ns();
        for out in &self.outputs {
            let part = rect.intersect(&out.rect);
            if !part.is_empty() {
                crate::display::flush_output(out.backend, out.primary, part.x - out.rect.x, part.y - out.rect.y, part.w, part.h);
            }
        }
        super::note_flush(crate::time::uptime_ns().wrapping_sub(started));
    }


    /// Сложить все слои одной полосы в буфер.
    ///
    /// Порядок — снизу вверх, и он же делает обрезку: значок, попавший под
    /// окно, просто перекрывается им в буфере. Пока слои шли прямо на экран,
    /// каждый рисующий обязан был обрезать себя сам по прямоугольнику
    /// изменений — иначе значок, задетый краем, ложился поверх закрывающего его
    /// окна.
    ///
    /// Тень слоя рисуется **перед** самим слоем и по тем же правилам порядка:
    /// тень окна ложится на всё, что ниже него, и не задевает то, что выше.
    fn compose_band(&self, back: &mut Surface, band: Rect) {
        let dy = -band.y;
        // «Завершение работы» (24.11) closes everything on every monitor.
        if let Some(screen) = self.ending.as_ref() {
            let outputs: Vec<Rect> = self.outputs.iter().map(|out| out.rect).collect();
            screen.compose(back, band, dy, &outputs);
            return;
        }
        // The screensaver (22.11) closes everything, the pointer too.
        if let Some(saver) = self.saver.as_ref() {
            saver.compose(back, band, dy);
            return;
        }
        // Экран блокировки закрывает всё; поверх — только указатель.
        if let Some((_, picture)) = self.lock.as_ref() {
            // Картинка — размером с основной монитор; остальные мониторы на
            // это время чёрные.
            let p = self.primary;
            if self.outputs.len() > 1 {
                back.fill(Rect::new(band.x, band.y + dy, band.w, band.h), Color::rgb(0, 0, 0));
            }
            let part = band.intersect(&p);
            if !part.is_empty() {
                back.blit_from(picture, (part.x, part.y + dy), part.translate(-p.x, -p.y));
            }
            if !theme::is_mobile() && !self.hw_cursor {
                self.pointer.draw(back, dy);
            }
            return;
        }
        let radius = super::look::corner(theme::R_WINDOW * self.scale);
        // Разложение сборки по слоям. Общее число уже сказало, что виновата
        // сборка, а не вывод; теперь надо знать, какой её кусок, — иначе
        // правка снова окажется догадкой.
        // Нижние слои пропускаются, когда полосу целиком накрывает окно: они
        // всё равно будут затёрты им же через несколько строк.
        //
        // Это измерено, а не придумано: с «Параметрами» во весь экран обои
        // стоили 4,9 мс, а значки 11,2 мс из 22 мс кадра — и всё это ложилось
        // под окно. Шестнадцать миллисекунд из двадцати двух уходили в мусор.
        //
        // Отсечение сделано **не** тем способом, что когда-то был убран. Тогда
        // короткий путь пропускал вывод самих окон, и порядок их наложения
        // ломался (см. комментарий к [`Self::compose`]). Здесь порядок слоёв не
        // меняется вовсе: окна выводятся ровно как раньше, отменяется только
        // рисование того, что под ними.
        let covered = self.band_covered(band);
        let t_start = crate::time::uptime_ns();
        // Летит окно — всё, что под ним, одним копированием из сцены (фаза 64).
        let scene = match self.scene.as_ref().filter(|_| self.flight.is_some()) {
            Some(scene) => {
                back.blit_from(scene, (band.x, band.y + dy), band);
                true
            }
            None => false,
        };
        let covered = covered || scene;
        // Подсмотр окна из миниатюр (20.11): всё, что под ним, - размытая
        // приглушённая подложка, собранная один раз при наведении.
        // Наплыв (20.11): пока подложка не в полную силу, стол рисуется как
        // обычно, а она ложится поверх окон с этой силой.
        let peek_alpha = self.peek_alpha();
        let peeking = match self.peek_back.as_ref().filter(|_| !scene && self.peek.is_some() && peek_alpha >= 255) {
            Some(small) => {
                self.draw_peek_back(back, band, dy, small, 255);
                true
            }
            None => false,
        };
        let peek_blend = if !scene && !peeking && peek_alpha > 0 { self.peek_back.as_ref() } else { None };
        let covered = covered || peeking;
        // Телефон: всё, что под окнами, — одним копированием из готовой картинки.
        let cached = match self.under.as_ref() {
            // Во время полёта — и устаревшая: пересобирать её в кадре полёта
            // нельзя (рывок), а треть секунды прежнего списка не видно.
            Some(under) if (self.under_valid || self.flight.is_some()) && theme::is_mobile() => {
                if !covered {
                    back.blit_from(under, (band.x, band.y + dy), band);
                }
                true
            }
            _ => false,
        };
        if !covered && !cached {
            self.draw_background(back, band, dy);
        }
        let t_wall = crate::time::uptime_ns();
        // Строка состояния — часть стола и лежит под окнами: она рисуется сразу
        // после обоев, до значков. На настольной машине не рисуется вовсе (см.
        // [`super::statusbar`]).
        if !covered && !cached {
            super::statusbar::draw(back, band, dy, self.primary.w, self.scale);
            if theme::is_mobile() {
                self.draw_home(back, band, dy);
            }
            self.icons.draw(back, band, dy);
        }
        let t_icons = crate::time::uptime_ns();
        let mut shadow_ns = 0u64;
        // Окна уже в сцене, если она есть.
        for window in self.windows.iter().filter(|window| !scene && !peeking && !window.minimized && !self.hidden_by_flight(window)) {
            let t0 = crate::time::uptime_ns();
            // Углы всплывающего окна не скругляются: меню рисует свою рамку
            // прямоугольником, и срезанный угол вырезал бы её.
            // Развёрнутое и полноэкранное окно — углами в углы экрана (20.11).
            let r = if window.popup || window.fullscreen || (window.is_maximized() && !theme::is_mobile()) { 0 } else { radius };
            // Диалог поверх всего — под ним стол затемнён на 40 % (макет 18).
            if dims(window.app) {
                draw::blend_rect(back, Rect::new(band.x, band.y + dy, band.w, band.h), Color::rgb(0, 0, 0), 102);
            }
            // Куда прилипнет тащимое окно (20.11) — под ним, над остальными.
            if let Some((_, preview)) = self.snap.filter(|_| self.drag == Some(window.app)) {
                self.draw_snap(back, band, dy, preview);
            }
            if !window.shaped {
                self.drop_shadow(back, window.rect, band, dy, r);
            }
            shadow_ns += crate::time::uptime_ns().wrapping_sub(t0);
            self.stack_window(back, window, band, dy, r);
        }
        if let Some(small) = peek_blend {
            self.draw_peek_back(back, band, dy, small, peek_alpha);
        }
        if let Some(window) = self.peek.filter(|_| !scene).and_then(|app| self.windows.iter().find(|window| window.app == app)) {
            if !window.rect.intersect(&band).is_empty() {
                if !window.shaped {
                    self.drop_shadow(back, window.rect, band, dy, radius);
                }
                self.stack_window(back, window, band, dy, radius);
            }
        }
        let t_windows = crate::time::uptime_ns();
        // Клавиатура — над окнами и вместо дока: в макете окно с клавиатурой
        // доходит до низа экрана, и док под ней не виден вовсе.
        let keyboard_up = match self.keyboard.as_ref() {
            Some(keyboard) if keyboard.is_visible() => {
                if !keyboard.rect.intersect(&band).is_empty() {
                    let r = mini_ui::paint::Ctx::scaled(self.scale).px(34);
                    self.drop_shadow(back, keyboard.rect, band, dy, r);
                    self.stack(back, keyboard.surface(), keyboard.rect, band, dy, r);
                }
                if keyboard.trail_visible() {
                    keyboard.draw_trail(back, band, dy, crate::time::uptime_ms());
                }
                true
            }
            _ => false,
        };
        // Программа на экране телефона — вместо дока полоска жеста (04.11):
        // светлая пилюля у самого низа, под клавиатурой, если та открыта.
        let front = self.app_in_front();
        if front {
            let pill = super::gesture::pill(self.primary.w, self.primary.h, self.scale);
            if !pill.intersect(&band).is_empty() {
                let ink = theme::palette().ink;
                draw::rounded(back, pill.translate(0, dy), pill.h / 2, ink, 220);
            }
        }
        // Под окном во весь экран, стоящим впереди, панели нет (20.11).
        let full = self.fullscreen_front();
        // Панели остальных мониторов (20.11) — каждая только на своём.
        for (_, panel) in self.side_panels.iter().filter(|_| !keyboard_up) {
            let clip = band.intersect(&panel.area());
            if !panel.rect.intersect(&clip).is_empty() && !self.fullscreen_over(panel.rect) {
                let r = panel.corner();
                if panel.rect.intersect(&panel.area()) == panel.rect {
                    self.drop_shadow(back, panel.rect, clip, dy, r);
                }
                self.stack(back, panel.surface(), panel.rect, clip, dy, r);
            }
        }
        if let Some(panel) = self.panel.as_ref().filter(|_| !keyboard_up && !front && !full) {
            // Полосы, которых док не задевает, отсекаются здесь, до всякой
            // работы. `stack` проверяет то же самое, но уже внутри — а нам
            // нужно знать, сколько раз он вызывается вхолостую: счётчик
            // `top` растёт и при тапе в верх экрана, где дока нет вовсе.
            // Только в пределах своего монитора (20.11): спрятанный Dock у
            // стыка с соседним монитором уезжал на соседний.
            let clip = if theme::is_mobile() { band } else { band.intersect(&panel.area()) };
            if !panel.rect.intersect(&clip).is_empty() {
                super::note_dock_band();
                let t0 = crate::time::uptime_ns();
                // Панель ПК — во всю ширину и без скругления (72b); Dock —
                // плашка со скруглёнными углами (20.11).
                let r = if theme::is_mobile() { radius } else { panel.corner() };
                // Тень — только у плашки целиком на своём мониторе: тень
                // уезжающей рисовалась бы и на соседнем.
                if theme::is_mobile() || panel.rect.intersect(&panel.area()) == panel.rect {
                    self.drop_shadow(back, panel.rect, clip, dy, r);
                }
                self.stack(back, panel.surface(), panel.rect, clip, dy, r);
                // Док считается отдельно от прочих верхних слоёв: он попадает
                // в девять-двенадцать полос, а `top` стоит восемь миллисекунд,
                // и одно с другим не сходится. Пока время не разведено, любое
                // объяснение — догадка.
                super::note_dock_ns(crate::time::uptime_ns().wrapping_sub(t0));
            }
        }
        // Летящее окно — над доком: оно садится в стопку, и стопка не должна
        // его перекрывать.
        if let Some(flight) = self.flight.as_ref() {
            let closing = self.closing.as_ref().filter(|_| flight.motion == Motion::Zoom && !flight.restore);
            if let Some(window) = closing.or_else(|| self.windows.iter().find(|window| window.app == flight.app)) {
                let to = self.flight_target(flight);
                let t = flight.phase(crate::time::uptime_ns());
                flight.draw(back, window.surface(), to, t, band, dy);
            }
        }
        if let Some(sheet) = self.start.as_ref() {
            let r = mini_ui::paint::Ctx::scaled(self.scale).px(34);
            self.drop_shadow(back, sheet.rect, band, dy, r);
            self.stack(back, sheet.surface(), sheet.rect, band, dy, r);
        }
        if let Some(sheet) = self.tray.as_ref() {
            let r = mini_ui::paint::Ctx::scaled(self.scale).px(30);
            self.drop_shadow(back, sheet.rect, band, dy, r);
            self.stack(back, sheet.surface(), sheet.rect, band, dy, r);
        }
        if let Some(sheet) = self.power.as_ref() {
            let r = mini_ui::paint::Ctx::scaled(self.scale).px(30);
            self.drop_shadow(back, sheet.rect, band, dy, r);
            self.stack(back, sheet.surface(), sheet.rect, band, dy, r);
        }
        if let Some(menu) = self.menu.as_ref() {
            if menu.is_open() {
                self.drop_shadow(back, menu.rect, band, dy, radius);
                self.stack(back, menu.surface(), menu.rect, band, dy, radius);
                // Подменю классического «Пуска» (22.11) — без углов и теней.
                for (surface, rect) in menu.layers() {
                    self.stack(back, surface, rect, band, dy, 0);
                }
            }
        }
        if let Some(toast) = self.toast.as_ref() {
            let r = theme::R_WINDOW * self.scale;
            self.drop_shadow(back, toast.rect, band, dy, r);
            self.stack(back, toast.surface(), toast.rect, band, dy, r);
        }
        // «Определить» (24.11): the number plate of every monitor, over
        // windows and the panel.
        if let Some(identify) = self.identify.as_ref() {
            let r = theme::R_WINDOW * self.scale;
            for (rect, surface) in &identify.plates {
                self.drop_shadow(back, *rect, band, dy, r);
                self.stack(back, surface, *rect, band, dy, r);
            }
        }
        if let Some((surface, rect)) = self.splash.layer() {
            let r = theme::R_WINDOW * self.scale;
            self.drop_shadow(back, rect, band, dy, r);
            self.stack(back, surface, rect, band, dy, r);
        }
        if let Some(pop) = self.tray_pop.as_ref() {
            let r = theme::R_WINDOW * self.scale;
            self.drop_shadow(back, pop.rect, band, dy, r);
            self.stack(back, pop.surface(), pop.rect, band, dy, r);
        }
        if let Some(thumbs) = self.thumbs.as_ref() {
            let halo = self.halo() as i32;
            let grown = Rect::new(thumbs.rect.x - halo, thumbs.rect.y - halo, thumbs.rect.w + halo as u32 * 2, thumbs.rect.h + halo as u32 * 2);
            if !grown.intersect(&band).is_empty() {
                self.draw_thumbs(back, band, dy, thumbs);
            }
        }
        // Макеты прилипания (20.11) — над миниатюрами, под меню стола.
        if let Some(pop) = self.snap_pop.as_ref() {
            let r = theme::R_WINDOW * self.scale;
            self.drop_shadow(back, pop.rect, band, dy, r);
            pop.draw(back, band, dy, self.scale);
        }
        if let Some(menu) = self.context.as_ref() {
            if menu.is_open() {
                let r = super::context::MENU_RADIUS * self.scale;
                self.drop_shadow(back, menu.rect, band, dy, r);
                self.stack(back, menu.surface(), menu.rect, band, dy, r);
                if let Some((surface, rect)) = menu.sub_layer() {
                    self.drop_shadow(back, rect, band, dy, r);
                    self.stack(back, surface, rect, band, dy, r);
                }
            }
        }
        // Шторка — поверх всего, включая док: она и открывается затем, чтобы
        // закрыть собой то, что под ней.
        if let Some(shade) = self.shade.as_ref() {
            // Там, где она сейчас на пути выезда (`Shade::advance`).
            let rect = shade.shown();
            if shade.is_visible() && !rect.is_empty() {
                // Скругление — как у самой шторки (макет, экран 02).
                let r = mini_ui::paint::Ctx::scaled(self.scale).px(38);
                self.drop_shadow(back, rect, band, dy, r);
                self.stack(back, shade.surface(), rect, band, dy, r);
            }
        }
        // Контур перетаскивания (20.11) — над всеми слоями, под указателем.
        if let Some(ghost) = self.outline {
            super::look::draw_outline(back, band, dy, ghost, self.outline_width());
        }
        // Курсор — последним и без проверки пересечения: он мал, а обрезка
        // однобитной картинки уже сделана внутри `draw_bitmap`. Проверка
        // «попадает ли он в полосу» стоила бы больше, чем экономила.
        //
        // На телефоне он не рисуется вовсе. Стрелка показывает, куда попадёт
        // следующее нажатие, — а палец уже стоит там, куда попадёт: он сам
        // виден человеку, и второй указатель на то же место означает, что
        // система показывает ему его собственный палец с опозданием на кадр.
        if !theme::is_mobile() && !self.hw_cursor {
            self.pointer.draw(back, dy);
        }
        let t_end = crate::time::uptime_ns();
        super::note_layers(
            t_wall.wrapping_sub(t_start),
            t_icons.wrapping_sub(t_wall),
            t_windows.wrapping_sub(t_icons),
            shadow_ns,
            t_end.wrapping_sub(t_windows),
        );
    }

    /// Фон для телефона: градиент по строкам и разметка, без световых пятен.
    ///
    /// # Почему он другой
    ///
    /// Не потому, что на телефоне некрасиво, а потому, что там он не виден.
    /// Окно на этом экране занимает его целиком, и весь счёт трёх эллипсов
    /// уходит под окно. Сам счёт недёшев: три умножения и до трёх смешиваний на
    /// каждую из миллиона с лишним точек, и всё это на каждом кадре.
    ///
    /// Здесь на точку остаётся одно обращение к таблице. Цвет строки считается
    /// один раз на строку, а разметка ставится там же, где и на столе, — она
    /// нужна и здесь: на однородной заливке движение не читается.
    fn draw_background_flat(&self, back: &mut Surface, rect: Rect, dy: i32) {
        let p = theme::palette();
        let height = self.primary.h.max(1) as i32;

        // Готовые цвета основы: двести пятьдесят шесть оттенков перехода, и
        // цвет точки — это выбор из массива, а не смешивание.
        let mut base = [0u32; 256];
        for (weight, slot) in base.iter_mut().enumerate() {
            *slot = p.wall_top.mix(p.wall_bottom, weight as u8).pixel();
        }

        for y in rect.y.max(0)..rect.bottom() {
            // Вес перехода — доля высоты экрана, посчитанная один раз на строку.
            let weight = (y.clamp(0, height) * 255 / height) as usize;
            let pixel = base[weight.min(255)];
            let target = (y + dy) as u32;
            let from = rect.x.max(0) as usize;
            let to = (rect.right().max(0) as usize).min(back.width() as usize);
            let row = back.row_mut(target);
            if from >= to || row.len() < to {
                continue;
            }
            row[from..to].fill(pixel);
        }

        // Разметка — тем же способом, что у настольного фона: редкие точки,
        // по которым глаз видит, что окно поехало.
        let step = DOT_STEP * self.scale;
        let size = DOT_SIZE * self.scale;
        let mut y = align_up(rect.y, step);
        while y < rect.bottom() {
            let mut x = align_up(rect.x, step);
            while x < rect.right() {
                draw::blend_rect(
                    back,
                    Rect::new(x, y + dy, size, size),
                    p.wall_dot.color,
                    p.wall_dot.alpha,
                );
                x += step as i32;
            }
            y += step as i32;
        }
    }

    /// Мягкая тень под слоем.
    ///
    /// Она не украшение: на светлой теме однопиксельная обводка панели по
    /// светлым обоям не видна вовсе, и без тени панель читается как полоса,
    /// нарисованная прямо на фоне, а не как лежащая на нём плашка.
    fn drop_shadow(&self, back: &mut Surface, placed: Rect, band: Rect, dy: i32, radius: u32) {
        // На телефоне тени нет, и это измерено, а не выбрано на глаз. Кадр там
        // стоил 114 мс, из них 37 мс — тень окна и ещё десятки — тени дока,
        // меню и шторки: расстояние до скруглённого прямоугольника считается
        // на каждую точку рамки в 36 точек по всему периметру слоя, и заново
        // для каждой из полос, на которые режется кадр.
        //
        // Отказаться можно без потери: тень заведена, чтобы плавающая плашка
        // не сливалась со светлыми обоями настольной темы. На телефоне слои
        // занимают экран целиком и лежат на тёмном фоне — отделять нечего.
        // Тени выключены в «Оформлении» (20.11).
        if theme::is_mobile() || !super::look::on(super::look::Flag::Shadows) {
            return;
        }
        let spread = SHADOW_SPREAD * self.scale;
        let drop = SHADOW_DROP * self.scale;
        if !Self::touches(placed.translate(0, drop as i32), band, spread) {
            return;
        }
        let ink = theme::palette().shadow;
        // Фигура тени — сам слой, опущенный на `drop` и обрезанный снизу по
        // его краю, а с боков сжатый на треть сдвига (20.11). Прежде тенью был
        // слой, просто сдвинутый вниз, а [`draw::shadow`] под фигурой тени не
        // кладёт: под нижним краем окна оставалась полоса чистых обоев
        // высотой в `drop` — та «прозрачная рамка» снизу, с тенью уже под ней.
        // Теперь тень начинается прямо от нижнего края и гаснет вниз на весь
        // `spread`, по бокам — чуть мягче, сверху — едва заметна.
        let side = (drop / 3).min(placed.w / 4);
        let shape = Rect::new(
            placed.x + side as i32,
            placed.y + drop as i32 + dy,
            placed.w.saturating_sub(side * 2),
            placed.h.saturating_sub(drop).max(1),
        );
        draw::shadow(back, shape, radius, spread, ink.color, ink.alpha);
    }

    /// Накрыто ли всё, что в этой полосе, каким-нибудь одним окном.
    ///
    /// Строго: окно должно быть видимым, непрозрачным и покрывать полосу по
    /// обеим сторонам **с запасом на скруглённые углы**. Углы срезает
    /// композитор при выводе ([`Self::stack`]), и в полосе, куда попал угол,
    /// сквозь него видно то, что лежит ниже, — такую полосу накрытой считать
    /// нельзя.
    ///
    /// Достаточно одного окна: два окна, вместе закрывающих полосу, но по
    /// отдельности — нет, здесь не рассматриваются. Случай редкий, а проверка
    /// на объединение прямоугольников стоила бы дороже, чем экономит.
    fn band_covered(&self, band: Rect) -> bool {
        // Радиус берётся полный, а [`Self::stack`] режет углы по
        // `min(radius, w/2, h/2)` — то есть настоящая угловая зона **не больше**
        // этой. Ошибка здесь возможна только в сторону «посчитали полосу
        // незакрытой, хотя она закрыта», и это лишняя отрисовка, а не дырка в
        // картинке. Обратная ошибка стоила бы угла, сквозь который видно
        // мусор, и такой запас брать нельзя.
        let radius = (theme::R_WINDOW * self.scale) as i32;
        // Окно с прозрачными точками полосу не накрывает: сквозь него видно.
        self.windows.iter().filter(|window| !window.minimized && !window.translucent && !self.hidden_by_flight(window)).any(|window| {
            let rect = window.rect;
            // Полоса целиком внутри окна по горизонтали и по вертикали, и при
            // этом не задевает ни верхний, ни нижний угол.
            rect.x <= band.x
                && rect.right() >= band.right()
                && rect.y + radius <= band.y
                && rect.bottom() - radius >= band.bottom()
        })
    }

    /// Задевает ли слой полосу с учётом того, на сколько его тень выходит наружу.
    fn touches(placed: Rect, band: Rect, margin: u32) -> bool {
        let grown = Rect::new(
            placed.x - margin as i32,
            placed.y - margin as i32,
            placed.w + margin * 2,
            placed.h + margin * 2,
        );
        !grown.intersect(&band).is_empty()
    }

    /// Положить окно в полосу: непрозрачное — со срезанными углами,
    /// прозрачное — смешав с тем, что под ним (углы и тень у него свои).
    fn stack_window(&self, back: &mut Surface, window: &Window, band: Rect, dy: i32, radius: u32) {
        if !window.translucent {
            self.stack(back, window.surface(), window.rect, band, dy, radius);
            return;
        }
        let placed = window.rect;
        let overlap = placed.intersect(&band);
        if overlap.is_empty() {
            return;
        }
        let src = Rect::new(overlap.x - placed.x, overlap.y - placed.y, overlap.w, overlap.h);
        back.blend_from(window.surface(), (overlap.x, overlap.y + dy), src);
    }

    /// Положить в полосу ту часть слоя, которая в неё попадает, срезав углы.
    ///
    /// Скругление живёт здесь, а не в самом слое, по одной причине: угол — это
    /// место, где сквозь окно видны обои, а обои к этому моменту уже лежат в
    /// буфере. Слой рисует себя прямоугольником и о срезе не знает.
    ///
    /// Строки, проходящие мимо углов, копируются целиком, как и раньше: срез
    /// касается четырёх квадратов со стороной в радиус, то есть восьмисот
    /// точек на окно любого размера.
    fn stack(
        &self,
        back: &mut Surface,
        surface: &Surface,
        placed: Rect,
        band: Rect,
        dy: i32,
        radius: u32,
    ) {
        let overlap = placed.intersect(&band);
        if overlap.is_empty() {
            return;
        }
        let radius = radius.min(placed.w / 2).min(placed.h / 2) as i32;
        for row in 0..overlap.h {
            let screen_y = overlap.y + row as i32;
            let local_y = screen_y - placed.y;
            // Насколько строка углублена в угловую зону сверху или снизу.
            // `None` — строка проходит мимо углов и копируется целиком.
            let corner_y = if local_y < radius {
                Some(radius - local_y)
            } else if local_y >= placed.h as i32 - radius {
                Some(local_y - (placed.h as i32 - radius) + 1)
            } else {
                None
            };
            let Some(depth_y) = corner_y else {
                let src = Rect::new(overlap.x - placed.x, local_y, overlap.w, 1);
                back.blit_from(surface, (overlap.x, screen_y + dy), src);
                continue;
            };
            // Середина строки углов не касается и копируется одним куском, как
            // и строки мимо угловой зоны. Поточечный цикл остаётся только на
            // двух квадратах по краям.
            //
            // Это измерено, а не выведено из общих соображений: док попадает в
            // пять-двенадцать полос, и на нём одном уходило шесть миллисекунд
            // из шести с небольшим, которые стоили все верхние слои вместе. У
            // дока скругление 26 при масштабе 2 — сотня строк, и в каждой
            // прежний цикл шёл по всем 720 точкам, хотя угловых из них 52 с
            // каждого края, а для остальных считался квадратный корень с
            // заведомо известным ответом.
            let left_end = (placed.x + radius - overlap.x).clamp(0, overlap.w as i32);
            let right_start =
                (placed.right() - radius - overlap.x).clamp(left_end, overlap.w as i32);
            if right_start > left_end {
                let src = Rect::new(
                    overlap.x - placed.x + left_end,
                    local_y,
                    (right_start - left_end) as u32,
                    1,
                );
                back.blit_from(surface, (overlap.x + left_end, screen_y + dy), src);
            }
            for column in (0..left_end as u32).chain(right_start as u32..overlap.w) {
                let screen_x = overlap.x + column as i32;
                let local_x = screen_x - placed.x;
                let depth_x = if local_x < radius {
                    radius - local_x
                } else if local_x >= placed.w as i32 - radius {
                    local_x - (placed.w as i32 - radius) + 1
                } else {
                    0
                };
                let pixel = surface.get(local_x as u32, local_y as u32);
                let target = (screen_x as u32, (screen_y + dy) as u32);
                if depth_x == 0 {
                    back.put(target.0, target.1, pixel);
                    continue;
                }
                let coverage = corner_coverage(depth_x, depth_y, radius);
                if coverage == 0 {
                    continue;
                }
                let under = Color::from_pixel(back.get(target.0, target.1));
                let over = Color::from_pixel(pixel);
                back.put(target.0, target.1, under.mix(over, coverage).pixel());
            }
        }
    }

    /// Обои рабочего стола.
    ///
    /// # Почему всё считается, а не хранится картинкой
    ///
    /// Картинка размером с экран — четыре мегабайта при куче в шестнадцать.
    /// Здесь же цвет любой точки — функция от её координат.
    ///
    /// # Из чего они состоят
    ///
    /// Эллиптическое световое пятно у верхнего края, два размытых цветных круга
    /// по противоположным углам и редкая сетка точек. Это не украшение: на
    /// однородной заливке перетаскиваемое окно кажется стоящим на месте, и
    /// глазу нужна опора, чтобы увидеть движение.
    ///
    /// # Почему считается именно так
    ///
    /// Стол перерисовывает фон на каждом переезде окна, а машина под ним —
    /// эмулятор без ускорения, где деление стоит десятки тактов. Первая версия
    /// делала на точку шесть делений и корень, и картинка отставала от мыши на
    /// секунду: окно на снимке стояло там, откуда его уже утащили.
    ///
    /// Поэтому здесь нет ни одного деления в цикле по точкам. Полуоси
    /// превращены в множители один раз, вклад строки посчитан один раз на
    /// строку, корень заменён таблицей на тысячу значений, а цвет основы —
    /// таблицей на двести пятьдесят шесть готовых точек. В цикле остаются
    /// умножение, сдвиг и обращение к таблице.
    fn draw_background(&self, back: &mut Surface, rect: Rect, dy: i32) {
        // На телефоне фон считается дешёвым способом, и это не упрощение ради
        // упрощения. Три световых пятна стоят на точку трёх умножений и до трёх
        // смешиваний; экран 720×1600 — это миллион с лишним точек, а окно на
        // телефоне занимает его целиком, то есть весь этот счёт уходит под
        // окно, где его никто не увидит. Глазом это выглядит как окно,
        // проявляющееся сверху вниз.
        if theme::is_mobile() {
            self.draw_background_flat(back, rect, dy);
            return;
        }
        // Обои у каждого монитора свои, во весь монитор: картинка, растянутая
        // на весь охватывающий прямоугольник, разрезала бы световые пятна по
        // стыку мониторов и сдвигала бы их при каждой смене раскладки.
        for out in self.output_rects() {
            let part = rect.intersect(out);
            if !part.is_empty() {
                self.draw_wall_on(back, *out, part, dy);
            }
        }
    }

    /// Обои одного монитора: `out` — его место в координатах `back` (на столе
    /// для кадра, (0, 0, w, h) для картинки экрана блокировки), `part` — что
    /// нарисовать, в тех же координатах.
    fn draw_wall_on(&self, back: &mut Surface, out: Rect, part: Rect, dy: i32) {
        let p = theme::palette();
        // Всё, от чего обои зависят: другой размер, тема или выбранные в
        // «Параметрах» обои — другая картинка.
        let key = WallKey {
            width: out.w,
            height: out.h,
            dark: theme::is_dark(),
            top: p.wall_top.pixel(),
            bottom: p.wall_bottom.pixel(),
            pic: super::wallpaper::generation(),
        };
        {
            let mut walls = self.wall.borrow_mut();
            if !walls.iter().any(|(_, made)| *made == key) {
                // Картинки прежней темы и обоев больше не понадобятся; других
                // размеров — нужны другим мониторам.
                walls.retain(|(_, made)| made.dark == key.dark && made.top == key.top && made.bottom == key.bottom && made.pic == key.pic);
                if let Some(mut picture) = Surface::new(key.width, key.height, p.wall_top) {
                    // Фон (22.11): картинка, цвет или FossAi — `wallpaper`.
                    let drawn = super::wallpaper::paint(&mut picture);
                    if !drawn {
                        let whole = Rect::new(0, 0, key.width, key.height);
                        self.draw_wallpaper(&mut picture, whole, 0, 0, (key.width, key.height));
                    }
                    walls.push((picture, key));
                }
            }
            if let Some((picture, _)) = walls.iter().find(|(_, made)| *made == key) {
                back.blit_from(
                    picture,
                    (part.x, part.y + dy),
                    Rect::new(part.x - out.x, part.y - out.y, part.w, part.h),
                );
                return;
            }
        }
        // Памяти под готовые обои не нашлось — считаем по точкам, как раньше,
        // в координатах монитора.
        let local = Rect::new(part.x - out.x, part.y - out.y, part.w, part.h);
        self.draw_wallpaper(back, local, out.x, dy + out.y, (out.w, out.h));
    }

    /// Обои настольного стола по точкам: см. [`Self::draw_background`].
    ///
    /// `rect` — в координатах монитора размером `size`; в `back` точка
    /// `(x, y)` монитора ложится в `(x + dx, y + dy)`.
    fn draw_wallpaper(&self, back: &mut Surface, rect: Rect, dx: i32, dy: i32, size: (u32, u32)) {
        let p = theme::palette();
        let width = size.0.max(1) as i32;
        let height = size.1.max(1) as i32;

        // Числа повторяют макет: эллипс шириной в три четверти экрана с центром
        // на трети ширины у самого верха, и два круга по противоположным углам.
        let glow = Ellipse::new(width * 3 / 10, 0, width * 3 / 4, height * 7 / 10);
        let cool = Ellipse::new(-width / 12, -height / 8, width * 5 / 8, height * 7 / 10);
        let warm = Ellipse::new(
            width + width / 8,
            height + height / 6,
            width * 5 / 7,
            height * 3 / 4,
        );
        let cool_ink = Color::rgb(0x5A, 0xA2, 0xFF);
        let warm_ink = Color::rgb(0xC7, 0x7D, 0xFF);
        // Цветные круги на светлой теме приглушены вчетверо: то, что на тёмном
        // фоне читается как подсветка, на светлом становится пятном краски.
        let tint = if theme::is_dark() { 1 } else { 4 };
        let cool_peak = 92 / tint;
        let warm_peak = 74 / tint;

        // Таблица перехода: квадрат расстояния → вес смешивания. Корень берётся
        // тысячу раз вместо миллиона.
        let mut ramp = [0u8; RAMP + 1];
        let full = UNIT * 65 / 100;
        for (index, slot) in ramp.iter_mut().enumerate() {
            let d2 = (index as i64) * UNIT / (RAMP as i64);
            let distance = isqrt((d2 * UNIT) as u64) as i64;
            *slot = (distance * 255 / full).min(255) as u8;
        }
        // Готовые точки основы: их всего двести пятьдесят шесть, и без круга
        // над ними цвет точки — это одно обращение к массиву.
        let mut base = [0u32; 256];
        for (weight, slot) in base.iter_mut().enumerate() {
            *slot = p.wall_top.mix(p.wall_bottom, weight as u8).pixel();
        }

        for y in rect.y.max(0)..rect.bottom() {
            let glow_row = glow.row(y);
            let cool_row = cool.row(y);
            let warm_row = warm.row(y);
            // Строка целиком вне круга отсекается один раз, а не на каждой её
            // точке.
            let cool_here = cool_row < UNIT && cool_peak > 0;
            let warm_here = warm_row < UNIT && warm_peak > 0;
            let target = (y + dy) as u32;
            // Столбцы — в координатах монитора; в строку `back` — со сдвигом.
            let from = rect.x.max(0).max(-dx);
            let to = rect.right().min(back.width() as i32 - dx);
            if from >= to {
                continue;
            }
            let row = back.row_mut(target);
            let (at, end) = ((from + dx) as usize, (to + dx) as usize);
            if row.len() < end {
                continue;
            }
            for (offset, slot) in row[at..end].iter_mut().enumerate() {
                let x = from + offset as i32;
                let d2 = glow.at(x, glow_row).clamp(0, UNIT);
                let weight = ramp[(d2 * (RAMP as i64) / UNIT) as usize];
                let mut pixel = base[weight as usize];
                if cool_here {
                    let blue = cool.glow(x, cool_row, cool_peak);
                    if blue > 0 {
                        pixel = Color::from_pixel(pixel).mix(cool_ink, blue).pixel();
                    }
                }
                if warm_here {
                    let violet = warm.glow(x, warm_row, warm_peak);
                    if violet > 0 {
                        pixel = Color::from_pixel(pixel).mix(warm_ink, violet).pixel();
                    }
                }
                *slot = pixel;
            }
        }

        // Разметка: редкая сетка точек. Вторая опора для глаза — по ней видно,
        // что окно едет, даже когда оно едет по однотонному месту.
        let step = DOT_STEP * self.scale;
        let size = DOT_SIZE * self.scale;
        let mut y = align_up(rect.y, step);
        while y < rect.bottom() {
            let mut x = align_up(rect.x, step);
            while x < rect.right() {
                draw::blend_rect(
                    back,
                    Rect::new(x + dx, y + dy, size, size),
                    p.wall_dot.color,
                    p.wall_dot.alpha,
                );
                x += step as i32;
            }
            y += step as i32;
        }
    }

    /// Сколько кадров собрано, сколько прямоугольников выведено, сколько окон.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, usize) {
        (self.frames, self.rects, self.windows.len())
    }
}

/// Доля экрана (0..65535) → точка на нём.
///
/// Считается по **последней** точке, а не по размеру: доля 65535 обязана давать
/// правый край, а не первую точку за ним. Ошибка на единицу здесь означала бы
/// недостижимый последний столбец экрана — то, что человек замечает первым,
/// когда не может попасть в кнопку закрытия развёрнутого окна.
fn scale_fraction(fraction: u16, extent: u32) -> i32 {
    let last = extent.saturating_sub(1);
    ((u64::from(fraction) * u64::from(last)) / u64::from(u16::MAX)) as i32
}

/// Ближайшая сверху координата, кратная шагу разметки.
fn align_up(value: i32, step: u32) -> i32 {
    let step = step as i32;
    // Округление к большему работает и для отрицательных: `div_euclid`
    // округляет вниз в математическом смысле, а не в сторону нуля.
    (value + step - 1).div_euclid(step) * step
}


/// Эллипс, по которому считается пятно на обоях.
///
/// Полуоси хранятся не длинами, а множителями: вклад точки в нормированное
/// расстояние — это `d² · k`, и деление, которое иначе пришлось бы делать на
/// каждую точку кадра, сделано здесь один раз.
struct Ellipse {
    cx: i32,
    cy: i32,
    kx: i64,
    ky: i64,
}

/// Единица нормированного расстояния в неподвижной точке.
const UNIT: i64 = 4096;

/// Сколько знаков после запятой хранит множитель полуоси.
///
/// Двадцать: при полуоси в тысячу точек множитель получается около пяти
/// миллионов, и произведение с квадратом расстояния остаётся далеко внутри
/// шестидесяти четырёх бит, а точность нормированного расстояния — доли
/// единицы из четырёх тысяч.
const AXIS_BITS: u32 = 20;

/// Сколько ступеней в таблице перехода.
const RAMP: usize = 1024;

impl Ellipse {
    fn new(cx: i32, cy: i32, a: i32, b: i32) -> Self {
        let a = i64::from(a.max(1));
        let b = i64::from(b.max(1));
        Self {
            cx,
            cy,
            kx: (UNIT << AXIS_BITS) / (a * a),
            ky: (UNIT << AXIS_BITS) / (b * b),
        }
    }

    /// Вклад строки в квадрат нормированного расстояния.
    fn row(&self, y: i32) -> i64 {
        let dy = i64::from(y - self.cy);
        (dy * dy * self.ky) >> AXIS_BITS
    }

    /// Квадрат нормированного расстояния до центра: [`UNIT`] на самом эллипсе.
    fn at(&self, x: i32, row: i64) -> i64 {
        let dx = i64::from(x - self.cx);
        row + ((dx * dx * self.kx) >> AXIS_BITS)
    }

    /// Насыщенность цветного круга в этой точке, 0 за его краем.
    fn glow(&self, x: i32, row: i64, peak: i64) -> u8 {
        let d2 = self.at(x, row);
        if d2 >= UNIT {
            return 0;
        }
        let fade = UNIT - d2;
        (peak * fade / UNIT * fade / UNIT).clamp(0, 255) as u8
    }
}

/// Для чего нарисованы готовые обои — см. `Compositor::wall`.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WallKey {
    width: u32,
    height: u32,
    dark: bool,
    top: u32,
    bottom: u32,
    /// Выбор фона (`wallpaper::generation`).
    pic: u32,
}

/// Доля точки, накрытая скруглённым углом.
///
/// `depth_x` и `depth_y` — на сколько точка углублена в угловой квадрат,
/// считая от его внешнего края внутрь. Центр дуги при этом стоит в точке
/// `(radius, radius)` того же квадрата, и задача сводится к расстоянию до него.
fn corner_coverage(depth_x: i32, depth_y: i32, radius: i32) -> u8 {
    if radius <= 0 {
        return 255;
    }
    // Расстояние считается до центра точки, поэтому половина точки прибавлена
    // сразу: без неё край дуги смещается на полточки внутрь, и скругление у
    // соседних окон выглядит разной толщины.
    let dx = i64::from(depth_x) * 256 - 128;
    let dy = i64::from(depth_y) * 256 - 128;
    let distance = isqrt((dx * dx + dy * dy) as u64) as i64;
    let edge = i64::from(radius) * 256;
    if distance + 128 <= edge {
        return 255;
    }
    if distance >= edge + 128 {
        return 0;
    }
    ((edge + 128 - distance) * 255 / 256).clamp(0, 255) as u8
}

/// Целочисленный квадратный корень.
///
/// Свой, а не из ядра языка: плавающую точку в ядре трогать нельзя — на AArch64
/// её регистры в обработчике прерывания не сохраняются, и одно умножение на
/// `f32` посреди отрисовки портит состояние прерванной задачи.
fn isqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }
    let mut guess = 1u64 << ((65 - value.leading_zeros()) / 2);
    loop {
        let next = (guess + value / guess) / 2;
        if next >= guess {
            return guess;
        }
        guess = next;
    }
}

/// Части `from`, не накрытые `to`: не больше четырёх полос — сверху, снизу и по
/// бокам в пределах общих строк.
fn uncovered(from: Rect, to: Rect) -> impl Iterator<Item = Rect> {
    let overlap = from.intersect(&to);
    let mut parts = [Rect::EMPTY; 4];
    if overlap.is_empty() {
        parts[0] = from;
    } else {
        let gap = |start: i32, end: i32| (end - start).max(0) as u32;
        parts[0] = Rect::new(from.x, from.y, from.w, gap(from.y, overlap.y));
        parts[1] = Rect::new(from.x, overlap.bottom(), from.w, gap(overlap.bottom(), from.bottom()));
        parts[2] = Rect::new(from.x, overlap.y, gap(from.x, overlap.x), overlap.h);
        parts[3] = Rect::new(overlap.right(), overlap.y, gap(overlap.right(), from.right()), overlap.h);
    }
    parts.into_iter().filter(|part| !part.is_empty())
}

/// Затемняет ли окно стол под собой: диалоги, которые ждут ответа (макет 18).
fn dims(app: App) -> bool {
    matches!(app, App::OpenWith | App::Hung | App::DeleteAsk | App::Shutdown | App::Restart)
}

/// Во сколько раз подложка подсмотра меньше стола (20.11).
const PEEK_STEP: u32 = 4;
/// Высота полосы, которой собирается подложка подсмотра (кратна шагу).
const PEEK_STRIP: u32 = PEEK_STEP * 16;

/// Уменьшить `h` верхних строк `strip` в [`PEEK_STEP`] раз (среднее по
/// клетке) и положить в `out` начиная со строки `row`.
fn shrink_into(strip: &Surface, h: u32, out: &mut Surface, row: u32) {
    let step = PEEK_STEP;
    let rows = h.div_ceil(step);
    let width = strip.width();
    for sy in 0..rows {
        let target = row + sy;
        if target >= out.height() {
            break;
        }
        let y0 = sy * step;
        let y1 = (y0 + step).min(h);
        let line = out.row_mut(target);
        for (sx, slot) in line.iter_mut().enumerate() {
            let x0 = sx as u32 * step;
            if x0 >= width {
                break;
            }
            let x1 = (x0 + step).min(width);
            let (mut c0, mut c1, mut c2, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for &pixel in &strip.row(y)[x0 as usize..x1 as usize] {
                    c0 += pixel & 0xFF;
                    c1 += (pixel >> 8) & 0xFF;
                    c2 += (pixel >> 16) & 0xFF;
                    n += 1;
                }
            }
            let n = n.max(1);
            *slot = 0xFF00_0000 | ((c2 / n) << 16) | ((c1 / n) << 8) | (c0 / n);
        }
    }
}

/// Приглушить точку на четверть (каждый канал * 3/4).
const fn darken(pixel: u32) -> u32 {
    let c0 = (pixel & 0xFF) * 3 / 4;
    let c1 = ((pixel >> 8) & 0xFF) * 3 / 4;
    let c2 = ((pixel >> 16) & 0xFF) * 3 / 4;
    (c2 << 16) | (c1 << 8) | c0
}

/// Смесь двух точек: `f` из 256 - доля второй.
const fn lerp(a: u32, b: u32, f: u32) -> u32 {
    let g = 256 - f;
    let c0 = ((a & 0xFF) * g + (b & 0xFF) * f) >> 8;
    let c1 = (((a >> 8) & 0xFF) * g + ((b >> 8) & 0xFF) * f) >> 8;
    let c2 = (((a >> 16) & 0xFF) * g + ((b >> 16) & 0xFF) * f) >> 8;
    0xFF00_0000 | (c2 << 16) | (c1 << 8) | c0
}

/// Размыть поверхность ящиком радиуса `r`: сначала строки, потом столбцы
/// (скользящая сумма, края - повтором крайней точки).
fn box_blur(surface: &mut Surface, r: u32) {
    let (w, h) = (surface.width() as usize, surface.height() as usize);
    if w == 0 || h == 0 {
        return;
    }
    let r = r as i32;
    let span = (2 * r + 1) as u32;
    let mut tmp: Vec<u32> = Vec::new();
    if tmp.try_reserve_exact(w.max(h)).is_err() {
        return;
    }
    tmp.resize(w.max(h), 0);
    let blur_line = |get: &dyn Fn(usize) -> u32, len: usize, out: &mut [u32]| {
        let at = |i: i32| get(i.clamp(0, len as i32 - 1) as usize);
        let (mut s0, mut s1, mut s2) = (0u32, 0u32, 0u32);
        for i in -r..=r {
            let p = at(i);
            s0 += p & 0xFF;
            s1 += (p >> 8) & 0xFF;
            s2 += (p >> 16) & 0xFF;
        }
        for i in 0..len as i32 {
            out[i as usize] = 0xFF00_0000 | ((s2 / span) << 16) | ((s1 / span) << 8) | (s0 / span);
            let (add, sub) = (at(i + r + 1), at(i - r));
            s0 = s0 + (add & 0xFF) - (sub & 0xFF);
            s1 = s1 + ((add >> 8) & 0xFF) - ((sub >> 8) & 0xFF);
            s2 = s2 + ((add >> 16) & 0xFF) - ((sub >> 16) & 0xFF);
        }
    };
    for y in 0..h {
        {
            let row = surface.row(y as u32);
            blur_line(&|i| row[i], w, &mut tmp[..w]);
        }
        surface.row_mut(y as u32).copy_from_slice(&tmp[..w]);
    }
    for x in 0..w {
        {
            let s = &*surface;
            blur_line(&|i| s.row(i as u32)[x], h, &mut tmp[..h]);
        }
        for y in 0..h {
            surface.row_mut(y as u32)[x] = tmp[y];
        }
    }
}
