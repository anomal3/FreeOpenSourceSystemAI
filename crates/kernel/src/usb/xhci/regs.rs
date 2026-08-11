//! Регистры xHCI: четыре набора по одному окну.
//!
//! Контроллер отдаёт своё окно MMIO не плоским списком регистров, а четырьмя
//! блоками, три из которых лежат по смещениям, сообщённым первым:
//!
//! ```text
//!   BAR0 + 0            Capability   что умеет контроллер (только чтение)
//!   BAR0 + CAPLENGTH    Operational  как им управлять
//!   BAR0 + RTSOFF       Runtime      прерыватели и кольца событий
//!   BAR0 + DBOFF        Doorbell     по слову на слот
//! ```
//!
//! Отсюда порядок в [`Registers::probe`]: сначала читается блок возможностей, и
//! только из него становятся известны адреса остальных трёх. Захардкодить их
//! нельзя — они разные у каждой реализации контроллера.
//!
//! # Почему все обращения `volatile`
//!
//! Потому что это регистры устройства, а не память: значение меняет контроллер,
//! и компилятору нельзя разрешать ни кешировать чтение, ни выбрасывать запись,
//! ни менять их порядок. Порядок здесь значим особенно: последовательность
//! «сообщить адрес кольца, затем разрешить работу» при перестановке даёт
//! контроллеру команду работать с мусорным адресом.

// Регистры контроллера описываются целиком, включая те, которых драйвер пока не
// касается: это карта железа, сверяемая со спецификацией, а не код. Выкинуть
// половину значит потерять именно то, что делает остальное проверяемым — и
// написать заново, когда появятся прерывания от контроллера или второе
// устройство.
#![allow(dead_code)]

use core::ptr;

/// Смещения блока возможностей.
///
/// # Почему всё читается словами по 32 бита
///
/// `CAPLENGTH` — это байт по смещению 0, а `HCIVERSION` — половина слова по
/// смещению 2, и прочитать их «как написано» — `u8` и `u16` — заманчиво. Так
/// делать нельзя, и это выяснилось не из спецификации, а из отладки: 16-битное
/// чтение по смещению 2 возвращало нули.
///
/// Причина в том, как устроена доставка обращений к устройству. Реализация окна
/// объявляет минимальный размер обращения 4 байта, и обращение меньшего размера
/// расширяется до четырёх **по тому же адресу**, а не по выровненному вниз. То
/// есть чтение двух байт по смещению 2 превращается в чтение четырёх байт по
/// смещению 2 — а такого регистра в карте нет, и устройство отвечает нулём. То
/// же самое произойдёт на любом контроллере, чьё окно объявляет только
/// 32-битный доступ, а таких хватает.
///
/// Поэтому блок возможностей читается словами, а поля извлекаются сдвигами. Это
/// и есть то, как описан регистр в спецификации: слово по смещению 0 содержит
/// `CAPLENGTH` в битах 7:0 и `HCIVERSION` в битах 31:16. Конфигурационное
/// пространство PCI — другое дело: оно байт-адресуемо по определению, и узкие
/// обращения к нему законны.
const CAP_LENGTH_VERSION: usize = 0x00;
/// Биты 7:0 слова по смещению 0.
const CAPLENGTH_MASK: u32 = 0xFF;
/// Биты 31:16 того же слова.
const HCIVERSION_SHIFT: u32 = 16;
const CAP_HCSPARAMS1: usize = 0x04;
const CAP_HCSPARAMS2: usize = 0x08;
const CAP_HCCPARAMS1: usize = 0x10;
const CAP_DBOFF: usize = 0x14;
const CAP_RTSOFF: usize = 0x18;

/// `HCSPARAMS1`: число слотов устройств (7:0), прерывателей (18:8), портов (31:24).
const HCSPARAMS1_MAX_SLOTS_MASK: u32 = 0xFF;
const HCSPARAMS1_MAX_PORTS_SHIFT: u32 = 24;

