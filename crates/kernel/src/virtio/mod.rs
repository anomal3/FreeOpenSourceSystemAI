//! virtio: транспорт поверх PCI и разделённая очередь (split virtqueue).
//!
//! # Почему virtio, а не AHCI
//!
//! По той же причине, по которой клавиатура берётся через xHCI, а не через
//! PS/2: **один драйвер на обе архитектуры**. AHCI существует только там, где
//! есть SATA, то есть на `q35` и не на `virt`; virtio-blk одинаково работает на
//! обеих машинах QEMU, а его спецификация не зависит от процессора вовсе.
//!
//! Честная оговорка: на настоящем Raspberry Pi 4 никакого virtio нет. Диск там
//! придёт через USB mass storage поверх уже написанного стека xHCI — это
//! отдельная работа, и её отсутствие названо в дорожной карте, а не
//! замаскировано. Порядок тот же, что был с вводом: сначала то, что позволяет
//! отладить всё остальное, потом то, что нужно железу.
//!
//! # Только современный интерфейс
//!
//! virtio знает два способа добраться до своих регистров: старый — через порты
//! ввода-вывода, и современный (virtio 1.0) — через структуры в памяти, адреса
//! которых лежат в списке возможностей PCI. Реализован только второй, и это не
//! выбор: портов ввода-вывода на AArch64 не существует. QEMU создаёт переходное
//! устройство, понимающее оба, поэтому ограничение ничего не стоит.
//!
//! # Опрос вместо прерываний
//!
//! Завершение запроса ядро ждёт опросом кольца `used`, а не по прерыванию.
//! Причина та же, что у xHCI: планировщик кооперативный, обращений к диску
//! единицы за загрузку, а прерывание потребовало бы маршрутизации MSI-X и
//! обработчика, который всё равно не с кем синхронизировать. Опрос при этом
//! ограничен по времени — зависшее устройство обязано приводить к ошибке, а не
//! к остановке системы.

pub mod blk;
pub mod net;

use core::sync::atomic::{Ordering, fence};

use crate::mm::dma::{self, DmaBuffer};
use crate::pci::Device;

// ---------------------------------------------------------------------------
// Возможности PCI, которыми virtio описывает своё расположение
// ---------------------------------------------------------------------------

/// Общая конфигурация устройства.
const CFG_TYPE_COMMON: u8 = 1;
/// Окно, запись в которое означает уведомление устройства.
const CFG_TYPE_NOTIFY: u8 = 2;
/// Регистр состояния прерывания.
const CFG_TYPE_ISR: u8 = 3;
/// Конфигурация, специфичная для типа устройства.
const CFG_TYPE_DEVICE: u8 = 4;

/// Раскладка записи возможности virtio внутри конфигурационного пространства.
const CAP_CFG_TYPE: usize = 3;
const CAP_BAR: usize = 4;
const CAP_OFFSET: usize = 8;
const CAP_LENGTH: usize = 12;
/// Только у возможности типа [`CFG_TYPE_NOTIFY`].
const CAP_NOTIFY_MULTIPLIER: usize = 16;

// ---------------------------------------------------------------------------
// Общая конфигурация: смещения полей
// ---------------------------------------------------------------------------

const COMMON_DEVICE_FEATURE_SELECT: usize = 0x00;
const COMMON_DEVICE_FEATURE: usize = 0x04;
const COMMON_DRIVER_FEATURE_SELECT: usize = 0x08;
const COMMON_DRIVER_FEATURE: usize = 0x0C;
/// Число очередей. Не читается: у блочного устройства она одна по
/// построению, но пропуск в карте регистров означал бы, что следующее
/// смещение посчитано наугад.
#[allow(dead_code)]
const COMMON_NUM_QUEUES: usize = 0x12;
const COMMON_DEVICE_STATUS: usize = 0x14;
const COMMON_QUEUE_SELECT: usize = 0x16;
const COMMON_QUEUE_SIZE: usize = 0x18;
const COMMON_QUEUE_ENABLE: usize = 0x1C;
const COMMON_QUEUE_NOTIFY_OFF: usize = 0x1E;
const COMMON_QUEUE_DESC: usize = 0x20;
const COMMON_QUEUE_DRIVER: usize = 0x28;
const COMMON_QUEUE_DEVICE: usize = 0x30;

