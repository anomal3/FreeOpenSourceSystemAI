//! Текстовая консоль поверх линейного фреймбуфера.
//!
//! Растровый шрифт взят из крейта `font8x8` (модуль `legacy`, таблица
//! `BASIC_LEGACY`): он без зависимостей, `no_std` и представляет собой просто
//! массив `[[u8; 8]; 128]` — по байту на строку глифа. Собственный шрифт писать
//! незачем, а тянуть `noto-sans-mono-bitmap` на Phase 1 избыточно: он на два
//! порядка больше и умеет то, что нам пока не нужно (несколько кеглей, Unicode).
//!
//! Прокрутка сделана без единого чтения из фреймбуфера. Сдвинуть картинку
//! «как есть» нельзя: чтение write-combining памяти устройства катастрофически
//! медленное, и именно поэтому скролла долго не было. Вместо этого консоль
//! держит теневой буфер символов в обычной памяти и перерисовывает экран из
//! него — фреймбуфер по-прежнему только пишется.
//!
//! Буфер живёт в куче, а `init` вызывается раньше, чем куча появляется: ядро
//! печатает баннер и карту памяти до того, как возьмёт память под контроль.
//! Поэтому режима два: до [`enable_scroll`] строки, не поместившиеся на экран,
//! отбрасываются, после — экран прокручивается. Если выделить буфер не
//! удалось, консоль остаётся в первом режиме: терять диагностику на экране
//! из-за нехватки памяти хуже, чем не иметь прокрутки.

use crate::sync::SpinLock;
use alloc::vec::Vec;
use boot_info::{Framebuffer, PixelFormat};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};
use font8x8::legacy::BASIC_LEGACY;
use mini_ui::typeface::{self, Face, Glyph};

/// Размер глифа в таблице `BASIC_LEGACY`.
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 8;

/// Шаг табуляции в символах.
const TAB_STOP: u32 = 8;

/// Отступ от края экрана, чтобы текст не лип к рамке монитора.
const MARGIN: u32 = 8;

/// Наибольший масштаб глифа: столько раз повторяется каждая точка шрифта.
///
/// Связан с лесенкой в [`Console::new`] и существует ради одного — размера
/// буфера строки развёртки ячейки. Поднять масштаб, не тронув эту константу,
/// значит получить обрезанный глиф, а не отказ; поэтому она названа здесь, а не
/// подразумевается.
const MAX_SCALE: u32 = 3;

/// Наибольшая ширина ячейки сглаженного шрифта (буфер строки на стеке).
const CELL_MAX: usize = 32;

/// Покрытие точки `(x, y)` ячейки глифом, 0..=255. `y` — от верха строки.
fn coverage(glyph: &Glyph, x: i32, y: i32) -> u32 {
    let gx = x - i32::from(glyph.left);
    let gy = y - i32::from(glyph.top);
    if gx < 0 || gy < 0 || gx >= i32::from(glyph.w) || gy >= i32::from(glyph.h) {
        return 0;
    }
    let stride = usize::from(glyph.w).div_ceil(2);
    let at = glyph.off as usize + gy as usize * stride + gx as usize / 2;
    let byte = typeface::data::COVERAGE.get(at).copied().unwrap_or(0);
    // Младшая тетрада — левая точка (так упаковывает генератор).
    let level = if gx % 2 == 0 { byte & 0x0f } else { byte >> 4 };
    u32::from(level) * 17
}

/// Покрытие глифа, увеличенного в `zoom` раз: билинейно между точками.
fn coverage_zoomed(glyph: &Glyph, tx: u32, ty: u32, zoom: u32) -> u32 {
    if zoom == 1 {
        return coverage(glyph, tx as i32, ty as i32);
    }
    // Центр точки цели в координатах глифа, в 1/256.
    let fx = ((tx * 2 + 1) * 128 / zoom) as i32 - 128;
    let fy = ((ty * 2 + 1) * 128 / zoom) as i32 - 128;
    let (sx, wx) = (fx.div_euclid(256), fx.rem_euclid(256) as u32);
    let (sy, wy) = (fy.div_euclid(256), fy.rem_euclid(256) as u32);
    let top = coverage(glyph, sx, sy) * (256 - wx) + coverage(glyph, sx + 1, sy) * wx;
    let bottom = coverage(glyph, sx, sy + 1) * (256 - wx) + coverage(glyph, sx + 1, sy + 1) * wx;
    (top * (256 - wy) + bottom * wy) >> 16
}

/// Смешать два упакованных пикселя: `level` из 255 — доля `fg`. Каналы
/// по байтам, поэтому порядок (RGB/BGR) не важен.
fn blend(bg: u32, fg: u32, level: u32) -> u32 {
    let level = level.min(255);
    let mut out = 0;
    for shift in [0, 8, 16] {
        let b = (bg >> shift) & 0xff;
        let f = (fg >> shift) & 0xff;
        out |= ((b * (255 - level) + f * level) / 255) << shift;
    }
    out
}

/// Значение ячейки «что на экране — неизвестно».
///
/// Ячейка — символ в младшем байте и номер цвета в старшем. Не может совпасть
/// ни с одной настоящей: символы — только от 0x20 до 0x7E (см.
/// `write_char_raw`), цветов — [`PALETTE`] штук.
const UNKNOWN_CELL: u16 = 0xFFFF;

/// Цвета в виде (R, G, B); в пиксель они упаковываются с учётом `PixelFormat`.
/// Фон — почти чёрный, тёмно-тёмно-синий (Роман 04.11: «не совсем чёрный»).
const BG: (u8, u8, u8) = (0x04, 0x07, 0x12);
const FG: (u8, u8, u8) = (0xD8, 0xE2, 0xEC); // светло-серый текст

/// Цвета текста по номерам: обычный, «OK» зелёным, «FAIL» красным, тусклый
/// (строки без тега), белый (надпись под логотипом).
const PALETTE: [(u8, u8, u8); 5] = [FG, (0x3D, 0xD6, 0x5C), (0xF0, 0x4A, 0x4A), (0x7C, 0x8A, 0x9C), (0xFF, 0xFF, 0xFF)];
const INK: u8 = 0;
const INK_OK: u8 = 1;
const INK_FAIL: u8 = 2;
const INK_DIM: u8 = 3;
const INK_WHITE: u8 = 4;

/// Шаг бегущей полосы, мс.
const MARQUEE_MS: u64 = 30;
/// Логотип над полосой загрузки: тот же, что у «Пуска» (RGBA, `FIMG`).
static LOGO: &[u8] = include_bytes!("../../../assets/logo/fossai-logo.fimg");
/// Сколько знаков строки журнала копится до перевода строки.
const LINE_MAX: usize = 200;

/// Всё, что нужно, чтобы положить точки на экран, — и ничего больше.
///
/// Вынесено из [`Console`] не ради красоты. Рисование идёт **вне** замка
/// консоли и с разрешёнными прерываниями, а состояние текста — под замком;
/// разделить их в коде надо было прежде, чем разделять во времени. Структура
/// маленькая и `Copy`: её выносят из-под замка целиком, одной копией.
#[derive(Clone, Copy)]
struct Painter {
    /// Адрес первого пикселя. Фреймбуфер 32-битный, поэтому `*mut u32`.
    base: *mut u32,
    width: u32,
    height: u32,
    /// Пикселей на строку развёртки; может быть больше `width`.
    stride: u32,
    /// Во сколько раз увеличен глиф 8x8 — только когда [`Painter::face`]
    /// пуст (надпись поверх стола, `overlay`).
    scale: u32,
    /// Сглаженный моноширинный шрифт (JetBrains Mono из `mini-ui`, 15.11:
    /// «пиксельный, как у XP — сделай сглаженнее»); `false` — растровый 8x8.
    ///
    /// Признак, а не ссылка на начертание: консоль заводится, пока ядро
    /// исполняется по тождественному отображению, и адрес статики, взятый
    /// тогда, после ухода в верхнюю половину указывал бы в снятые таблицы
    /// (15.11 — тройной сбой на первом же тике).
    smooth: bool,
    /// Верх текста: [`MARGIN`], а при заставке — середина экрана.
    top: u32,
    fg: u32,
    bg: u32,
    /// [`PALETTE`], упакованная под формат экрана.
    inks: [u32; 5],
}

