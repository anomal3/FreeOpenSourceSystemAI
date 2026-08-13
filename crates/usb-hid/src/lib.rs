//! Разбор HID Report Descriptor: как читать устройство, которое не говорит на
//! boot-протоколе.
//!
//! # Почему без этого не обойтись
//!
//! Boot protocol — это соглашение, введённое ради BIOS: клавиатура обязана уметь
//! отдавать восемь байт известного вида, мышь — три. Соглашение действует, пока
//! устройство объявляет подкласс «boot interface», и ровно этим ограничивался
//! драйвер до сих пор.
//!
//! Первая же чужая машина показала цену такого сужения. VirtualBox предлагает в
//! качестве манипулятора **планшет**, а планшет boot-протокола не объявляет
//! вовсе: он сообщает не смещение, а точку, и договориться о трёх байтах с ним
//! нельзя. Мыши в системе не было и не могло появиться — не оттого, что железо
//! незнакомое, а оттого, что ядро отказывалось читать единственный документ,
//! которым устройство себя описывает.
//!
//! # Что такое сам дескриптор
//!
//! Поток коротких элементов (HID 1.11, глава 6.2.2): байт-префикс, за ним от
//! нуля до четырёх байт значения. Префикс несёт тип (Main, Global, Local), тег и
//! длину. Global-элементы задают состояние, которое действует до следующего
//! такого же (страница usage, размеры полей, границы значений), Local-элементы
//! живут до ближайшего Main, а Main-элемент `Input` **и есть** объявление полей:
//! «взять текущие размеры и назначения и уложить их в отчёт подряд».
//!
//! Отсюда устройство разбора: пройти поток один раз, поддерживая состояние, и на
//! каждом `Input` откладывать битовые смещения тех полей, которые ядру нужны.
//! Результат — [`Descriptor`]: где в отчёте лежит X, где Y, где кнопки, и
//! абсолютны координаты или относительны.
//!
//! # Чего здесь намеренно нет
//!
//! Полный разбор HID — это подсистема, умеющая описать джойстик с шляпкой,
//! датчик освещённости и весы. Ядру нужны две вещи: указатель и клавиатура,
//! поэтому берутся ровно те usage, из которых они складываются, а всё остальное
//! пропускается вместе со своими битами. Пропустить поле здесь **обязательно**
//! именно вместе с битами: смещение следующего поля считается от начала отчёта,
//! и незнакомое поле, выброшенное без учёта его размера, сдвинуло бы всё, что за
//! ним, — то есть превратило бы разбор в правдоподобную бессмыслицу.
//!
//! Не поддержаны и **разные отчёты одного устройства сверх первого с каждым
//! идентификатором**: если дескриптор описывает несколько отчётов с разными
//! `Report ID`, ядро запоминает первый, в котором нашло нужные поля. Устройства
//! с несколькими отчётами (тач-панели, комбайны «мышь плюс медиаклавиши») от
//! этого теряют вторую половину, но не первую.
//!
//! # Почему это отдельный крейт, а не модуль ядра
//!
//! Потому что здесь ошибаются в битах, а не в железе. Смещение поля, сдвинутое
//! на один бит, даёт курсор, который ездит наискось, — и найти это, глядя на
//! экран в эмуляторе, стоит вечера. Вынесенный крейт проверяется на хосте
//! обычным `cargo test`, дескрипторами настоящих устройств, включая те, которых
//! в QEMU нет вовсе (планшет VirtualBox, устройство с `Report ID`, джойстик,
//! который курсором быть не должен).
//!
//! Отсюда же и отсутствие зависимостей: кнопки уезжают наружу битовой картой, а
//! не типом подсистемы ввода. Крейт не знает ни про экран, ни про очередь
//! событий — только про то, что устройство сказало о себе.

#![no_std]

// Тесты живут на хосте. Объявление обязательно: в `no_std`-крейте `std` не
// подключается сам даже там, где он доступен.
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

