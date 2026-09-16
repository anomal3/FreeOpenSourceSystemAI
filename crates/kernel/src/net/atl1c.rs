//! Atheros AR8151 (`atl1c`) — сетевая карта ноутбуков Intel того поколения.
//!
//! # Откуда она здесь
//!
//! Из переписи шины ноутбука ASUS K53SD: `0000:05:00.0 1969:1083 class
//! 02:00:00 ethernet controller -- NO DRIVER IN THIS KERNEL`. Это AR8151
//! ревизии 2.0 — гигабитная карта Atheros, которая стоит в огромном числе
//! ноутбуков 2010–2013 годов. Пока драйвера не было, «универсальная система» на
//! этой машине не могла ни обновиться, ни пустить к себе по сети.
//!
//! # Чем она отличается от e1000
//!
//! Тремя вещами, и каждая меняет код.
//!
//! **Приём описывается двумя кольцами, а не одним.** В кольце `RFD` лежат
//! только адреса свободных буферов, а результаты карта пишет в отдельное кольцо
//! `RRD`: там длина, ошибки и **номер буфера**, в который она положила кадр.
//! Соответствие между кольцами — один к одному, но читать надо второе, а
//! пополнять первое.
//!
//! **У дескриптора передачи нет признака «готово».** Узнать, какие из них карта
//! уже отправила, можно только по её счётчику — шестнадцатибитному регистру
//! `TPD_PRI0_CIDX`. Всё, что между ним и нашим счётчиком, ещё в полёте.
//!
//! **PHY отдельный и его надо будить руками.** У 8254x медный приёмопередатчик
//! включается сам; здесь он сидит за своим сбросом (`GPHY_CTRL`), общается
//! через MDIO, и его отладочные регистры требуют значений, зависящих от
//! ревизии. Без этого карта отвечает на регистры, но линии не поднимает
//! никогда.
//!
//! # Что здесь проверено, а что нет — вслух
//!
//! **Ничего из этого файла не проверяется на стенде.** QEMU такой карты не
//! эмулирует, VirtualBox тоже; единственная проверка — ноутбук. Поэтому каждый
//! шаг подъёма печатается в журнал: строка «карта не поднялась, стадия такая-то»
//! стоит дороже, чем красивый код, когда машина за тысячу километров и вся связь
//! с ней — фотография экрана.
//!
//! Устройство карты выяснено по её открытой реализации в Linux, которая читается
//! как документация: спецификации Atheros не публикует. Код там под GPLv2 и
//! несовместим с нашей GPLv3, поэтому заимствованы **сведения о железе** —
//! адреса регистров, порядок включения, раскладки дескрипторов, — а не строки.

use super::card::{Card, CardError, FRAME_MAX, Stats};
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};
use crate::pci::{self, Device};

/// Изготовитель Atheros (ныне Qualcomm Atheros).
const VENDOR_ATHEROS: u16 = 0x1969;

/// Карты, которые этот драйвер поднимает, и их имена для журнала.
///
/// Только две ревизии, и это осознанное ограничение. У семейства пять подвидов
/// (`l1c`, `l2c`, `l2c_b`, `l2c_b2`, `l1d`, `l1d_2`), и отличаются они не
/// названием, а **последовательностью включения**: у младших карт другие
/// значения отладочных регистров PHY, другое управление питанием и другой набор
/// обходов ошибок кремния. Заявить их все значило бы получить карту, которая
/// «нашлась» и молчит, — то есть ровно то состояние, ради выхода из которого всё
/// это и писалось.
const SUPPORTED: &[(u16, &str)] = &[
    (0x1073, "AR8151 v1.0"),
    (0x1083, "AR8151 v2.0"),
];

// ---------------------------------------------------------------------------
// Регистры
// ---------------------------------------------------------------------------

const REG_MASTER_CTRL: usize = 0x1400;
const REG_GPHY_CTRL: usize = 0x140C;
const REG_IDLE_STATUS: usize = 0x1410;
const REG_MDIO_CTRL: usize = 0x1414;
const REG_SERDES: usize = 0x1424;
const REG_MDIO_EXTN: usize = 0x1448;
const REG_LPI_CTRL: usize = 0x1440;
const REG_MAC_CTRL: usize = 0x1480;
/// Аппаратный адрес: младшее слово — байты 2..5, старшее (`+4`) — байты 0..1.
const REG_MAC_STA_ADDR: usize = 0x1488;
const REG_MTU: usize = 0x149C;
const REG_WOL_CTRL: usize = 0x14A0;
const REG_LOAD_PTR: usize = 0x1534;
const REG_RX_BASE_ADDR_HI: usize = 0x1540;
const REG_TX_BASE_ADDR_HI: usize = 0x1544;
const REG_RFD0_HEAD_ADDR_LO: usize = 0x1550;
const REG_RFD_RING_SIZE: usize = 0x1560;
const REG_RX_BUF_SIZE: usize = 0x1564;
const REG_RRD0_HEAD_ADDR_LO: usize = 0x1568;
const REG_RRD_RING_SIZE: usize = 0x1578;
const REG_TPD_PRI0_ADDR_LO: usize = 0x1580;
const REG_TPD_RING_SIZE: usize = 0x1584;
const REG_TXQ_CTRL: usize = 0x1590;
const REG_TXF_WATER_MARK: usize = 0x1598;
const REG_RXQ_CTRL: usize = 0x15A0;
const REG_RXD_DMA_CTRL: usize = 0x15AC;
const REG_DMA_CTRL: usize = 0x15C0;
const REG_SMB_STAT_TIMER: usize = 0x15C4;
/// Сколько приёмных буферов отдано карте (мейлбокс очереди 0).
const REG_MB_RFD0_PROD_IDX: usize = 0x15E0;
/// Наш счётчик отправленных дескрипторов — **шестнадцать бит**.
const REG_TPD_PRI0_PIDX: usize = 0x15F2;
/// Счётчик карты: всё до него уже ушло в провод. Тоже шестнадцать бит.
const REG_TPD_PRI0_CIDX: usize = 0x15F6;
const REG_ISR: usize = 0x1600;
const REG_IMR: usize = 0x1604;
const REG_CLK_GATING_CTRL: usize = 0x1814;

