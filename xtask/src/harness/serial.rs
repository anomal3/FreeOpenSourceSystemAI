//! Серийная линия гостя как канал стенда: чтение вывода и ввод строк.
//!
//! # Почему сокет, а не канал
//!
//! Потому что канал (pipe) на Windows съедает возврат каретки. Прежний стенд
//! был на PowerShell и писал в `StandardInput` процесса QEMU; три подряд
//! отправленных `\r` доходили до гостя как ничто, и путь «CR как Enter» —
//! ровно тот, которым живёт настоящий терминал, — не проверялся вовсе. Через
//! сокет байты доходят как есть, и этот путь снова проверяем.
//!
//! # Почему чтение в отдельном потоке
//!
//! Читать по запросу нельзя: пока стенд занят монитором или ждёт паузу, гость
//! продолжает писать, и буфер сокета (у QEMU он невелик) переполняется. QEMU в
//! этот момент не «копит вывод», а блокируется на записи — то есть встаёт вся
//! виртуальная машина. Поток-читатель существует затем, чтобы этого не
//! случалось никогда.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// Накопленный вывод гостя.
#[derive(Default)]
struct Buffer {
    text: String,
    /// Линия закрылась: QEMU завершился.
    closed: bool,
}

pub struct SerialLine {
    writer: TcpStream,
    buffer: Arc<Mutex<Buffer>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    /// Откуда искать следующее ожидание.
    ///
    /// Курсор обязателен: приглашение `freeos> ` встречается в выводе десятки
    /// раз, и поиск с начала находил бы первое, то есть считал бы выполненным
    /// то, что ещё не началось.
    cursor: usize,
}