/// Страницы usage, которые ядро различает (HID Usage Tables, глава 3).
const PAGE_GENERIC_DESKTOP: u16 = 0x01;
const PAGE_KEYBOARD: u16 = 0x07;
const PAGE_BUTTON: u16 = 0x09;
const PAGE_DIGITIZER: u16 = 0x0D;

/// Usage страницы Generic Desktop.
const USAGE_POINTER: u16 = 0x01;
const USAGE_MOUSE: u16 = 0x02;
const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_WHEEL: u16 = 0x38;

/// Первый и последний usage клавиш-модификаторов на странице Keyboard.
const USAGE_MODIFIER_FIRST: u16 = 0xE0;
const USAGE_MODIFIER_LAST: u16 = 0xE7;

/// Типы элементов в префиксе.
const ITEM_TYPE_MAIN: u8 = 0;
const ITEM_TYPE_GLOBAL: u8 = 1;
const ITEM_TYPE_LOCAL: u8 = 2;

/// Теги Main-элементов.
const MAIN_INPUT: u8 = 8;
const MAIN_COLLECTION: u8 = 10;
const MAIN_END_COLLECTION: u8 = 12;

/// Теги Global-элементов.
const GLOBAL_USAGE_PAGE: u8 = 0;
const GLOBAL_LOGICAL_MIN: u8 = 1;
const GLOBAL_LOGICAL_MAX: u8 = 2;
const GLOBAL_REPORT_SIZE: u8 = 7;
const GLOBAL_REPORT_ID: u8 = 8;
const GLOBAL_REPORT_COUNT: u8 = 9;
const GLOBAL_PUSH: u8 = 10;
const GLOBAL_POP: u8 = 11;

/// Теги Local-элементов.
const LOCAL_USAGE: u8 = 0;
const LOCAL_USAGE_MIN: u8 = 1;
const LOCAL_USAGE_MAX: u8 = 2;

/// Признаки в значении `Input` (HID 1.11, 6.2.2.5).
const INPUT_CONSTANT: u32 = 1 << 0;
const INPUT_VARIABLE: u32 = 1 << 1;
const INPUT_RELATIVE: u32 = 1 << 2;

/// Префикс длинного элемента. Такие элементы определены спецификацией, но не
/// используются ни одним известным устройством; пропускаются по объявленной
/// длине, а не разбираются.
const LONG_ITEM_PREFIX: u8 = 0xFE;

/// Сколько usage подряд ядро согласно запомнить до ближайшего Main-элемента.
///
/// Шестнадцати хватает: дескрипторы перечисляют кнопки диапазоном
/// (`Usage Minimum`/`Usage Maximum`), а поштучно называют единицы полей — X, Y,
/// колесо. Устройство, назвавшее больше, потеряет хвост списка, и это лучше
/// массива на стеке размером с отчёт.
const MAX_USAGES: usize = 16;

/// Сколько уровней `Push` ядро согласно запомнить.
const MAX_PUSH_DEPTH: usize = 4;

/// Предел на число элементов в потоке — страховка от испорченного дескриптора.
const MAX_ITEMS: usize = 1024;

/// Сколько кнопок помещается в битовую карту, которую крейт отдаёт наружу.
///
/// Восемь: столько их в байте. Устройство вправе объявить больше (у игровых
/// мышей их дюжина), и лишние молча не читаются — назначить им смысл всё равно
/// некому.
const MAX_BUTTONS: u8 = 8;

/// Поле отчёта: где лежит и как читается.
#[derive(Clone, Copy, Debug)]
pub struct Field {
    /// Смещение в битах от начала данных отчёта (то есть **после** байта
    /// `Report ID`, если он есть).
    offset: u16,
    /// Размер поля в битах.
    size: u8,
    /// Нижняя граница значения. Отрицательная означает, что поле знаковое, —
    /// другого признака знаковости у HID нет.
    logical_min: i32,
    /// Верхняя граница значения.
    logical_max: i32,
    /// Поле сообщает приращение, а не положение.
    relative: bool,
}

