//! Канал: байты, которые одна задача пишет, а другая читает.
//!
//! # Зачем он понадобился
//!
//! Из фазы 38. Сеанс SSH там появился, а запускать в нём было нечего: вывод
//! программы уходит в окно оболочки, и перехватить его было нечем. Поэтому
//! `sshd` носил внутри себя собственные `ls` и `cat` — со своей проверкой прав,
//! второй в системе. Две проверки прав расходятся; расходятся молча; и та,
//! которая слабее, становится дырой. Канал убирает не неудобство, а вторую
//! проверку: программа запускается **от имени вошедшего**, права спрашивает
//! ядро, а `sshd` остаётся тем, чем должен быть, — трубой между сетью и
//! программой.
//!
//! # Устройство
//!
//! Кольцевой буфер на [`CAPACITY`] байт под замком, и два конца — [`Reader`] и
//! [`Writer`]. Концы считаются: канал знает, сколько живых читателей и сколько
//! писателей у него осталось, и именно на этом стоит признак конца:
//!
//! * чтение из пустого канала, у которого **нет живых писателей**, — это ноль,
//!   то есть конец файла. Пока писатель жив, ноль вернуть нельзя: это соврало
//!   бы, что данных больше не будет.
//! * запись в канал, у которого **нет живых читателей**, — это [`PipeError::Broken`].
//!   Писать в никуда молча значит потерять вывод и не узнать об этом.
//!
//! Отсюда правило, которое приходится знать всякому, кто каналами пользуется:
//! **свой конец надо закрыть**. Тот, кто отдал конец запущенной задаче и держит
//! копию у себя, никогда не увидит конца файла — он сам и есть тот живой
//! писатель, которого ждут. Это не наша особенность, это устройство каналов
//! вообще; в Unix на этом спотыкаются с семидесятых.
//!
//! # Ожидание
//!
//! Через [`sched::block_on_lock`] — тот же механизм, что у замков ядра: задача
//! выходит из очереди и возвращается, когда её разбудят. Адресом ожидания
//! служит адрес самого канала в куче. Цикла с уступкой здесь нет намеренно:
//! программа, ждущая ввода, не должна занимать процессор — ровно та же причина,
//! по которой чтение с терминала устроено сном, а не опросом.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sched;
use crate::sync::SpinLock;

/// Сколько байт помещается в канал.
///
/// Страница. Больше не нужно: канал — это темп, а не хранилище, и писатель,
/// заполнивший его, обязан подождать читателя. Меньше — заметно: вывод `ls` у
/// большого каталога уходил бы десятками пробуждений.
pub const CAPACITY: usize = 4096;

/// Почему не получилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeError {
    /// Читателей не осталось: писать некому.
    Broken,
    /// Прямо сейчас данных нет (или нет места), а ждать не просили.
    WouldBlock,
    /// Не хватило памяти на буфер канала.
    OutOfMemory,
}

/// Внутренность канала: кольцо и счётчики концов.
struct Inner {
    bytes: Vec<u8>,
    /// Откуда читать.
    head: usize,
    /// Сколько байт лежит.
    len: usize,
    readers: usize,
    writers: usize,
}

impl Inner {
    fn free(&self) -> usize {
        CAPACITY - self.len
    }

    fn push(&mut self, data: &[u8]) -> usize {
        let take = data.len().min(self.free());
        let start = (self.head + self.len) % CAPACITY;
        for (offset, byte) in data[..take].iter().enumerate() {
            self.bytes[(start + offset) % CAPACITY] = *byte;
        }
        self.len += take;
        take
    }

    fn pop(&mut self, buffer: &mut [u8]) -> usize {
        let take = buffer.len().min(self.len);
        for (offset, slot) in buffer[..take].iter_mut().enumerate() {
            *slot = self.bytes[(self.head + offset) % CAPACITY];
        }
        self.head = (self.head + take) % CAPACITY;
        self.len -= take;
        take
    }
}

/// Сам канал. Наружу не выдаётся: снаружи бывают только его концы.
pub struct Pipe {
    inner: SpinLock<Inner>,
}

