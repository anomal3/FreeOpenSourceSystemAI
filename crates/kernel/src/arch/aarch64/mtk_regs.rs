//! Регистры MediaTek по кабелю: прочитать и записать слово по физическому
//! адресу (03.11).
//!
//! # Зачем
//!
//! Каждый опыт с железом телефона, зашитый в ядро, стоит сборки, прошивки и
//! двух нажатий питания живым человеком (см. `fastboot.rs`, `oem t`). Батарея,
//! зарядник и блок связи (Wi-Fi) — это десятки регистров, про которые сначала
//! надо узнать, что в них лежит после загрузчика. `fastboot oem peek/poke`
//! делают это на работающем ядре, без перезагрузки; в ядро уходит только то,
//! что уже проверено так.
//!
//! # Чего здесь нельзя
//!
//! Только окно периферии SoC (`0x1000_0000..0x2000_0000`) и контроллер
//! прерываний (`0x0c00_0000..0x0d00_0000`). Память здесь не читается и не
//! пишется: ошибка в адресе DRAM портит ядро молча, а ошибка в адресе
//! устройства в худшем случае роняет машину — это видно сразу.

use crate::sync::SpinLock;

/// Окна, в которых адрес считается регистром.
const WINDOWS: [(u64, u64); 2] = [(0x1000_0000, 0x2000_0000), (0x0c00_0000, 0x0d00_0000)];

/// Уже отображённые страницы: повторное отображение той же страницы ядро
/// отвергает, а спрашивать один блок будут много раз.
static MAPPED: SpinLock<([u64; 64], usize)> = SpinLock::new(([0; 64], 0));

/// Лежит ли адрес в окне регистров и выровнен ли на слово.
#[must_use]
pub fn allowed(address: u64) -> bool {
    address % 4 == 0 && WINDOWS.iter().any(|(start, end)| (*start..*end).contains(&address))
}

/// Отобразить страницу с этим адресом как память устройства (один раз).
pub fn map(address: u64) -> bool {
    let page = address & !0xfff;
    let mut mapped = MAPPED.lock();
    let (pages, count) = &mut *mapped;
    if pages[..*count].contains(&page) {
        return true;
    }
    if !super::mtk_touch::map_device_page(page) {
        return false;
    }
    if *count < pages.len() {
        pages[*count] = page;
        *count += 1;
    }
    true
}

/// Прочитать слово регистра. `None` — адрес вне окна или не отобразился.
#[must_use]
pub fn read32(address: u64) -> Option<u32> {
    if !allowed(address) || !map(address) {
        return None;
    }
    // SAFETY: адрес в окне регистров SoC, страница отображена как память
    // устройства; чтение volatile.
    Some(unsafe { (address as *const u32).read_volatile() })
}

/// Записать слово регистра. `false` — адрес вне окна или не отобразился.
pub fn write32(address: u64, value: u32) -> bool {
    if !allowed(address) || !map(address) {
        return false;
    }
    // SAFETY: как у [`read32`].
    unsafe { (address as *mut u32).write_volatile(value) };
    true
}

// ---------------------------------------------------------------------------
// PWRAP — дверь к микросхеме питания MT6357
// ---------------------------------------------------------------------------
//
// Микросхема питания (PMIC) не лежит на шине процессора: к ней ходят через
// «обёртку» PWRAP, которая превращает запись слова в посылку по своей
// последовательной линии. Батарея, зарядка, часы и признак «войти в fastboot»
// живут в PMIC, поэтому без этой двери ни одного из них не достать.
//
// Числа — из открытого ядра Xiaomi для этого аппарата
// (`drivers/misc/mediatek/pmic_wrap/mt6765/pwrap_hal.{h,c}`): канал WACS2,
// команда `запись<<31 | (адрес>>1)<<16 | данные`, в ответе — данные в младших
// шестнадцати битах и состояние автомата в битах 18:16. Инициализацию делает
// загрузчик; ядро Xiaomi её тоже не повторяет, если `INIT_DONE2` уже стоит.

