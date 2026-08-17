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

/// Размер глифа в таблице `BASIC_LEGACY`.
const GLYPH_W: u32 = 8;
const GLYPH_H: u32 = 8;

/// Шаг табуляции в символах.
const TAB_STOP: u32 = 8;

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
    /// Теневая копия экрана: `rows * cols` символов в обычной памяти. `None` —
    /// кучи ещё нет, прокрутка недоступна. Буфер существует ровно затем, чтобы
    /// при сдвиге строк не читать фреймбуфер.
    cells: Option<Vec<u8>>,
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
}

// SAFETY: единственное, что мешает вывести `Send` автоматически, — сырой
// указатель `base`. Он адресует фреймбуфер: память устройства, не привязанную
// ни к какому потоку и не имеющую владельца, которого можно было бы бросить.
// Всё остальное состояние (`Vec` теневого буфера, счётчики) уже `Send`.
unsafe impl Send for Console {}

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
            cells: None,
            stale: false,
            cursor: false,
            cursor_drawn: false,
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
        if let Some(cells) = self.cells.as_mut() {
            cells.fill(b' ');
        }
        self.stale = false;
        self.cursor_drawn = false;
    }

    /// Выделить теневой буфер и включить прокрутку.
    ///
    /// Возвращает `false`, если памяти не хватило: консоль при этом остаётся
    /// работоспособной в режиме без прокрутки.
    fn enable_scroll(&mut self) -> bool {
        if self.cells.is_some() {
            return true;
        }
        let len = (self.rows as usize) * (self.cols as usize);
        let mut cells = Vec::new();
        // `try_reserve_exact` вместо `vec![]`: отказ аллокатора обязан вернуться
        // ошибкой, а не уйти в `handle_alloc_error` и уронить ядро.
        if cells.try_reserve_exact(len).is_err() {
            return false;
        }
        cells.resize(len, b' ');
        self.cells = Some(cells);
        // Текст, напечатанный до этого момента, в буфер не попал.
        self.stale = self.row != 0 || self.col != 0;
        // Экран мог уже кончиться (в режиме без прокрутки `row` вырастает до
        // `rows` и служит признаком «дальше не рисуем»); возвращаем курсор в
        // последнюю строку, иначе прокручивать будет нечего.
        self.row = self.row.min(self.rows - 1);
        true
    }

    /// Сдвинуть экран на строку вверх; курсор остаётся в последней строке.
    ///
    /// Прокрутка стоит дорого: на 1280x800 при `scale = 2` экран — это 79x49
    /// ячеек по 16x16 пикселей, то есть около миллиона записей в фреймбуфер на
    /// полную перерисовку. Поэтому перерисовываются только ячейки, содержимое
    /// которых после сдвига изменилось; на типичном выводе ядра (короткие
    /// строки, много пробелов справа) это примерно половина экрана.
    fn scroll(&mut self) {
        // Буфер вынимается из `self` на время работы: одолженная ссылка на поле
        // не даёт вызывать методы рисования, которым нужен весь `&self`.
        let Some(mut cells) = self.cells.take() else {
            return;
        };
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let last = (rows - 1) * cols;

        if self.stale {
            // Сравнивать не с чем: на экране есть символы, которых буфер не
            // знает. Единственный корректный вариант — перерисовать всё.
            cells.copy_within(cols.., 0);
            cells[last..].fill(b' ');
            for row in 0..rows {
                for col in 0..cols {
                    self.draw_cell(col as u32, row as u32, cells[row * cols + col]);
                }
            }
            self.stale = false;
        } else {
            // Сдвиг и отрисовка одним проходом сверху вниз: строка `row` читает
            // строку `row + 1`, до которой проход ещё не дошёл. В фреймбуфер
            // уходят только те ячейки, содержимое которых действительно
            // изменилось, — сравнение идёт в обычной памяти и стоит на порядки
            // дешевле лишней записи в память устройства.
            for row in 0..rows - 1 {
                for col in 0..cols {
                    let src = cells[(row + 1) * cols + col];
                    let dst = row * cols + col;
                    if cells[dst] != src {
                        cells[dst] = src;
                        self.draw_cell(col as u32, row as u32, src);
                    }
                }
            }
            for col in 0..cols {
                if cells[last + col] != b' ' {
                    cells[last + col] = b' ';
                    self.draw_cell(col as u32, (rows - 1) as u32, b' ');
                }
            }
        }

        self.cells = Some(cells);
        self.col = 0;
        self.row = self.rows - 1;
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

    fn draw_cell(&self, col: u32, row: u32, byte: u8) {
        let glyph = BASIC_LEGACY[byte as usize];
        let x0 = MARGIN + col * GLYPH_W * self.scale;
        let y0 = MARGIN + row * GLYPH_H * self.scale;
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

    /// Положить символ в ячейку экрана и в теневой буфер.
    ///
    /// Если ячейка уже показывает этот символ, запись в фреймбуфер не делается
    /// вовсе — при прокрутке совпадений набирается много.
    fn put_cell(&mut self, col: u32, row: u32, byte: u8) {
        if let Some(cells) = self.cells.as_mut() {
            let idx = (row as usize) * (self.cols as usize) + (col as usize);
            if !self.stale && cells[idx] == byte {
                return;
            }
            cells[idx] = byte;
        }
        self.draw_cell(col, row, byte);
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
            self.put_cell(self.col, self.row, b' ');
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
        let x0 = MARGIN + self.col * GLYPH_W * self.scale;
        let y0 = MARGIN + (self.row + 1) * GLYPH_H * self.scale - self.scale;
        for y in 0..self.scale {
            for x in 0..GLYPH_W * self.scale {
                self.put_pixel(x0 + x, y0 + y, self.fg);
            }
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
                self.draw_cell(self.col, self.row, byte);
            }
            // Буфера нет: что было под курсором, неизвестно. Затираем только саму
            // полоску фоном — нижний ряд пикселей глифа при этом пострадает, но
            // это единственный вариант, не стирающий символ целиком.
            None => {
                let x0 = MARGIN + self.col * GLYPH_W * self.scale;
                let y0 = MARGIN + (self.row + 1) * GLYPH_H * self.scale - self.scale;
                for y in 0..self.scale {
                    for x in 0..GLYPH_W * self.scale {
                        self.put_pixel(x0 + x, y0 + y, self.bg);
                    }
                }
            }
        }
    }

    fn write_char_raw(&mut self, ch: char) {
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
        self.put_cell(self.col, self.row, byte);
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
static READY: AtomicBool = AtomicBool::new(false);

/// Инициализировать экранную консоль. Возвращает `true`, если экран доступен.
pub fn init(fb: &Framebuffer) -> bool {
    let Some(console) = Console::new(fb) else {
        return false;
    };
    *CONSOLE.lock() = Some(console);
    READY.store(true, Ordering::Release);
    true
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
    console.as_mut().is_some_and(Console::enable_scroll)
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
        console.cursor = visible;
        if visible {
            console.draw_cursor();
        } else {
            console.erase_cursor();
        }
    }
}
