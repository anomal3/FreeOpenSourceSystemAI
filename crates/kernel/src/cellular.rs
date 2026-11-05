//! Состояние сотовой связи для строки состояния (05.11).
//!
//! Роман: <у нас индикатор сотовой связи есть — указать, есть сеть или нет,
//! а также в какой сети: 4G, 3G>. Заполнять будет драйвер модема по ответам
//! AT: `+CREG`/`+CEREG` (зарегистрированы ли), `+COPS?` (поле AcT — тип
//! сети), `+CSQ`/`+CESQ` (уровень). Пока модема нет, состояние
//! [`Service::NoModem`] — полоски тусклые и перечёркнуты.

use core::sync::atomic::{AtomicU32, Ordering};

/// Есть ли связь.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Service {
    /// Модем не поднят (или его нет).
    NoModem = 0,
    /// Нет SIM-карты.
    NoSim = 1,
    /// Ищет сеть / не зарегистрирован.
    Searching = 2,
    /// Только экстренные вызовы (сеть видна, регистрация отклонена).
    Emergency = 3,
    /// Зарегистрирован (дома или в роуминге).
    Registered = 4,
}

/// Тип сети — из поля AcT ответа `+COPS?` / `+CREG`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Radio {
    Unknown = 0,
    /// GSM (AcT 0, 1, 3): 2G; с EDGE (AcT 3) — тоже 2G, подпись `E`.
    Gsm = 1,
    Edge = 2,
    /// UTRAN (AcT 2): 3G; с HSPA (4, 5, 6) — `H`.
    Umts = 3,
    Hspa = 4,
    /// E-UTRAN (AcT 7): 4G.
    Lte = 5,
}

impl Radio {
    /// Из числа AcT по 3GPP TS 27.007.
    #[must_use]
    pub fn from_act(act: u32) -> Self {
        match act {
            0 | 1 => Self::Gsm,
            3 => Self::Edge,
            2 => Self::Umts,
            4..=6 => Self::Hspa,
            7 | 9 => Self::Lte,
            _ => Self::Unknown,
        }
    }

    /// Подпись в строке состояния.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::Gsm => "2G",
            Self::Edge => "E",
            Self::Umts => "3G",
            Self::Hspa => "H",
            Self::Lte => "4G",
        }
    }
}

/// Упаковано в одно слово: служба, тип сети, полоски 0..=4, роуминг.
static STATE: AtomicU32 = AtomicU32::new(0);
/// Растёт на каждую смену — по нему стол перерисовывает строку.
static GENERATION: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
    pub service: Service,
    pub radio: Radio,
    /// 0..=4.
    pub bars: u8,
    pub roaming: bool,
}

impl State {
    /// Есть ли сеть (можно звонить).
    #[must_use]
    pub fn in_service(self) -> bool {
        self.service == Service::Registered
    }
}

#[must_use]
pub fn get() -> State {
    let word = STATE.load(Ordering::Acquire);
    let service = match word & 0xff {
        1 => Service::NoSim,
        2 => Service::Searching,
        3 => Service::Emergency,
        4 => Service::Registered,
        _ => Service::NoModem,
    };
    let radio = match (word >> 8) & 0xff {
        1 => Radio::Gsm,
        2 => Radio::Edge,
        3 => Radio::Umts,
        4 => Radio::Hspa,
        5 => Radio::Lte,
        _ => Radio::Unknown,
    };
    State { service, radio, bars: ((word >> 16) & 0xff).min(4) as u8, roaming: word & (1 << 24) != 0 }
}

pub fn set(state: State) {
    let word = state.service as u32
        | (state.radio as u32) << 8
        | u32::from(state.bars.min(4)) << 16
        | u32::from(state.roaming) << 24;
    if STATE.swap(word, Ordering::AcqRel) != word {
        GENERATION.fetch_add(1, Ordering::Release);
    }
}

#[must_use]
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Acquire)
}

/// Полоски из `+CSQ` (RSSI 0..31, 99 — неизвестно), как у Android.
#[must_use]
pub fn bars_from_csq(rssi: u32) -> u8 {
    match rssi {
        99 | 0..=1 => 0,
        2..=4 => 1,
        5..=7 => 2,
        8..=11 => 3,
        _ => 4,
    }
}

/// Полоски из RSRP LTE в дБм (`+CESQ`: rsrp − 141), как у Android.
#[must_use]
pub fn bars_from_rsrp(dbm: i32) -> u8 {
    match dbm {
        d if d >= -95 => 4,
        d if d >= -105 => 3,
        d if d >= -115 => 2,
        d if d >= -128 => 1,
        _ => 0,
    }
}
