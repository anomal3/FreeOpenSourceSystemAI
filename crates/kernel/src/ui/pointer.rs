//! Указатель мыши: положение на экране и его картинка.
//!
//! # Почему курсор — не окно и не поверхность
//!
//! [`mini_ui::Surface`] выводится прямоугольником целиком, а курсор обязан
//! пропускать сквозь себя то, что под ним: непрозрачный прямоугольник вокруг
//! стрелки закрывал бы текст, над которым она стоит. Поэтому курсор — две
//! однобитные маски, обводка и заливка, которые рисуются
//! [`mini_ui::Screen::draw_bitmap`] последним слоем кадра.
//!
//! # Почему не надо сохранять то, что под курсором
//!
//! Классический приём — запомнить прямоугольник экрана под стрелкой и вернуть
//! его при сдвиге — здесь не нужен и невозможен: фреймбуфер нельзя читать
//! (см. [`mini_ui`]). Вместо этого сдвиг помечает изменившимися **два**
//! прямоугольника, старый и новый, а композитор пересобирает их из фона и окон,
//! рисуя курсор поверх. Никакого «следа» за стрелкой это не оставляет, потому
//! что след — это как раз то, чего не пересобрали.

use mini_ui::{Color, Rect};

/// Ширина картинки курсора.
pub const WIDTH: u32 = 12;
/// Высота картинки курсора.
pub const HEIGHT: u32 = 19;

/// Обводка стрелки — тёмная.
///
/// Обводка обязательна, а не украшение: белая стрелка на светлом заголовке окна
/// исчезает целиком, а тёмная на тёмном фоне стола — тоже. Пара «тёмный контур
/// плюс светлая заливка» видна на любом фоне, и по этой причине так устроен
/// курсор во всех оконных системах.
const OUTLINE: [u16; HEIGHT as usize] = [
    0x800, 0xC00, 0xA00, 0x900, 0x880, 0x840, 0x820, 0x810, 0x808, 0x804, 0x83E, 0x920, 0xA90,
    0xC90, 0x848, 0x048, 0x024, 0x024, 0x018,
];

/// Заливка стрелки — светлая.
const FILL: [u16; HEIGHT as usize] = [
    0x000, 0x000, 0x400, 0x600, 0x700, 0x780, 0x7C0, 0x7E0, 0x7F0, 0x7F8, 0x7C0, 0x6C0, 0x460,
    0x060, 0x030, 0x030, 0x018, 0x018, 0x000,
];

/// Цвет обводки.
const OUTLINE_COLOR: Color = Color::rgb(0x04, 0x08, 0x0E);
/// Цвет заливки.
const FILL_COLOR: Color = Color::rgb(0xF2, 0xF6, 0xFA);

/// Указатель: где он и виден ли.
pub struct Pointer {
    x: i32,
    y: i32,
    /// Курсора нет, пока не пришёл первый отчёт мыши.
    ///
    /// Рисовать стрелку на машине без мыши значит обещать то, чего нет: человек
    /// будет искать, чем её двигать.
    visible: bool,
    /// До какого времени (мс) рядом со стрелкой крутится круг «программа
    /// запускается», как в Windows. Ноль — не крутится.
    busy_until: u64,
}

/// Поле круга «программа запускается» справа снизу от стрелки.
const SPIN_SIDE: u32 = 16;

