//! Кольца дескрипторов: как драйвер и контроллер передают друг другу работу.
//!
//! # Один дескриптор
//!
//! TRB (Transfer Request Block) — шестнадцать байт: параметр (обычно адрес),
//! состояние (обычно длина) и управляющее слово (тип и признаки). Всё, что
//! драйвер сообщает контроллеру, и всё, что контроллер сообщает драйверу, — это
//! TRB.
//!
//! # Бит цикла: как понять, чей дескриптор
//!
//! Кольцо — это массив, по которому оба участника идут по кругу. Проблема, для
//! которой придуман бит цикла (`C`, младший бит управляющего слова): дойдя до
//! конца и вернувшись в начало, нельзя отличить свежий дескриптор от того, что
//! лежит там с прошлого оборота — байты выглядят одинаково.
//!
//! Решение: каждый участник помнит, какое значение бита `C` он считает
//! «своим», и на каждом обороте меняет его на противоположное. Производитель
//! записывает дескриптор со своим текущим значением, потребитель обрабатывает
//! только те, у которых `C` совпал с его собственным. Прошлый оборот
//! автоматически оказывается «чужим», и обнулять кольцо не нужно.
//!
//! # Link TRB
//!
//! Последний элемент кольца драйвера — не дескриптор работы, а ссылка на его
//! начало с флагом Toggle Cycle. Она и делает массив кольцом. Место под неё
//! отнимается у полезных элементов, поэтому вместимость кольца на один меньше
//! длины массива.
//!
//! Кольцо событий устроено иначе: у него нет Link TRB, потому что производитель
//! там контроллер, а он знает размер сегмента из таблицы `ERST` и заворачивается
//! сам.

use crate::mm::dma::DmaBuffer;

/// Размер одного дескриптора.
pub const TRB_LEN: usize = 16;

/// Типы TRB, которыми пользуется драйвер (xHCI 1.2, таблица 6-91).
pub const TRB_NORMAL: u32 = 1;
pub const TRB_SETUP_STAGE: u32 = 2;
pub const TRB_DATA_STAGE: u32 = 3;
pub const TRB_STATUS_STAGE: u32 = 4;
pub const TRB_LINK: u32 = 6;
pub const TRB_ENABLE_SLOT: u32 = 9;
pub const TRB_ADDRESS_DEVICE: u32 = 11;
pub const TRB_CONFIGURE_ENDPOINT: u32 = 12;
pub const TRB_EVALUATE_CONTEXT: u32 = 13;
pub const TRB_NO_OP_COMMAND: u32 = 23;
/// События, которые кладёт контроллер.
pub const TRB_TRANSFER_EVENT: u32 = 32;
pub const TRB_COMMAND_COMPLETION: u32 = 33;
pub const TRB_PORT_STATUS_CHANGE: u32 = 34;

/// Сдвиг поля типа в управляющем слове.
pub const TRB_TYPE_SHIFT: u32 = 10;
pub const TRB_TYPE_MASK: u32 = 0x3F;

/// Бит 0: цикл.
pub const TRB_CYCLE: u32 = 1 << 0;
/// Бит 1 у Link TRB: Toggle Cycle — потребитель обязан сменить своё
/// представление о бите цикла, пройдя эту ссылку.
pub const TRB_TOGGLE_CYCLE: u32 = 1 << 1;
/// Бит 5: Interrupt On Completion — контроллер обязан породить событие.
pub const TRB_IOC: u32 = 1 << 5;
/// Бит 6: Immediate Data — параметр TRB содержит сами данные, а не их адрес.
pub const TRB_IDT: u32 = 1 << 6;

/// Сдвиг поля кода завершения в слове состояния события.
pub const COMPLETION_CODE_SHIFT: u32 = 24;
pub const COMPLETION_CODE_MASK: u32 = 0xFF;

/// Успех.
pub const COMPLETION_SUCCESS: u32 = 1;
/// Короткий пакет: устройство отдало меньше запрошенного. Для чтения
/// дескрипторов это нормальный, а не ошибочный исход.
pub const COMPLETION_SHORT_PACKET: u32 = 13;

