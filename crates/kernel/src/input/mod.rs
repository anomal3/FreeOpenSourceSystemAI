//! Подсистема ввода: события клавиатуры в терминах, не зависящих ни от
//! архитектуры, ни от протокола устройства.
//!
//! # Что здесь за границу проведено
//!
//! Драйвер знает про свою железку и больше ни про что: i8042 разбирает
//! scancode set 1, UART — поток байтов, будущий USB HID — отчёты boot protocol.
//! Все трое приводят своё событие к [`KeyCode`] и вызывают [`post`]. Остальное
//! ядро видит только очередь событий и не содержит ни одной строчки, знающей,
//! откуда клавиша приехала.
//!
//! Проверить это утверждение легко: [`crate::input::line`] — редактор строки,
//! которым пользуется приглашение ядра, — работает одинаково от PS/2-клавиатуры
//! в окне QEMU и от терминала, подключённого к серийному порту. Ни одного
//! `#[cfg]` и ни одной проверки «а это откуда» в нём нет.
//!
//! # Почему код клавиши позиционный, а не символ
//!
//! Драйвер физически не может отдавать символы: одна и та же клавиша даёт `2`,
//! `@` или `"` в зависимости от раскладки и модификаторов, а мышь и геймпад
//! символов не дают вовсе. Поэтому событие несёт **позицию** клавиши, а перевод
//! в символ выполняет [`keymap`] — отдельный слой, который и станет местом, куда
//! добавляется русская раскладка, не задевая ни один драйвер.
//!
//! Имена вариантов [`KeyCode`] совпадают по смыслу с usage-именами USB HID
//! (`KeyCode::A`, `KeyCode::Digit1`, `KeyCode::LeftShift`) намеренно: когда
//! появится USB-стек, его таблица перевода будет почти тождественной, а не
//! очередным словарём наименований.
//!
//! # Модификаторы едут внутри события
//!
//! [`KeyEvent`] содержит снимок [`Modifiers`] на момент нажатия. Это не
//! избыточность: потребитель разбирает очередь позже, чем она наполняется, и
//! спрашивать «а Shift сейчас нажат?» в момент разбора — значит получить ответ
//! про другое время. Классический симптом такой ошибки — заглавные буквы,
//! запаздывающие на одну клавишу.

// Часть контракта подсистемы заведомо не имеет вызывающих на этой фазе, и это
// спроектировано, а не забыто: `Modifiers::bits` — то, чем USB HID будет
// собирать состояние из байта отчёта, `has_events` нужен опросу без изъятия
// события, `LineEditor::without_echo` — вводу пароля в установщике. Так же
// поступают `mm` и `sched` — по той же причине: удалить сейчас значит написать
// заново через фазу, а шум предупреждений скрывает по-настоящему новые.
#![allow(dead_code)]

pub mod ascii;
pub mod keymap;
pub mod line;

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use crate::sync::SpinLock;

/// Какие устройства ввода удалось поднять.
///
/// Существует затем, чтобы ядро не рассказывало пользователю про клавиатуру на
/// машине, где её нет: набор источников у архитектур разный, и сообщение «нажмите
/// любую клавишу» на AArch64 без USB было бы неправдой.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sources {
    /// Настоящая клавиатура (PS/2 или USB HID).
    pub keyboard: bool,
    /// Приём по последовательному порту.
    pub serial: bool,
    /// Мышь (USB HID, boot protocol).
    pub mouse: bool,
}

impl Sources {
    const BIT_KEYBOARD: u8 = 1 << 0;
    const BIT_SERIAL: u8 = 1 << 1;
    const BIT_MOUSE: u8 = 1 << 2;

    /// Есть ли хоть один источник событий.
    #[must_use]
    pub const fn any(self) -> bool {
        self.keyboard || self.serial || self.mouse
    }

    const fn bits(self) -> u8 {
        (if self.keyboard { Self::BIT_KEYBOARD } else { 0 })
            | (if self.serial { Self::BIT_SERIAL } else { 0 })
            | (if self.mouse { Self::BIT_MOUSE } else { 0 })
    }

