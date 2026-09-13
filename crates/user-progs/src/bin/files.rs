//! Файловый менеджер: содержимое смонтированного корня в окне программы.
//!
//! # Что здесь изменилось по сравнению с прошлой фазой
//!
//! Это окно было **частью ядра** — модуль `ui::files`, девятьсот строк, которые
//! читали файловую систему прямым вызовом и рисовали прямо в поверхность окна,
//! заведённого композитором. Теперь окно просит программа, каталог читается
//! системными вызовами, а рисует она сама — тем же `mini-ui`, которым рисует
//! себя стол.
//!
//! Смысл переезда не в красоте. Менеджер — это разбор чужих имён, чужих прав и
//! чужого содержимого; падать он обязан вместе со своим окном, а не вместе с
//! машиной. Это последнее большое окно, которое ядро рисовало само.
//!
//! # Что он доказывает
//!
//! Что цепочка «virtio-blk → GPT → ext2 → VFS → системный вызов» работает не
//! только в выводе команды: права, владелец и размер в окне взяты из inode, а
//! просмотр файла читает его блоки по-настоящему — и всё это из третьего
//! кольца, через `SYS_READDIR` и `SYS_READ`.
//!
//! # Почему раскладка считается одной функцией
//!
//! Потому что нарисованное и нажимаемое обязаны совпадать. Пока кнопка «назад»
//! рисовалась одной формулой, а искалась под указателем другой, они сходились
//! ровно до первой правки отступа — и расхождение выглядело не как ошибка
//! раскладки, а как «мышь не работает». [`layout`] отвечает на вопрос «где что
//! лежит» один раз, а [`Files::draw`] и [`Files::click`] её спрашивают.
//!
//! # Аргументы: `files [путь]`
//!
//! Путь — каталог, с которого начать. Его передаёт стол, когда человек открыл
//! значок папки: до переезда то же самое делал вызов `reveal` внутри ядра.
//! Файл в аргументе тоже годится — менеджер откроет его каталог и покажет
//! содержимое, потому что показать файл, не показав, где он лежит, значит
//! оставить человека без единственного способа выйти из просмотра куда-то,
//! кроме корня.
//!
//! # Чего программа не знает и знать пока неоткуда
//!
//! **Имени вошедшего.** Оно есть у ядра (`/etc/passwd` читает оно, а права на
//! этот файл — `0640 root`), и до программ не доходит ничем. Поэтому домашний
//! каталог в боковой колонке — это `/root` для нулевого uid и `/home` для
//! остальных: первое верно, второе честно. Гадать имя по uid нечем, а
//! показывать `/home/roman` всем подряд — хуже, чем показать общий каталог.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};
use user_abi::{Dirent, KIND_DIRECTORY, Stat};
use user_progs::{
    Args, SYSINFO_DARK, SysInfo, WIN_CLOSE, WIN_KEY, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME,
    WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, WIN_POINTER,
    Window, close, exit, monotonic_ms, nanosleep, open, println, read, readdir_raw, stat,
    sysinfo,
};

/// Имя окна.
///
/// Латиницей и именно это слово: по нему автоматический стенд наводит мышь
/// (`Aim::Close("Files")`), и оно же стояло у окна, пока оно было частью ядра.
/// Переезд не должен быть заметен снаружи — в этом половина его проверки.
const TITLE: &str = "Files";

/// Сколько байт файла показывает просмотр.
///
/// Предел не косметический: размер файла приходит с носителя, и окно, в которое
/// вывалили сорок мегабайт, — это заполненная куча и остановка программы.
const PREVIEW_LIMIT: usize = 8 * 1024;

/// Сколько строк файла показывается.
const PREVIEW_LINES: usize = 256;

/// С какой ширины окна появляется боковая колонка.
///
/// Колонка в 240 точек съедает у списка треть окна шириной 700; ниже этого
/// порога быстрый доступ мешает тому, ради чего окно открыли.
const SIDE_FROM: u32 = 700;

