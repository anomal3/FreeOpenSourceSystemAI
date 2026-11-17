//! Realtek RTL8125 (2.5 Гбит/с) — встроенная сеть большинства плат на AM5 и
//! Intel 600/700 (15.11: MSI PRO B840-P Романа, `10ec:8125`).
//!
//! # Откуда устройство карты
//!
//! Только открытое: FreeBSD `rge(4)` (BSD-2, `sys/dev/rge/`) — по нему
//! раскладка регистров и порядок запуска; Linux `r8169` — сверка поведения
//! железа (код не переносится). Разбор с пометками «не уверен» — в рабочей
//! спецификации (ROADMAP «Сеть на настоящем ПК»).
//!
//! # Что здесь, а чего нет (фаза 1)
//!
//! Опрос, а не прерывания; дескрипторы старого 16-байтного вида (бит 0
//! регистра MAC OCP `0xEB58` снят — так делает Linux); по 64 дескриптора на
//! приём и передачу; без прошивки PHY (для связи она не нужна, Linux без неё
//! работает) и без подстройки PHY. Модель чипа (`TxConfig`), адрес и состояние
//! линии печатаются в журнал — первая загрузка на плате скажет, что там.

use alloc::vec::Vec;

use super::card::{Card, CardError, FRAME_MAX, Stats};
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{PageFlags, PhysAddr, VirtAddr};
use crate::pci::{self, Device};

const VENDOR_REALTEK: u16 = 0x10ec;
/// RTL8125/B/D и их варианты Killer E3000/E5000.
const SUPPORTED: &[u16] = &[0x8125, 0x3000, 0x5000];

const MAC0: usize = 0x00;
const MAC4: usize = 0x04;
const MAR0: usize = 0x08;
const MAR4: usize = 0x0c;
const TX_DESC_LO: usize = 0x20;
const TX_DESC_HI: usize = 0x24;
const INT_CFG0: usize = 0x34;
const CHIP_CMD: usize = 0x37;
const INTR_MASK: usize = 0x38;
const INTR_STATUS: usize = 0x3c;
const TX_CONFIG: usize = 0x40;
const RX_CONFIG: usize = 0x44;
const CFG9346: usize = 0x50;
const CONFIG1: usize = 0x52;
const CONFIG2: usize = 0x53;
const CONFIG3: usize = 0x54;
const CONFIG5: usize = 0x56;
const PHY_STATUS: usize = 0x6c;
const PMCH: usize = 0x6f;
const INT_CFG1: usize = 0x7a;
const TX_POLL: usize = 0x90;
const MAC_OCP: usize = 0xb0;
const PHY_OCP: usize = 0xb8;
const MCU: usize = 0xd3;
const RX_MAX_SIZE: usize = 0xda;
const CPLUS_CMD: usize = 0xe0;
const INTR_MITIGATE: usize = 0xe2;
const RX_DESC_LO: usize = 0xe4;
const RX_DESC_HI: usize = 0xe8;
const MISC: usize = 0xf0;
const MAC_BACKUP: usize = 0x19e0;

const CMD_RESET: u8 = 0x10;
const CMD_RX_ENABLE: u8 = 0x08;
const CMD_TX_ENABLE: u8 = 0x04;
const CMD_STOP_REQ: u8 = 0x80;
const MISC_RXDV_GATE: u32 = 1 << 19;
const MCU_NOW_IS_OOB: u8 = 0x80;
const MCU_FIFOS_EMPTY: u8 = 0x30;
const MCU_LINK_LIST_READY: u8 = 0x02;

const PHY_LINK: u16 = 0x0002;
const PHY_FULL_DUPLEX: u16 = 0x0001;

/// Принимать: широковещательные, свои и (раз таблица открыта) групповые.
const RX_ACCEPT: u32 = 0x08 | 0x02 | 0x04;
const TX_CONFIG_VALUE: u32 = 0x0300_0700;

const DESC_OWN: u32 = 1 << 31;
const DESC_EOR: u32 = 1 << 30;
const DESC_FS: u32 = 1 << 29;
const DESC_LS: u32 = 1 << 28;
const RX_RES: u32 = 1 << 21;

const RING: usize = 64;
const DESC_SIZE: usize = 16;
const BUFFER: usize = 2048;
/// Наибольший принимаемый кадр с контрольной суммой и меткой VLAN.
const RX_MAX: u16 = 1536;
/// Окно регистров: всё, что здесь трогается, лежит ниже `0x7000`.
const WINDOW: usize = 0x7000;

