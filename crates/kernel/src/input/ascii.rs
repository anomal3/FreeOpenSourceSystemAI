//! Ввод из потока байтов: терминал на серийном порту как клавиатура.
//!
//! # Зачем это нужно, если есть PS/2
//!
//! Во-первых, PS/2 есть только на x86-64: у QEMU `-machine virt` нет ни i8042,
//! ни какого-либо другого legacy-контроллера, и до появления USB-стека это
//! **единственный** способ что-нибудь набрать на ARM. Во-вторых, серийный порт
//! есть на обеих архитектурах и работает без окна QEMU, то есть даёт
//! воспроизводимый неинтерактивный тест: `echo ... | qemu ...` печатает в ядро
//! ровно то же, что человек набрал бы руками.
//!
//! # Обратная задача
//!
//! Направление здесь противоположно остальному вводу: клавиатура сообщает
//! позицию клавиши, а терминал — уже готовый символ, к которому применены и
//! раскладка, и модификаторы. Восстановить по символу нажатые клавиши в общем
//! случае нельзя (`@` на другой раскладке набирается иначе), но для US QWERTY
//! отображение однозначно, а другой раскладки в ядре пока и нет. Поэтому
//! декодер порождает **синтетическую** последовательность: нажать Shift, нажать
//! и отпустить клавишу, отпустить Shift.
//!
//! Почему именно так, а не отдельным «символьным» каналом в очередь: канал
//! пришлось бы поддерживать во всех потребителях, а состояние модификаторов у
//! него разъезжалось бы с состоянием клавиатуры. Синтетические нажатия проходят
//! через тот же [`super::post`], поэтому ниже по течению разницы между
//! терминалом и клавиатурой не существует вовсе.
//!
//! Цена этого решения одна и она честная: если к машине подключены сразу
//! терминал и клавиатура, отпускание синтетического Shift снимет флаг и у
//! физически удерживаемого. Ситуация «двумя руками на двух устройствах
//! одновременно» настолько редкая, что за неё не стоит платить вторым набором
//! состояний.
//!
//! # Escape-последовательности
//!
//! Стрелки, Home/End и PageUp/PageDown терминал присылает не байтом, а
//! последовательностью `ESC [ ...`. Разбор — маленький автомат: без него
//! нажатие «вверх» приехало бы как три мусорных символа `^[[A`, что в редакторе
//! строки выглядит как испорченный ввод.

use super::{KeyCode, post};
use crate::sync::SpinLock;

/// Состояние разбора escape-последовательности.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Обычный байт.
    Ground,
    /// Получен `ESC`, ждём `[`.
    Escape,
    /// Получен `ESC [`, копим параметр до финального байта.
    Csi,
    /// Получен `ESC O`: следующий байт — клавиша VT100 (F1–F4).
    Ss3,
}

struct Decoder {
    state: State,
    /// Числовой параметр CSI (`ESC [ 3 ~` — это Delete).
    param: u32,
}

impl Decoder {
    const fn new() -> Self {
        Self { state: State::Ground, param: 0 }
    }
}

static DECODER: SpinLock<Decoder> = SpinLock::new(Decoder::new());

/// Сколько раз приёмник UART сообщил, что байт потерян.
///
/// Считается, а не игнорируется, по правилу проекта: то, что нельзя увидеть
/// иначе, обязано печататься. Потерянный байт ввода выглядит снаружи как
/// «система не приняла команду» — неотличимо от ошибки разбора, от зависшей
/// оболочки и от неисправного драйвера. Со счётчиком это одна строка в `input`
/// и мгновенный ответ на вопрос «кто виноват».
///
/// Отчего он растёт: пока ядро печатает длинную строку, прерывания запрещены
/// (см. [`crate::print::_print`] — там объяснено, почему), а линия продолжает
/// принимать. Шестнадцати байт FIFO хватает не всегда, и на отладочной сборке,
/// где каждая строка ещё и перерисовывает окно, хвост команды пропадает.
static OVERRUNS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Отметить потерянный приёмником байт. Зовёт драйвер UART.
pub fn note_overrun() {
    OVERRUNS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
}

/// Сколько раз приёмник терял байты за время работы.
pub fn overruns() -> u64 {
    OVERRUNS.load(core::sync::atomic::Ordering::Relaxed)
}

