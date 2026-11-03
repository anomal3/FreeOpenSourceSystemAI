//! Блок связи MT6765 (CONNSYS: Wi-Fi, Bluetooth, GPS, FM) — включение и
//! загрузка прошивок (03.11).
//!
//! # Откуда числа
//!
//! Драйвер этого блока у Xiaomi вынесен из ядра; последовательность взята из
//! открытого драйвера MediaTek той же платформы (Motorola,
//! `vendor-mediatek-kernel_modules-connectivity-common`, `platform/mt6765.c`,
//! `mtk_wcn_consys_hw.c`, `core/wmt_ic_soc.c`) и из ядра Xiaomi
//! (`drivers/clk/mediatek/clk-mt6765-pg.c` — питание домена). Первые четыре
//! шага проверены на аппарате вживую командами `oem poke` 03.11: после них
//! регистр версии блока читается как `0x10020300`, ровно как ждёт драйвер.
//!
//! # Порядок
//!
//! `power_on` — питание от PMIC (VCN18, VCN33, VCN28), выводы аналоговой
//! половины, процессор блока в сброс, домен питания CONN, снятие защиты шины,
//! проверка версии; память блока — 4 МиБ на границе 16 МиБ, и окно EMI блока
//! (`0xF0000000` у него) переназначается на неё. `load` — разложить прошивки
//! по адресам из их заголовков. `boot` — отпустить процессор и дождаться
//! слова готовности `0x1D1E`.
//!
//! Каждый шаг — отдельная команда `fastboot oem consys …`: если что-то не
//! так, видно где, и машину не надо перезагружать.

use core::sync::atomic::{AtomicU64, Ordering};

use super::mtk_regs::{pmic_read, pmic_write, read32, write32};

/// Сброс процессора блока: `TOP_RGU + 0x18`, бит 12, ключ `0x88` в старшем
/// байте (без ключа запись отбрасывается).
const TOP_RGU_CONN: u64 = 0x1000_7018;
const CONN_CPU_RESET: u32 = 1 << 12;
const RGU_KEY: u32 = 0x88 << 24;
/// Питание домена CONN и его статус.
const CONN_PWR_CON: u64 = 0x1000_632c;
const PWR_STATUS: u64 = 0x1000_6180;
const PWR_STATUS_2ND: u64 = 0x1000_6184;
const PWR_RST_B: u32 = 1 << 0;
const PWR_ISO: u32 = 1 << 1;
const PWR_ON: u32 = 1 << 2;
const PWR_ON_2ND: u32 = 1 << 3;
const PWR_CLK_DIS: u32 = 1 << 4;
const CONN_STATUS_BIT: u32 = 1 << 1;
/// Снятие защиты шины (`INFRACFG_AO`).
const PROTECT_CLR: u64 = 0x1000_12a4;
const PROTECT_1_CLR: u64 = 0x1000_12ac;
/// Переназначение окна EMI блока на нашу память.
const EMI_REMAP: u64 = 0x1000_1380;
const PERI_REMAP: u64 = 0x1000_1388;
/// Версия блока и ответ его процессора.
const CONN_VERSION: u64 = 0x180b_1010;
const CONN_VERSION_EXPECTED: u32 = 0x1002_0300;
const CONN_HW_ID: u64 = 0x1800_2000;
const CONN_ROM_DELSEL: u64 = 0x1800_2114;
const CONN_EMI_CTRL: u64 = 0x1800_2150;
const CONN_MCU_STATE: u64 = 0x1800_2600;
const CONN_MCU_READY: u32 = 0x1d1e;
const CONN_CPUPCR: u64 = 0x1800_7104;
/// Выводы аналоговой половины (GPIO, `mt6765.h`).
const GPIO_MODE_3F0: u64 = 0x1000_53f0;
const GPIO_MODE_3E0: u64 = 0x1000_53e0;

/// Память блока: 4 МиБ, начало кратно 16 МиБ (`consys-reserve-memory`).
const EMI_SIZE: usize = 4 * 1024 * 1024;
const EMI_FRAMES: usize = EMI_SIZE / 4096;
const EMI_ALIGN_FRAMES: usize = 16 * 1024 * 1024 / 4096;
/// Физический адрес выделенной памяти; ноль — ещё не выделена.
static EMI: AtomicU64 = AtomicU64::new(0);

/// Где лежат прошивки (кладёт `xtask phone --lean`).
pub const FIRMWARE_DIR: &str = "/lib/firmware/connsys";
/// Что раскладывается по памяти блока, в порядке драйвера MediaTek.
const IMAGES: [&str; 4] =
    ["soc1_0_ram_mcu_1_1_hdr.bin", "soc1_0_patch_mcu_1_1_hdr.bin", "soc1_0_ram_bt_1_1_hdr.bin", "soc1_0_ram_wifi_1_1_hdr.bin"];

fn spin_us(us: u64) {
    let started = crate::time::uptime_ns();
    while crate::time::uptime_ns().wrapping_sub(started) < us * 1000 {
        core::hint::spin_loop();
    }
}

fn read(address: u64) -> Result<u32, &'static str> {
    read32(address).ok_or("register window not mapped")
}

