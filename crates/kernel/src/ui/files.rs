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
//! знает ровно два цвета на всё окно. Список, в котором выбранный элемент
//! помечен стрелкой вместо подсветки, читается как вывод команды, а не как
//! список, по которому ходят.
//!
//! # Почему раскладка считается одной функцией
//!
//! Потому что нарисованное и нажимаемое обязаны совпадать. Пока кнопка «назад»
//! рисовалась одной формулой, а искалась под указателем другой, они сходились
//! ровно до первой правки отступа — и расхождение выглядело не как ошибка
//! раскладки, а как «мышь не работает». Теперь [`layout`] отвечает на вопрос
//! «где что лежит» один раз, а [`FilesView::draw`] и [`FilesView::click`]
//! спрашивают её.
//!
//! # Что он доказывает
//!
//! Что цепочка «virtio-blk → GPT → ext2 → VFS» работает не только в
//! диагностическом выводе ядра: права, владелец и размер в окне взяты из inode,
//! а просмотр файла читает его блоки по-настоящему.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface};

use super::paint::{self, Ctx, RowState, Tone};
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

/// С какой ширины окна появляется боковая колонка.
///
/// Колонка в 240 точек съедает у списка треть окна шириной 700; ниже этого
/// порога быстрый доступ мешает тому, ради чего окно открыли.
const SIDE_FROM: u32 = 700;

/// Одна строка списка.
struct Row {
    name: String,
    directory: bool,
    /// Лежит ли запись там, где живут программы.
    ///
    /// Признак строки, а не вопрос к пути на каждой отрисовке: путь у всех
    /// строк списка один, и спрашивать его двадцать раз подряд значит двадцать
    /// раз ответить одно и то же.
    program: bool,
    mode: u16,
    uid: u32,
    gid: u32,
    size: u64,
}

impl Row {
    /// Значок строки: каталог, пакет, программа или обычный файл.
    ///
    /// Исполняемый бит один в признак программы не годится: в образе initrd он
    /// стоит у **всего**, включая `README.TXT`, и список получался из одних
    /// коробок. Поэтому пакет узнаётся по расширению, программа — по
    /// исполняемому биту вместе с каталогом, где программам и место, а всё
    /// остальное остаётся файлом.
    fn icon(&self) -> (Icon, Tone, bool) {
        if self.directory {
            (Icon::Folder, Tone::Accent, true)
        } else if self.name.ends_with(".fpk") {
            (Icon::Package, Tone::Accent, false)
        } else if self.mode & 0o111 != 0 && self.program {
            (Icon::Terminal, Tone::Ok, false)
        } else {
            (Icon::File, Tone::Muted, false)
        }
    }
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
    /// Куда можно вернуться кнопкой «назад» — стек посещённых каталогов.
    ///
    /// Стек, а не одно «предыдущее место»: человек, зашедший на три уровня
    /// вниз, ждёт, что «назад» проведёт его тем же путём обратно, а не швырнёт
    /// в начало.
    back: Vec<String>,
    /// Куда можно пойти «вперёд» — то, откуда вернулись назад.
    forward: Vec<String>,
    rows: Vec<Row>,
    selected: usize,
    /// Ошибка чтения каталога вместо списка.
    error: Option<String>,
    preview: Option<Preview>,
}