/// Биты регистра состояния устройства.
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;

/// Возможность «устройство соответствует virtio 1.0».
///
/// Согласовать её обязательно: без неё устройство остаётся в старом режиме, где
/// раскладка колец другая, а адреса передаются в страницах, а не в байтах.
const FEATURE_VERSION_1: u64 = 1 << 32;

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioError {
    /// В списке возможностей PCI нет структур virtio.
    NoCapabilities,
    /// Возможность ссылается на BAR, которого у устройства нет.
    BadBar(u8),
    /// Не удалось отобразить окно регистров.
    MapFailed,
    /// Устройство не согласилось на предложенный набор возможностей.
    FeaturesRejected,
    /// У устройства нет очереди с таким номером.
    NoQueue(u16),
    /// Не удалось выделить память под кольца.
    NoMemory(dma::DmaError),
    /// Устройство не ответило за отведённое время.
    Timeout,
    /// Устройство сообщило об ошибке выполнения запроса.
    RequestFailed(u8),
    /// Длина передачи не кратна сектору или равна нулю.
    BadTransfer,
    /// Устройство объявило нулевую ёмкость — носителя за ним нет.
    NoMedium,
    /// Все дескрипторы очереди в полёте: устройство не успевает.
    QueueFull,
    /// Устройство не сообщило свой аппаратный адрес.
    NoMac,
    /// Кадр не помещается в буфер очереди.
    TooLong(usize),
}

impl core::fmt::Display for VirtioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoCapabilities => f.write_str("no virtio capabilities in PCI config space"),
            Self::BadBar(bar) => write!(f, "capability points at BAR{bar}, which is not mapped"),
            Self::MapFailed => f.write_str("cannot map the register window"),
            Self::FeaturesRejected => f.write_str("the device rejected the negotiated features"),
            Self::NoQueue(index) => write!(f, "the device has no queue {index}"),
            Self::NoMemory(err) => write!(f, "cannot allocate the rings: {err}"),
            Self::Timeout => f.write_str("the device did not answer in time"),
            Self::RequestFailed(status) => write!(f, "the device failed the request ({status})"),
            Self::BadTransfer => f.write_str("transfer length is zero or not a multiple of 512"),
            Self::NoMedium => f.write_str("the device reports zero capacity"),
            Self::QueueFull => f.write_str("every descriptor is still in flight"),
            Self::NoMac => f.write_str("the device does not report a MAC address"),
            Self::TooLong(len) => write!(f, "{len} bytes do not fit in a queue buffer"),
        }
    }
}

// ---------------------------------------------------------------------------
// Транспорт
// ---------------------------------------------------------------------------

/// Окно регистров устройства virtio, найденное через список возможностей PCI.
pub struct Transport {
    common: crate::mm::VirtAddr,
    notify: crate::mm::VirtAddr,
    notify_multiplier: u32,
    /// Длина окна уведомлений — против неё проверяется смещение, которое
    /// сообщает устройство.
    notify_len: u32,
    device: Option<crate::mm::VirtAddr>,
}

/// Одна возможность virtio в разобранном виде.
struct Capability {
    cfg_type: u8,
    bar: u8,
    offset: u32,
    length: u32,
    notify_multiplier: u32,
}

