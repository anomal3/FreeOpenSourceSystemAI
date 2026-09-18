//! Intel 8254x (`e1000`) — вторая сетевая карта системы и первая настоящая.
//!
//! # Зачем она, если есть virtio-net
//!
//! Потому что virtio-net не существует вне виртуальной машины. Требование к
//! системе звучит иначе — «запускаться на любом компьютере», — и выполнить его
//! может только набор драйверов по семействам карт, выбираемых по паре
//! идентификаторов PCI. Это семейство выбрано первым по двум причинам: оно
//! стоит в бесчисленном множестве машин и, главное, его **эмулирует QEMU**
//! (`-device e1000` — это 82540EM). То есть первый же драйвер настоящей карты
//! проверяется на стенде целиком: адрес из EEPROM, кольца дескрипторов, приём,
//! передача, DHCP и `ping` поверх всего этого.
//!
//! # Что это семейство, а что нет
//!
//! Здесь — **8254x**: PCI и PCI-X карты, у которых аппаратный адрес читается из
//! EEPROM регистром `EERD`, а кольца описываются четвёркой `BAL/BAH/LEN/H/T`.
//!
//! Здесь **не** e1000e — 82567/82577/82579, i217/i218/i219, те, что стоят в
//! ноутбуках и материнских платах после 2008 года. Они того же изготовителя и
//! похожи регистрами, но их PHY настраивается через MDIO, адрес лежит не в
//! EEPROM, а в NVM за другим интерфейсом, и притворяться, что один драйвер
//! поднимет и то и другое, значит получить карту, которая «нашлась» и молчит.
//! Такие карты перепись назовёт по идентификатору и скажет, что драйвера нет, —
//! это честнее.
//!
//! # Кольца
//!
//! Два кольца по шестнадцать дескрипторов и по буферу на каждый. Шестнадцать —
//! не круглое число из воздуха: длина кольца в байтах обязана быть кратной 128
//! (`RDLEN`/`TDLEN`), дескриптор занимает 16 байт, и 16 × 16 = 256 — наименьший
//! кратный размер, при котором кольцо всё же кольцо, а не пара буферов.
//!
//! Приём: карта пишет в буфер и ставит бит `DD`. Драйвер копирует кадр, снимает
//! статус и двигает «хвост» (`RDT`) на разобранный дескриптор — тот самый
//! момент, когда буфер возвращается карте. Хвост всегда отстаёт от головы на
//! один дескриптор: кольцо, у которого голова догнала хвост, карта считает
//! пустым и начинает терять кадры.
//!
//! Передача: драйвер кладёт кадр в свободный дескриптор, просит отчёт (`RS`) и
//! двигает `TDT`. Ждать завершения незачем — признак `DD` проверяется в
//! следующий раз, когда этот дескриптор понадобится.

use alloc::vec::Vec;

use super::card::{Card, CardError, FRAME_MAX, Stats};
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::kprintln;
use crate::pci::{self, Device};

/// Изготовитель Intel.
const VENDOR_INTEL: u16 = 0x8086;

/// Карты семейства 8254x, которые этот драйвер поднимает.
///
/// Список, а не диапазон: соседние номера у Intel принадлежат другим семействам
/// (e1000e и ICH), и «поднять всё, что похоже» означало бы молчащую карту вместо
/// внятного «драйвера нет». Первым стоит 82540EM — та, которую эмулирует QEMU.
const SUPPORTED: &[u16] = &[
    0x100E, // 82540EM — QEMU `-device e1000`
    0x100F, // 82545EM
    0x1010, // 82546EB
    0x1011, // 82545EM (волокно)
    0x1012, // 82546EB (волокно)
    0x1015, // 82540EM LOM
    0x1016, // 82540EP LOM
    0x1017, // 82540EP
    0x1019, // 82547EI
    0x101A, // 82547EI mobile
    0x101D, // 82546EB четырёхпортовая
    0x1026, // 82545GM
    0x1027, // 82545GM (волокно)
    0x1028, // 82545GM (SerDes)
    0x1076, // 82541GI
    0x1077, // 82541GI mobile
    0x1078, // 82541ER
    0x107C, // 82541PI
];