fn write(address: u64, value: u32) -> Result<(), &'static str> {
    if write32(address, value) { Ok(()) } else { Err("register window not mapped") }
}

fn pmic_set(address: u16, set: u16, clear: u16) -> Result<(), &'static str> {
    let value = pmic_read(address).map_err(|_| "PMIC does not answer")?;
    pmic_write(address, (value & !clear) | set).map_err(|_| "PMIC does not answer")
}

/// Включить блок: шаги 1–4 и память. Возвращает строки отчёта.
pub fn power_on() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    let mut report = alloc::vec::Vec::new();
    // Питание аналоговой половины от PMIC (`mt6765.c`, `upmu_hw.h`): VCN18,
    // VCN33 (обе ветви — и BT, и Wi-Fi), VCN28 под управлением блока.
    pmic_set(0x1a5a, 1, 0)?; // VCN18 OP_EN
    pmic_set(0x1a58, 1, 0)?; // VCN18 EN
    spin_us(300);
    pmic_set(0x1b1e, 1, 0)?; // VCN33 OP_EN
    pmic_set(0x1b1c, 1, 0)?; // VCN33 EN_0 (BT)
    pmic_set(0x1b2a, 1, 0)?; // VCN33 EN_1 (Wi-Fi)
    spin_us(300);
    // Выводы связи с аналоговой микросхемой.
    let mode = read(GPIO_MODE_3F0)?;
    write(GPIO_MODE_3F0, (mode & 0xffff_ff88) | 0x11)?;
    let mode = read(GPIO_MODE_3E0)?;
    write(GPIO_MODE_3E0, (mode & 0x8888_888f) | 0x1111_1110)?;
    spin_us(150);
    pmic_set(0x1b8a, 1 << 1, 0)?; // VCN28 HW0_OP_EN
    pmic_set(0x1b90, 0, 1 << 1)?; // VCN28 HW0_OP_CFG = 0
    pmic_set(0x1b88, 1, 0)?; // VCN28 EN
    spin_us(300);

    // Процессор блока — в сброс, пока домен поднимается.
    let rgu = read(TOP_RGU_CONN)?;
    write(TOP_RGU_CONN, (rgu & 0x00ff_ffff) | CONN_CPU_RESET | RGU_KEY)?;

    // Домен питания CONN (`spm_mtcmos_ctrl_conn_pwr`).
    let mut pwr = read(CONN_PWR_CON)?;
    pwr |= PWR_ON;
    write(CONN_PWR_CON, pwr)?;
    pwr |= PWR_ON_2ND;
    write(CONN_PWR_CON, pwr)?;
    let started = crate::time::uptime_ns();
    while read(PWR_STATUS)? & CONN_STATUS_BIT == 0 || read(PWR_STATUS_2ND)? & CONN_STATUS_BIT == 0 {
        if crate::time::uptime_ns().wrapping_sub(started) > 50_000_000 {
            return Err("CONN power domain did not come up");
        }
    }
    pwr &= !PWR_CLK_DIS;
    write(CONN_PWR_CON, pwr)?;
    pwr &= !PWR_ISO;
    write(CONN_PWR_CON, pwr)?;
    pwr |= PWR_RST_B;
    write(CONN_PWR_CON, pwr)?;
    // Защита шины (`spm_mtcmos_ctrl_conn_bus_prot`).
    write(PROTECT_CLR, (1 << 14) | (1 << 16))?;
    write(PROTECT_CLR, 1 << 13)?;
    write(PROTECT_1_CLR, 1 << 18)?;
    spin_us(10);

    // Версия появляется не сразу: на аппарате через 10 мкс читался ещё не
    // тот ответ, а секундой позже — верный. Драйвер MediaTek ждёт её до
    // десяти раз по 20 мс (`polling_consys_chipid`) — так и здесь.
    let mut version = read(CONN_VERSION)?;
    for _ in 0..10 {
        if version == CONN_VERSION_EXPECTED {
            break;
        }
        crate::sched::sleep_ms(20);
        version = read(CONN_VERSION)?;
    }
    report.push(alloc::format!("version {version:08x} (want {CONN_VERSION_EXPECTED:08x})"));
    if version != CONN_VERSION_EXPECTED {
        return Err("CONNSYS version register does not match");
    }
    let id = read(CONN_HW_ID)?;
    report.push(alloc::format!("hw id {:04x}", id & 0xffff));

    // Память блока — один раз за жизнь ядра.
    let mut emi = EMI.load(Ordering::Relaxed);
    if emi == 0 {
        let phys = crate::mm::frame::with(|frames| frames.allocate_aligned(EMI_FRAMES, EMI_ALIGN_FRAMES))
            .flatten()
            .ok_or("no 4 MiB block on a 16 MiB boundary for CONNSYS")?;
        emi = phys.as_u64();
        if emi + EMI_SIZE as u64 > 0xc000_0000 {
            return Err("CONNSYS memory above the range the block can reach");
        }
        EMI.store(emi, Ordering::Relaxed);
    }
    let remap = read(EMI_REMAP)?;
    write(EMI_REMAP, (remap & 0xffff_f000) | ((emi >> 24) as u32 & 0xfff))?;
    let peri = read(PERI_REMAP)?;
    write(PERI_REMAP, (peri & 0xf000_ffff) | (0x10 << 16))?;
    report.push(alloc::format!("emi {emi:#x}, remap {:08x}", read(EMI_REMAP)?));
    crate::kprintln!("  consys      : powered, version {version:#010x}, memory {emi:#x}");
    Ok(report)
}