impl Transport {
    /// Найти структуры virtio и отобразить их.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц: функция
    /// доотображает окна регистров устройства.
    pub unsafe fn open(pci: &Device) -> Result<Self, VirtioError> {
        let mut common = None;
        let mut notify = None;
        let mut device = None;

        pci.for_each_capability(|id, offset| {
            if id != crate::pci::CAP_ID_VENDOR {
                return true;
            }
            let cap = Capability {
                cfg_type: pci.config8(offset + CAP_CFG_TYPE),
                bar: pci.config8(offset + CAP_BAR),
                offset: pci.config32(offset + CAP_OFFSET),
                length: pci.config32(offset + CAP_LENGTH),
                notify_multiplier: pci.config32(offset + CAP_NOTIFY_MULTIPLIER),
            };
            match cap.cfg_type {
                CFG_TYPE_COMMON => common = Some(cap),
                CFG_TYPE_NOTIFY => notify = Some(cap),
                CFG_TYPE_DEVICE => device = Some(cap),
                // Регистр состояния прерывания нам не нужен: завершения ждём
                // опросом кольца, а не по прерыванию.
                CFG_TYPE_ISR => {}
                _ => {}
            }
            true
        });

        let (Some(common), Some(notify)) = (common, notify) else {
            return Err(VirtioError::NoCapabilities);
        };

        // Отображается BAR целиком — от его начала и до конца самого дальнего
        // окна, — а не каждое окно по отдельности.
        //
        // Отдельно было и не сработало: окна лежат в одном BAR вплотную, их
        // границы не выровнены на страницу, и отображение «страница, где
        // начинается окно, плюс его длина» оставляло дыры между соседними
        // окнами. Обращение к полю у края окна попадало в такую дыру, и на
        // AArch64 это был отказ страницы посреди первого же чтения диска.
        // Один диапазон на BAR устраняет весь класс этой ошибки.
        let mut bars = [None; 6];
        for cap in [Some(&common), Some(&notify), device.as_ref()].into_iter().flatten() {
            let end = u64::from(cap.offset) + u64::from(cap.length);
            let slot = &mut bars[usize::from(cap.bar).min(5)];
            *slot = Some(slot.map_or(end, |current: u64| current.max(end)));
        }

        let mut mapped: [Option<crate::mm::VirtAddr>; 6] = [None; 6];
        for (index, span) in bars.iter().enumerate() {
            let Some(span) = *span else {
                continue;
            };
            // SAFETY: условия делегированы вызывающему.
            mapped[index] = Some(unsafe { map_bar(pci, index as u8, span) }?);
        }

        let at = |cap: &Capability| -> Result<crate::mm::VirtAddr, VirtioError> {
            let base = mapped[usize::from(cap.bar).min(5)].ok_or(VirtioError::BadBar(cap.bar))?;
            Ok(crate::mm::VirtAddr::new(base.as_usize() + cap.offset as usize))
        };


        Ok(Self {
            common: at(&common)?,
            notify: at(&notify)?,
            notify_multiplier: notify.notify_multiplier,
            notify_len: notify.length,
            device: match device.as_ref() {
                Some(cap) => Some(at(cap)?),
                None => None,
            },
        })
    }

    /// Провести согласование состояния и возможностей.
    ///
    /// Порядок шагов задан спецификацией и обязателен: устройство отслеживает
    /// его само и откажется работать, если, например, возможности выставить
    /// после `DRIVER_OK`.
    pub fn negotiate(&self, wanted: u64) -> Result<u64, VirtioError> {
        // Сброс: ноль в регистре состояния. Прошивка UEFI уже пользовалась этим
        // устройством своим драйвером, и продолжать с её состояния нельзя.
        self.write_status(0);
        self.write_status(STATUS_ACKNOWLEDGE);
        self.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        let offered = self.read_features();
        let negotiated = offered & wanted;
        // Без virtio 1.0 раскладка колец и смысл адресов другие. Работать в
        // старом режиме мы не умеем, и притворяться, что умеем, нельзя.
        if negotiated & FEATURE_VERSION_1 == 0 {
            self.write_status(STATUS_FAILED);
            return Err(VirtioError::FeaturesRejected);
        }
        self.write_features(negotiated);

        self.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
        // Устройство вправе не согласиться: бит остаётся снятым, если набор
        // возможностей его не устраивает.
        if self.read_status() & STATUS_FEATURES_OK == 0 {
            self.write_status(STATUS_FAILED);
            return Err(VirtioError::FeaturesRejected);
        }
        Ok(negotiated)
    }