#[derive(Debug)]
pub enum Rtl8125Error {
    NoCard,
    BadBar,
    MapFailed,
    /// Карта не отвечает (`TxConfig` = все единицы).
    Dead,
    /// Модель, которой этот драйвер не знает.
    Unknown(u32),
    ResetTimeout,
    NoMemory(DmaError),
}

impl core::fmt::Display for Rtl8125Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoCard => f.write_str("no RTL8125 on the bus"),
            Self::BadBar => f.write_str("no memory BAR"),
            Self::MapFailed => f.write_str("cannot map the register window"),
            Self::Dead => f.write_str("the card answers all ones"),
            Self::Unknown(tx) => write!(f, "unknown chip (TxConfig {tx:#010x})"),
            Self::ResetTimeout => f.write_str("the card did not come out of reset"),
            Self::NoMemory(err) => write!(f, "cannot allocate the rings: {err}"),
        }
    }
}

/// Окно регистров.
#[derive(Clone, Copy)]
struct Regs(usize);

impl Regs {
    fn r8(self, at: usize) -> u8 {
        // SAFETY: смещение внутри отображённого окна `WINDOW`.
        unsafe { ((self.0 + at) as *const u8).read_volatile() }
    }
    fn r16(self, at: usize) -> u16 {
        // SAFETY: см. `r8`; смещение выровнено.
        unsafe { ((self.0 + at) as *const u16).read_volatile() }
    }
    fn r32(self, at: usize) -> u32 {
        // SAFETY: см. `r8`.
        unsafe { ((self.0 + at) as *const u32).read_volatile() }
    }
    fn w8(self, at: usize, v: u8) {
        // SAFETY: см. `r8`.
        unsafe { ((self.0 + at) as *mut u8).write_volatile(v) }
    }
    fn w16(self, at: usize, v: u16) {
        // SAFETY: см. `r8`.
        unsafe { ((self.0 + at) as *mut u16).write_volatile(v) }
    }
    fn w32(self, at: usize, v: u32) {
        // SAFETY: см. `r8`.
        unsafe { ((self.0 + at) as *mut u32).write_volatile(v) }
    }
    /// Дождаться условия не дольше `us` микросекунд.
    fn wait(self, us: u64, ready: impl Fn(Self) -> bool) -> bool {
        let until = crate::time::uptime_ns().saturating_add(us * 1000);
        // Предел и в оборотах: часы на ранней загрузке могут ещё стоять.
        for _ in 0..50_000_000u32 {
            if ready(self) {
                return true;
            }
            if crate::time::uptime_ns() >= until {
                break;
            }
            core::hint::spin_loop();
        }
        ready(self)
    }

    /// Регистр микроконтроллера MAC (OCP).
    fn mac_read(self, reg: u16) -> u16 {
        self.w32(MAC_OCP, u32::from(reg >> 1) << 16);
        self.r32(MAC_OCP) as u16
    }
    fn mac_write(self, reg: u16, v: u16) {
        self.w32(MAC_OCP, 0x8000_0000 | (u32::from(reg >> 1) << 16) | u32::from(v));
    }
    fn mac_modify(self, reg: u16, clear: u16, set: u16) {
        let v = self.mac_read(reg);
        self.mac_write(reg, (v & !clear) | set);
    }

    /// Регистр PHY через OCP (стандартный MII `n` — по адресу `0xA400 + 2n`).
    fn phy_read(self, reg: u16) -> u16 {
        self.w32(PHY_OCP, u32::from(reg >> 1) << 16);
        let _ = self.wait(1000, |r| r.r32(PHY_OCP) & 0x8000_0000 != 0);
        self.r32(PHY_OCP) as u16
    }
    fn phy_write(self, reg: u16, v: u16) {
        self.w32(PHY_OCP, 0x8000_0000 | (u32::from(reg >> 1) << 16) | u32::from(v));
        let _ = self.wait(1000, |r| r.r32(PHY_OCP) & 0x8000_0000 == 0);
    }
    fn phy_modify(self, reg: u16, clear: u16, set: u16) {
        let v = self.phy_read(reg);
        self.phy_write(reg, (v & !clear) | set);
    }
}