/// Блок PWRAP у MT6765 (`mt6765.dts`, `pwrap@1000d000`).
const PWRAP: u64 = 0x1000_d000;
const WACS2_CMD: u64 = PWRAP + 0xc20;
const WACS2_RDATA: u64 = PWRAP + 0xc24;
const WACS2_VLDCLR: u64 = PWRAP + 0xc28;
/// Состояния автомата WACS2 (биты 18:16 ответа).
const FSM_IDLE: u32 = 0;
const FSM_WFVLDCLR: u32 = 6;
/// Бит «обёртка инициализирована» в слове ответа.
const RDATA_INIT_DONE: u32 = 1 << 22;
/// Сколько ждать автомат. Посылка к PMIC — микросекунды; миллисекунда
/// означает «не ответит».
const PWRAP_TIMEOUT_NS: u64 = 1_000_000;

/// Почему PMIC не ответила.
#[derive(Clone, Copy, Debug)]
pub enum PwrapError {
    /// Окно PWRAP не отобразилось.
    Unmapped,
    /// Загрузчик не включил обёртку.
    NotInitialized,
    /// Автомат не пришёл в нужное состояние вовремя.
    Timeout,
}

static PWRAP_LOCK: SpinLock<()> = SpinLock::new(());

fn fsm(rdata: u32) -> u32 {
    (rdata >> 16) & 7
}

/// Дождаться состояния автомата; вернуть последнее прочитанное слово.
fn wait_fsm(want: u32) -> Result<u32, PwrapError> {
    let started = crate::time::uptime_ns();
    loop {
        let rdata = read32(WACS2_RDATA).ok_or(PwrapError::Unmapped)?;
        if rdata & RDATA_INIT_DONE == 0 {
            return Err(PwrapError::NotInitialized);
        }
        if fsm(rdata) == want {
            return Ok(rdata);
        }
        // Прошлое чтение не забрали — забрать, иначе автомат не вернётся в
        // покой никогда (так же делает `wait_for_state_ready_init` у Xiaomi).
        if want == FSM_IDLE && fsm(rdata) == FSM_WFVLDCLR {
            write32(WACS2_VLDCLR, 1);
        }
        if crate::time::uptime_ns().wrapping_sub(started) >= PWRAP_TIMEOUT_NS {
            return Err(PwrapError::Timeout);
        }
        core::hint::spin_loop();
    }
}

/// Прочитать шестнадцатибитный регистр PMIC по его адресу (как в
/// `upmu_hw.h`: `HWCID` = `0x0008`).
pub fn pmic_read(address: u16) -> Result<u16, PwrapError> {
    let _guard = PWRAP_LOCK.lock();
    wait_fsm(FSM_IDLE)?;
    write32(WACS2_CMD, (u32::from(address >> 1)) << 16);
    let rdata = wait_fsm(FSM_WFVLDCLR)?;
    write32(WACS2_VLDCLR, 1);
    Ok(rdata as u16)
}

/// Записать регистр PMIC. Подтверждения у записи нет: следующее обращение
/// само дождётся, пока автомат освободится.
pub fn pmic_write(address: u16, value: u16) -> Result<(), PwrapError> {
    let _guard = PWRAP_LOCK.lock();
    wait_fsm(FSM_IDLE)?;
    write32(WACS2_CMD, 1 << 31 | (u32::from(address >> 1)) << 16 | u32::from(value));
    Ok(())
}

// ---------------------------------------------------------------------------
// Батарея: напряжение, температура, зарядка, ток — АЦП и счётчик MT6357
// ---------------------------------------------------------------------------

