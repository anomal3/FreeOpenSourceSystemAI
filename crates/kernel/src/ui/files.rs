//! Файловый менеджер: содержимое смонтированного корня в окне.
//!
//! # Зачем он в ядре
//!
//! По той же причине, по которой в ядре живёт оболочка: пользовательского
//! пространства ещё нет, и «программа» на этой фазе — это модуль. Граница всё
//! равно проведена там, где она будет проходить и потом: менеджер обращается к
//! файловой системе только через [`crate::fs`], то есть через тот же путь, что и
//! `ls` в оболочке, и ничего не знает ни про ext2, ни про virtio-blk.
//!
//! # Почему он рисует себя сам, а не печатает строки в сетку символов
//!
//! Потому что выделенная строка — это заливка прямоугольника, а сетка символов
//! ([`mini_ui::text::TextGrid`]) знает ровно два цвета на всё окно. Список,
//! в котором выбранный элемент помечен стрелкой вместо подсветки, читается как
//! вывод команды, а не как список, по которому ходят.
//!
//! # Что он доказывает
//!
//! Что цепочка «virtio-blk → GPT → ext2 → VFS» работает не только в
//! диагностическом выводе ядра: права, владелец и размер в окне взяты из inode,
//! а просмотр файла читает его блоки по-настоящему.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::text::{self, GLYPH_H};
use mini_ui::{Rect, Surface};

use super::theme;
use crate::fs;
use crate::input::KeyCode;
use crate::vfs::NodeKind;

/// Сколько байт файла показывает просмотр.
///
/// Предел не косметический: размер файла приходит с носителя, и окно, в которое
/// вывалили сорок мегабайт, — это заполненная куча и остановка системы.
const PREVIEW_LIMIT: usize = 8 * 1024;

/// Сколько строк файла показывается.
const PREVIEW_LINES: usize = 256;

/// Одна строка списка.
struct Row {
    name: String,
    directory: bool,
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
}

/// Просмотр файла.
struct Preview {
    name: String,
    lines: Vec<String>,
    /// Пояснение под текстом: сколько показано и почему не всё.
    note: String,
    /// Сколько строк пролистано.
    scroll: usize,
}

pub struct FilesView {
    path: String,
    rows: Vec<Row>,
    selected: usize,
    scroll: usize,
    /// Ошибка чтения каталога вместо списка.
    error: Option<String>,
    preview: Option<Preview>,
}

impl FilesView {
    #[must_use]
    pub fn new() -> Self {
        let mut view = Self {
            path: String::from("/"),
            rows: Vec::new(),
            selected: 0,
            scroll: 0,
            error: None,
            preview: None,
        };
        view.reload();
        view
    }

    /// Перечитать текущий каталог.
    fn reload(&mut self) {
        self.rows.clear();
        self.selected = 0;
        self.scroll = 0;
        self.error = None;

        match fs::list(&self.path) {
            Some(Ok(entries)) => {
                for entry in entries {
                    // «.» и «..» приходят от ext2 как настоящие записи. Свою
                    // навигацию мы уже дали (Backspace), а две строки, ведущие
                    // «сюда же» и «наверх», в списке только мешают.
                    if entry.name == "." || entry.name == ".." {
                        continue;
                    }
                    self.rows.push(Row {
                        name: entry.name,
                        directory: entry.kind == NodeKind::Directory,
                        mode: entry.mode,
                        uid: entry.uid,
                        gid: entry.gid,
                        size: entry.size,
                    });
                }
                // Каталоги наверх, дальше по имени: порядок записей в ext2 —
                // это порядок вставки, то есть для человека случайный.
                self.rows.sort_by(|a, b| {
                    b.directory
                        .cmp(&a.directory)
                        .then_with(|| a.name.cmp(&b.name))
                });
            }
            Some(Err(err)) => self.error = Some(format!("{err}")),
            None => self.error = Some("no filesystem is mounted".to_string()),
        }
    }