/// Сколько записей каталога читается.
///
/// Предел здесь по той же причине, по которой он есть у просмотра: число
/// записей приходит с носителя. Тысяча строк — это больше, чем помещается на
/// любой экран, умноженное на запас; каталог длиннее показывается не целиком, и
/// об этом написано в строке состояния, а не умалчивается.
const MAX_ROWS: usize = 1024;

/// Пауза между опросами очереди событий.
const POLL_NS: u32 = 30_000_000;

/// Как часто спрашивать систему о теме.
///
/// События «тема изменилась» договор не знает — своего состояния стола у
/// программы нет, — поэтому признак перечитывается. Две секунды: человек
/// переключает тему руками и замечает задержку в две секунды как «сработало», а
/// не как «не сработало».
const THEME_PERIOD_MS: u64 = 2_000;

/// Сколько всего ждать окна при запуске.
///
/// Полминуты, как у монитора системы, и по той же причине: при загрузке система
/// много печатает, а каждая строка в окне оболочки — перерисовка, на которую
/// стол берут целиком.
const OPEN_WAIT_MS: u64 = 30_000;

/// Сколько ждать графики при запуске.
const WAIT_GRAPHICS_MS: u64 = 10_000;

// ---------------------------------------------------------------------------
// Данные
// ---------------------------------------------------------------------------

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
    mode: u32,
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

/// Состояние менеджера.
struct Files {
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
    /// Сколько записей каталога не поместилось в [`MAX_ROWS`].
    dropped: usize,
    /// Ошибка чтения каталога вместо списка.
    error: Option<String>,
    preview: Option<Preview>,
    /// Домашний каталог для боковой колонки.
    home: String,
    /// Знакомый вид: подписи вместо имён, служебные деревья свёрнуты.
    ///
    /// По умолчанию включён, и это не мелочь: система, которую человек видит
    /// впервые, встречает его словами «Программы» и «Настройки», а не `/bin` и
    /// `/etc`. Выключается клавишей — тем же способом, каким везде показывают
    /// скрытые файлы, — и тогда видно настоящие имена и все каталоги.
    ///
    /// Настоящий путь при этом виден **всегда**, в адресной строке: подпись
    /// украшает, а не прячет. Подпись, вытеснившая путь, превращает «нет такого
    /// файла» в загадку.
    friendly: bool,
}

impl Files {
    fn new(path: String, home: String) -> Self {
        let mut view = Self {
            path,
            back: Vec::new(),
            forward: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            dropped: 0,
            error: None,
            preview: None,
            home,
            friendly: true,
        };
        view.reload();
        view
    }

    /// Перечитать текущий каталог.
    fn reload(&mut self) {
        self.rows.clear();
        self.selected = 0;
        self.dropped = 0;
        self.error = None;

        match list_dir(&self.path) {
            Ok((rows, dropped)) => {
                // Служебные деревья сворачиваются здесь, а не при отрисовке:
                // иначе стрелка вниз ходила бы по невидимым строкам, и выделение
                // пропадало бы на ровном месте.
                self.rows = if self.friendly {
                    rows.into_iter()
                        .filter(|row| !sysconf::winpath::is_folded(&join(&self.path, &row.name)))
                        .collect()
                } else {
                    rows
                };
                self.dropped = dropped;
                // Каталоги наверх, дальше по имени: порядок записей в ext2 —
                // это порядок вставки, то есть для человека случайный.
                self.rows.sort_by(|a, b| {
                    b.directory
                        .cmp(&a.directory)
                        .then_with(|| a.name.cmp(&b.name))
                });
            }
            Err(text) => self.error = Some(text),
        }
    }