/// `HCSPARAMS2`: размер буферов-черновиков разбит на два поля — старшие пять бит
/// (25:21) и младшие пять (31:27). Разнесены они историей ревизий спецификации, а
/// не смыслом.
const HCSPARAMS2_SPB_HI_SHIFT: u32 = 21;
const HCSPARAMS2_SPB_HI_MASK: u32 = 0x1F;
const HCSPARAMS2_SPB_LO_SHIFT: u32 = 27;
const HCSPARAMS2_SPB_LO_MASK: u32 = 0x1F;

/// `HCCPARAMS1`, бит 0: контроллер умеет 64-битную адресацию.
const HCCPARAMS1_AC64: u32 = 1 << 0;
/// Бит 2: размер структуры контекста — 64 байта вместо 32.
const HCCPARAMS1_CSZ: u32 = 1 << 2;

/// Маска смещений `DBOFF`/`RTSOFF`: младшие биты зарезервированы и в адрес не
/// входят.
const DBOFF_MASK: u32 = !0x3;
const RTSOFF_MASK: u32 = !0x1F;

// --- Operational --------------------------------------------------------------

pub const OP_USBCMD: usize = 0x00;
pub const OP_USBSTS: usize = 0x04;
pub const OP_PAGESIZE: usize = 0x08;
pub const OP_CRCR: usize = 0x18;
pub const OP_DCBAAP: usize = 0x30;
pub const OP_CONFIG: usize = 0x38;
/// Первый блок регистров порта; на каждый порт — 16 байт.
const OP_PORT_BASE: usize = 0x400;
const OP_PORT_STRIDE: usize = 0x10;

/// `USBCMD`, бит 0: Run/Stop.
pub const USBCMD_RUN: u32 = 1 << 0;
/// Бит 1: Host Controller Reset.
pub const USBCMD_RESET: u32 = 1 << 1;
/// Бит 2: разрешение прерываний контроллера. Ядро его **не** ставит — события
/// опрашиваются (см. заголовок [`super`]).
pub const USBCMD_INTE: u32 = 1 << 2;
/// Бит 3: Host System Error Enable.
pub const USBCMD_HSEE: u32 = 1 << 3;

/// `USBSTS`, бит 0: контроллер остановлен.
pub const USBSTS_HALTED: u32 = 1 << 0;
/// Бит 2: ошибка системы (контроллер не смог обратиться к памяти).
pub const USBSTS_HOST_SYSTEM_ERROR: u32 = 1 << 2;
/// Бит 3: есть необработанное прерывание.
pub const USBSTS_EVENT_INTERRUPT: u32 = 1 << 3;
/// Бит 4: изменилось состояние порта.
pub const USBSTS_PORT_CHANGE: u32 = 1 << 4;
/// Бит 11: Controller Not Ready. Пока стоит, писать в регистры кроме `USBSTS`
/// нельзя — запись будет молча потеряна.
pub const USBSTS_NOT_READY: u32 = 1 << 11;
/// Бит 12: внутренняя ошибка контроллера, восстановление невозможно.
pub const USBSTS_HOST_CONTROLLER_ERROR: u32 = 1 << 12;

/// `CRCR`, бит 0: Ring Cycle State.
pub const CRCR_RING_CYCLE_STATE: u64 = 1 << 0;

// --- PORTSC -------------------------------------------------------------------

/// Бит 0: устройство подключено.
pub const PORTSC_CONNECTED: u32 = 1 << 0;
/// Бит 1: порт разрешён. **RW1C**: запись единицы порт выключает.
pub const PORTSC_ENABLED: u32 = 1 << 1;
/// Бит 4: сброс порта. Запись единицы начинает сброс.
pub const PORTSC_RESET: u32 = 1 << 4;
/// Бит 9: питание на порт подано.
pub const PORTSC_POWER: u32 = 1 << 9;
/// Биты 13:10: скорость устройства, значения из таблицы Protocol Speed ID.
pub const PORTSC_SPEED_SHIFT: u32 = 10;
pub const PORTSC_SPEED_MASK: u32 = 0xF;
/// Бит 17: изменилось состояние подключения. RW1C.
pub const PORTSC_CONNECT_CHANGE: u32 = 1 << 17;
/// Бит 21: сброс порта завершён. RW1C.
pub const PORTSC_RESET_CHANGE: u32 = 1 << 21;

