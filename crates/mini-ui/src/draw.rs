//! Растровые примитивы нового вида: скруглённые прямоугольники, градиенты,
//! однопиксельные обводки, мягкие тени, круги, пилюли и отрезки — всё со
//! сглаженным краем.
//!
//! # Почему знаковое расстояние, а не четверти окружности по Брезенхэму
//!
//! Брезенхэм отвечает на вопрос «попала точка в фигуру или нет» — да или нет,
//! третьего нет. Этого хватает, пока углы прямые: там граница совпадает с
//! сеткой точек. Как только угол скруглён, наклонная граница режет точки
//! пополам, и ответ «да/нет» рисует лестницу — ровно тот вид, из-за которого
//! система выглядела как Windows NT 3.1.
//!
//! Знаковое расстояние (SDF) отвечает на другой вопрос: **насколько далеко**
//! точка от границы. Из расстояния получается доля закрытой площади, а из неё —
//! насыщенность, и лестница исчезает. Важнее другое: одна и та же формула
//! обслуживает заливку, обводку, тень и пилюлю — обводка это `|d|` вместо `d`,
//! тень это спад по `d` наружу, пилюля это радиус в половину высоты. По
//! Брезенхэму каждый из этих случаев пришлось бы писать отдельным алгоритмом со
//! своими краевыми ошибками, а согласовать их между собой до точки — то есть
//! добиться, чтобы обводка легла ровно на край заливки, — практически
//! невозможно.
//!
//! # Почему здесь нет ни одного числа с плавающей точкой
//!
//! Ядро собрано под `aarch64-unknown-none-softfloat` намеренно: обработчик
//! прерывания не сохраняет регистры сопроцессора, и любое живое значение в
//! `v8`–`v15` будет испорчено ближайшим тиком таймера. Отрисовку прерывают
//! постоянно, так что «немножко посчитать во float» здесь означает редкие
//! необъяснимые цветные точки — дефект, который ищут неделю.
//!
//! Поэтому всё считается в неподвижной точке с масштабом 256 ([`ONE`]), а
//! корень берётся своим целочисленным [`isqrt`]. Точности 1/256 точки хватает с
//! запасом: результат всё равно округляется до 256 уровней насыщенности.
//!
//! # Почему это быстро
//!
//! Наивный SDF считал бы корень для каждой точки окна — полмиллиона корней на
//! перерисовку окна 800×600. Здесь корень считается только там, где граница
//! действительно наклонная: строка, целиком лежащая между углами, заливается
//! одним вызовом [`Surface::fill`], а в строках, задевающих углы, серединный
//! пролёт заливается так же и считаются только два столбца шириной в радиус.
//! Заливка того же окна со скруглением 14 обходится восемью сотнями корней —
//! четырьмя квадратами радиуса, — и не зависит от размера окна вовсе.
//!
//! У обводки и тени вырожден другой случай: там пролёт между углами не
//! сплошной, но расстояние вдоль него постоянно (до боковой границы дальше, чем
//! достаёт штрих), и вся полоса заливается одним вызовом с одним покрытием.
//! Остаётся высота, умноженная на ширину угловой зоны.

use crate::{Color, Rect, Surface};

/// Насыщенность в 1/255. 255 — непрозрачно.
pub type Alpha = u8;

/// Масштаб неподвижной точки: одна точка экрана — 256 единиц.
const ONE: i32 = 256;

/// Половина точки. Встречается всюду: центр точки смещён на полшага от её
/// начала, и покрытие считается от середины границы.
const HALF: i32 = 128;

/// Потолок размеров, за которым перевод в неподвижную точку перестал бы
/// помещаться в 32 бита. Экранов такого размера не бывает; ограничение стоит
/// не ради них, а ради мусорного значения, которое иначе устроило бы
/// переполнение в отладочной сборке.
const LIMIT: u32 = 1 << 22;

// --------------------------------------------------------------------------
// Мелкая арифметика
// --------------------------------------------------------------------------

/// Перевести пиксельную координату в неподвижную точку.
///
/// Насыщение вместо переполнения: окно вправе уехать далеко за край экрана, и
/// это законное состояние, а не повод останавливать систему.
#[must_use]
const fn fxi(value: i32) -> i32 {
    value.saturating_mul(ONE)
}

/// То же для размера.
#[must_use]
fn fxu(value: u32) -> i32 {
    // Зажать до приведения: 2^22 * 256 = 2^30 ещё помещается в i32.
    value.min(LIMIT) as i32 * ONE
}