impl Field {
    /// Прочитать поле из отчёта.
    ///
    /// Порядок бит в HID — от младшего: поле, начинающееся на седьмом бите и
    /// длиной в два, состоит из старшего бита нулевого байта и младшего первого.
    /// Поэтому цикл по битам, а не чтение слова со сдвигом: слово пришлось бы
    /// читать за концом отчёта у последнего поля.
    ///
    /// Отчёт короче поля — не ошибка разбора, а короткий пакет от устройства;
    /// недостающие биты считаются нулями, и это ровно то, что означает
    /// «устройство не сообщило».
    fn read(&self, report: &[u8]) -> i32 {
        let mut raw = 0u32;
        for bit in 0..u16::from(self.size) {
            let index = usize::from(self.offset + bit);
            let Some(byte) = report.get(index / 8) else {
                break;
            };
            if byte >> (index % 8) & 1 != 0 {
                raw |= 1 << bit;
            }
        }

        // Знак восстанавливается только у поля, чья нижняя граница отрицательна.
        // Смещение мыши объявлено −127..127 и обязано читаться со знаком, а
        // координата планшета — 0..32767, и «знаковое расширение» превратило бы
        // правую половину экрана в отрицательные числа.
        if self.logical_min < 0 && self.size < 32 && raw >> (self.size - 1) & 1 != 0 {
            return (raw | (u32::MAX << self.size)) as i32;
        }
        raw as i32
    }

    /// Значение, приведённое к долям диапазона: 0 — начало, 65535 — конец.
    ///
    /// Нужно абсолютным координатам: драйвер не знает размера экрана и не должен
    /// его знать (см. [`crate::input::PointerEvent`]), а диапазон планшета — его
    /// собственное дело, у одного он 0..32767, у другого 0..4095.
    fn fraction(&self, report: &[u8]) -> u16 {
        let value = i64::from(self.read(report));
        let min = i64::from(self.logical_min);
        let max = i64::from(self.logical_max);
        if max <= min {
            return 0;
        }
        let clamped = value.clamp(min, max);
        ((clamped - min) * i64::from(u16::MAX) / (max - min)) as u16
    }
}

/// Карта отчёта указателя.
#[derive(Clone, Copy, Debug)]
pub struct PointerMap {
    /// Идентификатор отчёта; ноль означает, что устройство их не использует и
    /// байта с номером в отчёте нет.
    pub report_id: u8,
    x: Field,
    y: Field,
    wheel: Option<Field>,
    /// Смещение первой кнопки и сколько их объявлено.
    buttons: Option<(u16, u8)>,
}

impl PointerMap {
    /// Абсолютны ли координаты. Свойство пары: устройство, у которого X
    /// абсолютен, а Y относителен, не встречается, а если встретится — считаем
    /// его абсолютным, потому что так объявлен X.
    #[must_use]
    pub const fn is_absolute(&self) -> bool {
        !self.x.relative
    }

    /// Диапазон координаты X — только для диагностики.
    #[must_use]
    pub const fn range(&self) -> (i32, i32) {
        (self.x.logical_min, self.x.logical_max)
    }

    /// Сколько кнопок объявлено.
    #[must_use]
    pub fn button_count(&self) -> u8 {
        self.buttons.map_or(0, |(_, count)| count)
    }

    /// Есть ли колесо.
    #[must_use]
    pub const fn has_wheel(&self) -> bool {
        self.wheel.is_some()
    }