/// Максимальное значение параметра CSI, которое имеет смысл копить.
///
/// Ограничение не декоративное: параметр приходит из-за границы доверия (это
/// байты в линии), и без предела `param * 10 + digit` переполнится, а в отладке
/// сборке это паника — то есть падение ядра от мусора в порту.
const MAX_CSI_PARAM: u32 = 1000;

/// Скормить декодеру один принятый байт.
///
/// Вызывается драйвером UART, как правило из обработчика прерывания. Функция
/// сама решает, породить ли событие: часть байтов — это середина
/// escape-последовательности, и события от них быть не должно.
pub fn feed(byte: u8) {
    // Состояние автомата берётся под локом, но события отправляются уже без
    // него: `post` берёт собственный лок, и удерживать оба одновременно значило
    // бы заводить порядок захвата там, где он не нужен.
    let action = {
        let Some(mut decoder) = DECODER.try_lock() else {
            // Лок занят — значит байт пришёл в середину обработки предыдущего.
            // На одном процессоре это невозможно (лок держится с запрещёнными
            // прерываниями), но полагаться в обработчике на такое рассуждение
            // не стоит: см. ту же оговорку в `super::post`.
            return;
        };
        decoder.step(byte)
    };

    match action {
        Action::None => {}
        Action::Tap(code, shift, ctrl) => tap(code, shift, ctrl),
        Action::EscapeThen(byte) => {
            tap(KeyCode::Escape, false, false);
            // `ESC` без `[` означает, что escape-последовательности не было:
            // пользователь нажал Escape, а следом обычную клавишу. Повторный
            // вход в автомат безопасен — состояние уже сброшено в `Ground`,
            // поэтому глубина рекурсии здесь ровно один уровень.
            feed(byte);
        }
    }
}

/// Что делать с байтом.
enum Action {
    /// Байт был частью последовательности, события нет.
    None,
    /// Нажать и отпустить клавишу: код, нужен ли Shift, нужен ли Ctrl.
    Tap(KeyCode, bool, bool),
    /// Была `ESC`, а за ней — не начало последовательности: отдать Escape и
    /// разобрать байт заново.
    EscapeThen(u8),
}

impl Decoder {
    fn step(&mut self, byte: u8) -> Action {
        match self.state {
            State::Ground => self.ground(byte),
            State::Escape => {
                if byte == b'[' {
                    self.state = State::Csi;
                    self.param = 0;
                    Action::None
                } else if byte == b'O' {
                    // `ESC O …` — клавиши PF1–PF4 из VT100, которыми всякий
                    // терминал по сей день присылает F1–F4.
                    self.state = State::Ss3;
                    Action::None
                } else {
                    self.state = State::Ground;
                    Action::EscapeThen(byte)
                }
            }
            State::Csi => self.csi(byte),
            State::Ss3 => {
                self.state = State::Ground;
                let code = match byte {
                    b'P' => KeyCode::F1,
                    b'Q' => KeyCode::F2,
                    b'R' => KeyCode::F3,
                    b'S' => KeyCode::F4,
                    // Прочее в этой последовательности — клавиши цифрового
                    // блока в «прикладном» режиме; событий по ним не порождаем,
                    // но последовательность съедена целиком.
                    _ => return Action::None,
                };
                Action::Tap(code, false, false)
            }
        }
    }

    fn ground(&mut self, byte: u8) -> Action {
        match byte {
            0x1B => {
                self.state = State::Escape;
                Action::None
            }
            // Возврат каретки и перевод строки — одно и то же нажатие. Какой
            // именно байт пришлёт терминал, зависит от его настроек, и различать
            // их значило бы получить систему, которая на одном терминале
            // работает, а на другом «не реагирует на Enter».
            b'\r' | b'\n' => Action::Tap(KeyCode::Enter, false, false),
            b'\t' => Action::Tap(KeyCode::Tab, false, false),
            // 0x08 — Backspace, 0x7F — Delete-as-backspace. Терминалы
            // используют оба, причём xterm по умолчанию присылает 0x7F.
            0x08 | 0x7F => Action::Tap(KeyCode::Backspace, false, false),
            // Остальные управляющие символы — это комбинации с Ctrl. Обратная
            // арифметика к той, что в `keymap::control_char`: код 1..26
            // соответствует букве 'a' + код - 1.
            0x01..=0x1A => {
                let letter = b'a' + byte - 1;
                match key_for_ascii(letter) {
                    Some((code, _)) => Action::Tap(code, false, true),
                    None => Action::None,
                }
            }
            0x20..=0x7E => match key_for_ascii(byte) {
                Some((code, shift)) => Action::Tap(code, shift, false),
                // Печатный ASCII покрыт таблицей целиком, поэтому сюда не
                // попадаем; молчаливое игнорирование — страховка, а не путь.
                None => Action::None,
            },
            // Всё прочее: обрыв линии даёт мусорные байты со старшим битом, и
            // порождать по ним нажатия нельзя.
            _ => Action::None,
        }
    }