    const fn from_bits(bits: u8) -> Self {
        Self {
            keyboard: bits & Self::BIT_KEYBOARD != 0,
            serial: bits & Self::BIT_SERIAL != 0,
            mouse: bits & Self::BIT_MOUSE != 0,
        }
    }
}

static SOURCES: AtomicU8 = AtomicU8::new(0);

/// Запомнить, что удалось поднять. Вызывается арх-слоем из его `input::init`.
pub fn set_sources(sources: Sources) {
    SOURCES.store(sources.bits(), Ordering::Relaxed);
}

/// Что удалось поднять.
#[must_use]
pub fn sources() -> Sources {
    Sources::from_bits(SOURCES.load(Ordering::Relaxed))
}

/// Код клавиши: физическая позиция на клавиатуре, а не символ.
///
/// `#[repr(u8)]` — чтобы код можно было хранить и передавать компактно; значения
/// вариантов при этом нигде не зашиты в протоколы, так что порядок здесь
/// свободен.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
#[allow(dead_code)] // полный набор клавиш заводится сразу; часть кодов пока не порождает ни один драйвер
pub enum KeyCode {
    /// Клавиша, которую драйвер не смог опознать. Событие всё равно доезжает:
    /// потерять его молча значит потом искать «клавиатура иногда не работает».
    Unknown,

    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,

    Digit0, Digit1, Digit2, Digit3, Digit4,
    Digit5, Digit6, Digit7, Digit8, Digit9,

    Enter,
    Escape,
    Backspace,
    Tab,
    Space,
    /// `-` в основном блоке.
    Minus,
    /// `=`
    Equal,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `\`
    Backslash,
    /// `;`
    Semicolon,
    /// `'`
    Apostrophe,
    /// `` ` ``
    Grave,
    /// `,`
    Comma,
    /// `.`
    Period,
    /// `/`
    Slash,

    CapsLock,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,

    PrintScreen,
    ScrollLock,
    Pause,

    Insert,
    Home,
    PageUp,
    Delete,
    End,
    PageDown,

    Right,
    Left,
    Down,
    Up,

    NumLock,
    KeypadSlash,
    KeypadAsterisk,
    KeypadMinus,
    KeypadPlus,
    KeypadEnter,
    Keypad0, Keypad1, Keypad2, Keypad3, Keypad4,
    Keypad5, Keypad6, Keypad7, Keypad8, Keypad9,
    KeypadPeriod,

    LeftCtrl,
    LeftShift,
    LeftAlt,
    /// Клавиша с логотипом слева (в USB HID — `LeftGUI`).
    LeftMeta,
    RightCtrl,
    RightShift,
    RightAlt,
    RightMeta,
    /// Клавиша контекстного меню.
    Menu,
}

