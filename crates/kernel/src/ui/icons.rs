//! Значки рабочего стола: системные и то, что лежит в `~/Desktop`.
//!
//! # Почему значки — не окна и не поверхность
//!
//! Окно держит собственную поверхность в памяти, потому что его содержимое
//! меняется само по себе: терминал печатает, список файлов листается. Значок не
//! меняется вовсе — он рисуется поверх фона теми же примитивами, что и фон, и
//! стоит ровно столько же. Отдельная поверхность на каждый значок означала бы
//! мегабайты памяти под картинку, которую можно нарисовать плиткой и подписью.
//!
//! # Почему подсветка смешивается, а не сводится
//!
//! Значок лежит **прямо на обоях**, и полоса кадра, в которую он рисует, обои
//! уже содержит. Значит цвет под каждой точкой известен по-настоящему, и
//! полупрозрачные токены (`hover1`, `hover2`) можно смешать честно, а не
//! свести заранее к непрозрачному поверх усреднённых обоев. Сведение здесь
//! выдало бы себя сразу: прямоугольник подсветки оказался бы чуть светлее или
//! темнее фона по краям градиента.
//!
//! # Почему открытие по двойному щелчку
//!
//! Потому что одиночный нужен, чтобы значок выбрать, а выбранный значок —
//! единственный способ показать человеку, что система вообще заметила его
//! щелчок. Так это устроено везде, где человек уже видел рабочий стол, и
//! придумывать здесь своё — значит заставлять переучиваться ради ничего.
//!
//! # Почему содержимое каталога перечитывается целиком, а не следится
//!
//! Каталог стола меняют четыре разные дороги: меню стола, файловый менеджер,
//! оболочка и любая программа третьего кольца. Следить за ними всеми означало
//! бы завести в файловой системе оповещение об изменениях — устройство размером
//! с сам рабочий стол ради каталога, в котором десяток записей. Список
//! перечитывается там, где стол и так знает, что что-то произошло, и по пункту
//! «Обновить» — как в любом обозревателе файлов.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::glyphicon::Icon;
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface};

use super::paint::{self, Ctx, Tone};
use super::theme;
use super::window::App;
use crate::vfs::NodeKind;

/// Поле внутри ячейки: слева, справа и сверху.
///
/// Остальная сетка живёт в [`theme`] и только там: по тем же числам стенд
/// наводит указатель на значок, и второй набор констант здесь означал бы, что
/// проверка целится не туда, куда нарисовано.
const CELL_PAD: u32 = 10;

/// Сколько записей каталога стола показывается.
///
/// Предел не косметический: имена приходят с носителя, и каталог с тысячей
/// файлов означал бы тысячу строк в куче и сетку значков поверх всего экрана.
/// Лишнее не пропадает — оно видно в файловом менеджере, куда и ведёт двойной
/// щелчок по значку «Файлы».
const MAX_ENTRIES: usize = 48;

/// Что лежит на столе от системы и в каком порядке — сверху вниз.
///
/// Порядок не алфавитный и не случайный: сначала то, чем человек пользуется,
/// открыв систему впервые («здесь мои файлы»), затем инструменты. Список
/// короткий намеренно — стол, засыпанный значками, ничем не лучше пустого.
const SYSTEM: [App; 4] = [App::Files, App::Terminal, App::Settings, App::About];

/// Что за значок стоит в ячейке.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Системный значок: открывает окно программы.
    App(App),
    /// Каталог в каталоге стола.
    Folder,
    /// Файл там же.
    File,
}

/// Один значок: картинка, подпись и то, что за ней стоит.
pub struct Item {
    pub kind: Kind,
    pub label: String,
    /// Полный путь — только у того, что лежит в каталоге стола.
    ///
    /// У системного значка пути нет вовсе, и это не пропуск: «Параметры» — не
    /// файл, переименовать и удалить его нечем. По отсутствию пути меню и
    /// решает, что предлагать человеку.
    pub path: Option<String>,
}