fn delay_us(us: u64) {
    let until = crate::time::uptime_ns().saturating_add(us * 1000);
    for _ in 0..50_000_000u32 {
        if crate::time::uptime_ns() >= until {
            return;
        }
        core::hint::spin_loop();
    }
}

pub struct Rtl8125 {
    regs: Regs,
    rx_ring: DmaBuffer,
    tx_ring: DmaBuffer,
    rx_pool: DmaBuffer,
    tx_pool: DmaBuffer,
    rx_next: usize,
    tx_next: usize,
    /// Переполнение приёма уже сказано в журнал.
    overflow_seen: bool,
    mac: [u8; 6],
    stats: Stats,
    device_id: u16,
    address: pci::Address,
}

// SAFETY: как у `e1000`: только свои буферы DMA и окно регистров, под замком
// интерфейса.
unsafe impl Send for Rtl8125 {}

impl Rtl8125 {
    /// Найти карту и запустить её.
    ///
    /// # Safety
    ///
    /// Ядро на своих таблицах страниц, `root` — живая шина PCI.
    pub unsafe fn probe(root: &pci::Root) -> Result<Self, Rtl8125Error> {
        let mut found: Option<Device> = None;
        // SAFETY: контракт функции.
        unsafe {
            pci::for_each(root, |device| {
                if device.vendor == VENDOR_REALTEK && SUPPORTED.contains(&device.device) {
                    found = Some(*device);
                    return false;
                }
                true
            });
        }
        let device = found.ok_or(Rtl8125Error::NoCard)?;
        let bar = device
            .memory_bar(2)
            .or_else(|| device.memory_bar(0))
            .or_else(|| device.memory_bar(4))
            .ok_or(Rtl8125Error::BadBar)?;
        // SAFETY: адресов колец карта ещё не знает.
        unsafe { device.enable_bus_master() };
        // SAFETY: контракт функции.
        let regs = unsafe { map_window(bar) }?;

        let tx_config = regs.r32(TX_CONFIG);
        if tx_config == u32::MAX {
            return Err(Rtl8125Error::Dead);
        }
        let xid = (tx_config >> 20) & 0x7cf;
        let chip = match xid {
            0x609 => "RTL8125A",
            0x641 => "RTL8125B",
            0x688 | 0x689 => "RTL8125D",
            0x68a => "RTL8125K",
            0x681 => "RTL8125BP",
            0x708 => "RTL8125CP",
            _ => return Err(Rtl8125Error::Unknown(tx_config)),
        };
        let later_than_a = xid != 0x609;
        kprintln!("  rtl8125     : {} at {} (TxConfig {tx_config:#010x}, xid {xid:#x})", chip, device.address);

        // Прерывания карты не нужны (опрос) — закрыть и снять накопленное.
        regs.w32(INTR_MASK, 0);
        regs.w32(INTR_STATUS, u32::MAX);
        let _ = regs.r8(CHIP_CMD);

        // ASPM и CLKREQ на шине выключаются: на RTL8125 с ними ловят зависания
        // (Linux тоже выключает L1). Регистр Link Control — младшие 16 бит
        // двойного слова; старшие (Link Status) пишутся нулями — там биты
        // «записать 1, чтобы снять».
        // SAFETY: конфигурационное пространство этой карты.
        unsafe { pcie_aspm_off(&device) };

        hw_init(regs, later_than_a)?;
        phy_power_up(regs);
        let mac = read_mac(regs);

        let rx_ring = dma::alloc(RING * DESC_SIZE).map_err(Rtl8125Error::NoMemory)?;
        let tx_ring = dma::alloc(RING * DESC_SIZE).map_err(Rtl8125Error::NoMemory)?;
        let rx_pool = dma::alloc(RING * BUFFER).map_err(Rtl8125Error::NoMemory)?;
        let tx_pool = dma::alloc(RING * BUFFER).map_err(Rtl8125Error::NoMemory)?;
        rx_ring.zero();
        tx_ring.zero();

        let card = Self {
            regs,
            rx_ring,
            tx_ring,
            rx_pool,
            tx_pool,
            rx_next: 0,
            tx_next: 0,
            overflow_seen: false,
            mac,
            stats: Stats::default(),
            device_id: device.device,
            address: device.address,
        };
        card.fill_rx();
        // SAFETY: конфигурационное пространство этой карты.
        unsafe { card.hw_start(&device, later_than_a) };
        phy_autoneg(regs);
        let status = regs.r16(PHY_STATUS);
        kprintln!(
            "  rtl8125     : started, address {}, PHY status {status:#06x} ({})",
            super::eth::Display(mac),
            describe_phy(status).join(", ")
        );
        Ok(card)
    }