/// Все биты `PORTSC`, которые сбрасываются записью единицы.
///
/// Знать их наизусть обязательно: `PORTSC` читают, меняют один бит и пишут
/// обратно — и если в прочитанном значении стоял любой из этих, запись его
/// сбросит. С `PORTSC_ENABLED` (бит 1) хуже: это тоже RW1C, и обратная запись
/// прочитанного значения **выключит порт**, который только что заработал. Ошибка
/// выглядит как «устройство определяется и сразу исчезает».
pub const PORTSC_RW1C_MASK: u32 = PORTSC_ENABLED
    | (1 << 17)  // CSC
    | (1 << 18)  // PEC
    | (1 << 19)  // WRC
    | (1 << 20)  // OCC
    | (1 << 21)  // PRC
    | (1 << 22)  // PLC
    | (1 << 23); // CEC

// --- Runtime ------------------------------------------------------------------

/// Смещение первого прерывателя внутри блока Runtime.
const RT_INTERRUPTER_BASE: usize = 0x20;
const RT_INTERRUPTER_STRIDE: usize = 0x20;

/// Смещения внутри прерывателя.
pub const IR_IMAN: usize = 0x00;
pub const IR_IMOD: usize = 0x04;
pub const IR_ERSTSZ: usize = 0x08;
pub const IR_ERSTBA: usize = 0x10;
pub const IR_ERDP: usize = 0x18;

/// `ERDP`, бит 3: Event Handler Busy. RW1C — снимается записью единицы вместе с
/// новым значением указателя.
pub const ERDP_EVENT_HANDLER_BUSY: u64 = 1 << 3;

/// Скорости из поля `Port Speed`. Значения — стандартные Protocol Speed ID,
/// которые контроллер обязан объявлять именно так, если не переопределил их через
/// расширенную возможность Supported Protocol.
pub const SPEED_FULL: u32 = 1;
pub const SPEED_LOW: u32 = 2;
pub const SPEED_HIGH: u32 = 3;
pub const SPEED_SUPER: u32 = 4;

/// Человекочитаемое имя скорости.
#[must_use]
pub const fn speed_name(speed: u32) -> &'static str {
    match speed {
        SPEED_FULL => "full (12 Mbit/s)",
        SPEED_LOW => "low (1.5 Mbit/s)",
        SPEED_HIGH => "high (480 Mbit/s)",
        SPEED_SUPER => "super (5 Gbit/s)",
        _ => "unknown",
    }
}

/// Размер пакета конечной точки 0, предписанный скоростью.
///
/// До чтения дескриптора устройства это единственный источник значения, а
/// сообщить его контроллеру надо раньше, чем что-либо прочитать. Для low- и
/// full-speed спецификация допускает 8, 16, 32 и 64, но требует, чтобы первые
/// восемь байт дескриптора можно было прочитать с размером 8, — поэтому здесь и
/// стоит 8, а точное значение выставляется потом.
#[must_use]
pub const fn default_max_packet_size(speed: u32) -> u16 {
    match speed {
        SPEED_SUPER => 512,
        SPEED_HIGH => 64,
        _ => 8,
    }
}

/// Окна регистров одного контроллера.
pub struct Registers {
    cap: usize,
    op: usize,
    runtime: usize,
    doorbell: usize,
    /// Версия интерфейса в BCD: `0x0110` — это xHCI 1.1.
    pub version: u16,
    /// Сколько слотов устройств поддерживает контроллер.
    pub max_slots: u8,
    /// Сколько у него корневых портов.
    pub max_ports: u8,
    /// Сколько буферов-черновиков он требует от драйвера.
    pub max_scratchpad: u16,
    /// Размер структуры контекста: 32 или 64 байта.
    pub context_size: usize,
    /// Умеет ли контроллер 64-битные адреса.
    pub ac64: bool,
    /// Размер страницы, которым контроллер оперирует.
    pub page_size: usize,
}

