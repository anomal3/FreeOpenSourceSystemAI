//! Редактор строки: то, что в Unix называется канонической дисциплиной линии,
//! и то, что делает из неё readline (bash) и PSReadLine (PowerShell).
//!
//! Собирает символы до Enter и показывает набранное эхом. С терминала T1 —
//! курсор внутри строки: правка в любом месте, переходы по словам, вырезание
//! и вставка, режим замены. Раскладка клавиш — та, к которой привыкли в bash и
//! PowerShell:
//!
//! | Клавиши | Что делают |
//! |---|---|
//! | ←, →, Ctrl+B, Ctrl+F | символ влево, вправо |
//! | Ctrl+←, Ctrl+→ | слово влево, вправо |
//! | Home, End, Ctrl+A, Ctrl+E | начало, конец строки |
//! | Backspace, Delete | стереть символ перед курсором, под курсором |
//! | Ctrl+Backspace, Ctrl+W | вырезать слово перед курсором |
//! | Ctrl+Delete | вырезать слово после курсора |
//! | Ctrl+U, Ctrl+K | вырезать до начала, до конца строки |
//! | Ctrl+Y | вставить вырезанное |
//! | Esc | очистить строку (как в PowerShell) |
//! | Insert | вставка или замена |
//! | Ctrl+C | отменить строку |
//! | Ctrl+D | конец ввода на пустой строке, иначе стереть под курсором |
//!
//! Стрелки вверх и вниз, Tab, Ctrl+R, Ctrl+L и прочее, что касается не
//! строки, а оболочки (история, дополнение, экран), редактор отдаёт наружу:
//! [`Edit::Unhandled`].
//!
//! # Почему эхо здесь, а не в потребителе
//!
//! Потому что эхо и правка — одна и та же операция, разделить их нельзя.
//! Стереть символ означает и убрать байт из буфера, и убрать его с экрана;
//! сделай это два разных модуля — и первое же расхождение даст строку, не
//! совпадающую с тем, что видит человек. Именно поэтому в Unix эхо делает
//! драйвер терминала, а не оболочка.
//!
//! # Как перерисовывается строка
//!
//! Экран — сетка с переносом по правому краю, и строка длиннее окна занимает
//! несколько рядов. Редактор помнит, где сейчас курсор терминала (смещение в
//! символах от начала приглашения, [`LineEditor::at`]), и всякая правка — это
//! «дойти до места правки, напечатать хвост строки, стереть лишнее
//! (`ESC [ J`), вернуться к курсору». Переход между местами — рядами вверх или
//! вниз (`ESC [ A`/`B`) и колонкой от начала ряда (`\r`, `ESC [ C`): он верен и
//! через перенос, где `\b` останавливается у левого края.
//!
//! Сетка переносит отложенно: символ в последней колонке оставляет курсор за
//! краем, и вниз он уходит только со следующим символом. Это помнит
//! [`LineEditor::pending`].
//!
//! # Почему буфер фиксированный
//!
//! Строку набирает внешний источник, а его скорость ядро не контролирует.
//! Растущий буфер означает, что залипшая клавиша (или скрипт, льющий байты в
//! серийный порт) съедает кучу до отказа аллокатора. Фиксированные 256 байт
//! невозможно переполнить: лишний символ отвергается, и об этом слышно.

use core::fmt::Write;

use super::{KeyCode, KeyEvent, Modifiers};

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
    /// Что-то стёрто или вырезано.
    Erased,
    /// Курсор сдвинулся, текст прежний.
    Moved,
    /// Буфер полон, символ отвергнут.
    Full,
    /// Нажат Enter: строка готова к разбору, см. [`LineEditor::as_str`].
    Submitted,
    /// Ctrl+C: строка отменена и уже очищена.
    Cancelled,
    /// Ctrl+D на пустой строке — конец ввода.
    EndOfInput,
    /// Клавиша не о правке строки (стрелки вверх и вниз, Tab, F-клавиши,
    /// Ctrl+R, Ctrl+L) — её разбирает потребитель, с модификаторами.
    Unhandled(KeyEvent),
}

