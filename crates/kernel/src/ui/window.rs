//! Окно: поверхность вместе с заголовком и содержимым.
//!
//! Украшения нарисованы в той же поверхности, что и содержимое, поэтому вывод
//! окна на экран — одна операция, а не три. Плата — перерисовка заголовка при
//! смене активного окна, то есть несколько тысяч пикселей в обычной памяти,
//! которую, в отличие от экрана, можно и читать, и переписывать сколько угодно.
//!
//! # Почему у окна нет рамки
//!
//! Рамка в две точки была единственным способом отделить окно от фона, пока
//! фон и окно отличались только яркостью заливки. Теперь окно отделяет
//! однопиксельная обводка изнутри и светлая кромка по верхнему краю — то же,
//! чем отделяет себя лист бумаги, лежащий на столе. Толстая рамка при этом
//! съедала по четыре точки с каждой стороны и делала маленькое окно ещё меньше.
//!
//! # Почему скругление рисует не окно, а композитор
//!
//! Поверхность окна прямоугольна, и прозрачности в ней нет. Скруглённый угол —
//! это место, где сквозь окно видны обои, а обои знает только композитор.
//! Поэтому окно рисует себя во всю поверхность, а углы срезаются при выводе,
//! смешиванием с уже нарисованным фоном.
//!
//! # Почему высота заголовка не зависит от фокуса
//!
//! В макете у неактивного окна полоса ниже. Сделать так значило бы, что при
//! каждом переключении окон область содержимого меняет размер, а список файлов
//! и «Параметры» пересобирают раскладку. Неактивное окно отличается тем, что
//! теряет градиент, кромку и заливку кнопок, — этого достаточно, и это стоит
//! одной полосы, а не всего окна.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::text::TextGrid;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};
use user_abi::WinEvent;

use mini_ui::paint::{self, Ctx};
use super::dialog::{Answer, Dialog, DialogView};
use super::settings::SettingsView;
use mini_ui::theme;
use crate::input::KeyCode;

/// Какая программа живёт в окне.
///
/// Перечисление, а не строка-заголовок: по нему ищут окно, по нему же меню
/// решает, что запускать, и опечатка в имени становится ошибкой компиляции.
/// Пользовательского пространства ещё нет, поэтому «программа» — это модуль
/// ядра; граница проведена там, где она пройдёт и потом.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum App {
    /// Оболочка.
    Terminal,
    /// Ещё одно окно терминала (20.11): в него унесли вкладку из полосы
    /// (как в Windows Terminal). Номер - порядковый, окна ищут по [`App`], и
    /// второму окну терминала нужно своё имя.
    TermWin(u32),
    /// Что это за система.
    About,
    /// Параметры: сведения о системе, экран, программы, обновление.
    Settings,
    /// Подтверждение выключения.
    ///
    /// Окно, а не отдельная сущность: подтверждение ведёт себя как всё
    /// остальное на столе — его видно в панели задач, его можно отодвинуть и
    /// закрыть привычным Ctrl+W, и «закрыть» здесь означает «передумал». Это
    /// дешевле собственного модального слоя и понятнее человеку, который уже
    /// знает, как закрываются окна.
    Shutdown,
    /// Подтверждение перезагрузки.
    Restart,
    /// «Как открыть этот файл?» — выбор программы для файла (02.11, макет 18).
    OpenWith,
    /// «Программа не отвечает» (02.11, макет 18): просьба закрыться лежит
    /// непрочитанной; «Завершить программу» или «Подождать».
    Hung,
    /// «Удаление файла» (02.11, макет 18): в корзину или отмена.
    DeleteAsk,
    /// Окно пользовательской программы (фаза 47a).
    ///
    /// Внутри — номер задачи, а **не** имя. Имя у такого окна своё, приезжает
    /// из программы и живёт в самом окне ([`Window::caption`]); номер задачи
    /// отвечает на другой вопрос — чьё это окно, — и отвечает единственным
    /// способом, который переживает смерть программы. Снимая задачу, ядро
    /// знает, какое окно убрать, и не спрашивает об этом программу, которой
    /// может уже не быть.
    ///
    /// Второе число — номер окна у этой задачи (фаза N7c). Окна ищут по
    /// [`App`], и одного номера задачи на два окна не хватает, а окну сообщения
    /// WinForms нужно своё окно поверх формы. Окон у программы не больше
    /// [`user_abi::MAX_WINDOWS`]; первое получает номер 0.
    Program(u32, u32),
}

impl App {
    /// Порядок в меню запуска.
    ///
    /// Ни монитора системы, ни файлового менеджера здесь больше нет, и это не
    /// пропуск: с фазы 47b первый — программа `/bin/sysmon`, с 47c второй —
    /// `/bin/files`, и меню находит обоих там же, где все остальные программы
    /// из `/bin`. Оставить их здесь значило бы предлагать человеку два разных
    /// способа открыть одно и то же окно.
    pub const LAUNCHABLE: [App; 5] = [
        App::Terminal,
        App::Settings,
        App::About,
        App::Shutdown,
        App::Restart,
    ];

    /// Спрашивает ли это окно «точно?» — и о чём именно.
    ///
    /// `Some(true)` — перезагрузка, `Some(false)` — выключение, `None` — обычное
    /// окно. Один ответ на оба вопроса, а не два предиката: «спрашивает ли» и
    /// «о чём именно» нельзя разнести так, чтобы вызывающий проверил первое и
    /// забыл второе, — а перепутать выключение с перезагрузкой дороже, чем
    /// набрать лишний `match`.
    #[must_use]
    pub const fn confirms_power(self) -> Option<bool> {
        match self {
            App::Shutdown => Some(false),
            App::Restart => Some(true),
            _ => None,
        }
    }

    /// Окно терминала - первое или унесённое из полосы вкладок.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, App::Terminal | App::TermWin(_))
    }

    /// Как окно называется для человека.
    ///
    /// Отдельно от [`App::title`], а не вместо него: `title` попадает в журнал,
    /// по нему стенд находит окно на экране и по нему же сверяет строки прогона.
    /// Журнал — инструмент разработчика и остаётся на латинице; заголовок окна
    /// читает человек, и он на его языке.
    #[must_use]
    pub const fn caption(self) -> &'static str {
        match self {
            App::Terminal | App::TermWin(_) => "Терминал",
            App::About => "О системе",
            App::Settings => "Параметры",
            App::Shutdown => "Выключение",
            App::Restart => "Перезагрузка",
            App::OpenWith => "Открыть с помощью",
            App::Hung => "Программа не отвечает",
            App::DeleteAsk => "Удаление файла",
            // Запас, а не подпись: у окна программы есть собственное имя, и
            // берут его из окна ([`Window::caption`]), а не отсюда. Сюда
            // попадают, когда окна уже нет, — например, печатая в журнал о
            // только что закрытом.
            App::Program(..) => "Программа",
        }
    }

    /// Каким цветом горит значок программы.
    ///
    /// Цвет опознаёт окно раньше, чем прочитан заголовок: зелёный — терминал,
    /// красный — вопрос о выключении, синий — всё остальное.
    #[must_use]
    pub const fn tone(self) -> mini_ui::paint::Tone {
        match self {
            App::Terminal | App::TermWin(_) => mini_ui::paint::Tone::Ok,
            App::Shutdown | App::Restart => mini_ui::paint::Tone::Bad,
            _ => mini_ui::paint::Tone::Accent,
        }
    }

    /// Можно ли менять окну размер.
    ///
    /// Терминалу и «Параметрам» — да: их содержимое выкладывается по размеру.
    /// Вопросам о выключении и «О системе» — нет: это карточки с текстом ровно
    /// под свой размер, и растянутые они только пустеют. Окну программы — только
    /// если она сама сказала, что умеет ([`Window::resizable`], фаза 62b). Уголок
    /// у такого окна не рисуется: нарисованный, он обещал бы то, чего нет.
    #[must_use]
    pub const fn resizable(self) -> bool {
        matches!(self, App::Terminal | App::TermWin(_) | App::Settings)
    }

    /// Smallest content of the kernel's own resizable windows, layout points
    /// (24.11): «Параметры» keep the 270-point side list next to a page of
    /// rows (a row with its switch needs ~360), the terminal keeps ~40
    /// columns and a few lines.
    #[must_use]
    pub const fn min_content(self) -> (u32, u32) {
        match self {
            App::Settings => (640, 420),
            App::Terminal | App::TermWin(_) => (360, 180),
            _ => (0, 0),
        }
    }

    /// Значок программы: на столе, в меню запуска и в заголовке окна.
    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            App::Terminal | App::TermWin(_) => Icon::Terminal,
            App::About => Icon::Info,
            App::Settings => Icon::Settings,
            App::Shutdown | App::Restart => Icon::Power,
            App::OpenWith => Icon::File,
            App::Hung => Icon::Info,
            App::DeleteAsk => Icon::Close,
            App::Program(..) => Icon::Package,
        }
    }

    /// Имя окна в журнале и в прицеле стенда.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            App::Terminal => "Terminal",
            // Стенд целится в унесённое окно по этому имени.
            App::TermWin(_) => "Terminal window",
            App::About => "About",
            App::Settings => "Settings",
            App::Shutdown => "Shut down",
            App::Restart => "Restart",
            App::OpenWith => "Open with",
            App::Hung => "Not responding",
            App::DeleteAsk => "Delete",
            // Имя программы в журнал печатает не это: оно приезжает из окна, а
            // здесь стоит родовое слово — на случай, когда окна уже нет.
            App::Program(..) => "Program",
        }
    }

    /// Строка меню: что эта программа делает.
    #[must_use]
    pub const fn about(self) -> &'static str {
        match self {
            App::Terminal | App::TermWin(_) => "оболочка и команды ядра",
            App::About => "что это за система",
            App::Settings => "экран, программы, обновление",
            App::Shutdown => "закрыть том и выключить",
            App::Restart => "закрыть том и запустить снова",
            App::OpenWith => "выбрать программу для файла",
            App::Hung => "программа не отвечает",
            App::DeleteAsk => "удалить в корзину",
            // В меню запуска окна программ не попадают: их открывает сама
            // программа, а не человек со стола. Строка здесь затем, что разбор
            // обязан быть полным.
            App::Program(..) => "окно пользовательской программы",
        }
    }
}

/// Стороны окна, которые двигает [`Hit::Resize`].
pub const EDGE_LEFT: u8 = 1;
pub const EDGE_RIGHT: u8 = 2;
pub const EDGE_TOP: u8 = 4;
pub const EDGE_BOTTOM: u8 = 8;

/// Ширина полосы у края окна, за которую его тянут, — в точках до масштаба.
const EDGE_GRAB: u32 = 6;

/// Во что попадает указатель внутри окна.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// Кнопка закрытия.
    Close,
    /// Кнопка «свернуть».
    Minimize,
    /// Кнопка «развернуть» — она же «вернуть прежний размер».
    Maximize,
    /// Кнопка клавиатуры в заголовке окна программы на телефоне (05.11):
    /// показать или спрятать экранную клавиатуру для этого окна.
    Keyboard,
    /// Край или угол окна: за него окно тянут за размер. Какие стороны
    /// двигаются — [`EDGE_LEFT`] и прочие (01.11: раньше — только правый
    /// нижний уголок).
    Resize(u8),
    /// Полоса заголовка: за неё окно таскают.
    Title,
    /// Вкладка терминала (номер сессии): переключиться на неё.
    Tab(usize),
    /// Крестик вкладки терминала: закрыть её.
    TabClose(usize),
    /// «+» на полосе вкладок: новая вкладка.
    TabNew,
    /// Всё остальное.
    Body,
}

/// Вкладка терминала, которой сейчас не видно: её сетка копит вывод.
pub struct TermTab {
    pub session: usize,
    pub grid: TextGrid,
}

/// Вкладка, вынутая из окна (20.11): её сетка и имя едут в другое окно
/// терминала, а оболочка сессии живёт дальше - о переезде она не знает, её
/// вывод находит окно по номеру сессии.
pub struct LooseTab {
    pub session: usize,
    grid: TextGrid,
    title: Option<String>,
    renamed: bool,
}

/// Что сделала клавиша с именем вкладки, которое правят в полосе.
pub enum RenameStep {
    /// Набор продолжается (или клавиша проглочена).
    Typing,
    /// Enter: сессия и новое имя (пустое - имя снова даёт программа).
    Done(usize, String),
    /// Esc: имя прежнее.
    Cancelled,
}

/// Высота полосы вкладок терминала (как у Windows Terminal — под
/// заголовком, всегда видна).
const STRIP_H: u32 = 30;
/// Наибольшая ширина вкладки.
const TAB_W: u32 = 200;

/// Содержимое окна.
pub enum Content {
    /// Сетка символов: терминал и всё, что печатает строки.
    Text(TextGrid),
    /// Окно параметров: разделы слева, содержимое справа.
    Settings(SettingsView),
    /// Окно, которое сообщает или спрашивает: «О системе», выключение.
    Dialog(DialogView),
    /// Окно пользовательской программы: пиксели пишет она сама.
    Program(ProgramView),
}

/// Окно, которое рисует не ядро.
pub struct ProgramView {
    /// Поверхность программы — кадры, отображённые в её адресное пространство.
    ///
    /// Отдельно от поверхности окна, а не вместо неё, и это не лишняя копия на
    /// ровном месте: в поверхности окна лежат ещё и украшения, нарисованные
    /// ядром. Отдай мы программе её целиком — программа писала бы в
    /// собственный заголовок, и закрасить кнопку «закрыть» стало бы её
    /// законным правом.
    ///
    /// Цена названа: [`Window::commit`] копирует изменившийся кусок, то есть
    /// ровно столько пикселей, сколько программа объявила изменившимися.
    pixels: Surface,
    /// Размер окна меняет человек (фаза 62b): программа сказала это вызовом
    /// [`user_abi::SYS_WINSTYLE`] и тем самым обещала отвечать на
    /// [`user_abi::WIN_RESIZE`].
    resizable: bool,
    /// Форма указателя над содержимым (09.11.2026, [`user_abi::SYS_WINCURSOR`]).
    cursor: super::pointer::Shape,
    /// Размер содержимого, о котором стол последним попросил программу, пока
    /// она не ответила. См. [`Window::replace_pixels`].
    asked: Option<(u32, u32)>,
    /// События, которых программа ещё не забрала.
    ///
    /// Очередь конечна ([`MAX_EVENTS`]), и переполнение теряет **новые**
    /// события, а не старые: [`user_abi::WIN_CLOSE`] к этому моменту уже лежит
    /// в ней, и выбросить его ради очередного щелчка значило бы получить окно,
    /// которое невозможно закрыть.
    events: VecDeque<WinEvent>,
}

/// Сколько событий окно держит непрочитанными.
///
/// Тридцать два — это пара секунд невнимательности программы, занятой своим
/// делом. Держать больше незачем: события устаревают, и программа, не
/// разобравшая очередь за это время, получит от длинной очереди только
/// запоздалые ответы на то, чего человек уже не помнит.
const MAX_EVENTS: usize = 32;