// ---------------------------------------------------------------------------
// Регистры
// ---------------------------------------------------------------------------

const REG_CTRL: usize = 0x0000;
const REG_STATUS: usize = 0x0008;
const REG_EERD: usize = 0x0014;
const REG_ICR: usize = 0x00C0;
/// Разрешить причины прерывания (запись единиц).
const REG_IMS: usize = 0x00D0;
const REG_IMC: usize = 0x00D8;

/// Какие причины нас интересуют: пришёл кадр (`RXT0` — таймер приёма, `RXDMT0`
/// — кольцо наполовину пусто), переполнение приёма (`RXO`) и смена состояния
/// связи (`LSC`).
///
/// Передача сюда не входит намеренно: отправка у нас синхронная, и прерывание
/// «кадр ушёл» будило бы задачу ради того, чего она не ждёт.
const INTERRUPT_CAUSES: u32 = ICR_LSC | ICR_RXDMT0 | ICR_RXO | ICR_RXT0;

const ICR_LSC: u32 = 1 << 2;
const ICR_RXDMT0: u32 = 1 << 4;
const ICR_RXO: u32 = 1 << 6;
const ICR_RXT0: u32 = 1 << 7;

/// Окно регистров карты — для обработчика прерывания.
///
/// Обработчик вызывается без аргументов, а снять признак нужно именно в
/// регистрах: адрес приходится оставлять здесь. Карта поддерживается одна — та
/// же, что находит `probe`.
static CARD: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Прерывание от карты.
///
/// Линия `INTx` разделяемая, поэтому первое действие — спросить карту, её ли
/// это сигнал. Чтение `ICR` **снимает** причины, и в этом весь смысл: не сняв
/// их, мы оставили бы линию поднятой, а уровневое прерывание пришло бы снова и
/// снова, пока машина не встанет от занятости.
///
/// Ноль означает «не наша»: сосед по линии разберётся сам.
pub fn on_interrupt() {
    let base = CARD.load(core::sync::atomic::Ordering::Relaxed);
    if base == 0 {
        return;
    }
    // SAFETY: адрес положен сюда только после отображения окна, и окно живёт
    // всё время работы ядра.
    let causes = unsafe { read(VirtAddr::new(base), REG_ICR) };
    if causes == 0 {
        return;
    }
    crate::net::on_interrupt();
}
const REG_RCTL: usize = 0x0100;
const REG_TCTL: usize = 0x0400;
const REG_TIPG: usize = 0x0410;
const REG_RDBAL: usize = 0x2800;
const REG_RDBAH: usize = 0x2804;
const REG_RDLEN: usize = 0x2808;
const REG_RDH: usize = 0x2810;
const REG_RDT: usize = 0x2818;
const REG_TDBAL: usize = 0x3800;
const REG_TDBAH: usize = 0x3804;
const REG_TDLEN: usize = 0x3808;
const REG_TDH: usize = 0x3810;
const REG_TDT: usize = 0x3818;
/// Таблица многоадресной рассылки: 128 слов, все нули — «ничего лишнего».
const REG_MTA: usize = 0x5200;
const MTA_WORDS: usize = 128;
/// Младшая половина аппаратного адреса приёмного фильтра.
const REG_RAL0: usize = 0x5400;
/// Старшая половина и бит «запись действительна».
const REG_RAH0: usize = 0x5404;

/// `CTRL.SLU` — поднять линию. Без него 82540 не включает связь сама.
const CTRL_SLU: u32 = 1 << 6;
/// `CTRL.ASDE` — определять скорость и дуплекс автоматически.
const CTRL_ASDE: u32 = 1 << 5;
/// `CTRL.RST` — сброс устройства. Снимается самим устройством.
const CTRL_RST: u32 = 1 << 26;