/// Урезать координату до заведомо безопасного диапазона.
///
/// Точка в шестидесяти тысячах точек от экрана и точка в миллиарде рисуют одно
/// и то же — ничего, — но вторая по дороге переполняет скалярное произведение в
/// [`line`]: там координаты перемножаются попарно и ещё раз умножаются на
/// масштаб, то есть в показатель степени входят трижды. Урезаем один раз здесь,
/// чтобы дальше в файле не было ни одной проверки на переполнение.
#[must_use]
fn tame(value: i32) -> i32 {
    value.clamp(-(1 << 16), 1 << 16)
}

/// Умножение двух долей в 1/255. Округление к ближайшему, иначе непрозрачная
/// заливка (255 × 255) давала бы 254 и еле заметно светлела.
#[must_use]
const fn scale(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32 + 127) / 255) as u8
}

/// Доля закрытой площади по знаковому расстоянию: `clamp(0.5 - d, 0, 1)`.
///
/// Линейное приближение вместо честной площади пересечения точки с фигурой.
/// Разница видна только на дуге радиусом в одну точку, а стоит честная формула
/// вчетверо дороже.
#[must_use]
const fn coverage(distance: i32) -> u8 {
    let value = HALF - distance;
    if value <= 0 {
        0
    } else if value >= 255 {
        255
    } else {
        value as u8
    }
}

/// Доля закрытой площади для контура: `clamp(0.5 + w/2 - |d|, 0, 1)`.
#[must_use]
const fn stroke_coverage(distance: i32, half_weight: i32) -> u8 {
    let away = if distance < 0 { -distance } else { distance };
    coverage(away - half_weight)
}

/// Целочисленный квадратный корень.
///
/// Метод Ньютона от заведомо завышенного начального приближения: сходится
/// сверху и монотонно, поэтому условие выхода — «следующее не меньше
/// текущего», без эпсилонов, которых в целых числах не бывает. Начальное
/// приближение берётся из числа значащих бит, иначе на больших значениях
/// первые итерации уходили бы на деление пополам.
#[must_use]
fn isqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }
    let bits = 64 - value.leading_zeros();
    let mut current = 1u64 << bits.div_ceil(2);
    loop {
        let next = (current + value / current) / 2;
        if next >= current {
            return current;
        }
        current = next;
    }
}

/// Годится ли прямоугольник для рисования.
///
/// Проверка на переполнение стоит здесь, а не в вызывающем: [`Rect::right`]
/// складывает начало с размером без оглядки, и прямоугольник, пришедший из
/// испорченного состояния окна, остановил бы отладочную сборку прямо в обрезке.
#[must_use]
fn sane(rect: Rect) -> bool {
    !rect.is_empty()
        && rect.x.checked_add(rect.w as i32).is_some()
        && rect.y.checked_add(rect.h as i32).is_some()
}

/// Прямоугольник, обрезанный по поверхности. Пустой — рисовать нечего.
#[must_use]
fn clip(surface: &Surface, rect: Rect) -> Rect {
    if !sane(rect) {
        return Rect::EMPTY;
    }
    rect.intersect(&surface.bounds())
}

/// Половина толщины штриха в неподвижной точке. Толщина приходит в десятых
/// точки: 14 — это 1.4 точки, штрих значков из макета.
#[must_use]
fn half_weight(weight: u32) -> i32 {
    fxu(weight.min(1 << 12)) / 20
}

// --------------------------------------------------------------------------
// Геометрия скруглённого прямоугольника
// --------------------------------------------------------------------------

/// Скруглённый прямоугольник в неподвижной точке: центр, полуразмеры, радиус.
///
/// Считается один раз на фигуру, а не на точку: в цикле остаются вычитание,
/// модуль и один корень.
#[derive(Clone, Copy)]
struct Round {
    cx: i32,
    cy: i32,
    hx: i32,
    hy: i32,
    r: i32,
}

impl Round {
    /// Радиус зажимается до половины меньшей стороны — и зажимается **в
    /// неподвижной точке**, а не в целых точках.
    ///
    /// Разница видна на нечётной стороне: у пилюли высотой 21 половина — это
    /// 10.5, и округление вниз оставило бы посреди закругления плоский участок
    /// в точку шириной. Больший радиус не бессмыслен, а неопределён: дуги
    /// соседних углов начали бы пересекаться, и фигура вывернулась бы наизнанку.
    /// Зажимаем молча — просьба «сделай пилюлю» выглядит как заведомо большое
    /// число, и это законный приём.
    fn new(rect: Rect, radius: u32) -> Self {
        let hx = fxu(rect.w) / 2;
        let hy = fxu(rect.h) / 2;
        Self {
            cx: fxi(rect.x).saturating_add(hx),
            cy: fxi(rect.y).saturating_add(hy),
            hx,
            hy,
            r: fxu(radius).min(hx).min(hy),
        }
    }