pub struct Window {
    pub app: App,
    pub rect: Rect,
    surface: Surface,
    content: Content,
    /// Как окно называется в заголовке и в кнопке панели задач.
    ///
    /// Собственная строка, а не [`App::caption`], и с фазы 47a иначе нельзя: у
    /// окна программы имя своё, приезжает оно из самой программы, и заимствовать
    /// его нечем — строка лежит в памяти программы, которой может не стать
    /// раньше окна.
    title: String,
    /// Множитель геометрии стола: 1 на обычном экране, 2 на очень плотном.
    ///
    /// Из него же выводится размерный ряд шрифта — порог у них общий, и хранить
    /// два числа значило бы однажды нарисовать кнопку одного ряда шрифтом
    /// другого.
    scale: mini_ui::Scale,
    /// Окно свёрнуто: его нет на экране, но оно есть в панели задач.
    ///
    /// Свёрнутое окно сохраняет и поверхность, и содержимое: свернуть — это
    /// «убрать с глаз», а не «закрыть», и вернувшееся окно обязано показать то
    /// же, что показывало до этого.
    pub minimized: bool,
    /// Когда окно свернули (мс от запуска) — лист «Свёрнутые программы»
    /// показывает, сколько оно так лежит, и ставит последнее свёрнутое первым.
    pub minimized_at_ms: u64,
    /// Куда вернуть окно, если оно развёрнуто. `None` — окно обычного размера.
    restore: Option<Rect>,
    /// Окно прилеплено к половине монитора (брошено у края, 20.11, как Snap
    /// в Windows): его прежний прямоугольник. Потащили — окно возвращает
    /// прежний размер под указателем, как развёрнутое.
    snap_from: Option<Rect>,
    /// Окно-владелец той же программы (фаза 62d, `user_abi::SYS_WINOWNER`):
    /// это окно всегда стоит над ним, и поднять владельца — поднять и его.
    pub owner: Option<App>,
    /// Владелец ждёт ответа этого окна: при его поднятии фокус уходит сюда.
    pub modal: bool,
    /// Всплывающее окно программы (фаза 62e, `user_abi::SYS_WINPOPUP`): меню,
    /// выпадающий список, подсказка. Ни заголовка, ни рамки, ни кнопки на
    /// панели задач, и фокус оно не берёт — ввод остаётся у владельца.
    pub popup: bool,
    /// Где оно стоит — от угла содержимого владельца: стол двигает его вместе
    /// с владельцем.
    pub anchor: (i32, i32),
    /// Что изменилось в поверхности с прошлой сборки, в координатах поверхности.
    pub damage: Rect,
    /// Окну нужны коды клавиш, а не символы (окна сервера Wayland, фаза 71i):
    /// каждое нажатие и отпускание — [`super::WIN_RAWKEY`] с кодом evdev.
    pub raw_keys: bool,
    /// Без заголовка и рамки стола: рамку рисует сама программа (окна сервера
    /// Wayland — Chromium рисует вкладки, «свернуть», «развернуть» и
    /// «закрыть» сам, фаза 71i). В отличие от всплывающего, у такого окна
    /// есть кнопка на панели и фокус; двигают и растягивают его по просьбе
    /// программы (`ui::begin_move`, `ui::begin_resize`).
    pub frameless: bool,
    /// Когда программу попросили закрыться (мс со старта); ноль — не просили.
    /// Просьба, пролежавшая в очереди непрочитанной, — признак зависания.
    pub close_asked: u64,
    /// В поверхности есть прозрачные точки (альфа в старшем байте, цвет
    /// умножен на неё): стол смешивает окно с тем, что под ним, и углы не
    /// срезает — их рисует программа (окна Wayland: Rider, подсказки JBR).
    pub translucent: bool,
    /// Тень и форму рисует сама программа (прозрачные поля вокруг окна):
    /// тени стола нет, а щелчок по прозрачной точке проходит сквозь окно.
    pub shaped: bool,
    /// Где в текстовом окне нажали левую кнопку — начало выделения мышью
    /// (терминал T5): сквозная строка и столбец сетки.
    select_from: Option<(u64, u32)>,
    /// Значок программы окна (у окон ядра — `None`: значок даёт [`App`]).
    pub art: Option<super::appicon::Art>,
    /// Вкладки терминала в порядке полосы — номера сессий. Пусто — окно
    /// без вкладок.
    tab_order: Vec<usize>,
    /// Вкладки, которых не видно (сетка видимой — в `content`).
    tabs: Vec<TermTab>,
    /// Сессия видимой вкладки.
    session: usize,
    /// Подписи вкладок: сессия и что в ней идёт.
    tab_titles: Vec<(usize, String)>,
    /// Вкладки, которым человек дал имя сам (20.11): подпись от программы
    /// его больше не перекрывает.
    renamed: Vec<usize>,
    /// Вкладка, имя которой правят прямо в полосе: сессия и набранное.
    tab_edit: Option<(usize, String)>,
    /// Имя выделено целиком (правку только начали): первый знак заменяет
    /// его, как в Windows Terminal.
    tab_edit_all: bool,
    /// По чему окна складываются в одну кнопку панели задач: путь программы.
    /// Пусто — окно ядра, группа по [`App`].
    pub group: String,
    /// Номер в порядке создания: по нему стоят кнопки панели задач. Список
    /// окон стола упорядочен по глубине и меняется при каждом поднятии.
    pub born: u64,
    /// Кнопка заголовка под указателем (72b): подсвечена, крестик — красным.
    hot: Option<Hit>,
    /// Рамку окна программы выбирает стол, а не размер её пикселей (телефон,
    /// 05.11): окно во весь экран, а пиксели лежат в нём по центру — или от
    /// угла со срезанным лишним, если программа нарисовала больше экрана
    /// (NetSurf просит 800×600 на экране шириной 720). Программе, которая
    /// умеет менять размер, уходит просьба [`user_abi::WIN_RESIZE`], и её
    /// ответ заполняет рамку целиком.
    pub fit: bool,
    /// Человек попросил для этого окна экранную клавиатуру (кнопка в
    /// заголовке, телефон, 05.11): пока окно активно, клавиатура видна, а
    /// окно стоит над ней. Снимается вторым нажатием и уходом фокуса (см.
    /// `Compositor::sync_keyboard`).
    pub keyboard: bool,
    /// Низ окна поднят над клавиатурой (05.11): когда она уйдёт, стол вернёт
    /// его к полоске жеста (`Compositor::lift_program_windows`).
    pub lifted: bool,
    /// Что лежит под обводкой окна (20.11), см. [`EdgeBook`].
    edge: EdgeBook,
    /// Окно во весь монитор (`xdg_toplevel.set_fullscreen`, 20.11): ни
    /// рамки, ни панели поверх, пока оно впереди. Не то же, что развёрнутое:
    /// то занимает рабочую область, это — монитор целиком.
    pub fullscreen: bool,
    /// Куда вернуть окно из полноэкранного режима.
    full_from: Option<Rect>,
    /// Размер монитора, на который окно открыто во весь экран.
    full_size: (u32, u32),
    /// Последний размер программа выбрала сама, а не ответила им на просьбу
    /// стола (см. [`Self::replace_pixels`]).
    self_sized: bool,
    /// Маленький диалог программы (20.11, `user_abi::WIN_STYLE_DIALOG`,
    /// «Выполнить»): в заголовке только крестик.
    pub dialog: bool,
    /// Size limits the program declared (24.11, [`SizeLimits`]); empty —
    /// the built-in ones of [`App::min_content`].
    limits: SizeLimits,
    /// G3 (21.11): the window is shown straight from the program pixels
    /// (fullscreen fast path, `Compositor::direct_plan`) and its own surface
    /// was not updated since - copy everything before composing it again.
    pub stale: bool,
    /// G3: the program pixels are a client buffer in physically contiguous
    /// memory - `(address of the pixels, physical address)`; a display that
    /// can scan out of system memory shows it without any copy.
    pub direct_phys: Option<(usize, u64)>,
}

/// Учёт обводки окна (20.11).
///
/// Обводка — полупрозрачная линия поверх того, что уже лежит в поверхности.
/// Раньше она смешивалась с поверхностью заново при каждой перерисовке
/// заголовка и при каждом наведении на его кнопки, а содержимое под ней
/// не перерисовывалось: линия с каждым разом густела, но на экран попадала
/// только кусками — там, где кадр пересобирался по другой причине (под
/// курсором, ведомым вдоль верхнего края). Отсюда «кольцо» вокруг окна.
/// Копия пикселей программы, наоборот, стирала обводку по бокам и снизу.
///
/// Теперь для каждой точки, которой может коснуться обводка (две крайние
/// строки и столбца и дуги углов), помнится, что под ней (`base`) и что в ней
/// было после обводки (`drawn`). Точка, в которой уже не то, что нарисовали,
/// перерисована содержимым — её новое значение и есть основа. Обводка
/// всегда ложится на основу один раз.
struct EdgeBook {
    w: u32,
    h: u32,
    radius: u32,
    base: Vec<u32>,
    drawn: Vec<u32>,
    focused: bool,
}

impl EdgeBook {
    const fn new() -> Self {
        Self { w: 0, h: 0, radius: 0, base: Vec::new(), drawn: Vec::new(), focused: false }
    }
}

/// Точки поверхности `w`×`h`, которых касается обводка радиуса `radius`
/// ([`draw::rounded_stroke`]): по две крайние строки и столбца и полоса вдоль
/// дуги каждого угла. Порядок всегда один и тот же — по нему [`EdgeBook`]
/// хранит значения.
fn edge_points(w: u32, h: u32, radius: u32, mut f: impl FnMut(u32, u32)) {
    if w < 4 || h < 4 {
        return;
    }
    // Не меньше двух: у прямых углов (классические рамки, 20.11) кромка — в
    // две полные строки и два столбца.
    let c = (radius + 1).max(2).min(w / 2).min(h / 2);
    // Внутри круга радиуса `radius - 2.5` с центром в центре скругления
    // обводки нет. Всё в удвоенных координатах, чтобы остаться в целых.
    let r2 = 2 * i64::from(radius);
    let inner = (r2 - 5).max(0);
    let near = |lx: u32, ly: u32| {
        if lx < 2 || ly < 2 {
            return true;
        }
        let dx = 2 * i64::from(lx) + 1 - r2;
        let dy = 2 * i64::from(ly) + 1 - r2;
        dx * dx + dy * dy >= inner * inner
    };
    for y in 0..h {
        let ly = y.min(h - 1 - y);
        if ly < c {
            for x in 0..c {
                if near(x, ly) {
                    f(x, y);
                }
            }
            if ly < 2 {
                for x in c..w - c {
                    f(x, y);
                }
            }
            for x in w - c..w {
                if near(w - 1 - x, ly) {
                    f(x, y);
                }
            }
        } else {
            for x in [0, 1, w - 2, w - 1] {
                f(x, y);
            }
        }
    }
}

/// Знак кнопки заголовка Windows 98 (22.11) — по точкам, как в системном
/// шрифте Marlett: черта «свернуть» 6×2, рамка «развернуть» 9×9 с толстым
/// верхом, две рамки «восстановить», косой крест «закрыть» 8×7. `u` —
/// размер «точки» при масштабе.
fn classic_glyph(surface: &mut Surface, knob: Rect, hit: Hit, restore: bool, u: u32, ink: Color) {
    let black = ink;
    let ui = u as i32;
    // Начало сетки 16×14 — по центру кнопки (у масштаба кнопка больше).
    let ox = knob.x + (knob.w as i32 - 16 * ui) / 2;
    let oy = knob.y + (knob.h as i32 - 14 * ui) / 2;
    let mut dot = |x: i32, y: i32, w: i32, h: i32, color: Color| {
        surface.fill(Rect::new(ox + x * ui, oy + y * ui, (w * ui) as u32, (h * ui) as u32), color);
    };
    let frame = |x: i32, y: i32, w: i32, h: i32, dot: &mut dyn FnMut(i32, i32, i32, i32, Color)| {
        dot(x, y, w, 2, black);
        dot(x, y, 1, h, black);
        dot(x + w - 1, y, 1, h, black);
        dot(x, y + h - 1, w, 1, black);
    };
    match hit {
        Hit::Minimize => dot(4, 9, 6, 2, black),
        Hit::Maximize if restore => {
            frame(5, 1, 7, 6, &mut dot);
            // Передняя рамка закрывает заднюю: внутри неё — грань кнопки.
            dot(3, 4, 7, 6, theme::win98::FACE);
            frame(3, 4, 7, 6, &mut dot);
        }
        Hit::Maximize => frame(3, 2, 9, 9, &mut dot),
        Hit::Close => {
            for i in 0..7 {
                dot(4 + i, 3 + i, 2, 1, black);
                dot(10 - i, 3 + i, 2, 1, black);
            }
        }
        _ => {}
    }
}

/// Счётчик для [`Window::born`].
static BORN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Наименьший размер окна в пикселях.
///
/// Меньше — это окно, в котором не помещается ни строки текста, ни трёх кнопок
/// заголовка; вернуть его к рабочему размеру мышью человек уже не сможет.
const MIN_W: u32 = 220;
const MIN_H: u32 = 120;

/// Size limits of a window's content (24.11, WM SDK): what a program
/// declared through `user_abi::SYS_WINLIMITS` or a Wayland client through
/// `xdg_toplevel.set_min_size/set_max_size`, and the built-in minimum of the
/// kernel's own windows ([`App::min_content`]). A zero side — no limit.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SizeLimits {
    pub min: (u32, u32),
    pub max: (u32, u32),
    /// In layout points (times the window scale) — native programs; `false`
    /// — pixels (Wayland clients draw at the size they were given).
    pub logical: bool,
}

impl SizeLimits {
    /// Minimum equals maximum on both sides: the window has one size.
    #[must_use]
    pub const fn fixed(&self) -> bool {
        self.min.0 > 0 && self.min.1 > 0 && self.min.0 == self.max.0 && self.min.1 == self.max.1
    }

    /// Nothing declared.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.min.0 == 0 && self.min.1 == 0 && self.max.0 == 0 && self.max.1 == 0
    }
}

/// Сторона уголка, за который тянут размер.
///
/// Больше прежних шестнадцати: у скруглённого окна сам угол срезан, и попасть
/// в квадрат, половина которого приходится на срез, мышью нельзя.
const GRIP: u32 = 20;

/// Classic (Windows 98) caption band at 100 %: blue caption 18 and the face
/// line under it 1 (23.11; `SM_CYCAPTION` = 19).
pub const CLASSIC_CAPTION: u32 = 19;
/// Height of the whole classic title band of a framed window at 100 %: the
/// 4 px frame on top ([`Window::classic_frame`]) and [`CLASSIC_CAPTION`].
/// At 150-200 % the frame is 8 (bevel rows are 2 px there).
pub const CLASSIC_TITLE: u32 = 23;
const _: () = assert!(CLASSIC_TITLE == 4 + CLASSIC_CAPTION);
/// Кнопка заголовка классики: 16×14, как у Windows 98.
const CLASSIC_BTN_W: u32 = 16;
const CLASSIC_BTN_H: u32 = 14;

