//! Клиент монитора QEMU (HMP) — руки и глаза стенда.
//!
//! Через монитор стенд делает две вещи, которых больше нечем сделать снаружи:
//! нажимает клавиши (`sendkey`) и снимает экран (`screendump`).
//!
//! # Почему клавиши, а не байты в серийный порт
//!
//! Потому что это разные пути в системе. Байт в UART попадает в драйвер
//! последовательной линии; `sendkey` порождает настоящий scancode на настоящем
//! эмулируемом контроллере, то есть проходит через USB HID (или PS/2) и через
//! перевод usage → [`KeyCode`](крейт ядра) — код, ради которого фаза 6 и
//! существовала. Проверять клавиатуру байтами в UART значит не проверять её
//! вовсе.
//!
//! # Ловушка, стоившая целого прогона
//!
//! `sendkey` доходит **ровно до одной** клавиатуры. Если к машине подключены и
//! PS/2, и USB, QEMU выбирает PS/2, и «проверка USB» молча проверяет i8042.
//! Отсюда `-machine q35,i8042=off` в сценариях, которым нужен именно USB; на
//! `virt` PS/2 не существует, и флаг там не нужен.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Приглашение монитора. По нему определяется конец ответа: своей длины ответ
/// не сообщает, а закрытия соединения ждать нельзя — оно живёт весь прогон.
const PROMPT: &str = "(qemu) ";

/// Сколько ждать ответа на команду.
///
/// Секунды, а не миллисекунды: `screendump` на экране 1024×768 пишет два с
/// лишним мегабайта на диск, и на холодной файловой системе это не мгновение.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Monitor {
    stream: TcpStream,
    /// Прочитанное, но ещё не разобранное.
    pending: String,
}

impl Monitor {
    /// Дождаться, пока QEMU подключится к нашему сокету, и проглотить баннер.
    pub fn accept(listener: &TcpListener, timeout: Duration) -> Result<Self> {
        let stream = accept_with_timeout(listener, timeout).context("монитор QEMU не подключился")?;
        let mut monitor = Self { stream, pending: String::new() };
        // Баннер («QEMU 9.x monitor — type 'help'…») заканчивается приглашением.
        // Не прочитать его здесь значит получить его в ответе на первую команду.
        monitor.read_until_prompt(COMMAND_TIMEOUT).context("монитор не выдал приглашение")?;
        Ok(monitor)
    }

    /// Прочитать всё до ближайшего приглашения.
    fn read_until_prompt(&mut self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; 4096];