    #[must_use]
    pub const fn device_id(&self) -> u16 {
        self.device_id
    }

    #[must_use]
    pub const fn address(&self) -> pci::Address {
        self.address
    }

    /// Есть ли связь на проводе.
    #[must_use]
    pub fn link_up(&self) -> bool {
        self.regs.r16(PHY_STATUS) & PHY_LINK != 0
    }

    fn desc(ring: &DmaBuffer, index: usize) -> *mut u32 {
        // SAFETY: кольцо выделено на `RING * DESC_SIZE` байт, индекс меньше `RING`.
        unsafe { ring.as_ptr::<u8>().add(index * DESC_SIZE).cast::<u32>() }
    }

    /// Отдать дескриптор приёма карте.
    fn give_rx(&self, index: usize) {
        let desc = Self::desc(&self.rx_ring, index);
        let phys = self.rx_pool.phys().as_u64() + (index * BUFFER) as u64;
        let eor = if index == RING - 1 { DESC_EOR } else { 0 };
        // SAFETY: дескриптор внутри кольца; признак владения пишется последним.
        unsafe {
            desc.add(2).cast::<u64>().write_volatile(phys);
            desc.add(1).write_volatile(0);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            desc.write_volatile(DESC_OWN | eor | BUFFER as u32);
        }
    }