    /// Ширина угловой зоны в целых точках, с округлением вверх.
    ///
    /// Вверх — потому что от неё зависит, какой пролёт строки объявляется
    /// сплошным: недобор здесь означал бы залитую напрямую точку, которую на
    /// самом деле режет дуга.
    fn corner(&self) -> i32 {
        (self.r + ONE - 1) / ONE
    }

    /// Знаковое расстояние до границы: отрицательное внутри, ноль на границе.
    ///
    /// Классическая формула Инго Квилеза. Слагаемое `min(max(q.x, q.y), 0)`
    /// отвечает за внутренность: снаружи оно ноль и работает только длина
    /// `max(q, 0)`, внутри — наоборот.
    ///
    /// Считается в 64 битах: координаты приходят от вызывающего и могут быть
    /// какими угодно, а квадрат разности в 32 бита не всегда помещается.
    fn distance(&self, px: i32, py: i32) -> i32 {
        let qx = (i64::from(px) - i64::from(self.cx)).abs() - i64::from(self.hx - self.r);
        let qy = (i64::from(py) - i64::from(self.cy)).abs() - i64::from(self.hy - self.r);
        let mx = qx.max(0);
        let my = qy.max(0);
        let outside = isqrt((mx * mx + my * my) as u64) as i64;
        let inside = qx.max(qy).min(0);
        clip_i32(outside + inside - i64::from(self.r))
    }

    /// Расстояние до верхней или нижней границы — без учёта боковых.
    ///
    /// Годится ровно там, где столбец заведомо дальше от боковой границы, чем
    /// хватает штриху или тени: в серединном пролёте строки. Там полное
    /// расстояние равно этому, а корень считать не нужно вовсе.
    fn edge_distance(&self, py: i32) -> i32 {
        clip_i32((i64::from(py) - i64::from(self.cy)).abs() - i64::from(self.hy))
    }
}

/// Зажать 64-битное расстояние в 32 бита. Разница между 2^30 и 2^40 точками
/// для покрытия одинакова — и то и другое «бесконечно далеко».
#[must_use]
const fn clip_i32(value: i64) -> i32 {
    if value > (1 << 30) {
        1 << 30
    } else if value < -(1 << 30) {
        -(1 << 30)
    } else {
        value as i32
    }
}

// --------------------------------------------------------------------------
// Чем заливать
// --------------------------------------------------------------------------

/// Заливка фигуры: сплошная или градиент.
///
/// Заведена, чтобы форму (SDF, обрезка, оптимизация строк) написать один раз, а
/// не трижды — для цвета, для вертикального градиента и для горизонтального.
#[derive(Clone, Copy)]
enum Paint {
    Solid(Color),
    Vertical { top: Color, bottom: Color, y0: i32, span: u32 },
    Horizontal { left: Color, right: Color, x0: i32, span: u32 },
}

impl Paint {
    fn color(self, x: i32, y: i32) -> Color {
        match self {
            Self::Solid(color) => color,
            Self::Vertical { top, bottom, y0, span } => top.mix(bottom, ramp(y - y0, span)),
            Self::Horizontal { left, right, x0, span } => left.mix(right, ramp(x - x0, span)),
        }
    }

    /// Цвет, если он постоянен вдоль всей строки.
    ///
    /// Ради этого и заведено: постоянная строка заливается одним
    /// [`Surface::fill`], то есть срезом, а не точкой за точкой. Вертикальный
    /// градиент постоянен внутри строки — он тоже попадает в быстрый путь.
    fn row_color(self, y: i32) -> Option<Color> {
        match self {
            Self::Solid(_) | Self::Vertical { .. } => Some(self.color(0, y)),
            Self::Horizontal { .. } => None,
        }
    }
}

/// Положение на градиенте в 1/255.
#[must_use]
fn ramp(offset: i32, span: u32) -> u8 {
    if span < 2 {
        return 0;
    }
    let last = span - 1;
    let position = offset.clamp(0, last as i32) as u32;
    ((position * 255) / last) as u8
}

// --------------------------------------------------------------------------
// Точка и пролёт
// --------------------------------------------------------------------------

