//! Модем MT6765 (MD1, поколение 6293): питание и запуск (05.11, веха M3).
//!
//! # Что уже сделал LK
//!
//! При обычной загрузке (наш образ в разделе `boot`) LK кладёт `md1img` в
//! резерв `0x8E000000` (256 МиБ), проверяет подпись, настраивает EMI MPU и
//! переадресацию банков и оставляет описание в `ccci_tag_mem` (`0x9FFF0000`,
//! `/chosen ccci,modem_info_v2`). Ядро Linux образ не грузит («modem image
//! ready, bypass load») и делает только то, что здесь: питание домена, такты,
//! PLL модема и «отпустить» процессор MD.
//!
//! # Откуда значения
//!
//! Ядро Xiaomi/MediaTek для MT6765 (`eccci/mt6765/md_sys1_platform.c`:
//! `md_cd_power_on`, `md1_pll_init`, `md_cd_let_md_go`, `md1_pre/post_access`;
//! `clk-mt6765-pg.c`: `spm_mtcmos_ctrl_md1_bus_prot`, `_md1_pwr`;
//! `cldma_reg.h` — смещения infracfg). Регистры `0x2xxxxxxx` — физические
//! адреса AP (окно «bank2»), доступны только при включённом домене MD.
//!
//! # По шагам
//!
//! `oem md status` — снимок; `oem md power` — домен, такты, сброс CCIF;
//! `oem md pll` — защита шины на время настройки, SRCCLKENA, PLL модема;
//! `oem md go` — сторож MD, `BOOT_VECTOR_EN`, снять защиту; `oem md off`.
//! Дальше (M4) — CLDMA/CCIF и рукопожатие CCCI.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::mtk_regs::{pmic_read, pmic_write, read32, write32};

const TOPCKGEN: u64 = 0x1000_0000;
const INFRA: u64 = 0x1000_1000;
const SPM: u64 = 0x1000_6000;
const APMIXED: u64 = 0x1000_C000;
const MD1_CFG: u64 = 0x1020_E300;

// infracfg_ao
const PROTECTEN: u64 = INFRA + 0x220;
const PROTECTEN_STA1: u64 = INFRA + 0x228;
const PROTECTEN_SET: u64 = INFRA + 0x2A0;
const PROTECTEN_CLR: u64 = INFRA + 0x2A4;
const PROTECTEN_1: u64 = INFRA + 0x250;
const PROTECTEN_1_STA1: u64 = INFRA + 0x258;
const PROTECTEN_1_SET: u64 = INFRA + 0x2A8;
const PROTECTEN_1_CLR: u64 = INFRA + 0x2AC;
const AP2MD_DUMMY: u64 = INFRA + 0x370;
const MD_SRCCLKENA: u64 = INFRA + 0xF0C;
const CCIF_RST_SET: u64 = INFRA + 0x150;
const CCIF_RST_CLR: u64 = INFRA + 0x154;
const IFR3_CLR: u64 = INFRA + 0x8C;
const IFR3_STA: u64 = INFRA + 0x94;
const IFR4_CLR: u64 = INFRA + 0xA8;
const IFR4_STA: u64 = INFRA + 0xAC;
const IFR5_CLR: u64 = INFRA + 0xC4;
const IFR5_STA: u64 = INFRA + 0xC8;

const MD2PERI: u32 = 1 << 6; // в PROTECTEN_1
const PERI2MD: u32 = 1 << 7; // в PROTECTEN

// SPM
const MD1_PWR_CON: u64 = SPM + 0x320;
const PWR_STATUS: u64 = SPM + 0x180;
const PWR_STATUS_2ND: u64 = SPM + 0x184;
const MD_SRAM_ISO_CON: u64 = SPM + 0x394;
const MD_EXTRA_PWR_CON: u64 = SPM + 0x398;
const PWR_RST_B: u32 = 1 << 0;
const PWR_ISO: u32 = 1 << 1;
const PWR_ON: u32 = 1 << 2;
const PWR_ON_2ND: u32 = 1 << 3;
const PWR_CLK_DIS: u32 = 1 << 4;
const SRAM_PDN: u32 = 1 << 8;

// Модем (AP-адреса окна bank2)
const MD_BOOT_VECTOR_EN: u64 = 0x2000_0024;
const MD_RGU_WDTCR: u64 = 0x200F_0100;
const MD_PLL: u64 = 0x2014_0000;
const MD_CLKSW: u64 = 0x2015_0000;