/// Вытолкнуть строки кэша данных в память: блок связи читает её мимо кэша.
fn clean_dcache(start: usize, len: usize) {
    let mut line = start & !63;
    while line < start + len {
        // SAFETY: адрес — в прямом отображении физической памяти ядра;
        // `dc civac` только выталкивает и сбрасывает строку.
        unsafe { core::arch::asm!("dc civac, {}", in(reg) line) };
        line += 64;
    }
    // SAFETY: барьер без побочных эффектов.
    unsafe { core::arch::asm!("dsb sy") };
}

/// Разложить прошивки по памяти блока. `header` — длина заголовка файла
/// (у MediaTek в одном месте сказано 32, в структуре выходит 48; проверяется
/// на аппарате).
pub fn load(header: usize) -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    let emi = EMI.load(Ordering::Relaxed);
    if emi == 0 {
        return Err("power the block first: oem consys on");
    }
    let base = crate::mm::frame::with(|frames| frames.access(crate::mm::PhysAddr::new(emi)).as_usize())
        .ok_or("frame allocator not ready")?;
    let mut report = alloc::vec::Vec::new();
    for name in IMAGES {
        let path = alloc::format!("{FIRMWARE_DIR}/{name}");
        let data = match crate::fs::read(&path, EMI_SIZE) {
            Some(Ok((data, _))) => data,
            _ => return Err("firmware file missing in /lib/firmware/connsys"),
        };
        if data.len() <= header || data.get(16..20) != Some(b"ALPS") {
            return Err("firmware file has no MediaTek header");
        }
        // Адрес назначения — в окне блока (`0xF0xxxxxx`); младший байт — признаки.
        let target = u32::from_le_bytes([data[0x18], data[0x19], data[0x1a], data[0x1b]]);
        let offset = (target & 0x00ff_ff00) as usize;
        let body = &data[header..];
        if offset + body.len() > EMI_SIZE {
            return Err("firmware does not fit the CONNSYS memory");
        }
        // SAFETY: `base..base+EMI_SIZE` — выделенные нами кадры в прямом
        // отображении; границы проверены выше.
        unsafe { core::ptr::copy_nonoverlapping(body.as_ptr(), (base + offset) as *mut u8, body.len()) };
        clean_dcache(base + offset, body.len());
        report.push(alloc::format!("{} -> +{offset:#x}, {} bytes", &name[..name.len().min(20)], body.len()));
    }
    crate::kprintln!("  consys      : firmware laid out ({header}-byte headers)");
    Ok(report)
}

/// Отпустить процессор блока и дождаться слова готовности.
pub fn boot() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    if EMI.load(Ordering::Relaxed) == 0 {
        return Err("power the block first: oem consys on");
    }
    let mut report = alloc::vec::Vec::new();
    let delsel = read(CONN_ROM_DELSEL)?;
    write(CONN_ROM_DELSEL, (delsel & 0xffff_0000) | 0x17)?;
    let ctrl = read(CONN_EMI_CTRL)?;
    write(CONN_EMI_CTRL, ctrl | 1 | (1 << 15) | (1 << 21))?;
    let rgu = read(TOP_RGU_CONN)?;
    write(TOP_RGU_CONN, ((rgu & 0x00ff_ffff) & !CONN_CPU_RESET) | RGU_KEY)?;
    let started = crate::time::uptime_ns();
    let mut state = 0;
    while crate::time::uptime_ns().wrapping_sub(started) < 300_000_000 {
        state = read(CONN_MCU_STATE)?;
        if state == CONN_MCU_READY {
            break;
        }
        crate::sched::sleep_ms(20);
    }
    let pc = read(CONN_CPUPCR)?;
    report.push(alloc::format!("mcu state {state:08x} (ready {CONN_MCU_READY:04x}), pc {pc:08x}"));
    crate::kprintln!("  consys      : mcu state {state:#x}, pc {pc:#x}");
    Ok(report)
}

/// Короткая сводка: домен, версия, состояние процессора, память.
pub fn status() -> alloc::vec::Vec<alloc::string::String> {
    let mut report = alloc::vec::Vec::new();
    let powered = read32(PWR_STATUS).is_some_and(|value| value & CONN_STATUS_BIT != 0);
    report.push(alloc::format!("domain {}", if powered { "on" } else { "off" }));
    if powered {
        report.push(alloc::format!(
            "version {:08x}, mcu {:08x}, pc {:08x}",
            read32(CONN_VERSION).unwrap_or(0),
            read32(CONN_MCU_STATE).unwrap_or(0),
            read32(CONN_CPUPCR).unwrap_or(0)
        ));
    }
    report.push(alloc::format!("emi {:#x}", EMI.load(Ordering::Relaxed)));
    report
}