/// Текстовая консоль, рисующая глифы прямо в фреймбуфер.
pub struct Console {
    /// Куда и чем рисовать.
    fb: Painter,
    cols: u32,
    rows: u32,
    col: u32,
    row: u32,
    /// Теневая копия экрана: `rows * cols` символов в обычной памяти. `None` —
    /// кучи ещё нет, прокрутка недоступна. Буфер существует ровно затем, чтобы
    /// при сдвиге строк не читать фреймбуфер.
    cells: Option<Vec<u16>>,
    /// Что сейчас **на экране**, ячейка в ячейку с [`Console::cells`].
    ///
    /// Два буфера вместо одного — это и есть разделение «чего мы хотим» и «что
    /// туда положено». Печать меняет первый и возвращается; второй догоняет его
    /// отдельно, уже с разрешёнными прерываниями. Разница между ними и есть
    /// работа, которую осталось сделать, — и она же переживает прокрутку сама
    /// собой: сдвиг строк в первом буфере это обычный `copy_within` в памяти.
    painted: Option<Vec<u16>>,
    /// С какой ячейки продолжать поиск отличий.
    ///
    /// Без него каждый поиск начинался бы с нуля, и перерисовка экрана стоила
    /// бы квадрата от числа ячеек вместо линейного прохода.
    paint_at: usize,
    /// На экране есть текст, которого нет в буфере — всё, что напечатано до
    /// [`Console::enable_scroll`]. Пока флаг взведён, буфер не описывает экран,
    /// поэтому сравнивать с ним нельзя; первая же прокрутка перерисует экран
    /// целиком и приведёт их в соответствие.
    stale: bool,
    /// Показывать ли курсор. Пока ядро только печатает, он не нужен и мешает:
    /// мигающая или просто лишняя черта под последней строкой лога выглядит как
    /// артефакт. Включается тогда, когда появляется ввод.
    cursor: bool,
    /// Нарисован ли курсор прямо сейчас. Отдельно от [`Console::cursor`], потому
    /// что перед каждой записью его надо снять, а после — вернуть, и путать
    /// «включён» с «на экране» значило бы оставлять след при каждом переводе
    /// строки.
    cursor_drawn: bool,
    /// Строк на весь экран — столько ячеек у буферов, сколько бы строк ни было
    /// видно сейчас (при заставке текст — только нижняя половина).
    full_rows: u32,
    /// Экран загрузки (04.11): сверху логотип, FOSSAI и полоса, снизу журнал
    /// строками `[  OK  ]` / `[FAIL]`. Снимается паникой и вводом.
    splash: bool,
    /// Шаг бегущей полосы загрузки.
    stage: u32,
    /// Когда полоса делала шаг (мс).
    stage_at: u64,
    /// Верх полосы загрузки.
    bar_y: u32,
    format: PixelFormat,
    /// Строка журнала, пока не пришёл её конец: при заставке строка
    /// выводится целиком, уже разобранной.
    line: [u8; LINE_MAX],
    line_len: usize,
}

// SAFETY: единственное, что мешает вывести `Send` автоматически, — сырой
// указатель `base`. Он адресует фреймбуфер: память устройства, не привязанную
// ни к какому потоку и не имеющую владельца, которого можно было бы бросить.
// Всё остальное состояние (`Vec` теневого буфера, счётчики) уже `Send`.
unsafe impl Send for Console {}
// SAFETY: та же причина. Копия рисовальщика уходит из-под замка и рисует на
// другом процессоре — это законно ровно потому, что она не владеет ничем,
// кроме адреса памяти устройства, и две копии, пишущие в разные ячейки, друг
// другу не мешают.
unsafe impl Send for Painter {}

impl Painter {
    /// Указатель на начало горизонтального отрезка, если он целиком на экране.
    ///
    /// Проверка границ делается **один раз на отрезок**, а не на точку, и в
    /// этом весь смысл: в отладочной сборке проверка с умножением стоила
    /// столько же, сколько сама запись, а отрезков на порядок меньше, чем
    /// точек.
    fn row_at(&self, x: u32, y: u32, len: u32) -> Option<*mut u32> {
        if len == 0 || y >= self.height || x >= self.width || x + len > self.width {
            return None;
        }
        let offset = (y as usize) * (self.stride as usize) + (x as usize);
        // SAFETY: `offset + len` не выходит за `stride * height` точек — это
        // проверено в `new` против заявленного `fb.size`, а x, y и длина отсечены
        // выше.
        Some(unsafe { self.base.add(offset) })
    }

    /// Записать готовый отрезок точек одной строки развёртки.
    ///
    /// Отрезок, не помещающийся на экране целиком, рисуется по точке: это
    /// редкий случай (край экрана), и ради него незачем усложнять быстрый путь.
    fn blit_row(&self, x: u32, y: u32, pixels: &[u32]) {
        let Some(row) = self.row_at(x, y, pixels.len() as u32) else {
            for (index, color) in pixels.iter().enumerate() {
                self.put_pixel(x + index as u32, y, *color);
            }
            return;
        };
        for (index, color) in pixels.iter().enumerate() {
            // SAFETY: отрезок проверен `row_at` целиком. Запись обязана быть
            // `write_volatile` по той же причине, что и в `put_pixel`.
            unsafe { row.add(index).write_volatile(*color) };
        }
    }

    /// Залить отрезок одним цветом.
    fn fill_row(&self, x: u32, y: u32, len: u32, color: u32) {
        let Some(row) = self.row_at(x, y, len) else {
            for index in 0..len {
                self.put_pixel(x + index, y, color);
            }
            return;
        };
        for index in 0..len as usize {
            // SAFETY: см. [`Console::blit_row`].
            unsafe { row.add(index).write_volatile(color) };
        }
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

    fn draw_cell(&self, col: u32, row: u32, cell: u16) {
        let byte = (cell & 0x7f) as u8;
        let fg = self.inks.get((cell >> 8) as usize).copied().unwrap_or(self.fg);
        // Ячейка — единица работы с фреймбуфером, и именно здесь машина глохнет.
        // Вывод идёт с запрещёнными прерываниями (см. [`crate::print::_print`]),
        // а прокрутка перерисовывает до тысячи ячеек: замерено — **663 мс** на
        // 1280x720 в отладочной сборке, то есть более полусекунды, за которые не
        // приходит ни тик таймера, ни байт из линии. Приёмного FIFO (шестнадцать
        // байт у 16550, тридцать два у PL011) хватает на полторы миллисекунды
        // линии, так что всё набранное в этот промежуток терялось молча.
        //
        // Приёмник вычитывается изнутри отрисовки — но только если прерывания
        // запрещены. При разрешённых байт заберёт обработчик, а лишнее чтение
        // регистра UART на каждую ячейку стоит дороже, чем кажется: это
        // обращение к устройству, и на тысяче ячеек их тысяча.
        //
        // Запрещены они теперь редко: с тех пор как рисование вынесено из
        // промежутка молчания, сюда попадают только ранняя загрузка (кучи ещё
        // нет, буфера тоже) и паника. Разбор при этом не делается, он отложен
        // до возврата из прерывания; почему — написано у `input::ascii::PENDING`.
        if !crate::arch::interrupts::enabled() {
            crate::input::ascii::rescue();
        }
        if let Some(face) = self.face() {
            self.draw_face_cell(face, col, row, byte, fg);
            return;
        }
        let glyph = BASIC_LEGACY[byte as usize];
        let x0 = MARGIN + col * GLYPH_W * self.scale;
        let y0 = self.top + row * GLYPH_H * self.scale;
        let width = (GLYPH_W * self.scale) as usize;
        // Строка развёртки ячейки собирается в обычной памяти и уходит в
        // фреймбуфер одним отрезком. Так каждая точка шрифта превращается в
        // цвет ровно один раз, а не `scale` раз, и на весь отрезок приходится
        // одна проверка границ вместо проверки на точку.
        let mut line = [0u32; (GLYPH_W * MAX_SCALE) as usize];
        for (gy, bits) in glyph.iter().copied().enumerate() {
            for gx in 0..GLYPH_W {
                // В font8x8 младший бит байта — самый ЛЕВЫЙ пиксель строки
                // (формат унаследован от C-заголовка font8x8_basic.h), поэтому
                // сдвигаем вправо на номер столбца, а не на 7 - столбец.
                let lit = (bits >> gx) & 1 != 0;
                let color = if lit { fg } else { self.bg };
                for sx in 0..self.scale {
                    line[(gx * self.scale + sx) as usize] = color;
                }
            }
            let top = y0 + gy as u32 * self.scale;
            for sy in 0..self.scale {
                self.blit_row(x0, top + sy, &line[..width]);
            }
        }
    }
}

impl Painter {
    /// Ширина ячейки текста в точках.
    /// Сглаженное начертание журнала (адрес берётся по месту, см. `smooth`).
    fn face(&self) -> Option<&'static Face> {
        self.smooth.then(|| &typeface::data::MONOLARGE)
    }