// MASTER_CTRL
const MASTER_CTRL_SOFT_RST: u32 = 0x1;
const MASTER_CTRL_OOB_DIS: u32 = 0x40;
const MASTER_CTRL_SA_TIMER_EN: u32 = 0x80;
const MASTER_CTRL_TX_ITIMER_EN: u32 = 0x400;
const MASTER_CTRL_RX_ITIMER_EN: u32 = 0x800;
const MASTER_CTRL_INT_RDCLR: u32 = 0x4000;

// GPHY_CTRL
const GPHY_CTRL_EXT_RESET: u32 = 1 << 0;
const GPHY_CTRL_GATE_25M_EN: u32 = 1 << 5;
const GPHY_CTRL_PHY_IDDQ: u32 = 1 << 7;
const GPHY_CTRL_HIB_EN: u32 = 1 << 10;
const GPHY_CTRL_HIB_PULSE: u32 = 1 << 11;
const GPHY_CTRL_SEL_ANA_RST: u32 = 1 << 12;
const GPHY_CTRL_PWDOWN_HW: u32 = 1 << 14;
/// Сколько десятков микросекунд держать внешний сброс PHY: 80 × 10 = 800 мкс.
const GPHY_CTRL_EXT_RST_TO: u32 = 80;

// IDLE_STATUS
const IDLE_STATUS_RXMAC_BUSY: u32 = 1 << 0;
const IDLE_STATUS_TXMAC_BUSY: u32 = 1 << 1;
const IDLE_STATUS_RXQ_BUSY: u32 = 1 << 2;
const IDLE_STATUS_TXQ_BUSY: u32 = 1 << 3;
const IDLE_STATUS_MASK: u32 =
    IDLE_STATUS_RXMAC_BUSY | IDLE_STATUS_TXMAC_BUSY | IDLE_STATUS_RXQ_BUSY | IDLE_STATUS_TXQ_BUSY;

// MDIO_CTRL
const MDIO_CTRL_DATA_MASK: u32 = 0xFFFF;
const MDIO_CTRL_REG_SHIFT: u32 = 16;
const MDIO_CTRL_OP_READ: u32 = 1 << 21;
const MDIO_CTRL_START: u32 = 1 << 23;
const MDIO_CTRL_BUSY: u32 = 1 << 27;
const MDIO_CTRL_MODE_EXT: u32 = 1 << 30;
/// Делитель тактовой частоты MDIO: 25 МГц / 4.
const MDIO_CTRL_CLK_25_4: u32 = 0;
const MDIO_CTRL_CLK_SEL_SHIFT: u32 = 24;

// MDIO_EXTN
const MDIO_EXTN_DEVAD_SHIFT: u32 = 16;
const MDIO_EXTN_PORTAD_SHIFT: u32 = 21;

// SERDES
const SERDES_MAC_CLK_SLOWDOWN: u32 = 0x20000;
const SERDES_PHY_CLK_SLOWDOWN: u32 = 0x40000;

// LPI (Energy Efficient Ethernet)
const LPI_CTRL_EN: u32 = 0x1;

// MAC_CTRL
const MAC_CTRL_TX_EN: u32 = 1 << 0;
const MAC_CTRL_RX_EN: u32 = 1 << 1;
const MAC_CTRL_TX_FLOW: u32 = 1 << 2;
const MAC_CTRL_RX_FLOW: u32 = 1 << 3;
const MAC_CTRL_DUPLX: u32 = 1 << 5;
const MAC_CTRL_ADD_CRC: u32 = 1 << 6;
const MAC_CTRL_PAD: u32 = 1 << 7;
const MAC_CTRL_PRMLEN_SHIFT: u32 = 10;
const MAC_CTRL_BC_EN: u32 = 1 << 26;
const MAC_CTRL_SINGLE_PAUSE_EN: u32 = 1 << 28;
const MAC_CTRL_HASH_ALG_CRC32: u32 = 1 << 29;
const MAC_CTRL_SPEED_MODE_SW: u32 = 1 << 30;
const MAC_CTRL_SPEED_SHIFT: u32 = 20;
const MAC_CTRL_SPEED_10_100: u32 = 1;
const MAC_CTRL_SPEED_1000: u32 = 2;
/// Длина преамбулы кадра, как её задаёт драйвер Linux.
const PREAMBLE_LEN: u32 = 7;

// TXQ_CTRL: пачка дескрипторов 5, расширенный режим, длина 802.3, опции IP,
// и предвыборка передающего буфера 0x200 (значение для гигабитных карт).
const TXQ_CTRL_EN: u32 = 1 << 5;
const TXQ_CFGV: u32 = 5 | (1 << 4) | (1 << 6) | (1 << 7);
const TXQ_BURST_SHIFT: u32 = 16;
const L1C_TXQ_BURST: u32 = 0x200;

// RXQ_CTRL
const RXQ_RFD_BURST_SHIFT: u32 = 20;
const RXQ_RFD_BURST: u32 = 8;
const RXQ_CTRL_EN: u32 = 1 << 31;

// DMA_CTRL: чтение «out of order», приоритет данным, пачка чтения 1024 байта,
// задержки по умолчанию.
const DMA_CTRL_RORDER_MODE_OUT: u32 = 4;
const DMA_CTRL_RREQ_BLEN_1024: u32 = 3;
const DMA_CTRL_RREQ_BLEN_SHIFT: u32 = 4;
const DMA_CTRL_RREQ_PRI_DATA: u32 = 1 << 10;
const DMA_CTRL_RDLY_CNT_DEF: u32 = 15;
const DMA_CTRL_RDLY_CNT_SHIFT: u32 = 11;
const DMA_CTRL_WDLY_CNT_DEF: u32 = 4;
const DMA_CTRL_WDLY_CNT_SHIFT: u32 = 16;

/// Таймер сбора статистики карты; она нам не нужна, но регистр должен быть
/// осмысленным.
const SMB_TIMER: u32 = 200_000;

// Регистры PHY (стандартные номера MII).
const MII_BMCR: u8 = 0x00;
const MII_ADVERTISE: u8 = 0x04;
const MII_CTRL1000: u8 = 0x09;
const MII_PHYSID1: u8 = 0x02;
/// Регистр состояния, из которого читается **договорённая** скорость.
const MII_GIGA_PSSR: u8 = 0x11;
const MII_IER: u8 = 0x12;
/// Адресный и информационный регистры отладочного порта PHY.
const MII_DBG_ADDR: u8 = 0x1D;
const MII_DBG_DATA: u8 = 0x1E;