// PMIC MT6357
const BUCK_VMODEM_CON0: u16 = 0x1588;
const DA_VMODEM_EN: u16 = 0x15A4;
const BUCK_VMODEM_ELR0: u16 = 0x15AA;
const VMODEM_VOSEL_0V8: u16 = 0x30;

type Steps = Vec<String>;

fn rd(address: u64) -> Result<u32, String> {
    read32(address).ok_or_else(|| format!("read {address:#x} refused"))
}

fn wr(address: u64, value: u32) -> Result<(), String> {
    if write32(address, value) { Ok(()) } else { Err(format!("write {address:#x} refused")) }
}

fn modify(address: u64, clear: u32, set: u32) -> Result<(), String> {
    let value = rd(address)?;
    wr(address, (value & !clear) | set)
}

/// Ждать, пока `(reg & mask) == want`, не дольше `ms`.
fn poll(address: u64, mask: u32, want: u32, ms: u64, what: &str) -> Result<u64, String> {
    let start = crate::time::uptime_ns();
    loop {
        let value = rd(address)?;
        if value & mask == want {
            return Ok((crate::time::uptime_ns() - start) / 1000);
        }
        if crate::time::uptime_ns() - start > ms * 1_000_000 {
            return Err(format!("{what}: {address:#x} = {value:#010x}, want {want:#x} in {mask:#x}"));
        }
    }
}

fn pmic(register: u16) -> Result<u16, String> {
    pmic_read(register).map_err(|err| format!("pmic {register:#x}: {err:?}"))
}

/// Снимок состояния модема — без записей.
pub fn status() -> Steps {
    let mut out = Vec::new();
    let w = |a: u64| read32(a).map_or_else(|| String::from("?"), |v| format!("{v:08x}"));
    out.push(format!(
        "PWR_CON {} STA {}/{} EXTRA {}",
        w(MD1_PWR_CON),
        w(PWR_STATUS),
        w(PWR_STATUS_2ND),
        w(MD_EXTRA_PWR_CON)
    ));
    out.push(format!("PROT {} sta {} PROT1 {} sta {}", w(PROTECTEN), w(PROTECTEN_STA1), w(PROTECTEN_1), w(PROTECTEN_1_STA1)));
    out.push(format!("AP2MD {} SRCCLKENA {} TOPCK {}", w(AP2MD_DUMMY), w(MD_SRCCLKENA), w(TOPCKGEN)));
    out.push(format!("CG IFR3 {} IFR4 {} IFR5 {}", w(IFR3_STA), w(IFR4_STA), w(IFR5_STA)));
    let vm = |r: u16| pmic_read(r).map_or_else(|_| String::from("?"), |v| format!("{v:04x}"));
    out.push(format!("VMODEM con0 {} en {} vosel {}", vm(BUCK_VMODEM_CON0), vm(DA_VMODEM_EN), vm(BUCK_VMODEM_ELR0)));
    out.push(format!("MD boot stats {} {}", w(MD1_CFG), w(MD1_CFG + 4)));
    // Окно модема читается только при включённом домене — иначе шина виснет.
    if window_open() {
        out.push(format!("PLL+0 {} F00 {} BOOT_EN {}", w(MD_PLL), w(MD_PLL + 0xF00), w(MD_BOOT_VECTOR_EN)));
    }
    out
}

/// Домен MD включён и шина к нему открыта.
fn powered() -> bool {
    read32(PWR_STATUS).is_some_and(|v| v & 1 != 0)
        && read32(PWR_STATUS_2ND).is_some_and(|v| v & 1 != 0)
        && read32(PROTECTEN).is_some_and(|v| v & ((1 << 3) | (1 << 4)) == 0)
}

/// Окно регистров MD читается по-настоящему: домен включён и защита
/// AP→MD (`PERI2MD`) снята. После `go` она снова стоит (как у Linux), и
/// чтение отдаёт мусор шины — на аппарате все слова были `0x62930000`.
fn window_open() -> bool {
    powered() && read32(PROTECTEN).is_some_and(|v| v & PERI2MD == 0)
}