    fn cell_w(&self) -> u32 {
        match self.face() {
            Some(face) => typeface::cell(face).0,
            None => GLYPH_W * self.scale,
        }
    }

    /// Высота ячейки (строки) текста в точках.
    fn cell_h(&self) -> u32 {
        match self.face() {
            Some(face) => typeface::cell(face).1,
            None => GLYPH_H * self.scale,
        }
    }

    /// Ячейка сглаженным шрифтом: фон известен, поэтому покрытие смешивается
    /// с ним, а не с тем, что на экране, — из фреймбуфера не читается ничего.
    fn draw_face_cell(&self, face: &Face, col: u32, row: u32, byte: u8, fg: u32) {
        let (cw, ch) = typeface::cell(face);
        let x0 = MARGIN + col * cw;
        let y0 = self.top + row * ch;
        let width = (cw as usize).min(CELL_MAX);
        let glyph = if byte == b' ' { None } else { face.glyph(char::from(byte)) };
        let mut line = [0u32; CELL_MAX];
        for y in 0..ch {
            for (x, slot) in line[..width].iter_mut().enumerate() {
                let level = glyph.map_or(0, |glyph| coverage(glyph, x as i32, y as i32));
                *slot = if level == 0 { self.bg } else { blend(self.bg, fg, level) };
            }
            self.blit_row(x0, y0 + y, &line[..width]);
        }
    }

    /// Надпись сглаженным шрифтом с середины по ширине, верх строки — `y`,
    /// увеличенная в `zoom` раз с билинейным сглаживанием покрытия.
    fn big_text(&self, y: u32, face: &Face, text: &str, zoom: u32, ink: u32) {
        let zoom = zoom.max(1);
        let w = face.width(text) * zoom;
        let x0 = self.width.saturating_sub(w) / 2;
        let height = u32::from(face.line) * zoom;
        let mut pen = 0u32;
        let mut line = [0u32; 512];
        for ch in text.chars() {
            let Some(glyph) = face.glyph(ch) else { continue };
            let advance = u32::from(glyph.adv) * zoom;
            let span = (advance as usize).min(line.len());
            for ty in 0..height {
                for (tx, slot) in line[..span].iter_mut().enumerate() {
                    let level = coverage_zoomed(glyph, tx as u32, ty, zoom);
                    *slot = if level == 0 { self.bg } else { blend(self.bg, ink, level) };
                }
                self.blit_row(x0 + pen, y + ty, &line[..span]);
            }
            pen += advance;
        }
    }

    /// Логотип (`FIMG`: метка, ширина и высота по u16, RGBA) стороной `side`
    /// с центром по ширине, верх — `y`. Без кучи: строка — на стеке.
    fn logo(&self, y: u32, side: u32, bg: (u8, u8, u8), format: PixelFormat) {
        if LOGO.len() < 8 || &LOGO[..4] != b"FIMG" {
            return;
        }
        let w = u32::from(u16::from_le_bytes([LOGO[4], LOGO[5]]));
        let h = u32::from(u16::from_le_bytes([LOGO[6], LOGO[7]]));
        let side = side.min(512);
        if w == 0 || h == 0 || LOGO.len() < 8 + (w * h * 4) as usize || side == 0 {
            return;
        }
        let x0 = self.width.saturating_sub(side) / 2;
        let mut line = [0u32; 512];
        // Билинейная выборка с весами в 1/256 точки (15.11: при увеличении
        // ближайшим соседом логотип выходил «лесенкой»). Цвет берётся
        // премножённым на альфу, иначе по краю прозрачных точек ползёт кайма.
        let texel = |x: u32, y: u32| -> [u32; 4] {
            let at = 8 + ((y.min(h - 1) * w + x.min(w - 1)) * 4) as usize;
            let a = LOGO[at + 3] as u32;
            [LOGO[at] as u32 * a / 255, LOGO[at + 1] as u32 * a / 255, LOGO[at + 2] as u32 * a / 255, a]
        };
        for ty in 0..side {
            // Центр точки цели в координатах источника, в 1/256.
            let fy = ((ty * 2 + 1) * h * 128 / side).saturating_sub(128);
            let (sy, wy) = (fy >> 8, fy & 0xff);
            for tx in 0..side {
                let fx = ((tx * 2 + 1) * w * 128 / side).saturating_sub(128);
                let (sx, wx) = (fx >> 8, fx & 0xff);
                let (p00, p10, p01, p11) = (texel(sx, sy), texel(sx + 1, sy), texel(sx, sy + 1), texel(sx + 1, sy + 1));
                let mut c = [0u32; 4];
                for i in 0..4 {
                    let top = p00[i] * (256 - wx) + p10[i] * wx;
                    let bottom = p01[i] * (256 - wx) + p11[i] * wx;
                    c[i] = (top * (256 - wy) + bottom * wy) >> 16;
                }
                let a = c[3].min(255);
                let mix = |c: u32, back: u8| (c + back as u32 * (255 - a) / 255).min(255) as u8;
                line[tx as usize] = encode(format, (mix(c[0], bg.0), mix(c[1], bg.1), mix(c[2], bg.2)));
            }
            self.blit_row(x0, y + ty, &line[..side as usize]);
        }
    }