impl KeyCode {
    /// Человекочитаемое имя — для диагностики, а не для вывода пользователю.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::A => "A", Self::B => "B", Self::C => "C", Self::D => "D",
            Self::E => "E", Self::F => "F", Self::G => "G", Self::H => "H",
            Self::I => "I", Self::J => "J", Self::K => "K", Self::L => "L",
            Self::M => "M", Self::N => "N", Self::O => "O", Self::P => "P",
            Self::Q => "Q", Self::R => "R", Self::S => "S", Self::T => "T",
            Self::U => "U", Self::V => "V", Self::W => "W", Self::X => "X",
            Self::Y => "Y", Self::Z => "Z",
            Self::Digit0 => "0", Self::Digit1 => "1", Self::Digit2 => "2",
            Self::Digit3 => "3", Self::Digit4 => "4", Self::Digit5 => "5",
            Self::Digit6 => "6", Self::Digit7 => "7", Self::Digit8 => "8",
            Self::Digit9 => "9",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::Backspace => "Backspace",
            Self::Tab => "Tab",
            Self::Space => "Space",
            Self::Minus => "Minus",
            Self::Equal => "Equal",
            Self::LeftBracket => "LeftBracket",
            Self::RightBracket => "RightBracket",
            Self::Backslash => "Backslash",
            Self::Semicolon => "Semicolon",
            Self::Apostrophe => "Apostrophe",
            Self::Grave => "Grave",
            Self::Comma => "Comma",
            Self::Period => "Period",
            Self::Slash => "Slash",
            Self::CapsLock => "CapsLock",
            Self::F1 => "F1", Self::F2 => "F2", Self::F3 => "F3",
            Self::F4 => "F4", Self::F5 => "F5", Self::F6 => "F6",
            Self::F7 => "F7", Self::F8 => "F8", Self::F9 => "F9",
            Self::F10 => "F10", Self::F11 => "F11", Self::F12 => "F12",
            Self::PrintScreen => "PrintScreen",
            Self::ScrollLock => "ScrollLock",
            Self::Pause => "Pause",
            Self::Insert => "Insert",
            Self::Home => "Home",
            Self::PageUp => "PageUp",
            Self::Delete => "Delete",
            Self::End => "End",
            Self::PageDown => "PageDown",
            Self::Right => "Right",
            Self::Left => "Left",
            Self::Down => "Down",
            Self::Up => "Up",
            Self::NumLock => "NumLock",
            Self::KeypadSlash => "Keypad/",
            Self::KeypadAsterisk => "Keypad*",
            Self::KeypadMinus => "Keypad-",
            Self::KeypadPlus => "Keypad+",
            Self::KeypadEnter => "KeypadEnter",
            Self::Keypad0 => "Keypad0", Self::Keypad1 => "Keypad1",
            Self::Keypad2 => "Keypad2", Self::Keypad3 => "Keypad3",
            Self::Keypad4 => "Keypad4", Self::Keypad5 => "Keypad5",
            Self::Keypad6 => "Keypad6", Self::Keypad7 => "Keypad7",
            Self::Keypad8 => "Keypad8", Self::Keypad9 => "Keypad9",
            Self::KeypadPeriod => "Keypad.",
            Self::LeftCtrl => "LeftCtrl",
            Self::LeftShift => "LeftShift",
            Self::LeftAlt => "LeftAlt",
            Self::LeftMeta => "LeftMeta",
            Self::RightCtrl => "RightCtrl",
            Self::RightShift => "RightShift",
            Self::RightAlt => "RightAlt",
            Self::RightMeta => "RightMeta",
            Self::Menu => "Menu",
        }
    }
}

/// Состояние модификаторов.
///
/// Набор битов, а не структура из `bool`, по той же причине, что и
/// [`crate::mm::PageFlags`]: проверка «нажат ли хоть один Shift» — это одно
/// сравнение, а не разбор двух полей на каждой стороне.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1 << 0);
    pub const CTRL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    /// Клавиша с логотипом. Ядру пока не нужна, но терять её в драйвере значит
    /// потом переделывать все три драйвера сразу.
    pub const META: Self = Self(1 << 3);
    /// Caps Lock **залипший**, а не нажатый: это состояние, а не клавиша.
    pub const CAPS: Self = Self(1 << 4);
    pub const NUM_LOCK: Self = Self(1 << 5);

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[must_use]
    pub const fn toggled(self, other: Self) -> Self {
        Self(self.0 ^ other.0)
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl core::fmt::Debug for Modifiers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut first = true;
        let mut bit = |name: &str, flag: Modifiers| -> core::fmt::Result {
            if self.contains(flag) {
                if !first {
                    f.write_str("+")?;
                }
                first = false;
                f.write_str(name)?;
            }
            Ok(())
        };
        bit("shift", Self::SHIFT)?;
        bit("ctrl", Self::CTRL)?;
        bit("alt", Self::ALT)?;
        bit("meta", Self::META)?;
        bit("caps", Self::CAPS)?;
        bit("num", Self::NUM_LOCK)?;
        if first {
            f.write_str("-")?;
        }
        Ok(())
    }
}