/// `STATUS.LU` — связь есть.
const STATUS_LU: u32 = 1 << 1;

/// `EERD.START` — начать чтение слова EEPROM.
const EERD_START: u32 = 1 << 0;
/// `EERD.DONE` — слово прочитано.
const EERD_DONE: u32 = 1 << 4;
/// Сдвиг адреса слова в `EERD`.
const EERD_ADDR_SHIFT: u32 = 8;
/// Сдвиг прочитанных данных в `EERD`.
const EERD_DATA_SHIFT: u32 = 16;

/// `RCTL.EN` — приём включён.
const RCTL_EN: u32 = 1 << 1;
/// `RCTL.BAM` — принимать широковещательные кадры. Без него не работает ARP, а
/// значит, не работает ничего.
const RCTL_BAM: u32 = 1 << 15;
/// `RCTL.SECRC` — отрезать контрольную сумму кадра. Иначе к каждому кадру
/// приедут четыре лишних байта, и длина, которую увидит стек, будет чужой.
const RCTL_SECRC: u32 = 1 << 26;

/// `TCTL.EN` — передача включена.
const TCTL_EN: u32 = 1 << 1;
/// `TCTL.PSP` — дополнять короткие кадры до 64 байт.
const TCTL_PSP: u32 = 1 << 3;
/// Порог повторной передачи при столкновении — 15, как советует спецификация.
const TCTL_CT: u32 = 0x0F << 4;
/// Расстояние до позднего столкновения: 64 байта для полного дуплекса.
const TCTL_COLD: u32 = 0x40 << 12;

/// Межкадровые промежутки, рекомендованные для меди: 10, 8 и 6.
const TIPG_DEFAULT: u32 = 10 | (8 << 10) | (6 << 20);

/// `AV` — запись приёмного фильтра действительна.
const RAH_AV: u32 = 1 << 31;

/// Признак дескриптора: устройство с ним закончило.
const DESC_STATUS_DD: u8 = 1 << 0;
/// Признак дескриптора приёма: это последний кусок кадра.
const DESC_STATUS_EOP: u8 = 1 << 1;

/// Команда дескриптора передачи: конец кадра.
const TX_CMD_EOP: u8 = 1 << 0;
/// Команда: посчитать контрольную сумму кадра самому.
const TX_CMD_IFCS: u8 = 1 << 1;
/// Команда: отчитаться о завершении, поставив `DD`.
const TX_CMD_RS: u8 = 1 << 3;

/// Сколько дескрипторов в каждом кольце.
const RING: usize = 16;
/// Размер дескриптора обоих колец.
const DESC_SIZE: usize = 16;
/// Сколько места отводится под один кадр.
const BUFFER: usize = 2048;

/// Сколько раз опросить регистр, ожидая сброса или EEPROM.
///
/// Предел в оборотах, а не в миллисекундах, и это не лень: карта поднимается до
/// того, как в системе появляются часы, и `uptime_ms` на этой стадии на части
/// машин ещё стоит на месте. Опрос с пределом честно кончается отказом, а
/// ожидание по часам, которые не идут, — это вечная петля при загрузке.
const POLL_LIMIT: u32 = 2_000_000;

/// Чем карта может не подняться.
#[derive(Debug)]
pub enum E1000Error {
    /// Карты этого семейства на шине нет.
    NoCard,
    /// BAR0 не объявлен или не является памятью.
    BadBar,
    /// Не удалось отобразить окно регистров.
    MapFailed,
    /// Не удалось выделить кольца или буферы.
    NoMemory(DmaError),
    /// Карта не вышла из сброса.
    ResetTimeout,
    /// EEPROM не ответила, а фильтр пуст: аппаратного адреса взять негде.
    NoMac,
}

