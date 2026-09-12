//! Меню по правому щелчку: то, что можно сделать со столом и с тем, что на нём
//! лежит.
//!
//! # Почему это отдельный слой, а не меню запуска в другом месте
//!
//! Меню запуска знает свой список программ и своё место у панели; это меню
//! появляется там, где щёлкнули, и его пункты — действия, а не программы.
//! Общая структура на двоих означала бы одно перечисление, половина вариантов
//! которого не имеет смысла для второй половины случаев.
//!
//! # Почему пунктов то четыре, то четыре других
//!
//! Меню, открытое на пустом месте стола, предлагает создать; меню, открытое на
//! значке, — открыть, переименовать и удалить. Показывать всё сразу и гасить
//! половину пунктов было бы честнее ровно до первого вопроса «а почему
//! „удалить“ серое»: ответ на него — «вы ни во что не целились», и его дешевле
//! не задавать.
//!
//! # Почему имя набирается прямо в меню
//!
//! Диалогового окна с полем ввода в системе нет, и заводить его ради одной
//! строки — это оконный класс, фокус ввода и модальность, то есть половина
//! оконного менеджера заново. Меню уже забирает себе весь ввод, пока открыто,
//! поэтому строка набирается в нём же: одна дополнительная строка на экране
//! против отдельного вида окна.
//!
//! # Почему тени и углов здесь нет
//!
//! Меню — плавающая стеклянная карточка, но тень под ней и срез её углов
//! рисует композитор: он один знает, что лежит под слоем, а обои у него
//! радиальные, с цветными пятнами и сеткой точек. Повторить их внутри
//! поверхности нельзя даже приблизительно — прямоугольник другого оттенка
//! выдал бы себя сразу. Поэтому поверхность здесь ровно размером с карточку, а
//! заливка сводится к непрозрачному цвету поверх усреднённых обоев: всё, кроме
//! углов, композитор копирует как есть.
//!
//! # Почему раскладка считается одной функцией
//!
//! Потому что нарисованное и нажимаемое обязаны совпадать: [`plan`] отвечает,
//! где лежит какая строка, и её спрашивают и отрисовка, и поиск пункта под
//! указателем. Пока это были две формулы, они сходились ровно до первой правки
//! отступа.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::typeface::{Face, Role};
use mini_ui::{Rect, Surface};

use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::theme;
use crate::input::{KeyCode, KeyEvent, Modifiers};
use crate::vfs::perm::Access;

/// Поле внутри карточки.
const CARD_PAD: u32 = 8;
/// Высота строки меню.
const ROW_H: u32 = 32;
/// Зазор между строками.
const ROW_GAP: u32 = 1;
/// Поле внутри строки.
const ROW_PAD: u32 = 10;
/// Сторона значка в строке.
const ICON: u32 = 14;
/// Просвет между значком и подписью.
const ICON_GAP: u32 = 10;
/// Поле разделителя сверху и снизу.
const SEP_PAD: u32 = 5;
/// Высота поля ввода имени.
const FIELD_H: u32 = 30;

/// Подсказка под полем ввода имени — она же задаёт наименьшую ширину меню.
const RENAME_HINT: &str = "Enter — переименовать    Esc — отмена    Ctrl+U — очистить";
/// Подсказка под вопросом об удалении.
const CONFIRM_HINT: &str = "Y — удалить    N — оставить";

/// Что предлагает меню.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Открыть выбранный значок — то же, что двойной щелчок по нему.
    Open,
    /// Переименовать выбранное.
    Rename,
    /// Удалить выбранное.
    Delete,
    /// Создать каталог в каталоге стола.
    NewFolder,
    /// Создать пустой текстовый файл там же.
    NewTextFile,
    /// Переключить тему на противоположную.
    Theme,
    /// Открыть «Параметры» на разделе экрана.
    DisplaySettings,
    /// Перечитать стол и перерисовать его целиком.
    Refresh,
}

impl Action {
    /// Все пункты, какие бывают, — по ним считается размер поверхности.
    const ALL: [Action; 8] = [
        Action::Open,
        Action::Rename,
        Action::Delete,
        Action::NewFolder,
        Action::NewTextFile,
        Action::Theme,
        Action::DisplaySettings,
        Action::Refresh,
    ];

