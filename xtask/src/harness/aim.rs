//! Наведение мыши: куда стенд везёт указатель.
//!
//! # Почему цели не заданы числами
//!
//! Потому что экран разный. Прошивка x86-64 (OVMF) отдаёт 1280×800, а `ramfb`
//! на машине `virt` — 800×600, и от размера экрана зависит всё: масштаб шрифта,
//! высота панели, размеры и положение окон. Сценарий, написанный в точках,
//! работал бы ровно на одной архитектуре, а на второй молча щёлкал бы по фону.
//!
//! Поэтому цель описывается смыслом («заголовок окна `System`»), а координаты
//! берутся из **журнала самого гостя**: ядро печатает размер экрана и
//! прямоугольник каждого окна именно для этого. Так проверка остаётся честной —
//! стенд целится туда, где система сама сказала, что у неё что-то есть.
//!
//! # Почему стенд считает положение указателя сам
//!
//! Мышь относительная: абсолютных координат у неё нет, и «поставить курсор в
//! точку» невозможно — можно только сдвинуть его на приращение. Значит, стенд
//! обязан знать, где курсор сейчас. Начальное положение — середина экрана (так
//! его ставит ядро), дальше оно складывается из отправленных приращений.

use anyhow::{Context, Result, bail};

/// Прямоугольник в координатах экрана гостя.
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// Куда навести указатель.
#[derive(Clone, Copy, Debug)]
pub enum Aim {
    /// Левый верхний угол экрана. Заодно проверяет ограничение по краю:
    /// доехать туда можно только упёршись в него.
    Corner,
    /// Кнопка меню — левый нижний угол, где панель начинается всегда.
    MenuButton,
    /// Полоса заголовка окна, мимо кнопки закрытия.
    Title(&'static str),
    /// Кнопка закрытия окна.
    Close(&'static str),
    /// Кнопка «свернуть» — третья справа в полосе заголовка.
    Minimize(&'static str),
    /// Кнопка «развернуть» — вторая справа.
    Maximize(&'static str),
    /// Уголок изменения размера — правый нижний угол окна.
    Grip(&'static str),
    /// Значок рабочего стола по его номеру сверху, считая с нуля.
    Icon(usize),
    /// Пустое место стола — правее значков и выше панели.
    Empty,
    /// Пустое место ниже [`Aim::Empty`]: туда не попадает меню, открытое в нём.
    EmptyBelow,
    /// Пункт меню стола по номеру сверху; меню открывается в [`Aim::Empty`].
    ContextItem(usize),
    /// Середина окна.
    Middle(&'static str),
}

/// Размер экрана гостя из строки `desktop     : 1280x800, ...`.
pub fn screen(log: &str) -> Result<(i32, i32)> {
    let line = log
        .lines()
        .find(|line| line.contains("desktop     : ") && line.contains("glyph scale"))
        .context("в журнале нет строки с размером экрана")?;
    let rest = line
        .split("desktop     : ")
        .nth(1)
        .context("строка размера экрана разобрана неверно")?;
    let dims = rest.split(',').next().unwrap_or_default();
    let (width, height) = dims
        .trim()
        .split_once('x')
        .context("размер экрана не вида ШИРИНАxВЫСОТА")?;
    Ok((width.trim().parse()?, height.trim().parse()?))
}

/// Прямоугольник окна из строк журнала.
///
/// Берётся **последнее** упоминание: окно могли открыть заново или перетащить, и
/// первое упоминание описывало бы то, чего на экране уже нет. Строка о
/// перетаскивании несёт только новое начало — размер у окна не меняется.
pub fn window(log: &str, title: &str) -> Result<Rect> {
    let placed = format!("'{title}' at ");
    let moved = format!("moved '{title}' to ");
    let mut rect: Option<Rect> = None;

    for line in log.lines() {
        if let Some(rest) = line.split(&placed).nth(1) {
            if let Some(found) = parse_placement(rest) {
                rect = Some(found);
            }
            continue;
        }
        if let Some(rest) = line.split(&moved).nth(1) {
            if let (Some(origin), Some(current)) = (parse_origin(rest), rect) {
                rect = Some(Rect { x: origin.0, y: origin.1, ..current });
            }
        }
    }

    rect.with_context(|| format!("в журнале нет окна '{title}'"))
}

/// `53,48 800x576 (focused)` → прямоугольник.
fn parse_placement(rest: &str) -> Option<Rect> {
    let mut parts = rest.split_whitespace();
    let (x, y) = parse_origin(parts.next()?)?;
    let (w, h) = parts.next()?.split_once('x')?;
    Some(Rect { x, y, w: w.parse().ok()?, h: h.parse().ok()? })
}

/// `53,48` → пара чисел.
fn parse_origin(text: &str) -> Option<(i32, i32)> {
    let text = text.split_whitespace().next()?;
    let (x, y) = text.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Куда именно везти указатель.
///
/// Числа-отступы подобраны так, чтобы попадать при **любом** масштабе шрифта.
/// Полоса заголовка не ниже шестнадцати точек даже при масштабе 1, а кнопка
/// закрытия — квадрат той же высоты у правого края; поэтому шесть точек от
/// верхнего края и восемь от правого попадают в них всегда.
pub fn resolve(aim: Aim, log: &str) -> Result<(i32, i32)> {
    let (width, height) = screen(log)?;
    let point = match aim {
        Aim::Corner => (0, 0),
        // Не в самый угол: панель начинается от нуля, но щелчок ровно в
        // последнюю точку экрана слишком похож на промах, чтобы им что-то
        // проверять.
        Aim::MenuButton => (4, height - 4),
        Aim::Title(title) => {
            let rect = window(log, title)?;
            (rect.x + rect.w / 3, rect.y + 6)
        }
        Aim::Close(title) => {
            let rect = window(log, title)?;
            (rect.x + rect.w - 8, rect.y + 8)
        }
        // Кнопки одинаковые и квадратные со стороной в высоту полосы
        // заголовка; она равна высоте глифа на масштаб плюс отступы, и на всех
        // экранах, где живёт стол, это 24 точки. Считать её из журнала было бы
        // честнее, но стол её не печатает, а промах в кнопку соседа виден сразу
        // — по строке, которую гость напечатает в ответ.
        Aim::Minimize(title) => {
            let rect = window(log, title)?;
            (rect.x + rect.w - 8 - 48, rect.y + 8)
        }
        Aim::Maximize(title) => {
            let rect = window(log, title)?;
            (rect.x + rect.w - 8 - 24, rect.y + 8)
        }
        Aim::Grip(title) => {
            let rect = window(log, title)?;
            (rect.x + rect.w - 6, rect.y + rect.h - 6)
        }
        // Значки лежат столбиком слева: отступ 12 точек на масштаб, ячейка
        // 96×64 на масштаб. Масштаб равен двум на всех экранах шире 1024 точек,
        // а уже них стол не поднимается вовсе.
        // Пустое место: середина экрана по ширине и треть по высоте — там нет
        // ни значков (они слева), ни панели (она внизу). Окна к этому моменту
        // сценарий обязан убрать сам.
        Aim::Empty => (width / 2, height / 3),
        Aim::EmptyBelow => (width / 2, height * 3 / 4),
        // Меню открывается от точки щелчка вниз и вправо; строка — высота глифа
        // на масштаб плюс отступы, то есть 24 точки при масштабе 2.
        Aim::ContextItem(index) => {
            let row = 24;
            (width / 2 + 40, height / 3 + 6 + index as i32 * row + row / 2)
        }
        Aim::Icon(index) => {
            let scale = 2;
            let cell_h = 64 * scale;
            let margin = 12 * scale;
            (margin + 96 * scale / 2, margin + index as i32 * cell_h + 32)
        }
        Aim::Middle(title) => {
            let rect = window(log, title)?;
            (rect.x + rect.w / 2, rect.y + rect.h / 2)
        }
    };

    if point.0 < 0 || point.1 < 0 || point.0 >= width || point.1 >= height {
        bail!("цель {aim:?} оказалась за экраном {width}x{height}: {point:?}");
    }
    Ok(point)
}
