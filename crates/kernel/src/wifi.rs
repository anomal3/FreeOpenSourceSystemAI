//! Wi-Fi: что о нём известно столу (03.11).
//!
//! Драйвер — у каждой машины свой (на телефоне — блок связи MT6765,
//! `arch::aarch64::mtk_consys`), а показывают его шторка и страница «Сеть»
//! в «Параметрах». Здесь — состояние между ними и команда «включить».
//! Сетей пока не ищет никто: сторона WLAN блока связи — следующий шаг, и
//! состояние говорит об этом словами, а не пустым списком.

// На ПК драйвера пока нет — часть пути стоит без вызовов.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]

use core::sync::atomic::{AtomicU8, Ordering};

/// Где Wi-Fi сейчас.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// У машины нет оборудования, которое мы умеем включать.
    Absent,
    /// Оборудование есть, выключено.
    Off,
    /// Включается.
    Starting,
    /// Прошивка блока связи запущена и отвечает; поиска сетей ещё нет.
    FirmwareUp,
    /// Включить не вышло.
    Failed,
}

static STATE: AtomicU8 = AtomicU8::new(0);

impl State {
    const fn code(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Off => 1,
            Self::Starting => 2,
            Self::FirmwareUp => 3,
            Self::Failed => 4,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Off,
            2 => Self::Starting,
            3 => Self::FirmwareUp,
            4 => Self::Failed,
            _ => Self::Absent,
        }
    }

    /// Короткая подпись — для плитки шторки.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::Absent => "нет оборудования",
            Self::Off => "выключен",
            Self::Starting => "включается…",
            Self::FirmwareUp => "блок связи запущен",
            Self::Failed => "не включился",
        }
    }
}

/// Текущее состояние.
#[must_use]
pub fn state() -> State {
    State::from_code(STATE.load(Ordering::Relaxed))
}

fn set(state: State) {
    STATE.store(state.code(), Ordering::Relaxed);
}

/// Объявить оборудование (драйвер платформы при загрузке).
pub fn declare_present() {
    if state() == State::Absent {
        set(State::Off);
    }
}

/// Что сказал драйвер при последнем включении — для «Параметров».
static REPORT: crate::sync::SpinLock<alloc::vec::Vec<alloc::string::String>> =
    crate::sync::SpinLock::new(alloc::vec::Vec::new());

/// Строки отчёта последнего включения.
#[must_use]
pub fn report() -> alloc::vec::Vec<alloc::string::String> {
    REPORT.lock().clone()
}

/// Включить: отдельной задачей — включение длится секунды, а стол ждать не
/// должен. Повторное нажатие во время включения ничего не делает.
pub fn enable() {
    if matches!(state(), State::Absent | State::Starting | State::FirmwareUp) {
        return;
    }
    set(State::Starting);
    if let Err(err) = crate::sched::spawn_daemon("wifi-up", bring_up_task) {
        crate::kprintln!("  wifi        : cannot start: {err}");
        set(State::Failed);
    }
}

fn bring_up_task() {
    #[cfg(target_arch = "aarch64")]
    let result = crate::arch::mtk_consys::bring_up();
    #[cfg(not(target_arch = "aarch64"))]
    let result: Result<alloc::vec::Vec<alloc::string::String>, &'static str> = Err("no Wi-Fi hardware driver");
    match result {
        Ok(lines) => {
            crate::kprintln!("  wifi        : connectivity firmware up");
            *REPORT.lock() = lines;
            set(State::FirmwareUp);
        }
        Err(err) => {
            crate::kprintln!("  wifi        : bring-up failed: {err}");
            *REPORT.lock() = alloc::vec![alloc::string::String::from(err)];
            set(State::Failed);
        }
    }
}