impl core::fmt::Display for E1000Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoCard => f.write_str("no 8254x card on the bus"),
            Self::BadBar => f.write_str("BAR0 is not a memory window"),
            Self::MapFailed => f.write_str("cannot map the register window"),
            Self::NoMemory(err) => write!(f, "cannot allocate the rings: {err}"),
            Self::ResetTimeout => f.write_str("the card did not come out of reset"),
            Self::NoMac => f.write_str("the card does not tell its hardware address"),
        }
    }
}

/// Карта Intel 8254x.
pub struct E1000 {
    /// Окно регистров.
    regs: VirtAddr,
    /// Кольцо приёма: `RING` дескрипторов по 16 байт.
    rx_ring: DmaBuffer,
    /// Кольцо передачи.
    tx_ring: DmaBuffer,
    /// Приёмные буферы: буфер `i` лежит по смещению `i * BUFFER`.
    rx_pool: DmaBuffer,
    tx_pool: DmaBuffer,
    /// Какой дескриптор приёма разбирать следующим.
    rx_next: usize,
    /// Куда класть следующий исходящий кадр.
    tx_next: usize,
    mac: [u8; 6],
    stats: Stats,
    /// Идентификатор модели — для журнала.
    device_id: u16,
    /// Где карта стоит на шине. Нужен переписи: отметка ставится по адресу
    /// функции, и без неё диспетчер устройств показал бы работающую карту как
    /// «драйвер есть, но этим устройством не занят».
    address: pci::Address,
    /// Кадры приходят прерыванием, а не опросом.
    interrupts: bool,
}

// SAFETY: вся изменяемая память карты — её собственные буферы DMA и окно
// регистров; интерфейс живёт под замком, и двух владельцев у карты нет.
unsafe impl Send for E1000 {}

impl E1000 {
    /// Найти карту на шине и подготовить её к работе.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц, а `root` —
    /// описывать живое окно конфигурационного пространства.
    pub unsafe fn probe(root: &pci::Root) -> Result<Self, E1000Error> {
        let mut found: Option<Device> = None;
        // SAFETY: контракт функции.
        unsafe {
            pci::for_each(root, |device| {
                if device.vendor == VENDOR_INTEL && SUPPORTED.contains(&device.device) {
                    found = Some(*device);
                    return false;
                }
                true
            });
        }
        let device = found.ok_or(E1000Error::NoCard)?;

        let bar = device.memory_bar(0).ok_or(E1000Error::BadBar)?;
        // Окно памяти и DMA разрешаются **до** первого обращения к регистрам, а
        // не после сброса. Порядок стоил одного провала на стенде: на x86-64
        // прошивка оставляет бит `Memory Space` включённым, и драйвер работал;
        // на AArch64 (EDK II на `virt`) он выключен, все чтения регистров
        // возвращали мусор, и карта «никогда не выходила из сброса» — при том
        // что сброс до неё попросту не доезжал. Так же поступает `ahci`.
        //
        // SAFETY: адресов колец карта ещё не знает, читать ей нечего.
        unsafe { device.enable_bus_master() };
        // SAFETY: условие делегировано вызывающему.
        let regs = unsafe { map_window(bar) }?;

        // Прерываний карты мы не просим: очередь опрашивает задача приёмника,
        // как и у virtio-net. Выключить их надо до сброса и после него — иначе
        // линия INTx останется поднятой и заберёт вектор, который никто не ждёт.
        // SAFETY: окно отображено.
        unsafe { write(regs, REG_IMC, u32::MAX) };
        // SAFETY: см. выше.
        unsafe { reset(regs) }?;
        // SAFETY: см. выше.
        unsafe {
            write(regs, REG_IMC, u32::MAX);
            let _ = read(regs, REG_ICR);
        }

        // SAFETY: окно отображено, карта сброшена.
        let mac = unsafe { hardware_address(regs) }?;

        let rx_ring = dma::alloc(RING * DESC_SIZE).map_err(E1000Error::NoMemory)?;
        let tx_ring = dma::alloc(RING * DESC_SIZE).map_err(E1000Error::NoMemory)?;
        let rx_pool = dma::alloc(RING * BUFFER).map_err(E1000Error::NoMemory)?;
        let tx_pool = dma::alloc(RING * BUFFER).map_err(E1000Error::NoMemory)?;
        rx_ring.zero();
        tx_ring.zero();

        let mut card = Self {
            regs,
            rx_ring,
            tx_ring,
            rx_pool,
            tx_pool,
            rx_next: 0,
            tx_next: 0,
            mac,
            stats: Stats::default(),
            device_id: device.device,
            address: device.address,
            interrupts: false,
        };

        // SAFETY: кольца выделены и обнулены, окно отображено.
        unsafe { card.start() };
        // SAFETY: карта работает, окно отображено.
        card.interrupts = unsafe { card.enable_interrupts(&device) };
        Ok(card)
    }