/// Смешать цвет с тем, что уже лежит в точке.
pub fn blend_pixel(surface: &mut Surface, x: i32, y: i32, color: Color, alpha: Alpha) {
    if alpha == 0 {
        return;
    }
    let (Ok(px), Ok(py)) = (u32::try_from(x), u32::try_from(y)) else {
        return;
    };
    if px >= surface.width() || py >= surface.height() {
        return;
    }
    if alpha == 255 {
        // Непрозрачная заливка — самый частый случай, и чтение точки в нём
        // лишнее: результат смешивания от того, что под ней, не зависит.
        surface.put(px, py, color.pixel());
        return;
    }
    let under = Color::from_pixel(surface.get(px, py));
    surface.put(px, py, under.mix(color, alpha).pixel());
}

/// Залить горизонтальный пролёт `[x0, x1)` с постоянным покрытием.
fn fill_span(
    surface: &mut Surface,
    y: i32,
    x0: i32,
    x1: i32,
    paint: Paint,
    alpha: Alpha,
    cover: u8,
) {
    if x1 <= x0 {
        return;
    }
    let strength = scale(cover, alpha);
    if strength == 0 {
        return;
    }
    if strength == 255 {
        if let Some(color) = paint.row_color(y) {
            surface.fill(Rect::new(x0, y, (x1 - x0) as u32, 1), color);
            return;
        }
    }
    for x in x0..x1 {
        blend_pixel(surface, x, y, paint.color(x, y), strength);
    }
}

/// Залить прямоугольник с прозрачностью.
pub fn blend_rect(surface: &mut Surface, rect: Rect, color: Color, alpha: Alpha) {
    if alpha == 0 {
        return;
    }
    let visible = clip(surface, rect);
    if visible.is_empty() {
        return;
    }
    if alpha == 255 {
        surface.fill(visible, color);
        return;
    }
    for y in visible.y..visible.bottom() {
        fill_span(surface, y, visible.x, visible.right(), Paint::Solid(color), alpha, 255);
    }
}

// --------------------------------------------------------------------------
// Заливка скруглённого прямоугольника
// --------------------------------------------------------------------------

/// Общая заливка: форма считается здесь, цвет берётся из [`Paint`].
fn fill_rounded(surface: &mut Surface, rect: Rect, radius: u32, paint: Paint, alpha: Alpha) {
    if alpha == 0 {
        return;
    }
    let visible = clip(surface, rect);
    if visible.is_empty() {
        return;
    }
    let shape = Round::new(rect, radius);
    let corner = shape.corner();
    if corner == 0 {
        // Прямые углы — считать нечего, каждая строка сплошная.
        for y in visible.y..visible.bottom() {
            fill_span(surface, y, visible.x, visible.right(), paint, alpha, 255);
        }
        return;
    }

    for y in visible.y..visible.bottom() {
        // Строка между углами закрыта целиком: и по вертикали, и по горизонтали
        // она удалена от границы больше чем на полточки.
        if y >= rect.y + corner && y < rect.bottom() - corner {
            fill_span(surface, y, visible.x, visible.right(), paint, alpha, 255);
            continue;
        }
        let py = fxi(y) + HALF;
        let left = (rect.x + corner).clamp(visible.x, visible.right());
        let right = (rect.right() - corner).clamp(left, visible.right());
        for x in visible.x..left {
            corner_pixel(surface, &shape, x, y, py, paint, alpha);
        }
        fill_span(surface, y, left, right, paint, alpha, 255);
        for x in right..visible.right() {
            corner_pixel(surface, &shape, x, y, py, paint, alpha);
        }
    }
}

/// Одна точка угловой зоны: тот самый случай, ради которого считается корень.
fn corner_pixel(
    surface: &mut Surface,
    shape: &Round,
    x: i32,
    y: i32,
    py: i32,
    paint: Paint,
    alpha: Alpha,
) {
    let cover = coverage(shape.distance(fxi(x) + HALF, py));
    if cover == 0 {
        return;
    }
    blend_pixel(surface, x, y, paint.color(x, y), scale(cover, alpha));
}

/// Скруглённый прямоугольник со сглаженными углами.
pub fn rounded(surface: &mut Surface, rect: Rect, radius: u32, color: Color, alpha: Alpha) {
    if radius == 0 {
        // Прямые углы — это просто прямоугольник, и незачем идти через SDF.
        blend_rect(surface, rect, color, alpha);
        return;
    }
    fill_rounded(surface, rect, radius, Paint::Solid(color), alpha);
}

