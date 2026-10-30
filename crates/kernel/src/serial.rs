//! Вывод по последовательному порту — единственный канал диагностики ядра.
//!
//! После `ExitBootServices` UEFI stdout мёртв: прошивка больше не обслуживает
//! ни консоль, ни GOP-протоколы. Всё, что остаётся, — это UART, который никакой
//! инициализации со стороны ОС не требует и работает даже когда фреймбуфера нет.

use crate::arch;
use crate::sync::SpinLock;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

/// Общий интерфейс UART, который реализует каждая архитектура.
///
/// Намеренно минимальный: побайтовая запись — единственное, что одинаково
/// выражается и через порты ввода-вывода (16550), и через MMIO (PL011).
pub trait SerialDevice {
    /// Привести порт в состояние 8N1 / 115200 и включить передатчик.
    fn init(&mut self);
    /// Отправить один байт, дождавшись освобождения передатчика.
    fn write_byte(&mut self, byte: u8);
    /// Отдать передатчику, сколько он примет **без ожидания**, из начала
    /// `bytes`; вернуть, сколько взято. Порта нет — взять всё (молча).
    fn try_send(&mut self, bytes: &[u8]) -> usize;
}

/// Очередь передачи (фаза 74, 30.10): печать кладёт байты сюда, а в порт их
/// перекладывает [`pump`] — после каждой печати и на каждом тике таймера, —
/// ровно столько, сколько порт берёт без ожидания.
///
/// До этого печать ждала передатчик побайтово с запрещёнными прерываниями: на
/// 115200 бод строка в 512 байт — 45 мс, в которые процессор не слышит ни
/// таймера, ни мыши, ни диска (VirtualBox: «interrupts held off at most 45.983
/// ms, from serial.rs»). Chromium и Rider при запуске печатают сотни строк — и
/// стол замирал. QEMU отдаёт передатчик мгновенно, поэтому стенд этого не
/// видел и не теряет теперь ничего: очередь опустошается тем же вызовом.
struct Tx {
    bytes: [u8; TX_CAPACITY],
    /// Сколько байт положено и сколько отдано порту — с начала времён.
    head: u64,
    tail: u64,
    /// Сколько байт выброшено, потому что очередь была полна.
    dropped: u64,
}

const TX_CAPACITY: usize = 128 * 1024;

static TX: SpinLock<Tx> = SpinLock::new(Tx { bytes: [0; TX_CAPACITY], head: 0, tail: 0, dropped: 0 });

/// Печать идёт через очередь. До первого тика таймера и после остановки
/// машины (паника, выключение) — напрямую: очередь некому опустошать.
static QUEUED: AtomicBool = AtomicBool::new(false);

impl Tx {
    fn room(&self) -> usize {
        TX_CAPACITY - (self.head - self.tail) as usize
    }

    fn push(&mut self, byte: u8) {
        if self.room() == 0 {
            self.dropped += 1;
            return;
        }
        self.bytes[(self.head % TX_CAPACITY as u64) as usize] = byte;
        self.head += 1;
    }
}

impl Write for Tx {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.push(b'\r');
            }
            self.push(byte);
        }
        Ok(())
    }
}

/// Переложить из очереди в порт, сколько он примет без ожидания. Зовётся
/// после печати и из обработчика тика; занято — значит перекладывает другой.
pub fn pump() {
    if !QUEUED.load(Ordering::Acquire) {
        return;
    }
    let Some(mut port) = SERIAL.try_lock() else { return };
    let Some(mut tx) = TX.try_lock() else { return };
    // Не больше 4 КиБ за раз: столько порт QEMU берёт мгновенно, а держать
    // прерывания запрещёнными дольше незачем — остальное заберёт тик.
    let mut budget = 4096usize;
    while tx.head > tx.tail && budget > 0 {
        let at = (tx.tail % TX_CAPACITY as u64) as usize;
        let run = ((tx.head - tx.tail) as usize).min(TX_CAPACITY - at).min(budget);
        let sent = port.try_send(&tx.bytes[at..at + run]);
        if sent == 0 {
            break;
        }
        tx.tail += sent as u64;
        budget -= sent;
    }
}

/// Печатать через очередь — с первого тика таймера.
pub fn start_queue() {
    QUEUED.store(true, Ordering::Release);
}