    /// Попросить прерывания вместо опроса.
    ///
    /// Возвращает `false`, если линия неизвестна или её не удалось разрешить.
    /// Тогда карта остаётся на опросе и работает ровно как раньше — отказ здесь
    /// не ошибка, а другой режим работы, и назван он вслух.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено, карта — запущена.
    unsafe fn enable_interrupts(&self, device: &Device) -> bool {
        // Адрес окна кладётся **до** разрешения причин: прерывание может прийти
        // сразу, а обработчик без адреса не снимет признак — то есть оставит
        // уровневую линию поднятой навсегда.
        CARD.store(self.regs.as_usize(), core::sync::atomic::Ordering::Relaxed);

        let Some(gsi) = crate::irq::routing::request(device, on_interrupt) else {
            kprintln!("  e1000       : no interrupt line known; frames will be polled for");
            CARD.store(0, core::sync::atomic::Ordering::Relaxed);
            return false;
        };

        // SAFETY: контракт функции.
        unsafe {
            // Накопленные причины снимаются до разрешения: иначе первое же
            // прерывание придёт за событие, которого мы не видели.
            let _ = read(self.regs, REG_ICR);
            write(self.regs, REG_IMS, INTERRUPT_CAUSES);
        }
        kprintln!("  e1000       : INTx on GSI {gsi}, frames arrive by interrupt");
        true
    }

    /// Какая это модель — для журнала.
    #[must_use]
    pub const fn device_id(&self) -> u16 {
        self.device_id
    }

    /// Где карта стоит на шине.
    #[must_use]
    pub const fn address(&self) -> pci::Address {
        self.address
    }

    /// Есть ли связь на проводе.
    ///
    /// Спрашивается один раз, при подъёме, и печатается: «карта поднялась, но
    /// провод не воткнут» — самая частая причина молчащей сети, и узнавать её
    /// пересказом DHCP-таймаутов незачем.
    #[must_use]
    pub fn link_up(&self) -> bool {
        // SAFETY: окно отображено на всё время жизни карты.
        unsafe { read(self.regs, REG_STATUS) & STATUS_LU != 0 }
    }