/// Человекочитаемое имя кода завершения — только для диагностики.
#[must_use]
pub const fn completion_name(code: u32) -> &'static str {
    match code {
        0 => "invalid",
        1 => "success",
        2 => "data buffer error",
        3 => "babble detected",
        4 => "USB transaction error",
        5 => "TRB error",
        6 => "stall error",
        7 => "resource error",
        8 => "bandwidth error",
        9 => "no slots available",
        11 => "slot not enabled",
        12 => "endpoint not enabled",
        13 => "short packet",
        17 => "parameter error",
        19 => "context state error",
        21 => "secondary bandwidth error",
        _ => "other",
    }
}

/// Один дескриптор в виде, пригодном для записи в кольцо.
#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
pub struct Trb {
    pub parameter: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    /// Тип дескриптора.
    #[must_use]
    pub const fn kind(&self) -> u32 {
        (self.control >> TRB_TYPE_SHIFT) & TRB_TYPE_MASK
    }

    /// Код завершения из слова состояния (осмыслен только у событий).
    #[must_use]
    pub const fn completion_code(&self) -> u32 {
        (self.status >> COMPLETION_CODE_SHIFT) & COMPLETION_CODE_MASK
    }

    /// Успешно ли завершилась операция, о которой сообщает событие.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        matches!(self.completion_code(), COMPLETION_SUCCESS | COMPLETION_SHORT_PACKET)
    }

    /// Номер слота из события (биты 31:24 управляющего слова).
    #[must_use]
    pub const fn slot_id(&self) -> u8 {
        (self.control >> 24) as u8
    }

    /// Сколько байт **не** передано: контроллер сообщает остаток, а не длину.
    #[must_use]
    pub const fn residual(&self) -> u32 {
        self.status & 0x00FF_FFFF
    }
}

/// Кольцо, в которое пишет драйвер: команды или передачи.
pub struct Ring {
    buffer: DmaBuffer,
    /// Сколько дескрипторов помещается в массив, включая место под Link TRB.
    entries: usize,
    /// Куда писать следующий.
    enqueue: usize,
    /// Значение бита цикла, которым помечаются новые дескрипторы.
    cycle: bool,
}

impl Ring {
    /// Создать кольцо в уже выделенном буфере DMA.
    ///
    /// Буфер должен быть обнулён (это делает [`crate::mm::dma::alloc`]) и лежать
    /// физически непрерывно — контроллер читает кольцо, ничего не зная о таблицах
    /// страниц.
    #[must_use]
    pub fn new(buffer: DmaBuffer) -> Self {
        let entries = buffer.len() / TRB_LEN;
        let mut ring = Self { buffer, entries, enqueue: 0, cycle: true };
        ring.write_link();
        ring
    }

    /// Физический адрес начала кольца — то, что уезжает контроллеру.
    #[must_use]
    pub fn phys(&self) -> u64 {
        self.buffer.phys().as_u64()
    }

    /// Значение бита цикла, с которого кольцо начинается.
    #[must_use]
    pub const fn initial_cycle(&self) -> bool {
        // Кольцо создаётся с `cycle = true`, и контроллеру при старте сообщается
        // то же значение (`RCS` в `CRCR`, `DCS` в контексте точки). Расхождение
        // здесь означает, что контроллер сочтёт кольцо пустым и не сделает
        // ничего — отказа при этом не будет.
        true
    }

    /// Записать Link TRB в последний элемент.
    fn write_link(&mut self) {
        let link = Trb {
            parameter: self.buffer.phys().as_u64(),
            status: 0,
            control: (TRB_LINK << TRB_TYPE_SHIFT)
                | TRB_TOGGLE_CYCLE
                | if self.cycle { TRB_CYCLE } else { 0 },
        };
        self.write(self.entries - 1, link);
    }