/// Событие клавиатуры.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub code: KeyCode,
    /// `true` — нажатие, `false` — отпускание.
    pub pressed: bool,
    /// Состояние модификаторов **после** учёта этого события.
    pub mods: Modifiers,
}

impl KeyEvent {
    /// Значение для заполнения пустых слотов кольцевого буфера. Наружу оно
    /// никогда не выдаётся: [`Queue::pop`] отдаёт только записанные элементы.
    const EMPTY: Self =
        Self { code: KeyCode::Unknown, pressed: false, mods: Modifiers::NONE };

    /// Символ, который эта клавиша даёт с учётом модификаторов, если даёт.
    #[must_use]
    pub fn to_char(self) -> Option<char> {
        keymap::char_for(self)
    }
}

/// Сколько событий помещается в очередь.
///
/// 128 — это около трёх секунд быстрой печати (нажатие и отпускание, то есть
/// два события на символ). Смысл запаса ровно один: потребитель на этой фазе —
/// задача кооперативного планировщика, и между двумя её квантами вполне может
/// уместиться серия прерываний. Расти дальше незачем — за три секунды
/// незамеченного простоя система имеет проблему серьёзнее потерянных клавиш.
const QUEUE_CAPACITY: usize = 128;

/// Кольцевой буфер событий и состояние модификаторов.
struct Queue {
    events: [KeyEvent; QUEUE_CAPACITY],
    /// Откуда читать следующее событие.
    head: usize,
    /// Сколько событий лежит в буфере.
    len: usize,
    /// Сколько событий всего пришло от драйверов.
    posted: u64,
    /// Сколько событий потеряно — очередь была полна либо занята.
    dropped: u64,
    mods: Modifiers,
}

impl Queue {
    const fn new() -> Self {
        Self {
            events: [KeyEvent::EMPTY; QUEUE_CAPACITY],
            head: 0,
            len: 0,
            posted: 0,
            dropped: 0,
            mods: Modifiers::NONE,
        }
    }

    /// Обновить состояние модификаторов по событию.
    ///
    /// Возвращает состояние, которое поедет в событие. Для самих клавиш-
    /// модификаторов это состояние **уже с учётом** нажатия: иначе событие
    /// «Shift нажат» приезжало бы с флагом «Shift не нажат», и любой
    /// потребитель, отслеживающий состояние по событиям, отставал бы на шаг.
    fn apply(&mut self, code: KeyCode, pressed: bool) -> Modifiers {
        let flag = match code {
            KeyCode::LeftShift | KeyCode::RightShift => Some(Modifiers::SHIFT),
            KeyCode::LeftCtrl | KeyCode::RightCtrl => Some(Modifiers::CTRL),
            KeyCode::LeftAlt | KeyCode::RightAlt => Some(Modifiers::ALT),
            KeyCode::LeftMeta | KeyCode::RightMeta => Some(Modifiers::META),
            _ => None,
        };

        if let Some(flag) = flag {
            // Оба Shift'а делят один бит, поэтому отпускание правого снимает
            // флаг и при удерживаемом левом. Различать их значило бы держать по
            // биту на каждую физическую клавишу — цена, которую платят
            // оконные системы, а ядру она не нужна: смысл у обеих клавиш один.
            self.mods = if pressed { self.mods.union(flag) } else { self.mods.without(flag) };
            return self.mods;
        }

        // Фиксаторы переключаются по нажатию и игнорируют отпускание.
        if pressed {
            match code {
                KeyCode::CapsLock => self.mods = self.mods.toggled(Modifiers::CAPS),
                KeyCode::NumLock => self.mods = self.mods.toggled(Modifiers::NUM_LOCK),
                _ => {}
            }
        }
        self.mods
    }