/// Опознание PMIC.
pub const HWCID: u16 = 0x0008;
pub const SWCID: u16 = 0x000a;
/// Запрос измерения АЦП: бит на канал.
const AUXADC_RQST0: u16 = 0x110e;
/// Результаты: данные в младших битах, бит 15 — «готово».
const AUXADC_ADC_BATADC: u16 = 0x10b6;
const AUXADC_ADC_VCDT: u16 = 0x108c;
const AUXADC_ADC_BAT_TEMP: u16 = 0x108e;
const RQST_BATADC: u16 = 1 << 0;
const RQST_VCDT: u16 = 1 << 2;
const RQST_BAT_TEMP: u16 = 1 << 3;
/// Зарядка подключена: `CHR_TOP_CON0`, бит 4 (`RGS_CHRDET`).
const CHR_TOP_CON0: u16 = 0x0a88;
/// Батарея на месте: `BATON_ANA_CON0`, бит 1 (`RGS_BATON_UNDET`, ноль — есть).
const BATON_ANA_CON0: u16 = 0x0e08;
/// Счётчик заряда: защёлка и текущий ток.
const FGADC_CON1: u16 = 0x0d0a;
const FGADC_CUR_CON0: u16 = 0x0d8a;

/// Измерить канал АЦП: запросить, дождаться готовности, вернуть сырое число.
fn auxadc(request: u16, result: u16, mask: u16) -> Result<u16, PwrapError> {
    let current = pmic_read(AUXADC_RQST0)?;
    pmic_write(AUXADC_RQST0, current | request)?;
    let started = crate::time::uptime_ns();
    loop {
        let value = pmic_read(result)?;
        if value & 0x8000 != 0 {
            return Ok(value & mask);
        }
        // У Xiaomi — до 160 опросов по 100–200 мкс, то есть ~30 мс на
        // 128 отсчётов канала батареи.
        if crate::time::uptime_ns().wrapping_sub(started) >= 50_000_000 {
            return Err(PwrapError::Timeout);
        }
        core::hint::spin_loop();
    }
}

/// Что известно о батарее сейчас. Сырые числа — рядом с переведёнными:
/// формулы взяты из ядра Xiaomi, а проверяются они только на аппарате.
#[derive(Clone, Copy, Debug, Default)]
pub struct Battery {
    pub hwcid: u16,
    pub swcid: u16,
    /// Напряжение батареи: сырое (15 бит) и в мВ (`raw*3*1800 >> 15`).
    pub vbat_raw: u16,
    pub vbat_mv: u32,
    /// Вход зарядки (VCDT, 12 бит, до делителя платы) в мВ.
    pub vcdt_mv: u32,
    /// Напряжение на термисторе батареи (12 бит) в мВ.
    pub temp_mv: u32,
    pub charger: bool,
    pub battery_present: bool,
    /// Ток по счётчику заряда, сырой (дополнительный код) и в десятых мА:
    /// плюс — заряд, минус — разряд.
    pub current_raw: u16,
    pub current_tenth_ma: i32,
}

/// Снять показания батареи.
pub fn battery() -> Result<Battery, PwrapError> {
    let mut info = Battery { hwcid: pmic_read(HWCID)?, swcid: pmic_read(SWCID)?, ..Battery::default() };
    info.vbat_raw = auxadc(RQST_BATADC, AUXADC_ADC_BATADC, 0x7fff)?;
    info.vbat_mv = u32::from(info.vbat_raw) * 3 * 1800 >> 15;
    info.vcdt_mv = u32::from(auxadc(RQST_VCDT, AUXADC_ADC_VCDT, 0x0fff)?) * 1800 >> 12;
    info.temp_mv = u32::from(auxadc(RQST_BAT_TEMP, AUXADC_ADC_BAT_TEMP, 0x0fff)?) * 1800 >> 12;
    info.charger = pmic_read(CHR_TOP_CON0)? & (1 << 4) != 0;
    info.battery_present = pmic_read(BATON_ANA_CON0)? & (1 << 1) == 0;
    // Защёлкнуть счётчик, прочитать ток, отпустить (`mt6357_gauge.c`).
    pmic_write(FGADC_CON1, 0x0001)?;
    let started = crate::time::uptime_ns();
    while pmic_read(FGADC_CON1)? & 0x8000 == 0 {
        if crate::time::uptime_ns().wrapping_sub(started) >= 10_000_000 {
            return Err(PwrapError::Timeout);
        }
    }
    info.current_raw = pmic_read(FGADC_CUR_CON0)?;
    pmic_write(FGADC_CON1, 0x0008)?;
    pmic_write(FGADC_CON1, 0x0000)?;
    let raw = i32::from(info.current_raw as i16);
    info.current_tenth_ma = (i64::from(raw) * 314_331 / 100_000) as i32;
    Ok(info)
}

