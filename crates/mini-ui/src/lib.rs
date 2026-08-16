//! Рисование поверх линейного фреймбуфера: прямоугольники, поверхности в
//! памяти, текст и простые элементы интерфейса.
//!
//! Крейт выделен из ядра, когда у той же графики появился второй потребитель —
//! установщик. Общий код здесь не ради экономии строк: композитор ядра и
//! установщик показывают человеку **одну и ту же** систему, и расхождение в
//! шрифте или в палитре между ними выглядело бы как две разные программы.
//!
//! Работает и до `ExitBootServices` (установщик берёт адрес буфера у GOP), и
//! после (ядро — из `BootInfo`): для рисования фреймбуфер — просто память с
//! известной геометрией, и что происходит вокруг, ему безразлично.
//!
//! # Две разные памяти
//!
//! Фреймбуфер — память устройства. Писать в него дорого, а **читать**
//! катастрофически дорого: это write-combining область, и чтение сбрасывает
//! буфер записи, чтобы отдать значение. Именно поэтому здесь нет ни одной
//! операции, читающей экран, и именно поэтому окна живут не на экране, а в
//! обычной памяти — в [`Surface`], откуда их можно читать, сравнивать и
//! перерисовывать сколько угодно.
//!
//! # Почему поверхности хранят уже упакованные пиксели
//!
//! Формат пикселя (порядок каналов) задаёт прошивка, и он один на всю машину.
//! Если поверхность хранит цвет в виде (R, G, B), то каждый вывод на экран —
//! это упаковка миллиона пикселей; если она хранит уже упакованные слова,
//! вывод становится копированием. Поэтому формат запоминается один раз при
//! создании [`Screen`], а [`Color::pixel`] обращается к нему.
//!
//! Цена — глобальное состояние. Плата за альтернативу выше: формат пришлось бы
//! протаскивать в каждую функцию рисования и в каждую структуру, которая
//! рисует.

#![no_std]

extern crate alloc;

pub mod font;
pub mod text;
pub mod widget;

use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;
use boot_info::{Framebuffer as FbInfo, PixelFormat};

/// Формат пикселя, запомненный при создании [`Screen`].
///
/// Ноль означает «не инициализирован»; варианты кодируются числами ниже.
static FORMAT: AtomicU32 = AtomicU32::new(FORMAT_UNSET);

const FORMAT_UNSET: u32 = 0;
const FORMAT_RGB: u32 = 1;
const FORMAT_BGR: u32 = 2;

/// Запомнить формат пикселя машины.
fn set_format(format: PixelFormat) {
    let value = match format {
        PixelFormat::Rgb => FORMAT_RGB,
        PixelFormat::Bgr => FORMAT_BGR,
        PixelFormat::Unknown => FORMAT_UNSET,
    };
    FORMAT.store(value, Ordering::Release);
}

/// Цвет в виде, не зависящем от машины.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Упакованное значение пикселя для этой машины.
    ///
    /// До создания [`Screen`] возвращает ноль: рисовать ещё некуда, и чёрный —
    /// самый безобидный ответ.
    #[must_use]
    pub fn pixel(self) -> u32 {
        let (r, g, b) = (u32::from(self.r), u32::from(self.g), u32::from(self.b));
        match FORMAT.load(Ordering::Acquire) {
            // Little-endian: `Rgb` означает «байт 0 — красный», то есть красный
            // попадает в младшие восемь бит слова. Перепутать Rgb и Bgr —
            // классическая ошибка, после которой синий интерфейс краснеет.
            FORMAT_RGB => r | (g << 8) | (b << 16),
            FORMAT_BGR => b | (g << 8) | (r << 16),
            _ => 0,
        }
    }

    /// Смешать два цвета в заданной пропорции: `weight = 0` — целиком `self`,
    /// `255` — целиком `other`.
    ///
    /// Нужно для затенения: рамка неактивного окна — это его цвет, приглушённый
    /// к фону, а не отдельно подобранный третий цвет.
    #[must_use]
    pub const fn mix(self, other: Self, weight: u8) -> Self {
        Self {
            r: blend(self.r, other.r, weight),
            g: blend(self.g, other.g, weight),
            b: blend(self.b, other.b, weight),
        }
    }
}