    /// Сообщить устройству, что драйвер готов.
    pub fn set_driver_ok(&self) {
        self.write_status(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);
    }

    /// Пометить устройство отказавшим — чтобы прошивка или следующая система
    /// увидели, что мы бросили его в неопределённом состоянии.
    pub fn set_failed(&self) {
        self.write_status(STATUS_FAILED);
    }


    /// Прочитать байт конфигурации, специфичной для типа устройства.
    ///
    /// Байт, а не слово: MAC-адрес у virtio-net объявлен массивом из шести
    /// байт, и читать его 32-битными обращениями значило бы выйти за поле на
    /// два байта и зависеть от того, что лежит следом.
    ///
    /// # Safety
    ///
    /// Смещение должно лежать внутри окна, объявленного возможностью.
    pub unsafe fn device_config8(&self, offset: usize) -> u8 {
        let Some(base) = self.device else {
            return 0;
        };
        // SAFETY: контракт функции; окно отображено в `open`.
        unsafe { read8(base, offset) }
    }

    /// Прочитать поле конфигурации, специфичной для типа устройства.
    ///
    /// # Safety
    ///
    /// Смещение должно лежать внутри окна, объявленного возможностью.
    pub unsafe fn device_config64(&self, offset: usize) -> u64 {
        let Some(base) = self.device else {
            return 0;
        };
        // SAFETY: контракт функции; окно отображено в `open`.
        // Читается двумя словами: 64-битное обращение к регистрам устройства
        // спецификация не гарантирует, а два 32-битных допустимы всегда.
        let low = unsafe { read32(base, offset) };
        let high = unsafe { read32(base, offset + 4) };
        u64::from(low) | (u64::from(high) << 32)
    }

    fn read_status(&self) -> u8 {
        // SAFETY: окно отображено в `open`, смещение из спецификации.
        unsafe { read8(self.common, COMMON_DEVICE_STATUS) }
    }

    fn write_status(&self, value: u8) {
        // SAFETY: см. выше.
        unsafe { write8(self.common, COMMON_DEVICE_STATUS, value) };
    }

    /// Прочитать 64 бита возможностей двумя половинами.
    fn read_features(&self) -> u64 {
        // SAFETY: см. выше.
        unsafe {
            write32(self.common, COMMON_DEVICE_FEATURE_SELECT, 0);
            let low = read32(self.common, COMMON_DEVICE_FEATURE);
            write32(self.common, COMMON_DEVICE_FEATURE_SELECT, 1);
            let high = read32(self.common, COMMON_DEVICE_FEATURE);
            u64::from(low) | (u64::from(high) << 32)
        }
    }

    fn write_features(&self, features: u64) {
        // SAFETY: см. выше.
        unsafe {
            write32(self.common, COMMON_DRIVER_FEATURE_SELECT, 0);
            write32(self.common, COMMON_DRIVER_FEATURE, features as u32);
            write32(self.common, COMMON_DRIVER_FEATURE_SELECT, 1);
            write32(self.common, COMMON_DRIVER_FEATURE, (features >> 32) as u32);
        }
    }
}