/// Значки стола: их места и то, какой из них выбран.
pub struct Icons {
    /// Масштаб — тот же, что у всего стола.
    scale: u32,
    /// Системные значки и содержимое каталога стола, в порядке показа.
    items: Vec<Item>,
    /// Выбранный значок — номер в [`Icons::items`].
    selected: Option<usize>,
    /// Сколько значков помещается в столбец.
    ///
    /// Считается от рабочей области, а не задано числом: на 800×600 в столбец
    /// входит меньше ячеек, чем на 1080p, и сетка, посчитанная под один экран,
    /// на другом уехала бы под панель задач.
    rows: u32,
}

impl Icons {
    #[must_use]
    pub fn new(scale: u32) -> Self {
        let mut icons = Self { scale, items: Vec::new(), selected: None, rows: 1 };
        icons.items = system_items();
        icons
    }

    /// Контекст отрисовки.
    ///
    /// Подложка — усреднённые обои: значок лежит на них, и всё, что у него
    /// сводится к непрозрачному (заливка плитки), обязано сводиться поверх них,
    /// а не поверх окна. Берётся среднее, а не цвет под самой ячейкой, — иначе
    /// один и тот же значок менял бы оттенок при переносе сетки.
    fn ctx(&self) -> Ctx {
        let palette = theme::palette();
        Ctx::scaled(self.scale).on(theme::wall_average(palette))
    }

    /// Перечитать цвета под текущую тему.
    ///
    /// Тело пустое, и это не заглушка: значки не держат ни одной поверхности и
    /// ни одного сведённого цвета — палитра спрашивается заново на каждой
    /// отрисовке (см. [`Icons::ctx`]). Метод существует потому, что вызывающему
    /// не положено знать, у кого из слоёв есть что перекрашивать; появись у
    /// значков кеш — перекраска окажется здесь, а стол править не придётся.
    pub fn restyle(&mut self) {}

    /// Задать высоту рабочей области — от неё считается длина столбца.
    pub fn set_area(&mut self, work_bottom: i32) {
        let ctx = self.ctx();
        let usable = (work_bottom - ctx.px(theme::ICON_MARGIN) as i32).max(0) as u32;
        self.rows = (usable / ctx.px(theme::ICON_CELL_H + theme::ICON_GAP).max(1)).max(1);
    }

    /// Перечитать каталог стола.
    ///
    /// Выделение переезжает **по пути**, а не по номеру. Номер после
    /// перечитывания означает уже другую запись — созданная папка встаёт в
    /// середину списка и сдвигает всё, что за ней, — и выделение, оставленное
    /// числом, подсвечивало бы соседа. А «удалить» относилось бы к нему же.
    pub fn reload(&mut self) {
        let keep = self
            .selected
            .and_then(|index| self.items.get(index))
            .and_then(|item| item.path.clone());

        self.items = system_items();
        self.selected = None;

        let base = super::context::desktop_dir();
        let Some(Ok(entries)) = crate::fs::list(&base) else {
            // Каталога стола может не быть вовсе — его заводят при первой
            // надобности. Это не ошибка и говорить о ней нечего: стол просто
            // показывает системные значки, как до появления файлов.
            return;
        };

        let mut rows: Vec<(bool, String)> = entries
            .into_iter()
            .filter(|entry| entry.name != "." && entry.name != "..")
            .map(|entry| (entry.kind == NodeKind::Directory, entry.name))
            .collect();
        // Каталоги наверх, дальше по имени: порядок записей в ext2 — это
        // порядок вставки, то есть для человека случайный.
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        for (directory, name) in rows.into_iter().take(MAX_ENTRIES) {
            let path = if base.ends_with('/') {
                alloc::format!("{base}{name}")
            } else {
                alloc::format!("{base}/{name}")
            };
            self.items.push(Item {
                kind: if directory { Kind::Folder } else { Kind::File },
                label: name,
                path: Some(path),
            });
        }

        if let Some(path) = keep {
            self.selected = self.index_of_path(&path);
        }
    }

