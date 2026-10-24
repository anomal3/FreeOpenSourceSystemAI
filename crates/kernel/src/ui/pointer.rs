//! Указатель мыши: положение на экране и его картинка.
//!
//! # Картинки
//!
//! С фазы 72c — курсоры из макета Романа: стрелка, «программа запускается»
//! (стрелка с кругом, восемь кадров), текст, руки, растяжки. Картинки 32×32
//! и 48×48 (на экране с множителем 2 и больше) с прозрачностью — FIMG из
//! `assets/cursors`, вшитые в ядро; рисуются последним слоем кадра с
//! настоящим смешиванием, а не однобитной маской, как было.
//!
//! # Почему не надо сохранять то, что под курсором
//!
//! Классический приём — запомнить прямоугольник экрана под стрелкой и вернуть
//! его при сдвиге — здесь не нужен и невозможен: фреймбуфер нельзя читать
//! (см. [`mini_ui`]). Вместо этого сдвиг помечает изменившимися **два**
//! прямоугольника, старый и новый, а композитор пересобирает их из фона и окон,
//! рисуя курсор поверх. Никакого «следа» за стрелкой это не оставляет, потому
//! что след — это как раз то, чего не пересобрали.

use alloc::vec::Vec;

use mini_ui::image::Image;
use mini_ui::Rect;

use crate::sync::SpinLock;

/// Какой курсор показать.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Arrow,
    /// Над текстом (терминал).
    Text,
    /// Рука — над тем, что открывается одним щелчком.
    Hand,
    /// Тянут окно за угол: правый нижний и левый верхний.
    ResizeNwse,
    ResizeNesw,
    ResizeEw,
    ResizeNs,
    Move,
    NotAllowed,
}

/// Картинки одного размера: имя файла и точка прицела (из `hotspots.json`).
macro_rules! cursor_set {
    ($side:literal, $($name:literal => ($hx:literal, $hy:literal)),* $(,)?) => {
        [$((include_bytes!(concat!("../../../../assets/cursors/", $side, "/", $name, ".fimg")).as_slice(), ($hx, $hy))),*]
    };
}

/// Порядок картинок: формы [`Shape`] по порядку, затем восемь кадров
/// «программа запускается».
const SMALL: [(&[u8], (u32, u32)); 17] = cursor_set!("32",
    "arrow" => (7, 3), "text" => (16, 16), "hand" => (14, 4), "resize-nwse" => (16, 16),
    "resize-nesw" => (16, 16), "resize-ew" => (16, 16), "resize-ns" => (16, 16), "move" => (16, 16),
    "not-allowed" => (16, 16),
    "arrow-busy-1" => (7, 3), "arrow-busy-2" => (7, 3), "arrow-busy-3" => (7, 3), "arrow-busy-4" => (7, 3),
    "arrow-busy-5" => (7, 3), "arrow-busy-6" => (7, 3), "arrow-busy-7" => (7, 3), "arrow-busy-8" => (7, 3),
);
const LARGE: [(&[u8], (u32, u32)); 17] = cursor_set!("48",
    "arrow" => (10, 4), "text" => (24, 24), "hand" => (21, 6), "resize-nwse" => (24, 24),
    "resize-nesw" => (24, 24), "resize-ew" => (24, 24), "resize-ns" => (24, 24), "move" => (24, 24),
    "not-allowed" => (24, 24),
    "arrow-busy-1" => (10, 4), "arrow-busy-2" => (10, 4), "arrow-busy-3" => (10, 4), "arrow-busy-4" => (10, 4),
    "arrow-busy-5" => (10, 4), "arrow-busy-6" => (10, 4), "arrow-busy-7" => (10, 4), "arrow-busy-8" => (10, 4),
);

/// Номер первого кадра «программа запускается».
const BUSY: usize = 9;
/// Сколько длится кадр круга, мс.
const BUSY_FRAME_MS: u64 = 100;

/// Разобранные картинки: малые и большие. Разбираются один раз.
static IMAGES: SpinLock<Option<(Vec<(Image, (u32, u32))>, Vec<(Image, (u32, u32))>)>> = SpinLock::new(None);

fn parse(set: &[(&[u8], (u32, u32))]) -> Vec<(Image, (u32, u32))> {
    set.iter().filter_map(|(bytes, hot)| Image::parse(bytes).map(|image| (image, *hot))).collect()
}