/// Отобразить `span` байт от начала BAR и вернуть его виртуальный адрес.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_bar(
    pci: &Device,
    bar: u8,
    span: u64,
) -> Result<crate::mm::VirtAddr, VirtioError> {
    let phys = pci
        .memory_bar(usize::from(bar))
        .ok_or(VirtioError::BadBar(bar))?;
    // BAR выровнен на страницу по спецификации PCI, поэтому округляется только
    // длина.
    let len = usize::try_from(span)
        .map_err(|_| VirtioError::BadBar(bar))?
        .max(1)
        .next_multiple_of(crate::mm::PAGE_SIZE);

    let virt = phys.to_direct_map();
    let flags =
        crate::mm::PageFlags::READ | crate::mm::PageFlags::WRITE | crate::mm::PageFlags::DEVICE;
    // SAFETY: условия делегированы вызывающему. Это регистры устройства, а не
    // память: Device-семантика обязательна, а прямое отображение взаимно
    // однозначно, поэтому адреса не пересекутся ни с кодом, ни со стеком.
    unsafe { crate::arch::map_active(virt, phys, len, flags) }
        .map_err(|_| VirtioError::MapFailed)?;

    Ok(virt)
}

// --- обращения к регистрам ---------------------------------------------------

/// # Safety
///
/// Адрес должен указывать в отображённое окно регистров.
unsafe fn read8(base: crate::mm::VirtAddr, offset: usize) -> u8 {
    // SAFETY: контракт функции. `volatile` обязателен: это регистры.
    unsafe { (base.as_usize() as *const u8).add(offset).read_volatile() }
}

/// # Safety
///
/// См. [`read8`].
unsafe fn read16(base: crate::mm::VirtAddr, offset: usize) -> u16 {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *const u8).add(offset).cast::<u16>().read_volatile() }
}

/// # Safety
///
/// См. [`read8`].
unsafe fn read32(base: crate::mm::VirtAddr, offset: usize) -> u32 {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *const u8).add(offset).cast::<u32>().read_volatile() }
}

/// # Safety
///
/// См. [`read8`]. Запись меняет состояние устройства.
unsafe fn write8(base: crate::mm::VirtAddr, offset: usize, value: u8) {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *mut u8).add(offset).write_volatile(value) };
}

/// # Safety
///
/// См. [`write8`].
unsafe fn write16(base: crate::mm::VirtAddr, offset: usize, value: u16) {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *mut u8).add(offset).cast::<u16>().write_volatile(value) };
}

/// # Safety
///
/// См. [`write8`].
unsafe fn write32(base: crate::mm::VirtAddr, offset: usize, value: u32) {
    // SAFETY: контракт функции.
    unsafe { (base.as_usize() as *mut u8).add(offset).cast::<u32>().write_volatile(value) };
}

/// # Safety
///
/// См. [`write8`].
unsafe fn write64(base: crate::mm::VirtAddr, offset: usize, value: u64) {
    // Двумя половинами: 64-битная запись в регистры virtio спецификацией не
    // гарантирована, а пара 32-битных допустима всегда.
    // SAFETY: контракт функции.
    unsafe {
        write32(base, offset, value as u32);
        write32(base, offset + 4, (value >> 32) as u32);
    }
}

// ---------------------------------------------------------------------------
// Разделённая очередь
// ---------------------------------------------------------------------------

/// Размер дескриптора в таблице.
const DESC_SIZE: usize = 16;

/// Флаг дескриптора: за ним следует ещё один.
const DESC_F_NEXT: u16 = 1;
/// Флаг дескриптора: устройство пишет в этот буфер, а не читает из него.
const DESC_F_WRITE: u16 = 2;

/// Сколько дескрипторов заводится в очереди.
///
/// Диску хватило бы трёх: запрос состоит из заголовка, данных и байта
/// состояния, и в полёте у него всегда ровно один запрос (см. заголовок модуля
/// про опрос). Шестнадцать здесь ради сети: у приёмной очереди буферы выставлены
/// заранее и все сразу, и их число — это то, сколько кадров устройство успеет
/// сложить между двумя обходами очереди задачей-приёмником. Размер обязан быть
/// степенью двойки, а битовая маска свободных дескрипторов ниже — `u16`, и это
/// же её предел.
const QUEUE_SIZE: u16 = 16;