impl Pointer {
    /// Указатель в середине экрана, ещё невидимый.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { x: (width / 2) as i32, y: (height / 2) as i32, visible: false, busy_until: 0 }
    }

    /// Показывать круг «программа запускается» до `until` (мс от запуска).
    pub fn set_busy(&mut self, until: u64) {
        self.busy_until = until;
    }

    /// Крутится ли круг сейчас.
    #[must_use]
    pub fn is_busy(&self, now: u64) -> bool {
        self.busy_until != 0 && now < self.busy_until
    }

    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.visible
    }

    #[must_use]
    pub const fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    /// Прямоугольник, который занимает картинка курсора.
    #[must_use]
    pub const fn rect(&self) -> Rect {
        // С кругом — шире и ниже: курсор, сжавшийся без круга, обязан стереть
        // и то место, где круг был.
        if self.busy_until != 0 {
            return Rect::new(self.x, self.y, WIDTH + SPIN_SIDE, HEIGHT + SPIN_SIDE / 2);
        }
        Rect::new(self.x, self.y, WIDTH, HEIGHT)
    }

    /// Круг погас — указатель снова просто стрелка. Возвращает прямоугольник,
    /// который занимал круг (его надо перерисовать), если он был.
    pub fn clear_busy(&mut self) -> Option<Rect> {
        if self.busy_until == 0 {
            return None;
        }
        let was = self.rect();
        self.busy_until = 0;
        Some(was)
    }

    /// Сдвинуть указатель, оставив его на экране.
    ///
    /// Возвращает `true`, если положение действительно изменилось: отчёт мыши
    /// без движения (нажали кнопку, не шевельнув рукой) не должен требовать
    /// перерисовки.
    pub fn move_by(&mut self, dx: i32, dy: i32, width: u32, height: u32) -> bool {
        // Кончик стрелки обязан остаться на экране, иначе указатель уходит за
        // край и вернуть его нечем. Ограничение — по последней точке экрана, а
        // не по картинке: стрелка вправе высовываться за край, как в любой
        // оконной системе.
        let x = (self.x + dx).clamp(0, width.saturating_sub(1) as i32);
        let y = (self.y + dy).clamp(0, height.saturating_sub(1) as i32);
        if x == self.x && y == self.y {
            return false;
        }
        self.x = x;
        self.y = y;
        true
    }

    /// Поставить указатель в точку, оставив его на экране.
    ///
    /// Существует ради планшета: он сообщает не сдвиг, а положение, и «навести»
    /// для него — это буквально поставить, а не привезти. Возвращает `true`,
    /// если положение изменилось, по той же причине, что и [`Pointer::move_by`].
    pub fn move_to(&mut self, x: i32, y: i32, width: u32, height: u32) -> bool {
        let x = x.clamp(0, width.saturating_sub(1) as i32);
        let y = y.clamp(0, height.saturating_sub(1) as i32);
        if x == self.x && y == self.y {
            return false;
        }
        self.x = x;
        self.y = y;
        true
    }

    /// Показать курсор. Возвращает `true`, если он был скрыт до этого.
    pub fn show(&mut self) -> bool {
        if self.visible {
            return false;
        }
        self.visible = true;
        true
    }

    /// Картинка стрелки для курсора адаптера (фаза 67b): `side`×`side` точек
    /// `0xAARRGGBB`, вне стрелки — прозрачные. Те же маски, что рисует стол:
    /// стрелка у адаптера не должна отличаться от своей.
    #[must_use]
    pub fn image(side: u32) -> alloc::vec::Vec<u32> {
        let mut image = alloc::vec![0u32; (side * side) as usize];
        let opaque = |color: Color| 0xFF00_0000 | (color.pixel() & 0x00FF_FFFF);
        for (row, (outline, fill)) in OUTLINE.iter().zip(FILL.iter()).enumerate() {
            for column in 0..WIDTH {
                let bit = 1u16 << (WIDTH - 1 - column);
                let at = row * side as usize + column as usize;
                if outline & bit != 0 {
                    image[at] = opaque(OUTLINE_COLOR);
                } else if fill & bit != 0 {
                    image[at] = opaque(FILL_COLOR);
                }
            }
        }
        image
    }

    /// Нарисовать курсор в собираемой полосе кадра.
    ///
    /// `dy` — сдвиг координат экрана к координатам полосы. Курсор рисуется в
    /// каждой полосе, которую задевает, и обрезается сам: маска ставит точки
    /// поштучно, а поверхность отбрасывает то, что вышло за её края.
    pub fn draw(&self, surface: &mut mini_ui::Surface, dy: i32) {
        if !self.visible {
            return;
        }
        // Обводка первой, заливка поверх: маски не пересекаются, но порядок
        // задаёт, что окажется сверху при округлении координат.
        surface.draw_bitmap((self.x, self.y + dy), &OUTLINE, WIDTH, OUTLINE_COLOR);
        surface.draw_bitmap((self.x, self.y + dy), &FILL, WIDTH, FILL_COLOR);
        let now = crate::time::uptime_ms();
        if self.is_busy(now) {
            // Восемь точек по кругу, яркая бежит — 100 мс на шаг.
            const SPOTS: [(i32, i32); 8] = [(0, -5), (4, -4), (5, 0), (4, 4), (0, 5), (-4, 4), (-5, 0), (-4, -4)];
            let cx = self.x + WIDTH as i32 + SPIN_SIDE as i32 / 2 - 2;
            let cy = self.y + dy + HEIGHT as i32 - 2;
            let head = (now / 100) as usize % SPOTS.len();
            for (index, (sx, sy)) in SPOTS.iter().enumerate() {
                let age = (head + SPOTS.len() - index) % SPOTS.len();
                let alpha = 255u32.saturating_sub(age as u32 * 30) as u8;
                mini_ui::draw::circle(surface, cx + sx, cy + sy, 2, OUTLINE_COLOR, alpha);
                mini_ui::draw::circle(surface, cx + sx, cy + sy, 1, Color::rgb(0x3B, 0x82, 0xF6), alpha);
            }
        }
    }
}
