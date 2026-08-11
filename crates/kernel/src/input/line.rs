//! Редактор строки: то, что в Unix называется канонической дисциплиной линии.
//!
//! Собирает символы до Enter, показывает набранное эхом и умеет стирать. Ровно
//! этим и отличается «терминал» от «потока байтов»: пока строка не отправлена,
//! пользователь вправе её править, и ни один потребитель не должен видеть
//! промежуточных состояний.
//!
//! # Почему эхо здесь, а не в потребителе
//!
//! Потому что эхо и правка — одна и та же операция, разделить их нельзя. Стереть
//! символ означает и убрать байт из буфера, и убрать его с экрана; сделай это
//! два разных модуля — и первое же расхождение даст строку, не совпадающую с
//! тем, что видит человек. Именно поэтому в Unix эхо делает драйвер терминала, а
//! не оболочка.
//!
//! Эхо отключаемо ([`LineEditor::without_echo`]) — ввод пароля в будущем
//! установщике потребует именно этого.
//!
//! # Почему буфер фиксированный
//!
//! Строку набирает внешний источник, а его скорость ядро не контролирует.
//! Растущий буфер означает, что залипшая клавиша (или скрипт, льющий байты в
//! серийный порт) съедает кучу до отказа аллокатора. Фиксированные 256 байт
//! невозможно переполнить: лишний символ отвергается, и об этом слышно.

use super::{KeyCode, KeyEvent};
use crate::kprint;

/// Сколько байт помещается в строку.
///
/// 256 — с запасом на команду с несколькими путями; при 80 столбцах экрана это
/// больше трёх видимых строк.
pub const MAX_LINE: usize = 256;

/// Что произошло с редактором после обработки события.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit {
    /// Событие не изменило состояние (отпускание клавиши, модификатор,
    /// неизвестная клавиша).
    Ignored,
    /// Символ добавлен в строку.
    Inserted,
    /// Символ удалён.
    Erased,
    /// Буфер полон, символ отвергнут.
    Full,
    /// Нажат Enter: строка готова к разбору, см. [`LineEditor::as_str`].
    Submitted,
    /// Ctrl+C: строка отменена и уже очищена.
    Cancelled,
    /// Ctrl+D на пустой строке — конец ввода.
    EndOfInput,
    /// Клавиша не относится к редактированию текста (стрелки, F-клавиши).
    /// Потребитель вправе обработать её сам.
    Unhandled(KeyCode),
}

/// Накопитель строки с эхом.
pub struct LineEditor {
    buf: [u8; MAX_LINE],
    len: usize,
    echo: bool,
}

impl LineEditor {
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: [0; MAX_LINE], len: 0, echo: true }
    }

    /// Редактор, не показывающий набранное.
    #[must_use]
    pub const fn without_echo() -> Self {
        Self { buf: [0; MAX_LINE], len: 0, echo: false }
    }

    /// Набранное на данный момент.
    ///
    /// Всегда корректный UTF-8: в буфер попадают только целые символы через
    /// [`char::encode_utf8`], а стирание снимает символ целиком.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY-эквивалент без unsafe: `from_utf8` не может отказать по
        // построению буфера, но проверка стоит наносекунды, а `unsafe` в
        // разборе пользовательского ввода не стоит ничего экономить.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Очистить строку, не трогая экран.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Обработать событие клавиатуры.
    pub fn handle(&mut self, event: KeyEvent) -> Edit {
        if !event.pressed {
            return Edit::Ignored;
        }

        match event.code {
            KeyCode::Enter | KeyCode::KeypadEnter => {
                // Перевод строки печатается всегда, даже без эха: иначе вывод
                // потребителя приклеился бы к приглашению.
                kprint!("\n");
                return Edit::Submitted;
            }
            KeyCode::Backspace => return self.erase(),
            KeyCode::Escape
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Delete
            | KeyCode::Insert => return Edit::Unhandled(event.code),
            _ => {}
        }

        let Some(ch) = event.to_char() else {
            // Модификаторы, F-клавиши, всё, чему нет символа в раскладке.
            return Edit::Ignored;
        };

        match ch {
            // Ctrl+C — отмена набранного. Строка стирается и с экрана тоже:
            // оставить её висеть значило бы показывать текст, которого больше
            // нет ни в одном буфере.
            '\u{3}' => {
                kprint!("^C\n");
                self.len = 0;
                return Edit::Cancelled;
            }
            // Ctrl+D означает конец ввода только на пустой строке — так же, как
            // в любом tty. На непустой он традиционно отправляет строку без
            // перевода, но такое поведение нужно оболочкам, а не ядру, поэтому
            // здесь он просто игнорируется.
            '\u{4}' => {
                if self.len == 0 {
                    kprint!("^D\n");
                    return Edit::EndOfInput;
                }
                return Edit::Ignored;
            }
            // Ctrl+U — стереть строку целиком; классическая привычка из tty.
            '\u{15}' => {
                while self.len > 0 {
                    self.erase();
                }
                return Edit::Erased;
            }
            // Табуляция в строке ядра пользы не приносит, а ширину на экране
            // имеет переменную — то есть ломает подсчёт символов для стирания.
            '\t' => return Edit::Ignored,
            // Прочие управляющие символы: в текст им нельзя, эхом — тем более.
            _ if (ch as u32) < 0x20 => return Edit::Ignored,
            _ => {}
        }

        self.insert(ch)
    }

    fn insert(&mut self, ch: char) -> Edit {
        let mut utf8 = [0u8; 4];
        let encoded = ch.encode_utf8(&mut utf8).as_bytes();
        if self.len + encoded.len() > MAX_LINE {
            return Edit::Full;
        }
        self.buf[self.len..self.len + encoded.len()].copy_from_slice(encoded);
        self.len += encoded.len();
        if self.echo {
            kprint!("{ch}");
        }
        Edit::Inserted
    }

    fn erase(&mut self) -> Edit {
        if self.len == 0 {
            return Edit::Ignored;
        }
        // Снимаем весь символ, а не байт: продолжения UTF-8 имеют вид 10xxxxxx,
        // и остановиться на них значило бы оставить в буфере обрубок, из
        // которого `as_str` уже не соберётся.
        self.len -= 1;
        while self.len > 0 && self.buf[self.len] & 0b1100_0000 == 0b1000_0000 {
            self.len -= 1;
        }
        if self.echo {
            // Возврат-пробел-возврат: сам возврат каретки курсор двигает, но
            // символ на экране не стирает.
            kprint!("\u{8} \u{8}");
        }
        Edit::Erased
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}
