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

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::text::{self, GLYPH_H, GLYPH_W};
use mini_ui::{Rect, Surface};

use super::theme;
use crate::input::{KeyCode, KeyEvent, Modifiers};
use crate::vfs::perm::Access;

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
    /// Открыть «Параметры» на разделе экрана.
    DisplaySettings,
    /// Перечитать стол и перерисовать его целиком.
    Refresh,
}

impl Action {
    /// Все пункты, какие бывают, — по ним считается размер поверхности.
    const ALL: [Action; 7] = [
        Action::Open,
        Action::Rename,
        Action::Delete,
        Action::NewFolder,
        Action::NewTextFile,
        Action::DisplaySettings,
        Action::Refresh,
    ];

    /// Пункты меню, открытого на пустом месте стола.
    pub const ON_DESKTOP: [Action; 4] = [
        Action::NewFolder,
        Action::NewTextFile,
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

    const fn title(self) -> &'static str {
        match self {
            Action::Open => "Open",
            Action::Rename => "Rename",
            Action::Delete => "Delete",
            Action::NewFolder => "New folder",
            Action::NewTextFile => "New text file",
            Action::DisplaySettings => "Display settings",
            Action::Refresh => "Refresh",
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
    /// Ответ последнего действия — показывается строкой внизу меню.
    note: Option<String>,
}

impl ContextMenu {
    #[must_use]
    pub fn new(scale: u32) -> Option<Self> {
        let row_h = row_height(scale);
        let mut widest = 0;
        for action in Action::ALL {
            widest = widest.max(text::width_of(action.title(), scale));
        }
        // Место под сообщение внизу: оно бывает длиннее пунктов, и меню, которое
        // меняет ширину вместе с ответом, прыгало бы под рукой.
        let width = (widest + GLYPH_W * scale * 8).max(GLYPH_W * scale * 28);
        // Поверхность — на самое длинное меню, какое бывает; показывается из
        // неё столько строк, сколько пунктов у этого открытия. Растить и
        // сжимать поверхность на каждый щелчок значило бы просить память в
        // обработчике события ввода — и остаться без меню, когда её не дали.
        let height = row_h * (Action::ON_DESKTOP.len() as u32 + 2) + theme::PADDING * 2;
        let surface = Surface::new(width, height, theme::WINDOW_BG)?;
        Some(Self {
            surface,
            rect: Rect::new(0, 0, width, height),
            scale,
            items: Action::ON_DESKTOP.to_vec(),
            selected: 0,
            open: false,
            mode: Mode::Menu,
            damage: Rect::EMPTY,
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

    /// Высота меню с таким числом пунктов.
    fn height_for(&self, items: usize) -> u32 {
        row_height(self.scale) * (items as u32 + 1) + theme::PADDING * 2
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
        self.rect.h = self.height_for(self.items.len()).min(self.surface.height());
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
        let row_h = row_height(self.scale);
        let local = (y - self.rect.y - theme::PADDING as i32).max(0) as u32;
        let index = (local / row_h) as usize;
        self.items.get(index).copied()
    }

    /// Начать набор нового имени.
    pub fn start_rename(&mut self, name: &str) {
        self.mode = Mode::Rename { from: name.to_string(), text: name.to_string() };
        self.note = None;
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
        self.redraw();
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

    /// Показать ответ действия, не закрывая меню.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
        self.mode = Mode::Menu;
        self.redraw();
    }

    fn redraw(&mut self) {
        let scale = self.scale;
        let row_h = row_height(scale);
        let bounds = Rect::new(0, 0, self.surface.width(), self.rect.h);
        self.surface.fill(bounds, theme::WINDOW_BG);
        self.surface.frame(bounds, theme::BORDER, theme::ACCENT);

        match &self.mode {
            Mode::Menu => {
                for index in 0..self.items.len() {
                    let action = self.items[index];
                    let y = theme::PADDING + index as u32 * row_h;
                    if index == self.selected {
                        self.surface.fill(
                            Rect::new(
                                theme::BORDER as i32,
                                y as i32,
                                bounds.w.saturating_sub(theme::BORDER * 2),
                                row_h,
                            ),
                            theme::SELECT_BG,
                        );
                    }
                    // Удаление — единственный пункт, после которого нечего
                    // вернуть, и красным оно названо ровно поэтому.
                    let color = if action == Action::Delete {
                        theme::CLOSE
                    } else {
                        theme::TEXT
                    };
                    text::draw_text(
                        &mut self.surface,
                        GLYPH_W * scale,
                        y + scale,
                        action.title(),
                        scale,
                        color,
                        None,
                    );
                }
            }
            Mode::Rename { text, .. } => {
                let room = self.room();
                text::draw_text(
                    &mut self.surface,
                    GLYPH_W * scale,
                    theme::PADDING + scale,
                    "New name:",
                    scale,
                    theme::DIM,
                    None,
                );
                // Показывается **хвост** строки: набирают в конце, и уехавший
                // за край курсор выглядел бы как переставший отвечать ввод.
                let shown = tail(text, room.saturating_sub(1));
                let line = alloc::format!("{shown}_");
                text::draw_text(
                    &mut self.surface,
                    GLYPH_W * scale,
                    theme::PADDING + row_h + scale,
                    &line,
                    scale,
                    theme::TEXT,
                    None,
                );
                // Подсказка — у нижнего края, а не сразу под строкой ввода:
                // высота меню задана числом пунктов, и подсказка посередине
                // оставляла бы под собой пустую половину коробки.
                text::draw_text(
                    &mut self.surface,
                    GLYPH_W * scale,
                    self.rect.h.saturating_sub(row_h) + scale,
                    "Enter rename   Esc cancel   Ctrl+U clear",
                    scale.saturating_sub(1).max(1),
                    theme::DIM,
                    None,
                );
            }
            Mode::Confirm { name } => {
                let room = self.room();
                text::draw_text(
                    &mut self.surface,
                    GLYPH_W * scale,
                    theme::PADDING + scale,
                    "Delete for good?",
                    scale,
                    theme::CLOSE,
                    None,
                );
                text::draw_text(
                    &mut self.surface,
                    GLYPH_W * scale,
                    theme::PADDING + row_h + scale,
                    &clip(name, room),
                    scale,
                    theme::TEXT,
                    None,
                );
                text::draw_text(
                    &mut self.surface,
                    GLYPH_W * scale,
                    self.rect.h.saturating_sub(row_h) + scale,
                    "Y delete   N keep it",
                    scale.saturating_sub(1).max(1),
                    theme::DIM,
                    None,
                );
            }
        }

        if let Some(note) = &self.note {
            let y = self.rect.h.saturating_sub(row_h);
            let room = self.room();
            let text = clip(note, room);
            text::draw_text(
                &mut self.surface,
                GLYPH_W * scale,
                y + scale,
                &text,
                scale.saturating_sub(1).max(1),
                theme::DIRECTORY,
                None,
            );
        }

        self.damage = bounds;
    }

    /// Сколько знаков помещается в строку меню.
    fn room(&self) -> usize {
        let cell = GLYPH_W * self.scale;
        ((self.surface.width().saturating_sub(cell * 2)) / cell) as usize
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

fn row_height(scale: u32) -> u32 {
    GLYPH_H * scale + theme::PADDING * 2
}

/// Обрезать строку по числу знаков, пометив обрезку.
fn clip(text: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    if text.chars().count() <= room {
        return text.to_string();
    }
    text.chars().take(room.saturating_sub(1)).collect::<String>() + "~"
}

/// Оставить хвост строки — то, что набирают прямо сейчас.
fn tail(text: &str, room: usize) -> String {
    let count = text.chars().count();
    if count <= room || room == 0 {
        return text.to_string();
    }
    let skip = count - room.saturating_sub(1);
    alloc::format!("~{}", text.chars().skip(skip).collect::<String>())
}

/// Каталог стола — там появляется всё, что создаётся его меню.
///
/// `~/Desktop` у того, кто вошёл, а если имени нет — `/root/Desktop`. Каталог
/// создаётся при первой надобности: требовать, чтобы установщик завёл его
/// заранее, значит получить систему, где меню не работает на всех машинах,
/// поставленных раньше.
pub fn desktop_dir() -> String {
    let home = crate::user::session::with_name(|name| {
        if name.is_empty() || name == "root" {
            "/root".to_string()
        } else {
            alloc::format!("/home/{name}")
        }
    });
    alloc::format!("{home}/Desktop")
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