const BMCR_RESET: u16 = 0x8000;
const BMCR_ANENABLE: u16 = 0x1000;
const BMCR_ANRESTART: u16 = 0x0200;

const ADVERTISE_ALL: u16 = 0x01E0 | 0x0001;
const ADVERTISE_1000: u16 = 0x0300;

const IER_LINK_UP: u16 = 0x0400;
const IER_LINK_DOWN: u16 = 0x0800;

const GIGA_PSSR_SPD_DPLX_RESOLVED: u16 = 0x0800;
const GIGA_PSSR_DPLX: u16 = 0x2000;
const GIGA_PSSR_SPEED: u16 = 0xC000;
const GIGA_PSSR_100MBS: u16 = 0x4000;
const GIGA_PSSR_1000MBS: u16 = 0x8000;

// Значения отладочных регистров PHY для ревизий L1D — те, при которых карта
// работает. Числа не выводятся ни из чего: это настройки аналоговой части,
// подобранные изготовителем.
const MIIDBG_ANACTRL: u16 = 0x00;
const ANACTRL_DEF: u16 = 0x02EF;
const MIIDBG_SYSMODCTRL: u16 = 0x04;
const L1D_SYSMODCTRL_IECHOADJ_DEF: u16 = 0x4FBB;
const MIIDBG_SRDSYSMOD: u16 = 0x05;
const SRDSYSMOD_DEF: u16 = 0x2C46;
const MIIDBG_TST10BTCFG: u16 = 0x12;
const TST10BTCFG_DEF: u16 = 0x4C04;
const MIIDBG_LEGCYPS: u16 = 0x29;
const L1D_LEGCYPS_DEF: u16 = 0x129D;
const MIIDBG_TST100BTCFG: u16 = 0x36;
const TST100BTCFG_DEF: u16 = 0xE12C;

/// Расширенные регистры PHY: выключение EEE.
const MIIEXT_ANEG: u16 = 7;
const MIIEXT_LOCAL_EEEADV: u16 = 0x3C;
const MIIEXT_PCS: u16 = 3;
const MIIEXT_CLDCTRL3: u16 = 0x8003;
const L2CB_CLDCTRL3: u16 = 0x4D19;

// ---------------------------------------------------------------------------
// Кольца
// ---------------------------------------------------------------------------

/// Сколько дескрипторов в каждом кольце.
///
/// Тридцать два — не из спецификации, а из соображения «вчетверо больше, чем
/// успевает прийти между двумя обходами задачи-приёмника». Колец три, и все
/// одной длины: соответствие `RFD` и `RRD` в этой карте один к одному, а
/// передающее кольцо незачем делать другим.
const RING: usize = 32;
/// Размер дескриптора свободного приёмного буфера — только адрес.
const RFD_SIZE: usize = 8;
/// Размер дескриптора-результата приёма.
const RRD_SIZE: usize = 16;
/// Размер дескриптора передачи.
const TPD_SIZE: usize = 16;
/// Сколько места отводится под один кадр.
const BUFFER: usize = 2048;
/// Что объявлено карте как размер приёмного буфера: кадр с заголовком, меткой
/// VLAN и контрольной суммой, округлённый вверх до восьми.
const RX_BUF_SIZE: u32 = 1536;
/// Наибольший кадр, который карта примет.
const MTU_VALUE: u32 = 1500 + 14 + 4 + 4;

/// Признак дескриптора-результата: карта его заполнила.
const RRS_RXD_UPDATED: u32 = 0x8000_0000;
/// Ошибка кадра, одним битом на все причины.
const RRS_RX_ERR_SUM: u32 = 0x0010_0000;
/// Длина, не совпавшая с полем 802.3.
const RRS_802_3_LEN_ERR: u32 = 0x4000_0000;
const RRS_PKT_SIZE_MASK: u32 = 0x3FFF;
const RRS_RFD_INDEX_SHIFT: u32 = 20;
const RRS_RFD_INDEX_MASK: u32 = 0x0FFF;

/// Дескриптор передачи: конец кадра.
const TPD_EOP: u32 = 1 << 31;

/// Оборотов на десять микросекунд — грубая мера, которой хватает на паузы при
/// сбросе PHY.
///
/// Именно грубая: настоящая длительность оборота зависит от машины, и
/// единственное требование к этим паузам — «не короче». Взять их из часов
/// нельзя по той же причине, что и предел опроса ниже.
const SHORT_SPIN: u32 = 10_000;

/// Сколько раз опросить регистр, прежде чем признать отказ.
///
/// В оборотах, а не в миллисекундах, и по той же причине, что у [`super::e1000`]:
/// карта поднимается на стадии загрузки, где часы на части машин ещё стоят, и
/// ожидание по ним превратилось бы в вечную петлю.
const POLL_LIMIT: u32 = 2_000_000;
/// Сколько оборотов ждать простоя блоков при сбросе.
const IDLE_LIMIT: u32 = 8_000_000;

/// Чем карта может не подняться. Стадии названы словами: на чужой машине
/// единственное свидетельство — строка в журнале.
#[derive(Debug)]
pub enum Atl1cError {
    /// Карты этого семейства на шине нет.
    NoCard,
    /// BAR0 не объявлен памятью.
    BadBar,
    /// Не удалось отобразить окно регистров.
    MapFailed,
    /// Не удалось выделить кольца или буферы.
    NoMemory(DmaError),
    /// Блоки карты не остановились.
    NotIdle,
    /// Карта не вышла из сброса.
    ResetTimeout,
    /// PHY не отвечает по MDIO.
    PhyTimeout,
    /// Аппаратного адреса нет ни в регистрах, ни в EEPROM.
    NoMac,
}

impl core::fmt::Display for Atl1cError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoCard => f.write_str("no Atheros card this driver knows"),
            Self::BadBar => f.write_str("BAR0 is not a memory window"),
            Self::MapFailed => f.write_str("cannot map the register window"),
            Self::NoMemory(err) => write!(f, "cannot allocate the rings: {err}"),
            Self::NotIdle => f.write_str("the card did not stop its queues"),
            Self::ResetTimeout => f.write_str("the card did not come out of reset"),
            Self::PhyTimeout => f.write_str("the PHY does not answer over MDIO"),
            Self::NoMac => f.write_str("the card does not tell its hardware address"),
        }
    }
}