    /// Разобрать отчёт: движение, колесо, кнопки.
    ///
    /// `None` означает «отчёт не от этого поля»: у устройства с несколькими
    /// отчётами каждый начинается со своего идентификатора, и разбирать чужой по
    /// нашей карте значило бы выдавать нажатия кнопок из чего попало.
    #[must_use]
    pub fn decode(&self, report: &[u8]) -> Option<PointerReport> {
        let data = strip_report_id(self.report_id, report)?;
        let motion = if self.is_absolute() {
            Motion::Absolute { x: self.x.fraction(data), y: self.y.fraction(data) }
        } else {
            Motion::Relative { dx: self.x.read(data), dy: self.y.read(data) }
        };
        Some(PointerReport {
            motion,
            wheel: self.read_wheel(data),
            buttons: self.read_buttons(data),
        })
    }

    /// Состояние кнопок в отчёте: бит 0 — первая кнопка (левая).
    fn read_buttons(&self, report: &[u8]) -> u8 {
        let Some((offset, count)) = self.buttons else {
            return 0;
        };
        let mut bits = 0u8;
        for index in 0..count.min(MAX_BUTTONS) {
            let bit = usize::from(offset + u16::from(index));
            let Some(byte) = report.get(bit / 8) else {
                break;
            };
            if byte >> (bit % 8) & 1 != 0 {
                bits |= 1 << index;
            }
        }
        bits
    }

    /// Колесо: приращение, ноль при его отсутствии.
    fn read_wheel(&self, report: &[u8]) -> i32 {
        self.wheel.map_or(0, |field| field.read(report))
    }
}

/// Что сообщил один отчёт указателя.
#[derive(Clone, Copy, Debug)]
pub struct PointerReport {
    pub motion: Motion,
    /// Колесо: приращение, ноль при отсутствии колеса.
    pub wheel: i32,
    /// Битовая карта кнопок: бит 0 — левая, 1 — правая, 2 — средняя. Ровно тот
    /// порядок, в котором их перечисляет страница usage `Button`, и тот же, в
    /// котором их ждёт boot-протокол.
    pub buttons: u8,
}

/// Как устройство сообщает о положении.
///
/// Разница не в единицах, а в смысле, и поэтому это два разных варианта, а не
/// пара чисел с флагом: мышь говорит «сдвинулись на столько», планшет — «палец
/// вот здесь». Свести одно к другому в драйвере нельзя: чтобы превратить точку в
/// приращение, надо знать, где курсор, а это знает тот, кто его рисует.
#[derive(Clone, Copy, Debug)]
pub enum Motion {
    /// Приращение с прошлого отчёта.
    Relative { dx: i32, dy: i32 },
    /// Положение в долях диапазона устройства: 0 — начало, 65535 — конец.
    Absolute { x: u16, y: u16 },
}

/// Отделить идентификатор отчёта от данных.
///
/// Ноль означает, что устройство идентификаторами не пользуется и байта с
/// номером в отчёте нет вовсе. Чужой номер — не ошибка: у устройства с
/// несколькими отчётами они приходят по одной и той же конечной точке
/// вперемешку.
fn strip_report_id(id: u8, report: &[u8]) -> Option<&[u8]> {
    if id == 0 {
        return Some(report);
    }
    match report.split_first() {
        Some((first, rest)) if *first == id => Some(rest),
        _ => None,
    }
}

/// Карта отчёта клавиатуры.
#[derive(Clone, Copy, Debug)]
pub struct KeyboardMap {
    pub report_id: u8,
    /// Смещение восьми бит модификаторов: `LeftCtrl` первым, как в
    /// boot-протоколе, — потому что порядок задан таблицей usage, а не выбором
    /// устройства.
    modifiers: Option<u16>,
    /// Массив нажатых клавиш: смещение, сколько их и по сколько бит каждая.
    keys: Option<(u16, u8, u8)>,
}

impl KeyboardMap {
    /// Сколько клавиш помещается в один отчёт.
    #[must_use]
    pub fn key_slots(&self) -> u8 {
        self.keys.map_or(0, |(_, count, _)| count)
    }

    /// Есть ли байт модификаторов.
    #[must_use]
    pub const fn has_modifiers(&self) -> bool {
        self.modifiers.is_some()
    }

