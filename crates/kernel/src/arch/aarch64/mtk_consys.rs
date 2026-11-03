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

/// Блок прошёл весь путь (`bring_up`): питание, прошивки, патч, калибровка.
static UP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Работает ли блок связи, включённый этим ядром.
#[must_use]
pub fn is_up() -> bool {
    UP.load(Ordering::Relaxed)
}

/// Выключить домен блока: процессор в сброс, изоляция, такт, питание.
///
/// Нужно перед повторным включением: 03.11 включение поверх работающего
/// блока оставило его процессор стоять на нуле, и прошивка больше не
/// отвечала.
fn power_off() -> Result<(), &'static str> {
    let rgu = read(TOP_RGU_CONN)?;
    write(TOP_RGU_CONN, (rgu & 0x00ff_ffff) | CONN_CPU_RESET | RGU_KEY)?;
    let mut pwr = read(CONN_PWR_CON)?;
    pwr &= !PWR_RST_B;
    write(CONN_PWR_CON, pwr)?;
    pwr |= PWR_ISO;
    write(CONN_PWR_CON, pwr)?;
    pwr |= PWR_CLK_DIS;
    write(CONN_PWR_CON, pwr)?;
    pwr &= !(PWR_ON | PWR_ON_2ND);
    write(CONN_PWR_CON, pwr)?;
    let started = crate::time::uptime_ns();
    while read(PWR_STATUS)? & CONN_STATUS_BIT != 0 {
        if crate::time::uptime_ns().wrapping_sub(started) > 50_000_000 {
            return Err("CONN power domain did not go down");
        }
    }
    UP.store(false, Ordering::Relaxed);
    super::mtk_wlan::forget();
    crate::kprintln!("  consys      : powered off for a clean start");
    Ok(())
}

/// Включить блок: шаги 1–4 и память. Возвращает строки отчёта.
pub fn power_on() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    // Домен уже включён (прошлое включение, сбой) — сначала выключить.
    if read(PWR_STATUS)? & CONN_STATUS_BIT != 0 {
        power_off()?;
        spin_us(1000);
    }
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

/// Записать данные в память блока по смещению (регионы EMI прошивки WLAN).
pub fn emi_write(offset: usize, data: &[u8]) -> Result<(), &'static str> {
    let emi = EMI.load(Ordering::Relaxed);
    if emi == 0 {
        return Err("power the block first: oem consys on");
    }
    if offset + data.len() > EMI_SIZE {
        return Err("region does not fit the CONNSYS memory");
    }
    let base = crate::mm::frame::with(|frames| frames.access(crate::mm::PhysAddr::new(emi)).as_usize())
        .ok_or("frame allocator not ready")?;
    // SAFETY: границы проверены выше, память — наша.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), (base + offset) as *mut u8, data.len()) };
    clean_dcache(base + offset, data.len());
    Ok(())
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

// ---------------------------------------------------------------------------
// WMT по BTIF — разговор с прошивкой блока
// ---------------------------------------------------------------------------
//
// Проверено на аппарате 03.11 побайтно (`oem poke`/`peek`), здесь — то же в
// ядре. BTIF — последовательный порт внутри SoC; настраивается как у драйвера
// MediaTek (`btif_plat.c: hal_btif_hw_init`), приём — опросом `LSR`. Первые
// команды идут в кадре STP «MAND»: `80 40 <длина LE> 00`, пакет, `00 00`.

const BTIF: u64 = 0x1100_c000;
const BTIF_LSR: u64 = BTIF + 0x14;
const LSR_DATA: u32 = 1 << 0;
const LSR_THR_EMPTY: u32 = 1 << 5;

fn btif_init() -> Result<(), &'static str> {
    write(BTIF + 0x0c, 0)?; // FAKELCR: обычный режим
    let handshake = read(BTIF + 0x6c)?;
    write(BTIF + 0x6c, handshake | 1)?; // новый handshake
    write(BTIF + 0x08, 2)?; // сбросить FIFO приёма
    write(BTIF + 0x08, 4)?; // и передачи
    write(BTIF + 0x60, 0x18)?; // TRI_LVL: TX 8, RX 1, без петли
    write(BTIF + 0x4c, 4)?; // DMA_EN: опрос, AUTORST
    Ok(())
}