    fn push(&mut self, event: KeyEvent) -> bool {
        if self.len == QUEUE_CAPACITY {
            return false;
        }
        let tail = (self.head + self.len) % QUEUE_CAPACITY;
        self.events[tail] = event;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<KeyEvent> {
        if self.len == 0 {
            return None;
        }
        let event = self.events[self.head];
        self.head = (self.head + 1) % QUEUE_CAPACITY;
        self.len -= 1;
        Some(event)
    }
}

static QUEUE: SpinLock<Queue> = SpinLock::new(Queue::new());

/// Счётчик потерянных событий, которые не удалось учесть под локом.
///
/// Отдельно от [`Queue::dropped`] по необходимости: потеря случается именно
/// тогда, когда лок недоступен, и записать её в защищённую им структуру нельзя.
static DROPPED_LOCKED_OUT: AtomicU64 = AtomicU64::new(0);

/// Сколько событий ввода — любых — система приняла с начала работы.
///
/// Считает и клавиши, и указатель, и делает это **до** попытки взять лок
/// очереди: счётчик отвечает на вопрос «случилось ли хоть что-нибудь», а
/// событие, потерянное из-за занятого лока, случилось ровно так же, как и любое
/// другое.
///
/// Нужен тому, кто засыпает в ожидании ввода: он запоминает значение, разбирает
/// очередь и засыпает только если значение не изменилось (см.
/// [`crate::sched::block_on_input`]). Без этого событие, пришедшее между
/// «очередь пуста» и «я заснул», разбудило бы того, кто ещё не спит.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Сколько событий ввода принято с начала работы.
#[must_use]
pub fn sequence() -> u64 {
    SEQUENCE.load(Ordering::Relaxed)
}

/// Отметить пришедшее событие и разбудить тех, кто его ждёт.
///
/// Порядок обязателен: сначала счётчик, потом пробуждение. При обратном порядке
/// засыпающий успел бы прочитать старое значение уже после того, как его
/// попытались разбудить, и уснул бы до срока.
fn announce() {
    SEQUENCE.fetch_add(1, Ordering::Relaxed);
    crate::sched::wake_input();
}

/// Положить событие в очередь. Вызывается драйвером, как правило из обработчика
/// прерывания.
///
/// # Почему `try_lock`, а не `lock`
///
/// На одном процессоре `lock()` здесь не мог бы заблокироваться: [`SpinLock`]
/// удерживается с запрещёнными прерываниями, поэтому обработчик не в состоянии
/// застать лок занятым. Но это рассуждение опирается на устройство *другого*
/// модуля и на однопроцессорность, а цена ошибки — вечное зависание в
/// обработчике прерывания. Потерянная клавиша дешевле, и она хотя бы посчитана.
pub fn post(code: KeyCode, pressed: bool) {
    // Отметка о событии — до захвата лока и независимо от его успеха: разбудить
    // ждущего надо и в том случае, когда само событие потерялось. Иначе
    // потерянная клавиша превращается в задачу, спящую до срока.
    announce();

    let Some(mut queue) = QUEUE.try_lock() else {
        DROPPED_LOCKED_OUT.fetch_add(1, Ordering::Relaxed);
        return;
    };
    queue.posted += 1;
    let mods = queue.apply(code, pressed);
    if !queue.push(KeyEvent { code, pressed, mods }) {
        // Очередь полна: теряется **новое** событие, а не самое старое.
        // Выбор не произвольный — при переполнении важнее сохранить начало
        // ввода, потому что именно оно уже показано на экране эхом. Выбросив
        // старое, мы получили бы строку, не совпадающую с тем, что видит
        // пользователь.
        queue.dropped += 1;
    }
}

/// Забрать следующее событие. `None` — очередь пуста.
#[must_use]
pub fn next_event() -> Option<KeyEvent> {
    QUEUE.lock().pop()
}

/// Есть ли что читать. Дешевле, чем [`next_event`], когда событие не нужно
/// забирать.
#[must_use]
pub fn has_events() -> bool {
    QUEUE.lock().len != 0
}

/// Текущее состояние модификаторов.
#[must_use]
pub fn modifiers() -> Modifiers {
    QUEUE.lock().mods
}

/// Счётчики подсистемы ввода.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    /// Сколько событий пришло от драйверов.
    pub posted: u64,
    /// Сколько потеряно: очередь была полна либо занята.
    pub dropped: u64,
    /// Сколько сейчас ждёт разбора.
    pub queued: usize,
}

