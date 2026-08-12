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

    /// Встречалась ли подстрока во всём выводе.
    pub fn seen(&self, needle: &str) -> bool {
        self.buffer.lock().expect("буфер линии").text.contains(needle)
    }

    /// Отправить байты в линию как есть.
    pub fn write_raw(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer
            .write_all(bytes)
            .context("не удалось записать в серийную линию")?;
        self.writer.flush().ok();
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
