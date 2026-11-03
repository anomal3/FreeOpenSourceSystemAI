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
    // Одно имя — одна строка: у сети бывает несколько точек доступа
    // (MARK69 на аппарате — две), а человек выбирает сеть, а не точку.
    #[cfg(target_arch = "aarch64")]
    {
        let mut out: alloc::vec::Vec<(alloc::string::String, u8)> = alloc::vec::Vec::new();
        for net in crate::arch::mtk_wlan::networks() {
            if net.ssid.is_empty() || !out.iter().any(|(ssid, _)| *ssid == net.ssid) {
                out.push((net.ssid, net.channel));
            }
        }
        out
    }
    #[cfg(not(target_arch = "aarch64"))]
    alloc::vec::Vec::new()
}

/// Сеть и пароль, к которым подключается задача `wifi-join`.
static JOIN: crate::sync::SpinLock<Option<(alloc::string::String, alloc::string::String)>> = crate::sync::SpinLock::new(None);
/// Идёт подключение.
static JOINING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Подключиться к сети по имени и паролю (пустой — открытая сеть):
/// отдельной задачей, отчёт — в [`report`], итог — [`connected`].
pub fn connect(ssid: &str, password: &str) -> Result<(), &'static str> {
    if state() != State::Ready {
        return Err("Wi-Fi не включён");
    }
    if JOINING.swap(true, Ordering::Relaxed) {
        return Err("уже подключается");
    }
    *JOIN.lock() = Some((alloc::string::String::from(ssid), alloc::string::String::from(password)));
    if crate::sched::spawn_daemon("wifi-join", join_task).is_err() {
        JOINING.store(false, Ordering::Relaxed);
        return Err("не удалось запустить подключение");
    }
    Ok(())
}

/// Идёт ли подключение.
#[must_use]
pub fn joining() -> bool {
    JOINING.load(Ordering::Relaxed)
}

/// К какой сети подключены.
#[must_use]
pub fn connected() -> Option<alloc::string::String> {
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::mtk_wlan::connected()
    }
    #[cfg(not(target_arch = "aarch64"))]
    None
}

fn join_task() {
    let Some((ssid, password)) = JOIN.lock().take() else {
        JOINING.store(false, Ordering::Relaxed);
        return;
    };
    #[cfg(target_arch = "aarch64")]
    {
        let result = crate::arch::mtk_wlan::connect(&ssid, &password);
        let lines = match result {
            Ok(lines) => lines,
            Err(err) => {
                crate::kprintln!("  wifi        : connect to '{ssid}' failed: {err}");
                let mut lines = alloc::vec![alloc::format!("не подключиться к «{ssid}»: {err}")];
                if let Some(status) = crate::arch::mtk_wlan::last_status() {
                    lines.push(alloc::format!("код ответа точки доступа: {status}"));
                }
                lines
            }
        };
        *REPORT.lock() = lines;
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = (ssid, password);
    JOINING.store(false, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Счётчик перемен: стол перерисовывает шторку и «Параметры», когда он
/// сдвинулся (состояние, отчёт, подключение).
static GENERATION: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Текущее значение счётчика перемен.
#[must_use]
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
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
    GENERATION.fetch_add(1, Ordering::Relaxed);
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
    // Блок уже запущен этим ядром (в том числе командами `oem consys` по
    // кабелю) — его не трогать: включение поверх работающего блока 03.11
    // остановило его процессор.
    #[cfg(target_arch = "aarch64")]
    let result = if state_was_up() || crate::arch::mtk_consys::is_up() {
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