    /// Полоса загрузки в духе Windows XP (Marquee): три синих сегмента бегут
    /// по рамке слева направо и уходят за край, `phase` — шаг времени.
    /// Заполнение по этапам казалось зависшим (05.11): этапов мало, а между
    /// ними — секунды. С 15.11 сглаженная: скруглённая рамка со смешанными
    /// краями, сегменты едут по точкам, а не прыгают на свою ширину, и
    /// входят/выходят плавно (край сегмента смешивается с дорожкой).
    fn progress(&self, y: u32, width: u32, height: u32, phase: u32, format: PixelFormat) {
        let x0 = self.width.saturating_sub(width) / 2;
        let bg = encode(format, BG);
        let frame = encode(format, (0x4A, 0x55, 0x68));
        let track = encode(format, (0x0A, 0x10, 0x20));
        let pad = (height / 5).max(2);
        let seg_h = height.saturating_sub(pad * 2).max(1);
        let seg_w = (seg_h * 2 / 3).max(3);
        let gap = (seg_w / 3).max(1);
        let inner = width.saturating_sub(pad * 2);
        const RUN: u32 = 3;
        let run_w = RUN * (seg_w + gap);
        // Шаг — полсегмента раз в 30 мс: гладко, скорость почти прежняя.
        let step = (seg_w / 2).max(1);
        let lap = inner + run_w;
        let head = (phase * step) % lap.max(1);
        // Левый край группы относительно начала дорожки (может быть < 0).
        let group = head as i32 - run_w as i32;
        let radius = (height / 2).min(6) as i32;
        // Строка собирается кусками по 64 точки: полоса рисуется и из
        // прерывания таймера (`splash_tick`), где стек мал — буфер во всю
        // ширину (4 КиБ) его переполнял.
        const CHUNK: u32 = 64;
        let mut line = [0u32; CHUNK as usize];
        for row in 0..height {
            let shade = 255 - (row.saturating_sub(pad) * 110 / seg_h);
            let blue = encode(format, ((0x2E * shade / 255) as u8, (0x8C * shade / 255) as u8, (0xF5 * shade / 255) as u8));
            let in_seg_row = row >= pad && row < pad + seg_h;
            for col in 0..width {
                if col % CHUNK == 0 && col != 0 {
                    self.blit_row(x0 + col - CHUNK, y + row, &line);
                }
                // Рамка: скруглённый прямоугольник, покрытие края по расстоянию
                // до дуги угла (в 1/16 точки, без плавающей точки).
                let (cx, cy) = (col as i32, row as i32);
                let (w, h) = (width as i32, height as i32);
                let dx = if cx < radius { radius - cx } else if cx >= w - radius { cx - (w - radius - 1) } else { 0 };
                let dy = if cy < radius { radius - cy } else if cy >= h - radius { cy - (h - radius - 1) } else { 0 };
                let outer = if dx > 0 && dy > 0 {
                    // Расстояние центра точки до центра дуги против радиуса.
                    let d2 = (dx * 16 - 8) * (dx * 16 - 8) + (dy * 16 - 8) * (dy * 16 - 8);
                    let r = radius * 16;
                    if d2 <= (r - 16) * (r - 16) {
                        255
                    } else if d2 >= r * r {
                        0
                    } else {
                        let d = isqrt(d2 as u32) as i32;
                        ((r - d) * 255 / 16).clamp(0, 255) as u32
                    }
                } else {
                    255
                };
                let border = cx == 0 || cy == 0 || cx == w - 1 || cy == h - 1 || (dx > 0 && dy > 0 && outer < 255);
                let mut color = if border { frame } else { track };
                if in_seg_row && !border && cx >= pad as i32 && cx < (pad + inner) as i32 {
                    let x = cx - pad as i32 - group;
                    if x >= 0 && (x as u32) < run_w {
                        let within = x as u32 % (seg_w + gap);
                        if within < seg_w {
                            // Сегмент частично за краем дорожки — гасим у краёв.
                            let edge_l = cx - pad as i32;
                            let edge_r = (pad + inner) as i32 - 1 - cx;
                            let fade = (edge_l.min(edge_r).max(0) as u32 * 85).min(255);
                            color = blend(track, blue, fade.max(64));
                        }
                    }
                }
                line[(col % CHUNK) as usize] = if outer >= 255 { color } else { blend(bg, color, outer) };
            }
            let tail = match width % CHUNK {
                0 => CHUNK,
                rest => rest,
            };
            if width > 0 {
                self.blit_row(x0 + width - tail, y + row, &line[..tail as usize]);
            }
        }
    }
}

/// Целый квадратный корень (Ньютон) — для сглаживания углов.
fn isqrt(n: u32) -> u32 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

/// Как выглядит строка журнала на экране загрузки.
enum Shape<'a> {
    /// `---- имя ----` — новый этап: полоса делает шаг.
    Stage,
    /// `  тег : текст` — `[  OK  ]` или `[FAIL]`.
    Tagged { tag: &'a [u8], text: &'a [u8], failed: bool },
    /// Прочее — продолжение предыдущей строки, тусклым.
    Plain(&'a [u8]),
}

/// Слова отказа в строке журнала (английские: так пишет ядро).
const FAIL_WORDS: [&[u8]; 12] = [
    b"fail", b"error", b"could not", b"did not", b"cannot", b"can't", b"unreachable", b"refused", b"panic", b"timed out", b"not respond",
    b"broken",
];

