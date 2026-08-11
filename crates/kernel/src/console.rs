//! Текстовая консоль поверх линейного фреймбуфера.
//!
//! Растровый шрифт взят из крейта `font8x8` (модуль `legacy`, таблица
//! `BASIC_LEGACY`): он без зависимостей, `no_std` и представляет собой просто
//! массив `[[u8; 8]; 128]` — по байту на строку глифа. Собственный шрифт писать
//! незачем, а тянуть `noto-sans-mono-bitmap` на Phase 1 избыточно: он на два
//! порядка больше и умеет то, что нам пока не нужно (несколько кеглей, Unicode).
//!
//! Скролла нет намеренно: вывод Phase 1 умещается в экран, а корректный скролл
//! требует чтения из фреймбуфера, которое на write-combining памяти
//! катастрофически медленное. Строки, не поместившиеся на экран, отбрасываются.

use crate::sync::Racy;
use boot_info::{Framebuffer, PixelFormat};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use font8x8::legacy::BASIC_LEGACY;

/// Размер глифа в таблице `BASIC_LEGACY`.
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 8;

/// Отступ от края экрана, чтобы текст не лип к рамке монитора.
const MARGIN: u32 = 8;

/// Цвета в виде (R, G, B); в пиксель они упаковываются с учётом `PixelFormat`.
const BG: (u8, u8, u8) = (0x0A, 0x1C, 0x2E); // тёмно-синий фон ядра
const FG: (u8, u8, u8) = (0xD8, 0xE2, 0xEC); // светло-серый текст

/// Текстовая консоль, рисующая глифы прямо в фреймбуфер.
pub struct Console {
    /// Адрес первого пикселя. Фреймбуфер 32-битный, поэтому `*mut u32`.
    base: *mut u32,
    width: u32,
    height: u32,
    /// Пикселей на строку развёртки; может быть больше `width`.
    stride: u32,
    /// Во сколько раз увеличен глиф — 8x8 на экране 1024+ читается плохо.
    scale: u32,
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
    fg: u32,
    bg: u32,
}

impl Console {
    /// Создать консоль по описанию фреймбуфера от загрузчика.
    ///
    /// Возвращает `None`, если фреймбуфера нет, формат пикселя неизвестен или
    /// геометрия не выдерживает даже одного символа.
    fn new(fb: &Framebuffer) -> Option<Self> {
        if !fb.is_present() {
            return None;
        }
        // Неизвестный порядок каналов означает, что рисовать мы будем не тем
        // цветом, и что 32 бита на пиксель — тоже лишь предположение. Безопаснее
        // не трогать такой фреймбуфер вовсе.
        if fb.format == PixelFormat::Unknown {
            return None;
        }
        // Геометрия приходит из-за границы доверия: проверяем, что заявленный
        // размер действительно вмещает stride * height 32-битных пикселей.
        let needed = u64::from(fb.stride) * u64::from(fb.height) * 4;
        if fb.width == 0 || fb.height == 0 || fb.stride < fb.width || needed > fb.size {
            return None;
        }

        let scale = if fb.width >= 1600 {
            3
        } else if fb.width >= 1024 {
            2
        } else {
            1
        };
        let cell_w = GLYPH_W * scale;
        let cell_h = GLYPH_H * scale;
        let usable_w = fb.width.saturating_sub(MARGIN * 2);
        let usable_h = fb.height.saturating_sub(MARGIN * 2);
        let cols = usable_w / cell_w;
        let rows = usable_h / cell_h;
        if cols == 0 || rows == 0 {
            return None;
        }

        let mut console = Self {
            base: fb.base as *mut u32,
            width: fb.width,
            height: fb.height,
            stride: fb.stride,
            scale,
            cols,
            rows,
            col: 0,
            row: 0,
            fg: encode(fb.format, FG),
            bg: encode(fb.format, BG),
        };
        console.clear();
        Some(console)
    }