/// То же с вертикальным градиентом: `top` наверху, `bottom` внизу.
pub fn rounded_gradient(
    surface: &mut Surface,
    rect: Rect,
    radius: u32,
    top: Color,
    bottom: Color,
    alpha: Alpha,
) {
    let paint = Paint::Vertical { top, bottom, y0: rect.y, span: rect.h };
    fill_rounded(surface, rect, radius, paint, alpha);
}

/// Горизонтальный градиент (для полосок хода: `linear-gradient(90deg, acc2, acc)`).
pub fn horizontal_gradient(
    surface: &mut Surface,
    rect: Rect,
    radius: u32,
    left: Color,
    right: Color,
    alpha: Alpha,
) {
    let paint = Paint::Horizontal { left, right, x0: rect.x, span: rect.w };
    fill_rounded(surface, rect, radius, paint, alpha);
}

// --------------------------------------------------------------------------
// Контуры
// --------------------------------------------------------------------------

/// Кольцо вдоль границы скруглённого прямоугольника.
///
/// `shift` сдвигает середину штриха внутрь: ноль — штрих сидит верхом на
/// границе (так рисует SVG), половина толщины — штрих целиком внутри (так
/// рисует `box-shadow: inset`).
fn ring(
    surface: &mut Surface,
    rect: Rect,
    radius: u32,
    half: i32,
    shift: i32,
    color: Color,
    alpha: Alpha,
) {
    if alpha == 0 || !sane(rect) {
        return;
    }
    let shape = Round::new(rect, radius);

    // Насколько далеко штрих уходит наружу и внутрь от границы — в целых
    // точках, с запасом на сглаживание.
    let reach = (half + shift + HALF) / ONE + 1;
    let area = clip(
        surface,
        Rect::new(
            rect.x.saturating_sub(reach),
            rect.y.saturating_sub(reach),
            rect.w.saturating_add(2 * reach as u32),
            rect.h.saturating_add(2 * reach as u32),
        ),
    );
    if area.is_empty() {
        return;
    }

    // Серединный пролёт строки: там столбец дальше от боковой границы, чем
    // достаёт штрих, и расстояние равно расстоянию до верхнего или нижнего
    // края — постоянному вдоль строки. Отступ берётся не меньше радиуса, иначе
    // в пролёт попала бы дуга.
    let inset = shape.corner().max(reach);
    let split = 2 * inset < rect.w as i32;

    for y in area.y..area.bottom() {
        let py = fxi(y) + HALF;
        if !split {
            for x in area.x..area.right() {
                stroke_pixel(surface, &shape, x, y, py, half, shift, color, alpha);
            }
            continue;
        }
        let left = (rect.x + inset).clamp(area.x, area.right());
        let right = (rect.right() - inset).clamp(left, area.right());
        for x in area.x..left {
            stroke_pixel(surface, &shape, x, y, py, half, shift, color, alpha);
        }
        let cover = stroke_coverage(shape.edge_distance(py) + shift, half);
        fill_span(surface, y, left, right, Paint::Solid(color), alpha, cover);
        for x in right..area.right() {
            stroke_pixel(surface, &shape, x, y, py, half, shift, color, alpha);
        }
    }
}

/// Одна точка штриха.
#[allow(clippy::too_many_arguments)]
fn stroke_pixel(
    surface: &mut Surface,
    shape: &Round,
    x: i32,
    y: i32,
    py: i32,
    half: i32,
    shift: i32,
    color: Color,
    alpha: Alpha,
) {
    let cover = stroke_coverage(shape.distance(fxi(x) + HALF, py) + shift, half);
    if cover == 0 {
        return;
    }
    blend_pixel(surface, x, y, color, scale(cover, alpha));
}

/// Обводка в одну точку внутрь границы (аналог `box-shadow: inset 0 0 0 1px`).
///
/// Внутрь, а не по границе: обводка ложится ровно на край заливки, не выходя за
/// него ни на долю точки. Иначе рамка окна оказалась бы на точку шире самого
/// окна, и соседние окна начали бы задевать друг друга.
pub fn rounded_stroke(surface: &mut Surface, rect: Rect, radius: u32, color: Color, alpha: Alpha) {
    ring(surface, rect, radius, HALF, HALF, color, alpha);
}