/// Разделённая очередь: таблица дескрипторов и два кольца.
pub struct Queue {
    /// Общая память с устройством: дескрипторы, кольцо `avail`, кольцо `used`.
    memory: DmaBuffer,
    size: u16,
    /// Номер очереди у устройства. Он же уезжает в окно уведомлений: там ждут
    /// **номер очереди**, а не ноль, и у устройства с одной очередью эти два
    /// значения совпадают ровно по случайности.
    index: u16,
    /// Смещения внутри [`Queue::memory`].
    avail_offset: usize,
    used_offset: usize,
    /// Куда писать номер очереди, чтобы уведомить устройство.
    notify: crate::mm::VirtAddr,
    /// Сколько дескрипторов положено в `avail` с начала работы. Кольцо
    /// адресуется этим счётчиком по модулю размера, а сам счётчик переполняется
    /// естественным образом — так устроен формат.
    avail_index: u16,
    /// Сколько завершений уже забрано из `used`.
    used_index: u16,
    /// По биту на дескриптор: единица — свободен.
    ///
    /// Диск обходится без него (он всегда занимает дескрипторы 0, 1 и 2), а сети
    /// он необходим: у неё в полёте столько буферов, сколько влезло, и вернуть
    /// дескриптор в оборот можно только тогда, когда устройство сообщило о нём в
    /// кольце `used`.
    free: u16,
}