    /// Залить весь экран фоном ядра.
    ///
    /// Это же и визуальное доказательство, что рисует ядро: тестовый паттерн
    /// загрузчика исчезает целиком.
    fn clear(&mut self) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.put_pixel(x, y, self.bg);
            }
        }
        self.col = 0;
        self.row = 0;
    }

    fn put_pixel(&self, x: u32, y: u32, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let offset = (y as usize) * (self.stride as usize) + (x as usize);
        // SAFETY: `offset` не выходит за stride * height пикселей — это проверено
        // в `new` против заявленного `fb.size`, а x/y отсечены выше. Фреймбуфер
        // — это память устройства, поэтому запись обязана быть `write_volatile`:
        // обычную запись компилятор вправе выбросить или объединить, решив, что
        // никто не читает результат, — и на экране ничего бы не появилось.
        unsafe { self.base.add(offset).write_volatile(color) };
    }

    fn draw_glyph(&mut self, byte: u8) {
        let glyph = BASIC_LEGACY[byte as usize];
        let x0 = MARGIN + self.col * GLYPH_W * self.scale;
        let y0 = MARGIN + self.row * GLYPH_H * self.scale;
        for (gy, bits) in glyph.iter().copied().enumerate() {
            for gx in 0..GLYPH_W {
                // В font8x8 младший бит байта — самый ЛЕВЫЙ пиксель строки
                // (формат унаследован от C-заголовка font8x8_basic.h), поэтому
                // сдвигаем вправо на номер столбца, а не на 7 - столбец.
                let lit = (bits >> gx) & 1 != 0;
                let color = if lit { self.fg } else { self.bg };
                for sy in 0..self.scale {
                    for sx in 0..self.scale {
                        let px = x0 + gx * self.scale + sx;
                        let py = y0 + gy as u32 * self.scale + sy;
                        self.put_pixel(px, py, color);
                    }
                }
            }
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row += 1;
    }

    fn write_char_raw(&mut self, ch: char) {
        if ch == '\n' {
            self.newline();
            return;
        }
        if ch == '\r' {
            self.col = 0;
            return;
        }
        if self.row >= self.rows {
            return; // экран кончился, скролла нет
        }
        if self.col >= self.cols {
            self.newline();
            if self.row >= self.rows {
                return;
            }
        }
        // Таблица покрывает только ASCII; всё остальное показываем как '?',
        // чтобы не молчать о потерянном символе.
        let byte = if (0x20..0x7F).contains(&(ch as u32)) { ch as u8 } else { b'?' };
        self.draw_glyph(byte);
        self.col += 1;
    }
}

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            self.write_char_raw(ch);
        }
        Ok(())
    }
}

/// Упаковать (R, G, B) в 32-битный пиксель согласно порядку каналов.
///
/// Фреймбуфер little-endian: младший байт слова лежит по меньшему адресу.
/// `PixelFormat::Rgb` означает «байт 0 — красный», то есть красный попадает в
/// младшие 8 бит слова. Перепутать местами Rgb и Bgr — классическая ошибка,
/// после которой синий интерфейс становится красным.
const fn encode(format: PixelFormat, (r, g, b): (u8, u8, u8)) -> u32 {
    let (r, g, b) = (r as u32, g as u32, b as u32);
    match format {
        PixelFormat::Rgb => r | (g << 8) | (b << 16),
        PixelFormat::Bgr => b | (g << 8) | (r << 16),
        // До сюда не доходим: `Console::new` отвергает неизвестный формат.
        PixelFormat::Unknown => 0,
    }
}

static CONSOLE: Racy<Option<Console>> = Racy::new(None);
static READY: AtomicBool = AtomicBool::new(false);

/// Инициализировать экранную консоль. Возвращает `true`, если экран доступен.
pub fn init(fb: &Framebuffer) -> bool {
    let Some(console) = Console::new(fb) else {
        return false;
    };
    // SAFETY: однопоточное исполнение с выключенными прерываниями, повторных
    // входов нет — эксклюзивность доступа к глобальной консоли обеспечена.
    unsafe { *CONSOLE.get() = Some(console) };
    READY.store(true, Ordering::Release);
    true
}

/// Точка входа макросов вывода. Не вызывать напрямую.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: см. `init`.
    let slot = unsafe { &mut *CONSOLE.get() };
    if let Some(console) = slot.as_mut() {
        let _ = console.write_fmt(args);
    }
}