fn shape(line: &[u8]) -> Shape<'_> {
    let trimmed = trim(line);
    if trimmed.starts_with(b"----") {
        return Shape::Stage;
    }
    if line.starts_with(b"  ") {
        if let Some(at) = line.windows(3).position(|w| w == b" : ").or_else(|| line.windows(2).position(|w| w == b": ")) {
            let tag = trim(&line[..at]);
            let text = trim(&line[(at + 2).min(line.len())..]);
            // Тег — до 14 знаков, бывает и из двух слов (`serial in`).
            if !tag.is_empty() && tag.len() <= 14 && tag.windows(2).all(|w| w != b"  ") {
                let failed = FAIL_WORDS.iter().any(|word| contains_ci(text, word));
                return Shape::Tagged { tag, text, failed };
            }
        }
    }
    Shape::Plain(trimmed)
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| *b != b' ').unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| *b != b' ').map_or(start, |at| at + 1);
    &bytes[start..end.max(start)]
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b))
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

        // Сглаженный JetBrains Mono (15.11, Роман: «пиксельный — сделай
        // сглаженнее, шрифт чуть меньше, на ПК лог не влезает»): крупный кегль
        // из `mini-ui` — 10x22 точки на ячейку вместо прежних 24x24 (8x8 втрое).
        // Растровый 8x8 остаётся запасным для надписи поверх стола.
        let scale = if fb.width >= 1200 { 3 } else { 2 };
        let face: &'static Face = &typeface::data::MONOLARGE;
        let (cell_w, cell_h) = typeface::cell(face);
        let usable_w = fb.width.saturating_sub(MARGIN * 2);
        let usable_h = fb.height.saturating_sub(MARGIN * 2);
        let cols = usable_w / cell_w;
        let rows = usable_h / cell_h;
        if cols == 0 || rows == 0 {
            return None;
        }

        let console = Self {
            fb: Painter {
                base: fb.base as *mut u32,
                width: fb.width,
                height: fb.height,
                stride: fb.stride,
                scale,
                smooth: true,
                top: MARGIN,
                fg: encode(fb.format, FG),
                bg: encode(fb.format, BG),
                inks: [
                    encode(fb.format, PALETTE[0]),
                    encode(fb.format, PALETTE[1]),
                    encode(fb.format, PALETTE[2]),
                    encode(fb.format, PALETTE[3]),
                    encode(fb.format, PALETTE[4]),
                ],
            },
            cols,
            rows,
            col: 0,
            row: 0,
            cells: None,
            painted: None,
            paint_at: 0,
            stale: false,
            cursor: false,
            cursor_drawn: false,
            full_rows: rows,
            splash: false,
            stage: 0,
            stage_at: 0,
            bar_y: 0,
            format: fb.format,
            line: [0; LINE_MAX],
            line_len: 0,
        };
        // Заливки здесь нет: её делает [`init`]. Консоль, заведённая после смены
        // режима ([`adopt`]), рисовать не вправе — экран в это время у стола.
        Some(console)
    }

    /// Залить весь экран фоном ядра.
    ///
    /// Это же и визуальное доказательство, что рисует ядро: тестовый паттерн
    /// загрузчика исчезает целиком.
    fn clear(&mut self) {
        for y in 0..self.fb.height {
            // Заливка экрана — это миллион записей в память устройства.
            // Заливка идёт из `init` и из паники, то есть как раз там, где
            // прерывания запрещены; строка развёртки — достаточно мелкий шаг,
            // чтобы приёмник не переполнился между ними.
            if !crate::arch::interrupts::enabled() {
                crate::input::ascii::rescue();
            }
            self.fb.fill_row(0, y, self.fb.width, self.fb.bg);
        }
        self.col = 0;
        self.row = 0;
        if let Some(cells) = self.cells.as_mut() {
            cells.fill(u16::from(b' '));
        }
        // Экран только что залит фоном, то есть на нём ровно пробелы: второй
        // буфер обязан сказать то же самое, иначе первая же покраска
        // перерисовала бы весь экран без нужды.
        if let Some(painted) = self.painted.as_mut() {
            painted.fill(u16::from(b' '));
        }
        self.paint_at = 0;
        self.stale = false;
        self.cursor_drawn = false;
        // Очистка — это и конец заставки: паника и консоль для ввода пишут
        // на весь экран обычным текстом.
        self.splash = false;
        self.line_len = 0;
        self.fb.top = MARGIN;
        self.rows = self.full_rows;
    }

    /// Нарисовать заставку и отвести журналу нижнюю половину экрана.
    fn start_splash(&mut self) {
        let (w, h) = (self.fb.width, self.fb.height);
        let cell_h = self.fb.cell_h();
        let side = (w.min(h) / 5).clamp(64, 256);
        let title_face: &'static Face = &typeface::data::HEADINGLARGE;
        let title = if w >= 2560 { 3 } else { 2 };
        let title_h = u32::from(title_face.line) * title;
        let (_, _, bar_h) = self.bar_geometry();
        // Логотип, под ним FOSSAI, под ним полоса — одним блоком над
        // серединой экрана; журнал — под серединой.
        let block = side + side / 6 + title_h + cell_h * 2 + bar_h;
        let logo_y = (h / 2).saturating_sub(block + cell_h);
        self.fb.logo(logo_y, side, BG, self.format);
        let title_y = logo_y + side + side / 6;
        self.fb.big_text(title_y, title_face, "FOSSAI", title, self.fb.inks[INK_WHITE as usize]);
        self.bar_y = title_y + title_h + cell_h * 2;
        self.splash = true;
        self.stage = 0;
        self.draw_progress();
        // Журнал — ниже середины экрана и не выше её.
        let top = (h / 2).max(self.bar_y + bar_h) + cell_h;
        self.fb.top = top;
        self.rows = (h.saturating_sub(top + MARGIN) / cell_h).clamp(1, self.full_rows);
        self.row = 0;
        self.col = 0;
    }

    fn bar_geometry(&self) -> (u32, u32, u32) {
        let (w, h) = (self.fb.width, self.fb.height);
        let _ = h;
        let width = (w * 2 / 5).clamp(160, 520);
        let height = (self.fb.cell_h() + 4).max(14);
        (self.bar_y, width, height)
    }

    fn draw_progress(&self) {
        let (y, width, height) = self.bar_geometry();
        self.fb.progress(y, width, height, self.stage, self.format);
    }

    /// Строка журнала пришла целиком: разобрать и положить в ячейки.
    fn emit_line(&mut self) {
        let mut line = [0u8; LINE_MAX];
        let len = self.line_len;
        line[..len].copy_from_slice(&self.line[..len]);
        self.line_len = 0;
        // Длинная строка переносится с отступом под текст после тега (15.11:
        // на ПК журнал не влезал по ширине и обрезался).
        const INDENT: u32 = 9;
        let put = |console: &mut Self, text: &[u8], ink: u8| {
            for byte in text {
                if console.col >= console.cols && console.cols > INDENT * 2 {
                    console.newline();
                    while console.col < INDENT && console.row < console.rows {
                        console.put_cell(console.col, console.row, u16::from(b' '));
                        console.col += 1;
                    }
                }
                if console.col >= console.cols || console.row >= console.rows {
                    return;
                }
                console.put_cell(console.col, console.row, (u16::from(ink) << 8) | u16::from(*byte));
                console.col += 1;
            }
        };
        match shape(&line[..len]) {
            // Этап — просто строка журнала: полоса бежит сама, от таймера
            // ([`splash_tick`]).
            Shape::Stage => return,
            Shape::Tagged { tag, text, failed } => {
                put(self, b"[", INK);
                if failed {
                    put(self, b"FAIL", INK_FAIL);
                } else {
                    put(self, b"  OK  ", INK_OK);
                }
                put(self, b"] ", INK);
                put(self, tag, INK);
                put(self, b": ", INK);
                put(self, text, INK);
            }
            Shape::Plain(text) => {
                if text.is_empty() {
                    return;
                }
                put(self, b"         ", INK);
                put(self, text, INK_DIM);
            }
        }
        // Остаток строки — пробелами: строка, ушедшая прокруткой, не оставляет хвоста.
        while self.col < self.cols && self.row < self.rows {
            self.put_cell(self.col, self.row, u16::from(b' '));
            self.col += 1;
        }
        self.newline();
    }

    /// Выделить теневой буфер и включить прокрутку.
    ///
    /// Возвращает `false`, если памяти не хватило: консоль при этом остаётся
    /// работоспособной в режиме без прокрутки.
    fn enable_scroll(&mut self) -> bool {
        if self.cells.is_some() {
            return true;
        }
        let len = (self.full_rows as usize) * (self.cols as usize);
        let mut cells = Vec::new();
        let mut painted = Vec::new();
        // `try_reserve_exact` вместо `vec![]`: отказ аллокатора обязан вернуться
        // ошибкой, а не уйти в `handle_alloc_error` и уронить ядро.
        if cells.try_reserve_exact(len).is_err() || painted.try_reserve_exact(len).is_err() {
            return false;
        }
        cells.resize(len, u16::from(b' '));
        // «На экране» начинается с того же, с чего «хотим»: текст, напечатанный
        // до этого вызова, на экране есть, но ни одному из буферов не известен.
        // Считать его пробелами — ровно то, что делалось и раньше: он доживает
        // до первой прокрутки и исчезает вместе с ней.
        painted.resize(len, u16::from(b' '));
        self.cells = Some(cells);
        self.painted = Some(painted);
        self.paint_at = 0;
        // Текст, напечатанный до этого момента, в буфер не попал.
        self.stale = self.row != 0 || self.col != 0;
        // Экран мог уже кончиться (в режиме без прокрутки `row` вырастает до
        // `rows` и служит признаком «дальше не рисуем»); возвращаем курсор в
        // последнюю строку, иначе прокручивать будет нечего.
        self.row = self.row.min(self.rows - 1);
        true
    }

    /// Сдвинуть текст на строку вверх; курсор остаётся в последней строке.
    ///
    /// Раньше здесь был самый дорогой кусок ядра: прокрутка перерисовывала до
    /// тысячи ячеек, то есть сотни тысяч записей в память устройства, и всё это
    /// время (замерено — до 0.9 с) машина не принимала ни байта и не считала
    /// тиков. Теперь прокрутка — это `copy_within` в обычной памяти; на экран
    /// изменения переносит [`paint`], уже с разрешёнными прерываниями.
    fn scroll(&mut self) {
        let Some(cells) = self.cells.as_mut() else {
            return;
        };
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let last = (rows - 1) * cols;
        cells.copy_within(cols..rows * cols, 0);
        cells[last..rows * cols].fill(u16::from(b' '));

        if self.stale {
            // На экране есть символы, которых не знает ни один буфер, — всё,
            // что напечатано до появления кучи. Сравнивать не с чем, поэтому
            // экран объявляется неизвестным целиком: покраска перерисует его
            // весь, ровно как делала прежняя прокрутка в этом же случае.
            // Только видимые строки: остальные покраска не обходит, и
            // «неизвестно» там осталось бы навсегда (04.11 — вечная покраска).
            if let Some(painted) = self.painted.as_mut() {
                painted[..rows * cols].fill(UNKNOWN_CELL);
            }
            self.stale = false;
        }
        self.paint_at = 0;
        self.col = 0;
        self.row = self.rows - 1;
    }

    /// Найти следующую ячейку, которая на экране не такая, как в тексте.
    ///
    /// Поиск продолжается с того места, где остановился прошлый, и обходит
    /// буфер по кругу ровно один раз: перерисовка экрана стоит одного прохода,
    /// а не квадрата от числа ячеек.
    ///
    /// Ячейка отмечается покрашенной **до** того, как будет нарисована. Это
    /// намеренно: если между отметкой и рисованием кто-то напечатает в ту же
    /// ячейку другое, тексты снова разойдутся, и следующий проход это увидит.
    /// Обратный порядок потерял бы такую запись молча.
    fn next_dirty(&mut self) -> Option<(u32, u32, u16)> {
        let cols = self.cols as usize;
        let len = cols * self.rows as usize;
        let cells = self.cells.as_ref()?;
        let painted = self.painted.as_mut()?;
        for step in 0..len {
            let index = (self.paint_at + step) % len;
            let want = cells[index];
            if painted[index] == want {
                continue;
            }
            painted[index] = want;
            self.paint_at = (index + 1) % len;
            return Some(((index % cols) as u32, (index / cols) as u32, want));
        }
        None
    }

    /// Есть ли что красить.
    fn dirty(&self) -> bool {
        let len = (self.cols as usize) * (self.rows as usize);
        match (self.cells.as_ref(), self.painted.as_ref()) {
            (Some(cells), Some(painted)) => cells[..len] != painted[..len],
            _ => false,
        }
    }

    /// Положить символ в ячейку текста.
    ///
    /// На экран он попадёт не отсюда: пока буфер есть, печать только правит
    /// текст, а разницу между текстом и экраном переносит [`paint`]. Это и есть
    /// разделение, ради которого всё затевалось, — печать возвращается за
    /// микросекунды вместо сотен миллисекунд.
    ///
    /// Буфера может не быть: до появления кучи (баннер, карта памяти) рисовать
    /// больше некому и нечем, и там символ уходит на экран сразу.
    fn put_cell(&mut self, col: u32, row: u32, cell: u16) {
        let Some(cells) = self.cells.as_mut() else {
            self.fb.draw_cell(col, row, cell);
            return;
        };
        cells[(row as usize) * (self.cols as usize) + (col as usize)] = cell;
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else if self.cells.is_some() {
            self.scroll();
        } else {
            // Буфера нет — прокручивать нечем. `row == rows` означает
            // «экран кончился»: остаток вывода виден только на serial.
            self.row = self.rows;
        }
    }

    /// Табуляция до следующей позиции, кратной [`TAB_STOP`].
    ///
    /// Рисуется пробелами: иначе `\t` ушёл бы в таблицу шрифта как код 0x09 и
    /// превратился в случайный глиф.
    fn tab(&mut self) {
        let stop = (self.col / TAB_STOP + 1) * TAB_STOP;
        if stop >= self.cols {
            self.newline();
            return;
        }
        if self.row >= self.rows {
            return;
        }
        while self.col < stop {
            self.put_cell(self.col, self.row, u16::from(b' '));
            self.col += 1;
        }
    }

    /// Нарисовать курсор: подчёркивание под текущей ячейкой.
    ///
    /// Подчёркивание, а не заливка ячейки: блок скрыл бы символ под собой, а
    /// курсор в редакторе строки стоит именно там, где только что напечатан
    /// символ, — и видеть его полезнее, чем видеть курсор.
    fn draw_cursor(&mut self) {
        if !self.cursor || self.cursor_drawn || self.row >= self.rows || self.col >= self.cols {
            return;
        }
        let (cw, ch) = (self.fb.cell_w(), self.fb.cell_h());
        let thick = (ch / 8).max(2);
        let x0 = MARGIN + self.col * cw;
        let y0 = self.fb.top + (self.row + 1) * ch - thick;
        for y in 0..thick {
            self.fb.fill_row(x0, y0 + y, cw, self.fb.fg);
        }
        self.cursor_drawn = true;
    }

    /// Убрать курсор, восстановив то, что было под ним.
    fn erase_cursor(&mut self) {
        if !self.cursor_drawn {
            return;
        }
        self.cursor_drawn = false;
        if self.row >= self.rows || self.col >= self.cols {
            return;
        }
        match self.cells.as_ref() {
            // Буфер знает, какой символ стоит в ячейке, — перерисовываем её
            // целиком, и след курсора исчезает вместе с фоном.
            Some(cells) => {
                let idx = (self.row as usize) * (self.cols as usize) + (self.col as usize);
                let byte = cells[idx];
                self.fb.draw_cell(self.col, self.row, byte);
            }
            // Буфера нет: что было под курсором, неизвестно. Затираем только саму
            // полоску фоном — нижний ряд пикселей глифа при этом пострадает, но
            // это единственный вариант, не стирающий символ целиком.
            None => {
                let (cw, ch) = (self.fb.cell_w(), self.fb.cell_h());
                let thick = (ch / 8).max(2);
                let x0 = MARGIN + self.col * cw;
                let y0 = self.fb.top + (self.row + 1) * ch - thick;
                for y in 0..thick {
                    self.fb.fill_row(x0, y0 + y, cw, self.fb.bg);
                }
            }
        }
    }

    fn write_char_raw(&mut self, ch: char) {
        if self.splash {
            // Заставка: строка копится и выходит целиком, разобранной.
            match ch {
                '\n' => self.emit_line(),
                '\r' => {}
                _ if self.line_len < LINE_MAX => {
                    let byte = if (0x20..0x7F).contains(&(ch as u32)) { ch as u8 } else if ch == '\t' { b' ' } else { b'?' };
                    self.line[self.line_len] = byte;
                    self.line_len += 1;
                }
                _ => {}
            }
            return;
        }
        match ch {
            '\n' => {
                self.newline();
                return;
            }
            '\r' => {
                self.col = 0;
                return;
            }
            '\t' => {
                self.tab();
                return;
            }
            // Возврат на позицию. Символ под курсором не стирается — так же, как
            // в любом терминале: стирание делает последовательность
            // «возврат, пробел, возврат», и решать, стирать ли, обязан тот, кто
            // печатает, а не консоль.
            '\u{8}' => {
                self.col = self.col.saturating_sub(1);
                return;
            }
            _ => {}
        }
        // Перенос по правому краю: строка длиннее экрана продолжается снизу и
        // может утащить за собой прокрутку, поэтому проверка идёт до `row`.
        if self.col >= self.cols {
            self.newline();
        }
        if self.row >= self.rows {
            return;
        }
        // Таблица покрывает только ASCII; всё остальное показываем как '?',
        // чтобы не молчать о потерянном символе.
        let byte = if (0x20..0x7F).contains(&(ch as u32)) { ch as u8 } else { b'?' };
        self.put_cell(self.col, self.row, u16::from(byte));
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
///
/// # Про старший байт
///
/// В описании UEFI четвёртый байт назван зарезервированным, и прошивка его не
/// смотрит. У телефона он значит **прозрачность**, потому что кадровый буфер там
/// — слой наложения, который контроллер смешивает с фоном. Ноль в нём означает
/// «полностью прозрачно», то есть чёрный экран при исправно идущей отрисовке:
/// заставка загрузчика стирается, а на её месте не появляется ничего. На
/// аппарате это выглядело как неработающая графика, а не работал один байт.
/// Единица во все восемь бит верна на обеих машинах: там, где байт
/// зарезервирован, его никто не читает.
const fn encode(format: PixelFormat, (r, g, b): (u8, u8, u8)) -> u32 {
    let (r, g, b) = (r as u32, g as u32, b as u32);
    match format {
        PixelFormat::Rgb => OPAQUE | r | (g << 8) | (b << 16),
        PixelFormat::Bgr => OPAQUE | b | (g << 8) | (r << 16),
        // До сюда не доходим: `Console::new` отвергает неизвестный формат.
        PixelFormat::Unknown => 0,
    }
}

/// Старший байт точки: непрозрачно. См. [`encode`].
const OPAQUE: u32 = 0xFF00_0000;

static CONSOLE: SpinLock<Option<Console>> = SpinLock::new(None);

/// Отладка на аппарате (05.11): последние строки журнала — прямо поверх
/// экрана стола, мимо композитора. Телефон, повисший целиком (NetSurf),
/// оставляет на экране последний кадр — а с этим режимом и последние строки
/// ядра, которые можно прочитать глазами. Включает `oem overlay on`.
pub static OVERLAY: AtomicBool = AtomicBool::new(false);
const OV_ROWS: usize = 12;
const OV_COLS: usize = 88;

struct Overlay {
    lines: [[u8; OV_COLS]; OV_ROWS],
    next: usize,
    cur: [u8; OV_COLS],
    len: usize,
}

static OV: SpinLock<Overlay> =
    SpinLock::new(Overlay { lines: [[b' '; OV_COLS]; OV_ROWS], next: 0, cur: [b' '; OV_COLS], len: 0 });

impl Write for Overlay {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            if ch == '\n' {
                self.lines[self.next] = self.cur;
                self.next = (self.next + 1) % OV_ROWS;
                self.cur = [b' '; OV_COLS];
                self.len = 0;
            } else if self.len < OV_COLS {
                self.cur[self.len] = if (0x20..0x7F).contains(&(ch as u32)) { ch as u8 } else { b'?' };
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Дописать строку в полосу поверх экрана и перерисовать полосу.
fn overlay_print(args: fmt::Arguments<'_>) {
    let Some(mut ov) = OV.try_lock() else {
        return;
    };
    let _ = ov.write_fmt(args);
    draw_overlay(&ov);
}

/// Стол перерисовал кусок экрана: если он задел полосу журнала — вернуть её
/// поверх (иначе последний кадр зависшего телефона её затирал, 05.11).
pub fn overlay_redraw(y: i32, h: u32) {
    if !OVERLAY.load(Ordering::Relaxed) || y > (OV_TOP + OV_ROWS as u32 * GLYPH_H * 2 + 8) as i32 || y + h as i32 <= OV_TOP as i32 {
        return;
    }
    let Some(ov) = OV.try_lock() else {
        return;
    };
    draw_overlay(&ov);
}

/// Строка в полосу из обработчика прерывания: без кучи и без чужих замков
/// (только `try_lock` полосы и консоли), куски склеиваются здесь.
pub fn overlay_note(parts: &[&str]) {
    if !OVERLAY.load(Ordering::Relaxed) {
        return;
    }
    let Some(mut ov) = OV.try_lock() else {
        return;
    };
    for part in parts {
        let _ = ov.write_str(part);
    }
    let _ = ov.write_str("\n");
    draw_overlay(&ov);
}

/// Верх полосы журнала, в точках.
const OV_TOP: u32 = 100;

fn draw_overlay(ov: &Overlay) {
    let painter = CONSOLE.try_lock().and_then(|console| console.as_ref().map(|console| console.fb));
    let Some(mut painter) = painter else {
        return;
    };
    painter.top = OV_TOP;
    // Крупно: строки читают с фотографии экрана.
    painter.scale = 2;
    painter.smooth = false;
    let cols = (painter.width.saturating_sub(MARGIN * 2) / (GLYPH_W * painter.scale)).min(OV_COLS as u32);
    for row in 0..OV_ROWS {
        let line = ov.lines[(ov.next + row) % OV_ROWS];
        for col in 0..cols {
            painter.draw_cell(col, row as u32, (u16::from(INK_OK) << 8) | u16::from(line[col as usize]));
        }
    }
}
static READY: AtomicBool = AtomicBool::new(false);

/// Инициализировать экранную консоль. Возвращает `true`, если экран доступен.
pub fn init(fb: &Framebuffer) -> bool {
    let Some(mut console) = Console::new(fb) else {
        return false;
    };
    console.clear();
    console.start_splash();
    *CONSOLE.lock() = Some(console);
    READY.store(true, Ordering::Release);
    true
}

/// Перевести консоль с identity-адреса буфера прошивки на прямое отображение —
/// перед снятием identity (ядро в верхней половине, x86-64).
#[cfg(target_arch = "x86_64")]
pub fn move_to_direct_map() {
    if let Some(console) = CONSOLE.lock().as_mut() {
        let base = console.fb.base as usize;
        if base < crate::mm::PHYS_MAP_BASE {
            console.fb.base = (base + crate::mm::PHYS_MAP_BASE) as *mut u32;
        }
    }
}

/// Перевести консоль на новый фреймбуфер после смены режима (фаза С6a).
///
/// Экран в это время принадлежит столу, поэтому ничего не рисуется. Нужна
/// замена одному — панике: она забирает экран через [`reclaim_screen`] и иначе
/// рисовала бы по геометрии прежнего режима, строками чужой ширины, а на
/// `ramfb` ещё и в буфер, который устройство больше не показывает и который
/// уже вернулся в пул кадров.
pub fn adopt(fb: &Framebuffer) {
    let console = Console::new(fb).map(|mut console| {
        // Буфер выделяется до захвата замка: порядок «консоль, затем куча»
        // разрешён, но держать консоль на время выделения незачем.
        let _ = console.enable_scroll();
        console
    });
    *CONSOLE.lock() = console;
}

/// Включить прокрутку экранной консоли.
///
/// Вызывается ядром один раз, сразу после инициализации кучи: до неё выделить
/// теневой буфер не из чего. Повторные вызовы безвредны. Возвращает `true`,
/// если прокрутка работает, и `false`, если экрана нет или память под буфер
/// выделить не удалось — во втором случае консоль продолжает печатать в старом
/// режиме, отбрасывая строки за нижним краем.
///
/// Текст, напечатанный до вызова, в буфере не отражён и исчезнет при первой
/// прокрутке (он остаётся в логе serial).
pub fn enable_scroll() -> bool {
    if !READY.load(Ordering::Acquire) {
        return false;
    }
    // Единственное место, где под локом консоли выделяется память, то есть
    // берётся ещё и лок кучи. Порядок захвата здесь только такой — консоль,
    // затем куча; обратного не существует, потому что куча печатает свою
    // диагностику уже после того, как отпустит себя (см. `mm::heap`). А если
    // памяти не хватит, сообщение об этом уйдёт в serial и молча пропустит
    // экран: `_print` ниже не ждёт занятого лока.
    let mut console = CONSOLE.lock();
    let enabled = console.as_mut().is_some_and(Console::enable_scroll);
    drop(console);
    // См. `set_cursor`: замок брали мы, значит и докрашивать нам.
    paint();
    enabled
}

/// Точка входа макросов вывода. Не вызывать напрямую.
///
/// # Почему занятый лок означает отказ от вывода, а не ожидание
///
/// Обработчик отказа печатает откуда угодно, в том числе из кода, который прямо
/// сейчас держит лок консоли (например из-под [`enable_scroll`]). `lock()`
/// подвесил бы такой обработчик навсегда. Обойти лок, как это делает serial,
/// здесь нельзя: у консоли есть настоящее изменяемое состояние — позиция
/// курсора и теневой буфер, — и вторая ссылка на него была бы не «перемешанным
/// выводом», а неопределённым поведением с испорченным экраном впридачу.
///
/// Поэтому при занятом локе экранная копия просто теряется. Тот же текст уже
/// ушёл в serial — основной канал диагностики, — и потерять его дубликат
/// несопоставимо дешевле, чем зависнуть.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    if !READY.load(Ordering::Acquire) {
        if OVERLAY.load(Ordering::Relaxed) {
            overlay_print(args);
        }
        return;
    }
    let Some(mut console) = CONSOLE.try_lock() else {
        return;
    };
    if let Some(console) = console.as_mut() {
        // Курсор снимается до вывода и возвращается после. Иначе он остался бы
        // нарисованным там, где текст уже уехал дальше, — и на экране копилась
        // бы дорожка подчёркиваний.
        console.erase_cursor();
        let _ = console.write_fmt(args);
        console.draw_cursor();
    }
}

/// Один рисующий за раз.
///
/// Красить можно откуда угодно: печать зовёт покраску из обычного кода, а
/// обработчик прерывания печатает и тоже зовёт. Двое рисующих поделили бы
/// ячейки пополам и оба решили бы, что экран чист. Признак дешевле замка и,
/// главное, **не ждёт**: опоздавший просто уходит, а его грязь докрасит тот,
/// кто уже внутри.
static PAINTING: AtomicBool = AtomicBool::new(false);

/// Шаг бегущей полосы экрана загрузки — из тика таймера (не чаще
/// [`MARQUEE_MS`]). Без ожиданий: замок занят — шаг пропускается.
pub fn splash_tick() {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    let Some(mut guard) = CONSOLE.try_lock() else {
        return;
    };
    let Some(console) = guard.as_mut() else {
        return;
    };
    if !console.splash {
        return;
    }
    let now = crate::time::uptime_ms();
    if now.saturating_sub(console.stage_at) < MARQUEE_MS {
        return;
    }
    console.stage_at = now;
    console.stage = console.stage.wrapping_add(1);
    console.draw_progress();
}

/// Перенести на экран то, что печать уже записала в текст.
///
/// Здесь и происходит вся дорогая работа, и здесь же она законна: функция
/// зовётся **вне** промежутка с запрещёнными прерываниями, и между ячейками
/// машина полностью жива — идут часы, приходит ввод, работает вытеснение.
///
/// Замок консоли берётся на одну ячейку: под ним только чтение буфера, а
/// рисование — без него. Держать его на весь проход значило бы вернуть ту же
/// глухоту под другим именем.
pub fn paint() {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    while PAINTING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        paint_pass();
        PAINTING.store(false, Ordering::Release);
        // Между последней проверкой и снятием признака сосед мог напечатать и
        // уйти, решив, что докрасим мы. Без этой проверки его строка висела бы
        // ненарисованной до следующей чужой печати.
        if !dirty() {
            return;
        }
    }
}