    /// Собрать из отчёта то же, что прислала бы boot-клавиатура: байт
    /// модификаторов и до шести usage ID.
    ///
    /// Приведение к общему виду сделано здесь намеренно. Разбор отчёта
    /// клавиатуры — это сравнение с предыдущим состоянием, и писать его дважды
    /// (для boot-протокола и для произвольного дескриптора) значило бы завести
    /// две реализации одного правила, расходящиеся молча.
    #[must_use]
    pub fn decode(&self, report: &[u8]) -> Option<[u8; 8]> {
        let report = strip_report_id(self.report_id, report)?;
        let mut out = [0u8; 8];

        if let Some(offset) = self.modifiers {
            let mut bits = 0u8;
            for index in 0..8u16 {
                let bit = usize::from(offset + index);
                let Some(byte) = report.get(bit / 8) else {
                    break;
                };
                if byte >> (bit % 8) & 1 != 0 {
                    bits |= 1 << index;
                }
            }
            out[0] = bits;
        }

        if let Some((offset, count, size)) = self.keys {
            for index in 0..count.min(6) {
                let field = Field {
                    offset: offset + u16::from(index) * u16::from(size),
                    size,
                    logical_min: 0,
                    logical_max: i32::from(u8::MAX),
                    relative: false,
                };
                let usage = field.read(report);
                // Usage за пределами байта в boot-отчёт не помещается, и ни одна
                // клавиша его не занимает: страница 0x07 кончается на 0xE7.
                out[2 + usize::from(index)] = u8::try_from(usage).unwrap_or(0);
            }
        }

        Some(out)
    }
}

/// Что удалось понять из дескриптора.
#[derive(Clone, Copy, Debug, Default)]
pub struct Descriptor {
    pub pointer: Option<PointerMap>,
    pub keyboard: Option<KeyboardMap>,
}

/// Состояние Global-элементов: действует до следующего такого же.
#[derive(Clone, Copy)]
struct Global {
    usage_page: u16,
    logical_min: i32,
    logical_max: i32,
    report_size: u8,
    report_count: u8,
    report_id: u8,
}

impl Global {
    const fn new() -> Self {
        Self {
            usage_page: 0,
            logical_min: 0,
            logical_max: 0,
            report_size: 0,
            report_count: 0,
            report_id: 0,
        }
    }
}

/// Состояние Local-элементов: обнуляется каждым Main-элементом.
struct Local {
    usages: [u32; MAX_USAGES],
    count: usize,
    minimum: Option<u32>,
    maximum: Option<u32>,
}

impl Local {
    const fn new() -> Self {
        Self { usages: [0; MAX_USAGES], count: 0, minimum: None, maximum: None }
    }

    fn clear(&mut self) {
        self.count = 0;
        self.minimum = None;
        self.maximum = None;
    }

    /// Usage поля с номером `index` внутри Main-элемента.
    ///
    /// Правило спецификации: перечисленные поштучно usage раздаются полям по
    /// порядку, а последний из них повторяется для оставшихся полей. Если usage
    /// заданы диапазоном — раздаются подряд от минимума.
    fn usage(&self, index: u8) -> u32 {
        if self.count > 0 {
            let at = usize::from(index).min(self.count - 1);
            return self.usages[at];
        }
        match (self.minimum, self.maximum) {
            (Some(min), Some(max)) => min.saturating_add(u32::from(index)).min(max),
            (Some(min), None) => min.saturating_add(u32::from(index)),
            _ => 0,
        }
    }
}