    fn csi(&mut self, byte: u8) -> Action {
        match byte {
            b'0'..=b'9' => {
                let digit = u32::from(byte - b'0');
                self.param = (self.param * 10 + digit).min(MAX_CSI_PARAM);
                Action::None
            }
            // Разделитель параметров и приватные префиксы: содержимое нас не
            // интересует, но последовательность надо доесть до финального байта.
            b';' | b'?' => {
                self.param = 0;
                Action::None
            }
            _ => {
                self.state = State::Ground;
                let param = self.param;
                let code = match byte {
                    b'A' => KeyCode::Up,
                    b'B' => KeyCode::Down,
                    b'C' => KeyCode::Right,
                    b'D' => KeyCode::Left,
                    b'H' => KeyCode::Home,
                    b'F' => KeyCode::End,
                    // `ESC [ n ~` — семейство навигационных клавиш. Нумерация
                    // из спецификации DEC VT: 1 и 7 оба означают Home, 4 и 8 —
                    // End, потому что разные модели VT нумеровали их по-разному,
                    // а терминалы-эмуляторы унаследовали оба варианта.
                    b'~' => match param {
                        1 | 7 => KeyCode::Home,
                        2 => KeyCode::Insert,
                        3 => KeyCode::Delete,
                        4 | 8 => KeyCode::End,
                        5 => KeyCode::PageUp,
                        6 => KeyCode::PageDown,
                        // F5–F12 из VT220. Дыры в нумерации (16 и 22) не наши:
                        // их оставил DEC, и всякий терминал их повторяет.
                        15 => KeyCode::F5,
                        17 => KeyCode::F6,
                        18 => KeyCode::F7,
                        19 => KeyCode::F8,
                        20 => KeyCode::F9,
                        21 => KeyCode::F10,
                        23 => KeyCode::F11,
                        24 => KeyCode::F12,
                        _ => return Action::None,
                    },
                    _ => return Action::None,
                };
                Action::Tap(code, false, false)
            }
        }
    }
}

/// Породить нажатие и отпускание клавиши вместе с нужными модификаторами.
fn tap(code: KeyCode, shift: bool, ctrl: bool) {
    if shift {
        post(KeyCode::LeftShift, true);
    }
    if ctrl {
        post(KeyCode::LeftCtrl, true);
    }
    post(code, true);
    post(code, false);
    // Модификаторы снимаются в обратном порядке — не потому, что это важно для
    // флагов (они независимы), а чтобы последовательность читалась как парная.
    if ctrl {
        post(KeyCode::LeftCtrl, false);
    }
    if shift {
        post(KeyCode::LeftShift, false);
    }
}

