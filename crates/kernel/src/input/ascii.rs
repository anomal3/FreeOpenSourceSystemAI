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

use core::sync::atomic::{AtomicU64, Ordering};

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
/// Отчего он рос до того, как появилось спасение (см. [`rescue`]): пока ядро
/// печатает строку, прерывания запрещены, а линия продолжает принимать.
static OVERRUNS: AtomicU64 = AtomicU64::new(0);

/// Сколько байт вынуто из приёмника посреди печати, то есть спасено от потери.
static RESCUED: AtomicU64 = AtomicU64::new(0);

/// Сколько спасённых байт не поместилось в кольцо.
///
/// Отдельно от [`OVERRUNS`], потому что это потеря **наша**, а не аппаратная, и
/// лечится она другим — размером кольца. Сложить их в один счётчик значило бы
/// получить число, по которому нельзя решить, что чинить.
static SPILLED: AtomicU64 = AtomicU64::new(0);

/// Отметить потерянный приёмником байт. Зовёт драйвер UART.
pub fn note_overrun() {
    OVERRUNS.fetch_add(1, Ordering::Relaxed);
}

/// Сколько раз приёмник терял байты за время работы.
pub fn overruns() -> u64 {
    OVERRUNS.load(Ordering::Relaxed)
}

/// Сколько байт спасено из приёмника посреди печати и сколько при этом
/// потеряно уже самим кольцом.
pub fn rescued() -> (u64, u64) {
    (RESCUED.load(Ordering::Relaxed), SPILLED.load(Ordering::Relaxed))
}

// ---------------------------------------------------------------------------
// Кольцо принятых, но ещё не разобранных байт
// ---------------------------------------------------------------------------

/// Сколько байт кольцо держит между чтением приёмника и разбором.
///
/// Двести пятьдесят шесть — не «на всякий случай», а с оглядкой на худшее из
/// известного. Оболочка собирает строку целиком и отдаёт её в вывод одним
/// куском до 512 байт (`shell::LINE_BYTES`); на 115200 это сорок с лишним
/// миллисекунд линии, и всё это время принятое складывается сюда. При темпе
/// стенда (восемь байт каждые 15 мс) за такую строку приезжает десятка два
/// байт, так что запас здесь на порядок — его хватит и на вставку целой
/// команды разом.
const PENDING_BYTES: usize = 256;

/// Принятые байты, которых разбор ещё не видел.
struct Pending {
    bytes: [u8; PENDING_BYTES],
    /// Откуда читать следующий байт.
    head: usize,
    /// Сколько байт лежит.
    len: usize,
}

impl Pending {
    const fn new() -> Self {
        Self { bytes: [0; PENDING_BYTES], head: 0, len: 0 }
    }

    /// Положить байт. `false` — кольцо полно, и байт потерян.
    ///
    /// Теряется **новый**, а не самый старый, по той же причине, что и в
    /// очереди событий: начало ввода уже показано эхом, и выбросив его, мы
    /// получили бы строку, не совпадающую с тем, что видит человек.
    fn push(&mut self, byte: u8) -> bool {
        if self.len == PENDING_BYTES {
            return false;
        }
        self.bytes[(self.head + self.len) % PENDING_BYTES] = byte;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % PENDING_BYTES;
        self.len -= 1;
        Some(byte)
    }
}

/// Кольцо между приёмником и разбором.
///
/// # Почему байты вообще нужно где-то держать
///
/// Приёмник вычитывается теперь не только из обработчика прерывания, но и
/// **изнутри печати** — там, где ядро ждёт освобождения передатчика с
/// запрещёнными прерываниями (см. [`rescue`]). Разбирать байт в этой точке
/// нельзя: разбор кончается [`super::post`], а тот берёт лок очереди и будит
/// планировщик. Печатающий же держит лок serial и, вообще говоря, любой другой
/// лок ядра — вплоть до лока самого планировщика, который тоже печатает. Вызов
/// разбора оттуда завёл бы порядок захвата «планировщик → serial → планировщик»
/// и повесил бы машину на соседнем процессоре.
///
/// Поэтому спасение и разбор разведены: из печати байты только **вынимаются** в
/// это кольцо, а разбираются на возврате из прерывания, где заведомо не
/// удерживается ни одного лока (см. [`crate::irq::on_trap_return`]).
///
/// # Единственный лок, который здесь берут
///
/// Критическая секция не трогает ничего, кроме самого кольца и регистров UART,
/// и ничего не печатает. Значит, взявший его не может ждать ничего другого, а
/// цепочки ожидания через него не бывает — что и делает законным взятие этого
/// лока из-под лока serial.
static PENDING: SpinLock<Pending> = SpinLock::new(Pending::new());