impl Window {
    /// Создать текстовое окно.
    ///
    /// `None`, если не хватило памяти под поверхность или окно слишком мало,
    /// чтобы вместить хоть одну ячейку текста. Паниковать здесь нельзя: окно —
    /// это несколько мегабайт, отказ выделения совершенно реален, и система
    /// обязана продолжить работу без этого окна.
    #[must_use]
    pub fn text(app: App, rect: Rect, scale: mini_ui::Scale) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        // Терминал — почти чёрный при любой теме, как у терминалов Linux и
        // Windows Terminal (T5); заливка окна остаётся под заголовком.
        let back = mini_ui::text::TERM_BG;
        let mut surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let strip = app.is_terminal() && !theme::is_mobile();
        let (top, side) = (Window::title_height(scale), Window::frame_for(scale));
        surface.fill(content_area(&surface, top, side), back);
        let grid = TextGrid::new(
            grid_area(&surface, scale, strip, top, side),
            ctx.face(Role::Mono),
            mini_ui::text::TERM_FG,
            back,
        )?;
        let mut window = Self::wrap(app, rect, surface, Content::Text(grid), scale, own(app.caption())?);
        if strip {
            window.tab_order.push(0);
        }
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать окно параметров.
    #[must_use]
    pub fn settings(rect: Rect, scale: mini_ui::Scale, screen: (u32, u32)) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let content = Content::Settings(SettingsView::new(screen));
        let title = own(App::Settings.caption())?;
        let mut window = Self::wrap(App::Settings, rect, surface, content, scale, title);
        window.redraw_content();
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать окно, которое сообщает или спрашивает.
    #[must_use]
    pub fn dialog(app: App, rect: Rect, scale: mini_ui::Scale, dialog: Dialog) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let content = Content::Dialog(DialogView::new(dialog));
        let mut window = Self::wrap(app, rect, surface, content, scale, own(app.caption())?);
        window.redraw_content();
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать окно пользовательской программы.
    ///
    /// `pixels` — поверхность программы, то есть кадры, которые ей отображены;
    /// владеет ими не окно, а тот, кто их выдал ([`crate::user`]). Окно живёт
    /// ровно столько же, сколько они, и это не совпадение: снимает окно и
    /// возвращает кадры один и тот же путь.
    ///
    /// Размер окна больше размера поверхности на высоту заголовка: программа
    /// просит место **под содержимое**, а полосу сверху рисует ядро. Считать
    /// иначе значило бы отдавать программе окно, в котором её же рисунок
    /// наполовину закрыт заголовком.
    #[must_use]
    pub fn program(app: App, rect: Rect, scale: mini_ui::Scale, title: &str, pixels: Surface) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let content = Content::Program(ProgramView {
            pixels,
            resizable: false,
            cursor: super::pointer::Shape::Arrow,
            asked: None,
            events: VecDeque::new(),
        });
        let mut window = Self::wrap(app, rect, surface, content, scale, own(title)?);
        window.redraw_content();
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать всплывающее окно программы (фаза 62e): то же, что
    /// [`Self::program`], но без полосы заголовка — окно ровно с поверхность.
    #[must_use]
    pub fn popup_program(app: App, rect: Rect, scale: mini_ui::Scale, title: &str, pixels: Surface) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let content = Content::Program(ProgramView {
            pixels,
            resizable: false,
            cursor: super::pointer::Shape::Arrow,
            asked: None,
            events: VecDeque::new(),
        });
        let mut window = Self::wrap(app, rect, surface, content, scale, own(title)?);
        window.popup = true;
        window.redraw_content();
        Some(window)
    }

    /// Высота полосы заголовка **этого** окна: у всплывающего её нет; у
    /// развёрнутого в классике - без рамки сверху (23.11).
    #[must_use]
    pub fn bar(&self) -> u32 {
        self.bar_at(self.scale)
    }

    fn wrap(
        app: App,
        rect: Rect,
        surface: Surface,
        content: Content,
        scale: mini_ui::Scale,
        title: String,
    ) -> Self {
        Self {
            app,
            rect,
            surface,
            content,
            title,
            scale,
            minimized: false,
            minimized_at_ms: 0,
            restore: None,
            snap_from: None,
            owner: None,
            modal: false,
            popup: false,
            anchor: (0, 0),
            damage: Rect::EMPTY,
            raw_keys: false,
            frameless: false,
            close_asked: 0,
            translucent: false,
            shaped: false,
            select_from: None,
            art: None,
            group: String::new(),
            tab_order: Vec::new(),
            tabs: Vec::new(),
            session: 0,
            tab_titles: Vec::new(),
            renamed: Vec::new(),
            tab_edit: None,
            tab_edit_all: false,
            born: BORN.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            hot: None,
            fit: false,
            keyboard: false,
            lifted: false,
            edge: EdgeBook::new(),
            fullscreen: false,
            full_from: None,
            full_size: (0, 0),
            self_sized: false,
            dialog: false,
            limits: SizeLimits { min: app.min_content(), max: (0, 0), logical: true },
            stale: false,
            direct_phys: None,
        }
    }

    /// Окно программы на телефоне (05.11): рамку держит стол, пиксели ложатся
    /// в неё как есть (см. [`Self::fit`]). `restore` — куда вернуть окно
    /// кнопкой «на весь экран», если оно открыто развёрнутым.
    pub fn fit_frame(&mut self, restore: Option<Rect>, focused: bool) {
        if !self.is_program() || self.popup {
            return;
        }
        self.fit = true;
        self.restore = restore;
        self.redraw_content();
        self.draw_decorations(focused);
        self.damage = Rect::new(0, 0, self.rect.w, self.rect.h);
    }

    /// Где в поверхности окна лежит угол пикселей программы.
    ///
    /// Обычно — сразу под заголовком. В рамке, которую держит стол
    /// ([`Self::fit`]), — по центру области содержимого; пиксели больше неё
    /// стоят от её угла, и лишнее срезается при переносе. Всё — насыщающими
    /// действиями: окно шире экрана на телефоне с проверкой переполнений
    /// иначе остановило бы ядро на вычитании.
    fn pixels_at(&self) -> (i32, i32) {
        let area = content_area(&self.surface, self.bar(), self.frame());
        match &self.content {
            Content::Program(view) if self.fit => {
                let dx = area.w.saturating_sub(view.pixels.width()) / 2;
                let dy = area.h.saturating_sub(view.pixels.height()) / 2;
                (area.x.saturating_add(dx as i32), area.y.saturating_add(dy as i32))
            }
            _ => (area.x, area.y),
        }
    }

    /// Угол пикселей программы на экране: от него считаются её координаты
    /// указателя и места её всплывающих окон.
    #[must_use]
    pub fn content_origin(&self) -> (i32, i32) {
        let (x, y) = self.pixels_at();
        (self.rect.x.saturating_add(x), self.rect.y.saturating_add(y))
    }

    /// Точка экрана в координатах пикселей программы — в том числе за их
    /// краем (захваченному окну нужна и такая).
    #[must_use]
    pub fn program_local(&self, x: i32, y: i32) -> (i32, i32) {
        let (ox, oy) = self.content_origin();
        (x.saturating_sub(ox), y.saturating_sub(oy))
    }

    /// Лежит ли точка экрана на пикселях программы. Поле вокруг них в рамке
    /// стола ([`Self::fit`]) — не её: щелчок туда ей не уходит.
    #[must_use]
    pub fn on_pixels(&self, x: i32, y: i32) -> bool {
        let Content::Program(view) = &self.content else {
            return false;
        };
        let (lx, ly) = self.program_local(x, y);
        view.pixels.bounds().contains(lx, ly)
    }

    /// Высота полосы заголовка при заданном масштабе.
    ///
    /// С фазы С6 — настройка, а не константа: [`theme::title_h`] выбирает
    /// человек в «Параметрах».
    #[must_use]
    pub fn title_height(scale: mini_ui::Scale) -> u32 {
        // Классика (22.11): рамка 3 + заголовок 20 + 1 — как у Windows 98
        // (там 18 под шрифт 8 пт), а не полоса современного вида.
        // 23.11: the band starts with the separate 4 px window frame.
        if Self::classic() {
            return Self::classic_frame(scale) + CLASSIC_CAPTION * scale;
        }
        theme::title_h() * scale
    }

    /// Width of the Windows 98 window frame at `scale` (23.11): two bevel
    /// rows (#DFDFDF/black outside, white/#808080 inside) and two face rows,
    /// each [`mini_ui::classic::unit`] thick - 4 px at 100-125 %, 8 at
    /// 150-200 %. The frame lies OUTSIDE the content: the window is wider than
    /// the content by two frames and taller by the title band and one frame.
    #[must_use]
    pub fn classic_frame(scale: mini_ui::Scale) -> u32 {
        4 * mini_ui::classic::unit(scale)
    }

    /// Frame width of a new framed window at `scale` (classic look: the
    /// Windows 98 frame, otherwise none).
    #[must_use]
    pub fn frame_for(scale: mini_ui::Scale) -> u32 {
        if Self::classic() { Self::classic_frame(scale) } else { 0 }
    }

    /// How much bigger than its content a new framed window is: `(w, h)`.
    #[must_use]
    pub fn chrome_for(scale: mini_ui::Scale) -> (u32, u32) {
        let frame = Self::frame_for(scale);
        (2 * frame, Self::title_height(scale) + frame)
    }

    /// Side frame of THIS window at `scale`: the classic frame, except for
    /// popups, frameless windows, the phone frame (`fit`), fullscreen and
    /// maximized windows (Windows 98 puts a maximized window's frame
    /// off-screen; here it is not drawn, and the caption touches the screen
    /// edges).
    fn frame_at(&self, scale: mini_ui::Scale) -> u32 {
        if !Self::classic() || self.popup || self.frameless || self.fit || self.fullscreen || self.is_maximized() {
            0
        } else {
            Self::classic_frame(scale)
        }
    }

    /// Side frame of this window (see [`Self::frame_at`]).
    #[must_use]
    pub fn frame(&self) -> u32 {
        self.frame_at(self.scale)
    }

    /// Title band of this window at `scale`: none for popups and frameless
    /// windows; classic without the frame - only the caption.
    fn bar_at(&self, scale: mini_ui::Scale) -> u32 {
        if self.popup || self.frameless {
            0
        } else if Self::classic() && self.frame_at(scale) == 0 {
            CLASSIC_CAPTION * scale
        } else {
            Self::title_height(scale)
        }
    }

    /// How much bigger than its content this window is at `scale`: `(w, h)`.
    fn chrome_at(&self, scale: mini_ui::Scale) -> (u32, u32) {
        let frame = self.frame_at(scale);
        (2 * frame, self.bar_at(scale) + frame)
    }

    /// Content size of this window if it were `w`x`h` (in its current state).
    #[must_use]
    pub fn content_of(&self, w: u32, h: u32) -> (u32, u32) {
        let (cw, ch) = self.chrome_at(self.scale);
        (w.saturating_sub(cw), h.saturating_sub(ch))
    }

    /// Classic frame layout key: the look changed it when this differs - the
    /// windows are rebuilt ([`super::compositor::Compositor::retitle_all`]).
    #[must_use]
    pub fn chrome_key() -> (u32, u32) {
        (Self::title_height(mini_ui::Scale::ONE), Self::frame_for(mini_ui::Scale::ONE))
    }

    /// Вкладки терминала — своей полосой под заголовком, а не в нём: на
    /// телефоне и в классике (у Windows 98 заголовок — только заголовок).
    fn separate_strip() -> bool {
        theme::is_mobile() || Self::classic()
    }

    /// Сменить окну программы поверхность: программа попросила другой размер
    /// (фаза N7d, [`user_abi::SYS_WINRESIZE`]) — сама или в ответ на
    /// [`user_abi::WIN_RESIZE`]. Место окна на столе остаётся.
    ///
    /// # Чей размер у рамки
    ///
    /// Рамка обычно следует за пикселями: окно — это содержимое плюс заголовок.
    /// Исключение одно — ответ на **устаревшую** просьбу стола (фаза 62b).
    /// Уголок тянут быстрее, чем форма успевает выложиться заново: пока
    /// программа отвечала на прошлый размер, стол попросил следующий, и рамка,
    /// прыгнувшая назад к ответу, дёргалась бы на каждом шаге. Поэтому, пока
    /// последняя просьба лежит в очереди непрочитанной, рамка остаётся той, что
    /// выбрал человек, а новые пиксели ложатся в неё как есть. Ответ на
    /// последнюю прочитанную просьбу окончательный, какой бы размер в нём ни
    /// был: у формы бывает наименьший размер, и спорить с ней рамке незачем.
    ///
    /// `false` — памяти под окно нового размера не нашлось, и окно осталось
    /// прежним вместе с прежними пикселями.
    pub fn replace_pixels(&mut self, pixels: Surface, focused: bool) -> bool {
        let Content::Program(view) = &self.content else {
            return false;
        };
        let answer = (pixels.width(), pixels.height());
        let asked = view.asked;
        let stale = asked.is_some_and(|size| size != answer)
            && view.events.iter().any(|event| event.kind == user_abi::WIN_RESIZE);
        let self_sized = asked.is_none() && !self.fit;
        // The state changes come first (23.11): they decide whether the
        // classic frame is shown, and with it the window size around the
        // pixels. Undone if there is no memory for the new window.
        let saved = (self.restore, self.fullscreen, self.full_from);
        // Развёрнутое окно помнит, куда вернуться, пока размер ему выбирает
        // стол. Размер, который программа выбрала сама, не спросясь, — это уже
        // не развёрнутое окно, и возвращать его некуда.
        if self_sized {
            self.restore = None;
        }
        // Окно во весь экран, ставшее по своей воле другого размера, из
        // полноэкранного режима вышло (игра переключилась в окно). Ответ на
        // просьбу стола — нет: он и есть размер монитора, пусть и с опозданием.
        if self_sized && self.fullscreen && answer != self.full_size {
            self.fullscreen = false;
            self.full_from = None;
        }
        // Рамку, которую держит стол (телефон, [`Self::fit`]), пиксели не
        // меняют: ответ ложится в неё по центру или срезается.
        let (w, h) = if stale || self.fit {
            (self.rect.w, self.rect.h)
        } else {
            let (cw, ch) = self.chrome_at(self.scale);
            (answer.0 + cw, answer.1 + ch)
        };
        let Some(surface) = Surface::new(w, h, theme::window_bg()) else {
            (self.restore, self.fullscreen, self.full_from) = saved;
            return false;
        };
        if let Content::Program(view) = &mut self.content {
            view.pixels = pixels;
            if !stale {
                view.asked = None;
            }
        }
        self.surface = surface;
        self.rect.w = w;
        self.rect.h = h;
        self.self_sized = self_sized;
        self.redraw_content();
        self.draw_decorations(focused);
        self.damage = Rect::new(0, 0, w, h);
        true
    }

    /// Последний размер программа выбрала сама, а не ответила на просьбу
    /// стола: только такой размер стол вправе толковать как «хочу во весь
    /// экран» (`ui::resize_window`), иначе ответ на «вернуть размер» снова
    /// разворачивал бы окно — и так по кругу.
    #[must_use]
    pub const fn self_sized(&self) -> bool {
        self.self_sized
    }

    /// Может ли человек менять окну размер: окну ядра — по его виду
    /// ([`App::resizable`]), окну программы — если она сказала, что умеет.
    #[must_use]
    pub fn resizable(&self) -> bool {
        match &self.content {
            Content::Program(view) => view.resizable,
            _ => self.app.resizable(),
        }
    }

    /// Форма указателя над содержимым окна программы; у окон ядра — `None`.
    #[must_use]
    pub fn program_cursor(&self) -> Option<super::pointer::Shape> {
        match &self.content {
            Content::Program(view) => Some(view.cursor),
            _ => None,
        }
    }

    /// Запомнить форму указателя, которую просит программа (09.11.2026).
    pub fn set_program_cursor(&mut self, shape: super::pointer::Shape) {
        if let Content::Program(view) = &mut self.content {
            view.cursor = shape;
        }
    }

    /// Запомнить, что программа умеет менять размер окна (фаза 62b,
    /// [`user_abi::SYS_WINSTYLE`]). Уголок появляется или исчезает сразу.
    pub fn set_resizable(&mut self, resizable: bool, focused: bool) {
        let Content::Program(view) = &mut self.content else {
            return;
        };
        if view.resizable == resizable {
            return;
        }
        view.resizable = resizable;
        if !resizable {
            view.asked = None;
        }
        // Уголок лежит поверх пикселей программы, и снять его можно только
        // заново перенеся их: своей заливки под ним нет.
        self.redraw_content();
        self.draw_decorations(focused);
        self.damage = Rect::new(0, 0, self.rect.w, self.rect.h);
        // Рамка стола (телефон, 05.11): программа, сказавшая «умею», сразу
        // получает просьбу заполнить её — так окно открывается во весь экран.
        if resizable && self.fit {
            let (w, h) = self.content_of(self.rect.w, self.rect.h);
            if (w, h) != (self.program_size().0, self.program_size().1) {
                self.ask_resize(w, h);
            }
        }
    }

    /// Размер пикселей программы; у окна ядра — ноль.
    #[must_use]
    pub fn program_size(&self) -> (u32, u32) {
        match &self.content {
            Content::Program(view) => (view.pixels.width(), view.pixels.height()),
            _ => (0, 0),
        }
    }

    /// Попросить программу перерисоваться в новом размере содержимого.
    ///
    /// Событие, которое она ещё не прочла, заменяется: нужен размер, на котором
    /// человек остановился, а не каждый промежуточный, — и очередь окна не
    /// забивается шагами уголка.
    fn ask_resize(&mut self, w: u32, h: u32) {
        let Content::Program(view) = &mut self.content else {
            return;
        };
        view.asked = Some((w, h));
        let event = WinEvent { kind: user_abi::WIN_RESIZE, code: 0, x: w as i32, y: h as i32 };
        if let Some(queued) = view.events.iter_mut().find(|e| e.kind == user_abi::WIN_RESIZE) {
            *queued = event;
            return;
        }
        self.push_event(event);
    }

    /// Пересобрать окно под новую высоту заголовка.
    ///
    /// Содержимое сохраняет размер, а окно — нет: у окна программы пиксели
    /// лежат в кадрах, отображённых ей, и менять их размер нельзя (см.
    /// [`Window::resize`]), поэтому растёт или уменьшается сам прямоугольник
    /// окна — ровно на разницу высот заголовка. Окна ядра пересобираются как
    /// при изменении размера: их содержимое рисует себя от области заново.
    pub fn retitle(&mut self) -> bool {
        // Рамка стола размер держит: пересобирается поверхность того же
        // размера, а программе уходит новая высота содержимого.
        if self.fit {
            let (w, h) = (self.rect.w, self.rect.h);
            return self.rebuild(w, h);
        }
        if let Content::Program(view) = &self.content {
            // 23.11: a maximized, snapped or fullscreen window keeps its place
            // on the screen - the program is asked for the new content size
            // (Wayland: a configure) instead of the window growing past the
            // work area.
            if view.resizable && (self.is_maximized() || self.is_snapped() || self.fullscreen) {
                let (w, h) = (self.rect.w, self.rect.h);
                return self.rebuild(w, h);
            }
            // Otherwise the content keeps its size, and the window grows or
            // shrinks around it by the change of the frame and title band.
            let (cw, ch) = self.chrome_at(self.scale);
            let (w, h) = (view.pixels.width() + cw, view.pixels.height() + ch);
            let Some(surface) = Surface::new(w, h, theme::window_bg()) else {
                return false;
            };
            self.surface = surface;
            self.rect.w = w;
            self.rect.h = h;
            self.commit(None);
            self.draw_decorations(false);
            self.damage = Rect::new(0, 0, w, h);
            return true;
        }
        let (w, h) = (self.rect.w, self.rect.h);
        self.rebuild(w, h)
    }

    /// Масштаб стола сменился на ходу (20.11, «Масштаб» в «Параметрах» или
    /// смена режима экрана): заголовок, рамка, шрифт терминала и содержимое
    /// окон ядра — в новом масштабе, размер окна — тот же в точках макета.
    /// Окну программы, умеющему менять размер, уходит просьба о новом размере
    /// (содержимое она перерисует сама: масштаб ей сообщает `SysInfo`);
    /// не умеющему — меняется только заголовок.
    pub fn set_scale(&mut self, scale: mini_ui::Scale) -> bool {
        let old = self.scale;
        let scale = scale.min(mini_ui::Scale::TWO);
        if old == scale {
            return true;
        }
        self.scale = scale;
        let face = Ctx::scaled(scale).face(Role::Mono);
        if let Content::Text(grid) = &mut self.content {
            grid.set_face(face);
            for tab in &mut self.tabs {
                tab.grid.set_face(face);
            }
        }
        if let Some(restore) = self.restore.as_mut() {
            restore.w = scale.rescale(restore.w, old);
            restore.h = scale.rescale(restore.h, old);
        }
        let w = scale.rescale(self.rect.w, old);
        let (cw_old, ch_old) = self.chrome_at(old);
        let (cw, ch) = self.chrome_at(scale);
        let fits = self.fit;
        match &self.content {
            Content::Program(view) if view.resizable && !fits => {
                let content_w = self.rect.w.saturating_sub(cw_old);
                let content_h = self.rect.h.saturating_sub(ch_old);
                let w = scale.rescale(content_w, old) + cw;
                let h = scale.rescale(content_h, old) + ch;
                // Limits in pixels (Wayland) do not grow with the scale.
                let (w, h) = self.clamp_size(w, h);
                self.rebuild(w, h)
            }
            Content::Program(_) => self.retitle(),
            _ => {
                let h = scale.rescale(self.rect.h, old);
                self.rebuild(w, h)
            }
        }
    }

    /// Контекст отрисовки этого окна.
    fn ctx(&self) -> Ctx {
        Ctx::scaled(self.scale)
    }

    /// Кнопки заголовка справа налево: закрыть, развернуть, свернуть.
    ///
    /// Порядок как у окон, к которым человек привык: крестик крайний справа,
    /// потому что промахнуться мимо него — это закрыть окно, а не свернуть.
    ///
    /// С 72b — как в Windows и в макете (экран 13): плоские, во всю высоту
    /// полосы, шириной [`theme::TITLE_BTN`], вплотную друг к другу и к краю.
    fn title_button(&self, from_right: u32) -> Rect {
        let scale = self.scale;
        if Self::classic() && !self.has_keyboard_button() {
            // Windows 98: кнопки 16×14 группой у правого края заголовка —
            // «свернуть» и «развернуть» вплотную, «закрыть» на 2 точки
            // правее. Место нажатия — во всю высоту полосы, рисуется кнопка
            // внутри ([`Self::classic_knob`]).
            // 23.11: 2 px inside the caption, which is inside the frame.
            let ctx = self.ctx();
            let w = ctx.px(CLASSIC_BTN_W) as i32;
            let right = self.surface.width() as i32 - self.frame() as i32 - ctx.px(2) as i32;
            let x = match from_right {
                0 => right - w,
                n => right - w - ctx.px(2) as i32 - w * n as i32,
            };
            return Rect::new(x, 0, w as u32, self.bar());
        }
        let width = theme::TITLE_BTN * scale;
        let step = width + theme::TITLE_GAP * scale;
        let right = self.surface.width() as i32;
        Rect::new(right - (width + step * from_right) as i32, 0, width, Self::title_height(scale))
    }

    /// Кнопка закрытия в координатах поверхности.
    fn close_button(&self) -> Rect {
        self.title_button(0)
    }

    /// Кнопка «развернуть» в координатах поверхности.
    fn maximize_button(&self) -> Rect {
        self.title_button(1)
    }

    /// Кнопка «свернуть» в координатах поверхности.
    ///
    /// Места кнопок считаются справа налево: закрыть — нулевое, развернуть —
    /// первое, свернуть — второе. С 05.11 так и на телефоне: там «развернуть»
    /// — «на весь экран», от края до края между строкой состояния и полоской
    /// жеста.
    fn minimize_button(&self) -> Rect {
        self.title_button(2)
    }

    /// Есть ли у окна кнопка клавиатуры: у окна программы на телефоне (05.11).
    /// Клавиатуру стол сам показывает только терминалу и полям «Параметров»
    /// — когда программа ждёт текста, он не знает, и спрашивают человека.
    fn has_keyboard_button(&self) -> bool {
        theme::is_mobile() && self.is_program() && !self.popup && !self.frameless
    }

    /// Кнопка клавиатуры — четвёртая справа, левее «свернуть».
    fn keyboard_button(&self) -> Rect {
        self.title_button(3)
    }

    /// Самая левая кнопка заголовка: до неё доходит подпись.
    fn leftmost_button(&self) -> Rect {
        if self.has_keyboard_button() {
            self.keyboard_button()
        } else if self.dialog {
            self.close_button()
        } else {
            self.minimize_button()
        }
    }

    /// Нажать или отжать кнопку клавиатуры: перерисовать кнопки.
    pub fn set_keyboard(&mut self, on: bool, focused: bool) {
        if self.keyboard == on {
            return;
        }
        self.keyboard = on;
        if self.has_keyboard_button() {
            self.paint_title_buttons(focused);
            let row = self.leftmost_button().union(&self.close_button());
            self.damage = self.damage.union(&row);
        }
    }

    /// Уголок изменения размера в координатах поверхности.
    fn resize_grip(&self) -> Rect {
        Rect::new(
            self.surface.width().saturating_sub(GRIP) as i32,
            self.surface.height().saturating_sub(GRIP) as i32,
            GRIP,
            GRIP,
        )
    }

    /// Во что попадает точка экрана. `None` — мимо окна.
    ///
    /// Кнопка закрытия проверяется первой: она лежит внутри полосы заголовка, и
    /// обратный порядок означал бы, что окно за неё таскают, а не закрывается.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let local = (x - self.rect.x, y - self.rect.y);
        // Прозрачная тень, нарисованная программой, — не окно: щелчок по ней
        // достаётся тому, что видно сквозь неё.
        if self.shaped && self.surface().get(local.0 as u32, local.1 as u32) >> 24 == 0 {
            return None;
        }
        // Края и углы — за них окно тянут за размер, у любого окна, которому
        // размер менять можно, и с рамкой, и без (Chromium, Rider: их тень, за
        // которую тянут в Linux, стол срезает). Полоса узкая, а у углов —
        // шире, как у Windows: в угол целиться труднее.
        if !self.popup && self.resizable() && !self.fixed_size() && !self.is_maximized() && !self.fullscreen && !theme::is_mobile() {
            let edges = self.edges_at(local.0, local.1);
            if edges != 0 {
                return Some(Hit::Resize(edges));
            }
        }
        // У всплывающего окна и окна без рамки нет ни заголовка, ни кнопок:
        // всё окно — содержимое.
        if self.popup || self.frameless {
            return Some(Hit::Body);
        }
        // The classic frame (23.11) of a window whose size cannot change: not
        // the content - it is dragged like the title.
        let frame = self.frame() as i32;
        if frame > 0 {
            let (w, h) = (self.rect.w as i32, self.rect.h as i32);
            if local.0 < frame || local.1 < frame || local.0 >= w - frame || local.1 >= h - frame {
                return Some(Hit::Title);
            }
        }
        if self.close_button().contains(local.0, local.1) {
            return Some(Hit::Close);
        }
        // Вкладки в заголовке (72h, макет 15).
        if !self.tab_order.is_empty() && !Self::separate_strip() && local.1 < Self::title_height(self.scale) as i32 {
            let (tabs, plus) = self.strip_layout();
            if plus.contains(local.0, local.1) {
                return Some(Hit::TabNew);
            }
            for (session, tab, close) in tabs {
                if close.contains(local.0, local.1) {
                    return Some(Hit::TabClose(session));
                }
                if tab.contains(local.0, local.1) {
                    return Some(Hit::Tab(session));
                }
            }
        }
        // На телефоне это «на весь экран» (05.11): окно встаёт от края до края,
        // окно программы получает просьбу заполнить его. У диалога этих
        // кнопок нет (20.11): там просто заголовок.
        if !self.dialog && self.maximize_button().contains(local.0, local.1) {
            // Greyed out (24.11, a window of one size or one that cannot
            // change it): the title under it, as Windows does.
            return Some(if self.can_maximize() { Hit::Maximize } else { Hit::Title });
        }
        if !self.dialog && self.minimize_button().contains(local.0, local.1) {
            return Some(Hit::Minimize);
        }
        if self.has_keyboard_button() && self.keyboard_button().contains(local.0, local.1) {
            return Some(Hit::Keyboard);
        }
        let title_bottom = self.bar() as i32;
        if local.1 < title_bottom {
            return Some(Hit::Title);
        }
        if !self.tab_order.is_empty() && local.1 < title_bottom + self.strip_h() as i32 {
            let (tabs, plus) = self.strip_layout();
            if plus.contains(local.0, local.1) {
                return Some(Hit::TabNew);
            }
            for (session, tab, close) in tabs {
                if close.contains(local.0, local.1) {
                    return Some(Hit::TabClose(session));
                }
                if tab.contains(local.0, local.1) {
                    return Some(Hit::Tab(session));
                }
            }
            // Пустое место полосы — как заголовок: за него таскают.
            return Some(Hit::Title);
        }
        // Уголок проверяется после заголовка: у окна ростом с полосу заголовка
        // они пересекаются, и таскать такое окно важнее, чем тянуть его за
        // размер.
        if self.resizable() && !self.fixed_size() && !self.fit && !self.is_maximized() && !self.fullscreen && !Self::classic() && self.resize_grip().contains(local.0, local.1) {
            return Some(Hit::Resize(EDGE_RIGHT | EDGE_BOTTOM));
        }
        Some(Hit::Body)
    }

    /// Какие края окна под точкой `local` (координаты окна): полоса
    /// [`EDGE_GRAB`] у каждой стороны, у углов — вдвое шире вдоль стороны.
    fn edges_at(&self, x: i32, y: i32) -> u8 {
        // Classic (23.11): exactly the Windows 98 frame - it lies outside the
        // content, and the content keeps its clicks; corners reach along the
        // sides as far as the caption (like `HTTOPLEFT` and friends).
        let frame = self.frame() as i32;
        let (grab, corner) = if frame > 0 {
            (frame, frame + (CLASSIC_CAPTION * self.scale) as i32)
        } else {
            let grab = ((EDGE_GRAB * self.scale) as i32).max(3);
            (grab, grab * 3)
        };
        let (w, h) = (self.rect.w as i32, self.rect.h as i32);
        let mut edges = 0;
        if x < grab {
            edges |= EDGE_LEFT;
        } else if x >= w - grab {
            edges |= EDGE_RIGHT;
        }
        if y < grab {
            edges |= EDGE_TOP;
        } else if y >= h - grab {
            edges |= EDGE_BOTTOM;
        }
        // Угол «с запасом»: рядом с углом на сторону — тоже угол.
        if edges == EDGE_LEFT || edges == EDGE_RIGHT {
            if y < corner {
                edges |= EDGE_TOP;
            } else if y >= h - corner {
                edges |= EDGE_BOTTOM;
            }
        } else if edges == EDGE_TOP || edges == EDGE_BOTTOM {
            if x < corner {
                edges |= EDGE_LEFT;
            } else if x >= w - corner {
                edges |= EDGE_RIGHT;
            }
        }
        edges
    }

    /// Уголок размера: три точки в правом нижнем углу — у окна, которому размер
    /// менять можно.
    fn draw_grip(&mut self) {
        // Окно без рамки растягивают по просьбе программы, а не за уголок —
        // точки легли бы поверх её рисунка.
        // Рамку стола на телефоне (`fit`) не тянут вовсе: её размер — экран.
        // Развёрнутое окно за уголок не тянут (20.11) — и уголка у него нет.
        // Classic (23.11): the frame is what is dragged, as in Windows 98 -
        // no dots over the content.
        if !self.resizable() || self.fixed_size() || self.frameless || self.fit || self.is_maximized() || Self::classic() {
            return;
        }
        let ctx = self.ctx();
        let p = ctx.palette;
        let grip = self.resize_grip();
        let dot = ctx.px(2).max(1);
        let step = ctx.px(5) as i32;
        let inset = ctx.px(6) as i32;
        for (dx, dy) in [(0, 0), (1, 0), (0, 1)] {
            draw::circle(
                &mut self.surface,
                grip.right() - inset - dx * step,
                grip.bottom() - inset - dy * step,
                dot,
                p.ink6,
                255,
            );
        }
    }

    /// Нарисовать заголовок, кнопки и обводку окна.
    ///
    /// Полоса заголовка — градиент сверху вниз, поверх него светлая кромка в
    /// одну точку и линия под полосой. Три полосы вместо одной заливки: без них
    /// заголовок и содержимое отличаются на две единицы яркости, и граница
    /// между ними не читается вовсе.
    pub fn draw_decorations(&mut self, focused: bool) {
        // Заголовка и обводки у всплывающего окна и окна без рамки нет: меню,
        // список, Chromium рисуют свою рамку сами.
        if self.popup || self.frameless {
            return;
        }
        let ctx = self.ctx();
        let p = ctx.palette;
        let title_h = self.bar();
        let bounds = self.surface.bounds();
        let radius = self.corner_radius();
        let bar = Rect::new(0, 0, bounds.w, title_h);

        // Полоса — прямоугольник без скругления, и это не упущение: верхние
        // углы окна срезает композитор при выводе, потому что за ними видны
        // обои, а он один их знает. Скруглить полосу здесь значило бы срезать
        // угол дважды, а её нижние углы — ещё и там, где под ней лежит не фон,
        // а содержимое.
        //
        // Первая версия обходила это полосой на `radius` выше нужного, и она
        // молча закрашивала четырнадцать верхних точек содержимого: у
        // терминала верхняя строка оказывалась срезанной пополам.
        // Заливка сплошная (72b): у активного окна полоса светлее, у
        // неактивного — цвета окна.
        let classic = Self::classic();
        if classic {
            // Классический заголовок (22.11, Windows 98): серая грань рамки,
            // в ней — синяя полоса с горизонтальным градиентом.
            self.surface.fill(bar, theme::win98::FACE);
            let caption = self.caption_rect();
            mini_ui::classic::caption(&mut self.surface, caption, caption, focused);
        } else {
            let fill = ctx.flat(if focused { p.tb1 } else { p.tb2 });
            self.surface.fill(bar, fill);
            draw::hline(
                &mut self.surface,
                0,
                title_h as i32 - 1,
                bounds.w,
                p.tbline.color,
                p.tbline.alpha,
            );
            if focused {
                draw::crown(&mut self.surface, bounds, radius, p.crown.color, p.crown.alpha);
            }
        }

        // Значок программы: тот же скруглённый квадрат, что у неё на столе и в
        // меню. Он и опознаёт окно быстрее заголовка — цвет читается раньше
        // текста.
        // Вкладки в заголовке (20.11, как Windows Terminal): полоса вкладок и
        // есть заголовок - ни значка, ни имени программы перед ней (они
        // читались как лишняя вкладка «Термин…»).
        let tabs_here = !self.tab_order.is_empty() && !Self::separate_strip();
        let badge_side = ctx.px(16);
        let badge = if classic {
            let caption = self.caption_rect();
            Rect::new(caption.x + ctx.px(2) as i32, caption.y + (caption.h as i32 - badge_side as i32) / 2, badge_side, badge_side)
        } else {
            Rect::new(ctx.px(12) as i32, (title_h as i32 - badge_side as i32) / 2, badge_side, badge_side)
        };
        // Значок программы, как на панели задач (24.10: был цветной квадрат
        // без знака — «квадратик цветной»).
        if !tabs_here {
            self.icon_art().draw(ctx, &mut self.surface, badge);
        }
        self.draw_strip(focused);

        // Кнопки нарисованы всегда, а не только у активного окна: кнопка,
        // появляющаяся при наведении, потребовала бы следить за указателем и
        // перерисовывать заголовок на каждое его движение.
        //
        // Знак «развернуть» меняется вместе с состоянием: развёрнутое окно
        // предлагает вернуть прежний размер, и одинаковый значок в обоих
        // случаях означал бы, что человек нажимает наугад.
        // На телефоне «развернуть» — «на весь экран» (05.11).
        self.paint_title_buttons(focused);

        // Заголовок кончается там, где начинаются кнопки: подпись, заехавшая
        // под крестик, читается как ошибка отрисовки.
        let text_x = badge.right() + ctx.px(if classic { 4 } else { 8 }) as i32;
        let room = (self.leftmost_button().x - ctx.px(if classic { 4 } else { 12 }) as i32 - text_x).max(0) as u32;
        let ink = match (classic, focused) {
            (true, true) => Color::rgb(0xFF, 0xFF, 0xFF),
            (true, false) => theme::win98::GREY_TEXT,
            (false, true) => p.ink,
            (false, false) => p.ink4,
        };
        if !tabs_here {
            let label = self.label();
            // Классика — полужирная белая подпись, как у Windows 98.
            let (role, band) = if classic { (Role::Title, self.caption_rect()) } else { (Role::Body, bar) };
            paint::text_clipped(
                ctx,
                &mut self.surface,
                role,
                text_x,
                paint::baseline(ctx, role, band),
                room,
                &label,
                ink,
            );
        }
        // Обводка окна — последней: она ложится поверх и полосы заголовка, и
        // содержимого, и именно она делает из двух прямоугольников один лист.
        self.paint_edge(focused);

        // Уголок размера: три точки в правом нижнем углу. Без нарисованного
        // признака за него никто не потянет — угадывать, что окно где-то
        // тянется, человек не обязан.
        //
        // Уголок рисуется, но **не** помечается изменившимся: он не зависит ни
        // от фокуса, ни от содержимого. Пометить его вместе с заголовком стоило
        // бы всей площади окна — прямоугольник изменений один, и объединение
        // верхней полосы с нижним углом накрывает окно целиком. На 1920×1080
        // это превращало переключение окон в перерисовку всего экрана: клавиша
        // Tab обрабатывалась дольше пяти секунд.
        self.draw_grip();

        // Изменилась только полоса сверху — её и помечаем. Пометить всё окно
        // было бы проще на одну строку и дороже на площадь окна при каждом
        // переключении фокуса.
        self.damage = self.damage.union(&Rect::new(0, 0, bounds.w, title_h));
        // Обводка идёт по всему периметру, и без её краёв неактивное окно
        // осталось бы с яркой рамкой активного. Три полосы в одну точку стоят
        // ничтожно мало по сравнению с площадью окна.
        self.damage = self.damage.union(&Rect::new(0, bounds.h as i32 - 1, bounds.w, 1));
        self.damage = self.damage.union(&Rect::new(0, 0, 1, bounds.h));
        self.damage = self.damage.union(&Rect::new(bounds.w as i32 - 1, 0, 1, bounds.h));
    }

    /// Кнопки заголовка: плоские знаки (72b, макет 13), у неактивного окна —
    /// приглушённые; под указателем — плашка наведения, у крестика —
    /// красная.
    fn paint_title_buttons(&mut self, focused: bool) {
        let ctx = self.ctx();
        let p = ctx.palette;
        let fill = ctx.flat(if focused { p.tb1 } else { p.tb2 });
        let restore_icon = if self.restore.is_some() { Icon::Restore } else { Icon::Maximize };
        let glyph = ctx.px(10);
        let keyboard = self.has_keyboard_button().then(|| (self.keyboard_button(), Icon::Keyboard, Hit::Keyboard));
        // Диалог (20.11) — один крестик, как «Выполнить» в Windows.
        let sizing = (!self.dialog).then(|| {
            [
                (self.minimize_button(), Icon::Minimize, Hit::Minimize),
                (self.maximize_button(), restore_icon, Hit::Maximize),
            ]
        });
        let classic = Self::classic();
        let bar = Rect::new(0, 0, self.surface.width(), Self::title_height(self.scale));
        for (button, icon, hit) in keyboard.into_iter().chain(sizing.into_iter().flatten()).chain([(self.close_button(), Icon::Close, Hit::Close)]) {
            let hot = self.hot == Some(hit);
            // «Развернуть» of a window that cannot change size (24.11) — greyed.
            let disabled = hit == Hit::Maximize && !self.can_maximize();
            if classic && hit != Hit::Keyboard {
                // Кнопка Windows 98 16×14 (22.11): выпуклая серая, знак
                // чёрный; под ней — тот же градиент заголовка.
                let caption = self.caption_rect();
                let _ = bar;
                mini_ui::classic::caption(&mut self.surface, button.intersect(&caption), caption, focused);
                let knob = self.classic_knob(button);
                let t = mini_ui::classic::unit(self.scale);
                mini_ui::classic::button(&mut self.surface, knob, t, false);
                let u = ctx.px(1).max(1);
                if disabled {
                    // Windows 98 disabled glyph: white emboss under a grey one.
                    let shifted = Rect::new(knob.x + u as i32, knob.y + u as i32, knob.w, knob.h);
                    classic_glyph(&mut self.surface, shifted, hit, self.restore.is_some(), u, Color::rgb(0xFF, 0xFF, 0xFF));
                    classic_glyph(&mut self.surface, knob, hit, self.restore.is_some(), u, Color::rgb(0x80, 0x80, 0x80));
                } else {
                    classic_glyph(&mut self.surface, knob, hit, self.restore.is_some(), u, Color::rgb(0, 0, 0));
                }
                let _ = hot;
                continue;
            }
            // Нажатая кнопка клавиатуры — цветом акцента: видно, что
            // клавиатура открыта для этого окна.
            let (plate, ink) = match (hot, hit) {
                (true, Hit::Close) => (p.bad, Color::rgb(0xFF, 0xFF, 0xFF)),
                (_, Hit::Keyboard) if self.keyboard => (p.acc, Color::rgb(0xFF, 0xFF, 0xFF)),
                _ if disabled => (fill, if focused { p.ink5 } else { p.line3.color }),
                (true, _) => (p.btnhov.over(fill), p.ink),
                (false, _) => (fill, if focused { p.ink2 } else { p.ink4 }),
            };
            self.surface.fill(button, plate);
            glyphicon::draw(
                &mut self.surface,
                icon,
                button.x + (button.w as i32 - glyph as i32) / 2,
                button.y + (button.h as i32 - glyph as i32) / 2,
                glyph,
                ink,
                255,
            );
        }
    }

    /// Подсветить кнопку заголовка под указателем (`None` — никакую).
    /// Перерисовываются только кнопки и обводка окна над ними.
    pub fn set_hot(&mut self, hot: Option<Hit>, focused: bool) {
        let hot = hot.filter(|hit| matches!(hit, Hit::Close | Hit::Maximize | Hit::Minimize | Hit::Keyboard));
        if self.hot == hot || self.popup || self.frameless {
            return;
        }
        self.hot = hot;
        self.paint_title_buttons(focused);
        // Обводка — заново, но на основу, а не поверх прежней (см.
        // [`EdgeBook`]): кнопки только что залиты, остальной периметр
        // остаётся каким был. The classic frame lies apart from the
        // buttons (23.11) and stays as it is.
        if !Self::classic() {
            self.paint_edge(focused);
        }
        let row = self.leftmost_button().union(&self.close_button());
        self.damage = self.damage.union(&row);
    }

    /// Положить обводку окна на основу (см. [`EdgeBook`]) цветом активного
    /// или неактивного окна. Изменившиеся точки попадают в `damage`.
    fn paint_edge(&mut self, focused: bool) {
        if self.popup || self.frameless {
            return;
        }
        if Self::classic() {
            self.paint_classic_frame(focused);
            return;
        }
        let ctx = self.ctx();
        let p = ctx.palette;
        let radius = self.corner_radius();
        let ink = if focused { p.line3 } else { p.line2 };
        let bounds = self.surface.bounds();
        let (w, h) = (bounds.w, bounds.h);
        let Self { surface, edge: book, .. } = &mut *self;
        book.focused = focused;
        if (book.w, book.h, book.radius) != (w, h, radius) {
            let mut count = 0usize;
            edge_points(w, h, radius, |_, _| count += 1);
            book.base.clear();
            book.drawn.clear();
            if book.base.try_reserve_exact(count).is_err() || book.drawn.try_reserve_exact(count).is_err() {
                // Памяти на учёт нет — обводка как раньше, просто поверх.
                book.w = 0;
                draw::rounded_stroke(surface, bounds, radius, ink.color, ink.alpha);
                return;
            }
            book.w = w;
            book.h = h;
            book.radius = radius;
        }
        // Первый проход: основа под каждой точкой — прежняя, если точка
        // хранит нарисованное обводкой, иначе её нынешнее значение.
        // `drawn` до второго прохода хранит то, что было в точке до вызова.
        let mut i = 0usize;
        edge_points(w, h, radius, |x, y| {
            let now = surface.get(x, y);
            if i >= book.base.len() {
                book.base.push(now);
                book.drawn.push(now);
            } else if now != book.drawn[i] {
                book.base[i] = now;
            }
            book.drawn[i] = now;
            surface.row_mut(y)[x as usize] = book.base[i];
            i += 1;
        });
        draw::rounded_stroke(surface, bounds, radius, ink.color, ink.alpha);
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        let mut i = 0usize;
        edge_points(w, h, radius, |x, y| {
            let now = surface.get(x, y);
            if now != book.drawn[i] {
                let (x, y) = (x as i32, y as i32);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            book.drawn[i] = now;
            i += 1;
        });
        if x0 <= x1 {
            let changed = Rect::new(x0, y0, (x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
            self.damage = self.damage.union(&changed);
        }
    }

    /// Радиус углов окна: развёрнутое и полноэкранное стоят углами в углы
    /// экрана, и скругления у них нет — ни в обводке, ни в кромке (20.11).
    fn corner_radius(&self) -> u32 {
        // Телефон — как был: там окна всегда во весь экран и со скруглением.
        if self.fullscreen || (self.is_maximized() && !theme::is_mobile()) { 0 } else { super::look::corner(self.ctx().px(theme::R_WINDOW)) }
    }

    /// Классические рамки (20.11, «Оформление»; на телефоне — нет).
    fn classic() -> bool {
        super::look::on(super::look::Flag::Classic) && !theme::is_mobile()
    }

    /// The Windows 98 window frame (23.11), OUTSIDE the content: the outer
    /// bevel row #DFDFDF (top, left) / black (bottom, right), the inner one
    /// white / #808080, then face rows up to the content and the caption.
    /// Nothing of the content lies under it, so it is simply drawn (no
    /// [`EdgeBook`]). A maximized or fullscreen window has no frame.
    fn paint_classic_frame(&mut self, focused: bool) {
        self.edge.focused = focused;
        // The book does not cover this frame: after a switch back to the
        // modern look it starts afresh.
        self.edge.w = 0;
        let frame = self.frame();
        if frame == 0 {
            return;
        }
        let t = mini_ui::classic::unit(self.scale);
        let bounds = self.surface.bounds();
        let (w, h) = (bounds.w, bounds.h);
        let f = frame as i32;
        let strips = [
            Rect::new(0, 0, w, frame),
            Rect::new(0, 0, frame, h),
            Rect::new(w as i32 - f, 0, frame, h),
            Rect::new(0, h as i32 - f, w, frame),
        ];
        for strip in strips {
            self.surface.fill(strip, theme::win98::FACE);
        }
        mini_ui::classic::raised(&mut self.surface, bounds, t);
        for strip in strips {
            self.damage = self.damage.union(&strip);
        }
    }

    /// Синяя полоса классического заголовка (23.11): внутри рамки, 18 точек
    /// высотой; у окна без рамки (развёрнутого) — от края до края сверху.
    fn caption_rect(&self) -> Rect {
        let ctx = self.ctx();
        let inset = self.frame();
        let h = (CLASSIC_CAPTION * self.scale).saturating_sub(ctx.px(1));
        Rect::new(inset as i32, inset as i32, self.surface.width().saturating_sub(inset * 2), h)
    }

    /// Нарисованная кнопка классики внутри места нажатия `button`: 16×14 по
    /// вертикали посередине синей полосы.
    fn classic_knob(&self, button: Rect) -> Rect {
        let ctx = self.ctx();
        let caption = self.caption_rect();
        let h = ctx.px(CLASSIC_BTN_H).min(caption.h);
        Rect::new(button.x, caption.y + (caption.h as i32 - h as i32) / 2, button.w, h)
    }

    /// Задевает ли прямоугольник поверхности место обводки (с запасом на
    /// углы): тогда после копии пикселей её надо положить заново.
    fn touches_edge(&self, rect: Rect) -> bool {
        // The classic frame (23.11) lies outside the content: copies never
        // reach it.
        if self.popup || self.frameless || rect.is_empty() || Self::classic() {
            return false;
        }
        let c = (self.corner_radius() + 1) as i32;
        let (w, h) = (self.surface.width() as i32, self.surface.height() as i32);
        rect.x < c || rect.y < c || rect.right() > w - c || rect.bottom() > h - c
    }

    /// Перекрасить окно под текущую тему.
    ///
    /// Заголовок перерисует тот, кто знает, активно ли окно, — здесь только
    /// содержимое. Разделение не косметическое: фокус живёт у композитора, и
    /// окно, взявшееся угадывать его само, ошибётся на первом же переключении.
    pub fn restyle(&mut self) {
        let (bar, side) = (self.bar(), self.frame());
        match &mut self.content {
            Content::Text(grid) => {
                // Заливается вся область содержимого, а не только сетка: в
                // высоту окна редко помещается целое число строк, и остаток
                // внизу — несколько точек, а то и полтора десятка — иначе
                // остаётся полосой прежней темы под самой рамкой. Цвета
                // терминала от темы не зависят (T5).
                let area = content_area(&self.surface, bar, side);
                self.surface.fill(area, mini_ui::text::TERM_BG);
                grid.recolor(&mut self.surface, mini_ui::text::TERM_FG, mini_ui::text::TERM_BG);
                grid.take_damage();
                for tab in &mut self.tabs {
                    tab.grid.recolor(&mut self.surface, mini_ui::text::TERM_FG, mini_ui::text::TERM_BG);
                }
                self.damage = self.damage.union(&area);
            }
            Content::Settings(_) | Content::Dialog(_) => self.redraw_content(),
            // Окно программы темы не знает и знать не может: его пиксели
            // написала программа, и перекрасить их ядру нечем. Заголовок
            // перерисует композитор, а содержимое останется прежним — это
            // названный предел, а не упущение. Сказать программе «сменилась
            // тема» договор пока не умеет, и выдумывать событие ради одной
            // перекраски значило бы завести половину протокола.
            Content::Program(_) => {}
        }
    }

    /// Вкладки терминала в порядке полосы (номера сессий).
    #[must_use]
    pub fn tab_sessions(&self) -> &[usize] {
        &self.tab_order
    }

    /// Сессия видимой вкладки.
    #[must_use]
    pub const fn active_session(&self) -> usize {
        self.session
    }

    /// Есть ли у окна полоса вкладок.
    #[must_use]
    pub fn has_tabs(&self) -> bool {
        !self.tab_order.is_empty()
    }

    /// Добавить вкладку сессии `session` и показать её. `false` — не
    /// терминал или нет памяти под сетку.
    pub fn add_tab(&mut self, session: usize) -> bool {
        if self.tab_order.is_empty() || self.tab_order.contains(&session) {
            return false;
        }
        let ctx = self.ctx();
        let area = grid_area(&self.surface, self.scale, true, self.bar(), self.frame());
        let Some(mut grid) = TextGrid::new(area, ctx.face(Role::Mono), mini_ui::text::TERM_FG, mini_ui::text::TERM_BG) else {
            return false;
        };
        grid.hide();
        self.tabs.push(TermTab { session, grid });
        self.tab_order.push(session);
        self.switch_tab(session);
        true
    }

    /// Показать вкладку сессии `session`.
    pub fn switch_tab(&mut self, session: usize) -> bool {
        if session == self.session {
            return false;
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.session == session) else {
            return false;
        };
        let Content::Text(current) = &mut self.content else {
            return false;
        };
        let mut incoming = self.tabs.swap_remove(index);
        core::mem::swap(current, &mut incoming.grid);
        // `incoming.grid` — бывшая видимая.
        incoming.grid.hide();
        incoming.session = self.session;
        self.tabs.push(incoming);
        self.session = session;
        let area = content_area(&self.surface, self.bar() + self.strip_h(), self.frame());
        self.surface.fill(area, mini_ui::text::TERM_BG);
        if let Content::Text(grid) = &mut self.content {
            grid.show(&mut self.surface);
            grid.take_damage();
        }
        self.damage = self.damage.union(&area);
        self.draw_strip(true);
        true
    }

    /// Убрать вкладку сессии `session`; видимую сменяет соседняя.
    pub fn remove_tab(&mut self, session: usize) -> bool {
        let Some(at) = self.tab_order.iter().position(|item| *item == session) else {
            return false;
        };
        if self.tab_order.len() == 1 {
            return false;
        }
        if session == self.session {
            let next = if at + 1 < self.tab_order.len() { self.tab_order[at + 1] } else { self.tab_order[at - 1] };
            self.switch_tab(next);
        }
        self.tabs.retain(|tab| tab.session != session);
        self.tab_order.remove(at);
        self.tab_titles.retain(|(item, _)| *item != session);
        self.renamed.retain(|item| *item != session);
        if self.tab_edit.as_ref().is_some_and(|(item, _)| *item == session) {
            self.tab_edit = None;
        }
        self.draw_strip(true);
        true
    }

    /// Вынуть вкладку `session` из окна (20.11): унести в новое окно или в
    /// полосу другого. Последнюю не вынуть - окно без вкладок не бывает.
    pub fn take_tab(&mut self, session: usize) -> Option<LooseTab> {
        let at = self.tab_order.iter().position(|item| *item == session)?;
        if self.tab_order.len() < 2 {
            return None;
        }
        // Видимую сначала сменяет соседняя - так её сетка оказывается среди
        // скрытых, откуда её и берут.
        if session == self.session {
            let next = if at + 1 < self.tab_order.len() { self.tab_order[at + 1] } else { self.tab_order[at - 1] };
            self.switch_tab(next);
        }
        let index = self.tabs.iter().position(|tab| tab.session == session)?;
        let tab = self.tabs.swap_remove(index);
        let title = self.tab_titles.iter().find(|(item, _)| *item == session).map(|(_, title)| title.clone());
        let renamed = self.renamed.contains(&session);
        self.tab_order.retain(|item| *item != session);
        self.tab_titles.retain(|(item, _)| *item != session);
        self.renamed.retain(|item| *item != session);
        if self.tab_edit.as_ref().is_some_and(|(item, _)| *item == session) {
            self.tab_edit = None;
        }
        self.draw_strip(true);
        Some(LooseTab { session, grid: tab.grid, title, renamed })
    }

    /// Вынуть единственную вкладку окна (её бросили на полосу другого окна):
    /// окно остаётся без вкладок и сетки - его закрывают следом.
    pub fn take_last_tab(&mut self) -> Option<LooseTab> {
        if self.tab_order.len() != 1 {
            return None;
        }
        let session = self.session;
        let ctx = self.ctx();
        let area = grid_area(&self.surface, self.scale, true, self.bar(), self.frame());
        let blank = TextGrid::new(area, ctx.face(Role::Mono), mini_ui::text::TERM_FG, mini_ui::text::TERM_BG)?;
        let Content::Text(current) = &mut self.content else {
            return None;
        };
        let mut grid = core::mem::replace(current, blank);
        grid.hide();
        let title = self.tab_titles.iter().find(|(item, _)| *item == session).map(|(_, title)| title.clone());
        let renamed = self.renamed.contains(&session);
        self.tab_order.clear();
        self.tab_titles.clear();
        self.renamed.clear();
        self.tab_edit = None;
        Some(LooseTab { session, grid, title, renamed })
    }

    /// Принять вынутую вкладку в конец полосы и показать её. Не вышло (не
    /// терминал, такая уже есть) - вкладка возвращается вызывающему.
    pub fn put_tab(&mut self, tab: LooseTab) -> Result<(), LooseTab> {
        if self.tab_order.is_empty() || self.tab_order.contains(&tab.session) {
            return Err(tab);
        }
        let LooseTab { session, mut grid, title, renamed } = tab;
        grid.hide();
        // Сетка привязана к точкам прежнего окна: здесь у неё своя область.
        let area = grid_area(&self.surface, self.scale, true, self.bar(), self.frame());
        let _ = grid.rebind(&mut self.surface, area);
        self.tabs.push(TermTab { session, grid });
        self.tab_order.push(session);
        if let Some(title) = title {
            self.tab_titles.push((session, title));
        }
        if renamed {
            self.renamed.push(session);
        }
        self.switch_tab(session);
        Ok(())
    }

    /// Сделать только что созданное окно терминала окном одной вкладки
    /// `tab` (её унесли из полосы). Прежняя единственная вкладка нового окна
    /// - пустая сетка - выбрасывается.
    pub fn adopt_only(&mut self, tab: LooseTab) -> Result<(), LooseTab> {
        if !self.app.is_terminal() || !matches!(self.content, Content::Text(_)) {
            return Err(tab);
        }
        let LooseTab { session, mut grid, title, renamed } = tab;
        let area = grid_area(&self.surface, self.scale, true, self.bar(), self.frame());
        let _ = grid.rebind(&mut self.surface, area);
        let back = content_area(&self.surface, self.bar(), self.frame());
        self.surface.fill(back, mini_ui::text::TERM_BG);
        if let Content::Text(current) = &mut self.content {
            *current = grid;
            current.show(&mut self.surface);
            current.take_damage();
        }
        self.tabs.clear();
        self.tab_order.clear();
        self.tab_order.push(session);
        self.session = session;
        self.tab_titles.clear();
        self.renamed.clear();
        self.tab_edit = None;
        if let Some(title) = title {
            self.tab_titles.push((session, title));
        }
        if renamed {
            self.renamed.push(session);
        }
        self.damage = self.damage.union(&back);
        self.draw_strip(true);
        Ok(())
    }

    /// Переставить вкладку `session` на место `to` в полосе (её тянут вдоль
    /// полосы мышью).
    pub fn move_tab(&mut self, session: usize, to: usize) -> bool {
        let Some(at) = self.tab_order.iter().position(|item| *item == session) else {
            return false;
        };
        let to = to.min(self.tab_order.len() - 1);
        if at == to {
            return false;
        }
        let item = self.tab_order.remove(at);
        self.tab_order.insert(to, item);
        self.draw_strip(true);
        true
    }

    /// Место в полосе (номер по порядку) под точкой экрана `x` - по
    /// горизонтали, высоту не спрашивая: вкладку тянут вдоль полосы, и
    /// рука уходит немного вверх или вниз.
    #[must_use]
    pub fn tab_slot_at(&self, x: i32) -> Option<usize> {
        if self.tab_order.is_empty() {
            return None;
        }
        let local = x - self.rect.x;
        let (tabs, _) = self.strip_layout();
        tabs.iter().position(|(_, tab, _)| local >= tab.x && local < tab.right())
    }

    /// Лежит ли точка экрана на полосе вкладок этого окна (сюда бросают
    /// вкладку из другого окна).
    #[must_use]
    pub fn strip_contains(&self, x: i32, y: i32) -> bool {
        if self.tab_order.is_empty() || self.popup || self.frameless || self.minimized || !self.rect.contains(x, y) {
            return false;
        }
        let (lx, ly) = (x - self.rect.x, y - self.rect.y);
        let top = if Self::separate_strip() { self.bar() as i32 } else { 0 };
        let h = if Self::separate_strip() { self.ctx().px(STRIP_H) } else { self.bar() };
        ly >= top && ly < top + h as i32 && lx < self.minimize_button().x
    }

    /// Начать правку имени вкладки прямо в полосе («Переименовать»).
    pub fn begin_rename(&mut self, session: usize) -> bool {
        if !self.tab_order.contains(&session) {
            return false;
        }
        self.tab_edit = Some((session, self.tab_title(session)));
        self.tab_edit_all = true;
        self.draw_strip(true);
        true
    }

    /// Правят ли сейчас имя вкладки.
    #[must_use]
    pub const fn renaming(&self) -> bool {
        self.tab_edit.is_some()
    }

    /// Клавиша, пока правят имя вкладки: Enter - принять, Esc - отменить,
    /// Backspace - стереть знак, знак - дописать.
    pub fn rename_key(&mut self, code: KeyCode, ch: Option<char>) -> RenameStep {
        let Some((session, text)) = self.tab_edit.as_mut() else {
            return RenameStep::Cancelled;
        };
        let session = *session;
        let all = core::mem::take(&mut self.tab_edit_all);
        match code {
            KeyCode::Enter => {
                let name = self.commit_rename().map_or_else(String::new, |(_, name)| name);
                return RenameStep::Done(session, name);
            }
            KeyCode::Escape => {
                self.tab_edit = None;
                self.draw_strip(true);
                return RenameStep::Cancelled;
            }
            KeyCode::Backspace if all => text.clear(),
            KeyCode::Backspace => {
                text.pop();
            }
            _ => match ch {
                Some(ch) if !ch.is_control() && text.chars().count() < 64 => {
                    if all {
                        text.clear();
                    }
                    text.push(ch);
                }
                _ => {
                    self.draw_strip(true);
                    return RenameStep::Typing;
                }
            },
        }
        self.draw_strip(true);
        RenameStep::Typing
    }

    /// Принять набранное имя (Enter или щелчок мимо). Пустое имя - вкладку
    /// снова называет то, что в ней идёт.
    pub fn commit_rename(&mut self) -> Option<(usize, String)> {
        let (session, text) = self.tab_edit.take()?;
        let name = String::from(text.trim());
        if name.is_empty() {
            self.renamed.retain(|item| *item != session);
        } else {
            match self.tab_titles.iter_mut().find(|(item, _)| *item == session) {
                Some((_, old)) => *old = name.clone(),
                None => self.tab_titles.push((session, name.clone())),
            }
            if !self.renamed.contains(&session) {
                self.renamed.push(session);
            }
        }
        self.draw_strip(true);
        Some((session, name))
    }

    /// Выполнить `action` над сеткой сессии `session`, видимой или нет: на
    /// время действия её сетка становится содержимым окна, а скрытая ничего
    /// не рисует.
    pub fn with_session<R>(&mut self, session: usize, action: impl FnOnce(&mut Self) -> R) -> R {
        if session == self.session || self.tab_order.is_empty() {
            return action(self);
        }
        let Some(index) = self.tabs.iter().position(|tab| tab.session == session) else {
            return action(self);
        };
        let Content::Text(current) = &mut self.content else {
            return action(self);
        };
        core::mem::swap(current, &mut self.tabs[index].grid);
        let result = action(self);
        if let Content::Text(current) = &mut self.content {
            core::mem::swap(current, &mut self.tabs[index].grid);
        }
        result
    }

    /// Подпись вкладки сессии `session` (что в ней идёт).
    pub fn set_tab_title(&mut self, session: usize, title: &str) {
        // Имя, данное человеком, программа не перекрывает (20.11).
        if self.renamed.contains(&session) {
            return;
        }
        match self.tab_titles.iter_mut().find(|(item, _)| *item == session) {
            Some((_, old)) if old == title => return,
            Some((_, old)) => *old = String::from(title),
            None => self.tab_titles.push((session, String::from(title))),
        }
        self.draw_strip(true);
    }

    fn tab_title(&self, session: usize) -> String {
        self.tab_titles
            .iter()
            .find(|(item, _)| *item == session)
            .map_or_else(|| alloc::format!("Терминал {}", session + 1), |(_, title)| title.clone())
    }

    /// Высота своей полосы вкладок под заголовком: на ПК вкладки в заголовке.
    fn strip_h(&self) -> u32 {
        if self.tab_order.is_empty() || !Self::separate_strip() { 0 } else { self.ctx().px(STRIP_H) }
    }

    /// Где на полосе вкладки (сессия, вкладка, крестик) и «+».
    fn strip_layout(&self) -> (Vec<(usize, Rect, Rect)>, Rect) {
        let ctx = self.ctx();
        // ПК (72h, макет 15): вкладки в заголовке — после значка и подписи,
        // до кнопок окна. Телефон — своей полосой под заголовком.
        let in_title = !Self::separate_strip();
        let (top, h, left, right) = if in_title {
            // С 20.11 полоса и есть заголовок: вкладки от левого края.
            let title = Self::title_height(self.scale);
            (0, title, ctx.px(8), (self.minimize_button().x - ctx.px(8) as i32).max(0) as u32)
        } else {
            // Classic (23.11): under the title band, inside the frame.
            let side = self.frame();
            (self.bar() as i32, ctx.px(STRIP_H), side + ctx.px(6), self.surface.width().saturating_sub(side + ctx.px(6)))
        };
        let plus_w = ctx.px(28);
        let count = self.tab_order.len().max(1) as u32;
        let room = right.saturating_sub(left + plus_w + ctx.px(4));
        let width = (room / count).min(ctx.px(TAB_W)).max(ctx.px(60));
        let mut out = Vec::new();
        let mut x = left as i32;
        for session in &self.tab_order {
            let tab = Rect::new(x, top + ctx.px(3) as i32, width, h - ctx.px(3));
            let side = ctx.px(16);
            let close = Rect::new(tab.right() - (side + ctx.px(6)) as i32, tab.y + (tab.h as i32 - side as i32) / 2, side, side);
            out.push((*session, tab, close));
            x += (width + ctx.px(2)) as i32;
        }
        let plus = Rect::new(x + ctx.px(2) as i32, top + ctx.px(4) as i32, plus_w, h - ctx.px(8));
        (out, plus)
    }

    /// Нарисовать полосу вкладок: видимая — цвета терминала, остальные —
    /// приглушённые; крестик у каждой, «+» в конце.
    fn draw_strip(&mut self, focused: bool) {
        if self.tab_order.is_empty() || self.popup || self.frameless {
            return;
        }
        let ctx = self.ctx();
        let p = ctx.palette;
        let strip = if Self::separate_strip() {
            let (top, side) = (self.bar() as i32, self.frame());
            let strip = Rect::new(side as i32, top, self.surface.width().saturating_sub(2 * side), ctx.px(STRIP_H));
            self.surface.fill(strip, ctx.flat(p.tb2));
            strip
        } else {
            // В заголовке: место вкладок заливается цветом полосы заново —
            // вкладки меняются чаще заголовка.
            let (tabs, plus) = self.strip_layout();
            let from = tabs.first().map_or(plus.x, |(_, tab, _)| tab.x);
            let strip = Rect::new(from, 0, (self.minimize_button().x - from).max(0) as u32, Self::title_height(self.scale));
            self.surface.fill(strip, ctx.flat(if focused { p.tb1 } else { p.tb2 }));
            strip
        };
        let (tabs, plus) = self.strip_layout();
        for (session, tab, close) in tabs {
            let active = session == self.session;
            if active {
                // Видимая вкладка сливается с терминалом под ней: скругление
                // сверху, низ прямой. Не ниже полосы (20.11): прямоугольник
                // «на px(8) ниже вкладки» залезал в сетку и срезал верх первой
                // строки терминала — на 200 % на 3–4 точки.
                draw::rounded(&mut self.surface, tab, ctx.px(6), mini_ui::text::TERM_BG, 255);
                let half = tab.h / 2;
                self.surface.fill(Rect::new(tab.x, tab.y + half as i32, tab.w, tab.h - half), mini_ui::text::TERM_BG);
                if !Self::separate_strip() {
                    let bottom = Self::title_height(self.scale) as i32;
                    self.surface.fill(Rect::new(tab.x, bottom - ctx.px(2) as i32, tab.w, ctx.px(2)), mini_ui::text::TERM_BG);
                }
            }
            // Имя правят прямо во вкладке: рамка акцентом и курсор в конце.
            let editing = self.tab_edit.as_ref().filter(|(item, _)| *item == session).map(|(_, text)| text.clone());
            let title = editing.clone().unwrap_or_else(|| self.tab_title(session));
            let ink = if active || editing.is_some() { mini_ui::text::TERM_FG } else { p.ink4 };
            let glyph = ctx.px(12);
            glyphicon::draw(
                &mut self.surface,
                Icon::Terminal,
                tab.x + ctx.px(8) as i32,
                tab.y + (tab.h as i32 - glyph as i32) / 2,
                glyph,
                ink,
                255,
            );
            let text_x = tab.x + ctx.px(26) as i32;
            paint::text_clipped(
                ctx,
                &mut self.surface,
                Role::Body,
                text_x,
                paint::baseline(ctx, Role::Body, tab),
                (close.x - text_x - ctx.px(4) as i32).max(0) as u32,
                &title,
                ink,
            );
            if editing.is_some() {
                let field = Rect::new(text_x - ctx.px(4) as i32, tab.y + ctx.px(4) as i32, (close.x - text_x).max(0) as u32, tab.h.saturating_sub(ctx.px(8)));
                draw::rounded_stroke(&mut self.surface, field, ctx.px(4), p.acc, 255);
                let room = (close.x - text_x - ctx.px(4) as i32).max(0) as u32;
                // Только начали - имя выделено целиком: набор его заменит.
                if self.tab_edit_all {
                    let width = ctx.face(Role::Body).width(&title).min(room);
                    let mark = Rect::new(text_x - ctx.px(1) as i32, field.y + ctx.px(3) as i32, width + ctx.px(2), field.h.saturating_sub(ctx.px(6)));
                    draw::blend_rect(&mut self.surface, mark, p.acc, 110);
                    paint::text_clipped(ctx, &mut self.surface, Role::Body, text_x, paint::baseline(ctx, Role::Body, tab), room, &title, mini_ui::text::TERM_FG);
                }
                let caret = text_x + ctx.face(Role::Body).width(&title).min(room) as i32 + 1;
                self.surface.fill(Rect::new(caret, field.y + ctx.px(4) as i32, ctx.px(1).max(1), field.h.saturating_sub(ctx.px(8))), mini_ui::text::TERM_FG);
            }
            let cross = ctx.px(10);
            glyphicon::draw(
                &mut self.surface,
                Icon::Close,
                close.x + (close.w as i32 - cross as i32) / 2,
                close.y + (close.h as i32 - cross as i32) / 2,
                cross,
                p.ink4,
                255,
            );
        }
        // «+» — две черты: в наборе значков плюса нет.
        let arm = ctx.px(10) as i32;
        let (cx, cy) = (plus.x + plus.w as i32 / 2, plus.y + plus.h as i32 / 2);
        let thick = ctx.px(2).max(1);
        self.surface.fill(Rect::new(cx - arm / 2, cy - thick as i32 / 2, arm as u32, thick), p.ink3);
        self.surface.fill(Rect::new(cx - thick as i32 / 2, cy - arm / 2, thick, arm as u32), p.ink3);
        self.damage = self.damage.union(&strip);
    }

    /// Значок программы окна: свой у программы (из пакета или по таблице),
    /// у окон ядра — значок набора на плитке.
    #[must_use]
    pub fn icon_art(&self) -> super::appicon::Art {
        self.art.clone().unwrap_or_else(|| {
            if self.app == App::About {
                if let Some(logo) = super::appicon::logo_tile() {
                    return super::appicon::Art::Image(logo);
                }
            }
            let terminal = self.app.is_terminal();
            super::appicon::Art::Glyph(if terminal { Icon::Terminal } else { self.app.icon() }, self.app.tone(), terminal)
        })
    }

    /// Напечатать в окно. Действует только на текстовые окна.
    pub fn write_str(&mut self, text: &str) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.write_str(&mut self.surface, text);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Очистить содержимое.
    pub fn clear(&mut self) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.clear(&mut self.surface);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Показывать ли курсор.
    pub fn set_cursor(&mut self, visible: bool) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.set_cursor(&mut self.surface, visible);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Поставить курсор в заданную ячейку. Нумерация с нуля.
    ///
    /// Дальше идут пять методов, которыми пользуется разбор управляющих
    /// последовательностей ([`super::term`]). Каждый — одна строка делегирования,
    /// и это не бесполезная прослойка: содержимое окна бывает не только текстом,
    /// а разбор ANSI обязан молча ничего не делать над списком файлов, а не
    /// разбираться, что там внутри.
    pub fn term_move_to(&mut self, row: u32, col: u32) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.move_to(&mut self.surface, row, col);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Сдвинуть курсор на заданное число строк и столбцов.
    pub fn term_move_by(&mut self, rows: i32, cols: i32) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.move_by(&mut self.surface, rows, cols);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Стереть часть экрана: `0` — вниз от курсора, `1` — вверх, `2` — весь.
    pub fn term_erase_display(&mut self, mode: u8) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.erase_display(&mut self.surface, mode);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Стереть часть строки.
    pub fn term_erase_line(&mut self, mode: u8) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.erase_line(&mut self.surface, mode);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Цвет текста для последующего вывода.
    pub fn term_set_fg(&mut self, index: u8) {
        if let Content::Text(grid) = &mut self.content {
            grid.set_fg(index);
        }
    }

    /// Цвет фона для последующего вывода.
    pub fn term_set_bg(&mut self, index: u8) {
        if let Content::Text(grid) = &mut self.content {
            grid.set_bg(index);
        }
    }

    /// Вернуть тексту цвет окна.
    pub fn term_default_fg(&mut self) {
        if let Content::Text(grid) = &mut self.content {
            grid.default_fg();
        }
    }

    /// Вернуть фону цвет окна.
    pub fn term_default_bg(&mut self) {
        if let Content::Text(grid) = &mut self.content {
            grid.default_bg();
        }
    }

    /// Включить или выключить инверсию.
    pub fn term_set_inverse(&mut self, on: bool) {
        if let Content::Text(grid) = &mut self.content {
            grid.set_inverse(on);
        }
    }

    /// Вернуть цвета окна.
    pub fn term_reset_attr(&mut self) {
        if let Content::Text(grid) = &mut self.content {
            grid.reset_attr();
        }
    }

    /// Размер сетки в символах. Для нетекстовых окон — нули.
    #[must_use]
    pub fn size_in_cells(&self) -> (u32, u32) {
        match &self.content {
            Content::Text(grid) => (grid.cols(), grid.rows()),
            Content::Settings(_) | Content::Dialog(_) | Content::Program(_) => (0, 0),
        }
    }

    /// Передать клавишу содержимому. `true` — окно её обработало.
    pub fn handle_key(&mut self, code: KeyCode) -> bool {
        self.handle_typed(code, None, false)
    }

    /// То же, что [`Self::handle_key`], но с символом, который клавиша даёт в
    /// текущей раскладке: «Параметрам» он нужен для поля поиска (фаза 69b).
    /// `ctrl` — зажат ли Ctrl: Ctrl+стрелки двигают монитор в «Дисплее».
    pub fn handle_typed(&mut self, code: KeyCode, ch: Option<char>, ctrl: bool) -> bool {
        match &mut self.content {
            Content::Settings(view) => {
                if !view.handle(code, ch, ctrl) {
                    return false;
                }
                self.redraw_content();
                true
            }
            Content::Dialog(view) => {
                if !view.handle(code) {
                    return false;
                }
                self.redraw_content();
                true
            }
            // Терминал получает события не здесь: их разбирает редактор строки
            // в задаче оболочки, потому что набираемая строка — состояние
            // оболочки, а не окна.
            Content::Text(_) => false,
            // Окно программы — тоже не здесь, и по другой причине: сюда
            // приезжает код клавиши, а программе нужен символ, который зависит
            // от модификаторов. Модификаторы есть только у оконного менеджера
            // ([`super::route`]), он событие и кладёт в очередь.
            Content::Program(_) => false,
        }
    }

    /// Перерисовать содержимое, которое рисует себя само.
    pub fn redraw_content(&mut self) {
        // Сетку терминала рисует она сама, и заливать её нельзя — выход раньше
        // заливки, а не после. Дефект, найденный в фазе С9: `rebuild` звал эту
        // функцию сразу за `TextGrid::rebind`, заливка стирала только что
        // перенесённые знаки, а теневой буфер сетки продолжал считать их
        // нарисованными. Оболочка после разворачивания теряла напечатанное с
        // экрана, а `mc`, присылающий только изменившиеся ячейки, получал
        // тёмные дыры там, где новый кадр совпал со старым.
        if matches!(self.content, Content::Text(_)) {
            return;
        }
        let area = content_area(&self.surface, self.bar(), self.frame());
        let at = self.pixels_at();
        let ctx = self.ctx();
        // Содержимое рисуется поверх прежнего, и заливка обязательна: список,
        // ставший короче, иначе оставил бы под собой хвост предыдущего.
        self.surface.fill(area, theme::window_bg());
        match &self.content {
            Content::Settings(view) => view.draw(&mut self.surface, area, ctx),
            Content::Dialog(view) => view.draw(&mut self.surface, area, ctx),
            Content::Text(_) => return,
            // Пиксели программы ядро не рисует, а переносит: что в них
            // написано — её дело. Заливка выше не лишняя и здесь: поверхность
            // программы бывает меньше области содержимого, и без заливки по
            // краям осталось бы то, что лежало там прежде.
            Content::Program(view) => {
                // Пиксели больше области (окно шире экрана в рамке стола)
                // срезаются по ней: заголовок и обводку они не задевают.
                let whole = view.pixels.bounds();
                let placed = Rect::new(at.0, at.1, whole.w, whole.h).intersect(&area);
                if !placed.is_empty() {
                    let src = Rect::new(placed.x - at.0, placed.y - at.1, placed.w, placed.h);
                    self.surface.blit_from(&view.pixels, (placed.x, placed.y), src);
                    make_opaque(&mut self.surface, placed);
                }
            }
        }
        self.damage = self.damage.union(&area);
        // Заливка стёрла обводку по бокам и снизу (the classic frame lies
        // outside the filled area, 23.11).
        if !Self::classic() {
            let focused = self.edge.focused;
            self.paint_edge(focused);
        }
    }

    /// Показать в «Параметрах» раздел экрана.
    ///
    /// Нужно меню стола: пункт «Display settings» обязан открывать окно уже на
    /// нужном разделе, иначе он всего лишь синоним значка «Settings».
    pub fn show_settings(&mut self, section: super::settings::Section) {
        if let Content::Settings(view) = &mut self.content {
            view.show(section);
            self.redraw_content();
        }
    }

    /// Перерисовать «Параметры», если они открыты, — сменилось то, что они
    /// показывают снаружи (состояние Wi-Fi, 03.11).
    pub fn refresh_settings(&mut self) {
        if matches!(self.content, Content::Settings(_)) {
            self.redraw_content();
        }
    }

    /// Такт стола для «Параметров»: отсчёт «Сохранить эти параметры
    /// дисплея?». Перерисовывает окно, если на странице что-то сменилось.
    pub fn settings_tick(&mut self) {
        if let Content::Settings(view) = &mut self.content {
            if view.tick() {
                self.redraw_content();
            }
        }
    }

    /// Тянут ли сейчас монитор в «Параметрах → Дисплей».
    #[must_use]
    pub fn settings_dragging(&self) -> bool {
        matches!(&self.content, Content::Settings(view) if view.dragging())
    }

    /// Указатель с зажатой кнопкой в точке экрана `x`, `y`: тянуть монитор
    /// за ним. `true` — монитор сдвинулся и окно перерисовано.
    pub fn settings_drag(&mut self, x: i32, y: i32) -> bool {
        let local = (x - self.rect.x, y - self.rect.y);
        let moved = match &mut self.content {
            Content::Settings(view) => view.drag_to(local.0, local.1),
            _ => false,
        };
        if moved {
            self.redraw_content();
        }
        moved
    }

    /// Кнопку отпустили — монитор встал на место.
    pub fn settings_drop(&mut self) {
        if let Content::Settings(view) = &mut self.content {
            view.drop_drag();
            self.redraw_content();
        }
    }

    /// Отдать щелчок содержимому окна. Координаты — в точках экрана.
    ///
    /// `true` — содержимое им воспользовалось и окно перерисовано. Текстовые
    /// окна щелчков не разбирают: в них нечего выбирать мышью.
    pub fn handle_click(&mut self, x: i32, y: i32) -> bool {
        let area = content_area(&self.surface, self.bar(), self.frame());
        let ctx = self.ctx();
        let local = (x - self.rect.x, y - self.rect.y);
        let used = match &mut self.content {
            Content::Settings(view) => view.click(area, ctx, local.0, local.1),
            Content::Dialog(view) => view.click(area, ctx, local.0, local.1),
            Content::Text(_) => false,
            // Щелчок по окну программы — её событие, а не работа ядра: кладёт
            // его в очередь оконный менеджер, которому известны кнопки мыши.
            Content::Program(_) => false,
        };
        if used {
            self.redraw_content();
        }
        used
    }

    /// Сменило ли содержимое тему с прошлого вопроса.
    ///
    /// Признак снимается чтением: спросить обязан ровно один — тот, кто умеет
    /// перекрасить весь стол, — и второй утвердительный ответ означал бы вторую
    /// перерисовку экрана на ровном месте.
    pub fn took_theme_change(&mut self) -> bool {
        match &mut self.content {
            Content::Settings(view) => view.take_theme_change(),
            Content::Text(_) | Content::Dialog(_) | Content::Program(_) => false,
        }
    }

    /// Сменилась ли высота заголовка — тогда стол пересобирает все окна.
    pub fn took_title_change(&mut self) -> bool {
        match &mut self.content {
            Content::Settings(view) => view.take_title_change(),
            Content::Text(_) | Content::Dialog(_) | Content::Program(_) => false,
        }
    }

    /// Колесо мыши над окном ядра. `true` — содержимое сдвинулось и
    /// перерисовано.
    pub fn wheel(&mut self, clicks: i32) -> bool {
        let moved = match &mut self.content {
            Content::Settings(view) => view.wheel(clicks),
            // Терминал: три строки на щелчок колеса, как у Windows Terminal.
            Content::Text(_) => return self.term_scroll(clicks * 3),
            Content::Dialog(_) | Content::Program(_) => false,
        };
        if moved {
            self.redraw_content();
        }
        moved
    }

    /// Ждёт ли окно текста (поле ввода в «Параметрах») — для экранной
    /// клавиатуры телефона.
    #[must_use]
    pub fn wants_text(&self) -> bool {
        matches!(&self.content, Content::Settings(view) if view.wants_text())
    }

    /// Прокрутка пальцем: содержимое за пальцем на `dy` точек (телефон).
    /// `true` — сдвинулось и перерисовано.
    pub fn drag_scroll(&mut self, dy: i32) -> bool {
        let moved = match &mut self.content {
            Content::Settings(view) => view.drag_scroll(dy),
            _ => false,
        };
        if moved {
            self.redraw_content();
        }
        moved
    }

    /// Поднять вид терминала на `rows` строк прокрутки (отрицательное —
    /// опустить). `true`, если вид сдвинулся.
    pub fn term_scroll(&mut self, rows: i32) -> bool {
        let Content::Text(grid) = &mut self.content else {
            return false;
        };
        let moved = grid.scroll_view(&mut self.surface, rows);
        self.damage = self.damage.union(&grid.take_damage());
        moved
    }

    /// Вернуть вид терминала к живому экрану. `true`, если он был поднят.
    pub fn term_scroll_to_bottom(&mut self) -> bool {
        let Content::Text(grid) = &mut self.content else {
            return false;
        };
        let moved = grid.scroll_to_bottom(&mut self.surface);
        self.damage = self.damage.union(&grid.take_damage());
        moved
    }

    /// Вид терминала: на сколько строк поднят и сколько строк в прокрутке.
    #[must_use]
    pub fn term_view(&self) -> (u32, usize) {
        match &self.content {
            Content::Text(grid) => (grid.view_offset(), grid.scrollback_len()),
            _ => (0, 0),
        }
    }

    /// Нажатие левой кнопки в текстовом окне — начало выделения; прежнее
    /// выделение снимается. Точка — экранная.
    pub fn term_select_start(&mut self, x: i32, y: i32) -> bool {
        let Content::Text(grid) = &mut self.content else {
            return false;
        };
        let cell = grid.point_to_cell(x - self.rect.x, y - self.rect.y);
        grid.clear_selection(&mut self.surface);
        self.select_from = Some(cell);
        self.damage = self.damage.union(&grid.take_damage());
        true
    }

    /// Мышь с нажатой кнопкой ушла в точку `x, y`: выделение тянется за ней.
    pub fn term_select_to(&mut self, x: i32, y: i32) -> bool {
        let (Content::Text(grid), Some(anchor)) = (&mut self.content, self.select_from) else {
            return false;
        };
        let head = grid.point_to_cell(x - self.rect.x, y - self.rect.y);
        grid.select(&mut self.surface, anchor, head);
        self.damage = self.damage.union(&grid.take_damage());
        true
    }

    /// Кнопку отпустили: выделение закончено. Возвращает выделенный текст.
    pub fn term_select_end(&mut self) -> Option<String> {
        self.select_from.take()?;
        let Content::Text(grid) = &self.content else {
            return None;
        };
        Some(grid.selected_text()).filter(|text| !text.is_empty())
    }

    /// Выделенный в терминале текст (пустой — выделения нет).
    #[must_use]
    pub fn term_selection(&self) -> String {
        match &self.content {
            Content::Text(grid) => grid.selected_text(),
            _ => String::new(),
        }
    }

    /// Снять выделение в терминале. `true`, если оно было.
    pub fn term_clear_selection(&mut self) -> bool {
        let Content::Text(grid) = &mut self.content else {
            return false;
        };
        let had = grid.clear_selection(&mut self.surface);
        self.damage = self.damage.union(&grid.take_damage());
        had
    }

    /// Чего «Параметры» просят у стола (фаза 69b).
    pub fn took_request(&mut self) -> Option<super::settings::Request> {
        match &mut self.content {
            Content::Settings(view) => view.take_request(),
            Content::Text(_) | Content::Dialog(_) | Content::Program(_) => None,
        }
    }

    /// Попросили ли «Параметры» другой режим экрана (фаза С6a).
    pub fn took_mode_request(&mut self) -> Option<(u32, u32)> {
        match &mut self.content {
            Content::Settings(view) => view.take_mode_request(),
            Content::Text(_) | Content::Dialog(_) | Content::Program(_) => None,
        }
    }

    /// Сказать окну, чем кончилась смена режима, и перерисовать его.
    pub fn mode_applied(&mut self, requested: (u32, u32), outcome: Result<(), alloc::string::String>) {
        if let Content::Settings(view) = &mut self.content {
            view.mode_applied(requested, outcome);
            self.redraw_content();
        }
    }

    /// Забрать ответ диалога, если его дали.
    ///
    /// Ответ забирает стол, а не окно действует само: «Выключить» закрывает
    /// окно и поднимает просьбу к системе, и то и другое за пределами окна.
    pub fn take_answer(&mut self) -> Option<Answer> {
        match &mut self.content {
            Content::Dialog(view) => view.take_answer(),
            Content::Text(_) | Content::Settings(_) | Content::Program(_) => None,
        }
    }

    /// Что выбрано в «Как открыть этот файл?» (см. [`DialogView::open_with_choice`]).
    #[must_use]
    pub fn open_with_choice(&self) -> Option<(String, String, String, bool)> {
        match &self.content {
            Content::Dialog(view) => view.open_with_choice(),
            _ => None,
        }
    }

    /// Сдвинуть окно, оставив заголовок на экране.
    ///
    /// Полностью уехавшее окно нельзя ни вернуть, ни закрыть мышью, которой на
    /// этой фазе нет, — поэтому часть заголовка обязана остаться видимой.
    ///
    /// `areas` — рабочие области мониторов (у основного — до панели). Окно
    /// прижимается к той из них, где прижатое место ближе всего к
    /// запрошенному: так оно свободно переезжает с монитора на монитор, а
    /// упирается только во внешние края. Монитор один — ровно прежнее правило
    /// «по ширине экрана и до панели».
    pub fn move_within(&mut self, dx: i32, dy: i32, areas: &[Rect]) {
        let keep = Self::title_height(self.scale) as i32 * 3;
        let want_x = self.rect.x.saturating_add(dx);
        let want_y = self.rect.y.saturating_add(dy);
        let mut best: Option<(i64, i32, i32)> = None;
        for area in areas {
            let min_x = area.x + keep - self.rect.w as i32;
            let max_x = area.right() - keep;
            let max_y = (area.bottom() - keep).max(area.y);
            let x = want_x.clamp(min_x, max_x.max(min_x));
            let y = want_y.clamp(area.y, max_y);
            let d = i64::from(x - want_x).pow(2) + i64::from(y - want_y).pow(2);
            if best.is_none_or(|(bd, ..)| d < bd) {
                best = Some((d, x, y));
            }
        }
        if let Some((_, x, y)) = best {
            self.rect.x = x;
            self.rect.y = y;
        }
    }

    /// Declare the size limits of the content (24.11, WM SDK). An empty set
    /// brings back the built-in ones. `true` — they changed.
    pub fn set_limits(&mut self, limits: SizeLimits) -> bool {
        let limits = if limits.is_empty() { SizeLimits { min: self.app.min_content(), max: (0, 0), logical: true } } else { limits };
        if limits == self.limits {
            return false;
        }
        self.limits = limits;
        // «Развернуть» greys out or comes back.
        self.draw_decorations(false);
        true
    }

    /// The declared limits.
    #[must_use]
    pub const fn limits(&self) -> SizeLimits {
        self.limits
    }

    /// Whole-window size bounds now: `(min_w, min_h, max_w, max_h)` with the
    /// title band and the frame. The phone, a desktop frame (`fit`), a popup
    /// and a fullscreen window know only the smallest window [`MIN_W`].
    #[must_use]
    pub fn outer_bounds(&self) -> (u32, u32, u32, u32) {
        if theme::is_mobile() || self.fit || self.popup || self.fullscreen {
            return (MIN_W, MIN_H, u32::MAX, u32::MAX);
        }
        let l = self.limits;
        let px = |v: u32| if l.logical { v * self.scale } else { v };
        let (cw, ch) = self.chrome_at(self.scale);
        let min_w = if l.min.0 > 0 { px(l.min.0) + cw } else { 0 }.max(MIN_W);
        let min_h = if l.min.1 > 0 { px(l.min.1) + ch } else { 0 }.max(MIN_H);
        let max_w = if l.max.0 > 0 { (px(l.max.0) + cw).max(min_w) } else { u32::MAX };
        let max_h = if l.max.1 > 0 { (px(l.max.1) + ch).max(min_h) } else { u32::MAX };
        (min_w, min_h, max_w, max_h)
    }

    /// `w`×`h` brought inside [`Self::outer_bounds`].
    #[must_use]
    pub fn clamp_size(&self, w: u32, h: u32) -> (u32, u32) {
        let (min_w, min_h, max_w, max_h) = self.outer_bounds();
        (w.clamp(min_w, max_w), h.clamp(min_h, max_h))
    }

    /// The window has one size ([`SizeLimits::fixed`]).
    #[must_use]
    pub fn fixed_size(&self) -> bool {
        self.limits.fixed() && !theme::is_mobile()
    }

    /// Can «Развернуть» (the button, a double click on the title, snapping)
    /// change this window: it is resizable, not a dialog and not of one
    /// fixed size. Windows greys the button out for such windows.
    #[must_use]
    pub fn can_maximize(&self) -> bool {
        theme::is_mobile() || (self.resizable() && !self.dialog && !self.fixed_size())
    }

    /// Задать окну новый размер, сохранив содержимое.
    ///
    /// Поверхность создаётся заново: она хранит пиксели, и растянуть её нечем.
    /// Отказ выделения — это `false` и **прежнее** окно, а не половина нового:
    /// окно без поверхности нечем рисовать, и потерять на изменении размера
    /// работающий терминал было бы хуже, чем не изменить размер.
    pub fn resize(&mut self, w: u32, h: u32) -> bool {
        let (w, h) = self.clamp_size(w, h);
        if w == self.rect.w && h == self.rect.h {
            return true;
        }
        self.rebuild(w, h)
    }

    /// Собрать поверхность заново под такой размер — общая часть изменения
    /// размера и смены высоты заголовка.
    fn rebuild(&mut self, w: u32, h: u32) -> bool {
        let Some(mut surface) = Surface::new(w, h, theme::window_bg()) else {
            return false;
        };
        let (top, side) = (self.bar(), self.frame());
        let inner = grid_area(&surface, self.scale, !self.tab_order.is_empty(), top, side);
        match &mut self.content {
            Content::Text(grid) => {
                surface.fill(content_area(&surface, top, side), mini_ui::text::TERM_BG);
                if !grid.rebind(&mut surface, inner) {
                    return false;
                }
                for tab in &mut self.tabs {
                    let _ = tab.grid.rebind(&mut surface, inner);
                }
            }
            // Список файлов и «Параметры» рисуют себя от размера области, и
            // переносить в них нечего: содержимое соберётся заново.
            Content::Settings(_) | Content::Dialog(_) => {}
            // Окно программы меняет размер, только если она сама сказала, что
            // умеет (фаза 62b). Пиксели при этом не трогаются: они лежат в
            // кадрах, отображённых программе, и новый размер означал бы
            // поверхность, из-под которой у работающей программы выдернули
            // память. Меняется рамка, а программе уходит просьба
            // ([`user_abi::WIN_RESIZE`]); прежние пиксели до ответа лежат в
            // новой рамке как есть. Отвечает она [`user_abi::SYS_WINRESIZE`] —
            // сама, стоя внутри вызова, то есть память меняется у неё на глазах.
            Content::Program(view) if view.resizable => {}
            // Рамку стола (телефон, 05.11) менять можно и не умеющей программе:
            // её пиксели остаются прежними и встают в новую рамку по центру.
            Content::Program(_) if self.fit => {}
            Content::Program(_) => return false,
        }

        self.surface = surface;
        self.rect.w = w;
        self.rect.h = h;
        self.redraw_content();
        self.draw_decorations(false);
        self.damage = Rect::new(0, 0, w, h);
        if self.is_program() && self.resizable() {
            let (cw, ch) = self.content_of(w, h);
            self.ask_resize(cw, ch);
        }
        true
    }

    /// Развёрнуто ли окно на всю рабочую область.
    #[must_use]
    pub const fn is_maximized(&self) -> bool {
        self.restore.is_some()
    }

    /// Развернуть окно на всю рабочую область — или вернуть прежний размер.
    ///
    /// Возвращает `true`, если состояние изменилось. Прежний прямоугольник
    /// запоминается целиком, вместе с местом: развёрнутое окно, вернувшееся не
    /// туда, откуда его развернули, выглядит как потерянное.
    ///
    /// `area` — куда разворачивать: на ПК вся рабочая область от верха
    /// экрана, на телефоне — от строки состояния до полоски жеста.
    pub fn toggle_maximize(&mut self, area: Rect) -> bool {
        match self.restore.take() {
            Some(rect) => {
                let (x, y) = (rect.x, rect.y);
                if !self.resize(rect.w, rect.h) {
                    self.restore = Some(rect);
                    return false;
                }
                self.rect.x = x;
                self.rect.y = y;
                true
            }
            None => {
                let previous = self.rect;
                // «Развёрнуто» — до пересборки поверхности: она рисует
                // украшения уже развёрнутого окна (углы без скругления).
                self.restore = Some(previous);
                if !self.resize(area.w, area.h) {
                    self.restore = None;
                    return false;
                }
                self.rect.x = area.x;
                self.rect.y = area.y;
                true
            }
        }
    }

    /// Вернуть развёрнутое окно к прежнему размеру, но поставить его углом в
    /// `x`, `y` (20.11): развёрнутое окно потащили за заголовок, и оно
    /// уменьшается под указателем, как в Windows.
    ///
    /// Так же возвращается окно, прилепленное к половине монитора (20.11): к
    /// размеру, который был до прилипания, — даже если его потом ещё и
    /// развернули.
    pub fn unmaximize_at(&mut self, x: i32, y: i32, focused: bool) -> bool {
        let Some(rect) = self.drag_restore_rect() else {
            return false;
        };
        let (restore, snap) = (self.restore.take(), self.snap_from.take());
        if !self.resize(rect.w, rect.h) {
            self.restore = restore;
            self.snap_from = snap;
            return false;
        }
        self.rect.x = x;
        self.rect.y = y;
        self.draw_decorations(focused);
        true
    }

    /// Каким окно станет, если его потащить за заголовок: прилепленное — тем,
    /// что было до прилипания, развёрнутое — прежним. `None` — окно обычное.
    #[must_use]
    pub const fn drag_restore_rect(&self) -> Option<Rect> {
        match self.snap_from {
            Some(rect) => Some(rect),
            None => self.restore,
        }
    }

    /// Прилеплено ли окно к половине монитора.
    #[must_use]
    pub const fn is_snapped(&self) -> bool {
        self.snap_from.is_some()
    }

    /// Окно бросили у края экрана (20.11, как Snap в Windows): занять
    /// `area` — половину рабочей области монитора, или развернуться на неё
    /// (`maximize`, край сверху). `previous` — прямоугольник, к которому
    /// окно вернётся, когда его потащат или «восстановят»: окно, уже
    /// прилепленное, помнит самый первый. `false` — размер окну не меняется
    /// (калькулятор).
    pub fn snap_to(&mut self, area: Rect, previous: Rect, maximize: bool) -> bool {
        if area.is_empty() || self.fullscreen {
            return false;
        }
        let (snap, restore) = (self.snap_from.take(), self.restore.take());
        let previous = snap.unwrap_or(previous);
        if maximize {
            // «Развёрнуто» — до пересборки: углы без скругления.
            self.restore = Some(previous);
        }
        if !self.resize(area.w, area.h) {
            self.restore = restore;
            self.snap_from = snap;
            return false;
        }
        if !maximize {
            self.snap_from = Some(previous);
        }
        self.rect.x = area.x;
        self.rect.y = area.y;
        true
    }

    /// Забыть, что окно развёрнуто, оставив его размер и место (его сдвинули
    /// клавишами): кнопка снова предлагает «развернуть».
    pub fn forget_maximized(&mut self, focused: bool) {
        let chrome = self.chrome_at(self.scale);
        if let Some(rect) = self.restore.take() {
            // Classic (23.11): the frame comes back - the window is rebuilt
            // around a smaller content (the program is asked for it).
            if self.chrome_at(self.scale) != chrome {
                let (w, h) = (self.rect.w, self.rect.h);
                if !self.rebuild(w, h) {
                    self.restore = Some(rect);
                    return;
                }
            }
            self.draw_decorations(focused);
        }
    }

    /// Попросить программу ответить на текущий размер окна — даже если он не
    /// изменился: сервер Wayland отвечает на такую просьбу `configure` с
    /// состояниями окна (развёрнуто, во весь экран), и клиент узнаёт, что его
    /// просьба исполнена.
    pub fn ask_configure(&mut self) {
        if self.is_program() && self.resizable() {
            let (w, h) = self.content_of(self.rect.w, self.rect.h);
            self.ask_resize(w, h);
        }
    }

    /// Открыть окно во весь монитор `area` (20.11). `true` — состояние
    /// изменилось.
    pub fn enter_fullscreen(&mut self, area: Rect) -> bool {
        if self.fullscreen || area.is_empty() {
            return false;
        }
        let previous = self.rect;
        // Флаг — до изменения размера: просьба программе уходит из него же,
        // и сервер Wayland читает состояние, отвечая на неё.
        self.fullscreen = true;
        if !self.resize(area.w, area.h) {
            self.fullscreen = false;
            return false;
        }
        self.rect.x = area.x;
        self.rect.y = area.y;
        self.full_from = Some(previous);
        self.full_size = self.content_of(area.w, area.h);
        self.ask_configure();
        true
    }

    /// Запомнить, куда вернуться из полноэкранного режима (D4, 20.11.2026):
    /// игра, сама ставшая размером с монитор, вошла в него уже огромной — и
    /// «вернуть» давало те же 2560x1440 в стороне (hw-log12, 4523). Место
    /// до роста — правильнее.
    pub fn set_full_from(&mut self, rect: Rect) {
        if self.fullscreen && !rect.is_empty() {
            self.full_from = Some(rect);
        }
    }

    /// Выйти из полноэкранного режима туда, где окно стояло до него.
    pub fn leave_fullscreen(&mut self) -> bool {
        if !self.fullscreen {
            return false;
        }
        self.fullscreen = false;
        let back = self.full_from.take().unwrap_or(self.rect);
        if !self.resize(back.w, back.h) {
            self.fullscreen = true;
            self.full_from = Some(back);
            return false;
        }
        self.rect.x = back.x;
        self.rect.y = back.y;
        self.ask_configure();
        true
    }

    /// Поверхность окна — для сборки кадра.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Как окно называется для человека.
    ///
    /// У окон ядра это [`App::caption`], у окна программы — имя, которое
    /// назвала она сама.
    ///
    /// Программа может прислать имя из двух частей через `\x1f` (20.11):
    /// «Files\x1fЗагрузки». Первая - имя окна для журнала и прицела стенда,
    /// вторая - что видит человек ([`Self::label`]): папку, файл.
    #[must_use]
    pub fn caption(&self) -> &str {
        self.title.split('\x1f').next().unwrap_or(&self.title)
    }

    /// Подпись окна для человека: заголовок, кнопка панели, миниатюра
    /// (20.11). У окна с вкладками - имя видимой вкладки, у программы - вторая
    /// часть её имени (см. [`Self::caption`]), если она есть.
    #[must_use]
    pub fn label(&self) -> String {
        if !self.tab_order.is_empty() {
            return self.tab_title(self.session);
        }
        match self.title.split_once('\x1f') {
            Some((_, shown)) if !shown.is_empty() => String::from(shown),
            _ => self.title.clone(),
        }
    }

    /// Сменить имя окна программы (фаза 62c, `user_abi::SYS_WINTITLE`).
    ///
    /// Перерисовывается только полоса заголовка ([`Self::draw_decorations`]
    /// сама отмечает её к выводу): пиксели программы под ней не менялись. Ответ
    /// — сменилось ли имя; то же имя второй раз не стоит кадра.
    pub fn set_title(&mut self, title: &str, focused: bool) -> bool {
        if !self.is_program() || self.title == title {
            return false;
        }
        self.title.clear();
        self.title.push_str(title);
        self.draw_decorations(focused);
        true
    }

    /// Сменить подпись любого окна (терминал T4: «Терминал — выполняется
    /// …»). В отличие от [`Self::set_title`], и окну ядра.
    pub fn set_caption(&mut self, title: &str, focused: bool) -> bool {
        if self.title == title {
            return false;
        }
        self.title.clear();
        self.title.push_str(title);
        self.draw_decorations(focused);
        true
    }

    /// Окно ли это пользовательской программы.
    ///
    /// Спрашивает оконный менеджер, и спрашивает не из любопытства: закрытие
    /// такого окна — **просьба** к программе, а не действие над окном.
    #[must_use]
    pub fn is_program(&self) -> bool {
        matches!(self.content, Content::Program(_))
    }

    /// Перенести на поверхность окна тот кусок, который программа объявила
    /// изменившимся.
    ///
    /// Координаты — внутри поверхности программы; `None` означает «всё окно».
    /// Прямоугольник, вылезающий за её край, обрезается, а не отвергается:
    /// программа считает в своих координатах и о полосе заголовка не знает
    /// ничего.
    pub fn commit(&mut self, area: Option<Rect>) {
        let Content::Program(view) = &self.content else {
            return;
        };
        let content = content_area(&self.surface, self.bar(), self.frame());
        let origin = self.pixels_at();
        let whole = view.pixels.bounds();
        let src = area.unwrap_or(whole).intersect(&whole);
        if src.is_empty() {
            return;
        }
        // Срезается по области содержимого: в рамке стола пиксели бывают
        // больше окна (05.11), и копия иначе легла бы на обводку.
        let copied = Rect::new(origin.0.saturating_add(src.x), origin.1.saturating_add(src.y), src.w, src.h)
            .intersect(&content);
        if copied.is_empty() {
            return;
        }
        let src = Rect::new(copied.x - origin.0, copied.y - origin.1, copied.w, copied.h);
        let resizable = view.resizable;
        self.surface.blit_from(&view.pixels, (copied.x, copied.y), src);
        make_opaque(&mut self.surface, copied);
        // Уголок размера лежит поверх содержимого, и копия пикселей его
        // стирает. Рисуется заново только тогда, когда копия его задела.
        if resizable && !copied.intersect(&self.resize_grip()).is_empty() {
            self.draw_grip();
        }
        self.damage = self.damage.union(&copied);
        // Копия легла и на обводку (20.11: раньше её края так и пропадали —
        // до следующего наведения на кнопки заголовка).
        if self.touches_edge(copied) {
            let focused = self.edge.focused;
            self.paint_edge(focused);
        }
    }

    /// Поверхность программы — чтобы ядро нарисовало в ней само (сервер
    /// Wayland, фаза 71i: у его окон программы-владельца поверхности нет).
    pub fn program_pixels(&mut self) -> Option<&mut Surface> {
        match &mut self.content {
            Content::Program(view) => Some(&mut view.pixels),
            _ => None,
        }
    }

    /// G3 (21.11): the program pixels when they can go to the screen as they
    /// are - an opaque frameless program window whose pixels are exactly its
    /// surface (no frame, title, `fit` offset or alpha). `None` otherwise.
    #[must_use]
    pub fn direct_pixels(&self) -> Option<&Surface> {
        if self.popup || !self.frameless || self.fit || self.translucent || self.shaped || self.minimized {
            return None;
        }
        match &self.content {
            Content::Program(view)
                if (view.pixels.width(), view.pixels.height()) == (self.surface.width(), self.surface.height())
                    && (self.rect.w, self.rect.h) == (self.surface.width(), self.surface.height())
                    && self.pixels_at() == (0, 0) =>
            {
                Some(&view.pixels)
            }
            _ => None,
        }
    }

    /// G3: physical address and pitch of the pixels shown directly, when they
    /// are a contiguous client buffer (see [`Self::direct_phys`]); `Err` -
    /// why not (logged by the desktop when a flip cannot engage).
    pub fn scanout_source(&self) -> Result<(u64, u32), &'static str> {
        let pixels = self.direct_pixels().ok_or("the window is not shown straight from its pixels")?;
        let (at, phys) = self.direct_phys.ok_or("the window pixels are not one physically contiguous client buffer (shm or scattered pages)")?;
        if pixels.row(0).as_ptr() as usize != at {
            return Err("the window shows other pixels than its client buffer");
        }
        Ok((phys, pixels.width() * 4))
    }

    /// G3: the program changed `area` of its pixels while the window is shown
    /// directly - remember the damage (screen gets it from the pixels), do not
    /// copy into the surface.
    pub fn defer_commit(&mut self, area: Option<Rect>) {
        let Content::Program(view) = &self.content else {
            return;
        };
        let whole = view.pixels.bounds();
        let src = area.unwrap_or(whole).intersect(&whole);
        if src.is_empty() {
            return;
        }
        self.stale = true;
        self.damage = self.damage.union(&src);
    }

    /// G3: the window leaves the direct path - bring its surface up to date.
    pub fn refresh_stale(&mut self) {
        if core::mem::take(&mut self.stale) {
            self.commit(None);
        }
    }

    /// Лежит ли в очереди непрочитанная просьба закрыться.
    #[must_use]
    pub fn close_pending(&self) -> bool {
        match &self.content {
            Content::Program(view) => view.events.iter().any(|event| event.kind == user_abi::WIN_CLOSE),
            _ => false,
        }
    }

    /// Положить событие в очередь окна. `false` — очередь полна.
    pub fn push_event(&mut self, event: WinEvent) -> bool {
        let Content::Program(view) = &mut self.content else {
            return false;
        };
        if view.events.len() >= MAX_EVENTS {
            // Просьба закрыться вытесняет самое старое событие, а не теряется
            // сама. Без этой строки обещание договора («переполнение теряет
            // новые события, а `WIN_CLOSE` к этому моменту уже лежит в
            // очереди») держалось бы только на порядке, в котором события
            // приходят, — то есть не держалось бы вовсе: тридцать два щелчка
            // подряд по невнимательной программе, и окно больше нечем закрыть.
            if event.kind != user_abi::WIN_CLOSE {
                return false;
            }
            view.events.pop_front();
        }
        // Отказ аллокатора — это потерянное событие, а не остановка системы:
        // очередь окна не то место, ради которого стоит гасить машину.
        if view.events.try_reserve(1).is_err() {
            return false;
        }
        view.events.push_back(event);
        true
    }

    /// Положить в очередь движение указателя, склеив его с предыдущим.
    ///
    /// Движений у мыши поток, а очередь конечна: без склейки сорок движений
    /// заполнили бы её, и щелчок, пришедший следом, потерялся бы — договор
    /// теряет **новые** события. Программе нужно, где указатель сейчас, поэтому
    /// движение, лежащее последним, просто заменяется.
    pub fn push_move(&mut self, event: WinEvent) -> bool {
        if let Content::Program(view) = &mut self.content {
            if let Some(last) = view.events.back_mut() {
                if last.kind == user_abi::WIN_MOVE {
                    *last = event;
                    return true;
                }
            }
            // G3: a relative motion after the last move - replace the move
            // before it (relative and absolute motion do not depend on order).
            let len = view.events.len();
            if len >= 2 && view.events[len - 1].kind == super::WIN_RELMOTION && view.events[len - 2].kind == user_abi::WIN_MOVE {
                view.events[len - 2] = event;
                return true;
            }
        }
        self.push_event(event)
    }

    /// G3: raw mouse motion for `zwp_relative_pointer_v1` - added to the last
    /// queued relative motion, so a burst of reports costs one queue slot and
    /// loses nothing (unlike [`Self::push_move`], deltas must add up).
    pub fn push_relative(&mut self, dx: i32, dy: i32) -> bool {
        if let Content::Program(view) = &mut self.content {
            // The last relative motion among the last two events (a pointer
            // move may sit between them: their order does not matter).
            let len = view.events.len();
            for at in (len.saturating_sub(2)..len).rev() {
                let kind = view.events[at].kind;
                if kind == super::WIN_RELMOTION {
                    let last = &mut view.events[at];
                    last.x = last.x.saturating_add(dx);
                    last.y = last.y.saturating_add(dy);
                    return true;
                }
                if kind != user_abi::WIN_MOVE {
                    break;
                }
            }
        }
        self.push_event(WinEvent { kind: super::WIN_RELMOTION, code: 0, x: dx, y: dy })
    }

    /// Забрать самое старое событие. `None` — очередь пуста.
    pub fn pop_event(&mut self) -> Option<WinEvent> {
        let Content::Program(view) = &mut self.content else {
            return None;
        };
        view.events.pop_front()
    }
}

/// Скопировать заголовок в собственную строку.
///
/// `None` — не хватило памяти. `to_string` здесь не годится ровно поэтому: он
/// уводит отказ аллокатора в `handle_alloc_error`, то есть останавливает
/// систему из-за заголовка окна.
fn own(text: &str) -> Option<String> {
    let mut owned = String::new();
    owned.try_reserve_exact(text.len()).ok()?;
    owned.push_str(text);
    Some(owned)
}

/// Область поверхности под содержимое: всё, что ниже полосы заголовка.
///
/// Поля не отнимаются: их назначает само содержимое, и они у списка файлов и у
/// «Параметров» разные. Отнимать их здесь значило бы, что боковая колонка,
/// которая обязана доходить до края окна, до него не доходит.
/// Где лежит сетка терминала: область содержимого без полосы вкладок и с
/// полем в четыре точки.
fn grid_area(surface: &Surface, scale: mini_ui::Scale, strip: bool, bar: u32, side: u32) -> Rect {
    let ctx = Ctx::scaled(scale);
    // С 72h вкладки ПК — в заголовке (макет 15), своя полоса только у
    // телефона (и в классике).
    let top = bar + if strip && Window::separate_strip() { ctx.px(STRIP_H) } else { 0 };
    let mut area = content_area(surface, top, side).shrink(ctx.px(4));
    // Справа — поле под уголок размера (20.11). Сетка прокручивается
    // сдвигом пикселей (`Surface::scroll_up`), и точки уголка, лежавшие в
    // её прямоугольнике, уезжали вверх с каждой строкой вывода — вдоль
    // правого края терминала оставался столбик серых точек.
    if !theme::is_mobile() {
        area.w = area.w.saturating_sub(ctx.px(GRIP_GUTTER));
    }
    area
}

/// Ширина поля справа от сетки терминала, где лежит уголок размера: точки
/// уголка — не дальше 13 точек от края окна, сетка кончается в 4.
const GRIP_GUTTER: u32 = 10;

/// Сделать точки прямоугольника непрозрачными (старший байт — 0xFF).
///
/// У телефона кадровый буфер — слой наложения, и старший байт точки там —
/// непрозрачность: ноль значит «прозрачно», то есть чёрный экран. Программы на
/// C пишут XRGB, где этот байт — «всё равно» и обычно ноль: NetSurf на
/// телефоне стоял чёрным, хотя страница была в буфере (05.11). Окно
/// программы непрозрачно всегда.
fn make_opaque(surface: &mut Surface, rect: Rect) {
    let rect = rect.intersect(&surface.bounds());
    if rect.is_empty() {
        return;
    }
    let (x0, x1) = (rect.x as usize, rect.x as usize + rect.w as usize);
    for y in rect.y as u32..rect.y as u32 + rect.h {
        for pixel in &mut surface.row_mut(y)[x0..x1] {
            *pixel |= 0xFF00_0000;
        }
    }
}

/// The content below the title band `top` and inside the side frame `side`
/// (the classic Windows 98 frame, 23.11; 0 otherwise).
fn content_area(surface: &Surface, top: u32, side: u32) -> Rect {
    Rect::new(
        side as i32,
        top as i32,
        surface.width().saturating_sub(2 * side),
        surface.height().saturating_sub(top + side),
    )
}