/// Дописать очередь в порт с ожиданием и дальше печатать напрямую: машина
/// останавливается (паника, отказ, выключение), и тиков больше не будет.
pub fn drain_and_stop_queue() {
    if !QUEUED.swap(false, Ordering::AcqRel) {
        return;
    }
    let mut port = match SERIAL.try_lock() {
        Some(port) => port,
        None => return,
    };
    let Some(mut tx) = TX.try_lock() else { return };
    while tx.head > tx.tail {
        let byte = tx.bytes[(tx.tail % TX_CAPACITY as u64) as usize];
        port.write_byte(byte);
        tx.tail += 1;
    }
    if tx.dropped > 0 {
        let dropped = tx.dropped;
        tx.dropped = 0;
        let _ = write!(Port(&mut port), "\n[serial: {dropped} bytes dropped, queue was full]\n");
    }
}

static SERIAL: SpinLock<arch::Serial> = SpinLock::new(arch::Serial::PLATFORM);

/// Пока порт не проинициализирован, вывод отбрасывается: писать в неготовый
/// UART бессмысленно, а на некоторых платформах ещё и опасно.
static READY: AtomicBool = AtomicBool::new(false);

/// Порт был отключён, потому что его там не оказалось.
static ABSENT: AtomicBool = AtomicBool::new(false);

/// Порт назван до входа в общую часть ядра — см. [`preset`].
static PRESET: AtomicBool = AtomicBool::new(false);

/// Инициализировать порт. Вызывается один раз, самым первым делом в ядре.
pub fn init() {
    // Порт, названный заранее, повторно не заводится и не подменяется. Разница
    // не косметическая: `arch::Serial::PLATFORM` — это адрес PL011 у QEMU, и на
    // машине, где по нему нет ничего, «инициализация» означала бы запись в
    // чужие регистры вслепую.
    if PRESET.load(Ordering::Acquire) {
        return;
    }
    let mut port = SERIAL.lock();
    port.init();
    crate::arch::remember_serial(&port);
    READY.store(true, Ordering::Release);
}

/// Назвать порт до того, как ядро начнёт печатать.
///
/// Нужно там, где адрес линии неизвестен до разбора дерева устройств, то есть
/// на входе по договору Linux. `None` означает «в этой машине порта нет»: тогда
/// ядро молчит с самого начала, а не после двух десятков строк, ушедших по
/// угаданному адресу.
///
/// # Safety
///
/// `device` обязан описывать настоящий UART, чьё окно регистров уже отображено
/// с Device-семантикой. Вызывать до [`init`].
pub unsafe fn preset(device: Option<crate::arch::Serial>) {
    match device {
        Some(device) => {
            let mut port = SERIAL.lock();
            *port = device;
            port.init();
            crate::arch::remember_serial(&port);
            ABSENT.store(false, Ordering::Release);
            READY.store(true, Ordering::Release);
        }
        None => {
            READY.store(false, Ordering::Release);
            ABSENT.store(true, Ordering::Release);
        }
    }
    PRESET.store(true, Ordering::Release);
}

/// Замолчать: по адресу порта не порт.
///
/// Вызывается, как только становится известна карта памяти (см.
/// [`crate::serial_lands_in_ram`]) — то есть после проверки хэндоффа и **до**
/// того, как ядро начнёт раздавать кадры. Всё, что напечатано раньше, ушло по
/// угаданному адресу; окно это в два десятка строк, и закрыть его нечем:
/// сообщить о непригодной карте памяти можно только по тому же порту.
///
/// Дальше молчание — единственный правильный ответ. Писать в память, которую
/// вот-вот отдадут под таблицы страниц, значит портить их; читать оттуда флаг
/// готовности передатчика — значит верить содержимому чужих данных. Вывод
/// остаётся на экране, а на машине без экрана его не будет вовсе, и это честнее
/// порчи памяти.
pub fn silence() {
    READY.store(false, Ordering::Release);
    ABSENT.store(true, Ordering::Release);
}

/// Был ли порт отключён за отсутствием. Нужно тем, кто иначе стал бы настраивать
/// его прерывание или отображать его окно.
#[must_use]
pub fn absent() -> bool {
    ABSENT.load(Ordering::Acquire)
}

/// Заговорить снова — в порт, который назвала прошивка.
///
/// Существует потому, что на ARM адрес UART ничем не закреплён, и первое, что
/// ядро о нём знает, — это догадка (умолчание QEMU). Догадка бывает неверной, и
/// тогда порт [`silence`]-ится, а система становится немой: на VirtualBox не
/// было ни одной строки журнала, и разбираться приходилось по снимкам экрана,
/// на которых журнал уже затёрт рабочим столом.
///
/// Настоящий адрес приезжает из ACPI позже — когда ядро уже перешло на свои
/// таблицы. Отсюда и эта функция: устройство подменяется целиком, потому что
/// «поменять адрес» и «завести порт заново» на PL011 — одно и то же действие.
///
/// # Safety
///
/// `device` обязан описывать настоящий UART, чьё окно регистров уже отображено
/// с Device-семантикой.
pub unsafe fn adopt(device: arch::Serial) {
    let mut port = SERIAL.lock();
    *port = device;
    port.init();
    arch::remember_serial(&port);
    ABSENT.store(false, Ordering::Release);
    READY.store(true, Ordering::Release);
}