    /// Пункты меню, открытого на пустом месте стола.
    /// Пункты меню, открытого на пустом месте стола.
    ///
    /// Тема стоит здесь, а не только в «Параметрах», по той же причине, по
    /// которой обои меняют щелчком по столу: это свойство самого стола, и идти
    /// за ним в окно настроек человек не должен.
    pub const ON_DESKTOP: [Action; 5] = [
        Action::NewFolder,
        Action::NewTextFile,
        Action::Theme,
        Action::DisplaySettings,
        Action::Refresh,
    ];

    /// Пункты меню, открытого на файле или каталоге стола.
    pub const ON_ENTRY: [Action; 4] = [
        Action::Open,
        Action::Rename,
        Action::Delete,
        Action::Refresh,
    ];

    /// Пункты меню, открытого на системном значке: переименовать «Settings»
    /// нечем, а открыть его — можно.
    pub const ON_APP: [Action; 2] = [Action::Open, Action::Refresh];

    fn title(self) -> &'static str {
        match self {
            Action::Open => "Открыть",
            Action::Rename => "Переименовать",
            Action::Delete => "Удалить",
            Action::NewFolder => "Создать папку",
            Action::NewTextFile => "Создать текстовый файл",
            // Пункт называется тем, что произойдёт, а не тем, что есть сейчас:
            // «Тёмная тема» при включённой тёмной читается как признак, а не
            // как действие, и человек нажимает её, чтобы «включить», получая
            // обратное.
            Action::Theme => {
                if theme::is_dark() {
                    "Светлая тема"
                } else {
                    "Тёмная тема"
                }
            }
            Action::DisplaySettings => "Настройки экрана",
            Action::Refresh => "Обновить",
        }
    }

    /// К какой группе относится пункт — между группами идёт черта.
    ///
    /// Группа, а не список разделителей: пункты собираются по три разных
    /// набора, и разделитель, заданный номером строки, съезжал бы в каждом из
    /// них по-своему.
    const fn group(self) -> u8 {
        match self {
            Action::Open | Action::Rename | Action::Delete => 0,
            Action::NewFolder | Action::NewTextFile => 1,
            Action::Theme | Action::DisplaySettings | Action::Refresh => 2,
        }
    }

    /// Значок пункта. `None` — подходящего в наборе нет.
    ///
    /// «Переименовать» остаётся без значка намеренно: карандаша в наборе нет, а
    /// подобрать «что-нибудь похожее» значит поставить в строку картинку,
    /// которая говорит не то. Отступ подписи от этого не меняется — иначе одна
    /// строка выехала бы левее остальных.
    fn icon(self) -> Option<Icon> {
        match self {
            Action::Open => Some(Icon::ChevronRight),
            Action::Rename => None,
            Action::Delete => Some(Icon::Close),
            Action::NewFolder => Some(Icon::Folder),
            Action::NewTextFile => Some(Icon::File),
            Action::Theme => Some(if theme::is_dark() { Icon::Sun } else { Icon::Moon }),
            Action::DisplaySettings => Some(Icon::Display),
            Action::Refresh => Some(Icon::Update),
        }
    }
}

/// Чем меню занято прямо сейчас.
enum Mode {
    /// Обычный список пунктов.
    Menu,
    /// Набирается новое имя.
    Rename { from: String, text: String },
    /// Ждём подтверждения удаления.
    Confirm { name: String },
}

/// Что меню просит сделать в ответ на клавишу.
///
/// Отдельный тип, а не `Option<Action>`: переименование приносит с собой
/// набранную строку, и втискивать её в перечисление пунктов значило бы иметь
/// пункт, у которого есть данные ровно в одном случае из семи.
pub enum Reply {
    /// Клавиша меню не понадобилась — пусть её разбирает кто-нибудь ещё.
    Ignored,
    /// Меню разобралось само, перерисовалось; делать больше нечего.
    Handled,
    /// Закрыть меню.
    Close,
    /// Выполнить пункт.
    Run(Action),
    /// Переименовать выбранное в это имя.
    Rename(String),
    /// Удаление подтверждено.
    Delete,
}

/// Меню стола: где оно, что в нём выбрано и чем оно занято.
pub struct ContextMenu {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    /// Пункты этого открытия — они зависят от того, куда щёлкнули.
    items: Vec<Action>,
    selected: usize,
    open: bool,
    mode: Mode,
    damage: Rect,
    /// Верх панели задач: ниже него карточке выезжать нельзя.
    ///
    /// Запоминается при открытии, потому что подгонять высоту приходится и
    /// позже — когда меню сменило список пунктов на поле ввода имени.
    work_bottom: i32,
    /// Ответ последнего действия — показывается строкой внизу меню.
    note: Option<String>,
}