/// `oem md power`: такты MD, VMODEM, MTCMOS MD1, такты CCIF/CLDMA, сброс CCIF.
pub fn power_on() -> Result<Steps, String> {
    let mut out = Vec::new();
    // 1. topckgen: снять биты 8 и 9.
    modify(TOPCKGEN, (1 << 8) | (1 << 9), 0)?;
    // 2. VMODEM 0,8 В; включён ли — проверить (ядро Linux его не включает).
    let elr = pmic(BUCK_VMODEM_ELR0)?;
    if elr & 0x7f != VMODEM_VOSEL_0V8 {
        pmic_write(BUCK_VMODEM_ELR0, (elr & !0x7f) | VMODEM_VOSEL_0V8).map_err(|e| format!("pmic: {e:?}"))?;
    }
    let con0 = pmic(BUCK_VMODEM_CON0)?;
    if con0 & 1 == 0 {
        pmic_write(BUCK_VMODEM_CON0, con0 | 1).map_err(|e| format!("pmic: {e:?}"))?;
        super::timer::delay_us(300);
    }
    out.push(format!("VMODEM da_en {:#06x}", pmic(DA_VMODEM_EN)?));
    // 3a. Снять защиту шины MD (как `spm_mtcmos_ctrl_md1_bus_prot(ON)`).
    wr(PROTECTEN_CLR, (1 << 3) | (1 << 4))?;
    wr(PROTECTEN_1_CLR, MD2PERI)?;
    wr(PROTECTEN_CLR, PERI2MD)?;
    // 3b. MTCMOS MD1.
    modify(MD1_PWR_CON, PWR_RST_B, 0)?;
    modify(MD1_PWR_CON, 0, PWR_ON)?;
    modify(MD1_PWR_CON, 0, PWR_ON_2ND)?;
    let us = poll(PWR_STATUS, 1, 1, 10, "PWR_STATUS md1")?;
    poll(PWR_STATUS_2ND, 1, 1, 10, "PWR_STATUS_2ND md1")?;
    modify(MD1_PWR_CON, SRAM_PDN, 0)?;
    modify(MD_SRAM_ISO_CON, 0, 1)?;
    modify(MD1_PWR_CON, PWR_ISO, 0)?;
    modify(MD1_PWR_CON, PWR_CLK_DIS, 0)?;
    modify(MD1_PWR_CON, 0, PWR_RST_B)?;
    modify(MD_EXTRA_PWR_CON, 1, 0)?;
    out.push(format!("MTCMOS md1 on in {us} us, PWR_CON {:#010x}", rd(MD1_PWR_CON)?));
    // 4. Такты CLDMA и CCIF0..2 (запись 1 в CLR — открыть).
    wr(IFR3_CLR, (1 << 23) | (1 << 26) | (1 << 12) | (1 << 13))?;
    wr(IFR4_CLR, 1 << 3)?;
    wr(IFR5_CLR, (1 << 18) | (1 << 19))?;
    // Сброс CCIF (`ccci_reset_ccif_hw`).
    wr(CCIF_RST_SET, 1 << 8)?;
    wr(CCIF_RST_CLR, 1 << 8)?;
    out.push(format!("clocks gated IFR3 {:#x} IFR4 {:#x} IFR5 {:#x}", rd(IFR3_STA)?, rd(IFR4_STA)?, rd(IFR5_STA)?));
    Ok(out)
}