/// Служба батареи: раз в десять секунд снять показания и отдать столу
/// (`crate::battery`). Первый же отказ PMIC — машина не та, задача уходит.
pub fn battery_task() {
    let mut percent = None;
    let mut logged = false;
    loop {
        match battery() {
            Ok(info) if info.battery_present => {
                let measured = crate::battery::percent_from_ocv(info.vbat_mv);
                let smoothed = crate::battery::smooth(percent, measured);
                percent = Some(smoothed);
                let state = crate::battery::State {
                    millivolts: info.vbat_mv,
                    percent: smoothed,
                    charger: info.charger,
                    charging: info.charger && info.current_tenth_ma > 0,
                    current_tenth_ma: info.current_tenth_ma,
                    tenth_celsius: crate::battery::tenth_celsius_from_ntc(info.temp_mv),
                };
                if !logged {
                    crate::kprintln!("  battery     : {} mV, {} %, charger {}", state.millivolts, state.percent, state.charger);
                    logged = true;
                }
                crate::battery::set(state);
            }
            Ok(_) => {}
            Err(err) => {
                crate::kprintln!("  battery     : PMIC does not answer ({err:?}); no battery shown");
                return;
            }
        }
        crate::sched::sleep_ms(10_000);
    }
}

// ---------------------------------------------------------------------------
// Подсветка и кнопка питания (03.11)
// ---------------------------------------------------------------------------
//
// Подсветкой `dandelion` управляет сама панель: команда DCS 0x51 (яркость,
// 12 бит) задаёт ШИМ её вывода LEDPWM, а от него работает преобразователь
// подсветки. Вывод GPIO154 `LCD_BL_EN` подсветку не гасит (Роман 03.11:
// «пиксели горят чёрные»), `DISP_PWM` (0x1100E000) выключен. Команду панели
// шлёт сам DSI0 без остановки видеопотока — «VM CMD»: загрузчик оставил в
// `VM_CMD_CON` длинный пакет 0x39 на 3 байта, а в `VM_CMD_DATA0` —
// `51 05 C1` (яркость 0x5C1); запуск — фронт бита 16 `DSI_START`.
// Кнопка питания — `TOPSTATUS` (0x24) микросхемы питания, бит 1
// (`PMIC_PWRKEY_DEB`); отпущенная — 1.

/// GPIO154: банк 4 (выводы 128–159), бит 26. `DOUT` банка, `+4` — установить,
/// `+8` — сбросить.
const GPIO_DOUT_BANK4: u64 = 0x1000_5140;
const LCD_BL_EN: u32 = 1 << 26;

const DSI_START: u64 = 0x1401_4000;
const DSI_START_BIT: u32 = 1 << 0;
const VM_CMD_START: u32 = 1 << 16;
const DSI_VM_CMD_CON: u64 = 0x1401_4130;
const DSI_VM_CMD_DATA0: u64 = 0x1401_4134;
/// `VM_CMD_EN | LONG_PKT | TS_VFP_EN`, тип 0x39 (DCS long write), 3 байта.
const VM_CMD_DCS_LONG3: u32 = 0x0003_3923;

/// Яркость, которую вернуть при пробуждении (по умолчанию — загрузчика).
static BRIGHTNESS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0x5C1);

/// Послать панели DCS 0x51 с яркостью `level` (0..=0xFFF).
fn panel_brightness(level: u32) -> bool {
    let level = level.min(0xFFF);
    let data = 0x51 | ((level >> 8) << 8) | ((level & 0xFF) << 16);
    let Some(start) = read32(DSI_START) else { return false };
    write32(DSI_VM_CMD_CON, VM_CMD_DCS_LONG3)
        && write32(DSI_VM_CMD_DATA0, data)
        && write32(DSI_START, (start | DSI_START_BIT) & !VM_CMD_START)
        && write32(DSI_START, start | DSI_START_BIT | VM_CMD_START)
}