impl FilesView {
    #[must_use]
    pub fn new() -> Self {
        let mut view = Self {
            path: String::from("/"),
            back: Vec::new(),
            forward: Vec::new(),
            rows: Vec::new(),
            selected: 0,
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
                    let program = self.path == "/bin" || self.path.ends_with("/bin");
                    self.rows.push(Row {
                        name: entry.name,
                        directory: entry.kind == NodeKind::Directory,
                        program,
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
            KeyCode::Enter | KeyCode::Right => self.open_selected(),
            KeyCode::Backspace => self.go_up(),
            // Влево — «назад», как у стрелки на панели: подниматься наверх
            // умеет Backspace, и две клавиши на одно действие ничего не дают.
            KeyCode::Left => self.go_back() || self.go_up(),
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
            self.go_to(target);
            return true;
        }
        self.preview = Some(read_preview(&row.name, &target));
        true
    }

    /// Перейти в каталог, запомнив, откуда пришли.
    ///
    /// Переход вперёд обнуляет список «вперёд» — как в любом обозревателе: путь,
    /// с которого свернули, перестаёт существовать.
    fn go_to(&mut self, path: String) {
        if path == self.path {
            return;
        }
        self.back.push(core::mem::replace(&mut self.path, path));
        self.forward.clear();
        self.preview = None;
        self.reload();
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
        self.go_to(parent);
        true
    }

    /// Вернуться туда, откуда пришли.
    pub fn go_back(&mut self) -> bool {
        let Some(previous) = self.back.pop() else {
            return false;
        };
        self.forward.push(core::mem::replace(&mut self.path, previous));
        self.preview = None;
        self.reload();
        true
    }

    /// Пойти обратно вперёд — туда, откуда вернулись назад.
    pub fn go_forward(&mut self) -> bool {
        let Some(next) = self.forward.pop() else {
            return false;
        };
        self.back.push(core::mem::replace(&mut self.path, next));
        self.preview = None;
        self.reload();
        true
    }

    /// Показать то, что открыли значком со стола.
    ///
    /// Каталог открывается сам; файл открывается **в своём каталоге** и сразу на
    /// просмотре: показать содержимое файла, не показав, где он лежит, значит
    /// оставить человека без единственного способа выйти из просмотра куда-то,
    /// кроме корня.
    pub fn reveal(&mut self, path: &str, directory: bool) {
        if directory {
            self.go_to(path.to_string());
            return;
        }
        let (parent, name) = match path.rfind('/') {
            Some(0) | None => (String::from("/"), path.trim_start_matches('/').to_string()),
            Some(index) => (path[..index].to_string(), path[index + 1..].to_string()),
        };
        if parent != self.path {
            self.go_to(parent);
        }
        if let Some(index) = self.rows.iter().position(|row| row.name == name) {
            self.selected = index;
        }
        self.preview = Some(read_preview(&name, path));
    }

    /// Перечитать текущий каталог, оставшись в нём.
    ///
    /// Нужно тому, кто изменил файл со стороны: созданный, переименованный или
    /// удалённый файл обязан появиться и исчезнуть в открытом окне, а не
    /// дожидаться, пока человек уйдёт из каталога и вернётся.
    pub fn refresh(&mut self) {
        self.preview = None;
        self.reload();
    }

    /// Щелчок по окну: координаты внутри области содержимого.
    ///
    /// Кнопки навигации, строки быстрого доступа и строки списка ищутся по той
    /// же [`layout`], по которой рисуются, — иначе они разъедутся при первом же
    /// изменении размера окна, и попасть в них будет можно только наугад.
    pub fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);

        if plan.toolbar.contains(x, y) {
            if plan.nav[0].contains(x, y) {
                return self.go_back();
            }
            if plan.nav[1].contains(x, y) {
                return self.go_forward();
            }
            if plan.nav[2].contains(x, y) {
                return self.go_up();
            }
            return false;
        }

        if let Some(side) = plan.side {
            if side.contains(x, y) {
                let inner = ctx.on(theme::panel_bg());
                for slot in place_slots(inner, side) {
                    if slot.rect.contains(x, y) {
                        if slot.path == self.path {
                            return false;
                        }
                        self.go_to(slot.path);
                        return true;
                    }
                }
                return false;
            }
        }

        // В просмотре список не показан, и щёлкать в нём не по чему: строки под
        // текстом файла нет, а «попал в невидимую строку» — это выбор вслепую.
        if self.preview.is_some() || !plan.list.contains(x, y) {
            return false;
        }
        let visible = plan.visible();
        let step = (plan.row_h + plan.row_gap).max(1) as i32;
        let offset = ((y - plan.list.y) / step).max(0) as usize;
        if offset >= visible {
            return false;
        }
        let index = self.first_visible(visible) + offset;
        if index >= self.rows.len() {
            return false;
        }
        // Щелчок по уже выбранной строке открывает её. Двойного щелчка окно не
        // получает — до содержимого доходит одно событие, — а открыть файл
        // мышью надо; повторное попадание в ту же строку и есть это «ещё раз».
        if self.selected == index {
            return self.open_selected();
        }
        self.selected = index;
        true
    }

