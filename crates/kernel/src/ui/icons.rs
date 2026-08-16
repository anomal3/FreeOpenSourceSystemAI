//! Значки рабочего стола: системные и то, что лежит в `~/Desktop`.
//!
//! # Почему значки — не окна и не поверхность
//!
//! Окно держит собственную поверхность в памяти, потому что его содержимое
//! меняется само по себе: терминал печатает, список файлов листается. Значок не
//! меняется вовсе — он рисуется поверх фона теми же примитивами, что и фон, и
//! стоит ровно столько же. Отдельная поверхность на каждый значок означала бы
//! мегабайты памяти под картинку, которую можно нарисовать шестью заливками.
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
//! «Refresh» — как в любом обозревателе файлов.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::text::{self};
use mini_ui::{Color, Rect, Screen};

use super::theme;
use super::window::App;
use crate::vfs::NodeKind;

/// Сторона картинки значка при масштабе 1.
const ART: u32 = 32;
/// Ширина ячейки значка вместе с подписью, при масштабе 1.
///
/// Не 96, как было до появления файлов: в ячейку такой ширины помещается
/// двенадцать знаков подписи, и «This computer» обрывалось на «This comput>».
/// Имена файлов бывают любые, но четырнадцать знаков покрывают и системные
/// подписи, и то, что создаёт меню стола («New file 2.txt»).
const CELL_W: u32 = 112;
/// Высота ячейки значка, при масштабе 1.
const CELL_H: u32 = 64;
/// Отступ сетки значков от края экрана, при масштабе 1.
const MARGIN: u32 = 12;

/// Сколько записей каталога стола показывается.
///
/// Предел не косметический: имена приходят с носителя, и каталог с тысячей
/// файлов означал бы тысячу строк в куче и сетку значков поверх всего экрана.
/// Лишнее не пропадает — оно видно в файловом менеджере, куда и ведёт двойной
/// щелчок по значку «This computer».
const MAX_ENTRIES: usize = 48;

/// Что лежит на столе от системы и в каком порядке — сверху вниз.
///
/// Порядок не алфавитный и не случайный: сначала то, чем человек пользуется,
/// открыв систему впервые («здесь мои файлы»), затем инструменты. Список
/// короткий намеренно — стол, засыпанный значками, ничем не лучше пустого.
const SYSTEM: [(App, &str); 4] = [
    (App::Files, "This computer"),
    (App::Terminal, "Terminal"),
    (App::Settings, "Settings"),
    (App::About, "About"),
];

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
    /// У системного значка пути нет вовсе, и это не пропуск: «Settings» — не
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
    /// входит восемь ячеек, на 1080p — шестнадцать, и сетка, посчитанная под
    /// один экран, на другом уехала бы под панель задач.
    rows: u32,
}

impl Icons {
    #[must_use]
    pub fn new(scale: u32) -> Self {
        let mut icons = Self { scale, items: Vec::new(), selected: None, rows: 1 };
        icons.items = system_items();
        icons
    }

    /// Задать высоту рабочей области — от неё считается длина столбца.
    pub fn set_area(&mut self, work_bottom: i32) {
        let usable = (work_bottom - (MARGIN * self.scale) as i32).max(0) as u32;
        self.rows = (usable / (CELL_H * self.scale)).max(1);
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
        let scale = self.scale;
        let column = index as u32 / self.rows;
        let row = index as u32 % self.rows;
        Rect::new(
            (MARGIN * scale + column * CELL_W * scale) as i32,
            (MARGIN * scale + row * CELL_H * scale) as i32,
            CELL_W * scale,
            CELL_H * scale,
        )
    }