impl SerialLine {
    /// Начать читать линию в фоне.
    pub fn spawn(stream: TcpStream) -> Result<Self> {
        let mut reader_stream = stream
            .try_clone()
            .context("не удалось раздвоить сокет серийной линии")?;
        // Таймаут чтения нужен, чтобы поток замечал команду «остановись», а не
        // висел в `read` до конца процесса.
        reader_stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .context("не удалось выставить таймаут чтения серийной линии")?;

        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_buffer = Arc::clone(&buffer);
        let thread_stop = Arc::clone(&stop);
        let reader = std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while !thread_stop.load(Ordering::Relaxed) {
                match reader_stream.read(&mut chunk) {
                    Ok(0) => {
                        thread_buffer.lock().expect("буфер линии").closed = true;
                        return;
                    }
                    Ok(count) => {
                        let text = String::from_utf8_lossy(&chunk[..count]);
                        let mut guard = thread_buffer.lock().expect("буфер линии");
                        guard.text.push_str(&text);
                    }
                    Err(err)
                        if err.kind() == ErrorKind::WouldBlock
                            || err.kind() == ErrorKind::TimedOut => {}
                    Err(_) => {
                        thread_buffer.lock().expect("буфер линии").closed = true;
                        return;
                    }
                }
            }
        });

        Ok(Self { writer: stream, buffer, stop, reader: Some(reader), cursor: 0 })
    }

    /// Весь вывод гостя с начала прогона.
    ///
    /// Нужен наведению мыши: цели берутся из строк, которые ядро само напечатало
    /// про размер экрана и положение окон.
    pub fn text(&self) -> String {
        self.buffer.lock().expect("буфер линии").text.clone()
    }

    /// Дождаться подстроки после места, где закончилось прошлое ожидание.
    pub fn wait_for(&mut self, needle: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let guard = self.buffer.lock().expect("буфер линии");
                if let Some(index) = guard.text.get(self.cursor..).and_then(|tail| tail.find(needle))
                {
                    self.cursor += index + needle.len();
                    return Ok(());
                }
                if guard.closed {
                    bail!(
                        "серийная линия закрылась, не дождавшись {needle:?} \
                         (QEMU завершился — гость перезагрузился или упал)"
                    );
                }
            }
            if Instant::now() >= deadline {
                bail!("за {} с не дождались {needle:?}", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Дождаться подстроки и прочитать десятичное число сразу за ней.
    ///
    /// Нужно там, где сценарий должен сослаться на то, что система назвала сама:
    /// номер задачи у запущенной программы. Проверять его константой нельзя —
    /// нумерация меняется от одной служебной задачи, заведённой в ядре, и
    /// сценарий начинает снимать не ту задачу либо падать на ровном месте.
    ///
    /// Число обязано начинаться сразу за подстрокой: у `"started as #"` за ним
    /// идут цифры, и никакого разбора формата тут не нужно.
    ///
    /// # Почему число дожидается отдельно
    ///
    /// Потому что строка приходит по линии не целиком. Подстрока может
    /// оказаться в буфере на одном чтении сокета, а цифры за ней — на
    /// следующем, и тогда «прочитать сразу после `wait_for`» означает прочитать
    /// пустоту. Ловилось это ровно так: `date` печатает `epoch  <число>`, и
    /// стенд падал с «за "epoch  " не оказалось числа» при том, что число в
    /// журнале было. Ждать надо оба события — и подстроку, и то, что за ней.
    ///
    /// Ждать конца числа — тоже обязательно, и по той же причине: буфер может
    /// застать `17865` от `1786551504`, и сценарий получил бы правдоподобное,
    /// но неверное число. Признак конца — любой не-цифровой байт после цифр;
    /// у всех, кто это печатает, за числом идёт пробел или перевод строки.
    pub fn capture_number(&mut self, prefix: &str, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        self.wait_for(prefix, timeout)?;
        loop {
            {
                let guard = self.buffer.lock().expect("буфер линии");
                let tail = &guard.text[self.cursor..];
                let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
                // Число считается дочитанным, когда за ним видно что-то ещё.
                if !digits.is_empty() && tail.len() > digits.len() {
                    return Ok(digits);
                }
                if guard.closed {
                    bail!("серийная линия закрылась, не дождавшись числа за {prefix:?}");
                }
            }
            if Instant::now() >= deadline {
                bail!("за {prefix:?} не оказалось числа");
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Встречалась ли подстрока во всём выводе.
    pub fn seen(&self, needle: &str) -> bool {
        self.buffer.lock().expect("буфер линии").text.contains(needle)
    }

    /// Дождаться подстроки **где угодно** в выводе — в том числе позади курсора.
    ///
    /// Существует из-за вполне конкретного провала, который дважды портил
    /// приёмочные прогоны. Обычный [`Self::wait_for`] ищет от места, где
    /// закончилось прошлое ожидание, и это правильно для приглашения оболочки,
    /// которое повторяется десятки раз. Но когда сценарий запускает две
    /// программы подряд и ждёт, что обе закончат, порядок строк в линии ему уже
    /// не подчиняется: если байты команды дошли рвано и вторая копия стартовала
    /// после того, как первая успела завершиться, её `done` оказывается **до**
    /// курсора — то есть навсегда потеряно, хотя в журнале оно есть.
    ///
    /// Поиск по всему буферу это чинит, а курсор двигается вперёд только если
    /// найденное дальше него: следующее ожидание не должно снова наткнуться на
    /// ту же строку.
    pub fn wait_seen(&mut self, needle: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let guard = self.buffer.lock().expect("буфер линии");
                if let Some(index) = guard.text.find(needle) {
                    self.cursor = self.cursor.max(index + needle.len());
                    return Ok(());
                }
                if guard.closed {
                    bail!(
                        "серийная линия закрылась, не дождавшись {needle:?} \
                         (QEMU завершился — гость перезагрузился или упал)"
                    );
                }
            }
            if Instant::now() >= deadline {
                bail!("за {} с не дождались {needle:?} нигде в выводе", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Отправить байты в линию — небольшими порциями с паузой.
    ///
    /// Порции обязательны, и это не осторожность. У приёмника UART есть FIFO
    /// (32 байта у PL011, 16 у 16550), а гость успевает его вычитывать не
    /// всегда: пока он держит лок с запрещёнными прерываниями — например,
    /// перерисовывая окно или дожидаясь диска, — байты, пришедшие сверх FIFO,
    /// теряются молча. Выглядит это так: эхо команды обрывается на середине, и
    /// сеанс встаёт навсегда — оболочка ждёт конца строки, стенд ждёт ответа.
    ///
    /// Ровно так падал сценарий `write` на AArch64: команда в 56 байт не
    /// помещалась в FIFO целиком, а команды покороче проходили.
    ///
    /// Порции — не обход дефекта ядра, и это выяснилось попыткой сделать из
    /// пачки проверку. Обрыв приходится **ровно на 32 байта** — на размер FIFO,
    /// — и не сдвигается, сколько бы прерываний ядро ни разрешало: у QEMU нет
    /// темпа линии, она отдаёт устройству всё, что пришло в сокет, разом.
    /// Настоящая последовательная линия так себя не ведёт, и человек за
    /// терминалом тем более: он набирает по символу, и линия без управления
    /// потоком рассчитана именно на это. Стенд был единственным, кто стрелял
    /// очередью, — теперь он печатает.
    pub fn write_raw(&mut self, bytes: &[u8]) -> Result<()> {
        /// Байт в порции — вдвое меньше самого маленького FIFO из тех, что
        /// встречаются на другом конце.
        const CHUNK: usize = 8;
        /// Пауза между порциями: на 115200 бод восемь байт летят 0,7 мс, так
        /// что задержку задаёт целиком она. Двадцать команд по три порции —
        /// это меньше секунды на сценарий.
        const GAP: Duration = Duration::from_millis(15);

        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            if index > 0 {
                std::thread::sleep(GAP);
            }
            self.writer
                .write_all(chunk)
                .context("не удалось записать в серийную линию")?;
            self.writer.flush().ok();
        }
        Ok(())
    }

    /// Отправить строку с переводом строки.
    pub fn write_line(&mut self, line: &str) -> Result<()> {
        self.write_raw(line.as_bytes())?;
        self.write_raw(b"\n")
    }

    /// Остановить чтение и дождаться потока.
    pub fn finish(mut self) -> String {
        self.stop.store(true, Ordering::Relaxed);
        // Закрытие сокета на запись будит читателя на той стороне; свой поток
        // проснётся сам по таймауту чтения.
        self.writer.shutdown(std::net::Shutdown::Both).ok();
        if let Some(handle) = self.reader.take() {
            handle.join().ok();
        }
        self.buffer.lock().expect("буфер линии").text.clone()
    }
}

impl Drop for SerialLine {
    /// Остановить поток-читатель, даже если сценарий упал и до [`Self::finish`]
    /// дело не дошло. Иначе поток пережил бы прогон и держал бы сокет.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.writer.shutdown(std::net::Shutdown::Both).ok();
        if let Some(handle) = self.reader.take() {
            handle.join().ok();
        }
    }
}
