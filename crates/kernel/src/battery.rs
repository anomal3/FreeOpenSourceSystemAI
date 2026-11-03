//! Батарея: что о ней известно сейчас (03.11).
//!
//! Источник у каждой машины свой — на телефоне это микросхема питания
//! MT6357 (`arch::aarch64::mtk_regs::battery`), на ПК пока никакого, — а
//! показывает её один стол: строка состояния и шторка трея. Здесь — место
//! между ними: драйвер кладёт показания, стол берёт. Нет показаний — нет и
//! батареи, и стол говорит «питание от сети», а не выдумывает проценты.

// Кладёт показания пока только телефон (aarch64); на ПК источника нет.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]

use crate::sync::SpinLock;

/// Показания батареи.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// Напряжение на батарее, мВ.
    pub millivolts: u32,
    /// Оценка заряда, 0–100.
    pub percent: u8,
    /// Подключена зарядка.
    pub charger: bool,
    /// Батарея принимает заряд (ток в неё).
    pub charging: bool,
    /// Ток: плюс — в батарею, минус — из неё, десятые мА.
    pub current_tenth_ma: i32,
    /// Температура батареи, десятые °C; `None` — термистора не видно.
    pub tenth_celsius: Option<i32>,
}

static STATE: SpinLock<Option<State>> = SpinLock::new(None);

/// Положить свежие показания (драйвер платформы).
pub fn set(state: State) {
    *STATE.lock() = Some(state);
}

/// Последние показания. `None` — у машины батареи нет или её не видно.
#[must_use]
pub fn get() -> Option<State> {
    *STATE.lock()
}

/// Оценка заряда по напряжению покоя.
///
/// Кривая — из профиля батареи `dandelion` в ядре Xiaomi
/// (`bat_setting/mt6765_battery_table_ext.dtsi`, `battery0` «NVT», 25 °C,
/// каждая пятая из ста строк; вторая батарея, «sunwoda», отличается на
/// единицы милливольт). Ниже 3,4 В Xiaomi выключает аппарат
/// (`SHUTDOWN_GAUGE0_VOLTAGE`) — это и ноль. Под нагрузкой и на зарядке
/// напряжение отходит от покоя на десятки милливольт; учёт тока счётчиком
/// заряда — следующим шагом, а скачки гасит `smooth`.
#[must_use]
pub fn percent_from_ocv(millivolts: u32) -> u8 {
    const CURVE: [(u32, u8); 21] = [
        (3400, 0),
        (3601, 5),
        (3692, 10),
        (3711, 15),
        (3741, 20),
        (3760, 25),
        (3783, 30),
        (3800, 35),
        (3816, 40),
        (3835, 45),
        (3859, 50),
        (3892, 55),
        (3952, 60),
        (3995, 65),
        (4039, 70),
        (4088, 75),
        (4142, 80),
        (4198, 85),
        (4257, 90),
        (4314, 95),
        (4370, 100),
    ];
    if millivolts <= CURVE[0].0 {
        return 0;
    }
    for pair in CURVE.windows(2) {
        let ((low_mv, low), (high_mv, high)) = (pair[0], pair[1]);
        if millivolts <= high_mv {
            let span = high_mv - low_mv;
            let into = millivolts - low_mv;
            return low + ((u32::from(high - low) * into + span / 2) / span) as u8;
        }
    }
    100
}

/// Температура батареи по напряжению на термисторе, в десятых °C.
///
/// NTC 100 кОм с подтяжкой 100 кОм к 1,8 В (`mtk_battery_table.h`:
/// `RBAT_PULL_UP_R`, `RBAT_PULL_UP_VOLT`, `Fg_Temperature_Table`):
/// `R = 100000·V / (1800 − V)`, градусы — линейно между строками таблицы,
/// за краями — −40 и 60, как у MediaTek. `None` — термистора нет (на входе
/// полная шкала).
#[must_use]
pub fn tenth_celsius_from_ntc(millivolts: u32) -> Option<i32> {
    const TABLE: [(i32, u32); 21] = [
        (-40, 4_251_119),
        (-35, 3_005_599),
        (-30, 2_149_225),
        (-25, 1_554_881),
        (-20, 1_135_037),
        (-15, 837_879),
        (-10, 624_188),
        (-5, 469_132),
        (0, 355_612),
        (5, 271_800),
        (10, 209_410),
        (15, 162_551),
        (20, 127_080),
        (25, 100_000),
        (30, 79_232),
        (35, 63_187),
        (40, 50_687),
        (45, 40_904),
        (50, 33_195),
        (55, 27_091),
        (60, 22_224),
    ];
    if millivolts >= 1800 {
        return None;
    }
    let ohms = u64::from(millivolts) * 100_000 / u64::from(1800 - millivolts);
    let ohms = ohms as u32;
    if ohms >= TABLE[0].1 {
        return Some(-400);
    }
    for pair in TABLE.windows(2) {
        let ((cold, cold_r), (warm, warm_r)) = (pair[0], pair[1]);
        if ohms >= warm_r {
            // Между строками: сопротивление падает с ростом температуры.
            let span = i64::from(cold_r - warm_r);
            let into = i64::from(cold_r - ohms);
            return Some(cold * 10 + ((i64::from(warm - cold) * 10 * into + span / 2) / span) as i32);
        }
    }
    Some(600)
}

/// Напряжение на входе зарядки по показанию VCDT: делитель платы 330/39 кОм
/// (`mtk_battery_property.h`: `R_CHARGER_1`, `R_CHARGER_2`).
#[must_use]
pub fn vbus_from_vcdt(millivolts: u32) -> u32 {
    millivolts * (330 + 39) / 39
}

/// Сгладить процент: один замер скачет от нагрузки на десятки милливольт,
/// и значок, прыгающий 61→58→63, читался бы как неисправность.
#[must_use]
pub fn smooth(previous: Option<u8>, measured: u8) -> u8 {
    // Шаг — четверть разницы, но не меньше единицы: иначе среднее застревает
    // в паре процентов от правды.
    match previous {
        None => measured,
        Some(previous) if measured > previous => previous + ((measured - previous) / 4).max(1),
        Some(previous) if measured < previous => previous - ((previous - measured) / 4).max(1),
        Some(previous) => previous,
    }
}