/// Смешать два значения канала. Отдельная функция, а не замыкание внутри
/// [`Color::mix`]: в `const fn` замыкания вызывать нельзя.
const fn blend(a: u8, b: u8, weight: u8) -> u8 {
    let a = a as u16;
    let b = b as u16;
    let w = weight as u16;
    ((a * (255 - w) + b * w) / 255) as u8
}

/// Прямоугольник. Начало со знаком: окно вправе выехать за край экрана, и
/// отрицательная координата — законное состояние, а не ошибка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const EMPTY: Self = Self { x: 0, y: 0, w: 0, h: 0 };

    #[must_use]
    pub const fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Правая граница (исключающая).
    #[must_use]
    pub const fn right(&self) -> i32 {
        self.x + self.w as i32
    }

    /// Нижняя граница (исключающая).
    #[must_use]
    pub const fn bottom(&self) -> i32 {
        self.y + self.h as i32
    }

    /// Пересечение. Пустой прямоугольник, если пересечения нет.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            return Self::EMPTY;
        }
        Self { x, y, w: (right - x) as u32, h: (bottom - y) as u32 }
    }

    /// Наименьший прямоугольник, содержащий оба.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self { x, y, w: (right - x) as u32, h: (bottom - y) as u32 }
    }

    /// Сдвинуть.
    #[must_use]
    pub const fn translate(&self, dx: i32, dy: i32) -> Self {
        Self { x: self.x + dx, y: self.y + dy, w: self.w, h: self.h }
    }

    /// Уменьшить со всех сторон на `margin`.
    #[must_use]
    pub fn shrink(&self, margin: u32) -> Self {
        let twice = margin.saturating_mul(2);
        if self.w <= twice || self.h <= twice {
            return Self::EMPTY;
        }
        Self {
            x: self.x + margin as i32,
            y: self.y + margin as i32,
            w: self.w - twice,
            h: self.h - twice,
        }
    }

    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

/// Поверхность в обычной памяти: то, во что рисуют окна и экраны установщика.
///
/// Пиксели хранятся уже упакованными под формат машины — см. заголовок модуля.
pub struct Surface {
    pixels: Vec<u32>,
    width: u32,
    height: u32,
}