/// Карта Atheros AR8151.
pub struct Atl1c {
    regs: VirtAddr,
    /// Кольцо адресов свободных приёмных буферов.
    rfd: DmaBuffer,
    /// Кольцо результатов приёма.
    rrd: DmaBuffer,
    /// Кольцо передачи.
    tpd: DmaBuffer,
    rx_pool: DmaBuffer,
    tx_pool: DmaBuffer,
    /// Какой результат приёма разбирать следующим.
    rx_next: usize,
    /// Куда класть следующий исходящий кадр.
    tx_next: usize,
    mac: [u8; 6],
    stats: Stats,
    device_id: u16,
    address: pci::Address,
    /// Скорость, на которую настроен MAC сейчас: 0 — линии нет.
    speed: u16,
    /// Через сколько обходов задачи спрашивать PHY о линии.
    link_countdown: u32,
}

// SAFETY: вся изменяемая память карты — её собственные буферы DMA и окно
// регистров; интерфейс живёт под замком, и двух владельцев у карты нет.
unsafe impl Send for Atl1c {}

impl Atl1c {
    /// Найти карту на шине и поднять её.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц, а `root` —
    /// описывать живое окно конфигурационного пространства.
    pub unsafe fn probe(root: &pci::Root) -> Result<Self, Atl1cError> {
        let mut found: Option<Device> = None;
        // SAFETY: контракт функции.
        unsafe {
            pci::for_each(root, |device| {
                if device.vendor == VENDOR_ATHEROS && supports(device.vendor, device.device) {
                    found = Some(*device);
                    return false;
                }
                true
            });
        }
        let device = found.ok_or(Atl1cError::NoCard)?;
        let name = model_name(device.device);
        crate::kprintln!("  network     : atl1c: {} at {}, bringing it up", name, device.address);

        // Окно памяти и DMA — до первого обращения к регистрам. Урок, купленный
        // драйвером e1000: на машине, где прошивка окно не открыла, все чтения
        // возвращают мусор, и карта выглядит вечно сброшенной.
        //
        // SAFETY: адресов колец карта ещё не знает.
        unsafe { device.enable_bus_master() };

        let bar = device.memory_bar(0).ok_or(Atl1cError::BadBar)?;
        // SAFETY: условие делегировано вызывающему.
        let regs = unsafe { map_window(bar) }?;

        // Прерывания выключаются первым делом и навсегда: очередь опрашивает
        // задача, а поднятая линия INTx забрала бы вектор, которого никто не ждёт.
        // SAFETY: окно отображено.
        unsafe {
            write(regs, REG_IMR, 0);
            write(regs, REG_ISR, u32::MAX);
            write(regs, REG_WOL_CTRL, 0);
        }

        // SAFETY: см. выше.
        unsafe { stop_mac(regs) }?;
        // SAFETY: см. выше.
        unsafe { reset(regs, device.device) }?;
        // SAFETY: карта сброшена.
        unsafe { phy_up(regs) }?;

        // SAFETY: карта сброшена, PHY поднят.
        let mac = unsafe { hardware_address(regs) }?;

        let rfd = dma::alloc(RING * RFD_SIZE).map_err(Atl1cError::NoMemory)?;
        let rrd = dma::alloc(RING * RRD_SIZE).map_err(Atl1cError::NoMemory)?;
        let tpd = dma::alloc(RING * TPD_SIZE).map_err(Atl1cError::NoMemory)?;
        let rx_pool = dma::alloc(RING * BUFFER).map_err(Atl1cError::NoMemory)?;
        let tx_pool = dma::alloc(RING * BUFFER).map_err(Atl1cError::NoMemory)?;
        rfd.zero();
        rrd.zero();
        tpd.zero();

        let card = Self {
            regs,
            rfd,
            rrd,
            tpd,
            rx_pool,
            tx_pool,
            rx_next: 0,
            tx_next: 0,
            mac,
            stats: Stats::default(),
            device_id: device.device,
            address: device.address,
            speed: 0,
            link_countdown: 0,
        };

        // SAFETY: кольца выделены и обнулены, окно отображено.
        unsafe { card.configure() };
        Ok(card)
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

    /// Есть ли линия на проводе.
    #[must_use]
    pub fn link_up(&self) -> bool {
        // SAFETY: окно отображено всё время жизни карты.
        unsafe { link_speed(self.regs) }.is_some()
    }

    /// Разложить кольца и включить приём с передачей.
    ///
    /// # Safety
    ///
    /// Кольца выделены и обнулены, окно регистров отображено.
    unsafe fn configure(&self) {
        let regs = self.regs;

        // Аппаратный адрес приёмного фильтра. Раскладка не симметричная:
        // младшее слово несёт байты 2..5, старшее — 0..1.
        let low = u32::from_be_bytes([self.mac[2], self.mac[3], self.mac[4], self.mac[5]]);
        let high = u32::from(u16::from_be_bytes([self.mac[0], self.mac[1]]));
        // SAFETY: контракт функции.
        unsafe {
            write(regs, REG_MAC_STA_ADDR, low);
            write(regs, REG_MAC_STA_ADDR + 4, high);
        }

        // Приёмные буферы: адрес в кольцо свободных, по буферу на дескриптор.
        for index in 0..RING {
            let buffer = self.rx_pool.phys().as_u64() + (index * BUFFER) as u64;
            // SAFETY: дескриптор внутри выделенного кольца.
            unsafe { self.rfd_desc(index).cast::<u64>().write_volatile(buffer) };
        }

        // SAFETY: контракт функции; все адреса — из выделенных буферов DMA.
        unsafe {
            // Старшая половина адресов: у приёма и передачи она своя, но окно
            // DMA у нас одно, поэтому обе одинаковы.
            write(regs, REG_RX_BASE_ADDR_HI, (self.rfd.phys().as_u64() >> 32) as u32);
            write(regs, REG_TX_BASE_ADDR_HI, (self.tpd.phys().as_u64() >> 32) as u32);

            write(regs, REG_RFD0_HEAD_ADDR_LO, self.rfd.phys().as_u64() as u32);
            write(regs, REG_RFD_RING_SIZE, RING as u32);
            write(regs, REG_RX_BUF_SIZE, RX_BUF_SIZE);
            write(regs, REG_RRD0_HEAD_ADDR_LO, self.rrd.phys().as_u64() as u32);
            write(regs, REG_RRD_RING_SIZE, RING as u32);
            write(regs, REG_TPD_PRI0_ADDR_LO, self.tpd.phys().as_u64() as u32);
            write(regs, REG_TPD_RING_SIZE, RING as u32);
            // Карта перечитывает адреса колец только по этой команде.
            write(regs, REG_LOAD_PTR, 1);

            write(regs, REG_MTU, MTU_VALUE);
            write(regs, REG_SMB_STAT_TIMER, SMB_TIMER);
            write(regs, REG_TXF_WATER_MARK, 0);
            write(regs, REG_RXD_DMA_CTRL, 0);
            write(regs, REG_CLK_GATING_CTRL, 0);

            write(regs, REG_TXQ_CTRL, TXQ_CFGV | (L1C_TXQ_BURST << TXQ_BURST_SHIFT));
            write(regs, REG_RXQ_CTRL, RXQ_RFD_BURST << RXQ_RFD_BURST_SHIFT);
            write(
                regs,
                REG_DMA_CTRL,
                DMA_CTRL_RORDER_MODE_OUT
                    | DMA_CTRL_RREQ_PRI_DATA
                    | (DMA_CTRL_RREQ_BLEN_1024 << DMA_CTRL_RREQ_BLEN_SHIFT)
                    | (DMA_CTRL_RDLY_CNT_DEF << DMA_CTRL_RDLY_CNT_SHIFT)
                    | (DMA_CTRL_WDLY_CNT_DEF << DMA_CTRL_WDLY_CNT_SHIFT),
            );

            // Таймеры прерываний выключены, «чтение очищает» — тоже: события
            // мы не ждём, а счётчик статистики карте нужен.
            let master = read(regs, REG_MASTER_CTRL)
                & !(MASTER_CTRL_TX_ITIMER_EN | MASTER_CTRL_RX_ITIMER_EN | MASTER_CTRL_INT_RDCLR);
            write(regs, REG_MASTER_CTRL, master | MASTER_CTRL_SA_TIMER_EN);

            // Все приёмные буферы отданы карте.
            write(regs, REG_MB_RFD0_PROD_IDX, (RING - 1) as u32);

            // Очереди и MAC. Скорость пока предполагается стомегабитной: настоящую
            // скажет автосогласование, и `service` перепишет регистр (см.
            // `poll_link`). Ждать линию здесь было бы неправильно — провод могут
            // воткнуть и через час после загрузки.
            write(regs, REG_TXQ_CTRL, read(regs, REG_TXQ_CTRL) | TXQ_CTRL_EN);
            write(regs, REG_RXQ_CTRL, read(regs, REG_RXQ_CTRL) | RXQ_CTRL_EN);
            write(regs, REG_MAC_CTRL, mac_control(MAC_CTRL_SPEED_10_100, true));
        }
    }

    /// Спросить PHY о линии и, если скорость переменилась, перенастроить MAC.
    ///
    /// Зовётся из задачи-приёмника, но не на каждом обходе: обращение к PHY идёт
    /// через MDIO с опросом готовности, и делать это двести раз в секунду ради
    /// сведения, которое меняется раз в месяц, — расточительство.
    fn poll_link(&mut self) {
        if self.link_countdown > 0 {
            self.link_countdown -= 1;
            return;
        }
        // Раз в две секунды при обходе каждые 5 мс.
        self.link_countdown = 400;

        // SAFETY: окно отображено, карта работает.
        let found = unsafe { link_speed(self.regs) };
        let (speed, duplex) = match found {
            Some(state) => state,
            None => {
                if self.speed != 0 {
                    crate::kprintln!("  atl1c       : the link went down");
                    self.speed = 0;
                }
                return;
            }
        };
        if speed == self.speed {
            return;
        }
        self.speed = speed;
        let coded = if speed == 1000 { MAC_CTRL_SPEED_1000 } else { MAC_CTRL_SPEED_10_100 };
        // SAFETY: см. выше.
        unsafe { write(self.regs, REG_MAC_CTRL, mac_control(coded, duplex)) };
        crate::kprintln!(
            "  atl1c       : link up at {speed} Mbit/s, {} duplex",
            if duplex { "full" } else { "half" }
        );
    }

    /// Адрес дескриптора свободного приёмного буфера.
    fn rfd_desc(&self, index: usize) -> *mut u8 {
        // SAFETY: кольцо выделено на `RING * RFD_SIZE` байт, индекс меньше `RING`.
        unsafe { self.rfd.as_ptr::<u8>().add(index * RFD_SIZE) }
    }

    /// Адрес дескриптора-результата приёма.
    fn rrd_desc(&self, index: usize) -> *mut u8 {
        // SAFETY: см. `rfd_desc`.
        unsafe { self.rrd.as_ptr::<u8>().add(index * RRD_SIZE) }
    }

    /// Адрес дескриптора передачи.
    fn tpd_desc(&self, index: usize) -> *mut u8 {
        // SAFETY: см. `rfd_desc`.
        unsafe { self.tpd.as_ptr::<u8>().add(index * TPD_SIZE) }
    }

    /// Свободен ли дескриптор передачи с этим номером.
    ///
    /// У дескриптора передачи нет признака завершения — карта сообщает только
    /// свой счётчик. Свободно всё, что она уже прошла.
    fn tx_free(&self, index: usize) -> bool {
        // SAFETY: окно отображено.
        let hardware = unsafe { read16(self.regs, REG_TPD_PRI0_CIDX) } as usize % RING;
        // Кольцо пустое, когда счётчики совпали; занято — то, что между нашим и
        // её счётчиком. Один дескриптор всегда остаётся незанятым, иначе полное
        // кольцо неотличимо от пустого.
        let next = (index + 1) % RING;
        next != hardware
    }
}

impl Card for Atl1c {
    fn name(&self) -> &'static str {
        "atl1c"
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
        if !self.tx_free(index) {
            self.stats.tx_dropped += 1;
            return Err(CardError::Busy);
        }

