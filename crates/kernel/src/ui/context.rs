//! Меню по правому щелчку: то, что можно сделать прямо на столе.
//!
//! # Почему это отдельный слой, а не меню запуска в другом месте
//!
//! Меню запуска знает свой список программ и своё место у панели; это меню
//! появляется там, где щёлкнули, и его пункты — действия, а не программы.
//! Общая структура на двоих означала бы одно перечисление, половина вариантов
//! которого не имеет смысла для второй половины случаев.
//!
//! # Что оно делает и чего не делает
//!
//! Создаёт каталог и текстовый файл в каталоге стола (`~/Desktop`) и открывает
//! «Параметры» на разделе экрана. Ни переименования, ни удаления здесь нет:
//! то и другое требует выбранного файла, а выбирать на столе пока нечего — стол
//! показывает системные значки, а не содержимое каталога. Это следующий шаг, и
//! он назван вслух, чтобы пустой пункт «Удалить» не появился раньше него.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::text::{self, GLYPH_H, GLYPH_W};
use mini_ui::{Rect, Surface};

use super::theme;
use crate::vfs::perm::{Access, Credentials};

/// Что предлагает меню стола.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Создать каталог в каталоге стола.
    NewFolder,
    /// Создать пустой текстовый файл там же.
    NewTextFile,
    /// Открыть «Параметры» на разделе экрана.
    DisplaySettings,
    /// Перерисовать стол целиком.
    Refresh,
}

impl Action {
    const ALL: [Action; 4] = [
        Action::NewFolder,
        Action::NewTextFile,
        Action::DisplaySettings,
        Action::Refresh,
    ];

    const fn title(self) -> &'static str {
        match self {
            Action::NewFolder => "New folder",
            Action::NewTextFile => "New text file",
            Action::DisplaySettings => "Display settings",
            Action::Refresh => "Refresh",
        }
    }
}

/// Меню стола: где оно и что в нём выбрано.
pub struct ContextMenu {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    selected: usize,
    open: bool,
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
        let width = (widest + GLYPH_W * scale * 8).max(GLYPH_W * scale * 24);
        let height = row_h * (Action::ALL.len() as u32 + 1) + theme::PADDING * 2;
        let surface = Surface::new(width, height, theme::WINDOW_BG)?;
        Some(Self {
            surface,
            rect: Rect::new(0, 0, width, height),
            scale,
            selected: 0,
            open: false,
            damage: Rect::EMPTY,
            note: None,
        })
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Открыть меню в точке экрана, не выпуская его за края.
    pub fn open_at(&mut self, x: i32, y: i32, screen: (u32, u32), work_bottom: i32) {
        let max_x = (screen.0 as i32 - self.rect.w as i32).max(0);
        let max_y = (work_bottom - self.rect.h as i32).max(0);
        self.rect.x = x.clamp(0, max_x);
        self.rect.y = y.clamp(0, max_y);
        self.selected = 0;
        self.note = None;
        self.open = true;
        self.redraw();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Пункт под точкой экрана.
    #[must_use]
    pub fn action_at(&self, x: i32, y: i32) -> Option<Action> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let row_h = row_height(self.scale);
        let local = (y - self.rect.y - theme::PADDING as i32).max(0) as u32;
        let index = (local / row_h) as usize;
        Action::ALL.get(index).copied()
    }

    /// Показать ответ действия, не закрывая меню.
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
        self.redraw();
    }

    fn redraw(&mut self) {
        let scale = self.scale;
        let row_h = row_height(scale);
        let bounds = self.surface.bounds();
        self.surface.fill(bounds, theme::WINDOW_BG);
        self.surface.frame(bounds, theme::BORDER, theme::ACCENT);

        for (index, action) in Action::ALL.iter().enumerate() {
            let y = theme::PADDING + index as u32 * row_h;
            let chosen = index == self.selected;
            if chosen {
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
            text::draw_text(
                &mut self.surface,
                GLYPH_W * scale,
                y + scale,
                action.title(),
                scale,
                theme::TEXT,
                None,
            );
        }

        if let Some(note) = &self.note {
            let y = theme::PADDING + Action::ALL.len() as u32 * row_h;
            let room = ((bounds.w - GLYPH_W * scale * 2) / (GLYPH_W * scale)) as usize;
            let text: String = if note.chars().count() > room {
                note.chars().take(room.saturating_sub(1)).collect::<String>() + "~"
            } else {
                note.to_string()
            };
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

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }
}

fn row_height(scale: u32) -> u32 {
    GLYPH_H * scale + theme::PADDING * 2
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
/// писать поверх уже существующего нельзя, а требовать от человека придумать
/// имя до создания — это диалог, которого на этой фазе нет.
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
        if crate::fs::resolve_as(crate::user::session::credentials(), &path, Access::NONE)
            .is_some_and(|result| result.is_ok())
        {
            continue;
        }
        let done = if directory {
            crate::fs::mkdir_as(crate::user::session::credentials(), &path, 0o755)
        } else {
            crate::fs::create_as(crate::user::session::credentials(), &path, 0o644).map(|r| r.map(|_| ()))
        };
        return match done {
            Some(Ok(())) => Ok(name),
            Some(Err(err)) => Err(alloc::format!("{err}")),
            None => Err("no filesystem is mounted".to_string()),
        };
    }
    Err("too many entries with that name".to_string())
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
        if crate::fs::resolve_as(crate::user::session::credentials(), &built, Access::NONE)
            .is_some_and(|result| result.is_ok())
        {
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

/// Все пункты — для перебора снаружи.
#[must_use]
pub fn actions() -> Vec<Action> {
    Action::ALL.to_vec()
}