impl ContextMenu {
    #[must_use]
    pub fn new(scale: u32) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        // Ширина считается по самому длинному пункту и по самой длинной
        // подсказке: меню, которое меняет ширину вместе с содержимым, прыгало
        // бы под рукой.
        let mut widest = 0;
        for action in Action::ALL {
            widest = widest.max(ctx.face(Role::Body).width(action.title()));
        }
        let content = ctx.px(ICON + ICON_GAP) + widest;
        let hint = ctx
            .face(Role::Caption)
            .width(RENAME_HINT)
            .max(ctx.face(Role::Caption).width(CONFIRM_HINT));
        let width = content.max(hint) + ctx.px((CARD_PAD + ROW_PAD) * 2);

        // Поверхность — на самое длинное меню, какое бывает; показывается из
        // неё столько строк, сколько пунктов у этого открытия. Растить и
        // сжимать поверхность на каждый щелчок значило бы просить память в
        // обработчике события ввода — и остаться без меню, когда её не дали.
        let height = height_for(ctx, &Action::ON_ENTRY).max(height_for(ctx, &Action::ON_DESKTOP));
        let surface = Surface::new(width, height, theme::wall_average(theme::palette()))?;
        Some(Self {
            surface,
            rect: Rect::new(0, 0, width, height),
            scale,
            items: Action::ON_DESKTOP.to_vec(),
            selected: 0,
            open: false,
            mode: Mode::Menu,
            damage: Rect::EMPTY,
            work_bottom: i32::MAX,
            note: None,
        })
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Занято ли меню вводом — тогда щелчок внутри него не пункт, а промах.
    #[must_use]
    pub const fn is_editing(&self) -> bool {
        !matches!(self.mode, Mode::Menu)
    }

    /// Контекст отрисовки. Подложка — обои: карточка лежит на них.
    fn ctx(&self) -> Ctx {
        Ctx::scaled(self.scale).on(theme::wall_average(theme::palette()))
    }

    /// Открыть меню в точке экрана, не выпуская его за края.
    pub fn open_at(
        &mut self,
        x: i32,
        y: i32,
        screen: (u32, u32),
        work_bottom: i32,
        items: &[Action],
    ) {
        self.items.clear();
        self.items.extend_from_slice(items);
        self.work_bottom = work_bottom;
        let ctx = self.ctx();
        self.rect.h = height_for(ctx, &self.items).min(self.surface.height());
        let max_x = (screen.0 as i32 - self.rect.w as i32).max(0);
        let max_y = (work_bottom - self.rect.h as i32).max(0);
        self.rect.x = x.clamp(0, max_x);
        self.rect.y = y.clamp(0, max_y);
        self.selected = 0;
        self.mode = Mode::Menu;
        self.note = None;
        self.open = true;
        self.redraw();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.mode = Mode::Menu;
    }

    /// Пункт под точкой экрана.
    #[must_use]
    pub fn action_at(&self, x: i32, y: i32) -> Option<Action> {
        if self.is_editing() || !self.rect.contains(x, y) {
            return None;
        }
        let local = (x - self.rect.x, y - self.rect.y);
        let layout = plan(self.ctx(), self.card(), &self.items);
        layout
            .rows
            .iter()
            .find(|slot| slot.rect.contains(local.0, local.1))
            .map(|slot| slot.action)
    }

    /// Карточка — вся поверхность целиком.
    ///
    /// Отдельный метод, а не `Rect::new(0, 0, w, h)` по месту: раскладку
    /// спрашивают из трёх мест, и «где начинается карточка» обязано быть одним
    /// ответом.
    fn card(&self) -> Rect {
        Rect::new(0, 0, self.rect.w, self.rect.h)
    }

    /// Начать набор нового имени.
    pub fn start_rename(&mut self, name: &str) {
        self.mode = Mode::Rename { from: name.to_string(), text: name.to_string() };
        self.note = None;
        self.fit(rename_height(self.ctx()));
        self.redraw();
    }

