//! Разбор управляющих последовательностей: то, что делает из окна терминал.
//!
//! # Что понимает и чего не понимает
//!
//! Подмножество ANSI X3.64 (оно же — то, что умеет любой VT100-совместимый
//! терминал): перемещение курсора, очистка экрана и строки, шестнадцать цветов,
//! показ и скрытие курсора. Ни прокрутки по областям, ни альтернативного экрана,
//! ни мыши: всё это существует ради программ, которых здесь пока нет, а
//! невыполненная последовательность хуже неизвестной — она обещает поведение,
//! которого не будет.
//!
//! Неизвестная последовательность **проглатывается целиком** и не печатается.
//! Это важнее, чем кажется: программа, попросившая то, чего мы не умеем, иначе
//! получила бы её текст на экране вперемешку со своим выводом — и выглядело бы
//! это как испорченный вывод программы, а не как незнакомая команда терминала.
//!
//! # Почему разбор в ядре, а не в `mini-ui`
//!
//! Потому что у него есть побочный эффект, которого крейт рисования иметь не
//! может: разобранное **называется в журнале**. Проверить эту фазу иначе нечем
//! — картинка с очищенным экраном и картинка с экраном, который никто не
//! очищал, отличаются только тем, что на них нарисовано, а снимок экрана
//! доказательством не является (правило дома, см. README).
//!
//! # Почему журнал не заливается
//!
//! Полноэкранная программа шлёт последовательности сотнями в секунду, и печатать
//! каждую значило бы сделать журнал нечитаемым ровно тогда, когда он нужен.
//! Поэтому называются первые [`LOG_LIMIT`] — этого хватает, чтобы увидеть, что
//! разбор работает, — а дальше терминал говорит об этом один раз и замолкает.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::kprintln;
use crate::sync::SpinLock;

use super::window::Window;

/// Сколько разобранных последовательностей называется в журнале.
const LOG_LIMIT: u64 = 32;

/// Сколько числовых параметров помещается в одной последовательности.
///
/// `ESC [ 1 ; 34 ; 47 m` — три; больше в подмножестве, которое здесь разобрано,
/// не встречается. Лишние параметры отбрасываются, а не роняют разбор: поток
/// приходит от программы, то есть из-за границы доверия.
const MAX_PARAMS: usize = 4;

/// Состояние автомата разбора.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Обычный текст.
    Ground,
    /// Получен `ESC`, ждём `[` или `O`.
    Escape,
    /// Получен `ESC [`, копим параметры до финального байта.
    Csi,
}

struct Parser {
    state: State,
    params: [u32; MAX_PARAMS],
    count: usize,
    /// Была ли у последовательности приватная приставка `?` — ею отмечены
    /// режимы DEC (`ESC [ ? 25 h` — показать курсор).
    private: bool,
}

impl Parser {
    const fn new() -> Self {
        Self { state: State::Ground, params: [0; MAX_PARAMS], count: 0, private: false }
    }

    fn reset(&mut self) {
        self.state = State::Ground;
        self.params = [0; MAX_PARAMS];
        self.count = 0;
        self.private = false;
    }

    /// Параметр по номеру; `default`, если его не было или он ноль.
    ///
    /// Ноль и отсутствие — одно и то же по спецификации: `ESC [ 0 A` и
    /// `ESC [ A` обе означают «на одну строку вверх».
    fn param(&self, index: usize, default: u32) -> u32 {
        match self.params.get(index) {
            Some(0) | None => default,
            Some(value) => *value,
        }
    }
}

static PARSER: SpinLock<Parser> = SpinLock::new(Parser::new());

/// Сколько последовательностей уже названо в журнале.
static LOGGED: AtomicU64 = AtomicU64::new(0);

/// Разобрать поток и применить его к окну.
///
/// Печатаемый текст уходит в сетку символов как раньше; управляющие
/// последовательности выполняются. `window` равное `None` означает, что
/// показать сейчас негде — стол занят перерисовкой или графики нет вовсе. Разбор
/// при этом идёт как обычно, и это существенно: состояние автомата принадлежит
/// терминалу, а не окну, и последовательность, разорванная пополам чужой
/// перерисовкой, обязана доехать целой.
pub fn feed(mut window: Option<&mut Window>, text: &str) {
    let mut parser = PARSER.lock();

    // Печатаемые куски копятся и уходят в окно одной строкой: вывод по символу
    // означал бы снятие и возврат курсора на каждый знак, то есть три записи в
    // поверхность вместо одной.
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut at = 0;

    while at < bytes.len() {
        let byte = bytes[at];

        if parser.state == State::Ground && byte != 0x1B {
            at += 1;
            continue;
        }

        // Дошли до управляющего байта: сначала выводим накопленное перед ним.
        if start < at {
            // Границы среза — байтовые, но резать по ним UTF-8 безопасно: и
            // `ESC`, и всё, что автомат ест дальше, — байты ASCII, а они не
            // бывают продолжением многобайтного символа.
            if let Some(window) = window.as_deref_mut() {
                window.write_str(&text[start..at]);
            }
        }
        at += 1;
        start = at;

        match parser.state {
            State::Ground => {
                parser.state = State::Escape;
                parser.params = [0; MAX_PARAMS];
                parser.count = 0;
                parser.private = false;
            }
            State::Escape => match byte {
                b'[' => parser.state = State::Csi,
                // `ESC O …` — последовательности VT100 в «прикладном» режиме
                // клавиатуры. На выводе они не значат ничего, но пришедшую
                // печатать нельзя: это команда, а не текст.
                b'O' => parser.reset(),
                // Что угодно ещё после ESC — короткая последовательность из
                // двух байтов; ни одна из них здесь не выполняется.
                _ => parser.reset(),
            },
            State::Csi => {
                match byte {
                    b'0'..=b'9' => {
                        let digit = u32::from(byte - b'0');
                        if parser.count == 0 {
                            parser.count = 1;
                        }
                        let index = parser.count - 1;
                        // Предел на значение параметра: он приходит от
                        // программы, и без него `value * 10 + digit`
                        // переполнится — то есть в отладочной сборке уронит
                        // ядро мусором из третьего кольца.
                        parser.params[index] = (parser.params[index] * 10 + digit).min(9999);
                    }
                    b';' => {
                        if parser.count == 0 {
                            parser.count = 1;
                        }
                        if parser.count < MAX_PARAMS {
                            parser.count += 1;
                        }
                    }
                    b'?' => parser.private = true,
                    // Промежуточные байты спецификации: их содержимое нас не
                    // касается, но доесть последовательность до финального
                    // байта обязательно.
                    0x20..=0x2F => {}
                    _ => {
                        apply(window.as_deref_mut(), &parser, byte);
                        parser.reset();
                    }
                }
            }
        }
    }

    if start < bytes.len() {
        if let Some(window) = window.as_deref_mut() {
            window.write_str(&text[start..]);
        }
    }
}