    /// Номер значка с этим путём.
    #[must_use]
    pub fn index_of_path(&self, path: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.path.as_deref() == Some(path))
    }

    /// Сколько всего значков и сколько из них пришло из каталога стола.
    #[must_use]
    pub fn counts(&self) -> (usize, usize) {
        let entries = self.items.iter().filter(|item| item.path.is_some()).count();
        (self.items.len(), entries)
    }

    /// Прямоугольник ячейки значка с номером `index`.
    ///
    /// Сетка заполняется **по столбцам**: значки идут сверху вниз, дойдя до
    /// панели задач — переходят в следующий столбец. Так это устроено на всех
    /// рабочих столах, и причина у всех одна — вниз экрана меньше, чем вправо,
    /// и столбец кончается предсказуемо.
    fn cell(&self, index: usize) -> Rect {
        let ctx = self.ctx();
        let margin = ctx.px(theme::ICON_MARGIN) as i32;
        let step_x = ctx.px(theme::ICON_CELL_W + theme::ICON_GAP);
        let step_y = ctx.px(theme::ICON_CELL_H + theme::ICON_GAP);
        let column = index as u32 / self.rows.max(1);
        let row = index as u32 % self.rows.max(1);
        Rect::new(
            margin + (column * step_x) as i32,
            margin + (row * step_y) as i32,
            ctx.px(theme::ICON_CELL_W),
            ctx.px(theme::ICON_CELL_H),
        )
    }

    /// Все ячейки вместе — область, которую занимает сетка значков.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        let ctx = self.ctx();
        let columns = (self.items.len() as u32).div_ceil(self.rows.max(1)).max(1);
        Rect::new(
            ctx.px(theme::ICON_MARGIN) as i32,
            ctx.px(theme::ICON_MARGIN) as i32,
            columns * ctx.px(theme::ICON_CELL_W + theme::ICON_GAP),
            self.rows * ctx.px(theme::ICON_CELL_H + theme::ICON_GAP),
        )
    }

    /// Какой значок лежит под точкой экрана.
    #[must_use]
    pub fn at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.items.len()).find(|index| self.cell(*index).contains(x, y))
    }

    /// Значок с этим номером.
    #[must_use]
    pub fn item(&self, index: usize) -> Option<&Item> {
        self.items.get(index)
    }

    /// Что выбрано сейчас.
    #[must_use]
    pub const fn selection(&self) -> Option<usize> {
        self.selected
    }

    /// Выбрать значок. Возвращает область, которую надо перерисовать.
    pub fn select(&mut self, index: Option<usize>) -> Rect {
        let index = index.filter(|index| *index < self.items.len());
        if self.selected == index {
            return Rect::EMPTY;
        }
        let previous = self.selected;
        self.selected = index;
        // Перерисовать надо и то, что перестало быть выбранным: подсветка
        // снимается ровно так же, как ставится.
        let mut damage = Rect::EMPTY;
        for slot in [previous, index].into_iter().flatten() {
            damage = damage.union(&self.cell(slot));
        }
        damage
    }

    /// Нарисовать значки, попадающие в полосу кадра.
    ///
    /// Значок — часть стола: он ложится в буфер между фоном и окнами, и окно,
    /// наехавшее на него, перекрывает его само. Обрезать себя значку больше не
    /// нужно — этим занимается порядок слоёв. Пока слои шли прямо на экран,
    /// обрезка была обязательной: нарисованный целиком ради задетого края,
    /// значок ложился поверх закрывающего его окна.
    pub fn draw(&self, back: &mut Surface, band: Rect, dy: i32) {
        for index in 0..self.items.len() {
            let cell = self.cell(index);
            if cell.intersect(&band).is_empty() {
                continue;
            }
            let item = &self.items[index];
            self.draw_one(back, cell, dy, item, self.selected == Some(index));
        }
    }

    /// Нарисовать один значок в полосе кадра.
    fn draw_one(&self, back: &mut Surface, cell: Rect, dy: i32, item: &Item, selected: bool) {
        // Всё считается в координатах экрана, а в полосу переводится одним
        // сдвигом здесь: две системы координат внутри рисующего кода — это две
        // возможности перепутать, и обе выглядят как значок, уехавший на
        // полполосы.
        let cell = cell.translate(0, dy);
        let ctx = self.ctx();
        let p = ctx.palette;

        if selected {
            // Подсветка — по всей ячейке, а не по плитке: человек целится в
            // значок вместе с подписью, и выделять надо то, во что он целился.
            let r = ctx.px(theme::R_CARD);
            draw::rounded(back, cell, r, p.hover2.color, p.hover2.alpha);
            draw::rounded_stroke(back, cell, r, p.accline.color, p.accline.alpha);
        }

        let side = ctx.px(theme::ICON_TILE);
        let tile = Rect::new(
            cell.x + (cell.w as i32 - side as i32) / 2,
            cell.y + ctx.px(CELL_PAD) as i32,
            side,
            side,
        );
        let (icon, tone, filled) = art(&item.kind);
        paint::icon_tile(ctx, back, tile, icon, tone, filled);

        // Подпись — до двух строк. Одна строка обрезала бы «This computer» на
        // «This comp…», а три сделали бы сетку неровной: высота ячейки задана
        // числом, и третья строка выехала бы на соседнюю ячейку.
        let pad = ctx.px(CELL_PAD);
        let room = cell.w.saturating_sub(pad * 2);
        let ink = if selected { p.ink } else { p.ink2 };
        let step = u32::from(ctx.face(Role::Label).line);
        let mut y = tile.bottom() + ctx.px(theme::ICON_LABEL_GAP) as i32;
        let (first, second) = wrap(ctx, &item.label, room);
        centered(ctx, back, cell, y, room, &first, ink);
        if let Some(second) = second {
            y += step as i32;
            centered(ctx, back, cell, y, room, &second, ink);
        }
    }
}