        // SAFETY: буфер выделен на `RING * BUFFER` байт, длина проверена.
        unsafe {
            let buffer = self.tx_pool.as_ptr::<u8>().add(index * BUFFER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer, frame.len());
        }

        let phys = self.tx_pool.phys().as_u64() + (index * BUFFER) as u64;
        // SAFETY: дескриптор внутри кольца. Порядок записи важен: адрес и длина
        // раньше слова с признаком конца кадра.
        unsafe {
            let desc = self.tpd_desc(index);
            desc.cast::<u16>().write_volatile(frame.len() as u16);
            desc.add(2).cast::<u16>().write_volatile(0);
            desc.add(8).cast::<u64>().write_volatile(phys);
            desc.add(4).cast::<u32>().write_volatile(TPD_EOP);
        }

        self.tx_next = (index + 1) % RING;
        // SAFETY: окно отображено. Счётчик — шестнадцатибитный и считает
        // дескрипторы, а не байты.
        unsafe { write16(self.regs, REG_TPD_PRI0_PIDX, self.tx_next as u16) };

        self.stats.tx_frames += 1;
        self.stats.tx_bytes += frame.len() as u64;
        Ok(())
    }

    fn receive(&mut self, frame: &mut [u8; FRAME_MAX]) -> Option<usize> {
        self.poll_link();

        let index = self.rx_next;
        let desc = self.rrd_desc(index);
        // SAFETY: дескриптор внутри кольца.
        let word3 = unsafe { desc.add(12).cast::<u32>().read_volatile() };
        if word3 & RRS_RXD_UPDATED == 0 {
            return None;
        }
        // SAFETY: см. выше.
        let word0 = unsafe { desc.cast::<u32>().read_volatile() };

        // Номер буфера говорит сама карта: кольцо результатов и кольцо буферов
        // идут в ногу только пока нет ошибок, и доверять своему счётчику нельзя.
        let buffer_index = ((word0 >> RRS_RFD_INDEX_SHIFT) & RRS_RFD_INDEX_MASK) as usize % RING;
        // Длина включает контрольную сумму кадра — её карта не срезает.
        let length = (word3 & RRS_PKT_SIZE_MASK) as usize;
        let taken = if word3 & (RRS_RX_ERR_SUM | RRS_802_3_LEN_ERR) != 0 || length <= 4 {
            self.stats.rx_dropped += 1;
            None
        } else {
            let length = (length - 4).min(FRAME_MAX);
            // SAFETY: буфер выделен, длина не больше буфера.
            unsafe {
                let buffer = self.rx_pool.as_ptr::<u8>().add(buffer_index * BUFFER);
                core::ptr::copy_nonoverlapping(buffer, frame.as_mut_ptr(), length);
            }
            self.stats.rx_frames += 1;
            self.stats.rx_bytes += length as u64;
            Some(length)
        };

        // Результат разобран — освободить дескриптор и вернуть буфер карте.
        // SAFETY: дескриптор внутри кольца, окно отображено.
        unsafe {
            desc.add(12).cast::<u32>().write_volatile(0);
            write(self.regs, REG_MB_RFD0_PROD_IDX, buffer_index as u32);
        }
        self.rx_next = (index + 1) % RING;
        taken
    }