    /// Включить приём и передачу.
    ///
    /// # Safety
    ///
    /// Кольца выделены и обнулены, окно регистров отображено.
    unsafe fn start(&self) {
        let regs = self.regs;

        // Приёмный фильтр: свой адрес и ничего больше. Таблица многоадресной
        // рассылки обнуляется явно — после сброса в ней мусор, и карта,
        // принимающая чужие группы, отдаёт стеку кадры, на которые он ответит.
        // SAFETY: контракт функции.
        unsafe {
            let low = u32::from_le_bytes([self.mac[0], self.mac[1], self.mac[2], self.mac[3]]);
            let high = u32::from(u16::from_le_bytes([self.mac[4], self.mac[5]]));
            write(regs, REG_RAL0, low);
            write(regs, REG_RAH0, high | RAH_AV);
            for word in 0..MTA_WORDS {
                write(regs, REG_MTA + word * 4, 0);
            }
        }

        // Кольцо приёма: каждому дескриптору — свой буфер.
        for index in 0..RING {
            let buffer = self.rx_pool.phys().as_u64() + (index * BUFFER) as u64;
            // SAFETY: дескриптор внутри выделенного кольца.
            unsafe {
                let desc = self.rx_desc(index);
                desc.cast::<u64>().write_volatile(buffer);
                desc.add(8).cast::<u16>().write_volatile(0);
                desc.add(12).write_volatile(0);
            }
        }

        // SAFETY: контракт функции.
        unsafe {
            write(regs, REG_RDBAL, self.rx_ring.phys().as_u64() as u32);
            write(regs, REG_RDBAH, (self.rx_ring.phys().as_u64() >> 32) as u32);
            write(regs, REG_RDLEN, (RING * DESC_SIZE) as u32);
            write(regs, REG_RDH, 0);
            // Хвост — на последнем дескрипторе: карте принадлежит всё, что
            // между головой и хвостом, а равные голова и хвост означают пустое
            // кольцо и потерянный первый же кадр.
            write(regs, REG_RDT, (RING - 1) as u32);
            write(regs, REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);

            write(regs, REG_TDBAL, self.tx_ring.phys().as_u64() as u32);
            write(regs, REG_TDBAH, (self.tx_ring.phys().as_u64() >> 32) as u32);
            write(regs, REG_TDLEN, (RING * DESC_SIZE) as u32);
            write(regs, REG_TDH, 0);
            write(regs, REG_TDT, 0);
            write(regs, REG_TIPG, TIPG_DEFAULT);
            write(regs, REG_TCTL, TCTL_EN | TCTL_PSP | TCTL_CT | TCTL_COLD);

            // Линия поднимается последней: до этой минуты карта не должна
            // принимать ничего, потому что принимать некуда.
            let ctrl = read(regs, REG_CTRL);
            write(regs, REG_CTRL, ctrl | CTRL_SLU | CTRL_ASDE);
        }
    }

    /// Адрес дескриптора приёма.
    fn rx_desc(&self, index: usize) -> *mut u8 {
        // SAFETY: кольцо выделено на `RING * DESC_SIZE` байт, индекс меньше `RING`.
        unsafe { self.rx_ring.as_ptr::<u8>().add(index * DESC_SIZE) }
    }

    /// Адрес дескриптора передачи.
    fn tx_desc(&self, index: usize) -> *mut u8 {
        // SAFETY: см. `rx_desc`.
        unsafe { self.tx_ring.as_ptr::<u8>().add(index * DESC_SIZE) }
    }
}