/// Плитка значка: что на ней нарисовано и залита ли она цветом.
///
/// Картинка и оттенок берутся у самой программы: значок в заголовке окна, в
/// меню запуска и на столе обязан быть одним и тем же, а три списка соответствий
/// разошлись бы в первый же день.
///
/// Залиты только два значка из всех. Заливка — это «сюда смотреть в первую
/// очередь», и если залить всё, она перестаёт что-либо значить.
fn art(kind: &Kind) -> (Icon, Tone, bool) {
    match kind {
        Kind::App(app) => (
            app.icon(),
            app.tone(),
            matches!(app, App::Files | App::Terminal),
        ),
        Kind::Folder => (Icon::Folder, Tone::Muted, false),
        Kind::File => (Icon::File, Tone::Muted, false),
    }
}

/// Написать строку подписи по центру ячейки.
fn centered(
    ctx: Ctx,
    s: &mut Surface,
    cell: Rect,
    y: i32,
    room: u32,
    line: &str,
    ink: mini_ui::Color,
) {
    let width = ctx.face(Role::Label).width(line).min(room);
    let x = cell.x + (cell.w as i32 - width as i32) / 2;
    paint::text_clipped(ctx, s, Role::Label, x, y, room, line, ink);
}

/// Разложить подпись на одну-две строки.
///
/// Перенос ищется по границе слова: «New folder 2», разорванное на «New fold» и
/// «er 2», читается как испорченное имя, а не как перенос. Слова, которое не
/// помещается целиком, это не спасает — тогда рвём где придётся, а хвост второй
/// строки обрежет многоточие.
fn wrap(ctx: Ctx, label: &str, room: u32) -> (String, Option<String>) {
    let face = ctx.face(Role::Label);
    if face.width(label) <= room {
        return (label.to_string(), None);
    }
    let fits = face.fits(label, room);
    let head: String = label.chars().take(fits).collect();
    match head.rfind(' ') {
        Some(at) if at > 0 => {
            let first: String = head[..at].to_string();
            let rest: String = label.chars().skip(first.chars().count() + 1).collect();
            (first, Some(rest))
        }
        _ => {
            let rest: String = label.chars().skip(fits).collect();
            (head, Some(rest))
        }
    }
}

/// Системные значки — те, что есть на столе всегда.
fn system_items() -> Vec<Item> {
    SYSTEM
        .iter()
        .map(|app| Item {
            kind: Kind::App(*app),
            label: app.caption().to_string(),
            path: None,
        })
        .collect()
}