/// Дуга скруглённого прямоугольника — контур со сглаживанием, толщина в
/// десятых точки.
///
/// В отличие от [`rounded_stroke`], штрих сидит верхом на границе — как в SVG,
/// откуда значки и перерисованы. Автор значка сам отступает от края на половину
/// толщины, и второй отступ здесь сжал бы рисунок.
pub fn rounded_outline(
    surface: &mut Surface,
    rect: Rect,
    radius: u32,
    weight: u32,
    color: Color,
    alpha: Alpha,
) {
    ring(surface, rect, radius, half_weight(weight), 0, color, alpha);
}

/// Светлая кромка по верхнему краю: одна строка, гаснущая к обоим углам.
///
/// Это то, что в макете `inset 0 1px 0 var(--crown)` вместе с
/// `linear-gradient(90deg, transparent, crown, transparent)`. Гаснет она не для
/// красоты: сплошная светлая строка на скруглённом углу торчит усиком, потому
/// что там фигуры под ней уже нет.
pub fn crown(surface: &mut Surface, rect: Rect, radius: u32, color: Color, alpha: Alpha) {
    if alpha == 0 || rect.is_empty() {
        return;
    }
    let visible = clip(surface, rect);
    if visible.is_empty() || rect.y < visible.y || rect.y >= visible.bottom() {
        return;
    }
    let shape = Round::new(rect, radius);
    let y = rect.y;
    let py = fxi(y) + HALF;
    let last = rect.w - 1;
    for x in visible.x..visible.right() {
        // Насыщенность — произведение трёх долей: сколько от точки закрывает
        // сама фигура, сколько даёт поперечный градиент и сколько просили.
        let cover = coverage(shape.distance(fxi(x) + HALF, py));
        if cover == 0 {
            continue;
        }
        let from_left = (x - rect.x) as u32;
        let fade = ramp(2 * from_left.min(last - from_left) as i32, rect.w);
        blend_pixel(surface, x, y, color, scale(scale(cover, fade), alpha));
    }
}

// --------------------------------------------------------------------------
// Тень
// --------------------------------------------------------------------------

/// Мягкая тень под прямоугольником: расходится на `spread` точек наружу,
/// насыщенность спадает к краю.
///
/// Расходится во все стороны одинаково; смещение вниз, если оно нужно, делается
/// сдвигом самого прямоугольника — так вызывающий волен сдвинуть тень и вбок, а
/// примитив не обрастает ещё двумя числами.
///
/// Рисуется до фигуры. Под самой фигурой тени нет: она домножена на то,
/// **сколько точки не закрыто** фигурой, — иначе полупрозрачное окно
/// подсвечивалось бы изнутри собственной тенью.
pub fn shadow(
    surface: &mut Surface,
    rect: Rect,
    radius: u32,
    spread: u32,
    color: Color,
    alpha: Alpha,
) {
    if alpha == 0 || spread == 0 || !sane(rect) {
        return;
    }
    let shape = Round::new(rect, radius);
    let reach = spread.min(LIMIT) as i32;
    let falloff = fxu(spread);
    let area = clip(
        surface,
        Rect::new(
            rect.x.saturating_sub(reach),
            rect.y.saturating_sub(reach),
            rect.w.saturating_add(2 * reach as u32),
            rect.h.saturating_add(2 * reach as u32),
        ),
    );
    if area.is_empty() {
        return;
    }

    // Тот же приём, что в `ring`: в серединном пролёте расстояние постоянно
    // вдоль строки, и вся полоса тени над окном или под ним заливается одним
    // вызовом вместо тысяч корней.
    let inset = shape.corner().max(reach + 1);
    let split = 2 * inset < rect.w as i32;

    for y in area.y..area.bottom() {
        let py = fxi(y) + HALF;
        if !split {
            for x in area.x..area.right() {
                let strength = shadow_alpha(shape.distance(fxi(x) + HALF, py), falloff);
                blend_pixel(surface, x, y, color, scale(strength, alpha));
            }
            continue;
        }
        let left = (rect.x + inset).clamp(area.x, area.right());
        let right = (rect.right() - inset).clamp(left, area.right());
        for x in area.x..left {
            let strength = shadow_alpha(shape.distance(fxi(x) + HALF, py), falloff);
            blend_pixel(surface, x, y, color, scale(strength, alpha));
        }
        let strength = shadow_alpha(shape.edge_distance(py), falloff);
        fill_span(surface, y, left, right, Paint::Solid(color), alpha, strength);
        for x in right..area.right() {
            let strength = shadow_alpha(shape.distance(fxi(x) + HALF, py), falloff);
            blend_pixel(surface, x, y, color, scale(strength, alpha));
        }
    }
}