    fn fill_rx(&self) {
        for index in 0..RING {
            self.give_rx(index);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }

    /// Запуск: `rtl_hw_start_8125` + общая часть, по Linux и rge.
    ///
    /// # Safety
    ///
    /// `device` — эта карта.
    unsafe fn hw_start(&self, device: &Device, later_than_a: bool) {
        let r = self.regs;
        r.w8(CFG9346, 0xc0);
        // ASPM и CLKREQ внутри чипа.
        r.w8(CONFIG2, r.r8(CONFIG2) & !0x80);
        r.w8(CONFIG5, r.r8(CONFIG5) & !0x01);
        r.mac_modify(0xe092, 0x00ff, 0);
        // Без разгрузки контрольных сумм и VLAN.
        r.w16(CPLUS_CMD, r.r16(CPLUS_CMD) & !0x0060);
        r.w16(0x6048, 1500 + 14 + 32);
        r.w8(INT_CFG0, 0);
        let mitigation_end = if later_than_a { 0x0a80 } else { 0x0b00 };
        for at in (0x0a00..mitigation_end).step_by(4) {
            r.w32(at, 0);
        }
        if later_than_a {
            r.w16(INT_CFG1, 0);
        }
        // Задержка входа в ASPM (CSI 0x70F) — как у обоих драйверов.
        // SAFETY: контракт функции.
        unsafe { device.write_config8(0x70f, 0x27) };

        // `rtl_hw_start_8125_common`.
        r.w8(CONFIG3, r.r8(CONFIG3) & !0x02);
        r.w16(0x382, 0x221b);
        r.w32(0x4500, 0);
        r.w16(0x4800, 0);
        r.mac_modify(0xd40a, 0x0010, 0);
        r.w8(CONFIG1, r.r8(CONFIG1) & !0x10);
        r.mac_write(0xc140, 0xffff);
        r.mac_write(0xc142, 0xffff);
        r.mac_modify(0xd3e2, 0x0fff, 0x03a9);
        r.mac_modify(0xd3e4, 0x00ff, 0);
        r.mac_modify(0xe860, 0, 0x0080);
        // Старый (16-байтный) вид дескрипторов передачи.
        r.mac_modify(0xeb58, 0x0001, 0);
        if later_than_a {
            r.mac_modify(0xe614, 0x0700, 0x0200);
            r.mac_modify(0xe63e, 0x0c30, 0);
        } else {
            r.mac_modify(0xe614, 0x0700, 0x0300);
            r.mac_modify(0xe63e, 0x0c30, 0x0020);
        }
        r.mac_modify(0xc0b4, 0, 0x000c);
        r.mac_modify(0xeb6a, 0x00ff, 0x0033);
        r.mac_modify(0xeb50, 0x03e0, 0x0040);
        r.mac_modify(0xe056, 0x00f0, 0);
        r.mac_modify(0xe040, 0x1000, 0);
        r.mac_modify(0xea1c, 0x0003, 0x0001);
        r.mac_modify(0xea1c, 0x0004, 0);
        r.mac_modify(0xe0c0, 0x4f0f, 0x4403);
        r.mac_modify(0xe052, 0x0080, 0x0068);
        r.mac_modify(0xd430, 0x0fff, 0x047f);
        r.mac_modify(0xeb54, 0, 0x0001);
        delay_us(2);
        r.mac_modify(0xeb54, 0x0001, 0);
        r.w16(0x1880, r.r16(0x1880) & !0x0030);
        r.mac_write(0xe098, 0xc302);
        let _ = r.wait(10_000, |r| r.mac_read(0xe00e) & 0x2000 == 0);
        // Ворота RXDV открываются: без этого карта не принимает ни кадра.
        r.w32(MISC, r.r32(MISC) & !MISC_RXDV_GATE);

        // Выход из L1 по событиям.
        r.mac_modify(0xc0ac, 0, 0x1f80);
        r.w16(RX_MAX_SIZE, RX_MAX);

        let tx = self.tx_ring.phys().as_u64();
        let rx = self.rx_ring.phys().as_u64();
        r.w32(TX_DESC_HI, (tx >> 32) as u32);
        r.w32(TX_DESC_LO, tx as u32);
        r.w32(RX_DESC_HI, (rx >> 32) as u32);
        r.w32(RX_DESC_LO, rx as u32);

        // Адрес — в фильтр (пишется только при открытых регистрах настройки).
        r.w32(MAC4, u32::from(self.mac[4]) | (u32::from(self.mac[5]) << 8));
        let _ = r.r8(CHIP_CMD);
        r.w32(MAC0, u32::from_le_bytes([self.mac[0], self.mac[1], self.mac[2], self.mac[3]]));
        let _ = r.r8(CHIP_CMD);
        r.w8(CFG9346, 0);
        let _ = r.r8(CHIP_CMD);

        r.w8(CHIP_CMD, CMD_TX_ENABLE | CMD_RX_ENABLE);
        let rx_config = if later_than_a { 0x4000_0f00 } else { 0x4000_0700 };
        r.w32(RX_CONFIG, rx_config | RX_ACCEPT);
        r.w32(TX_CONFIG, TX_CONFIG_VALUE);
        r.w32(MAR0, u32::MAX);
        r.w32(MAR4, u32::MAX);
        let _ = r.r8(CHIP_CMD);
    }
}

/// Предсброс и сброс (`rtl_hw_init_8125` + `rtl_hw_reset`).
fn hw_init(r: Regs, later_than_a: bool) -> Result<(), Rtl8125Error> {
    // Ворота RXDV закрыть и дождаться пустых очередей.
    r.w32(MISC, r.r32(MISC) | MISC_RXDV_GATE);
    delay_us(2000);
    if later_than_a {
        r.w8(CHIP_CMD, r.r8(CHIP_CMD) | CMD_STOP_REQ);
    }
    let _ = r.wait(5000, |r| r.r8(MCU) & MCU_FIFOS_EMPTY == MCU_FIFOS_EMPTY);
    if later_than_a {
        let _ = r.wait(5000, |r| r.r16(INTR_MITIGATE) & 0x0103 == 0x0103);
    }
    r.w8(CHIP_CMD, r.r8(CHIP_CMD) & !(CMD_TX_ENABLE | CMD_RX_ENABLE));
    delay_us(1000);

    // Выход из OOB: прошивка платы (или Windows) могла оставить карту
    // во «внешнем управлении» (Wake-on-LAN, DASH).
    r.mac_write(0xc0bc, 0x00ff);
    r.w8(MCU, r.r8(MCU) & !MCU_NOW_IS_OOB);
    r.mac_modify(0xe8de, 0x4000, 0);
    let _ = r.wait(4000, |r| r.r8(MCU) & MCU_LINK_LIST_READY != 0);
    r.mac_write(0xc0aa, 0x07d0);
    r.mac_write(0xc0a6, 0x0150);
    r.mac_write(0xc01e, 0x5555);
    let _ = r.wait(4000, |r| r.r8(MCU) & MCU_LINK_LIST_READY != 0);
    if r.mac_read(0xd42c) & 0x0100 != 0 {
        let _ = r.wait(100_000, |r| r.phy_read(0xa420) & 7 == 2);
        r.mac_modify(0xd42c, 0x0100, 0);
        r.phy_modify(0xa468, 0x000a, 0);
        if later_than_a {
            r.phy_modify(0xa466, 0x0001, 0);
        }
    }
    r.mac_modify(0xd40a, 0x0010, 0);

    r.w8(CHIP_CMD, CMD_RESET);
    if !r.wait(100_000, |r| r.r8(CHIP_CMD) & CMD_RESET == 0) {
        return Err(Rtl8125Error::ResetTimeout);
    }
    Ok(())
}

/// Разбудить PHY: после Windows или горячей перезагрузки он мог спать.
fn phy_power_up(r: Regs) {
    r.w8(PMCH, r.r8(PMCH) | 0xc0);
    r.phy_write(0xa400, 0x1000);
    if !r.wait(150_000, |r| r.phy_read(0xa420) & 7 == 3) {
        kprintln!("  rtl8125     : PHY did not report 'LAN on' (state {})", r.phy_read(0xa420) & 7);
    }
}

/// Объявить 10/100/1000/2500 и перезапустить согласование. EEE выключен.
fn phy_autoneg(r: Regs) {
    // EEE (энергосбережение на линии) — выключить, как делает rge.
    r.mac_modify(0xe040, 0x0003, 0);
    r.phy_modify(0xa5d0, 0x0006, 0);
    r.phy_modify(0xa6d4, 0x0001, 0);
    r.phy_modify(0xa428, 0x0080, 0);
    // «Gigabit Lite» — выключить.
    r.phy_modify(0xa428, 0x0200, 0);
    r.phy_modify(0xa5ea, 0x0001, 0);
    r.phy_write(0xa408, 0x0de1);
    r.phy_write(0xa412, 0x0200);
    r.phy_modify(0xa5d4, 0, 0x0080);
    r.phy_write(0xa400, 0x9200);
    let _ = r.wait(500_000, |r| r.phy_read(0xa400) & 0x8000 == 0);
}

/// Адрес карты: копия на заводе (`0x19E0`), иначе текущий фильтр.
fn read_mac(r: Regs) -> [u8; 6] {
    let pick = |at: usize| {
        let lo = r.r32(at).to_le_bytes();
        let hi = r.r16(at + 4).to_le_bytes();
        [lo[0], lo[1], lo[2], lo[3], hi[0], hi[1]]
    };
    let valid = |mac: &[u8; 6]| *mac != [0; 6] && *mac != [0xff; 6] && mac[0] & 1 == 0;
    let backup = pick(MAC_BACKUP);
    if valid(&backup) {
        return backup;
    }
    let live = pick(MAC0);
    if valid(&live) {
        return live;
    }
    // Адреса нет нигде — местный, из счётчика времени.
    let seed = crate::time::uptime_ns().to_le_bytes();
    let mac = [0x02, 0x80, seed[0], seed[1], seed[2], seed[3]];
    kprintln!("  rtl8125     : no factory address, using a local one");
    mac
}

/// Выключить ASPM L0s/L1 и CLKREQ в Link Control карты.
///
/// # Safety
///
/// `device` — эта карта.
unsafe fn pcie_aspm_off(device: &Device) {
    let mut express = None;
    device.for_each_capability(|id, at| {
        if id == 0x10 {
            express = Some(at);
            return false;
        }
        true
    });
    let Some(cap) = express else { return };
    let at = cap + 0x10;
    let value = device.config32(at);
    // SAFETY: контракт функции; Link Status (старшая половина) пишется нулями.
    unsafe { device.write_config32(at, value & 0xffff & !0x0103) };
}

/// Отобразить окно регистров.
///
/// # Safety
///
/// Ядро на своих таблицах страниц.
unsafe fn map_window(phys: PhysAddr) -> Result<Regs, Rtl8125Error> {
    let virt: VirtAddr = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;
    // SAFETY: контракт функции; регистры устройства, семантика `DEVICE`.
    unsafe { crate::arch::map_active(virt, phys, WINDOW, flags) }.map_err(|_| Rtl8125Error::MapFailed)?;
    Ok(Regs(virt.as_usize()))
}

impl Card for Rtl8125 {
    fn name(&self) -> &'static str {
        "rtl8125"
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
        let desc = Self::desc(&self.tx_ring, index);
        // SAFETY: дескриптор внутри кольца.
        if unsafe { desc.read_volatile() } & DESC_OWN != 0 {
            self.stats.tx_dropped += 1;
            return Err(CardError::Busy);
        }
        // Короткие кадры дополняются нулями до 60 байт.
        let length = frame.len().max(60);
        // SAFETY: буфер выделен на `RING * BUFFER`, длина проверена.
        unsafe {
            let buffer = self.tx_pool.as_ptr::<u8>().add(index * BUFFER);
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer, frame.len());
            core::ptr::write_bytes(buffer.add(frame.len()), 0, length - frame.len());
        }
        let phys = self.tx_pool.phys().as_u64() + (index * BUFFER) as u64;
        let eor = if index == RING - 1 { DESC_EOR } else { 0 };
        // SAFETY: дескриптор внутри кольца; признак владения — последним.
        unsafe {
            desc.add(2).cast::<u64>().write_volatile(phys);
            desc.add(1).write_volatile(0);
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            desc.write_volatile(DESC_OWN | eor | DESC_FS | DESC_LS | length as u32);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        self.regs.w16(TX_POLL, 1);
        self.tx_next = (index + 1) % RING;
        self.stats.tx_frames += 1;
        self.stats.tx_bytes += frame.len() as u64;
        Ok(())
    }