impl Registers {
    /// Прочитать блок возможностей и вычислить адреса остальных блоков.
    ///
    /// # Safety
    ///
    /// `base` должен быть виртуальным адресом отображённого как Device-память
    /// окна BAR0 работающего контроллера xHCI.
    pub unsafe fn probe(base: usize) -> Self {
        // SAFETY: контракт функции; все смещения — внутри первых 32 байт окна и
        // выровнены на 4 байта (почему это существенно — см. `CAP_LENGTH_VERSION`).
        let (length_version, hcs1, hcs2, hcc1, dboff, rtsoff) = unsafe {
            (
                ptr::read_volatile((base + CAP_LENGTH_VERSION) as *const u32),
                ptr::read_volatile((base + CAP_HCSPARAMS1) as *const u32),
                ptr::read_volatile((base + CAP_HCSPARAMS2) as *const u32),
                ptr::read_volatile((base + CAP_HCCPARAMS1) as *const u32),
                ptr::read_volatile((base + CAP_DBOFF) as *const u32),
                ptr::read_volatile((base + CAP_RTSOFF) as *const u32),
            )
        };
        let cap_length = (length_version & CAPLENGTH_MASK) as u8;
        let version = (length_version >> HCIVERSION_SHIFT) as u16;

        let scratchpad_hi = (hcs2 >> HCSPARAMS2_SPB_HI_SHIFT) & HCSPARAMS2_SPB_HI_MASK;
        let scratchpad_lo = (hcs2 >> HCSPARAMS2_SPB_LO_SHIFT) & HCSPARAMS2_SPB_LO_MASK;
        let max_scratchpad = ((scratchpad_hi << 5) | scratchpad_lo) as u16;

        let op = base + usize::from(cap_length);
        let mut regs = Self {
            cap: base,
            op,
            runtime: base + (rtsoff & RTSOFF_MASK) as usize,
            doorbell: base + (dboff & DBOFF_MASK) as usize,
            version,
            max_slots: (hcs1 & HCSPARAMS1_MAX_SLOTS_MASK) as u8,
            max_ports: (hcs1 >> HCSPARAMS1_MAX_PORTS_SHIFT) as u8,
            max_scratchpad,
            context_size: if hcc1 & HCCPARAMS1_CSZ != 0 { 64 } else { 32 },
            ac64: hcc1 & HCCPARAMS1_AC64 != 0,
            page_size: 4096,
        };

        // `PAGESIZE` — битовая карта: бит `n` означает поддержку страниц
        // размером `2^(n + 12)`. Берём младший установленный бит: буферы
        // выделяются страницами ядра, и меньший размер всегда подходит.
        // SAFETY: регистр лежит в блоке Operational, адрес которого вычислен выше.
        let page_bits = unsafe { regs.read_op32(OP_PAGESIZE) };
        if page_bits != 0 {
            regs.page_size = 1usize << (page_bits.trailing_zeros() + 12);
        }
        regs
    }

    /// Начало блока возможностей — нужно для обхода расширенных возможностей.
    #[must_use]
    pub const fn cap_base(&self) -> usize {
        self.cap
    }

    /// # Safety
    ///
    /// Смещение обязано указывать на существующий 32-битный регистр блока
    /// Operational.
    pub unsafe fn read_op32(&self, offset: usize) -> u32 {
        // SAFETY: контракт функции.
        unsafe { ptr::read_volatile((self.op + offset) as *const u32) }
    }

    /// # Safety
    ///
    /// См. [`Registers::read_op32`]. Запись меняет состояние контроллера.
    pub unsafe fn write_op32(&self, offset: usize, value: u32) {
        // SAFETY: контракт функции.
        unsafe { ptr::write_volatile((self.op + offset) as *mut u32, value) };
    }

    /// # Safety
    ///
    /// Смещение обязано указывать на существующий 64-битный регистр блока
    /// Operational и быть выровнено на 8.
    pub unsafe fn read_op64(&self, offset: usize) -> u64 {
        // SAFETY: контракт функции.
        unsafe { ptr::read_volatile((self.op + offset) as *const u64) }
    }