impl Card for E1000 {
    fn name(&self) -> &'static str {
        "e1000"
    }

    fn interrupts(&self) -> bool {
        self.interrupts
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), CardError> {
        if frame.len() > FRAME_MAX {
            self.stats.tx_dropped += 1;
            return Err(CardError::TooLong(frame.len()));
        }

        let index = self.tx_next;
        let desc = self.tx_desc(index);
        // SAFETY: дескриптор внутри кольца.
        let status = unsafe { desc.add(12).read_volatile() };
        // SAFETY: см. выше.
        let length = unsafe { desc.add(8).cast::<u16>().read_volatile() };
        // Дескриптор занят: он уже был использован (длина ненулевая) и карта с
        // ним ещё не закончила. Ждать нечего — очередь передачи забита, и кадр
        // теряется здесь, а не превращается в остановку системы.
        if length != 0 && status & DESC_STATUS_DD == 0 {
            self.stats.tx_dropped += 1;
            return Err(CardError::Busy);
        }

        // SAFETY: буфер выделен на `RING * BUFFER` байт, индекс меньше `RING`,
        // длина кадра проверена выше.
        unsafe {
            let buffer = self.tx_pool.as_ptr::<u8>().add(index * BUFFER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer, frame.len());
        }

        let phys = self.tx_pool.phys().as_u64() + (index * BUFFER) as u64;
        // SAFETY: дескриптор внутри кольца; порядок записи — сначала описание
        // кадра, потом команда: устройство читает дескриптор целиком по хвосту,
        // и обратный порядок означал бы отправку старого содержимого.
        unsafe {
            desc.cast::<u64>().write_volatile(phys);
            desc.add(8).cast::<u16>().write_volatile(frame.len() as u16);
            desc.add(10).write_volatile(0);
            desc.add(11).write_volatile(TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS);
            desc.add(12).write_volatile(0);
            desc.add(13).write_volatile(0);
            desc.add(14).cast::<u16>().write_volatile(0);
        }

        self.tx_next = (index + 1) % RING;
        // SAFETY: окно отображено.
        unsafe { write(self.regs, REG_TDT, self.tx_next as u32) };

        self.stats.tx_frames += 1;
        self.stats.tx_bytes += frame.len() as u64;
        Ok(())
    }

    fn receive(&mut self, frame: &mut [u8; FRAME_MAX]) -> Option<usize> {
        let index = self.rx_next;
        let desc = self.rx_desc(index);
        // SAFETY: дескриптор внутри кольца.
        let status = unsafe { desc.add(12).read_volatile() };
        if status & DESC_STATUS_DD == 0 {
            return None;
        }

        // SAFETY: см. выше.
        let length = unsafe { desc.add(8).cast::<u16>().read_volatile() } as usize;
        let taken = if status & DESC_STATUS_EOP == 0 || length == 0 || length > FRAME_MAX {
            // Кадр, разложенный по нескольким буферам, здесь невозможен: буфер
            // вдвое длиннее наибольшего кадра, а длинные кадры не разрешены
            // (`RCTL.LPE` выключен). Значит, это испорченный дескриптор, и
            // правильный ответ — посчитать потерю и вернуть буфер карте.
            self.stats.rx_dropped += 1;
            None
        } else {
            // SAFETY: буфер выделен, длина проверена.
            unsafe {
                let buffer = self.rx_pool.as_ptr::<u8>().add(index * BUFFER);
                core::ptr::copy_nonoverlapping(buffer, frame.as_mut_ptr(), length);
            }
            self.stats.rx_frames += 1;
            self.stats.rx_bytes += length as u64;
            Some(length)
        };

        // Дескриптор возвращается карте в обоих случаях: статус снимается, а
        // хвост двигается на него — теперь этот буфер снова принадлежит карте.
        // SAFETY: дескриптор внутри кольца, окно отображено.
        unsafe {
            desc.add(12).write_volatile(0);
            write(self.regs, REG_RDT, index as u32);
        }
        self.rx_next = (index + 1) % RING;
        taken
    }

    fn stats(&self) -> Stats {
        self.stats
    }
}

/// Сбросить карту и дождаться, пока она выйдет из сброса.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn reset(regs: VirtAddr) -> Result<(), E1000Error> {
    // SAFETY: контракт функции.
    unsafe {
        let ctrl = read(regs, REG_CTRL);
        write(regs, REG_CTRL, ctrl | CTRL_RST);
    }
    for _ in 0..POLL_LIMIT {
        // SAFETY: см. выше.
        if unsafe { read(regs, REG_CTRL) } & CTRL_RST == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(E1000Error::ResetTimeout)
}

/// Узнать аппаратный адрес карты.
///
/// Сначала — приёмный фильтр: прошивка, грузившаяся по сети, уже прочитала
/// адрес и положила его туда, и это быстрее и надёжнее, чем EEPROM. Если фильтр
/// пуст, читаются три слова EEPROM.
///
/// # Safety
///
/// Окно регистров отображено, карта сброшена.
unsafe fn hardware_address(regs: VirtAddr) -> Result<[u8; 6], E1000Error> {
    // SAFETY: контракт функции.
    let (low, high) = unsafe { (read(regs, REG_RAL0), read(regs, REG_RAH0)) };
    if high & RAH_AV != 0 && (low != 0 || high & 0xFFFF != 0) {
        let low = low.to_le_bytes();
        let high = (high as u16).to_le_bytes();
        return Ok([low[0], low[1], low[2], low[3], high[0], high[1]]);
    }

    let mut mac = [0u8; 6];
    for word in 0..3u32 {
        // SAFETY: см. выше.
        let value = unsafe { eeprom_word(regs, word) }.ok_or(E1000Error::NoMac)?;
        let bytes = value.to_le_bytes();
        mac[word as usize * 2] = bytes[0];
        mac[word as usize * 2 + 1] = bytes[1];
    }
    if mac == [0u8; 6] {
        return Err(E1000Error::NoMac);
    }
    Ok(mac)
}