    /// Спросить, точно ли удалять.
    ///
    /// Спрашивается всегда, а не только у каталога: отменить удаление нечем —
    /// корзины в системе нет, — и «нажал не туда» здесь означает потерянный
    /// файл.
    pub fn start_confirm(&mut self, name: &str) {
        self.mode = Mode::Confirm { name: name.to_string() };
        self.note = None;
        self.fit(confirm_height(self.ctx()));
        self.redraw();
    }

    /// Подогнать карточку под новую высоту, не дав ей выехать вниз.
    ///
    /// Меню открылось у нижнего края и стало ниже — оно обязано остаться там,
    /// где стояло; стало выше — обязано подняться, а не уехать под панель.
    fn fit(&mut self, height: u32) {
        let before = self.rect.h;
        self.rect.h = height.min(self.surface.height());
        if self.rect.h > before {
            let limit = self.work_bottom - self.rect.h as i32;
            self.rect.y = self.rect.y.min(limit.max(0));
        }
    }

    /// Разобрать клавишу, пока меню открыто.
    pub fn handle_key(&mut self, event: KeyEvent) -> Reply {
        if !event.pressed {
            return Reply::Ignored;
        }
        match &mut self.mode {
            Mode::Menu => self.menu_key(event.code),
            Mode::Confirm { .. } => match event.code {
                KeyCode::Y | KeyCode::Enter => Reply::Delete,
                KeyCode::N | KeyCode::Escape => {
                    self.mode = Mode::Menu;
                    self.redraw();
                    Reply::Handled
                }
                _ => Reply::Handled,
            },
            Mode::Rename { from, text } => match event.code {
                KeyCode::Escape => {
                    self.mode = Mode::Menu;
                    self.redraw();
                    Reply::Handled
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.redraw();
                    Reply::Handled
                }
                // Строка приходит заполненной прежним именем — так правят
                // букву, не набирая всё заново. Заменить имя целиком стоило бы
                // тогда десяти нажатий Backspace, поэтому здесь то же
                // сочетание, что стирает строку в любой оболочке.
                KeyCode::U if event.mods.contains(Modifiers::CTRL) => {
                    text.clear();
                    self.redraw();
                    Reply::Handled
                }
                KeyCode::Enter => {
                    let name = text.trim().to_string();
                    // Имя, не изменившееся или пустое, — это отказ от
                    // переименования, а не переименование в ничто. Молча
                    // выполнить его значило бы получить `rename(a, a)` и в
                    // лучшем случае ничего, в худшем — потерянную запись.
                    if name.is_empty() || name == *from {
                        self.mode = Mode::Menu;
                        self.redraw();
                        return Reply::Handled;
                    }
                    Reply::Rename(name)
                }
                _ => {
                    // Косая черта в имени — это путь, а не имя: переименование
                    // с ней увело бы файл в другой каталог, чего человек,
                    // набирающий имя под значком, не просил.
                    match event.to_char() {
                        Some(ch) if ch != '/' && ch != '\n' && !ch.is_control() => {
                            if text.chars().count() < NAME_LIMIT {
                                text.push(ch);
                                self.redraw();
                            }
                            Reply::Handled
                        }
                        _ => Reply::Handled,
                    }
                }
            },
        }
    }

    fn menu_key(&mut self, code: KeyCode) -> Reply {
        match code {
            KeyCode::Up => {
                let count = self.items.len().max(1);
                self.selected = (self.selected + count - 1) % count;
                self.redraw();
                Reply::Handled
            }
            KeyCode::Down => {
                let count = self.items.len().max(1);
                self.selected = (self.selected + 1) % count;
                self.redraw();
                Reply::Handled
            }
            KeyCode::Enter => match self.items.get(self.selected).copied() {
                Some(action) => Reply::Run(action),
                None => Reply::Close,
            },
            KeyCode::Escape => Reply::Close,
            _ => Reply::Ignored,
        }
    }

    /// Поставить выделение на пункт под указателем.
    pub fn select_action(&mut self, action: Action) {
        if let Some(index) = self.items.iter().position(|item| *item == action) {
            if index != self.selected {
                self.selected = index;
                self.redraw();
            }
        }
    }

    /// Перерисовать меню под текущую тему.
    ///
    /// У меню своя поверхность, и цвета в неё уже вписаны: смена темы не
    /// перекрашивает нарисованное, её надо нарисовать заново. Закрытое меню
    /// изменившимся не помечается — его на экране нет, и просить композитор
    /// перерисовать место, где его нет, значит тратить кадр впустую.
    pub fn restyle(&mut self) {
        self.redraw();
        if !self.open {
            self.damage = Rect::EMPTY;
        }
    }