    /// # Safety
    ///
    /// См. [`Registers::read_op64`].
    ///
    /// Запись делается одним 64-битным обращением, а не двумя 32-битными.
    /// Спецификация допускает оба варианта, но у половинчатой записи есть
    /// промежуточное состояние: в регистре оказывается новая младшая половина со
    /// старой старшей — то есть адрес, не принадлежащий ни одной структуре.
    /// Контроллер вправе прочитать регистр именно в этот момент.
    pub unsafe fn write_op64(&self, offset: usize, value: u64) {
        // SAFETY: контракт функции.
        unsafe { ptr::write_volatile((self.op + offset) as *mut u64, value) };
    }

    /// Регистр состояния порта. Порты нумеруются с единицы.
    ///
    /// # Safety
    ///
    /// `port` обязан быть в пределах [`Registers::max_ports`].
    pub unsafe fn read_portsc(&self, port: u8) -> u32 {
        let offset = OP_PORT_BASE + (usize::from(port) - 1) * OP_PORT_STRIDE;
        // SAFETY: контракт функции.
        unsafe { self.read_op32(offset) }
    }

    /// Записать `PORTSC`, не сбросив ничего лишнего.
    ///
    /// `value` обязано быть уже очищенным от RW1C-битов (см.
    /// [`PORTSC_RW1C_MASK`]) — эта функция сама этого не делает намеренно: часть
    /// вызовов существует именно затем, чтобы сбросить конкретный признак.
    ///
    /// # Safety
    ///
    /// См. [`Registers::read_portsc`].
    pub unsafe fn write_portsc(&self, port: u8, value: u32) {
        let offset = OP_PORT_BASE + (usize::from(port) - 1) * OP_PORT_STRIDE;
        // SAFETY: контракт функции.
        unsafe { self.write_op32(offset, value) };
    }

    /// # Safety
    ///
    /// Смещение обязано указывать на 32-битный регистр прерывателя `index`.
    pub unsafe fn write_interrupter32(&self, index: usize, offset: usize, value: u32) {
        let addr = self.runtime + RT_INTERRUPTER_BASE + index * RT_INTERRUPTER_STRIDE + offset;
        // SAFETY: контракт функции.
        unsafe { ptr::write_volatile(addr as *mut u32, value) };
    }

    /// # Safety
    ///
    /// См. [`Registers::write_interrupter32`]; смещение выровнено на 8.
    pub unsafe fn write_interrupter64(&self, index: usize, offset: usize, value: u64) {
        let addr = self.runtime + RT_INTERRUPTER_BASE + index * RT_INTERRUPTER_STRIDE + offset;
        // SAFETY: контракт функции.
        unsafe { ptr::write_volatile(addr as *mut u64, value) };
    }

    /// # Safety
    ///
    /// См. [`Registers::write_interrupter64`].
    pub unsafe fn read_interrupter64(&self, index: usize, offset: usize) -> u64 {
        let addr = self.runtime + RT_INTERRUPTER_BASE + index * RT_INTERRUPTER_STRIDE + offset;
        // SAFETY: контракт функции.
        unsafe { ptr::read_volatile(addr as *const u64) }
    }

    /// Позвонить в дверной звонок слота.
    ///
    /// Слот 0 — кольцо команд; слот `n` — конечные точки устройства `n`, и
    /// `target` там означает идентификатор точки (1 — управляющая, дальше
    /// `2 * ep` для OUT и `2 * ep + 1` для IN).
    ///
    /// # Safety
    ///
    /// `slot` обязан быть в пределах числа слотов, а кольцо соответствующей
    /// точки — содержать дескрипторы, готовые к исполнению: звонок означает
    /// «работай», и контроллер начнёт читать кольцо немедленно.
    pub unsafe fn ring_doorbell(&self, slot: u8, target: u8) {
        let addr = self.doorbell + usize::from(slot) * 4;
        // SAFETY: контракт функции.
        unsafe { ptr::write_volatile(addr as *mut u32, u32::from(target)) };
    }
}