impl Queue {
    /// Создать очередь и сообщить её адреса устройству.
    pub fn new(transport: &Transport, index: u16) -> Result<Self, VirtioError> {
        // SAFETY: окно общей конфигурации отображено в `Transport::open`.
        unsafe { write16(transport.common, COMMON_QUEUE_SELECT, index) };
        // Смещение уведомления читается сразу за выбором очереди, а не в конце
        // настройки: поле относится к выбранной очереди, и чем меньше между
        // ними чужих обращений к регистрам, тем меньше поводов гадать, почему
        // прочиталось не то.
        // SAFETY: см. выше.
        let notify_off = unsafe { read16(transport.common, COMMON_QUEUE_NOTIFY_OFF) };
        // SAFETY: см. выше.
        let max = unsafe { read16(transport.common, COMMON_QUEUE_SIZE) };
        if max == 0 {
            return Err(VirtioError::NoQueue(index));
        }
        let size = QUEUE_SIZE.min(max);

        // Раскладка колец в памяти. Выравнивание — требование спецификации
        // virtio 1.0: дескрипторы на 16 байт, `avail` на 2, `used` на 4.
        // Промах здесь не приводит к отказу — устройство просто читает не то.
        let desc_bytes = size as usize * DESC_SIZE;
        let avail_bytes = 6 + size as usize * 2;
        let avail_offset = desc_bytes.next_multiple_of(2);
        let used_offset = (avail_offset + avail_bytes).next_multiple_of(4);
        let used_bytes = 6 + size as usize * 8;
        let total = used_offset + used_bytes;

        let memory = dma::alloc(total).map_err(VirtioError::NoMemory)?;

        // SAFETY: окно отображено, смещения из спецификации.
        unsafe {
            write16(transport.common, COMMON_QUEUE_SIZE, size);
            write64(transport.common, COMMON_QUEUE_DESC, memory.phys().as_u64());
            write64(
                transport.common,
                COMMON_QUEUE_DRIVER,
                memory.phys().as_u64() + avail_offset as u64,
            );
            write64(
                transport.common,
                COMMON_QUEUE_DEVICE,
                memory.phys().as_u64() + used_offset as u64,
            );
        }

        // Куда уведомлять — считается по смещению очереди и множителю из
        // возможности. Множитель ноль означает, что все очереди уведомляются в
        // один и тот же адрес; это законно и встречается.
        //
        // Смещение приходит от устройства, то есть из-за границы доверия, и
        // проверяется против длины окна, которую объявила сама возможность.
        // Это не паранойя: QEMU на машине `virt` отдавал здесь 0xFFFF, и без
        // проверки уведомление уходило на 256 КиБ дальше отображённого окна —
        // отказ страницы посреди первого же чтения диска.
        let raw = u64::from(notify_off) * u64::from(transport.notify_multiplier);
        let within = if raw + 2 <= u64::from(transport.notify_len) {
            raw
        } else {
            crate::kprintln!(
                "  virtio      : queue {index} reports notify offset {notify_off}, \
                 which is outside its {} byte window -- using offset 0",
                transport.notify_len,
            );
            0
        };
        let notify = crate::mm::VirtAddr::new(
            transport.notify.as_usize() + usize::try_from(within).unwrap_or(0),
        );

        // Прерываний от очереди не ждём и просим их не присылать: завершения
        // забираются опросом (см. заголовок модуля), а незамаскированное
        // прерывание от устройства, обработчика которому никто не ставил, — это
        // в лучшем случае шум, а на level-triggered линии PCI и вовсе поток,
        // который некому погасить. Бит `VIRTQ_AVAIL_F_NO_INTERRUPT` — просьба, а
        // не запрет: устройство вправе её проигнорировать, поэтому `INTx` в
        // регистре команд PCI выключается отдельно (`enable_bus_master`).
        // SAFETY: смещение внутри выделенного буфера, кольцо ещё не отдано
        // устройству — `QUEUE_ENABLE` ниже.
        unsafe {
            memory
                .as_ptr::<u8>()
                .add(avail_offset)
                .cast::<u16>()
                .write_volatile(1);
        }

        // SAFETY: см. выше.
        unsafe { write16(transport.common, COMMON_QUEUE_ENABLE, 1) };

        // Состояние очереди перечитывается, а не принимается на веру. Это не
        // осторожность вообще, а конкретный урок: при сброшенном бите Memory
        // Space в PCI записи в регистры пропадают молча, чтения возвращают все
        // единицы, и очередь остаётся невключённой. Единственным видимым
        // признаком был отказ по таймауту при первом чтении диска — там, где
        // причину искать никто не станет. Строка ниже показывает её сразу.
        // SAFETY: см. выше.
        let enabled = unsafe { read16(transport.common, COMMON_QUEUE_ENABLE) };
        crate::kprintln!(
            "  virtio      : queue {index}, {size} descriptors, notify +{within:#x}, enabled {enabled}"
        );

        Ok(Self {
            memory,
            size,
            index,
            avail_offset,
            used_offset,
            notify,
            avail_index: 0,
            used_index: 0,
            // Свободны все, что существуют. Дескрипторы сверх `size` помечены
            // занятыми навсегда: очередь, которую устройство урезало до восьми,
            // не должна выдать девятый.
            free: if size >= 16 { u16::MAX } else { (1u16 << size) - 1 },
        })
    }

    /// Сколько дескрипторов в очереди.
    pub fn size(&self) -> u16 {
        self.size
    }

    /// Занять свободный дескриптор.
    pub fn alloc_descriptor(&mut self) -> Option<u16> {
        if self.free == 0 {
            return None;
        }
        let index = self.free.trailing_zeros() as u16;
        self.free &= !(1 << index);
        Some(index)
    }

    /// Вернуть дескриптор в оборот.
    ///
    /// Вызывается только после того, как устройство сообщило о нём в кольце
    /// `used`: вернуть раньше значит отдать под новый запрос буфер, в который
    /// устройство ещё пишет.
    pub fn free_descriptor(&mut self, index: u16) {
        if index < self.size {
            self.free |= 1 << index;
        }
    }