/// Клавиша и признак Shift, дающие этот печатный ASCII-символ на US QWERTY.
///
/// Таблица — зеркало [`super::keymap`]. Дублирование намеренное: обратное
/// отображение по прямой таблице пришлось бы искать перебором на каждый байт, а
/// главное — оно было бы неоднозначным (`\n` даёт и Enter, и KeypadEnter).
/// Явная таблица позволяет выбрать, какую именно клавишу считать источником.
const fn key_for_ascii(byte: u8) -> Option<(KeyCode, bool)> {
    let pair = match byte {
        b'a' => (KeyCode::A, false), b'A' => (KeyCode::A, true),
        b'b' => (KeyCode::B, false), b'B' => (KeyCode::B, true),
        b'c' => (KeyCode::C, false), b'C' => (KeyCode::C, true),
        b'd' => (KeyCode::D, false), b'D' => (KeyCode::D, true),
        b'e' => (KeyCode::E, false), b'E' => (KeyCode::E, true),
        b'f' => (KeyCode::F, false), b'F' => (KeyCode::F, true),
        b'g' => (KeyCode::G, false), b'G' => (KeyCode::G, true),
        b'h' => (KeyCode::H, false), b'H' => (KeyCode::H, true),
        b'i' => (KeyCode::I, false), b'I' => (KeyCode::I, true),
        b'j' => (KeyCode::J, false), b'J' => (KeyCode::J, true),
        b'k' => (KeyCode::K, false), b'K' => (KeyCode::K, true),
        b'l' => (KeyCode::L, false), b'L' => (KeyCode::L, true),
        b'm' => (KeyCode::M, false), b'M' => (KeyCode::M, true),
        b'n' => (KeyCode::N, false), b'N' => (KeyCode::N, true),
        b'o' => (KeyCode::O, false), b'O' => (KeyCode::O, true),
        b'p' => (KeyCode::P, false), b'P' => (KeyCode::P, true),
        b'q' => (KeyCode::Q, false), b'Q' => (KeyCode::Q, true),
        b'r' => (KeyCode::R, false), b'R' => (KeyCode::R, true),
        b's' => (KeyCode::S, false), b'S' => (KeyCode::S, true),
        b't' => (KeyCode::T, false), b'T' => (KeyCode::T, true),
        b'u' => (KeyCode::U, false), b'U' => (KeyCode::U, true),
        b'v' => (KeyCode::V, false), b'V' => (KeyCode::V, true),
        b'w' => (KeyCode::W, false), b'W' => (KeyCode::W, true),
        b'x' => (KeyCode::X, false), b'X' => (KeyCode::X, true),
        b'y' => (KeyCode::Y, false), b'Y' => (KeyCode::Y, true),
        b'z' => (KeyCode::Z, false), b'Z' => (KeyCode::Z, true),

        b'1' => (KeyCode::Digit1, false), b'!' => (KeyCode::Digit1, true),
        b'2' => (KeyCode::Digit2, false), b'@' => (KeyCode::Digit2, true),
        b'3' => (KeyCode::Digit3, false), b'#' => (KeyCode::Digit3, true),
        b'4' => (KeyCode::Digit4, false), b'$' => (KeyCode::Digit4, true),
        b'5' => (KeyCode::Digit5, false), b'%' => (KeyCode::Digit5, true),
        b'6' => (KeyCode::Digit6, false), b'^' => (KeyCode::Digit6, true),
        b'7' => (KeyCode::Digit7, false), b'&' => (KeyCode::Digit7, true),
        b'8' => (KeyCode::Digit8, false), b'*' => (KeyCode::Digit8, true),
        b'9' => (KeyCode::Digit9, false), b'(' => (KeyCode::Digit9, true),
        b'0' => (KeyCode::Digit0, false), b')' => (KeyCode::Digit0, true),

        b' ' => (KeyCode::Space, false),
        b'-' => (KeyCode::Minus, false), b'_' => (KeyCode::Minus, true),
        b'=' => (KeyCode::Equal, false), b'+' => (KeyCode::Equal, true),
        b'[' => (KeyCode::LeftBracket, false), b'{' => (KeyCode::LeftBracket, true),
        b']' => (KeyCode::RightBracket, false), b'}' => (KeyCode::RightBracket, true),
        b'\\' => (KeyCode::Backslash, false), b'|' => (KeyCode::Backslash, true),
        b';' => (KeyCode::Semicolon, false), b':' => (KeyCode::Semicolon, true),
        b'\'' => (KeyCode::Apostrophe, false), b'"' => (KeyCode::Apostrophe, true),
        b'`' => (KeyCode::Grave, false), b'~' => (KeyCode::Grave, true),
        b',' => (KeyCode::Comma, false), b'<' => (KeyCode::Comma, true),
        b'.' => (KeyCode::Period, false), b'>' => (KeyCode::Period, true),
        b'/' => (KeyCode::Slash, false), b'?' => (KeyCode::Slash, true),
        _ => return None,
    };
    Some(pair)
}