/// Прочитать слово EEPROM. `None` — карта не ответила.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn eeprom_word(regs: VirtAddr, word: u32) -> Option<u16> {
    // SAFETY: контракт функции.
    unsafe { write(regs, REG_EERD, EERD_START | (word << EERD_ADDR_SHIFT)) };
    for _ in 0..POLL_LIMIT {
        // SAFETY: см. выше.
        let value = unsafe { read(regs, REG_EERD) };
        if value & EERD_DONE != 0 {
            return Some((value >> EERD_DATA_SHIFT) as u16);
        }
        core::hint::spin_loop();
    }
    None
}

/// Отобразить окно регистров карты.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_window(phys: PhysAddr) -> Result<VirtAddr, E1000Error> {
    // Окно карты — 128 КиБ, из которых нам нужны первые 0x5400 с хвостом на
    // приёмный фильтр. Отображается страничный кратный кусок с запасом: размер
    // BAR узнать нельзя, не записав в него (см. [`pci::Device::memory_bar`]).
    let span = (REG_RAH0 + PAGE_SIZE).next_multiple_of(PAGE_SIZE);
    let virt = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
    // SAFETY: условие делегировано вызывающему. Это регистры устройства:
    // семантика `DEVICE` обязательна — на кешируемой памяти запись в `TDT`
    // случилась бы когда-нибудь потом.
    unsafe { crate::arch::map_active(virt, phys, span, flags) }.map_err(|_| E1000Error::MapFailed)?;
    Ok(virt)
}

/// Прочитать регистр.
///
/// # Safety
///
/// Окно отображено, смещение внутри него.
unsafe fn read(regs: VirtAddr, offset: usize) -> u32 {
    // SAFETY: контракт функции.
    unsafe { ((regs.as_usize() + offset) as *const u32).read_volatile() }
}

/// Записать регистр.
///
/// # Safety
///
/// Окно отображено, смещение внутри него.
unsafe fn write(regs: VirtAddr, offset: usize, value: u32) {
    // SAFETY: контракт функции.
    unsafe { ((regs.as_usize() + offset) as *mut u32).write_volatile(value) };
}

/// Поднимает ли этот драйвер такую карту.
///
/// Спрашивает перепись устройств: ей нужно назвать драйвер до того, как драйвер
/// что-нибудь поднял, — иначе окно «диспетчер устройств» на машине без сети
/// молчало бы о том, что карта в системе вообще известна.
#[must_use]
pub fn supports(vendor: u16, device: u16) -> bool {
    vendor == VENDOR_INTEL && SUPPORTED.contains(&device)
}

/// Сетевые карты на шине, для которых в ядре нет драйвера.
///
/// Существует ради одной строки в журнале чужой машины: «карта такая-то, а
/// драйвера к ней нет». Без неё человек видит «сети нет» и не знает, что
/// прислать разработчику; с ней — присылает четыре и четыре цифры, по которым
/// пишется драйвер.
///
/// # Safety
///
/// Та же, что у [`pci::for_each`].
pub unsafe fn undriven(root: &pci::Root) -> Vec<(pci::Address, u16, u16)> {
    let mut found = Vec::new();
    // SAFETY: контракт функции.
    unsafe {
        pci::for_each(root, |device| {
            if device.class == 0x02 {
                found.push((device.address, device.vendor, device.device));
            }
            true
        });
    }
    found
}