/// Насыщенность тени на расстоянии `distance` от фигуры.
///
/// Спад квадратичный, а не линейный: линейный виден как чёткое кольцо там, где
/// тень обрывается, — глаз ловит излом производной, даже когда сам обрыв
/// незаметен.
#[must_use]
fn shadow_alpha(distance: i32, falloff: i32) -> u8 {
    let hidden = u32::from(coverage(distance));
    if hidden == 255 {
        return 0;
    }
    let position = if distance <= 0 {
        0
    } else {
        ((i64::from(distance) * 255) / i64::from(falloff.max(1))).min(255) as u32
    };
    let value = (255 - position) * (255 - position) / 255;
    ((value * (255 - hidden)) / 255) as u8
}

// --------------------------------------------------------------------------
// Круги
// --------------------------------------------------------------------------

/// Круг со сглаженным краем. Центр — середина точки `(cx, cy)`.
pub fn circle(surface: &mut Surface, cx: i32, cy: i32, radius: u32, color: Color, alpha: Alpha) {
    if alpha == 0 || radius == 0 {
        return;
    }
    let (cx, cy) = (tame(cx), tame(cy));
    let r = fxu(radius);
    // Точка закрыта целиком, когда её центр ближе к центру круга, чем на
    // полточки от границы: такую строку можно залить срезом.
    let solid = r - HALF;
    each_disc_row(surface, cx, cy, radius, 1, |surface, y, x0, x1, dy| {
        let inner = span_half(solid, dy);
        let left = (cx - inner).clamp(x0, x1);
        let right = (cx + inner + 1).clamp(left, x1);
        for x in x0..left {
            disc_pixel(surface, cx, y, x, dy, r, color, alpha, 0);
        }
        fill_span(surface, y, left, right, Paint::Solid(color), alpha, 255);
        for x in right..x1 {
            disc_pixel(surface, cx, y, x, dy, r, color, alpha, 0);
        }
    });
}

/// Окружность контуром.
pub fn circle_outline(
    surface: &mut Surface,
    cx: i32,
    cy: i32,
    radius: u32,
    weight: u32,
    color: Color,
    alpha: Alpha,
) {
    if alpha == 0 || radius == 0 {
        return;
    }
    let (cx, cy) = (tame(cx), tame(cy));
    let r = fxu(radius);
    let half = half_weight(weight);
    // Внутри этого круга штриха уже нет — там нечего считать.
    let hollow = r - half - HALF;
    let pad = half / ONE + 1;
    each_disc_row(surface, cx, cy, radius, pad, |surface, y, x0, x1, dy| {
        let inner = span_half(hollow, dy);
        let left = (cx - inner).clamp(x0, x1);
        let right = (cx + inner + 1).clamp(left, x1);
        for x in x0..left {
            disc_pixel(surface, cx, y, x, dy, r, color, alpha, half);
        }
        for x in right..x1 {
            disc_pixel(surface, cx, y, x, dy, r, color, alpha, half);
        }
    });
}

/// Пройти по строкам круга, отдавая обрезанные границы строки и удаление её
/// центра от центра круга.
fn each_disc_row<F>(surface: &mut Surface, cx: i32, cy: i32, radius: u32, pad: i32, mut row: F)
where
    F: FnMut(&mut Surface, i32, i32, i32, i32),
{
    let reach = radius.min(LIMIT) as i32 + pad;
    let height = surface.height() as i32;
    let width = surface.width() as i32;
    let y0 = (cy - reach).max(0);
    let y1 = (cy + reach + 1).min(height);
    let x0 = (cx - reach).max(0);
    let x1 = (cx + reach + 1).min(width);
    if x1 <= x0 {
        return;
    }
    for y in y0..y1 {
        let dy = fxi(y - cy).abs();
        row(surface, y, x0, x1, dy);
    }
}

/// Половина ширины строки, целиком лежащей внутри круга радиуса `r`, в точках.
///
/// Один корень на строку вместо корня на точку — ради этого всё и затевалось.
#[must_use]
fn span_half(r: i32, dy: i32) -> i32 {
    if r <= 0 || dy >= r {
        return -1;
    }
    let r = i64::from(r);
    let dy = i64::from(dy);
    (isqrt((r * r - dy * dy) as u64) / ONE as u64) as i32
}