    /// Обработать клавишу. Возвращает `true`, если картинку надо перерисовать.
    pub fn handle(&mut self, code: KeyCode) -> bool {
        if self.preview.is_some() {
            return self.handle_preview(code);
        }
        match code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                true
            }
            KeyCode::Home => {
                self.selected = 0;
                true
            }
            KeyCode::End => {
                self.selected = self.rows.len().saturating_sub(1);
                true
            }
            KeyCode::Enter => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left => self.go_up(),
            _ => false,
        }
    }

    fn handle_preview(&mut self, code: KeyCode) -> bool {
        let Some(preview) = self.preview.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Escape | KeyCode::Backspace | KeyCode::Left => {
                self.preview = None;
                true
            }
            KeyCode::Down => {
                if preview.scroll + 1 < preview.lines.len() {
                    preview.scroll += 1;
                }
                true
            }
            KeyCode::Up => {
                preview.scroll = preview.scroll.saturating_sub(1);
                true
            }
            KeyCode::Home => {
                preview.scroll = 0;
                true
            }
            _ => false,
        }
    }

    /// Войти в каталог или открыть файл на просмотр.
    fn open_selected(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else {
            return false;
        };
        let target = join(&self.path, &row.name);
        if row.directory {
            self.path = target;
            self.reload();
            return true;
        }
        self.preview = Some(read_preview(&row.name, &target));
        true
    }

    /// Подняться на уровень выше.
    fn go_up(&mut self) -> bool {
        if self.path == "/" {
            return false;
        }
        let parent = match self.path.rfind('/') {
            Some(0) | None => String::from("/"),
            Some(index) => self.path[..index].to_string(),
        };
        self.path = parent;
        self.reload();
        true
    }

    /// Нарисовать содержимое окна.
    pub fn draw(&self, surface: &mut Surface, area: Rect, scale: u32) {
        surface.fill(area, theme::WINDOW_BG);
        let line_h = GLYPH_H * scale + 2;
        if area.h < line_h * 3 {
            return;
        }

        let left = area.x as u32;
        let mut y = area.y as u32;

        // Заголовок: где мы находимся. Он же — единственное место, где виден
        // путь целиком, поэтому рисуется всегда, и в списке, и в просмотре.
        let header = match self.preview.as_ref() {
            Some(preview) => format!("{}  -  {}", self.path, preview.name),
            None => self.path.clone(),
        };
        text::draw_text(surface, left, y, &header, scale, theme::ACCENT, None);
        y += line_h + line_h / 2;

        let footer_h = line_h * 2;
        let body_bottom = (area.bottom() as u32).saturating_sub(footer_h);
        let visible = ((body_bottom.saturating_sub(y)) / line_h) as usize;

        match self.preview.as_ref() {
            Some(preview) => {
                self.draw_preview(surface, preview, area, left, y, line_h, visible, scale)
            }
            None => self.draw_list(surface, area, left, y, line_h, visible, scale),
        }

        // Подсказка внизу: без неё стрелки и Enter — это то, что надо угадать.
        let hint_y = (area.bottom() as u32).saturating_sub(line_h);
        let hint = if self.preview.is_some() {
            "Up/Down scroll    Esc back to the list"
        } else {
            "Up/Down select    Enter open    Backspace up"
        };
        text::draw_text(surface, left, hint_y, hint, scale, theme::DIM, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_list(
        &self,
        surface: &mut Surface,
        area: Rect,
        left: u32,
        top: u32,
        line_h: u32,
        visible: usize,
        scale: u32,
    ) {
        if let Some(error) = &self.error {
            text::draw_text(surface, left, top, error, scale, theme::CLOSE, None);
            return;
        }
        if self.rows.is_empty() {
            text::draw_text(surface, left, top, "(empty)", scale, theme::DIM, None);
            return;
        }

        // Прокрутка считается здесь, а не хранится: сколько строк помещается,
        // знает только тот, кто рисует, а размер окна может измениться.
        let first = if self.selected >= visible {
            self.selected + 1 - visible
        } else {
            0
        };

        for (offset, row) in self.rows.iter().skip(first).take(visible).enumerate() {
            let index = first + offset;
            let y = top + offset as u32 * line_h;
            let selected = index == self.selected;
            if selected {
                surface.fill(
                    Rect::new(area.x, y as i32 - 1, area.w, line_h),
                    theme::SELECT_BG,
                );
            }
            let name_color = if row.directory {
                theme::DIRECTORY
            } else {
                theme::TEXT
            };
            // Три колонки с фиксированным началом: права и размер, выровненные
            // по левому краю случайной длины имени, не читаются вовсе.
            let meta = format!(
                "{:04o} {:>4}:{:<4} {:>9}",
                row.mode,
                row.uid,
                row.gid,
                if row.directory {
                    String::from("-")
                } else {
                    size_text(row.size)
                }
            );
            text::draw_text(surface, left, y, &meta, scale, theme::DIM, None);
            let name_x = left + text::width_of(&meta, scale) + text::GLYPH_W * scale;
            let name = if row.directory {
                format!("{}/", row.name)
            } else {
                row.name.clone()
            };
            let room = (area.right() as u32).saturating_sub(name_x);
            text::draw_text(
                surface,
                name_x,
                y,
                &fit(&name, columns(room, scale)),
                scale,
                name_color,
                None,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_preview(
        &self,
        surface: &mut Surface,
        preview: &Preview,
        area: Rect,
        left: u32,
        top: u32,
        line_h: u32,
        visible: usize,
        scale: u32,
    ) {
        let width = columns(area.w, scale);
        let body = visible.saturating_sub(1);
        for (offset, line) in preview.lines.iter().skip(preview.scroll).take(body).enumerate() {
            let y = top + offset as u32 * line_h;
            text::draw_text(surface, left, y, &fit(line, width), scale, theme::TEXT, None);
        }
        if !preview.note.is_empty() {
            let y = top + body as u32 * line_h;
            text::draw_text(surface, left, y, &preview.note, scale, theme::DIM, None);
        }
    }
}

impl Default for FilesView {
    fn default() -> Self {
        Self::new()
    }
}

/// Сколько знаков помещается в полосу шириной `width`.
fn columns(width: u32, scale: u32) -> usize {
    (width / (text::GLYPH_W * scale.max(1))) as usize
}

/// Обрезать строку по ширине, пометив обрезку.
///
/// Обрезать обязательно: рисование текста молча уходит за границу поверхности,
/// и длинное имя выглядело бы как обрубленное на полбукве — то есть как дефект
/// вывода, а не как «здесь не поместилось». Знак `>` на конце говорит, что
/// строка продолжается; многоточия в шрифте 8×8 нет.
fn fit(text: &str, columns: usize) -> String {
    if text.chars().count() <= columns {
        return text.to_string();
    }
    if columns == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(columns - 1).collect();
    out.push('>');
    out
}

/// Собрать путь к записи внутри каталога.
fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Размер в виде, который читается с одного взгляда.
fn size_text(bytes: u64) -> String {
    if bytes < 10 * 1024 {
        format!("{bytes}")
    } else if bytes < 10 * 1024 * 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{}M", bytes / (1024 * 1024))
    }
}

/// Прочитать файл для просмотра.
fn read_preview(name: &str, path: &str) -> Preview {
    let mut lines = Vec::new();
    let mut note = String::new();

    match fs::read(path, PREVIEW_LIMIT) {
        Some(Ok((bytes, total))) => match core::str::from_utf8(&bytes) {
            Ok(text) => {
                for line in text.lines().take(PREVIEW_LINES) {
                    // Табуляции и управляющие байты испортили бы разметку строки:
                    // рисование текста не знает про них ничего.
                    lines.push(line.replace('\t', "    "));
                }
                if total > bytes.len() as u64 {
                    note = format!("... {} of {total} bytes shown", bytes.len());
                }
            }
            // Двоичный файл не показывается вовсе, а не показывается мусором:
            // из «шрифт нарисовал непечатное» никто не сделает вывода, что файл
            // двоичный.
            Err(_) => note = format!("binary file, {} bytes", bytes.len()),
        },
        Some(Err(err)) => note = format!("cannot read: {err}"),
        None => note = String::from("no filesystem is mounted"),
    }

    Preview { name: name.to_string(), lines, note, scroll: 0 }
}