/// Зажечь или погасить подсветку панели.
pub fn backlight(on: bool) {
    if !on {
        // Запомнить, какую яркость оставил загрузчик (или мы сами).
        if let Some(data) = read32(DSI_VM_CMD_DATA0)
            && data & 0xFF == 0x51
        {
            let level = ((data >> 8) & 0xFF) << 8 | ((data >> 16) & 0xFF);
            if level != 0 {
                BRIGHTNESS.store(level, core::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    let level = if on { BRIGHTNESS.load(core::sync::atomic::Ordering::Relaxed) } else { 0 };
    if !panel_brightness(level) {
        crate::kprintln!("  backlight   : DSI window not mapped");
    }
    let address = if on { GPIO_DOUT_BANK4 + 4 } else { GPIO_DOUT_BANK4 + 8 };
    let _ = write32(address, LCD_BL_EN);
}

/// `TOPSTATUS` микросхемы питания и бит кнопки питания в нём.
const TOPSTATUS: u16 = 0x0024;
const PWRKEY_DEB: u16 = 1 << 1;

/// Служба кнопки питания: опрос раз в 30 мс, нажатие — по смене уровня
/// относительно того, что было при старте (кнопку при загрузке не держат).
/// Вибромотор — на LDO `VIBR` микросхемы питания: бит 0 `LDO_VIBR_CON0`
/// включает, напряжение (`VIBR_ANA_CON0` = 0x900, 2,8 В) оставил загрузчик
/// (`mt6357-regulator.c`). Проверено на аппарате 03.11.
const LDO_VIBR_CON0: u16 = 0x1b08;
/// Когда выключить мотор (мс от старта), 0 — не гудит. Гасит его
/// [`powerkey_task`].
static VIBRATE_UNTIL_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Коротко прогудеть мотором `ms` миллисекунд.
pub fn vibrate(ms: u32) {
    let until = crate::time::uptime_ms() + u64::from(ms.max(1));
    VIBRATE_UNTIL_MS.store(until, core::sync::atomic::Ordering::Relaxed);
    let _ = pmic_write(LDO_VIBR_CON0, 1);
}

pub fn powerkey_task() {
    let Ok(idle) = pmic_read(TOPSTATUS).map(|value| value & PWRKEY_DEB) else {
        crate::kprintln!("  powerkey    : PMIC does not answer; power key not watched");
        return;
    };
    crate::kprintln!("  powerkey    : watching TOPSTATUS bit 1, released = {}", idle != 0);
    let mut down = false;
    loop {
        // Пока гудит мотор — шаг мельче: отклик клавиши длится десятки мс.
        let until = VIBRATE_UNTIL_MS.load(core::sync::atomic::Ordering::Relaxed);
        crate::sched::sleep_ms(if until != 0 { 5 } else { 30 });
        if until != 0 && crate::time::uptime_ms() >= until {
            VIBRATE_UNTIL_MS.store(0, core::sync::atomic::Ordering::Relaxed);
            let _ = pmic_write(LDO_VIBR_CON0, 0);
        }
        let Ok(value) = pmic_read(TOPSTATUS) else { continue };
        let pressed = value & PWRKEY_DEB != idle;
        if pressed && !down {
            crate::kprintln!("  powerkey    : pressed");
            crate::power::power_button();
        }
        down = pressed;
    }
}

// ---------------------------------------------------------------------------
// «Войти в fastboot» — метка в часах PMIC
// ---------------------------------------------------------------------------
//
// Android на `reboot bootloader` ставит бит 13 (`RTC_PDN1_FAST_BOOT`) в
// регистре часов `RTC_PDN1` и сбрасывает машину сторожевым таймером; LK,
// увидев бит, открывает fastboot сам. Без этого каждый новый образ стоил
// человеку зажатого питания (`mtk_rtc_common.c: rtc_mark_fast`).

const RTC_BBPU: u16 = 0x0588;
const RTC_PDN1: u16 = 0x05b4;
const RTC_PROT: u16 = 0x05bc;
const RTC_WRTGR: u16 = 0x05c2;
const RTC_PDN1_FAST_BOOT: u16 = 1 << 13;
/// Часы заняты переносом записи: `RTC_BBPU`, бит 6 (`CBUSY`).
const RTC_CBUSY: u16 = 1 << 6;

/// Перенести записанное в часы и дождаться, пока они закончат.
fn rtc_commit() -> Result<(), PwrapError> {
    pmic_write(RTC_WRTGR, 1)?;
    let started = crate::time::uptime_ns();
    while pmic_read(RTC_BBPU)? & RTC_CBUSY != 0 {
        if crate::time::uptime_ns().wrapping_sub(started) >= 500_000_000 {
            return Err(PwrapError::Timeout);
        }
    }
    Ok(())
}

/// Поставить метку «следующая загрузка — в fastboot». Возвращает прочитанное
/// после записи значение `RTC_PDN1`.
pub fn mark_fastboot() -> Result<u16, PwrapError> {
    // Снять защиту записи часов (`RTC_PROT`: 0x586A, 0x9136): `rtc_mark_fast`
    // у Xiaomi этого не делает, но там часы к тому мигу уже открыты — у нас
    // такой уверенности нет.
    pmic_write(RTC_PROT, 0x586a)?;
    rtc_commit()?;
    pmic_write(RTC_PROT, 0x9136)?;
    rtc_commit()?;
    let pdn1 = pmic_read(RTC_PDN1)?;
    pmic_write(RTC_PDN1, pdn1 | RTC_PDN1_FAST_BOOT)?;
    rtc_commit()?;
    pmic_read(RTC_PDN1)
}

// ---------------------------------------------------------------------------
// I²C по кабелю — зарядник и прочие микросхемы платы
// ---------------------------------------------------------------------------

/// Прочитать `out.len()` байт с регистра `register` устройства `address`
/// (семибитный адрес) на контроллере I²C по `base`. Раскладку регистров
/// контроллера перебирает так же, как проба тачскрина: у MT6765 она своя у
/// разных шин, и узнать её можно только ответом.
pub fn i2c_read(base: u64, address: u8, register: u8, out: &mut [u8]) -> Result<&'static str, super::mtk_i2c::Error> {
    i2c_transfer(base, address, &[register], out)
}

/// Записать байт в регистр устройства на шине I²C.
pub fn i2c_write(base: u64, address: u8, register: u8, value: u8) -> Result<&'static str, super::mtk_i2c::Error> {
    i2c_transfer(base, address, &[register, value], &mut [])
}

static I2C_LOCK: SpinLock<()> = SpinLock::new(());

fn i2c_transfer(
    base: u64,
    address: u8,
    write: &[u8],
    read: &mut [u8],
) -> Result<&'static str, super::mtk_i2c::Error> {
    use super::mtk_i2c::{Bus, Error, V1, V2};
    if !allowed(base) || !map(base) {
        return Err(Error::Timeout);
    }
    let _guard = I2C_LOCK.lock();
    let mut last = Error::Timeout;
    for layout in [V2, V1] {
        // SAFETY: окно контроллера в окне регистров и отображено выше;
        // обращения к шине отсюда идут под замком.
        let bus = unsafe { Bus::new(base as usize, layout) };
        // SAFETY: см. выше.
        unsafe { bus.prepare() };
        // SAFETY: см. выше.
        match unsafe { bus.transfer(address, write, read) } {
            Ok(()) => return Ok(layout.name),
            Err(err) => last = err,
        }
    }
    Err(last)
}

/// Число из текста: `0x…` или без префикса — шестнадцатеричное, как пишут
/// адреса регистров; десятичное — с префиксом `#`.
#[must_use]
pub fn parse_number(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Some(decimal) = text.strip_prefix('#') {
        return decimal.parse().ok();
    }
    let hex = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
    u64::from_str_radix(&hex.replace('_', ""), 16).ok()
}