        loop {
            if let Some(index) = self.pending.find(PROMPT) {
                let answer = self.pending[..index].to_string();
                self.pending = self.pending[index + PROMPT.len()..].to_string();
                return Ok(answer);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("монитор молчит дольше {} с; получено: {:?}", timeout.as_secs(), self.pending);
            }
            self.stream
                .set_read_timeout(Some(remaining.min(Duration::from_millis(250))))
                .context("не удалось выставить таймаут чтения монитора")?;

            match self.stream.read(&mut buffer) {
                Ok(0) => bail!("монитор закрыл соединение (QEMU завершился?)"),
                Ok(count) => {
                    // Монитор — текстовый протокол, но байты приходят кусками и
                    // граница куска может разрезать UTF-8. Потери здесь
                    // безобидны: искажённый символ в диагностике, а не в данных.
                    self.pending.push_str(&String::from_utf8_lossy(&buffer[..count]));
                }
                // Таймаут чтения — не ошибка: ответ ещё не пришёл целиком.
                Err(err) if err.kind() == ErrorKind::WouldBlock || err.kind() == ErrorKind::TimedOut => {}
                Err(err) => return Err(err).context("не удалось прочитать ответ монитора"),
            }
        }
    }

    /// Выполнить команду монитора и вернуть её ответ без эха.
    pub fn command(&mut self, command: &str) -> Result<String> {
        self.stream
            .write_all(format!("{command}\n").as_bytes())
            .with_context(|| format!("не удалось отправить монитору '{command}'"))?;
        self.stream.flush().ok();

        let answer = self
            .read_until_prompt(COMMAND_TIMEOUT)
            .with_context(|| format!("нет ответа монитора на '{command}'"))?;

        Ok(strip_echo(&answer, command))
    }

    /// Выполнить команду, для которой любой ответ означает ошибку.
    fn command_silent(&mut self, command: &str) -> Result<()> {
        let answer = self.command(command)?;
        if !answer.is_empty() {
            bail!("монитор ответил на '{command}': {answer}");
        }
        Ok(())
    }

    /// Нажать клавишу. Имя — в терминах QEMU (`a`, `ret`, `shift-h`, `spc`).
    pub fn sendkey(&mut self, keys: &str) -> Result<()> {
        self.command_silent(&format!("sendkey {keys}"))
    }

    /// Сдвинуть мышь на заданное приращение.
    ///
    /// Именно приращение: подключена обычная мышь, а не планшет, и абсолютных
    /// координат у неё нет. Стенд поэтому не «ставит курсор в точку», а везёт
    /// его туда — как это делает рука.
    pub fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<()> {
        self.command_silent(&format!("mouse_move {dx} {dy}"))
    }

    /// Нажать или отпустить кнопки мыши. Битовая карта: 1 — левая, 2 — правая,
    /// 4 — средняя; ноль означает «все отпущены».
    pub fn mouse_button(&mut self, mask: u32) -> Result<()> {
        self.command_silent(&format!("mouse_button {mask}"))
    }

    /// Снять экран в файл PPM.
    pub fn screendump(&mut self, path: &Path) -> Result<()> {
        // Прямая косая работает и на Windows, а обратная в HMP выглядит как
        // экранирование. Пробелы неустранимы: аргумент-имя файла читается до
        // первого пробела, кавычек этот разбор не знает.
        let text = path.to_string_lossy().replace('\\', "/");
        if text.chars().any(char::is_whitespace) {
            bail!(
                "путь снимка содержит пробел: {text}\n\
                 Монитор QEMU разбирает имя файла до первого пробела, \
                 закавычить его нельзя."
            );
        }
        // Формат не задаётся: `-f png` понимают не все сборки QEMU, а ошибка
        // разбора выглядит как «invalid char in expression» и не объясняет
        // ничего. PPM умеют все, а перевод в PNG делает сам стенд.
        self.command_silent(&format!("screendump {text}"))
    }
}

/// Отделить ответ монитора от эха.
///
/// Монитор построен на readline и **перерисовывает строку целиком после каждого
/// символа**: в ответе на команду из сорока знаков приезжает сорок её префиксов
/// вперемешку с `ESC[D` и `ESC[K`. Отфильтровать это построчно нельзя — переводов
/// строки там нет вовсе, и первая же попытка дала «монитор ответил» на успешную
/// команду.
///
/// Работает так: первое вхождение **полной** команды — это последняя, дописанная
/// до конца перерисовка; всё, что после неё, и есть ответ. Остатки управляющих
/// последовательностей убираются следом.
fn strip_echo(answer: &str, command: &str) -> String {
    let tail = match answer.find(command) {
        Some(index) => &answer[index + command.len()..],
        None => answer,
    };
    strip_ansi(tail).trim().to_string()
}

/// Убрать управляющие последовательности ANSI и одиночные управляющие символы.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // CSI: ESC [ параметры буква. Всё до финальной буквы включительно —
            // не текст.
            if chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() || next == '~' {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\r' || (ch.is_control() && ch != '\n') {
            continue;
        }
        out.push(ch);
    }
    out
}

/// Дождаться подключения с ограничением по времени.
///
/// У [`TcpListener`] таймаута приёма нет, поэтому сокет переводится в
/// неблокирующий режим и опрашивается. Принятое соединение возвращается в
/// блокирующий режим: дальше им управляют таймауты чтения.
pub fn accept_with_timeout(listener: &TcpListener, timeout: Duration) -> Result<TcpStream> {
    listener
        .set_nonblocking(true)
        .context("не удалось перевести сокет в неблокирующий режим")?;
    let deadline = Instant::now() + timeout;

    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .context("не удалось вернуть сокет в блокирующий режим")?;
                listener.set_nonblocking(false).ok();
                return Ok(stream);
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("никто не подключился за {} с", timeout.as_secs());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err).context("не удалось принять подключение"),
        }
    }
}