/// Один проход: красить, пока есть что.
///
/// Замок берётся и отпускается **на каждую ячейку**. Не «на проход» — тогда
/// вернулась бы та же глухота, только называлась бы иначе; и не «отпустить и
/// рисовать без него» — адрес экрана из-под замка уносить нельзя. Смена режима
/// ([`adopt`]) подменяет консоль целиком, а у `ramfb` прежний буфер после этого
/// возвращается в пул кадров: рисующий по унесённой копии писал бы в чужую
/// память. Замок консоли — ровно то, что разводит эти два действия, и терять
/// его нельзя даже на одну ячейку.
///
/// Прерывания при этом запрещены на время одной ячейки — около двухсот
/// пятидесяти записей, — а между ячейками машина полностью жива.
fn paint_pass() {
    loop {
        let Some(mut guard) = CONSOLE.try_lock() else {
            return;
        };
        let Some(console) = guard.as_mut() else {
            return;
        };
        // Экран мог уйти композитору, пока мы красили предыдущую ячейку.
        if !READY.load(Ordering::Acquire) {
            return;
        }
        let Some((col, row, byte)) = console.next_dirty() else {
            // Красить нечего — самое время вернуть курсор. Его могла стереть
            // перерисовка символа в той же ячейке (см. ниже), и без этого он
            // пропал бы ровно тогда, когда нужен: система ждёт ввода и не
            // печатает, а значит и не перерисовывает.
            console.draw_cursor();
            return;
        };
        if console.cursor_drawn && row == console.row && col == console.col {
            // Под курсором сейчас будет перерисован символ, и подчёркивание
            // исчезнет вместе с ним. Признак снимается заранее: иначе консоль
            // считала бы курсор нарисованным, а его бы не было.
            console.cursor_drawn = false;
        }
        console.fb.draw_cell(col, row, byte);
    }
}