/// Разбор дескриптора: один проход по потоку элементов.
struct Parser {
    global: Global,
    local: Local,
    /// Сохранённые `Push` состояния.
    stack: [Global; MAX_PUSH_DEPTH],
    depth: usize,
    /// Usage коллекции верхнего уровня: что за устройство описывается.
    top_usage: u32,
    /// Глубина вложенности коллекций.
    collections: u32,
    /// Битовое смещение внутри текущего отчёта.
    bits: u16,
    /// Идентификатор отчёта, для которого считается смещение.
    current_report: u8,
    found: Descriptor,
    /// Незавершённая карта указателя: X и Y приходят порознь.
    x: Option<Field>,
    y: Option<Field>,
    wheel: Option<Field>,
    buttons: Option<(u16, u8)>,
    pointer_report: u8,
    modifiers: Option<u16>,
    keys: Option<(u16, u8, u8)>,
    keyboard_report: u8,
}

impl Parser {
    const fn new() -> Self {
        Self {
            global: Global::new(),
            local: Local::new(),
            stack: [Global::new(); MAX_PUSH_DEPTH],
            depth: 0,
            top_usage: 0,
            collections: 0,
            bits: 0,
            current_report: 0,
            found: Descriptor { pointer: None, keyboard: None },
            x: None,
            y: None,
            wheel: None,
            buttons: None,
            pointer_report: 0,
            modifiers: None,
            keys: None,
            keyboard_report: 0,
        }
    }

    /// Описывает ли коллекция верхнего уровня указатель.
    ///
    /// Проверка нужна, чтобы X и Y джойстика не стали курсором: usage у осей
    /// одинаковый, различает их только то, во что они вложены. Планшеты
    /// объявляются мышью (так делает и VirtualBox, и QEMU) либо цифровым
    /// планшетом — приняты оба.
    const fn is_pointer_collection(&self) -> bool {
        let page = (self.top_usage >> 16) as u16;
        let usage = self.top_usage as u16;
        match page {
            PAGE_GENERIC_DESKTOP => usage == USAGE_MOUSE || usage == USAGE_POINTER,
            PAGE_DIGITIZER => true,
            _ => false,
        }
    }

    /// Перейти к отчёту с другим идентификатором.
    ///
    /// Каждый отчёт — свой битовый поток, и смещения в нём считаются с нуля.
    /// Общий счётчик означал бы, что поля второго отчёта лежат «после» первого,
    /// то есть за концом пакета.
    fn switch_report(&mut self, id: u8) {
        if id != self.current_report {
            self.current_report = id;
            self.bits = 0;
        }
    }

    fn main_item(&mut self, tag: u8, value: u32) {
        match tag {
            MAIN_INPUT => self.input_item(value),
            MAIN_COLLECTION => {
                if self.collections == 0 {
                    // Usage коллекции верхнего уровня — единственное, что
                    // говорит, чем устройство себя считает.
                    self.top_usage = self.local.usage(0);
                    if self.top_usage <= 0xFFFF {
                        self.top_usage |= u32::from(self.global.usage_page) << 16;
                    }
                }
                self.collections = self.collections.saturating_add(1);
            }
            MAIN_END_COLLECTION => {
                self.collections = self.collections.saturating_sub(1);
                if self.collections == 0 {
                    self.finish_collection();
                }
            }
            _ => {}
        }
        self.local.clear();
    }

    /// Закрыть коллекцию верхнего уровня: то, что в ней нашлось, становится
    /// картой.
    ///
    /// Именно здесь, а не в конце разбора: у устройства с двумя коллекциями
    /// (клавиатура плюс медиаклавиши) поля второй не должны дописываться в карту
    /// первой.
    fn finish_collection(&mut self) {
        if self.found.pointer.is_none() {
            if let (Some(x), Some(y)) = (self.x, self.y) {
                self.found.pointer = Some(PointerMap {
                    report_id: self.pointer_report,
                    x,
                    y,
                    wheel: self.wheel,
                    buttons: self.buttons,
                });
            }
        }
        if self.found.keyboard.is_none() && (self.modifiers.is_some() || self.keys.is_some()) {
            self.found.keyboard = Some(KeyboardMap {
                report_id: self.keyboard_report,
                modifiers: self.modifiers,
                keys: self.keys,
            });
        }
        self.x = None;
        self.y = None;
        self.wheel = None;
        self.buttons = None;
        self.modifiers = None;
        self.keys = None;
    }