    fn write(&mut self, index: usize, trb: Trb) {
        debug_assert!(index < self.entries);
        // SAFETY: индекс внутри массива, буфер отображён на запись и физически
        // непрерывен. Поля пишутся `volatile` и в фиксированном порядке:
        // управляющее слово последним, потому что именно его бит цикла делает
        // дескриптор видимым контроллеру. Запиши его первым — контроллер вправе
        // прочитать дескриптор с недописанными параметром и состоянием.
        unsafe {
            let base = self.buffer.as_ptr::<u8>().add(index * TRB_LEN);
            (base as *mut u64).write_volatile(trb.parameter);
            (base.add(8) as *mut u32).write_volatile(trb.status);
            (base.add(12) as *mut u32).write_volatile(trb.control);
        }
    }

    /// Положить дескриптор в кольцо. Возвращает его физический адрес — по нему
    /// потом опознаётся событие о завершении.
    pub fn push(&mut self, mut trb: Trb) -> u64 {
        trb.control = (trb.control & !TRB_CYCLE) | if self.cycle { TRB_CYCLE } else { 0 };
        let index = self.enqueue;
        self.write(index, trb);
        let addr = self.buffer.phys().as_u64() + (index * TRB_LEN) as u64;

        self.enqueue += 1;
        // Дойдя до места, где лежит Link TRB, обновляем его бит цикла (иначе
        // контроллер остановится на нём, сочтя чужим) и начинаем оборот заново с
        // противоположным значением бита.
        if self.enqueue == self.entries - 1 {
            self.write_link();
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        addr
    }
}

/// Кольцо, в которое пишет контроллер: события.
pub struct EventRing {
    buffer: DmaBuffer,
    entries: usize,
    /// Откуда читать следующее событие.
    dequeue: usize,
    /// Значение бита цикла, которое драйвер считает «своим».
    cycle: bool,
}

impl EventRing {
    #[must_use]
    pub fn new(buffer: DmaBuffer) -> Self {
        let entries = buffer.len() / TRB_LEN;
        // Контроллер начинает заполнять сегмент с `C = 1`, поэтому и потребитель
        // ищет единицу. Обнулённый буфер при этом означает «событий нет» — что
        // верно, и потому кольцо событий, в отличие от кольца команд, не требует
        // никакой начальной разметки.
        Self { buffer, entries, dequeue: 0, cycle: true }
    }

    #[must_use]
    pub fn phys(&self) -> u64 {
        self.buffer.phys().as_u64()
    }

    #[must_use]
    pub const fn entries(&self) -> usize {
        self.entries
    }

    /// Физический адрес, который надо сообщить контроллеру как позицию
    /// потребителя.
    #[must_use]
    pub fn dequeue_phys(&self) -> u64 {
        self.buffer.phys().as_u64() + (self.dequeue * TRB_LEN) as u64
    }

    /// Забрать следующее событие, если контроллер его уже положил.
    pub fn pop(&mut self) -> Option<Trb> {
        // SAFETY: индекс внутри массива, буфер отображён и доступен на чтение.
        // `volatile` обязателен: содержимое меняет контроллер, и обычное чтение
        // компилятор вправе поднять из цикла — получился бы вечный опрос
        // однажды прочитанного значения.
        let trb = unsafe {
            let base = self.buffer.as_ptr::<u8>().add(self.dequeue * TRB_LEN);
            // Управляющее слово читается **первым**: именно его бит цикла
            // означает «дескриптор готов». Прочитав сначала параметр, можно
            // получить недописанное значение от ещё не завершённой записи
            // контроллера.
            let control = (base.add(12) as *const u32).read_volatile();
            if (control & TRB_CYCLE != 0) != self.cycle {
                return None;
            }
            Trb {
                parameter: (base as *const u64).read_volatile(),
                status: (base.add(8) as *const u32).read_volatile(),
                control,
            }
        };

        self.dequeue += 1;
        if self.dequeue == self.entries {
            self.dequeue = 0;
            self.cycle = !self.cycle;
        }
        Some(trb)
    }
}