    /// Показать ответ действия, не закрывая меню.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
        self.mode = Mode::Menu;
        self.redraw();
    }

    fn redraw(&mut self) {
        let ctx = self.ctx();
        let p = ctx.palette;
        let bounds = Rect::new(0, 0, self.surface.width(), self.rect.h);
        let card = self.card();

        // Внутренности карточки лежат на стекле, а не на обоях: сведи их поверх
        // обоев — и строка под указателем окажется на полтона мимо той
        // подложки, на которой она нарисована.
        let glass = ctx.flat(p.glass);
        let inner = ctx.on(glass);
        let radius = ctx.px(theme::R_CARD);
        let layout = plan(ctx, card, &self.items);

        {
            let s = &mut self.surface;
            // Заливка идёт по всей поверхности, а не по скруглённой фигуре:
            // углы срезает композитор, а точки, оставшиеся от прошлой
            // отрисовки, просвечивали бы сквозь его сглаживание.
            s.fill(bounds, glass);
            draw::rounded_stroke(s, card, radius, p.line3.color, p.line3.alpha);
            draw::crown(s, card, radius, p.crown.color, p.crown.alpha);
        }

        match &self.mode {
            Mode::Menu => {
                let selected = self.selected;
                let s = &mut self.surface;
                for y in &layout.lines {
                    paint::separator(
                        inner,
                        s,
                        card.x + ctx.px(CARD_PAD + ROW_PAD) as i32,
                        *y,
                        card.w.saturating_sub(ctx.px((CARD_PAD + ROW_PAD) * 2)),
                    );
                }
                for (index, slot) in layout.rows.iter().enumerate() {
                    draw_row(inner, s, slot, index == selected);
                }
            }
            Mode::Rename { text, .. } => {
                let s = &mut self.surface;
                draw_rename(inner, s, card, text);
            }
            Mode::Confirm { name } => {
                let s = &mut self.surface;
                draw_confirm(inner, s, card, name);
            }
        }

        if let Some(note) = &self.note {
            let s = &mut self.surface;
            paint::text_clipped(
                inner,
                s,
                Role::Caption,
                layout.note.x,
                layout.note.y,
                layout.note.w,
                note,
                p.ink4,
            );
        }

        self.damage = bounds;
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }
}

/// Сколько знаков помещается в имя.
///
/// Предел ext2 — 255 байт, но набранное имя рисуется в меню, и строка, которую
/// негде показать, — это ввод вслепую.
const NAME_LIMIT: usize = 64;

/// Одна строка меню и место, которое она занимает.
struct Slot {
    action: Action,
    rect: Rect,
}

/// Где что лежит в карточке.
struct Plan {
    rows: Vec<Slot>,
    /// Высоты, на которых идут черты между группами.
    lines: Vec<i32>,
    /// Место под ответ действия внизу карточки.
    note: Rect,
}

/// Разложить пункты по карточке.
///
/// Одна функция на отрисовку и на поиск пункта под указателем — см. заголовок
/// модуля.
fn plan(ctx: Ctx, card: Rect, items: &[Action]) -> Plan {
    let pad = ctx.px(CARD_PAD);
    let row_h = ctx.px(ROW_H);
    let gap = ctx.px(ROW_GAP);
    let sep = ctx.px(SEP_PAD * 2) + 1;
    let mut y = card.y + pad as i32;
    let mut rows = Vec::new();
    let mut lines = Vec::new();
    let mut previous: Option<Action> = None;

    for action in items {
        if previous.is_some_and(|before| before.group() != action.group()) {
            lines.push(y + ctx.px(SEP_PAD) as i32);
            y += sep as i32;
        }
        rows.push(Slot {
            action: *action,
            rect: Rect::new(
                card.x + pad as i32,
                y,
                card.w.saturating_sub(pad * 2),
                row_h,
            ),
        });
        y += (row_h + gap) as i32;
        previous = Some(*action);
    }

    let note_pad = ctx.px(CARD_PAD + ROW_PAD);
    let note = Rect::new(
        card.x + note_pad as i32,
        y + ctx.px(4) as i32,
        card.w.saturating_sub(note_pad * 2),
        u32::from(ctx.face(Role::Caption).line),
    );
    Plan { rows, lines, note }
}