    fn stats(&self) -> Stats {
        self.stats
    }
}

/// Собрать значение регистра управления MAC для заданной скорости и дуплекса.
const fn mac_control(speed: u32, duplex: bool) -> u32 {
    let mut value = MAC_CTRL_TX_EN
        | MAC_CTRL_RX_EN
        | MAC_CTRL_TX_FLOW
        | MAC_CTRL_RX_FLOW
        | MAC_CTRL_ADD_CRC
        | MAC_CTRL_PAD
        | MAC_CTRL_BC_EN
        | MAC_CTRL_SINGLE_PAUSE_EN
        | MAC_CTRL_HASH_ALG_CRC32
        | MAC_CTRL_SPEED_MODE_SW
        | (speed << MAC_CTRL_SPEED_SHIFT)
        | (PREAMBLE_LEN << MAC_CTRL_PRMLEN_SHIFT);
    if duplex {
        value |= MAC_CTRL_DUPLX;
    }
    value
}

/// Остановить очереди и MAC, дождавшись, пока они замолчат.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn stop_mac(regs: VirtAddr) -> Result<(), Atl1cError> {
    // SAFETY: контракт функции.
    unsafe {
        let rxq = read(regs, REG_RXQ_CTRL);
        write(regs, REG_RXQ_CTRL, rxq & !RXQ_CTRL_EN);
        let txq = read(regs, REG_TXQ_CTRL);
        write(regs, REG_TXQ_CTRL, txq & !TXQ_CTRL_EN);
    }
    // SAFETY: см. выше.
    unsafe { wait_idle(regs, IDLE_STATUS_RXQ_BUSY | IDLE_STATUS_TXQ_BUSY) }?;

    // SAFETY: см. выше.
    unsafe {
        let mac = read(regs, REG_MAC_CTRL);
        write(regs, REG_MAC_CTRL, mac & !(MAC_CTRL_TX_EN | MAC_CTRL_RX_EN));
    }
    // SAFETY: см. выше.
    unsafe { wait_idle(regs, IDLE_STATUS_TXMAC_BUSY | IDLE_STATUS_RXMAC_BUSY) }
}

/// Дождаться, пока названные блоки карты перестанут работать.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn wait_idle(regs: VirtAddr, blocks: u32) -> Result<(), Atl1cError> {
    for _ in 0..IDLE_LIMIT {
        // SAFETY: контракт функции.
        if unsafe { read(regs, REG_IDLE_STATUS) } & blocks == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Atl1cError::NotIdle)
}