/// Сколько байт забирается из приёмника за один заход.
///
/// Тридцать два — глубина приёмного FIFO у PL011, самого ёмкого из двух наших
/// портов (у 16550 их шестнадцать). Больше бессмысленно, меньше означало бы
/// оставить в железе байты, за которыми придётся возвращаться.
const RX_BATCH: usize = 32;

/// Вычитать приёмник в кольцо. Возвращает, сколько байт забрано.
fn collect() -> usize {
    // Единственное условие — тот же порт, в который ядро согласно писать.
    // Признано отсутствующим — не трогаем: на AArch64 адрес до разбора ACPI
    // остаётся догадкой, и чтение по ней означало бы обращение к чужому
    // устройству (см. [`crate::serial::silence`]).
    //
    // Условием «приём уже поднят» (`sources().serial`) пользоваться нельзя, и
    // это выяснилось не сразу: между `enable_serial_rx` и `set_sources`
    // проходит десяток строк, прерывание приёмника в этот промежуток уже
    // доставляется, — и обработчик, ничего не вычитавший, оставил бы байты в
    // FIFO до первого тика.
    if crate::serial::absent() {
        return 0;
    }
    // `try_lock`, а не `lock`: единственный случай, когда лок держит **этот**
    // процессор, — это печать из обработчика отказа, вклинившаяся в чужое
    // спасение. Ждать там нечего, а потерять диагностику отказа нельзя.
    let Some(mut pending) = PENDING.try_lock() else {
        return 0;
    };
    let mut batch = [0u8; RX_BATCH];
    // Чтение железа идёт **под** локом кольца, и это не перестраховка: два
    // процессора, читающие одно FIFO, разложили бы байты в кольцо в том
    // порядке, в каком успели, — то есть переставили бы местами символы
    // команды.
    let taken = crate::arch::drain_serial_rx(&mut batch);
    for &byte in &batch[..taken] {
        if !pending.push(byte) {
            SPILLED.fetch_add(1, Ordering::Relaxed);
        }
    }
    taken
}

/// Вынуть из приёмника всё, что там есть, посреди печати.
///
/// Зовётся из [`crate::serial`] на каждый отправляемый байт. Это и есть лечение
/// переполнения: пока строка уходит в линию — а строка оболочки уходит десятки
/// миллисекунд, — приёмник опорожняется в темпе передатчика, и шестнадцати
/// байт FIFO перестаёт не хватать.
///
/// Цена — чтение регистра состояния на каждый переданный байт. На фоне
/// ожидания передатчика (87 мкс на байт при 115200) она не измерима.
pub fn rescue() {
    let taken = collect();
    if taken != 0 {
        RESCUED.fetch_add(taken as u64, Ordering::Relaxed);
    }
}

/// Вычитать приёмник, не считая это спасением. Зовёт обработчик прерывания.
pub fn take() {
    let _ = collect();
}

/// Разобрать всё, что накопилось в кольце.
///
/// Зовётся на возврате из внешнего прерывания — в точке, где заведомо не
/// удерживается ни одного лока. Оттуда же и требование к этой функции: лок
/// кольца отпускается **до** разбора байта, а не удерживается на весь проход.
/// Иначе разбор (а он доходит до планировщика) шёл бы из-под лока кольца, и
/// порядок захвата «кольцо → планировщик» встретился бы с уже существующим
/// «планировщик → serial → кольцо».
pub fn dispatch() {
    loop {
        let byte = {
            let Some(mut pending) = PENDING.try_lock() else {
                return;
            };
            match pending.pop() {
                Some(byte) => byte,
                None => return,
            }
        };
        feed(byte);
    }
}

/// Максимальное значение параметра CSI, которое имеет смысл копить.
///
/// Ограничение не декоративное: параметр приходит из-за границы доверия (это
/// байты в линии), и без предела `param * 10 + digit` переполнится, а в отладке
/// сборке это паника — то есть падение ядра от мусора в порту.
const MAX_CSI_PARAM: u32 = 1000;

/// Скормить декодеру один принятый байт.
///
/// Зовётся только из [`dispatch`], то есть в точке, где не удерживается ни
/// одного лока. Прямо из драйвера байты сюда больше не попадают: почему —
/// написано у [`PENDING`]. Функция сама решает, породить ли событие: часть
/// байтов — это середина escape-последовательности, и события от них быть не
/// должно.
fn feed(byte: u8) {
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
