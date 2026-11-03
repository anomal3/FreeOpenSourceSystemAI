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

/// Оценка заряда по напряжению без нагрузки, для литий-полимерной ячейки
/// на 4,4 В (у `dandelion` — 5000 мА·ч, `charge_full_design` в Android).
///
/// Это приближение, и оно названо таковым: настоящую кривую ячейки
/// (таблицу OCV из профиля батареи MTK) и учёт тока счётчиком заряда
/// поставим, когда достанем профиль этого аппарата. Под зарядкой напряжение
/// выше покоя, поэтому процент на кабеле завышен — его сглаживает
/// `smooth`.
#[must_use]
pub fn percent_from_ocv(millivolts: u32) -> u8 {
    const CURVE: [(u32, u8); 21] = [
        (3400, 0),
        (3660, 5),
        (3690, 10),
        (3715, 15),
        (3740, 20),
        (3760, 25),
        (3775, 30),
        (3790, 35),
        (3805, 40),
        (3825, 45),
        (3850, 50),
        (3880, 55),
        (3920, 60),
        (3960, 65),
        (4010, 70),
        (4060, 75),
        (4120, 80),
        (4180, 85),
        (4250, 90),
        (4320, 95),
        (4380, 100),
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