/// Точка круга: заливки при `half = 0`, контура — иначе.
#[allow(clippy::too_many_arguments)]
fn disc_pixel(
    surface: &mut Surface,
    cx: i32,
    y: i32,
    x: i32,
    dy: i32,
    r: i32,
    color: Color,
    alpha: Alpha,
    half: i32,
) {
    let dx = i64::from(fxi(x - cx).abs());
    let dy = i64::from(dy);
    let distance = clip_i32(isqrt((dx * dx + dy * dy) as u64) as i64 - i64::from(r));
    let cover = if half == 0 {
        coverage(distance)
    } else {
        stroke_coverage(distance, half)
    };
    if cover == 0 {
        return;
    }
    blend_pixel(surface, x, y, color, scale(cover, alpha));
}

// --------------------------------------------------------------------------
// Линии
// --------------------------------------------------------------------------

/// Линия толщиной в точку. Отдельно от `fill`, потому что читается лучше.
pub fn hline(surface: &mut Surface, x: i32, y: i32, len: u32, color: Color, alpha: Alpha) {
    blend_rect(surface, Rect::new(x, y, len, 1), color, alpha);
}

/// Вертикальная линия толщиной в точку.
pub fn vline(surface: &mut Surface, x: i32, y: i32, len: u32, color: Color, alpha: Alpha) {
    blend_rect(surface, Rect::new(x, y, 1, len), color, alpha);
}

/// Отрезок произвольного наклона со сглаживанием, толщиной в `weight` десятых
/// точки (14 = 1.4 px, как штрих значков в макете).
///
/// Расстояние до отрезка — это расстояние до его ближайшей точки, а она
/// находится проекцией с зажимом на концы. Вырожденный отрезок (начало равно
/// концу) при этом сам собой превращается в кружок: проекция зажимается в
/// начало, и делить на нулевую длину не приходится.
pub fn line(
    surface: &mut Surface,
    from: (i32, i32),
    to: (i32, i32),
    weight: u32,
    color: Color,
    alpha: Alpha,
) {
    if alpha == 0 {
        return;
    }
    let half = half_weight(weight);
    let from = (tame(from.0), tame(from.1));
    let to = (tame(to.0), tame(to.1));
    let ax = fxi(from.0) + HALF;
    let ay = fxi(from.1) + HALF;
    let dx = i64::from(fxi(to.0) - ax + HALF);
    let dy = i64::from(fxi(to.1) - ay + HALF);
    let length2 = dx * dx + dy * dy;

    let reach = half / ONE + 1;
    let width = surface.width() as i32;
    let height = surface.height() as i32;
    let x0 = (from.0.min(to.0) - reach).max(0);
    let x1 = (from.0.max(to.0) + reach + 1).min(width);
    let y0 = (from.1.min(to.1) - reach).max(0);
    let y1 = (from.1.max(to.1) + reach + 1).min(height);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // Полоса вокруг бесконечной прямой: за её пределами считать нечего. Без
    // этого диагональ через весь экран стоила бы корня на каждую точку своего
    // охватывающего прямоугольника, то есть на миллион точек ради тысячи.
    let band = (i64::from(half) + i64::from(ONE)) * isqrt(length2 as u64) as i64;

    for y in y0..y1 {
        let py = fxi(y) + HALF;
        let (mut lo, mut hi) = (x0, x1);
        if dy != 0 {
            let centre = (i64::from(py) - i64::from(ay)) * dx;
            let one = (centre - band) / dy;
            let two = (centre + band) / dy;
            let (near, far) = if dy > 0 { (one, two) } else { (two, one) };
            lo = lo.max(clip_i32(i64::from(ax) + near - i64::from(HALF)).div_euclid(ONE));
            hi = hi.min(clip_i32(i64::from(ax) + far - i64::from(HALF)).div_euclid(ONE) + 1);
        }
        for x in lo..hi {
            let px = fxi(x) + HALF;
            let vx = i64::from(px) - i64::from(ax);
            let vy = i64::from(py) - i64::from(ay);
            // Положение проекции на отрезке, в долях 1/256, зажатое концами.
            let position = if length2 == 0 {
                0
            } else {
                ((vx * dx + vy * dy) * i64::from(ONE) / length2).clamp(0, i64::from(ONE))
            };
            let ox = vx - dx * position / i64::from(ONE);
            let oy = vy - dy * position / i64::from(ONE);
            let distance = clip_i32(isqrt((ox * ox + oy * oy) as u64) as i64);
            let cover = stroke_coverage(distance, half);
            if cover == 0 {
                continue;
            }
            blend_pixel(surface, x, y, color, scale(cover, alpha));
        }
    }
}