/// Расходится ли текст с экраном.
fn dirty() -> bool {
    // Экран ушёл композитору — красить больше нечего, сколько бы текст ни
    // расходился с последней покраской. Без этой строки [`paint`] крутился
    // вечно: проход возвращается сразу (`READY` снят), а «грязно» остаётся
    // правдой навсегда. Так 27.09.2026 нашёлся процессор 1, застрявший с
    // закрытыми прерываниями в печати «cpu 1 online»: в отладочной сборке
    // покраска долгая, и композитор забирал экран посреди неё. Снаружи это
    // выглядело как процессор, не получивший ни одного тика, — а нашлось
    // тогда, когда сброс трансляций (фаза 56) стал ждать ответа от каждого.
    if !READY.load(Ordering::Acquire) {
        return false;
    }
    let Some(console) = CONSOLE.try_lock() else {
        // Замок занят — значит кто-то печатает, и он же позовёт покраску.
        return false;
    };
    console.as_ref().is_some_and(Console::dirty)
}

/// Отдать экран композитору.
///
/// После этого [`kprintln!`](crate::kprintln) продолжает писать в serial, но
/// перестаёт рисовать на экране: там теперь окна, и чужой текст поверх них — это
/// испорченная картинка. Загрузочный лог при этом не теряется, он весь в serial.
pub fn release_screen() {
    set_cursor(false);
    READY.store(false, Ordering::Release);
}

