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
    /// Прошивка Wi-Fi запущена, идёт поиск сетей.
    Scanning,
    /// Поиск закончен — список сетей в [`networks`].
    Ready,
}

static STATE: AtomicU8 = AtomicU8::new(0);
/// Блок связи уже включён этим ядром (питание, прошивки, WMT).
static CONSYS_UP: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

fn state_was_up() -> bool {
    CONSYS_UP.load(Ordering::Relaxed)
}

impl State {
    const fn code(self) -> u8 {
        match self {
            Self::Absent => 0,
            Self::Off => 1,
            Self::Starting => 2,
            Self::FirmwareUp => 3,
            Self::Failed => 4,
            Self::Scanning => 5,
            Self::Ready => 6,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Off,
            2 => Self::Starting,
            3 => Self::FirmwareUp,
            4 => Self::Failed,
            5 => Self::Scanning,
            6 => Self::Ready,
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
            Self::Scanning => "ищет сети…",
            Self::Ready => "включён",
        }
    }
}

/// Найденные сети: имя и канал.
#[must_use]
pub fn networks() -> alloc::vec::Vec<(alloc::string::String, u8)> {
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::mtk_wlan::networks().into_iter().map(|net| (net.ssid, net.channel)).collect()
    }
    #[cfg(not(target_arch = "aarch64"))]
    alloc::vec::Vec::new()
}

/// Искать заново (кнопка «Обновить» в «Параметрах»).
pub fn rescan() {
    if state() != State::Ready {
        return;
    }
    set(State::Scanning);
    if crate::sched::spawn_daemon("wifi-scan", scan_task).is_err() {
        set(State::Ready);
    }
}

fn scan_task() {
    #[cfg(target_arch = "aarch64")]
    match crate::arch::mtk_wlan::scan() {
        Ok(lines) => crate::kprintln!("  wifi        : scan: {}", lines.first().map_or("", |l| l.as_str())),
        Err(err) => crate::kprintln!("  wifi        : scan failed: {err}"),
    }
    set(State::Ready);
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
    if matches!(state(), State::Absent | State::Starting | State::Scanning | State::Ready) {
        return;
    }
    set(State::Starting);
    if let Err(err) = crate::sched::spawn_daemon("wifi-up", bring_up_task) {
        crate::kprintln!("  wifi        : cannot start: {err}");
        set(State::Failed);
    }
}

fn bring_up_task() {
    // Блок связи уже запущен (повторное нажатие после сбоя WLAN) — его
    // половину пути не повторять.
    #[cfg(target_arch = "aarch64")]
    let result = if state_was_up() {
        Ok(alloc::vec::Vec::new())
    } else {
        crate::arch::mtk_consys::bring_up()
    };
    #[cfg(not(target_arch = "aarch64"))]
    let result: Result<alloc::vec::Vec<alloc::string::String>, &'static str> = Err("no Wi-Fi hardware driver");
    match result {
        Ok(mut lines) => {
            crate::kprintln!("  wifi        : connectivity firmware up");
            CONSYS_UP.store(true, Ordering::Relaxed);
            set(State::FirmwareUp);
            *REPORT.lock() = lines.clone();
            // Вторая половина: сторона WLAN и поиск сетей.
            #[cfg(target_arch = "aarch64")]
            match crate::arch::mtk_wlan::bring_up() {
                Ok(more) => {
                    lines.extend(more);
                    *REPORT.lock() = lines;
                    set(State::Scanning);
                    scan_task();
                }
                Err(err) => {
                    crate::kprintln!("  wifi        : WLAN side failed: {err}");
                    lines.push(alloc::string::String::from(err));
                    *REPORT.lock() = lines;
                    set(State::Failed);
                }
            }
        }
        Err(err) => {
            crate::kprintln!("  wifi        : bring-up failed: {err}");
            *REPORT.lock() = alloc::vec![alloc::string::String::from(err)];
            set(State::Failed);
        }
    }
}