/// Накопитель строки с эхом.
pub struct LineEditor {
    buf: [u8; MAX_LINE],
    len: usize,
    /// Курсор — байт в [`Self::buf`], всегда на границе символа.
    cursor: usize,
    echo: bool,
    /// Ширина приглашения на экране (без управляющих последовательностей).
    prompt_cols: usize,
    /// Ширина окна в символах; ноль — неизвестна (серийная консоль без
    /// переноса), и переход между рядами не нужен.
    cols: usize,
    /// Где курсор терминала: символов от начала приглашения.
    at: usize,
    /// Курсор стоит за правым краем после символа в последней колонке и
    /// уйдёт на следующий ряд только со следующим символом.
    pending: bool,
    /// Режим замены (Insert).
    overwrite: bool,
    /// Вырезанное (Ctrl+K, Ctrl+U, Ctrl+W) — для Ctrl+Y.
    kill: [u8; MAX_LINE],
    kill_len: usize,
    /// Серая подсказка после строки (терминал T3, как Predictive IntelliSense
    /// в PowerShell): → или End в конце строки её принимают.
    hint: [u8; MAX_LINE],
    hint_len: usize,
}

impl LineEditor {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_LINE],
            len: 0,
            cursor: 0,
            echo: true,
            prompt_cols: 0,
            cols: 0,
            at: 0,
            pending: false,
            overwrite: false,
            kill: [0; MAX_LINE],
            kill_len: 0,
            hint: [0; MAX_LINE],
            hint_len: 0,
        }
    }

    /// Редактор, не показывающий набранное.
    #[must_use]
    pub const fn without_echo() -> Self {
        let mut editor = Self::new();
        editor.echo = false;
        editor
    }

    /// Набранное на данный момент.
    ///
    /// Всегда корректный UTF-8: в буфер попадают только целые символы через
    /// [`char::encode_utf8`], а стирание снимает символ целиком.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    /// Набранное до курсора — для дополнения (Tab).
    #[must_use]
    pub fn before_cursor(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.cursor]).unwrap_or("")
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Курсор в конце строки.
    #[must_use]
    pub const fn at_end(&self) -> bool {
        self.cursor == self.len
    }

    /// Очистить строку, не трогая экран.
    pub fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
        self.hint_len = 0;
    }

    /// Показать серую подсказку `text` после строки (`None` — убрать).
    /// Только при курсоре в конце: подсказка продолжает набранное.
    pub fn set_hint(&mut self, text: Option<&str>, out: &mut impl Write) {
        let text = text.filter(|_| self.cursor == self.len && self.echo).unwrap_or("");
        let same = core::str::from_utf8(&self.hint[..self.hint_len]).is_ok_and(|old| old == text);
        if same {
            return;
        }
        let end = self.offset(self.len);
        if self.hint_len > 0 {
            self.go(out, end);
            let _ = out.write_str("\x1b[J");
        }
        self.hint_len = 0;
        if text.is_empty() || self.len + text.len() > MAX_LINE {
            return;
        }
        self.hint[..text.len()].copy_from_slice(text.as_bytes());
        self.hint_len = text.len();
        self.go(out, end);
        let _ = out.write_str("\x1b[90m");
        self.put(out, text);
        let _ = out.write_str("\x1b[0m");
        self.go(out, end);
    }

    /// Принять подсказку целиком (`word` — до конца её первого слова).
    fn accept_hint(&mut self, word: bool, out: &mut impl Write) -> Edit {
        let mut text = [0u8; MAX_LINE];
        let mut len = self.hint_len;
        text[..len].copy_from_slice(&self.hint[..len]);
        if word {
            let skip = text[..len].iter().take_while(|byte| !Self::is_word(**byte)).count();
            len = skip + text[skip..len].iter().take_while(|byte| Self::is_word(**byte)).count();
        }
        let text = core::str::from_utf8(&text[..len]).unwrap_or("");
        if self.insert_str(text, out) { Edit::Inserted } else { Edit::Full }
    }

    /// Заменить слово `[start, курсор)` текстом (дополнение по Tab).
    pub fn replace_word(&mut self, start: usize, text: &str, out: &mut impl Write) -> bool {
        let start = start.min(self.cursor);
        let removed = self.cursor - start;
        if self.len - removed + text.len() > MAX_LINE {
            return false;
        }
        let old = self.offset(self.len) + self.hint_cols();
        self.hint_len = 0;
        self.buf.copy_within(self.cursor..self.len, start + text.len());
        self.buf[start..start + text.len()].copy_from_slice(text.as_bytes());
        self.len = self.len - removed + text.len();
        self.cursor = start + text.len();
        self.redraw_from(out, start, old);
        true
    }

    /// Курсор терминала — в начало приглашения (Ctrl+R перепечатывает
    /// приглашение другим).
    pub fn to_prompt_start(&mut self, out: &mut impl Write) {
        self.go(out, 0);
    }

    fn hint_cols(&self) -> usize {
        core::str::from_utf8(&self.hint[..self.hint_len]).map_or(0, |hint| hint.chars().count())
    }

    /// Приглашение только что напечатано: курсор терминала стоит сразу за
    /// ним. `prompt_cols` — его видимая ширина, `cols` — ширина окна.
    /// Уже набранное (см. [`Self::preload`]) считается напечатанным следом.
    pub fn begin(&mut self, prompt_cols: usize, cols: usize) {
        self.prompt_cols = prompt_cols;
        self.cols = cols;
        self.at = self.offset(self.len);
        self.pending = self.cols != 0 && self.at != 0 && self.at % self.cols == 0;
        self.cursor = self.len;
    }

    /// Дописать уже набранное, **не показывая его повторно**.
    ///
    /// Нужно оболочке: пока работала программа переднего плана, человек набирал
    /// следующую команду, и эхо этих байтов на экране уже есть. Показать их
    /// второй раз значило бы удвоить строку на глазах у того, кто её набирал.
    ///
    /// Возвращает, сколько байт принято: лишнее в буфер не помещается и
    /// отбрасывается — так же, как отбрасывается лишний символ при наборе.
    pub fn preload(&mut self, bytes: &[u8]) -> usize {
        let room = MAX_LINE - self.len;
        let mut take = bytes.len().min(room);
        // Только целые символы: обрубок UTF-8 сделал бы строку нечитаемой.
        while take > 0 && core::str::from_utf8(&bytes[..take]).is_err() {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&bytes[..take]);
        self.len += take;
        self.cursor = self.len;
        take
    }

    /// Заменить строку целиком и показать (история, дополнение). Курсор — в
    /// конец. Не поместившееся отбрасывается.
    pub fn replace(&mut self, text: &str, out: &mut impl Write) {
        let old = self.offset(self.len);
        let mut take = text.len().min(MAX_LINE);
        while !text.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[..take].copy_from_slice(&text.as_bytes()[..take]);
        self.len = take;
        self.cursor = take;
        self.redraw_from(out, 0, old);
    }

    /// Вставить текст в курсор (дополнение). `false` — не поместился.
    pub fn insert_str(&mut self, text: &str, out: &mut impl Write) -> bool {
        if self.len + text.len() > MAX_LINE {
            return false;
        }
        let at = self.cursor;
        let old = self.offset(self.len);
        self.buf.copy_within(at..self.len, at + text.len());
        self.buf[at..at + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        self.cursor += text.len();
        self.redraw_from(out, at, old);
        true
    }

    /// Показать строку заново целиком под новым приглашением — после того,
    /// как поверх неё что-то напечатали (список вариантов дополнения).
    pub fn reprint(&mut self, prompt_cols: usize, cols: usize, out: &mut impl Write) {
        self.prompt_cols = prompt_cols;
        self.cols = cols;
        self.at = prompt_cols;
        self.pending = false;
        self.redraw_from(out, 0, prompt_cols);
    }

    /// Обработать событие клавиатуры, отправляя эхо в `out`.
    pub fn handle(&mut self, event: KeyEvent, out: &mut impl Write) -> Edit {
        // Alt+Shift переключает раскладку и здесь — на консоли без стола
        // редактор строки первым видит клавиши. На столе сочетание перехватит
        // стол, и сюда оно не дойдёт; дважды не переключится.
        if super::keymap::observe(event) {
            return Edit::Ignored;
        }
        if !event.pressed {
            return Edit::Ignored;
        }
        let ctrl = event.mods.contains(Modifiers::CTRL);

        match event.code {
            KeyCode::Enter | KeyCode::KeypadEnter => {
                let end = self.offset(self.len);
                if self.hint_len > 0 {
                    self.go(out, end);
                    let _ = out.write_str("\x1b[J");
                    self.hint_len = 0;
                }
                self.go(out, end);
                // Перевод строки печатается всегда, даже без эха: иначе вывод
                // потребителя приклеился бы к приглашению.
                let _ = out.write_str("\n");
                self.at = 0;
                self.pending = false;
                return Edit::Submitted;
            }
            KeyCode::Backspace if ctrl => return self.kill_word_back(out),
            KeyCode::Backspace => return self.erase_back(out),
            KeyCode::Delete if ctrl => return self.kill_word_forward(out),
            KeyCode::Delete => return self.erase_forward(out),
            KeyCode::Right | KeyCode::End if self.cursor == self.len && self.hint_len > 0 => {
                return self.accept_hint(ctrl && event.code == KeyCode::Right, out);
            }
            KeyCode::Left if ctrl => return self.move_to(self.word_left(), out),
            KeyCode::Right if ctrl => return self.move_to(self.word_right(), out),
            KeyCode::Left => return self.move_to(self.char_left(), out),
            KeyCode::Right => return self.move_to(self.char_right(), out),
            KeyCode::Home => return self.move_to(0, out),
            KeyCode::End => return self.move_to(self.len, out),
            KeyCode::Insert => {
                self.overwrite = !self.overwrite;
                return Edit::Moved;
            }
            KeyCode::Escape => {
                if self.len == 0 {
                    return Edit::Ignored;
                }
                self.replace("", out);
                return Edit::Erased;
            }
            // Строки они не касаются: история, дополнение, прокрутка — дело
            // оболочки.
            KeyCode::Tab
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::F1
            | KeyCode::F2
            | KeyCode::F3
            | KeyCode::F4
            | KeyCode::F5
            | KeyCode::F6
            | KeyCode::F7
            | KeyCode::F8
            | KeyCode::F9
            | KeyCode::F10
            | KeyCode::F11
            | KeyCode::F12 => return Edit::Unhandled(event),
            _ => {}
        }

        let Some(ch) = event.to_char() else {
            // Модификаторы и всё, чему нет символа в раскладке.
            return Edit::Ignored;
        };

        match ch {
            // Ctrl+C — отмена набранного: строка остаётся на экране с `^C`, как
            // в bash, и начинается новая.
            '\u{3}' => {
                let end = self.offset(self.len);
                self.go(out, end);
                if self.hint_len > 0 {
                    let _ = out.write_str("\x1b[J");
                }
                let _ = out.write_str("^C\n");
                self.clear();
                self.at = 0;
                self.pending = false;
                Edit::Cancelled
            }
            // Ctrl+D: на пустой строке — конец ввода, иначе стереть под
            // курсором (bash).
            '\u{4}' => {
                if self.len == 0 {
                    let _ = out.write_str("^D\n");
                    return Edit::EndOfInput;
                }
                self.erase_forward(out)
            }
            '\u{1}' => self.move_to(0, out),
            '\u{5}' => self.move_to(self.len, out),
            '\u{2}' => self.move_to(self.char_left(), out),
            '\u{6}' => self.move_to(self.char_right(), out),
            '\u{15}' => self.kill_range(0, self.cursor, out),
            '\u{b}' => self.kill_range(self.cursor, self.len, out),
            '\u{17}' => self.kill_word_back(out),
            '\u{19}' => self.yank(out),
            // Ctrl+L (экран), Ctrl+R (поиск по истории), Ctrl+P/N (история) —
            // оболочке.
            '\u{c}' | '\u{12}' | '\u{10}' | '\u{e}' => Edit::Unhandled(event),
            // Сюда попадает Ctrl+I: у него тот же код, что у табуляции, но
            // клавиша другая, и до ветки `KeyCode::Tab` выше он не доходит.
            '\t' => Edit::Ignored,
            // Прочие управляющие символы: в текст им нельзя, эхом — тем более.
            _ if (ch as u32) < 0x20 => Edit::Ignored,
            _ => self.insert(ch, out),
        }
    }

    // ---- правка ----------------------------------------------------------------

    fn insert(&mut self, ch: char, out: &mut impl Write) -> Edit {
        let mut utf8 = [0u8; 4];
        let encoded = ch.encode_utf8(&mut utf8).as_bytes();
        let at = self.cursor;
        let old = self.offset(self.len);
        // Замена: символ под курсором уходит, новый встаёт на его место.
        let replaced = if self.overwrite && at < self.len { self.char_right() - at } else { 0 };
        if self.len - replaced + encoded.len() > MAX_LINE {
            return Edit::Full;
        }
        self.buf.copy_within(at + replaced..self.len, at + encoded.len());
        self.buf[at..at + encoded.len()].copy_from_slice(encoded);
        self.len = self.len - replaced + encoded.len();
        self.cursor = at + encoded.len();
        let at_end = at + encoded.len() == self.len && replaced == 0;
        if self.echo && at_end && self.hint_len >= encoded.len() && self.hint[..encoded.len()] == *encoded && !self.pending_mismatch(at) {
            // Набирают то, что предлагает подсказка: знак ложится поверх её
            // серой буквы, а остаток подсказки уже на экране. Перерисовка
            // подсказки на каждый знак — сотня байт вывода на нажатие, и по
            // последовательной линии ввод, приходящий быстрее, терялся
            // (приёмник переполнялся, пока оболочка печатала).
            let mut tmp = [0u8; 4];
            self.put(out, ch.encode_utf8(&mut tmp));
            self.hint.copy_within(encoded.len()..self.hint_len, 0);
            self.hint_len -= encoded.len();
            return Edit::Inserted;
        }
        if self.echo && at_end && self.hint_len == 0 && !self.pending_mismatch(at) {
            // Самый частый случай — символ в конец строки: напечатать его, и
            // всё. Без перерисовки хвоста, которого нет.
            let mut tmp = [0u8; 4];
            self.put(out, ch.encode_utf8(&mut tmp));
        } else {
            self.redraw_from(out, at, old);
        }
        Edit::Inserted
    }

    /// Курсор терминала не там, где курсор строки, — печатать «на месте»
    /// нельзя.
    fn pending_mismatch(&self, at: usize) -> bool {
        self.at != self.offset(at)
    }

    fn erase_back(&mut self, out: &mut impl Write) -> Edit {
        if self.cursor == 0 {
            return Edit::Ignored;
        }
        let from = self.char_left();
        self.remove(from, self.cursor, out);
        Edit::Erased
    }

    fn erase_forward(&mut self, out: &mut impl Write) -> Edit {
        if self.cursor == self.len {
            return Edit::Ignored;
        }
        let to = self.char_right();
        self.remove(self.cursor, to, out);
        Edit::Erased
    }

    fn kill_word_back(&mut self, out: &mut impl Write) -> Edit {
        let from = self.word_left();
        self.kill_range(from, self.cursor, out)
    }

    fn kill_word_forward(&mut self, out: &mut impl Write) -> Edit {
        let to = self.word_right();
        self.kill_range(self.cursor, to, out)
    }

    /// Вырезать `[from, to)` в буфер вставки.
    fn kill_range(&mut self, from: usize, to: usize, out: &mut impl Write) -> Edit {
        if from >= to {
            return Edit::Ignored;
        }
        self.kill_len = to - from;
        self.kill[..self.kill_len].copy_from_slice(&self.buf[from..to]);
        self.remove(from, to, out);
        Edit::Erased
    }

    fn yank(&mut self, out: &mut impl Write) -> Edit {
        if self.kill_len == 0 {
            return Edit::Ignored;
        }
        let mut text = [0u8; MAX_LINE];
        text[..self.kill_len].copy_from_slice(&self.kill[..self.kill_len]);
        let text = core::str::from_utf8(&text[..self.kill_len]).unwrap_or("");
        if self.insert_str(text, out) { Edit::Inserted } else { Edit::Full }
    }

    /// Убрать `[from, to)`, курсор — в `from`.
    fn remove(&mut self, from: usize, to: usize, out: &mut impl Write) {
        let old = self.offset(self.len);
        self.buf.copy_within(to..self.len, from);
        self.len -= to - from;
        self.cursor = from;
        self.redraw_from(out, from, old);
    }

    fn move_to(&mut self, to: usize, out: &mut impl Write) -> Edit {
        if to == self.cursor {
            return Edit::Ignored;
        }
        self.cursor = to;
        let target = self.offset(to);
        self.go(out, target);
        Edit::Moved
    }

    // ---- границы ---------------------------------------------------------------

    fn char_left(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 {
            at -= 1;
            if self.buf[at] & 0b1100_0000 != 0b1000_0000 {
                break;
            }
        }
        at
    }

    fn char_right(&self) -> usize {
        let mut at = self.cursor;
        if at < self.len {
            at += 1;
            while at < self.len && self.buf[at] & 0b1100_0000 == 0b1000_0000 {
                at += 1;
            }
        }
        at
    }

    /// Слово — всё, кроме пробела и `/`: путь стирается по каталогу.
    fn is_word(byte: u8) -> bool {
        !matches!(byte, b' ' | b'\t' | b'/')
    }

    fn word_left(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && !Self::is_word(self.buf[at - 1]) {
            at -= 1;
        }
        while at > 0 && Self::is_word(self.buf[at - 1]) {
            at -= 1;
        }
        at
    }

    fn word_right(&self) -> usize {
        let mut at = self.cursor;
        while at < self.len && !Self::is_word(self.buf[at]) {
            at += 1;
        }
        while at < self.len && Self::is_word(self.buf[at]) {
            at += 1;
        }
        at
    }

    // ---- экран -----------------------------------------------------------------

    /// Смещение байта строки на экране: символов от начала приглашения.
    fn offset(&self, byte: usize) -> usize {
        let text = core::str::from_utf8(&self.buf[..byte]).unwrap_or("");
        self.prompt_cols + text.chars().count()
    }

    /// Напечатать текст с места, где стоит курсор терминала.
    fn put(&mut self, out: &mut impl Write, text: &str) {
        if !self.echo || text.is_empty() {
            return;
        }
        let _ = out.write_str(text);
        self.at += text.chars().count();
        self.pending = self.cols != 0 && self.at % self.cols == 0;
    }

    /// Перевести курсор терминала на смещение `to`.
    fn go(&mut self, out: &mut impl Write, to: usize) {
        if !self.echo || to == self.at {
            return;
        }
        if self.cols == 0 {
            // Без ширины окна — только по ряду.
            if to < self.at {
                let _ = write!(out, "\x1b[{}D", self.at - to);
            } else {
                let _ = write!(out, "\x1b[{}C", to - self.at);
            }
            self.at = to;
            return;
        }
        let cols = self.cols;
        let from_row = if self.pending { self.at / cols - 1 } else { self.at / cols };
        let (to_row, to_col) = (to / cols, to % cols);
        if to_row < from_row {
            let _ = write!(out, "\x1b[{}A", from_row - to_row);
        } else if to_row > from_row {
            let _ = write!(out, "\x1b[{}B", to_row - from_row);
        }
        let _ = out.write_str("\r");
        if to_col > 0 {
            let _ = write!(out, "\x1b[{to_col}C");
        }
        self.at = to;
        self.pending = false;
    }

    /// Перерисовать строку с байта `from`; `old_end` — где кончалась прежняя
    /// строка на экране (лишнее после новой стирается).
    fn redraw_from(&mut self, out: &mut impl Write, from: usize, old_end: usize) {
        if !self.echo {
            return;
        }
        // Подсказка относится к прежней строке: стирается вместе с хвостом,
        // новую покажет оболочка.
        let old_end = old_end.max(self.offset(self.len) + self.hint_cols());
        self.hint_len = 0;
        let start = self.offset(from);
        self.go(out, start);
        let mut tail = [0u8; MAX_LINE];
        let count = self.len - from;
        tail[..count].copy_from_slice(&self.buf[from..self.len]);
        let tail = core::str::from_utf8(&tail[..count]).unwrap_or("");
        self.put(out, tail);
        if old_end > self.at {
            let _ = out.write_str("\x1b[J");
        }
        let cursor = self.offset(self.cursor);
        if cursor != self.at {
            self.go(out, cursor);
        }
    }
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}