    /// Нарисовать содержимое окна.
    ///
    /// Область приходит уже залитой фоном окна — это делает окно, потому что
    /// заливать её обязано и то содержимое, которое рисует одну строку. Второй
    /// заливки здесь нет намеренно: на 1080p она стоила бы лишних двух
    /// миллионов записей на каждое нажатие стрелки.
    pub fn draw(&self, surface: &mut Surface, area: Rect, ctx: Ctx) {
        let plan = layout(ctx, area);
        self.draw_toolbar(surface, ctx, &plan);
        if let Some(side) = plan.side {
            self.draw_side(surface, ctx, side);
        }
        match self.preview.as_ref() {
            Some(preview) => self.draw_preview(surface, ctx, &plan, preview),
            None => self.draw_list(surface, ctx, &plan),
        }
        self.draw_status(surface, ctx, &plan);
    }

    /// Панель инструментов: три стрелки и строка пути.
    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.toolbar.is_empty() {
            return;
        }
        s.fill(plan.toolbar, ctx.flat(p.panel));
        draw::hline(
            s,
            plan.toolbar.x,
            plan.toolbar.bottom() - 1,
            plan.toolbar.w,
            p.line.color,
            p.line.alpha,
        );
        // Всё, что лежит на панели, сводится поверх **панели**, а не поверх
        // окна: разница в один-два уровня яркости глазом не ловится, а на
        // снимке видна как кнопка чуть другого оттенка, чем соседняя.
        let bar = ctx.on(theme::panel_bg());

        // Недоступная кнопка гаснет, а не пропадает: «назад» из первого же
        // каталога не должно выглядеть как неисправность.
        let states = [
            (Icon::Back, !self.back.is_empty()),
            (Icon::Forward, !self.forward.is_empty()),
            (Icon::Up, self.path != "/"),
        ];
        for (rect, (icon, enabled)) in plan.nav.iter().zip(states) {
            nav_button(bar, s, *rect, icon, enabled);
        }