    /// Объявление полей: разложить их по битам отчёта.
    fn input_item(&mut self, flags: u32) {
        self.switch_report(self.global.report_id);

        let size = self.global.report_size;
        let count = self.global.report_count;
        let start = self.bits;
        let width = u16::from(size).saturating_mul(u16::from(count));
        // Смещение двигается **всегда**, даже если поле не пригодилось: биты
        // чужого поля занимают место, и пропустить их значит сдвинуть всё
        // остальное. Именно этой ошибкой разбор превращается в правдоподобную
        // бессмыслицу вместо честного отказа.
        self.bits = self.bits.saturating_add(width);

        if flags & INPUT_CONSTANT != 0 || size == 0 || count == 0 {
            return;
        }

        let relative = flags & INPUT_RELATIVE != 0;

        if flags & INPUT_VARIABLE == 0 {
            // Массив: поле несёт не состояние одного usage, а **номер** того,
            // что сейчас нажато. Так устроен список клавиш в отчёте клавиатуры.
            if self.global.usage_page == PAGE_KEYBOARD && self.keys.is_none() {
                self.keys = Some((start, count, size));
                self.keyboard_report = self.global.report_id;
            }
            return;
        }

        for index in 0..count {
            let offset = start + u16::from(index) * u16::from(size);
            let usage = self.local.usage(index);
            // Usage может приехать вместе со своей страницей в старших разрядах
            // (четырёхбайтовый элемент): тогда действует она, а не текущая
            // глобальная.
            let (page, usage) = if usage > 0xFFFF {
                ((usage >> 16) as u16, usage as u16)
            } else {
                (self.global.usage_page, usage as u16)
            };

            let field = Field {
                offset,
                size,
                logical_min: self.global.logical_min,
                logical_max: self.global.logical_max,
                relative,
            };

            match (page, usage) {
                (PAGE_GENERIC_DESKTOP, USAGE_X) if self.is_pointer_collection() => {
                    self.x.get_or_insert(field);
                    self.pointer_report = self.global.report_id;
                }
                (PAGE_GENERIC_DESKTOP, USAGE_Y) if self.is_pointer_collection() => {
                    self.y.get_or_insert(field);
                }
                (PAGE_GENERIC_DESKTOP, USAGE_WHEEL) if self.is_pointer_collection() => {
                    self.wheel.get_or_insert(field);
                }
                (PAGE_BUTTON, number) if (1..=8).contains(&number) => {
                    // Кнопки объявляются диапазоном и лежат подряд, но опираться
                    // на это нельзя: начало ряда вычисляется из номера кнопки,
                    // поэтому дескриптор, назвавший их в другом порядке, всё
                    // равно разберётся верно.
                    let base = offset.saturating_sub((number - 1) * u16::from(size));
                    let count = u8::try_from(number).unwrap_or(u8::MAX);
                    self.buttons = Some(match self.buttons {
                        Some((known, seen)) => (known.min(base), seen.max(count)),
                        None => (base, count),
                    });
                }
                (PAGE_KEYBOARD, code)
                    if (USAGE_MODIFIER_FIRST..=USAGE_MODIFIER_LAST).contains(&code) =>
                {
                    let base = offset
                        .saturating_sub((code - USAGE_MODIFIER_FIRST) * u16::from(size));
                    self.modifiers.get_or_insert(base);
                    self.keyboard_report = self.global.report_id;
                }
                _ => {}
            }
        }
    }