#[must_use]
pub fn stats() -> Stats {
    let queue = QUEUE.lock();
    Stats {
        posted: queue.posted,
        dropped: queue.dropped + DROPPED_LOCKED_OUT.load(Ordering::Relaxed),
        queued: queue.len,
    }
}

// ---------------------------------------------------------------------------
// Указатель
// ---------------------------------------------------------------------------

/// Кнопки указателя.
///
/// Тот же приём, что и с [`Modifiers`]: набор битов, а не три `bool`. Драйвер
/// получает их именно битовой картой (байт 0 отчёта boot-протокола), и разбирать
/// её на поля, чтобы тут же собрать обратно, незачем.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Buttons(u8);

impl Buttons {
    pub const NONE: Self = Self(0);
    pub const LEFT: Self = Self(1 << 0);
    pub const RIGHT: Self = Self(1 << 1);
    pub const MIDDLE: Self = Self(1 << 2);

    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b111)
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Какие биты различаются — то есть какие кнопки изменили состояние.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 ^ other.0)
    }
}

impl core::fmt::Debug for Buttons {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.0 == 0 {
            return f.write_str("-");
        }
        let mut first = true;
        for (name, flag) in [("left", Self::LEFT), ("right", Self::RIGHT), ("middle", Self::MIDDLE)]
        {
            if self.contains(flag) {
                if !first {
                    f.write_str("+")?;
                }
                first = false;
                f.write_str(name)?;
            }
        }
        Ok(())
    }
}

/// Событие указателя.
///
/// Обычно несёт **приращение**, а не позицию, потому что именно это сообщает
/// мышь: абсолютных координат у неё нет, и где находится курсор — знает не
/// драйвер, а тот, кто его рисует.
///
/// Планшет — устройство, которое VirtualBox предлагает вместо мыши, — сообщает
/// противоположное: не сдвиг, а точку. Свести одно к другому в драйвере
/// невозможно (чтобы получить сдвиг, надо знать, где курсор), поэтому событие
/// умеет нести и то и другое, а выбирает — устройство.
#[derive(Clone, Copy, Debug)]
pub struct PointerEvent {
    pub dx: i32,
    pub dy: i32,
    /// Положение в **долях** экрана: 0 — левый верхний угол, 65535 — правый
    /// нижний. `None` у мыши, которая сообщила приращение.
    ///
    /// Доли, а не точки, по той же причине, по которой в событии нет позиции
    /// курсора: драйвер не знает размера экрана и знать его не должен. Диапазон
    /// же у планшета свой — у одного 0..32767, у другого 0..4095, — и приводить
    /// его к общему виду обязан тот, кто читает устройство.
    pub absolute: Option<(u16, u16)>,
    /// Колесо: положительное — от себя.
    pub wheel: i32,
    /// Состояние кнопок после этого отчёта.
    pub buttons: Buttons,
    /// Кнопки, изменившие состояние. По той же причине, что и снимок
    /// модификаторов в [`KeyEvent`]: потребитель разбирает очередь позже, чем
    /// она наполняется, и «нажата ли сейчас кнопка» — ответ про другое время.
    pub changed: Buttons,
}

impl PointerEvent {
    const EMPTY: Self = Self {
        dx: 0,
        dy: 0,
        absolute: None,
        wheel: 0,
        buttons: Buttons::NONE,
        changed: Buttons::NONE,
    };

    /// Нажата ли эта кнопка именно этим событием.
    #[must_use]
    pub const fn pressed(self, button: Buttons) -> bool {
        self.changed.contains(button) && self.buttons.contains(button)
    }

    /// Отпущена ли эта кнопка именно этим событием.
    #[must_use]
    pub const fn released(self, button: Buttons) -> bool {
        self.changed.contains(button) && !self.buttons.contains(button)
    }
}