fn btif_send(bytes: &[u8]) -> Result<(), &'static str> {
    for byte in bytes {
        let started = crate::time::uptime_ns();
        while read(BTIF_LSR)? & LSR_THR_EMPTY == 0 {
            if crate::time::uptime_ns().wrapping_sub(started) > 10_000_000 {
                return Err("BTIF transmitter stuck");
            }
        }
        write(BTIF, u32::from(*byte))?;
    }
    Ok(())
}

/// Принять кадр MAND: заголовок, длина, тело, два байта CRC. Вернуть тело.
fn btif_receive(timeout_ms: u64) -> Result<alloc::vec::Vec<u8>, &'static str> {
    let started = crate::time::uptime_ns();
    let mut frame = alloc::vec::Vec::new();
    loop {
        if read(BTIF_LSR)? & LSR_DATA != 0 {
            let byte = (read(BTIF)? & 0xff) as u8;
            // Кадр начинается с 0x80 — всё до него мусор (как у парсера STP).
            if frame.is_empty() && byte != 0x80 {
                continue;
            }
            frame.push(byte);
            if frame.len() >= 4 {
                let len = usize::from(frame[2]) | (usize::from(frame[1] & 0x0f) << 8);
                if frame.len() == 4 + len + 2 {
                    return Ok(frame[4..4 + len].to_vec());
                }
            }
            continue;
        }
        if crate::time::uptime_ns().wrapping_sub(started) > timeout_ms * 1_000_000 {
            return Err("CONNSYS firmware did not answer");
        }
        core::hint::spin_loop();
    }
}

/// Команда WMT в кадре MAND — и ответ на неё.
fn wmt(packet: &[u8]) -> Result<alloc::vec::Vec<u8>, &'static str> {
    let mut frame = alloc::vec::Vec::with_capacity(packet.len() + 6);
    frame.extend_from_slice(&[0x80, 0x40 | ((packet.len() >> 8) as u8 & 0x0f), packet.len() as u8, 0]);
    frame.extend_from_slice(packet);
    frame.extend_from_slice(&[0, 0]);
    btif_send(&frame)?;
    // У драйвера — 2 с до первого включения функций (`WMT_LIB_RX_TIMEOUT`).
    btif_receive(2_000)
}

/// Прочитать регистр блока командой WMT (`01 08 10 00 02 01 00 01` +
/// адрес + значение + маска).
fn wmt_read(address: u32) -> Result<u32, &'static str> {
    let mut packet = alloc::vec![0x01, 0x08, 0x10, 0x00, 0x02, 0x01, 0x00, 0x01];
    packet.extend_from_slice(&address.to_le_bytes());
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet.extend_from_slice(&0xffffu32.to_le_bytes());
    let answer = wmt(&packet)?;
    if answer.len() < 16 || answer[0] != 0x02 || answer[1] != 0x08 {
        return Err("unexpected answer to a WMT register read");
    }
    Ok(u32::from_le_bytes([answer[12], answer[13], answer[14], answer[15]]))
}

/// Начальный обмен драйвера MediaTek: опознание, версии, сброс, номер чипа,
/// аналоговая микросхема. Возвращает строки отчёта.
pub fn wmt_init() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    btif_init()?;
    let mut report = alloc::vec::Vec::new();
    let core_id = wmt_read(0x8000_0008)?;
    let hw = wmt_read(0x8000_0000)?;
    let fw = wmt_read(0x8000_0004)?;
    report.push(alloc::format!("core {core_id:04x}, hw {hw:04x}, fw {fw:04x}"));
    if wmt(&[0x01, 0x07, 0x01, 0x00, 0x04])?.get(..5) != Some(&[0x02, 0x07, 0x01, 0x00, 0x00]) {
        return Err("WMT reset refused");
    }
    if wmt(&[0x01, 0x02, 0x05, 0x00, 0x11, 0x65, 0x67, 0x00, 0x00])?.get(..5) != Some(&[0x02, 0x02, 0x01, 0x00, 0x00]) {
        return Err("WMT refused the chip id");
    }
    let adie = wmt(&[0x01, 0x02, 0x01, 0x00, 0x12])?;
    let adie_id = match adie.get(7..9) {
        Some(bytes) => u16::from_le_bytes([bytes[0], bytes[1]]),
        None => 0,
    };
    report.push(alloc::format!("wmt reset ok, chip id set, a-die {adie_id:04x}"));
    crate::kprintln!("  consys      : wmt up: core {core_id:#x}, hw {hw:#x}, a-die {adie_id:#x}");
    Ok(report)
}