/// Сбросить карту.
///
/// # Safety
///
/// Окно регистров отображено, очереди остановлены.
unsafe fn reset(regs: VirtAddr, device_id: u16) -> Result<(), Atl1cError> {
    // SAFETY: контракт функции.
    let base = unsafe { read(regs, REG_MASTER_CTRL) } | MASTER_CTRL_OOB_DIS;
    // SAFETY: см. выше.
    unsafe { write(regs, REG_MASTER_CTRL, base | MASTER_CTRL_SOFT_RST) };

    // Сброс идёт заметное время; его конец виден по замолчавшим блокам.
    // SAFETY: см. выше.
    unsafe { wait_idle(regs, IDLE_STATUS_MASK) }.map_err(|_| Atl1cError::ResetTimeout)?;
    // SAFETY: см. выше.
    unsafe {
        write(regs, REG_MASTER_CTRL, base);

        // Скорость MAC задаётся программно, а не выводится из состояния линии:
        // иначе он менял бы режим под нами в тот момент, когда мы его читаем.
        let mac = read(regs, REG_MAC_CTRL);
        write(regs, REG_MAC_CTRL, mac | MAC_CTRL_SPEED_MODE_SW);

        // У ревизии 2.0 такты PHY и MAC полагается замедлять — так поступает и
        // родной драйвер. На ревизии 1.0 биты не трогаем.
        if device_id == 0x1083 {
            let serdes = read(regs, REG_SERDES);
            write(regs, REG_SERDES, serdes | SERDES_PHY_CLK_SLOWDOWN | SERDES_MAC_CLK_SLOWDOWN);
        }
    }
    Ok(())
}

/// Поднять приёмопередатчик: снять сброс, задать настройки аналоговой части,
/// выключить энергосбережение и запустить автосогласование.
///
/// # Safety
///
/// Карта сброшена, окно регистров отображено.
unsafe fn phy_up(regs: VirtAddr) -> Result<(), Atl1cError> {
    // SAFETY: контракт функции.
    let mut ctrl = unsafe { read(regs, REG_GPHY_CTRL) };
    ctrl &= !(GPHY_CTRL_EXT_RESET | GPHY_CTRL_PHY_IDDQ | GPHY_CTRL_GATE_25M_EN | GPHY_CTRL_PWDOWN_HW);
    ctrl |= GPHY_CTRL_SEL_ANA_RST | GPHY_CTRL_HIB_EN | GPHY_CTRL_HIB_PULSE;
    // SAFETY: см. выше.
    unsafe {
        write(regs, REG_GPHY_CTRL, ctrl);
        spin(SHORT_SPIN);
        write(regs, REG_GPHY_CTRL, ctrl | GPHY_CTRL_EXT_RESET);
        // Внешний сброс держится восемьсот микросекунд — столько же, сколько у
        // родного драйвера; короче, и PHY не успевает подняться.
        spin(SHORT_SPIN * u32::from(GPHY_CTRL_EXT_RST_TO));
    }

    // Настройки аналоговой части. Числа не выводятся ни из чего — это значения
    // изготовителя для этой ревизии, и без них линия либо не поднимается, либо
    // поднимается с ошибками на длинном кабеле.
    // SAFETY: карта сброшена, PHY вышел из сброса.
    unsafe {
        write_phy_dbg(regs, MIIDBG_LEGCYPS, L1D_LEGCYPS_DEF)?;
        write_phy_dbg(regs, MIIDBG_SYSMODCTRL, L1D_SYSMODCTRL_IECHOADJ_DEF)?;
        write_phy_dbg(regs, MIIDBG_ANACTRL, ANACTRL_DEF)?;
        write_phy_dbg(regs, MIIDBG_SRDSYSMOD, SRDSYSMOD_DEF)?;
        write_phy_dbg(regs, MIIDBG_TST10BTCFG, TST10BTCFG_DEF)?;
        write_phy_dbg(regs, MIIDBG_TST100BTCFG, TST100BTCFG_DEF)?;
    }

    // Energy Efficient Ethernet выключается целиком. Он экономит доли ватта и
    // стоит того, чтобы его иметь, — но пока ядро не умеет просыпаться по линии,
    // засыпающая линия выглядит как пропадающая сеть.
    // SAFETY: см. выше.
    unsafe {
        let lpi = read(regs, REG_LPI_CTRL);
        write(regs, REG_LPI_CTRL, lpi & !LPI_CTRL_EN);
        let _ = write_phy_ext(regs, MIIEXT_ANEG, MIIEXT_LOCAL_EEEADV, 0);
        let _ = write_phy_ext(regs, MIIEXT_PCS, MIIEXT_CLDCTRL3, L2CB_CLDCTRL3);
    }

    // Проверка, что PHY вообще отвечает: его идентификатор обязан читаться.
    // SAFETY: см. выше.
    let id = unsafe { read_phy(regs, MII_PHYSID1) }?;
    if id == 0xFFFF {
        return Err(Atl1cError::PhyTimeout);
    }

    // Объявляем всё, что умеем, и просим договориться заново.
    // SAFETY: см. выше.
    unsafe {
        write_phy(regs, MII_ADVERTISE, ADVERTISE_ALL)?;
        write_phy(regs, MII_CTRL1000, ADVERTISE_1000)?;
        write_phy(regs, MII_IER, IER_LINK_UP | IER_LINK_DOWN)?;
        write_phy(regs, MII_BMCR, BMCR_RESET | BMCR_ANENABLE | BMCR_ANRESTART)?;
    }
    Ok(())
}

/// Договорённые скорость и дуплекс; `None` — линии нет.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn link_speed(regs: VirtAddr) -> Option<(u16, bool)> {
    // SAFETY: контракт функции.
    let status = unsafe { read_phy(regs, MII_GIGA_PSSR) }.ok()?;
    if status & GIGA_PSSR_SPD_DPLX_RESOLVED == 0 {
        return None;
    }
    let speed = match status & GIGA_PSSR_SPEED {
        GIGA_PSSR_1000MBS => 1000,
        GIGA_PSSR_100MBS => 100,
        _ => 10,
    };
    Some((speed, status & GIGA_PSSR_DPLX != 0))
}

/// Прочитать аппаратный адрес карты.
///
/// Берётся из приёмного фильтра: его заполняет прошивка при включении машины, и
/// на ноутбуке он там есть всегда. Чтения EEPROM через TWSI здесь нет намеренно
/// — это отдельная последовательность с ожиданиями, которую нечем проверить, а
/// адрес без неё известен.
///
/// # Safety
///
/// Окно регистров отображено, карта сброшена.
unsafe fn hardware_address(regs: VirtAddr) -> Result<[u8; 6], Atl1cError> {
    // SAFETY: контракт функции.
    let (low, high) = unsafe { (read(regs, REG_MAC_STA_ADDR), read(regs, REG_MAC_STA_ADDR + 4)) };
    let low = low.to_be_bytes();
    let high = (high as u16).to_be_bytes();
    let mac = [high[0], high[1], low[0], low[1], low[2], low[3]];
    // Ноль и «все единицы» — не адреса, а пустой регистр.
    if mac == [0; 6] || mac == [0xFF; 6] {
        return Err(Atl1cError::NoMac);
    }
    Ok(mac)
}

