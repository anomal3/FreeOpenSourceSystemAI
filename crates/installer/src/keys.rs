//! Клавиатура через `EFI_SIMPLE_TEXT_INPUT_PROTOCOL`.
//!
//! Собственного драйвера здесь нет и не будет — в этом весь смысл того, что
//! установщик отдельное UEFI-приложение. Ввод даёт прошивка, и она же уже
//! разобралась, PS/2 это, USB или последовательная линия. Готовность
//! установщика тем самым развязана с готовностью драйверов ядра: он работал бы
//! и до того, как в ядре появился xHCI.
//!
//! Побочная выгода — последовательная линия тоже ConIn, поэтому установщиком
//! можно управлять из терминала, из которого запущен QEMU.

use core::time::Duration;

use uefi::proto::console::text::{Key as UefiKey, ScanCode};
use uefi::{boot, system};

/// Нажатие в том виде, в каком его понимает интерфейс.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    /// Печатный символ.
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    /// Всё остальное — чтобы вызывающий мог не разбирать его и не заметить.
    Other,
}

/// Пауза между опросами.
///
/// Опрос, а не ожидание события. `WaitForKey` был бы «правильнее», но требует
/// провести хендл события через замыкание `with_stdin`, а выигрыш — несколько
/// миллисекунд простоя процессора в программе, которая всё равно ждёт человека.
/// Десять миллисекунд не заметны на нажатии и не сжигают такты впустую.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Забрать нажатие, если оно уже есть.
#[must_use]
pub fn poll() -> Option<Key> {
    let key = system::with_stdin(|stdin| stdin.read_key().ok().flatten())?;
    Some(translate(key))
}

/// Дождаться нажатия.
#[must_use]
pub fn wait() -> Key {
    loop {
        if let Some(key) = poll() {
            return key;
        }
        boot::stall(POLL_INTERVAL);
    }
}

fn translate(key: UefiKey) -> Key {
    match key {
        UefiKey::Printable(ch) => {
            let ch = char::from(ch);
            match ch {
                // Прошивка отдаёт Enter как CR, а не LF: терминальное
                // наследство, и путать их здесь — значит не заметить Enter,
                // пришедший с последовательной линии.
                '\r' | '\n' => Key::Enter,
                '\u{8}' | '\u{7F}' => Key::Backspace,
                '\t' => Key::Tab,
                '\u{1B}' => Key::Escape,
                _ => Key::Char(ch),
            }
        }
        UefiKey::Special(code) => match code {
            ScanCode::UP => Key::Up,
            ScanCode::DOWN => Key::Down,
            ScanCode::LEFT => Key::Left,
            ScanCode::RIGHT => Key::Right,
            ScanCode::ESCAPE => Key::Escape,
            ScanCode::DELETE => Key::Backspace,
            _ => Key::Other,
        },
    }
}