/// Патч процессора блока (`soc1_0_patch_mcu`) — командой WMT «PDA»
/// (`mtk_wcn_soc_pda_patch_dwn`): его адрес в заголовке — `0x0001C000`,
/// внутренняя память процессора, а не EMI, и копией в EMI его не положить.
/// Команда `01 01 11 00 00 <адрес> <размер> <ключ 0> 2f 02 02 00` → `02 01 01
/// 00 00`, затем без кадров STP: `<размер+12>` и восемь нулей, затем тело;
/// после — сброс WMT.
pub fn wmt_patch() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    let path = alloc::format!("{FIRMWARE_DIR}/soc1_0_patch_mcu_1_1_hdr.bin");
    let data = match crate::fs::read(&path, EMI_SIZE) {
        Some(Ok((data, _))) => data,
        _ => return Err("soc1_0_patch_mcu missing in /lib/firmware/connsys"),
    };
    if data.len() <= 48 || data.get(16..20) != Some(b"ALPS") {
        return Err("patch has no MediaTek header");
    }
    let address = u32::from_le_bytes([data[0x18], data[0x19], data[0x1a], data[0x1b]]) & !0xff;
    let body = &data[48..];
    let size = body.len() as u32;
    let mut command = alloc::vec![0x01, 0x01, 0x11, 0x00, 0x00];
    command.extend_from_slice(&address.to_le_bytes());
    command.extend_from_slice(&size.to_le_bytes());
    command.extend_from_slice(&[0, 0, 0, 0, 0x2f, 0x02, 0x02, 0x00]);
    let answer = wmt(&command)?;
    if answer.get(..5) != Some(&[0x02, 0x01, 0x01, 0x00, 0x00]) {
        crate::kprintln!("  consys      : PDA answer {:02x?}", &answer[..answer.len().min(16)]);
        return Err("WMT refused the patch download");
    }
    let mut head = [0u8; 12];
    head[0..4].copy_from_slice(&(size + 12).to_le_bytes());
    btif_send(&head)?;
    btif_send(body)?;
    // Ответ на тело, если он есть, и сброс.
    let tail = btif_receive(500).ok();
    let reset = wmt(&[0x01, 0x07, 0x01, 0x00, 0x04])?;
    crate::kprintln!("  consys      : patch {address:#x} {size} B sent, tail {:02x?}, reset {:02x?}", tail.as_deref().map(|t| &t[..t.len().min(8)]), &reset[..reset.len().min(5)]);
    Ok(alloc::vec![
        alloc::format!("patch {address:#x}, {size} B over BTIF"),
        alloc::format!("after: {:02x?}", tail.as_deref().map(|t| &t[..t.len().min(8)])),
        alloc::format!("wmt reset {:02x?}", &reset[..reset.len().min(5)]),
    ])
}

/// Калибровка радиочасти (`01 14 01 00 01`) — драйвер шлёт её после опроса
/// аналоговой микросхемы. Ответ длинный: таблица калибровки.
pub fn wmt_calibrate() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    let answer = wmt(&[0x01, 0x14, 0x01, 0x00, 0x01])?;
    if answer.get(..2) != Some(&[0x02, 0x14]) {
        return Err("RF calibration refused");
    }
    Ok(alloc::vec![alloc::format!("rf calibration ok, {} B", answer.len())])
}

/// Весь путь включения: питание, прошивки, процессор, начальный обмен.
pub fn bring_up() -> Result<alloc::vec::Vec<alloc::string::String>, &'static str> {
    // Первое включение после загрузки на аппарате дважды не увидело версию
    // блока даже за 200 мс, а повтор всей последовательности сразу проходил.
    let mut report = match power_on() {
        Ok(report) => report,
        Err(err) => {
            crate::kprintln!("  consys      : first power-on: {err}; trying once more");
            power_on()?
        }
    };
    report.extend(load(48)?);
    report.extend(boot()?);
    if read(CONN_MCU_STATE)? != CONN_MCU_READY {
        return Err("CONNSYS processor did not start");
    }
    report.extend(wmt_init()?);
    report.extend(wmt_patch()?);
    report.extend(wmt_calibrate()?);
    UP.store(true, Ordering::Relaxed);
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