/// `oem md pll`: `md1_pre_access_md_reg`, SRCCLKENA, `md1_pll_init`.
pub fn pll() -> Result<Steps, String> {
    if !window_open() {
        return Err(String::from("MD domain is off or already released: oem md power first"));
    }
    let mut out = Vec::new();
    // Защита MD→периферия на время настройки.
    modify(AP2MD_DUMMY, 1, 0)?;
    wr(PROTECTEN_1_SET, MD2PERI)?;
    poll(PROTECTEN_1_STA1, MD2PERI, MD2PERI, 10, "MD2PERI prot")?;
    // SRCCLKENA: младший байт 0x21.
    modify(MD_SRCCLKENA, 0xff, 0x21)?;
    // PLL модема.
    let start = crate::time::uptime_ns();
    while rd(MD_PLL)? == 0 {
        if crate::time::uptime_ns() - start > 10_000_000 {
            return Err(String::from("MD PLL+0 stays 0"));
        }
    }
    modify(APMIXED, 0, 2)?;
    super::timer::delay_us(100);
    wr(MD_PLL + 0x004, 0x0202_0E88)?;
    wr(MD_PLL + 0x060, 0x8017_13B1)?;
    wr(MD_PLL + 0x058, 0x8017_1400)?;
    wr(MD_PLL + 0x050, 0x8022_9E00)?;
    wr(MD_PLL + 0x048, 0x8020_4E00)?;
    wr(MD_PLL + 0x040, 0x8021_3C00)?;
    poll(MD_PLL + 0xC00, 1 << 14, 0, 10, "PLL+C00 bit14")?;
    modify(MD_PLL + 0x064, 0x80, 0)?;
    wr(MD_PLL + 0x104, 0x04C4_3100)?;
    let mut tries = 0;
    loop {
        if tries % 5 == 0 {
            wr(MD_PLL + 0x010, 0x0010_0010)?;
        }
        if rd(MD_PLL + 0x010)? == 0x0010_0010 {
            break;
        }
        tries += 1;
        if tries > 1000 {
            return Err(format!("PLL+010 = {:#x}", rd(MD_PLL + 0x010)?));
        }
        super::timer::delay_us(10);
    }
    poll(MD_CLKSW + 0x084, 1 << 15, 1 << 15, 10, "CLKSW+084 bit15")?;
    modify(MD_CLKSW + 0x024, 0, 0x3)?;
    modify(MD_CLKSW + 0x024, 0, 0x0581_03FC)?;
    modify(MD_CLKSW + 0x028, 0, 0x10)?;
    wr(MD_CLKSW + 0x020, 1)?;
    wr(MD_PLL + 0x314, 0xFFFF)?;
    wr(MD_PLL + 0x318, 0xFFFF)?;
    wr(MD_PLL + 0xF00, 0x6293_0000)?;
    out.push(format!("PLL ready, +F00 {:#x}, tries {tries}", rd(MD_PLL + 0xF00)?));
    Ok(out)
}

/// `oem md go`: сторож MD, `MD_BOOT_VECTOR_EN`, снять защиту.
pub fn go() -> Result<Steps, String> {
    if !window_open() {
        return Err(String::from("MD domain is off or already released: oem md power, oem md pll first"));
    }
    if rd(MD_PLL + 0xF00)? != 0x6293_0000 {
        return Err(String::from("MD PLL not set: oem md pll first"));
    }
    wr(MD_RGU_WDTCR, 0x5500_0030)?;
    wr(MD_BOOT_VECTOR_EN, 1)?;
    // `md1_post_access_md_reg`.
    wr(PROTECTEN_SET, PERI2MD)?;
    poll(PROTECTEN_STA1, PERI2MD, PERI2MD, 10, "PERI2MD prot")?;
    wr(PROTECTEN_1_CLR, MD2PERI)?;
    poll(PROTECTEN_1_STA1, MD2PERI, 0, 10, "MD2PERI unprot")?;
    modify(AP2MD_DUMMY, 0, 1)?;
    crate::kprintln!("  modem       : MD1 released, boot stats {:#x} {:#x}", rd(MD1_CFG)?, rd(MD1_CFG + 4)?);
    let mut out = Vec::new();
    // Первые полсекунды — как идут стадии загрузки MD.
    let mut last = (u32::MAX, u32::MAX);
    for step in 0..10 {
        let now = (rd(MD1_CFG)?, rd(MD1_CFG + 4)?);
        if now != last {
            out.push(format!("t+{}ms boot stats {:08x} {:08x}", step * 50, now.0, now.1));
            last = now;
        }
        super::timer::delay_us(50_000);
    }
    Ok(out)
}

/// `oem md off`: защитить шину и обесточить домен (как power-down в PG).
pub fn off() -> Result<Steps, String> {
    wr(PROTECTEN_SET, (1 << 3) | (1 << 4) | PERI2MD)?;
    wr(PROTECTEN_1_SET, MD2PERI)?;
    modify(AP2MD_DUMMY, 1, 0)?;
    modify(MD_EXTRA_PWR_CON, 0, 1)?;
    modify(MD1_PWR_CON, 0, PWR_ISO)?;
    modify(MD1_PWR_CON, PWR_RST_B, 0)?;
    modify(MD1_PWR_CON, 0, PWR_CLK_DIS)?;
    modify(MD1_PWR_CON, 0, SRAM_PDN)?;
    modify(MD1_PWR_CON, PWR_ON | PWR_ON_2ND, 0)?;
    Ok(alloc::vec![format!("MD1 off, PWR_CON {:#010x} PWR_STA {:#x}", rd(MD1_PWR_CON)?, rd(PWR_STATUS)?)])
}