    fn receive(&mut self, frame: &mut [u8; FRAME_MAX]) -> Option<usize> {
        let index = self.rx_next;
        let desc = Self::desc(&self.rx_ring, index);
        // SAFETY: дескриптор внутри кольца.
        let status = unsafe { desc.read_volatile() };
        if status & DESC_OWN != 0 {
            // Кольцо пусто — заодно подтвердить события карты. Прерывания
            // замаскированы (опрос), но биты состояния копятся, и после
            // «нет свободных дескрипторов» (RDU, бит 4) или переполнения
            // FIFO (бит 6) неподтверждённая карта может больше не принимать
            // (17.11, ПК Романа: адрес по DHCP получен, дальше ни пинга, ни
            // страниц — пока шла установка Proton, задача приёма отставала).
            let events = self.regs.r32(INTR_STATUS);
            if events != 0 {
                self.regs.w32(INTR_STATUS, events);
                if events & (1 << 4 | 1 << 6) != 0 && !self.overflow_seen {
                    self.overflow_seen = true;
                    crate::kprintln!("  rtl8125     : receive ring overflowed (status {events:#x}), acknowledged");
                }
            }
            return None;
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let whole = status & (DESC_FS | DESC_LS) == (DESC_FS | DESC_LS);
        let length = (status & 0x3fff) as usize;
        let taken = if status & RX_RES != 0 || !whole || length < 18 || length - 4 > FRAME_MAX {
            self.stats.rx_dropped += 1;
            None
        } else {
            let length = length - 4; // без контрольной суммы кадра
            // SAFETY: буфер выделен, длина проверена.
            unsafe {
                let buffer = self.rx_pool.as_ptr::<u8>().add(index * BUFFER);
                core::ptr::copy_nonoverlapping(buffer, frame.as_mut_ptr(), length);
            }
            self.stats.rx_frames += 1;
            self.stats.rx_bytes += length as u64;
            Some(length)
        };
        self.give_rx(index);
        self.rx_next = (index + 1) % RING;
        taken
    }

    fn stats(&self) -> Stats {
        self.stats
    }
}

/// Поднимает ли драйвер такую карту — для переписи устройств.
#[must_use]
pub fn supports(vendor: u16, device: u16) -> bool {
    vendor == VENDOR_REALTEK && SUPPORTED.contains(&device)
}

/// Описание скорости и дуплекса по `PHYstatus` — для журнала.
#[must_use]
pub fn describe_phy(status: u16) -> Vec<&'static str> {
    let mut out = Vec::new();
    if status & PHY_LINK == 0 {
        out.push("no link");
        return out;
    }
    out.push(match status {
        s if s & 0x0400 != 0 => "2500M",
        s if s & 0x0010 != 0 => "1000M",
        s if s & 0x0008 != 0 => "100M",
        _ => "10M",
    });
    out.push(if status & PHY_FULL_DUPLEX != 0 { "full duplex" } else { "half duplex" });
    out
}