/// Сколько событий указателя помещается в очередь.
///
/// Меньше, чем у клавиатуры, и это осознанно: мышь при движении шлёт отчёт
/// каждые несколько миллисекунд, и хранить их секунду смысла нет — устаревшее
/// перемещение курсора никому не нужно. Тридцать два отчёта — это примерно
/// четверть секунды движения.
const POINTER_CAPACITY: usize = 32;

struct PointerQueue {
    events: [PointerEvent; POINTER_CAPACITY],
    head: usize,
    len: usize,
    posted: u64,
    dropped: u64,
    buttons: Buttons,
}

impl PointerQueue {
    const fn new() -> Self {
        Self {
            events: [PointerEvent::EMPTY; POINTER_CAPACITY],
            head: 0,
            len: 0,
            posted: 0,
            dropped: 0,
            buttons: Buttons::NONE,
        }
    }
}

static POINTER: SpinLock<PointerQueue> = SpinLock::new(PointerQueue::new());

/// Положить отчёт мыши в очередь. Вызывается драйвером.
pub fn post_pointer(dx: i32, dy: i32, wheel: i32, buttons: Buttons) {
    enqueue_pointer(PointerEvent {
        dx,
        dy,
        absolute: None,
        wheel,
        buttons,
        // Заполняется в [`enqueue_pointer`]: какие кнопки изменились, знает
        // очередь — только она помнит их прошлое состояние.
        changed: Buttons::NONE,
    });
}

/// Положить отчёт планшета в очередь: положение вместо приращения.
///
/// Координаты — доли экрана, см. [`PointerEvent::absolute`].
pub fn post_pointer_at(x: u16, y: u16, wheel: i32, buttons: Buttons) {
    enqueue_pointer(PointerEvent {
        dx: 0,
        dy: 0,
        absolute: Some((x, y)),
        wheel,
        buttons,
        changed: Buttons::NONE,
    });
}

/// Общая часть обоих путей: состояние кнопок и место в очереди.
///
/// При переполнении **сливается** движение: приращения складываются в последнее
/// событие вместо того, чтобы потеряться. Курсор от этого не отстаёт и не
/// перескакивает — в отличие от клавиатуры, где сложить два нажатия нельзя.
fn enqueue_pointer(event: PointerEvent) {
    // См. [`post`]: отметка о событии не зависит от того, поместилось ли оно в
    // очередь.
    announce();

    let Some(mut queue) = POINTER.try_lock() else {
        return;
    };
    queue.posted += 1;
    let changed = queue.buttons.difference(event.buttons);
    queue.buttons = event.buttons;

    if queue.len == POINTER_CAPACITY {
        // Кнопки терять нельзя, движение — можно сложить.
        let tail = (queue.head + queue.len - 1) % POINTER_CAPACITY;
        let last = &mut queue.events[tail];
        last.dx += event.dx;
        last.dy += event.dy;
        last.wheel += event.wheel;
        // А вот положение не складывается: оно не приращение, и последнее
        // сказанное устройством и есть текущее. Сложение здесь дало бы курсор,
        // улетающий в правый нижний угол при первом же переполнении.
        if event.absolute.is_some() {
            last.absolute = event.absolute;
        }
        last.buttons = event.buttons;
        last.changed = Buttons(last.changed.0 | changed.0);
        queue.dropped += 1;
        return;
    }

    let tail = (queue.head + queue.len) % POINTER_CAPACITY;
    queue.events[tail] = PointerEvent { changed, ..event };
    queue.len += 1;
}

/// Забрать следующее событие указателя.
#[must_use]
pub fn next_pointer() -> Option<PointerEvent> {
    let mut queue = POINTER.lock();
    if queue.len == 0 {
        return None;
    }
    let event = queue.events[queue.head];
    queue.head = (queue.head + 1) % POINTER_CAPACITY;
    queue.len -= 1;
    Some(event)
}

/// Счётчики указателя: сколько отчётов пришло и сколько слито при переполнении.
#[must_use]
pub fn pointer_stats() -> (u64, u64) {
    let queue = POINTER.lock();
    (queue.posted, queue.dropped)
}