        if plan.path.is_empty() {
            return;
        }
        paint::sunk(bar, s, plan.path, ctx.px(theme::R_ROW));
        // Путь виден только здесь, поэтому он рисуется и в просмотре файла:
        // иначе, открыв файл, человек перестаёт понимать, где находится.
        let shown = match self.preview.as_ref() {
            Some(preview) => format!("{}  ·  {}", self.path, preview.name),
            None => self.path.clone(),
        };
        let pad = ctx.px(12);
        let mut x = plan.path.x + pad as i32;
        let mut room = plan.path.w.saturating_sub(pad * 2);
        let y = paint::baseline(bar, Role::Mono, plan.path);
        // Первая косая — акцентом: она отмечает корень, от которого читается
        // всё остальное, и без неё путь сливается в одну серую строку.
        if let Some(rest) = shown.strip_prefix('/') {
            let used = paint::text(bar, s, Role::Mono, x, y, "/", p.acc_ink);
            x += used as i32;
            room = room.saturating_sub(used);
            paint::text_clipped(bar, s, Role::Mono, x, y, room, rest, p.ink3);
        } else {
            paint::text_clipped(bar, s, Role::Mono, x, y, room, &shown, p.ink3);
        }
    }

    /// Боковая колонка быстрого доступа.
    fn draw_side(&self, s: &mut Surface, ctx: Ctx, side: Rect) {
        let p = ctx.palette;
        if side.is_empty() {
            return;
        }
        s.fill(side, ctx.flat(p.panel));
        draw::vline(s, side.right() - 1, side.y, side.h, p.line.color, p.line.alpha);
        let inner = ctx.on(theme::panel_bg());
        let pad = ctx.px(12);

        for slot in place_slots(inner, side) {
            if let Some(head) = slot.head {
                paint::caps(inner, s, side.x + ctx.px(16) as i32, slot.head_y, head);
            }
            let state = if slot.path == self.path {
                RowState::Selected
            } else {
                RowState::Idle
            };
            paint::row(inner, s, slot.rect, state);
            paint::text_clipped(
                inner,
                s,
                Role::Mono,
                slot.rect.x + pad as i32,
                paint::baseline(inner, Role::Mono, slot.rect),
                slot.rect.w.saturating_sub(pad * 2),
                &slot.path,
                paint::row_ink(inner, state),
            );
        }
    }

    /// Список файлов: заголовок столбцов и строки.
    fn draw_list(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.list.is_empty() {
            return;
        }

        if !plan.header.is_empty() {
            let x = plan.header.x + ctx.px(24) as i32;
            let y = paint::baseline(ctx, Role::MonoCaps, plan.header);
            paint::caps(ctx, s, x, y, "ИМЯ");
            let tail = ctx
                .face(Role::MonoCaps)
                .width_tracked("РАЗМЕР", theme::CAPS_TRACKING * ctx.scale);
            paint::caps(
                ctx,
                s,
                plan.header.right() - ctx.px(22) as i32 - tail as i32,
                y,
                "РАЗМЕР",
            );
            draw::hline(
                s,
                plan.header.x + ctx.px(12) as i32,
                plan.header.bottom() - 1,
                plan.header.w.saturating_sub(ctx.px(24)),
                p.line.color,
                p.line.alpha,
            );
        }

        let visible = plan.visible();
        if visible == 0 {
            return;
        }

        if let Some(error) = &self.error {
            let rect = plan.row_rect(ctx, 0);
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                rect.x + ctx.px(10) as i32,
                paint::baseline(ctx, Role::Body, rect),
                rect.w.saturating_sub(ctx.px(20)),
                error,
                p.bad_ink,
            );
            return;
        }
        if self.rows.is_empty() {
            let rect = plan.row_rect(ctx, 0);
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                rect.x + ctx.px(10) as i32,
                paint::baseline(ctx, Role::Body, rect),
                rect.w.saturating_sub(ctx.px(20)),
                "Пусто",
                p.ink5,
            );
            return;
        }

        // Прокрутка считается здесь, а не хранится: сколько строк помещается,
        // знает только тот, кто рисует, а размер окна может измениться.
        let first = self.first_visible(visible);
        for (offset, row) in self.rows.iter().skip(first).take(visible).enumerate() {
            let rect = plan.row_rect(ctx, offset);
            let selected = first + offset == self.selected;
            let state = if selected { RowState::Selected } else { RowState::Idle };
            paint::row(ctx, s, rect, state);

            let tile_side = ctx.px(26);
            let tile = Rect::new(
                rect.x + ctx.px(6) as i32,
                rect.y + (rect.h as i32 - tile_side as i32) / 2,
                tile_side,
                tile_side,
            );
            let (icon, tone, filled) = row.icon();
            paint::icon_tile(ctx, s, tile, icon, tone, filled);

            // Размер и права прижаты к правому краю: выровненные по левому краю
            // случайной длины имени, столбцы чисел не читаются вовсе.
            let mut right = rect.right() - ctx.px(10) as i32;
            let small = paint::baseline(ctx, Role::MonoSmall, rect);
            let size = if row.directory {
                String::from("—")
            } else {
                size_text(row.size)
            };
            paint::text_right(ctx, s, Role::MonoSmall, right, small, &size, p.ink4);
            right -= ctx.px(76) as i32;
            if rect.w > ctx.px(360) {
                let meta = format!("{:04o} {}:{}", row.mode, row.uid, row.gid);
                paint::text_right(ctx, s, Role::MonoSmall, right, small, &meta, p.ink4);
                right -= ctx.px(120) as i32;
            }

            let name_x = tile.right() + ctx.px(10) as i32;
            let room = (right - name_x).max(0) as u32;
            let (role, ink) = if selected {
                (Role::Title, p.ink)
            } else if row.directory {
                (Role::Label, p.ink2)
            } else {
                (Role::Body, p.ink3)
            };
            paint::text_clipped(
                ctx,
                s,
                role,
                name_x,
                paint::baseline(ctx, role, rect),
                room,
                &row.name,
                ink,
            );
        }
    }

    /// Содержимое открытого файла на месте списка.
    fn draw_preview(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, preview: &Preview) {
        let p = ctx.palette;
        let area = plan.list.union(&plan.header);
        if area.is_empty() {
            return;
        }
        let step = u32::from(ctx.face(Role::Mono).line) + ctx.px(2);
        let pad = ctx.px(16);
        let room = area.w.saturating_sub(pad * 2);
        let note_h = if preview.note.is_empty() { 0 } else { step * 2 };
        let body = (area.h.saturating_sub(note_h) / step.max(1)) as usize;

        for (offset, line) in preview.lines.iter().skip(preview.scroll).take(body).enumerate() {
            paint::text_clipped(
                ctx,
                s,
                Role::Mono,
                area.x + pad as i32,
                area.y + (offset as u32 * step) as i32,
                room,
                line,
                p.ink3,
            );
        }
        if !preview.note.is_empty() {
            paint::text_clipped(
                ctx,
                s,
                Role::Caption,
                area.x + pad as i32,
                area.bottom() - step as i32,
                room,
                &preview.note,
                p.ink4,
            );
        }
    }

    /// Строка состояния: без неё стрелки и Enter — это то, что надо угадать.
    fn draw_status(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.status.is_empty() {
            return;
        }
        s.fill(plan.status, ctx.flat(p.panel));
        draw::hline(
            s,
            plan.status.x,
            plan.status.y,
            plan.status.w,
            p.line2.color,
            p.line2.alpha,
        );
        let bar = ctx.on(theme::panel_bg());
        let text = if self.preview.is_some() {
            String::from("Стрелки — листать    Esc — назад к списку")
        } else if self.error.is_some() {
            String::from("Стрелка влево — назад    Backspace — вверх")
        } else {
            format!(
                "{} объектов    Enter — открыть    влево — назад    Backspace — вверх",
                self.rows.len()
            )
        };
        let pad = ctx.px(14);
        paint::text_clipped(
            bar,
            s,
            Role::MonoSmall,
            plan.status.x + pad as i32,
            paint::baseline(bar, Role::MonoSmall, plan.status),
            plan.status.w.saturating_sub(pad * 2),
            &text,
            p.ink4,
        );
    }

    /// Первая показанная строка при таком числе видимых.
    fn first_visible(&self, visible: usize) -> usize {
        if visible == 0 || self.selected < visible {
            0
        } else {
            self.selected + 1 - visible
        }
    }
}