    /// Записать дескриптор.
    pub fn set_descriptor(&self, index: u16, phys: u64, len: u32, flags: u16, next: u16) {
        let at = usize::from(index) * DESC_SIZE;
        // SAFETY: `index` меньше размера очереди, буфер выделен под неё целиком.
        unsafe {
            let base = self.memory.as_ptr::<u8>().add(at);
            base.cast::<u64>().write_volatile(phys);
            base.add(8).cast::<u32>().write_volatile(len);
            base.add(12).cast::<u16>().write_volatile(flags);
            base.add(14).cast::<u16>().write_volatile(next);
        }
    }

    /// Положить голову цепочки в кольцо `avail`, ничего не дожидаясь.
    pub fn offer(&mut self, head: u16) {
        // Кольцо `avail`: сначала положить номер головного дескриптора в его
        // ячейку, и только потом увеличить индекс. Обратный порядок означал бы,
        // что устройство вправе прочитать ячейку, которую мы ещё не заполнили.
        let slot = self.avail_index % self.size;
        // SAFETY: смещение внутри выделенного буфера.
        unsafe {
            let ring = self.memory.as_ptr::<u8>().add(self.avail_offset + 4 + usize::from(slot) * 2);
            ring.cast::<u16>().write_volatile(head);
        }
        // Барьер между заполнением ячейки и публикацией индекса — то же
        // требование, что и в любой очереди без блокировок.
        fence(Ordering::SeqCst);

        self.avail_index = self.avail_index.wrapping_add(1);
        // SAFETY: смещение внутри выделенного буфера.
        unsafe {
            self.memory
                .as_ptr::<u8>()
                .add(self.avail_offset + 2)
                .cast::<u16>()
                .write_volatile(self.avail_index);
        }
        fence(Ordering::SeqCst);
    }

    /// Толкнуть устройство: в окно уведомлений пишется номер очереди.
    pub fn kick(&self) {
        // SAFETY: адрес получен из возможности `notify` и отображён.
        unsafe { write16(self.notify, 0, self.index) };
    }

    /// Забрать одно завершение, если оно есть.
    ///
    /// Возвращает номер головного дескриптора и число байт, которые устройство
    /// в него записало. Для отправки длина не значит ничего, для приёма это
    /// длина кадра вместе с заголовком virtio.
    pub fn take_used(&mut self) -> Option<(u16, u32)> {
        fence(Ordering::SeqCst);
        // SAFETY: смещение внутри выделенного буфера.
        let published = unsafe {
            self.memory
                .as_ptr::<u8>()
                .add(self.used_offset + 2)
                .cast::<u16>()
                .read_volatile()
        };
        if published == self.used_index {
            return None;
        }
        let slot = usize::from(self.used_index % self.size);
        // SAFETY: смещение внутри выделенного буфера; элемент кольца `used` —
        // это пара 32-битных полей, так его задаёт спецификация.
        let (id, len) = unsafe {
            let entry = self.memory.as_ptr::<u8>().add(self.used_offset + 4 + slot * 8);
            (
                entry.cast::<u32>().read_volatile(),
                entry.add(4).cast::<u32>().read_volatile(),
            )
        };
        self.used_index = self.used_index.wrapping_add(1);
        // Номер дескриптора приходит от устройства, то есть из-за границы
        // доверия. Значение вне таблицы означало бы освобождение чужого бита в
        // маске и выдачу того же буфера дважды — поэтому оно отбрасывается
        // здесь, а не проверяется каждым вызывающим.
        let id = u16::try_from(id).ok()?;
        if id >= self.size {
            return None;
        }
        Some((id, len))
    }

    /// Отдать устройству цепочку дескрипторов, начинающуюся с нулевого, и
    /// дождаться завершения.
    ///
    /// `polls` — сколько раз опрашивать кольцо, прежде чем признать устройство
    /// зависшим.
    fn submit_and_wait(&mut self, polls: u32) -> Result<(), VirtioError> {
        self.offer(0);
        self.kick();

        for _ in 0..polls {
            if self.take_used().is_some() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(VirtioError::Timeout)
    }
}