    /// Все ячейки вместе — область, которую занимает сетка значков.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        let columns = (self.items.len() as u32).div_ceil(self.rows).max(1);
        Rect::new(
            (MARGIN * self.scale) as i32,
            (MARGIN * self.scale) as i32,
            columns * CELL_W * self.scale,
            self.rows * CELL_H * self.scale,
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

    /// Нарисовать значки, попадающие в `rect`.
    ///
    /// Рисует прямо на экран, между фоном и окнами: значок — часть стола, и
    /// окно, наехавшее на него, обязано его закрывать.
    pub fn draw(&self, screen: &Screen, rect: Rect) {
        for index in 0..self.items.len() {
            let cell = self.cell(index);
            if cell.intersect(&rect).is_empty() {
                continue;
            }
            let item = &self.items[index];
            self.draw_one(screen, cell, rect, item, self.selected == Some(index));
        }
    }

    /// Нарисовать один значок, не выходя за `clip`.
    ///
    /// Обрезка обязательна: значок рисуется поверх фона, а собирается экран
    /// прямоугольниками изменений. Нарисованный целиком ради задетого края, он
    /// лёг бы поверх окна, которое его закрывает.
    fn draw_one(&self, screen: &Screen, cell: Rect, clip: Rect, item: &Item, selected: bool) {
        let paint = |rect: Rect, color: Color| {
            let visible = rect.intersect(&clip);
            if !visible.is_empty() {
                screen.fill(visible, color);
            }
        };
        let scale = self.scale;
        let art = ART * scale;
        let art_x = cell.x + (cell.w.saturating_sub(art) / 2) as i32;
        let art_y = cell.y + (4 * scale) as i32;

        if selected {
            // Подсветка — по всей ячейке, а не по картинке: человек целится в
            // значок вместе с подписью, и выделять надо то, во что он целился.
            paint(cell, theme::SELECT_BG);
        }

        match &item.kind {
            // Системный блок с экраном: прямоугольник, светлое «стекло» и
            // подставка. Узнаваемость здесь важнее правдоподобия — значок
            // размером в тридцать две точки не бывает похож на настоящую вещь.
            Kind::App(App::Files) => {
                paint(Rect::new(art_x, art_y, art, art * 3 / 4), theme::FRAME);
                paint(
                    Rect::new(
                        art_x + 3 * scale as i32,
                        art_y + 3 * scale as i32,
                        art - 6 * scale,
                        art * 3 / 4 - 6 * scale,
                    ),
                    theme::ACCENT,
                );
                paint(
                    Rect::new(
                        art_x + (art / 3) as i32,
                        art_y + (art * 3 / 4) as i32,
                        art / 3,
                        4 * scale,
                    ),
                    theme::FRAME,
                );
                paint(
                    Rect::new(
                        art_x + (art / 6) as i32,
                        art_y + (art * 3 / 4 + 4 * scale) as i32,
                        art * 2 / 3,
                        3 * scale,
                    ),
                    theme::FRAME,
                );
            }
            // Окно терминала с приглашением: рамка и две чёрточки.
            Kind::App(App::Terminal) => {
                paint(Rect::new(art_x, art_y, art, art), theme::FRAME);
                paint(
                    Rect::new(
                        art_x + 2 * scale as i32,
                        art_y + (6 * scale) as i32,
                        art - 4 * scale,
                        art - 8 * scale,
                    ),
                    Color::rgb(0x06, 0x10, 0x18),
                );
                text::draw_text_on_screen(
                    screen,
                    (art_x + 5 * scale as i32) as u32,
                    (art_y + 11 * scale as i32) as u32,
                    ">_",
                    scale,
                    theme::DIRECTORY,
                    clip,
                );
            }
            // Шестерёнка: круг из четырёх зубцов вокруг квадрата. На такой
            // сетке настоящая шестерёнка превращается в кашу, а этот силуэт
            // читается.
            Kind::App(App::Settings) => {
                let centre = (art_x + (art / 2) as i32, art_y + (art / 2) as i32);
                let arm = (art / 3) as i32;
                let thick = 6 * scale;
                paint(
                    Rect::new(
                        centre.0 - (thick / 2) as i32,
                        centre.1 - arm,
                        thick,
                        arm as u32 * 2,
                    ),
                    theme::DIM,
                );
                paint(
                    Rect::new(
                        centre.0 - arm,
                        centre.1 - (thick / 2) as i32,
                        arm as u32 * 2,
                        thick,
                    ),
                    theme::DIM,
                );
                paint(
                    Rect::new(
                        centre.0 - (art / 4) as i32,
                        centre.1 - (art / 4) as i32,
                        art / 2,
                        art / 2,
                    ),
                    theme::ACCENT,
                );
                paint(
                    Rect::new(
                        centre.0 - (art / 8) as i32,
                        centre.1 - (art / 8) as i32,
                        art / 4,
                        art / 4,
                    ),
                    theme::DESKTOP_TOP,
                );
            }
            // Папка: корешок с язычком сверху слева. Язычок — единственное, чем
            // папка отличается от любого другого прямоугольника, поэтому он
            // рисуется даже при масштабе 1, где на него приходится три точки.
            Kind::Folder => {
                let body_y = art_y + (art / 5) as i32;
                paint(
                    Rect::new(art_x, art_y + (art / 8) as i32, art * 2 / 5, art / 8),
                    theme::DIRECTORY,
                );
                paint(
                    Rect::new(art_x, body_y, art, art * 5 / 8),
                    theme::DIRECTORY,
                );
                // Тёмная полоска внутри — «створка»: без неё папка на тёмном
                // фоне читается как сплошная зелёная плитка.
                paint(
                    Rect::new(
                        art_x + 2 * scale as i32,
                        body_y + 3 * scale as i32,
                        art - 4 * scale,
                        2 * scale,
                    ),
                    theme::DESKTOP_TOP,
                );
            }
            // Лист бумаги с загнутым уголком и тремя строчками текста. Уголок —
            // то, по чему файл отличают от папки на любом рабочем столе.
            Kind::File => {
                let w = art * 3 / 4;
                let x = art_x + (art - w) as i32 / 2;
                paint(Rect::new(x, art_y, w, art), theme::TEXT);
                // Уголок: ступенька из треугольника, сложенного полосками.
                let fold = w / 3;
                for step in 0..fold {
                    paint(
                        Rect::new(
                            x + (w - fold + step) as i32,
                            art_y + step as i32,
                            fold - step,
                            1,
                        ),
                        theme::DESKTOP_TOP,
                    );
                }
                for line in 0..3u32 {
                    paint(
                        Rect::new(
                            x + 3 * scale as i32,
                            art_y + (art / 2 + line * 5 * scale) as i32,
                            w.saturating_sub(6 * scale),
                            2 * scale,
                        ),
                        theme::DIM,
                    );
                }
            }
            // Буква «i» в круге — то, чем «сведения» обозначены везде.
            Kind::App(_) => {
                paint(Rect::new(art_x, art_y, art, art), theme::ACCENT);
                paint(
                    Rect::new(
                        art_x + 3 * scale as i32,
                        art_y + 3 * scale as i32,
                        art - 6 * scale,
                        art - 6 * scale,
                    ),
                    theme::WINDOW_BG,
                );
                text::draw_text_on_screen(
                    screen,
                    (art_x + (art / 2) as i32 - (text::GLYPH_W * scale / 2) as i32) as u32,
                    (art_y + (art / 2) as i32 - (text::GLYPH_H * scale / 2) as i32) as u32,
                    "i",
                    scale,
                    theme::TEXT,
                    clip,
                );
            }
        }

        // Подпись — по центру ячейки. Длинную обрезаем, а не переносим: две
        // строки под значком сделали бы сетку неровной, а имена файлов бывают
        // любой длины и переносить их пришлось бы посреди слова.
        let room = (cell.w / (text::GLYPH_W * scale)) as usize;
        let label = fit(&item.label, room);
        let text_w = text::width_of(&label, scale);
        let text_x = cell.x + ((cell.w.saturating_sub(text_w)) / 2) as i32;
        let text_y = art_y + (ART * scale + 4 * scale) as i32;
        text::draw_text_on_screen(
            screen,
            text_x.max(cell.x) as u32,
            text_y as u32,
            &label,
            scale,
            theme::TEXT,
            clip,
        );
    }
}

/// Системные значки — те, что есть на столе всегда.
fn system_items() -> Vec<Item> {
    SYSTEM
        .iter()
        .map(|(app, label)| Item {
            kind: Kind::App(*app),
            label: (*label).to_string(),
            path: None,
        })
        .collect()
}

/// Обрезать подпись по числу знаков, пометив обрезку.
///
/// Знак `>` на конце говорит, что имя продолжается; многоточия в шрифте 8×8
/// нет, а обрубленное на полбукве имя выглядит как испорченный вывод, а не как
/// «здесь не поместилось».
fn fit(text: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    if text.chars().count() <= room {
        return text.to_string();
    }
    let mut out: String = text.chars().take(room - 1).collect();
    out.push('>');
    out
}