/// Высота поверхности, нужная меню с такими пунктами.
///
/// Считается по той же раскладке, что и рисуется: разойдись формулы — и
/// последний пункт оказался бы за нижним краем поверхности, то есть невидимым и
/// недостижимым мышью.
fn height_for(ctx: Ctx, items: &[Action]) -> u32 {
    let card = Rect::new(0, 0, 0, 0);
    let layout = plan(ctx, card, items);
    let bottom = layout.note.bottom() + ctx.px(CARD_PAD) as i32;
    bottom.max(0) as u32
}

/// Высота карточки, занятой набором имени.
///
/// Считается отдельно, а не берётся от списка пунктов: поверхность заведена
/// под самый длинный список раз и навсегда, и если карточку рисовать во всю её
/// высоту, под подсказкой остаётся пустая треть. Пустота внизу читается как
/// «здесь что-то не нарисовалось».
fn rename_height(ctx: Ctx) -> u32 {
    let caps = u32::from(ctx.face(Role::MonoCaps).line);
    let hint = u32::from(ctx.face(Role::Caption).line);
    // Слагаемые те же и в том же порядке, что в [`draw_rename`], — иначе
    // подпись однажды уедет под нижний край, и заметит это только глаз.
    ctx.px(CARD_PAD + 6) + caps + ctx.px(10) + ctx.px(FIELD_H) + ctx.px(10) + hint
        + ctx.px(CARD_PAD + 6)
}

/// Высота карточки с вопросом об удалении.
fn confirm_height(ctx: Ctx) -> u32 {
    let chip = ctx.px(22);
    let body = u32::from(ctx.face(Role::Body).line);
    let hint = u32::from(ctx.face(Role::Caption).line);
    ctx.px(CARD_PAD + 4) + chip + ctx.px(10) + body + ctx.px(10) + hint + ctx.px(CARD_PAD + 4)
}

/// Нарисовать строку меню.
fn draw_row(ctx: Ctx, s: &mut Surface, slot: &Slot, selected: bool) {
    let p = ctx.palette;
    // Удаление — единственный пункт, после которого нечего вернуть, и красным
    // оно названо ровно поэтому.
    let danger = slot.action == Action::Delete;
    let r = ctx.px(theme::R_CHIP);
    if selected {
        if danger {
            draw::rounded(s, slot.rect, r, ctx.flat(p.badbg), 255);
            draw::rounded_stroke(s, slot.rect, r, p.badline, 255);
        } else {
            paint::row(ctx, s, slot.rect, RowState::Selected);
        }
    }
    let ink = if danger {
        p.bad_ink
    } else if selected {
        paint::row_ink(ctx, RowState::Selected)
    } else {
        p.ink2
    };

    let pad = ctx.px(ROW_PAD);
    let icon = ctx.px(ICON);
    if let Some(glyph) = slot.action.icon() {
        glyphicon::draw(
            s,
            glyph,
            slot.rect.x + pad as i32,
            slot.rect.y + (slot.rect.h as i32 - icon as i32) / 2,
            icon,
            ink,
            255,
        );
    }
    // Отступ подписи один и тот же со значком и без него: строка без картинки,
    // подтянутая к краю, ломает колонку подписей.
    let x = slot.rect.x + (pad + icon + ctx.px(ICON_GAP)) as i32;
    paint::text_clipped(
        ctx,
        s,
        Role::Body,
        x,
        paint::baseline(ctx, Role::Body, slot.rect),
        (slot.rect.right() - pad as i32 - x).max(0) as u32,
        slot.action.title(),
        ink,
    );
}