    /// Обработать клавишу. `true` — картинку надо перерисовать.
    fn key(&mut self, code: u32) -> bool {
        if self.preview.is_some() {
            return self.key_preview(code);
        }
        match code {
            WIN_KEY_UP => {
                self.selected = self.selected.saturating_sub(1);
                true
            }
            WIN_KEY_DOWN => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                true
            }
            WIN_KEY_HOME => {
                self.selected = 0;
                true
            }
            WIN_KEY_END => {
                self.selected = self.rows.len().saturating_sub(1);
                true
            }
            // Enter приезжает символом: у него он есть, и договор отдаёт
            // символ раньше имени.
            ENTER | WIN_KEY_RIGHT => self.open_selected(),
            BACKSPACE => self.go_up(),
            // Влево — «назад», как у стрелки на панели: подниматься наверх
            // умеет Backspace, и две клавиши на одно действие ничего не дают.
            WIN_KEY_LEFT => self.go_back() || self.go_up(),
            // Обновить. В ядре этого пункта не было и не требовалось: меню
            // стола само звало `refresh_files` у открытого окна. Через границу
            // привилегий такого вызова нет, и чинить это извещением о смене
            // каталога — работа не этой фазы. Поэтому клавиша, и она названа в
            // строке состояния, а не оставлена на угадывание.
            REFRESH => {
                self.reload();
                true
            }
            // Переключить вид. Выделение сбрасывается вместе с перечитыванием
            // списка — иначе номер строки указывал бы на другую запись: в
            // обычном виде каталогов больше.
            VIEW => {
                self.friendly = !self.friendly;
                self.reload();
                println(&format!(
                    "files: view {}",
                    if self.friendly { "friendly" } else { "plain" }
                ));
                true
            }
            _ => false,
        }
    }

    /// Как называется эта запись на экране.
    ///
    /// Подпись есть у немногих каталогов и только на верхних уровнях — таблица в
    /// `sysconf::winpath`. У остальных возвращается настоящее имя: сочинять
    /// перевод для `/usr/lib` значило бы показывать человеку слово, которого он
    /// нигде больше не увидит.
    fn shown_name<'a>(&self, row: &'a Row) -> &'a str {
        if !self.friendly {
            return &row.name;
        }
        match sysconf::winpath::label(&join(&self.path, &row.name)) {
            Some(label) => label,
            None => &row.name,
        }
    }

    fn key_preview(&mut self, code: u32) -> bool {
        let Some(preview) = self.preview.as_mut() else {
            return false;
        };
        let page = PREVIEW_PAGE;
        match code {
            ESCAPE | BACKSPACE | WIN_KEY_LEFT => {
                self.preview = None;
                true
            }
            WIN_KEY_DOWN => {
                if preview.scroll + 1 < preview.lines.len() {
                    preview.scroll += 1;
                }
                true
            }
            WIN_KEY_UP => {
                preview.scroll = preview.scroll.saturating_sub(1);
                true
            }
            WIN_KEY_PAGE_DOWN => {
                preview.scroll = (preview.scroll + page).min(preview.lines.len().saturating_sub(1));
                true
            }
            WIN_KEY_PAGE_UP => {
                preview.scroll = preview.scroll.saturating_sub(page);
                true
            }
            WIN_KEY_HOME => {
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
        // Отдельная строка, а не «список перечитан»: «выделение переехало» и
        // «мы вошли внутрь» печатают одно и то же — путь и число строк, — и
        // отличить одно от другого снаружи было бы нечем.
        println(&format!("files: entered '{}'", self.path));
    }

    /// Подняться на уровень выше.
    fn go_up(&mut self) -> bool {
        if self.path == "/" {
            return false;
        }
        let parent = parent_of(&self.path);
        self.go_to(parent);
        true
    }

    /// Вернуться туда, откуда пришли.
    fn go_back(&mut self) -> bool {
        let Some(previous) = self.back.pop() else {
            return false;
        };
        self.forward.push(core::mem::replace(&mut self.path, previous));
        self.preview = None;
        self.reload();
        true
    }

    /// Пойти обратно вперёд — туда, откуда вернулись назад.
    fn go_forward(&mut self) -> bool {
        let Some(next) = self.forward.pop() else {
            return false;
        };
        self.back.push(core::mem::replace(&mut self.path, next));
        self.preview = None;
        self.reload();
        true
    }

    /// Щелчок по окну: координаты внутри поверхности.
    ///
    /// Кнопки навигации, строки быстрого доступа и строки списка ищутся по той
    /// же [`layout`], по которой рисуются, — иначе они разъедутся при первом же
    /// изменении размера окна, и попасть в них будет можно только наугад.
    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
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
                for slot in place_slots(inner, side, &self.home) {
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

    /// Нарисовать окно целиком.
    ///
    /// Фон заливается здесь, а не приходит готовым: до переезда область
    /// заливало окно ядра, а у программы поверхность своя и заливать её больше
    /// некому.
    fn draw(&self, surface: &mut Surface, area: Rect, ctx: Ctx) {
        surface.fill(area, theme::window_bg());
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

        for slot in place_slots(inner, side, &self.home) {
            if let Some(head) = slot.head {
                paint::caps(inner, s, side.x + ctx.px(16) as i32, slot.head_y, head);
            }
            let state = if slot.path == self.path {
                RowState::Selected
            } else {
                RowState::Idle
            };
            paint::row(inner, s, slot.rect, state);
            // В боковой колонке подпись уместна больше всего: это ровно те
            // места, у которых знакомое имя есть, и человек ищет их глазами, а
            // не читает путь.
            let shown = if self.friendly {
                sysconf::winpath::label(&slot.path).unwrap_or(slot.path.as_str())
            } else {
                slot.path.as_str()
            };
            paint::text_clipped(
                inner,
                s,
                Role::Mono,
                slot.rect.x + pad as i32,
                paint::baseline(inner, Role::Mono, slot.rect),
                slot.rect.w.saturating_sub(pad * 2),
                shown,
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
                let meta = format!("{:04o} {}:{}", row.mode & 0o7777, row.uid, row.gid);
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
                self.shown_name(row),
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
        } else if self.dropped != 0 {
            format!(
                "{} из {} объектов    показаны не все",
                self.rows.len(),
                self.rows.len() + self.dropped
            )
        } else {
            format!(
                "{} объектов    Enter — открыть    Backspace — вверх    R — обновить                     V — {}",
                self.rows.len(),
                if self.friendly { "настоящие имена" } else { "знакомый вид" }
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

// ---------------------------------------------------------------------------
// Клавиши, у которых символ есть
// ---------------------------------------------------------------------------

/// Enter. Договор отдаёт символ раньше имени, и у этой клавиши он есть.
const ENTER: u32 = '\n' as u32;
/// Backspace — `0x08`, его собственный код в ASCII.
const BACKSPACE: u32 = 0x08;
/// Escape — `0x1B`, тоже собственный.
const ESCAPE: u32 = 0x1B;
/// Обновить список. `r` — потому что F-ряд договор программам не отдаёт.
const REFRESH: u32 = 'r' as u32;

/// Переключить знакомый вид на настоящие имена и обратно.
const VIEW: u32 = 'v' as u32;
/// Закрыть окно.
const QUIT: u32 = 'q' as u32;

/// На сколько строк прокручивает просмотр страница.
const PREVIEW_PAGE: usize = 20;

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

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
/// Список короткий и составлен из того, что в системе точно есть: корень, дом,
/// стол и два системных каталога. Закладок человек пока не заводит — заводить
/// их некуда, файла настроек у стола нет.
fn places(home: &str) -> [(Option<&'static str>, String); 5] {
    [
        (Some("МЕСТА"), String::from("/")),
        (None, home.to_string()),
        (None, format!("{home}/Desktop")),
        (Some("СИСТЕМА"), String::from("/bin")),
        (None, String::from("/etc")),
    ]
}

/// Разложить быстрый доступ по колонке.
fn place_slots(ctx: Ctx, side: Rect, home: &str) -> Vec<PlaceSlot> {
    let padx = ctx.px(16);
    let row_h = ctx.px(32);
    let gap = ctx.px(2);
    let caps_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(12);
    let mut y = side.y + ctx.px(10) as i32;
    let mut out = Vec::new();

    for (head, path) in places(home) {
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

// ---------------------------------------------------------------------------
// Файловая система
// ---------------------------------------------------------------------------

/// Собрать путь к записи внутри каталога.
fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Каталог, в котором лежит этот путь.
fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(index) => path[..index].to_string(),
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

/// Прочитать каталог. Возвращает строки и сколько записей не поместилось.
///
/// Ошибка — текстом, а не кодом: показывать её человеку всё равно строкой, а
/// перевод кода в слова в одном месте лучше, чем в трёх.
fn list_dir(path: &str) -> Result<(Vec<Row>, usize), String> {
    let fd = open(path);
    if fd < 0 {
        return Err(format!("cannot open {path}: error {fd}"));
    }
    let program = path == "/bin" || path.ends_with("/bin");
    let mut rows = Vec::new();
    let mut dropped = 0usize;
    let mut entry = Dirent::default();

    loop {
        let step = readdir_raw(fd, &mut entry);
        if step == 0 {
            break;
        }
        if step < 0 {
            close(fd);
            return Err(format!("cannot read {path}: error {step}"));
        }
        // Имя пришло из-за границы доверия: длина — поле структуры, а байты —
        // содержимое носителя. И то и другое проверяется, а не берётся на веру:
        // длина за пределом массива увела бы срез в чужую память, а не-UTF-8
        // прошёл бы в отрисовку и вышел мусором на экране.
        let len = (entry.name_len as usize).min(entry.name.len());
        let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
            continue;
        };
        // «.» и «..» приходят от ext2 как настоящие записи. Своя навигация уже
        // есть (Backspace), а две строки, ведущие «сюда же» и «наверх», в
        // списке только мешают.
        if name == "." || name == ".." {
            continue;
        }
        if rows.len() >= MAX_ROWS {
            dropped += 1;
            continue;
        }
        rows.push(Row {
            name: name.to_string(),
            directory: entry.kind == KIND_DIRECTORY,
            program,
            mode: entry.mode,
            uid: entry.uid,
            gid: entry.gid,
            size: entry.size,
        });
    }

    close(fd);
    Ok((rows, dropped))
}

/// Прочитать файл для просмотра.
fn read_preview(name: &str, path: &str) -> Preview {
    let mut lines = Vec::new();
    let mut note = String::new();

    let fd = open(path);
    if fd < 0 {
        return Preview {
            name: name.to_string(),
            lines,
            note: format!("cannot read: error {fd}"),
            scroll: 0,
        };
    }

    // Полный размер спрашивается отдельно: прочитано будет не больше предела, а
    // сказать «показано не всё» можно только зная, сколько всего.
    let mut info = Stat::default();
    let total = if fstat_ok(fd, &mut info) { info.size } else { 0 };

    let mut buffer = Vec::new();
    // `try_reserve` вместо `vec![]`: отказ аллокатора обязан вернуться ошибкой,
    // а не уронить программу. Восемь килобайт есть почти всегда — «почти» здесь
    // и означает, что проверка нужна.
    if buffer.try_reserve_exact(PREVIEW_LIMIT).is_err() {
        close(fd);
        return Preview {
            name: name.to_string(),
            lines,
            note: String::from("not enough memory to preview this file"),
            scroll: 0,
        };
    }
    buffer.resize(PREVIEW_LIMIT, 0u8);

    let read_bytes = read(fd, &mut buffer);
    close(fd);
    if read_bytes < 0 {
        return Preview {
            name: name.to_string(),
            lines,
            note: format!("cannot read: error {read_bytes}"),
            scroll: 0,
        };
    }
    let got = (read_bytes as usize).min(buffer.len());

    match core::str::from_utf8(&buffer[..got]) {
        Ok(text) => {
            for line in text.lines().take(PREVIEW_LINES) {
                // Табуляции и управляющие байты испортили бы разметку строки:
                // рисование текста не знает про них ничего.
                lines.push(line.replace('\t', "    "));
            }
            if total > got as u64 {
                note = format!("... {got} of {total} bytes shown");
            }
        }
        // Двоичный файл не показывается вовсе, а не показывается мусором: из
        // «шрифт нарисовал непечатное» никто не сделает вывода, что файл
        // двоичный.
        Err(_) => note = format!("binary file, {got} bytes"),
    }

    Preview { name: name.to_string(), lines, note, scroll: 0 }
}

/// `fstat`, у которого ответ — «получилось или нет».
fn fstat_ok(fd: i64, out: &mut Stat) -> bool {
    user_progs::fstat(fd, out) >= 0
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно в том виде, в каком их положило
    // ядро, — это и есть единственный допустимый источник по контракту `Args`.
    let args = unsafe { Args::new(argc, argv) };

    let Some(info) = wait_for_graphics() else {
        // Машина без графики — это не сбой: система работает в серийной линии,
        // и показывать каталог просто негде.
        println("files: no graphics on this machine, nothing to show");
        exit(0)
    };

    // Формат точки и тема — первое, что нужно сделать, и сделать до всякой
    // отрисовки. Оба счётчика у программы свои: адресное пространство своё, и
    // заполненные ядром у себя ей не видны. Без формата окно вышло бы сплошь
    // чёрным при совершенно исправной отрисовке, без темы — светлым на тёмном
    // столе; обе ошибки глазами ищут долго.
    mini_ui::use_raw_format(info.pixel_format);
    theme::set_dark(info.flags & SYSINFO_DARK != 0);

    let start = starting_dir(args.get(1));
    let home = if user_progs::uid() == 0 { "/root" } else { "/home" };

    let (width, height) = window_size(&info);
    let scale = theme::geometry_scale(info.screen_w.max(1));

    let Some(mut window) = open_patiently(width, height) else {
        println("files: FAILED the desktop never freed up; no window");
        exit(1)
    };

    let base = window.pixels().as_mut_ptr();
    // SAFETY: ядро отобразило ровно `width * height` точек по этому адресу и
    // держит их, пока живо окно. Второй ссылки на них нет — `window` больше
    // пикселей никому не отдаёт.
    let Some(mut surface) = (unsafe { Surface::from_raw(base, width, height) }) else {
        println("files: FAILED the surface the kernel gave makes no sense");
        exit(1)
    };

    let area = Rect::new(0, 0, width, height);
    let ctx = Ctx::scaled(scale);
    let mut view = Files::new(start, home.to_string());

    println(&format!("files: window '{TITLE}' opened, {width}x{height}"));
    // Что именно прочитано — в журнал, по разу на каждый каталог. Нарисованное
    // на экране снаружи не проверить, а строка проверяется: она и отличает
    // «список показан» от «окно нарисовано пустым».
    view.report();

    let mut dark = info.flags & SYSINFO_DARK != 0;
    let mut next_theme = monotonic_ms() + THEME_PERIOD_MS;
    let mut dirty = true;
    let reason;

    'live: loop {
        while let Some(event) = window.next_event() {
            match event.kind {
                // Просьба закрыться — крестиком или Ctrl+W. Соглашаемся сразу:
                // несохранённого у менеджера нет.
                WIN_CLOSE => {
                    reason = "request";
                    break 'live;
                }
                WIN_KEY if event.code == QUIT && view.preview.is_none() => {
                    reason = "'q'";
                    break 'live;
                }
                WIN_KEY => {
                    if view.key(event.code) {
                        view.report_if_moved();
                        dirty = true;
                    }
                }
                WIN_POINTER => {
                    if view.click(area, ctx, event.x, event.y) {
                        view.report_if_moved();
                        dirty = true;
                    }
                }
                _ => {}
            }
        }

        let now = monotonic_ms();
        if now >= next_theme {
            next_theme = now + THEME_PERIOD_MS;
            if let Some(fresh) = sysinfo() {
                let fresh_dark = fresh.flags & SYSINFO_DARK != 0;
                if fresh_dark != dark {
                    dark = fresh_dark;
                    theme::set_dark(dark);
                    println(if dark {
                        "files: repainted for the dark theme"
                    } else {
                        "files: repainted for the light theme"
                    });
                    dirty = true;
                }
            }
        }

        if dirty {
            // Контекст пересобирается на каждый кадр, а не хранится: он держит
            // палитру и сведённую подложку, а обе меняются вместе с темой.
            view.draw(&mut surface, area, Ctx::scaled(scale));
            // Отказ здесь — **не** сбой, и выходить из-за него нельзя. Занятый
            // стол отвечает `ERR_AGAIN`, а пропущенный кадр ничего не стоит:
            // следующий виток нарисует то же самое. Ровно на этом монитор
            // системы падал и поднимался супервизором по кругу.
            if window.commit() >= 0 {
                dirty = false;
            }
        }

        nanosleep(0, POLL_NS);
    }

    println(&format!("files: closing on {reason}"));
    window.close();
    exit(0)
}

impl Files {
    /// Сказать в журнал, что показано сейчас.
    ///
    /// Печатается то, чего снаружи не видно иначе: путь, число строк и имя
    /// выбранной. Снимок экрана доказательством не считается — это правило
    /// дома, — а по этой строке стенд проверяет и переход по каталогам, и то,
    /// что стрелка действительно двигает выделение.
    fn report(&self) {
        match &self.error {
            Some(text) => println(&format!("files: {} failed: {text}", self.path)),
            None => println(&format!(
                "files: {} has {} entries, selected '{}'",
                self.path,
                self.rows.len(),
                self.selected_name()
            )),
        }
    }

    /// Имя выбранной строки, либо прочерк, если выбирать не из чего.
    fn selected_name(&self) -> &str {
        match self.rows.get(self.selected) {
            Some(row) => &row.name,
            None => "-",
        }
    }

    /// Сказать в журнал после действия, которое могло сменить каталог, строку
    /// или открыть просмотр.
    fn report_if_moved(&self) {
        match self.preview.as_ref() {
            Some(preview) => println(&format!(
                "files: preview '{}' has {} line(s)",
                preview.name,
                preview.lines.len()
            )),
            None => self.report(),
        }
    }
}

/// С какого каталога начать.
///
/// Аргумент бывает и файлом: значок на столе указывает на файл ровно так же,
/// как на папку, и требовать от стола различать их значило бы завести две
/// команды запуска вместо одной.
fn starting_dir(argument: Option<&str>) -> String {
    let Some(path) = argument else {
        return String::from("/");
    };
    if path.is_empty() {
        return String::from("/");
    }
    let mut info = Stat::default();
    if stat(path, &mut info) >= 0 && info.kind == KIND_DIRECTORY {
        return path.to_string();
    }
    // Не каталог или его вовсе нет — показываем то место, где он должен был бы
    // лежать. Пустое окно с сообщением «нет такого пути» человеку бесполезно:
    // он открыл менеджер, чтобы смотреть файлы, а не чтобы читать отказ.
    parent_of(path)
}

/// Какого размера просить окно.
///
/// Две трети экрана — ровно столько занимало это окно, пока его раскладку
/// считало ядро. Считается от экрана, а не задано числом: окно в 850 точек на
/// экране 3840 выглядит маркой на конверте, а на 800×600 не помещается вовсе.
fn window_size(info: &SysInfo) -> (u32, u32) {
    let w = (info.screen_w * 2 / 3).clamp(320, info.screen_w.max(320));
    let h = (info.screen_h * 2 / 3).clamp(240, info.screen_h.max(240));
    (w, h)
}

/// Попросить окно столько раз, сколько нужно.
fn open_patiently(width: u32, height: u32) -> Option<Window> {
    let deadline = monotonic_ms() + OPEN_WAIT_MS;
    loop {
        match Window::open(TITLE, width, height) {
            Ok(window) => return Some(window),
            Err(code) => {
                // Всё, кроме «попробуйте ещё», окончательно: окна такого
                // размера не дадут никогда, сколько ни проси.
                if code != user_progs::ERR_AGAIN {
                    println(&format!("files: FAILED opening the window: {code}"));
                    return None;
                }
            }
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}

/// Дождаться, пока система скажет формат точки, — это и значит «графика есть».
fn wait_for_graphics() -> Option<SysInfo> {
    let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
    loop {
        let info = sysinfo()?;
        if info.pixel_format != 0 && info.screen_w != 0 {
            return Some(info);
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}