/// Вернуть экран загрузочной консоли, очистив его.
///
/// Нужно в двух случаях: композитор не поднялся (и тогда консоль остаётся
/// единственным выводом) и паника (и тогда сообщение важнее картинки, которая
/// всё равно больше не изменится). В обоих экран очищается: печатать поверх
/// нарисованных окон значит смешать текст с рамками, а восстановить то, что было
/// на экране до композитора, всё равно нечем — фреймбуфер не читается.
///
/// Ничего не делает, если экрана нет или консоль занята: вызов бывает из
/// обработчика паники, а там ждать нельзя.
pub fn reclaim_screen() {
    let Some(mut console) = CONSOLE.try_lock() else {
        return;
    };
    if let Some(console) = console.as_mut() {
        console.clear();
        // Теневой буфер после очистки описывает пустой экран, поэтому режим
        // «на экране есть то, чего буфер не знает» надо снять — иначе первая же
        // прокрутка перерисовала бы экран целиком без нужды.
        console.stale = false;
        READY.store(true, Ordering::Release);
    }
    drop(console);
    // Экран только что залит фоном, и вернуть на него текст должен тот, кто
    // его забрал. Зовётся это из паники — там прерывания запрещены, и покраска
    // пойдёт синхронно; так и надо, сообщение важнее отзывчивости.
    paint();
}

/// Показывать или не показывать курсор.
///
/// Вызывается, когда ядро начинает ждать ввод, и снимается, когда перестаёт: до
/// появления ввода курсор под последней строкой лога — просто артефакт.
pub fn set_cursor(visible: bool) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    // `try_lock`, а не `lock`: функция вызывается из обычного кода, но замок
    // консоли может быть занят выводом из обработчика. Не показать курсор —
    // косметическая потеря, ждать освобождения в такой ситуации дороже.
    let Some(mut console) = CONSOLE.try_lock() else {
        return;
    };
    if let Some(console) = console.as_mut() {
        // Ждут ввода на консоли (безопасный режим, стол не поднялся) —
        // заставка уходит, экран — обычному тексту.
        if visible && console.splash {
            console.clear();
        }
        console.cursor = visible;
        if visible {
            console.draw_cursor();
        } else {
            console.erase_cursor();
        }
    }
    drop(console);
    // Покраску зовёт печать, но замок консоли держит не только она. Красящий,
    // наткнувшийся на занятый замок, уходит и рассчитывает, что докрасит
    // державший, — значит, докрасить обязан каждый, кто этот замок брал.
    // Иначе последняя строка осталась бы ненарисованной ровно до следующей
    // печати, то есть до тех пор, пока человек чего-нибудь не наберёт.
    paint();
}