/// Поле ввода нового имени.
fn draw_rename(ctx: Ctx, s: &mut Surface, card: Rect, text: &str) {
    let p = ctx.palette;
    let pad = ctx.px(CARD_PAD + ROW_PAD);
    let left = card.x + pad as i32;
    let width = card.w.saturating_sub(pad * 2);
    let mut y = card.y + ctx.px(CARD_PAD + 6) as i32;

    paint::caps(ctx, s, left, y, "НОВОЕ ИМЯ");
    y += (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(10)) as i32;

    let field = Rect::new(left, y, width, ctx.px(FIELD_H));
    let r = ctx.px(theme::R_CHIP);
    paint::sunk(ctx, s, field, r);
    // Кольцо снаружи и обводка по краю: поле, в которое сейчас набирают, обязано
    // отличаться от поля, которое просто нарисовано, — иначе непонятно, куда
    // уходят нажатия.
    draw::rounded_stroke(s, field, r, p.acc, 255);
    let ring = Rect::new(
        field.x - ctx.px(2) as i32,
        field.y - ctx.px(2) as i32,
        field.w + ctx.px(4),
        field.h + ctx.px(4),
    );
    draw::rounded_stroke(s, ring, r + ctx.px(2), p.acctint.color, p.acctint.alpha);

    // Показывается **хвост** строки: набирают в конце, и уехавший за край
    // курсор выглядел бы как переставший отвечать ввод.
    let inner = ctx.px(10);
    let room = field.w.saturating_sub(inner * 2 + ctx.px(4));
    let shown = tail(ctx.face(Role::Mono), text, room);
    let baseline = paint::baseline(ctx, Role::Mono, field);
    let used = paint::text(ctx, s, Role::Mono, field.x + inner as i32, baseline, &shown, p.ink2);
    let caret = Rect::new(
        field.x + (inner + used) as i32 + ctx.px(1) as i32,
        field.y + (field.h as i32 - ctx.px(15) as i32) / 2,
        ctx.px(1),
        ctx.px(15),
    );
    s.fill(caret, p.acc);

    y += (field.h + ctx.px(10)) as i32;
    paint::text_clipped(ctx, s, Role::Caption, left, y, width, RENAME_HINT, p.ink5);
}

/// Вопрос об удалении.
fn draw_confirm(ctx: Ctx, s: &mut Surface, card: Rect, name: &str) {
    let p = ctx.palette;
    let pad = ctx.px(CARD_PAD + ROW_PAD);
    let left = card.x + pad as i32;
    let width = card.w.saturating_sub(pad * 2);
    let mut y = card.y + ctx.px(CARD_PAD + 4) as i32;

    let chip = Rect::new(left, y, paint::chip_width(ctx, "УДАЛИТЬ НАВСЕГДА"), ctx.px(22));
    paint::chip(ctx, s, chip, "УДАЛИТЬ НАВСЕГДА", Tone::Bad);
    y += (chip.h + ctx.px(10)) as i32;

    paint::text_clipped(ctx, s, Role::Body, left, y, width, name, p.ink2);
    y += (u32::from(ctx.face(Role::Body).line) + ctx.px(10)) as i32;

    paint::text_clipped(ctx, s, Role::Caption, left, y, width, CONFIRM_HINT, p.bad_ink);
}

/// Оставить хвост строки — то, что набирают прямо сейчас.
fn tail(face: &Face, text: &str, room: u32) -> String {
    if face.width(text) <= room {
        return text.to_string();
    }
    let mark = face.width("…");
    let room = room.saturating_sub(mark);
    let count = text.chars().count();
    for skip in 1..=count {
        let candidate: String = text.chars().skip(skip).collect();
        if face.width(&candidate) <= room {
            let mut out = String::from("…");
            out.push_str(&candidate);
            return out;
        }
    }
    String::from("…")
}

/// Каталог стола — там появляется всё, что создаётся его меню.
///
/// `~/Desktop` у того, кто вошёл, а если имени нет — `/root/Desktop`. Каталог
/// создаётся при первой надобности: требовать, чтобы установщик завёл его
/// заранее, значит получить систему, где меню не работает на всех машинах,
/// поставленных раньше.
pub fn desktop_dir() -> String {
    alloc::format!("{}/Desktop", home_dir())
}

/// Домашний каталог того, кто вошёл, — `/root`, если имени нет.
#[must_use]
pub fn home_dir() -> String {
    crate::user::session::with_name(|name| {
        if name.is_empty() || name == "root" {
            "/root".to_string()
        } else {
            alloc::format!("/home/{name}")
        }
    })
}