impl Default for FilesView {
    fn default() -> Self {
        Self::new()
    }
}

/// Где что лежит в окне менеджера.
///
/// Все прямоугольники — в координатах поверхности окна, те же, в которых
/// приходит щелчок.
struct Plan {
    toolbar: Rect,
    /// «Назад», «вперёд», «вверх» — в этом порядке.
    nav: [Rect; 3],
    path: Rect,
    side: Option<Rect>,
    header: Rect,
    list: Rect,
    status: Rect,
    row_h: u32,
    row_gap: u32,
}

impl Plan {
    /// Сколько строк списка помещается.
    fn visible(&self) -> usize {
        let step = (self.row_h + self.row_gap).max(1);
        ((self.list.h + self.row_gap) / step) as usize
    }

    /// Строка списка с таким номером сверху.
    fn row_rect(&self, ctx: Ctx, offset: usize) -> Rect {
        let pad = ctx.px(12);
        Rect::new(
            self.list.x + pad as i32,
            self.list.y + (offset as u32 * (self.row_h + self.row_gap)) as i32,
            self.list.w.saturating_sub(pad * 2),
            self.row_h,
        )
    }
}

/// Посчитать раскладку окна.
///
/// Одна функция на отрисовку и на щелчок — см. заголовок модуля.
fn layout(ctx: Ctx, area: Rect) -> Plan {
    let pad = ctx.px(14);
    let toolbar_h = ctx.px(theme::TOOLBAR_H).min(area.h);
    let toolbar = Rect::new(area.x, area.y, area.w, toolbar_h);

    let btn = ctx.px(30);
    let gap = ctx.px(6);
    let btn_y = toolbar.y + (toolbar_h as i32 - btn as i32) / 2;
    let mut x = area.x + pad as i32;
    let mut nav = [Rect::EMPTY; 3];
    for slot in &mut nav {
        *slot = Rect::new(x, btn_y, btn, btn);
        x += (btn + gap) as i32;
    }

    let path_h = ctx.px(32);
    let path_x = x + ctx.px(4) as i32;
    let path_w = (area.right() - pad as i32 - path_x).max(0) as u32;
    let path = Rect::new(
        path_x,
        toolbar.y + (toolbar_h as i32 - path_h as i32) / 2,
        path_w,
        path_h,
    );

    let status_h = ctx.px(theme::STATUS_H);
    let status_y = (area.bottom() - status_h as i32).max(toolbar.bottom());
    let status = Rect::new(area.x, status_y, area.w, status_h);

    let top = toolbar.bottom();
    let bottom = status.y;
    let body_h = (bottom - top).max(0) as u32;

    // Колонка есть только там, где после неё остаётся окно, а не щель.
    let side = if area.w > ctx.px(SIDE_FROM) && body_h > 0 {
        Some(Rect::new(area.x, top, ctx.px(theme::SIDE_W), body_h))
    } else {
        None
    };
    let body_x = side.map_or(area.x, |side| side.right());
    let body_w = (area.right() - body_x).max(0) as u32;

    let header_h = (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14)).min(body_h);
    let header = Rect::new(body_x, top, body_w, header_h);
    let list = Rect::new(
        body_x,
        top + header_h as i32,
        body_w,
        body_h.saturating_sub(header_h),
    );

    Plan {
        toolbar,
        nav,
        path,
        side,
        header,
        list,
        status,
        row_h: ctx.px(32),
        row_gap: ctx.px(2),
    }
}