// ---------------------------------------------------------------------------
// MDIO — разговор с приёмопередатчиком
// ---------------------------------------------------------------------------

/// Прочитать регистр PHY.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn read_phy(regs: VirtAddr, reg: u8) -> Result<u16, Atl1cError> {
    let command = MDIO_CTRL_START
        | MDIO_CTRL_OP_READ
        | (u32::from(reg & 0x1F) << MDIO_CTRL_REG_SHIFT)
        | (MDIO_CTRL_CLK_25_4 << MDIO_CTRL_CLK_SEL_SHIFT);
    // SAFETY: контракт функции.
    unsafe { write(regs, REG_MDIO_CTRL, command) };
    // SAFETY: см. выше.
    let value = unsafe { wait_mdio(regs) }?;
    Ok((value & MDIO_CTRL_DATA_MASK) as u16)
}

/// Записать регистр PHY.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn write_phy(regs: VirtAddr, reg: u8, data: u16) -> Result<(), Atl1cError> {
    let command = MDIO_CTRL_START
        | (u32::from(reg & 0x1F) << MDIO_CTRL_REG_SHIFT)
        | u32::from(data)
        | (MDIO_CTRL_CLK_25_4 << MDIO_CTRL_CLK_SEL_SHIFT);
    // SAFETY: контракт функции.
    unsafe { write(regs, REG_MDIO_CTRL, command) };
    // SAFETY: см. выше.
    unsafe { wait_mdio(regs) }.map(|_| ())
}

/// Записать регистр отладочного порта PHY.
///
/// Порт устроен как пара регистров: в один кладётся номер, в другой — значение.
/// Через него настраивается аналоговая часть, которой в обычном наборе MII места
/// не предусмотрено.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn write_phy_dbg(regs: VirtAddr, reg: u16, data: u16) -> Result<(), Atl1cError> {
    // SAFETY: контракт функции.
    unsafe {
        write_phy(regs, MII_DBG_ADDR, reg)?;
        write_phy(regs, MII_DBG_DATA, data)
    }
}

/// Записать расширенный регистр PHY (адресация по устройству и номеру).
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn write_phy_ext(regs: VirtAddr, device: u16, reg: u16, data: u16) -> Result<(), Atl1cError> {
    let extn = u32::from(reg) | (u32::from(device & 0x1F) << MDIO_EXTN_DEVAD_SHIFT) | (0 << MDIO_EXTN_PORTAD_SHIFT);
    let command = MDIO_CTRL_START | MDIO_CTRL_MODE_EXT | u32::from(data) | (MDIO_CTRL_CLK_25_4 << MDIO_CTRL_CLK_SEL_SHIFT);
    // SAFETY: контракт функции.
    unsafe {
        write(regs, REG_MDIO_EXTN, extn);
        write(regs, REG_MDIO_CTRL, command);
    }
    // SAFETY: см. выше.
    unsafe { wait_mdio(regs) }.map(|_| ())
}

/// Дождаться конца обмена по MDIO и вернуть регистр целиком.
///
/// # Safety
///
/// Окно регистров отображено.
unsafe fn wait_mdio(regs: VirtAddr) -> Result<u32, Atl1cError> {
    for _ in 0..POLL_LIMIT {
        // SAFETY: контракт функции.
        let value = unsafe { read(regs, REG_MDIO_CTRL) };
        if value & MDIO_CTRL_BUSY == 0 {
            return Ok(value);
        }
        core::hint::spin_loop();
    }
    Err(Atl1cError::PhyTimeout)
}

// ---------------------------------------------------------------------------
// Мелочи
// ---------------------------------------------------------------------------

/// Подождать примерно `turns` оборотов. Часов на этой стадии может не быть.
fn spin(turns: u32) {
    for _ in 0..turns {
        core::hint::spin_loop();
    }
}

/// Поднимает ли этот драйвер такую карту.
#[must_use]
pub fn supports(vendor: u16, device: u16) -> bool {
    vendor == VENDOR_ATHEROS && SUPPORTED.iter().any(|(id, _)| *id == device)
}

/// Имя модели для журнала.
fn model_name(device: u16) -> &'static str {
    SUPPORTED.iter().find(|(id, _)| *id == device).map_or("AR8151", |(_, name)| name)
}

/// Отобразить окно регистров карты.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_window(phys: PhysAddr) -> Result<VirtAddr, Atl1cError> {
    // Нужны первые восемь килобайт: последний регистр, к которому мы
    // обращаемся, — управление тактами по смещению 0x1814.
    let span = (REG_CLK_GATING_CTRL + PAGE_SIZE).next_multiple_of(PAGE_SIZE);
    let virt = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
    // SAFETY: условие делегировано вызывающему; это регистры устройства, и
    // семантика `DEVICE` для них обязательна.
    unsafe { crate::arch::map_active(virt, phys, span, flags) }.map_err(|_| Atl1cError::MapFailed)?;
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

/// Прочитать шестнадцатибитный регистр — такими у этой карты сделаны счётчики
/// дескрипторов, и читать их словом нельзя: рядом лежит соседний счётчик.
///
/// # Safety
///
/// Окно отображено, смещение внутри него и выровнено на два байта.
unsafe fn read16(regs: VirtAddr, offset: usize) -> u16 {
    // SAFETY: контракт функции.
    unsafe { ((regs.as_usize() + offset) as *const u16).read_volatile() }
}

/// Записать шестнадцатибитный регистр.
///
/// # Safety
///
/// Окно отображено, смещение внутри него и выровнено на два байта.
unsafe fn write16(regs: VirtAddr, offset: usize, value: u16) {
    // SAFETY: контракт функции.
    unsafe { ((regs.as_usize() + offset) as *mut u16).write_volatile(value) };
}

// Перечисление карт, которых ядро не умеет, живёт в [`super::e1000::undriven`]:
// оно спрашивает шину, а не драйвер, и второй такой же функции здесь быть не
// должно.