/// Создать в каталоге стола каталог или пустой файл с незанятым именем.
///
/// Возвращает имя того, что получилось, либо объяснение отказа. Имя
/// подбирается с номером — «New folder», «New folder 2» и так далее: молча
/// писать поверх уже существующего нельзя, а переименовать созданное человек
/// теперь может прямо на столе.
pub fn create_entry(directory: bool) -> Result<String, String> {
    let base = desktop_dir();
    ensure_dir(&base)?;

    let stem = if directory { "New folder" } else { "New file.txt" };
    for attempt in 1..=32u32 {
        let name = if attempt == 1 {
            stem.to_string()
        } else if directory {
            alloc::format!("New folder {attempt}")
        } else {
            alloc::format!("New file {attempt}.txt")
        };
        let path = alloc::format!("{base}/{name}");
        if exists(&path) {
            continue;
        }
        let done = if directory {
            crate::fs::mkdir_as(crate::user::session::credentials(), &path, 0o755)
        } else {
            crate::fs::create_as(crate::user::session::credentials(), &path, 0o644)
                .map(|r| r.map(|_| ()))
        };
        return match done {
            Some(Ok(())) => Ok(name),
            Some(Err(err)) => Err(alloc::format!("{err}")),
            None => Err("no filesystem is mounted".to_string()),
        };
    }
    Err("too many entries with that name".to_string())
}

/// Переименовать запись каталога стола. Возвращает её **новый путь**.
///
/// Новое имя — именно имя, а не путь: каталог берётся у прежнего пути, и увести
/// файл в чужой каталог набором «../» здесь нельзя. Проверка занятости идёт до
/// переименования: `rename` в ext2 перезаписал бы чужую запись молча, а на
/// рабочем столе это выглядело бы как исчезнувший файл.
///
/// Путь возвращается целиком, а не одно имя, потому что зовущему он нужен: по
/// нему стол снова находит значок после перечитывания каталога. Собирать его
/// заново на стороне вызывающего значило бы иметь две склейки пути, которые
/// однажды разойдутся.
pub fn rename_entry(path: &str, new_name: &str) -> Result<String, String> {
    let name = new_name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err("that is not a name".to_string());
    }
    let parent = match path.rfind('/') {
        Some(0) | None => String::new(),
        Some(index) => path[..index].to_string(),
    };
    let target = alloc::format!("{parent}/{name}");
    if target == path {
        return Ok(target);
    }
    if exists(&target) {
        return Err(alloc::format!("{name} already exists"));
    }
    match crate::fs::rename_as(crate::user::session::credentials(), path, &target) {
        Some(Ok(())) => Ok(target),
        Some(Err(err)) => Err(alloc::format!("{err}")),
        None => Err("no filesystem is mounted".to_string()),
    }
}

/// Имя в конце пути.
#[must_use]
pub fn base_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(index) => &path[index + 1..],
        None => path,
    }
}

/// Удалить запись каталога стола.
///
/// Непустой каталог не удаляется, и это не ограничение реализации: рекурсивное
/// удаление по одному нажатию — самая дорогая ошибка, какую может совершить
/// рабочий стол. Отказ приходит от файловой системы и пересказывается как есть.
pub fn delete_entry(path: &str) -> Result<(), String> {
    match crate::fs::remove_as(crate::user::session::credentials(), path) {
        Some(Ok(())) => Ok(()),
        Some(Err(err)) => Err(alloc::format!("{err}")),
        None => Err("no filesystem is mounted".to_string()),
    }
}

/// Есть ли уже что-нибудь по этому пути.
fn exists(path: &str) -> bool {
    crate::fs::resolve_as(crate::user::session::credentials(), path, Access::NONE)
        .is_some_and(|result| result.is_ok())
}

/// Каталог стола существует — создать вместе с родителями, если его нет.
///
/// Родителей приходится создавать своими руками: у живого носителя корень —
/// образ initrd, в котором нет ни `/root`, ни `/home`, и создание одного лишь
/// последнего звена кончалось отказом «нет такого файла» — верным по сути и
/// бесполезным для человека, который просто нажал «создать папку».
fn ensure_dir(path: &str) -> Result<(), String> {
    let mut built = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        built.push('/');
        built.push_str(part);
        if exists(&built) {
            continue;
        }
        match crate::fs::mkdir_as(crate::user::session::credentials(), &built, 0o755) {
            Some(Ok(())) => {}
            // Живой носитель — это образ initrd в FAT, где каталогов не
            // заводят вовсе. Сказать об этом словами человека дешевле, чем
            // оставить ему «operation not supported by this filesystem»:
            // ошибка верная, а делать с ней нечего, пока система не поставлена
            // на диск.
            Some(Err(crate::vfs::VfsError::Unsupported)) => {
                return Err("the live system cannot store files; install it first".to_string());
            }
            Some(Err(err)) => return Err(alloc::format!("{built}: {err}")),
            None => return Err("no filesystem is mounted".to_string()),
        }
    }
    Ok(())
}