/// Сделать что-то с картинкой номер `index` нужного размера.
fn with_image<T>(large: bool, index: usize, work: impl FnOnce(&Image, (u32, u32)) -> T) -> Option<T> {
    let mut images = IMAGES.lock();
    if images.is_none() {
        *images = Some((parse(&SMALL), parse(&LARGE)));
    }
    let (small, big) = images.as_ref()?;
    let set = if large { big } else { small };
    set.get(index).map(|(image, hot)| work(image, *hot))
}

/// Указатель: где он и виден ли.
pub struct Pointer {
    x: i32,
    y: i32,
    /// Курсора нет, пока не пришёл первый отчёт мыши.
    ///
    /// Рисовать стрелку на машине без мыши значит обещать то, чего нет: человек
    /// будет искать, чем её двигать.
    visible: bool,
    /// До какого времени (мс) курсор — «программа запускается», как в
    /// Windows. Ноль — нет.
    busy_until: u64,
    shape: Shape,
    /// Картинки 48×48 — на экране с множителем 2 и больше.
    large: bool,
}

impl Pointer {
    /// Указатель в середине экрана, ещё невидимый.
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { x: (width / 2) as i32, y: (height / 2) as i32, visible: false, busy_until: 0, shape: Shape::Arrow, large: false }
    }

    /// Картинки для экрана с этим множителем геометрии.
    pub fn set_scale(&mut self, scale: u32) {
        self.large = scale >= 2;
    }

    /// Показывать «программа запускается» до `until` (мс от запуска).
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

    #[must_use]
    pub const fn shape(&self) -> Shape {
        self.shape
    }

    /// Сменить форму. `true` — сменилась.
    pub fn set_shape(&mut self, shape: Shape) -> bool {
        let changed = self.shape != shape;
        self.shape = shape;
        changed
    }

    /// Какая картинка сейчас: форма или кадр круга.
    fn index(&self, now: u64) -> usize {
        if self.is_busy(now) && self.shape == Shape::Arrow {
            return BUSY + (now / BUSY_FRAME_MS) as usize % 8;
        }
        self.shape as usize
    }

    /// Номер картинки для курсора адаптера: меняется — картинку надо отдать
    /// заново.
    #[must_use]
    pub fn image_key(&self) -> usize {
        self.index(crate::time::uptime_ms())
    }

    /// Прямоугольник, который занимает картинка курсора. Один на все формы —
    /// квадрат картинки, сдвинутый на точку прицела стрелки: смена формы
    /// стирает и прежнюю картинку.
    #[must_use]
    pub fn rect(&self) -> Rect {
        let (side, hot) = if self.large { (48, (10, 4)) } else { (32, (7, 3)) };
        let half = side as i32 / 2;
        Rect::new(self.x - half, self.y - half, side, side).union(&Rect::new(self.x - hot.0, self.y - hot.1, side, side))
    }

    /// Круг погас — указатель снова просто стрелка. Возвращает прямоугольник,
    /// который занимал круг (его надо перерисовать), если он был.
    pub fn clear_busy(&mut self) -> Option<Rect> {
        if self.busy_until == 0 {
            return None;
        }
        self.busy_until = 0;
        Some(self.rect())
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

    /// Картинка курсора для адаптера (фаза 67b): `side`×`side` точек
    /// `0xAARRGGBB` и точка прицела. Та же картинка, что рисует стол.
    #[must_use]
    pub fn image(&self, side: u32) -> (Vec<u32>, (u32, u32)) {
        let mut out = alloc::vec![0u32; (side * side) as usize];
        let hot = with_image(self.large, self.image_key(), |image, hot| {
            for y in 0..image.height.min(side) {
                for x in 0..image.width.min(side) {
                    let [r, g, b, a] = image.pixel(x, y);
                    out[(y * side + x) as usize] =
                        (u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
                }
            }
            hot
        })
        .unwrap_or((0, 0));
        (out, hot)
    }

    /// Нарисовать курсор в собираемой полосе кадра.
    ///
    /// `dy` — сдвиг координат экрана к координатам полосы. Картинка
    /// обрезается сама: поверхность отбрасывает то, что вышло за её края.
    pub fn draw(&self, surface: &mut mini_ui::Surface, dy: i32) {
        if !self.visible {
            return;
        }
        let (x, y) = (self.x, self.y + dy);
        with_image(self.large, self.index(crate::time::uptime_ms()), |image, (hx, hy)| {
            image.draw(surface, Rect::new(x - hx as i32, y - hy as i32, image.width, image.height));
        });
    }
}