/// Адаптер `fmt::Write` над устройством: добавляет к побайтовой записи перевод
/// `\n` в CRLF. Одалживает устройство, а не берёт его из глобального состояния
/// сам, — чтобы вся строка уходила в порт под одним захватом лока.
struct Port<'a>(&'a mut arch::Serial);

impl Write for Port<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            // Приёмник опоражнивается в темпе передатчика, и это лечение
            // дефекта, а не оптимизация. Печать идёт с запрещёнными
            // прерываниями (см. [`_print`] и [`crate::print::_print`]), а
            // строка оболочки уходит одним куском до 512 байт — сорок с лишним
            // миллисекунд на 115200. Всё это время принимающее FIFO (16 байт у
            // 16550, 32 у PL011) некому вычитывать, и приходящая команда
            // обрывается на середине: снаружи это «система не приняла ввод»,
            // неотличимое от зависшей оболочки. Три полных прогона ушло на то,
            // чтобы это опознать.
            //
            // Здесь байты только вынимаются в кольцо; разбирает их возврат из
            // ближайшего прерывания. Почему нельзя разобрать сразу — написано у
            // `input::ascii::PENDING`: разбор берёт локи, а мы сейчас держим
            // лок serial и, возможно, лок того, кто печатает.
            crate::input::ascii::rescue();
            // Терминалы, к которым QEMU подключает последовательный порт,
            // ожидают CRLF: без '\r' строки уезжают лесенкой.
            if byte == b'\n' {
                self.0.write_byte(b'\r');
            }
            self.0.write_byte(byte);
        }
        Ok(())
    }
}

/// Точка входа макросов вывода. Не вызывать напрямую.
///
/// # Почему здесь `try_lock`, а не `lock`
///
/// Serial — единственный канал, по которому ядро может сообщить о собственной
/// поломке, и обработчик отказа печатает из произвольной точки: в том числе из
/// середины уже начатого вывода. Обычный `lock` в этом случае ждал бы
/// освобождения, которого не будет никогда — держатель лока не продолжится,
/// пока обработчик не завершится, а обработчик не завершится, пока не
/// напечатает. Получилось бы тихое зависание вместо диагностики, то есть ровно
/// та потеря, против которой весь этот модуль и написан.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    if !READY.load(Ordering::Acquire) {
        return;
    }
    if QUEUED.load(Ordering::Acquire) {
        // Очередь держат микросекунды (копирование строки): соседний
        // процессор дождётся. Не дождались за разумное время — значит её
        // держит этот же процессор, прерванный отказом посреди печати.
        let mut taken = None;
        for _ in 0..200_000 {
            taken = TX.try_lock();
            if taken.is_some() {
                break;
            }
            core::hint::spin_loop();
        }
        if let Some(mut tx) = taken {
            // Выброшенное раньше — одной пометкой, как только есть место.
            if tx.dropped > 0 && tx.room() > 128 {
                let dropped = tx.dropped;
                tx.dropped = 0;
                let _ = write!(tx, "\n[serial: {dropped} bytes dropped, queue was full]\n");
            }
            let _ = tx.write_fmt(args);
            drop(tx);
            pump();
            return;
        }
        // Дальше напрямую, как ниже.
    }
    // Ошибка форматирования в UART невозможна: `write_byte` не возвращает
    // ошибок, — но `write_fmt` обязан вернуть Result, и игнорировать его здесь
    // корректно.
    match SERIAL.try_lock() {
        Some(mut device) => {
            let _ = Port(&mut device).write_fmt(args);
        }
        None => {
            // Лок занят — значит мы вклинились в чужой вывод. Пишем мимо лока:
            // `arch::Serial` не хранит состояния, которое можно порвать, — это
            // адрес порта и его вид, а `write_byte` укладывается в одну запись
            // регистра. Поэтому копия на стеке адресует тот же самый UART и не
            // создаёт второй ссылки на глобальное значение. Цена — перемешанные
            // символы двух сообщений; молчание дороже.
            //
            // Копия берётся у арх-слоя, а не из константы `PLATFORM`: на
            // машине, где порт нашёлся в дереве, константа означает адрес, по
            // которому нет ничего, и сообщение об отказе стало бы вторым
            // отказом.
            let mut fallback = arch::serial_fallback();
            let _ = Port(&mut fallback).write_fmt(args);
        }
    }
}