/// Завести канал. Возвращает его концы: читающий и пишущий.
pub fn create() -> Result<(Reader, Writer), PipeError> {
    let mut bytes = Vec::new();
    // `try_reserve_exact`, а не `vec![0; CAPACITY]`: канал заводит программа,
    // то есть третье кольцо, и нехватка памяти здесь — обычный отказ, а не
    // повод останавливать машину.
    bytes.try_reserve_exact(CAPACITY).map_err(|_| PipeError::OutOfMemory)?;
    bytes.resize(CAPACITY, 0);

    let pipe = Arc::new(Pipe {
        inner: SpinLock::new(Inner { bytes, head: 0, len: 0, readers: 1, writers: 1 }),
    });
    Ok((Reader { pipe: Arc::clone(&pipe) }, Writer { pipe }))
}

/// Адрес, по которому спят ожидающие этот канал.
fn channel(pipe: &Arc<Pipe>) -> usize {
    Arc::as_ptr(pipe) as usize
}

/// Читающий конец канала.
pub struct Reader {
    pipe: Arc<Pipe>,
}

impl Reader {
    /// Прочитать байты. Ноль означает конец: писателей больше нет.
    ///
    /// `blocking` — ждать ли данных. Ждут программы, читающие свой стандартный
    /// ввод; не ждёт тот, кто обслуживает ещё что-нибудь, кроме этого канала, —
    /// например `sshd`, у которого рядом сокет.
    pub fn read(&self, buffer: &mut [u8], blocking: bool) -> Result<usize, PipeError> {
        loop {
            {
                let mut inner = self.pipe.inner.lock();
                if inner.len > 0 {
                    let read = inner.pop(buffer);
                    drop(inner);
                    // Разбудить писателя: в канале освободилось место.
                    sched::wake_lock(channel(&self.pipe));
                    return Ok(read);
                }
                if inner.writers == 0 {
                    return Ok(0);
                }
            }
            if !blocking {
                return Err(PipeError::WouldBlock);
            }
            let pipe = Arc::clone(&self.pipe);
            sched::block_on_lock(channel(&self.pipe), move || {
                let inner = pipe.inner.lock();
                inner.len > 0 || inner.writers == 0
            });
        }
    }

    /// Есть ли что читать прямо сейчас (или уже наступил конец).
    #[must_use]
    pub fn ready(&self) -> bool {
        let inner = self.pipe.inner.lock();
        inner.len > 0 || inner.writers == 0
    }
}

impl Clone for Reader {
    fn clone(&self) -> Self {
        self.pipe.inner.lock().readers += 1;
        Self { pipe: Arc::clone(&self.pipe) }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        {
            let mut inner = self.pipe.inner.lock();
            inner.readers -= 1;
        }
        // Писатель, ждущий места, обязан узнать, что ждать больше некого:
        // иначе он спит до конца работы системы.
        sched::wake_lock(channel(&self.pipe));
    }
}

/// Пишущий конец канала.
pub struct Writer {
    pipe: Arc<Pipe>,
}

impl Writer {
    /// Записать байты. Возвращает, сколько уместилось.
    ///
    /// Частичная запись законна и является нормой у полного канала: писатель
    /// обязан смотреть на возвращённое число, как это делает `write` в Unix.
    pub fn write(&self, data: &[u8], blocking: bool) -> Result<usize, PipeError> {
        if data.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut inner = self.pipe.inner.lock();
                if inner.readers == 0 {
                    return Err(PipeError::Broken);
                }
                if inner.free() > 0 {
                    let written = inner.push(data);
                    drop(inner);
                    sched::wake_lock(channel(&self.pipe));
                    return Ok(written);
                }
            }
            if !blocking {
                return Err(PipeError::WouldBlock);
            }
            let pipe = Arc::clone(&self.pipe);
            sched::block_on_lock(channel(&self.pipe), move || {
                let inner = pipe.inner.lock();
                inner.free() > 0 || inner.readers == 0
            });
        }
    }
}

impl Clone for Writer {
    fn clone(&self) -> Self {
        self.pipe.inner.lock().writers += 1;
        Self { pipe: Arc::clone(&self.pipe) }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        {
            let mut inner = self.pipe.inner.lock();
            inner.writers -= 1;
        }
        // Читатель, ждущий данных, обязан узнать о конце файла. Без этого
        // пробуждения программа, читающая пустой канал последнего писателя,
        // спит вечно, а выглядит это как зависшая команда.
        sched::wake_lock(channel(&self.pipe));
    }
}