impl Surface {
    /// Создать поверхность, залитую цветом.
    ///
    /// Возвращает `None`, если памяти не хватило: поверхность размером с экран —
    /// это несколько мегабайт, и отказ здесь совершенно реален. Паниковать
    /// из-за этого нельзя — и ядро, и установщик обязаны продолжить работу без
    /// графики.
    #[must_use]
    pub fn new(width: u32, height: u32, fill: Color) -> Option<Self> {
        if width == 0 || height == 0 {
            return None;
        }
        let len = (width as usize).checked_mul(height as usize)?;
        let mut pixels = Vec::new();
        // `try_reserve_exact`, а не `vec![]`: отказ аллокатора обязан вернуться
        // ошибкой, а не уйти в `handle_alloc_error` и остановить систему.
        pixels.try_reserve_exact(len).ok()?;
        pixels.resize(len, fill.pixel());
        Some(Self { pixels, width, height })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Границы поверхности в её собственных координатах.
    #[must_use]
    pub const fn bounds(&self) -> Rect {
        Rect { x: 0, y: 0, w: self.width, h: self.height }
    }

    /// Одна строка пикселей — для вывода на экран без промежуточных копий.
    #[must_use]
    pub fn row(&self, y: u32) -> &[u32] {
        let start = (y as usize) * (self.width as usize);
        &self.pixels[start..start + self.width as usize]
    }

    /// Поставить пиксель. Координаты за пределами поверхности игнорируются:
    /// рисующий код обрезкой заниматься не должен.
    pub fn put(&mut self, x: u32, y: u32, pixel: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        self.pixels[(y as usize) * (self.width as usize) + (x as usize)] = pixel;
    }

    /// Залить прямоугольник.
    pub fn fill(&mut self, rect: Rect, color: Color) {
        let rect = rect.intersect(&self.bounds());
        if rect.is_empty() {
            return;
        }
        let pixel = color.pixel();
        for y in rect.y..rect.bottom() {
            let start = (y as usize) * (self.width as usize) + rect.x as usize;
            self.pixels[start..start + rect.w as usize].fill(pixel);
        }
    }

    /// Обвести прямоугольник рамкой толщиной `thickness` внутрь.
    pub fn frame(&mut self, rect: Rect, thickness: u32, color: Color) {
        if rect.is_empty() || thickness == 0 {
            return;
        }
        let t = thickness;
        self.fill(Rect::new(rect.x, rect.y, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.bottom() - t as i32, rect.w, t), color);
        self.fill(Rect::new(rect.x, rect.y, t, rect.h), color);
        self.fill(Rect::new(rect.right() - t as i32, rect.y, t, rect.h), color);
    }

    /// Сдвинуть содержимое прямоугольника вверх на `lines` строк, освободившееся
    /// снизу залить цветом.
    ///
    /// Существует ради прокрутки текста: сдвиг внутри обычной памяти стоит на
    /// порядки дешевле перерисовки всех глифов заново, и — в отличие от сдвига на
    /// экране — вообще возможен, потому что поверхность можно читать.
    pub fn scroll_up(&mut self, rect: Rect, lines: u32, fill: Color) {
        let rect = rect.intersect(&self.bounds());
        if rect.is_empty() || lines == 0 {
            return;
        }
        if lines >= rect.h {
            self.fill(rect, fill);
            return;
        }
        let stride = self.width as usize;
        let x = rect.x as usize;
        let w = rect.w as usize;
        for y in 0..(rect.h - lines) {
            let dst = ((rect.y as u32 + y) as usize) * stride + x;
            let src = ((rect.y as u32 + y + lines) as usize) * stride + x;
            // `copy_within` на всём буфере, а не построчная копия срезов: две
            // непересекающиеся строки одного `Vec` иначе не одолжить.
            self.pixels.copy_within(src..src + w, dst);
        }
        let cleared = Rect::new(rect.x, rect.bottom() - lines as i32, rect.w, lines);
        self.fill(cleared, fill);
    }
}

/// Экран: линейный фреймбуфер, полученный от прошивки.
pub struct Screen {
    base: *mut u32,
    width: u32,
    height: u32,
    /// Пикселей на строку развёртки; может быть больше `width`.
    stride: u32,
}

// SAFETY: единственное, что мешает вывести `Send` автоматически, — сырой
// указатель. Он адресует память устройства, не привязанную ни к какому потоку и
// не имеющую владельца, которого можно было бы бросить.
unsafe impl Send for Screen {}

impl Screen {
    /// Создать экран по описанию фреймбуфера.
    ///
    /// Возвращает `None`, если фреймбуфера нет, формат пикселя неизвестен или
    /// заявленный размер не сходится с геометрией.
    #[must_use]
    pub fn new(fb: &FbInfo) -> Option<Self> {
        if !fb.is_present() || fb.format == PixelFormat::Unknown {
            return None;
        }
        // Геометрия приходит из-за границы доверия: проверяем, что заявленный
        // размер действительно вмещает stride * height 32-битных пикселей.
        let needed = u64::from(fb.stride) * u64::from(fb.height) * 4;
        if fb.width == 0 || fb.height == 0 || fb.stride < fb.width || needed > fb.size {
            return None;
        }
        set_format(fb.format);
        Some(Self {
            base: fb.base as *mut u32,
            width: fb.width,
            height: fb.height,
            stride: fb.stride,
        })
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn bounds(&self) -> Rect {
        Rect { x: 0, y: 0, w: self.width, h: self.height }
    }

    /// Залить прямоугольник экрана.
    pub fn fill(&self, rect: Rect, color: Color) {
        let rect = rect.intersect(&self.bounds());
        if rect.is_empty() {
            return;
        }
        let pixel = color.pixel();
        for y in rect.y..rect.bottom() {
            let start = (y as usize) * (self.stride as usize) + rect.x as usize;
            // Строка целиком, а не пиксель за пикселем. Разница не
            // косметическая: `write_volatile` компилятор не вправе ни объединить,
            // ни векторизовать, и заливка шла по четыре байта за раз. На экране
            // 1920×1080 это превращало обычное переключение окон в четверть
            // секунды работы — за это время терялось следующее нажатие клавиши,
            // потому что клавиатуру некому было опросить.
            //
            // SAFETY: `start + rect.w` не выходит за `stride * height` пикселей —
            // это проверено в `new` против заявленного `fb.size`, а
            // прямоугольник обрезан по границам экрана. Ссылка живёт только
            // внутри этой итерации, и другой ссылки на те же пиксели нет:
            // фреймбуфер трогает один рабочий стол из одной задачи.
            let row = unsafe { core::slice::from_raw_parts_mut(self.base.add(start), rect.w as usize) };
            row.fill(pixel);
        }
    }

    /// Нарисовать однобитную картинку: точка ставится там, где в маске единица.
    ///
    /// Заведено ради указателя мыши, и другого способа его нарисовать нет.
    /// [`Surface`] выводится прямоугольником целиком, а курсор обязан пропускать
    /// сквозь себя то, что под ним: непрозрачный прямоугольник вокруг стрелки
    /// закрывал бы текст, над которым она стоит.
    ///
    /// Строка маски — `u16`, старший используемый бит слева; ширина не больше 16
    /// точек. Ограничение не мешает: курсор шире шестнадцати точек не бывает, а
    /// произвольная ширина потребовала бы срезов срезов ради одной картинки.
    pub fn draw_bitmap(&self, at: (i32, i32), rows: &[u16], width: u32, color: Color) {
        let width = width.min(16);
        let pixel = color.pixel();
        let bounds = self.bounds();

        for (row, bits) in rows.iter().copied().enumerate() {
            let y = at.1 + row as i32;
            if y < 0 || y >= bounds.h as i32 {
                continue;
            }
            for column in 0..width {
                if bits & (1 << (width - 1 - column)) == 0 {
                    continue;
                }
                let x = at.0 + column as i32;
                if x < 0 || x >= bounds.w as i32 {
                    continue;
                }
                let offset = (y as usize) * (self.stride as usize) + x as usize;
                // SAFETY: координаты обрезаны по границам экрана, а `stride *
                // height` пикселей проверено в `new` против заявленного размера
                // буфера. `write_volatile` — как и в `fill`.
                unsafe { self.base.add(offset).write_volatile(pixel) };
            }
        }
    }

    /// Вывести часть поверхности на экран.
    ///
    /// `dst` задаёт, куда на экране попадёт начало `src_rect` поверхности. Всё,
    /// что не попадает в экран, отсекается здесь — вызывающему обрезкой
    /// заниматься не нужно.
    pub fn blit(&self, surface: &Surface, dst: (i32, i32), src_rect: Rect) {
        let src = src_rect.intersect(&surface.bounds());
        if src.is_empty() {
            return;
        }
        // Прямоугольник на экране, куда это ляжет, и то, что от него осталось
        // после обрезки по краям.
        let placed = Rect::new(dst.0, dst.1, src.w, src.h);
        let visible = placed.intersect(&self.bounds());
        if visible.is_empty() {
            return;
        }
        // На сколько обрезка съела слева и сверху — на столько же сдвигается
        // начало чтения из поверхности.
        let skip_x = (visible.x - placed.x) as u32;
        let skip_y = (visible.y - placed.y) as u32;

        for row in 0..visible.h {
            let source = surface.row(src.y as u32 + skip_y + row);
            let from = (src.x as u32 + skip_x) as usize;
            let slice = &source[from..from + visible.w as usize];
            let start =
                ((visible.y as u32 + row) as usize) * (self.stride as usize) + visible.x as usize;
            // SAFETY: см. `fill`; и строка поверхности, и место на экране
            // обрезаны по своим границам выше, а копия идёт строкой — по той же
            // причине, по которой залив­ка перестала идти по пикселю.
            let target =
                unsafe { core::slice::from_raw_parts_mut(self.base.add(start), slice.len()) };
            target.copy_from_slice(slice);
        }
    }
}