/// Выполнить разобранную последовательность.
///
/// Названа она в журнале в любом случае — даже когда применять её некуда: то,
/// что терминал **понял** команду, и то, что кадр в этот момент рисовался, —
/// разные утверждения, и проверяется снаружи первое.
fn apply(window: Option<&mut Window>, parser: &Parser, final_byte: u8) {
    let Some(window) = window else {
        log_only(parser, final_byte);
        return;
    };
    match (parser.private, final_byte) {
        // Курсор в заданное место. Нумерация в последовательности — с единицы
        // (так её задал DEC), внутри сетки — с нуля.
        (false, b'H' | b'f') => {
            let row = parser.param(0, 1).saturating_sub(1);
            let col = parser.param(1, 1).saturating_sub(1);
            window.term_move_to(row, col);
            log(format_args!("CSI {};{}H", row + 1, col + 1));
        }
        (false, b'A') => {
            let n = parser.param(0, 1) as i32;
            window.term_move_by(-n, 0);
            log(format_args!("CSI {n}A"));
        }
        (false, b'B') => {
            let n = parser.param(0, 1) as i32;
            window.term_move_by(n, 0);
            log(format_args!("CSI {n}B"));
        }
        (false, b'C') => {
            let n = parser.param(0, 1) as i32;
            window.term_move_by(0, n);
            log(format_args!("CSI {n}C"));
        }
        (false, b'D') => {
            let n = parser.param(0, 1) as i32;
            window.term_move_by(0, -n);
            log(format_args!("CSI {n}D"));
        }
        // Очистка экрана. Умолчание здесь — ноль, а не единица: `ESC [ J` без
        // параметра означает «от курсора до конца».
        (false, b'J') => {
            let mode = parser.params[0];
            window.term_erase_display(mode as u8);
            log(format_args!("CSI {mode}J"));
        }
        (false, b'K') => {
            let mode = parser.params[0];
            window.term_erase_line(mode as u8);
            log(format_args!("CSI {mode}K"));
        }
        (false, b'm') => {
            let count = parser.count.max(1);
            for index in 0..count {
                sgr(window, parser.params[index]);
            }
            log(format_args!("CSI {}m", parser.params[0]));
        }
        // Режимы DEC. Из всех нужен ровно один — видимость курсора.
        (true, b'h' | b'l') => {
            if parser.params[0] == 25 {
                let show = final_byte == b'h';
                window.set_cursor(show);
                log(format_args!("CSI ?25{}", if show { 'h' } else { 'l' }));
            }
        }
        // Всё остальное проглочено: см. заголовок модуля.
        _ => {}
    }
}

/// Назвать последовательность, которую применять некуда.
fn log_only(parser: &Parser, final_byte: u8) {
    if parser.private {
        if parser.params[0] == 25 {
            log(format_args!(
                "CSI ?25{}",
                if final_byte == b'h' { 'h' } else { 'l' }
            ));
        }
        return;
    }
    match final_byte {
        b'H' | b'f' => log(format_args!(
            "CSI {};{}H",
            parser.param(0, 1),
            parser.param(1, 1)
        )),
        b'A' | b'B' | b'C' | b'D' => log(format_args!(
            "CSI {}{}",
            parser.param(0, 1),
            final_byte as char
        )),
        b'J' | b'K' | b'm' => log(format_args!(
            "CSI {}{}",
            parser.params[0], final_byte as char
        )),
        _ => {}
    }
}

/// Один параметр `ESC [ … m`.
fn sgr(window: &mut Window, value: u32) {
    match value {
        0 => window.term_reset_attr(),
        30..=37 => window.term_set_fg((value - 30) as u8),
        // Яркая половина палитры: те же восемь цветов, сдвинутые на восемь.
        90..=97 => window.term_set_fg((value - 90 + 8) as u8),
        40..=47 => window.term_set_bg((value - 40) as u8),
        100..=107 => window.term_set_bg((value - 100 + 8) as u8),
        // Жирный (1) в шрифте 8×8 изобразить нечем, и подменять его яркостью
        // значило бы решать за программу, какой у неё цвет.
        _ => {}
    }
}

/// Назвать разобранное в журнале — пока их не стало слишком много.
fn log(args: core::fmt::Arguments<'_>) {
    let seen = LOGGED.fetch_add(1, Ordering::Relaxed);
    if seen < LOG_LIMIT {
        kprintln!("  term        : {args}");
    } else if seen == LOG_LIMIT {
        kprintln!("  term        : further sequences are parsed but no longer named");
    }
}