    fn global_item(&mut self, tag: u8, value: u32, length: u8) {
        match tag {
            GLOBAL_USAGE_PAGE => self.global.usage_page = value as u16,
            // Нижняя граница знаковая всегда: именно она сообщает, что поле
            // знаковое.
            GLOBAL_LOGICAL_MIN => self.global.logical_min = signed(value, length),
            // Верхняя — знаковая только при отрицательной нижней. Иначе
            // `0x26 0xFF 0xFF` (65535 у планшета) прочиталось бы как −1, и
            // правая половина экрана оказалась бы недостижимой.
            GLOBAL_LOGICAL_MAX => {
                self.global.logical_max = if self.global.logical_min < 0 {
                    signed(value, length)
                } else {
                    value as i32
                };
            }
            GLOBAL_REPORT_SIZE => self.global.report_size = u8::try_from(value).unwrap_or(0),
            GLOBAL_REPORT_COUNT => self.global.report_count = u8::try_from(value).unwrap_or(0),
            GLOBAL_REPORT_ID => self.global.report_id = value as u8,
            GLOBAL_PUSH => {
                if self.depth < MAX_PUSH_DEPTH {
                    self.stack[self.depth] = self.global;
                    self.depth += 1;
                }
            }
            GLOBAL_POP => {
                if self.depth > 0 {
                    self.depth -= 1;
                    self.global = self.stack[self.depth];
                }
            }
            _ => {}
        }
    }

    fn local_item(&mut self, tag: u8, value: u32) {
        match tag {
            LOCAL_USAGE => {
                if self.local.count < MAX_USAGES {
                    self.local.usages[self.local.count] = value;
                    self.local.count += 1;
                }
            }
            LOCAL_USAGE_MIN => self.local.minimum = Some(value),
            LOCAL_USAGE_MAX => self.local.maximum = Some(value),
            _ => {}
        }
    }
}

/// Знаковое значение элемента длиной `length` байт.
fn signed(value: u32, length: u8) -> i32 {
    match length {
        1 => i32::from(value as u8 as i8),
        2 => i32::from(value as u16 as i16),
        _ => value as i32,
    }
}

/// Разобрать дескриптор отчётов.
///
/// Испорченный дескриптор не является ошибкой, о которой стоит рассказывать
/// отдельно: результат — [`Descriptor`] без карт, и вызывающий сам решает, есть
/// ли у него запасной путь (boot-протокол) или устройство остаётся неопознанным.
#[must_use]
pub fn parse(bytes: &[u8]) -> Descriptor {
    let mut parser = Parser::new();
    let mut offset = 0usize;
    let mut items = 0usize;

    while offset < bytes.len() {
        items += 1;
        if items > MAX_ITEMS {
            break;
        }

        let prefix = bytes[offset];
        offset += 1;

        if prefix == LONG_ITEM_PREFIX {
            // Длинных элементов не встречается ни у одного устройства, но
            // пропустить их надо правильно: иначе разбор пойдёт с середины
            // данных и увидит там элементы, которых нет.
            let Some(&size) = bytes.get(offset) else {
                break;
            };
            offset = offset.saturating_add(2 + usize::from(size));
            continue;
        }

        // Длина в префиксе кодируется двумя битами, и значение 3 означает
        // четыре байта, а не три.
        let length = match prefix & 0b11 {
            3 => 4,
            other => usize::from(other),
        };
        if offset + length > bytes.len() {
            break;
        }

        let mut value = 0u32;
        for (index, byte) in bytes[offset..offset + length].iter().enumerate() {
            value |= u32::from(*byte) << (index * 8);
        }
        offset += length;

        let kind = (prefix >> 2) & 0b11;
        let tag = prefix >> 4;
        match kind {
            ITEM_TYPE_MAIN => parser.main_item(tag, value),
            ITEM_TYPE_GLOBAL => parser.global_item(tag, value, length as u8),
            ITEM_TYPE_LOCAL => parser.local_item(tag, value),
            _ => {}
        }
    }

    // Дескриптор, забывший закрыть коллекцию, встречается: закрываем сами, иначе
    // всё найденное пропало бы из-за одного отсутствующего байта.
    parser.finish_collection();
    parser.found
}