/// Кнопка навигации: доступная — с подложкой, недоступная — одним значком.
///
/// Рисуется здесь, а не через [`paint::icon_button`]: у той кнопки состояния
/// «под указателем», а у этой — «есть куда идти», и цвет значка в них меняется
/// по-разному.
fn nav_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Icon, enabled: bool) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_CHIP);
    if enabled {
        draw::rounded(s, rect, r, ctx.flat(p.ghost), 255);
        draw::rounded_stroke(s, rect, r, p.line2.color, p.line2.alpha);
    }
    // Та же доля, что у значка в кнопке заголовка: 11 точек в кнопке 30.
    let side = rect.w.min(rect.h) * 11 / 30;
    let x = rect.x + (rect.w as i32 - side as i32) / 2;
    let y = rect.y + (rect.h as i32 - side as i32) / 2;
    let ink = if enabled { p.ink3 } else { p.ink6 };
    glyphicon::draw(s, icon, x, y, side, ink, 255);
}

/// Строка быстрого доступа вместе с местом, которое она занимает.
struct PlaceSlot {
    /// Заголовок группы над строкой, если строка её открывает.
    head: Option<&'static str>,
    head_y: i32,
    path: String,
    rect: Rect,
}

/// Куда ведёт быстрый доступ.
///
/// Список короткий и составлен из того, что в системе точно есть: корень, дом
/// вошедшего, его стол и два системных каталога. Закладок человек пока не
/// заводит — заводить их некуда, файла настроек у стола нет.
fn places() -> [(Option<&'static str>, String); 5] {
    [
        (Some("МЕСТА"), String::from("/")),
        (None, super::context::home_dir()),
        (None, super::context::desktop_dir()),
        (Some("СИСТЕМА"), String::from("/bin")),
        (None, String::from("/etc")),
    ]
}

/// Разложить быстрый доступ по колонке.
fn place_slots(ctx: Ctx, side: Rect) -> Vec<PlaceSlot> {
    let padx = ctx.px(16);
    let row_h = ctx.px(32);
    let gap = ctx.px(2);
    let caps_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(12);
    let mut y = side.y + ctx.px(10) as i32;
    let mut out = Vec::new();

    for (head, path) in places() {
        let head_y = if head.is_some() {
            let at = y + ctx.px(4) as i32;
            y += caps_h as i32;
            at
        } else {
            0
        };
        let rect = Rect::new(
            side.x + padx as i32,
            y,
            side.w.saturating_sub(padx * 2),
            row_h,
        );
        y += (row_h + gap) as i32;
        // Не поместилось — не рисуем: строка, наполовину заехавшая под список,
        // выглядит как испорченная отрисовка, а не как «здесь кончилось место».
        if rect.bottom() > side.bottom() {
            break;
        }
        out.push(PlaceSlot { head, head_y, path, rect });
    }
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
