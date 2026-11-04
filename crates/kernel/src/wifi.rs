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

// ---------------------------------------------------------------------------
// Запомненные сети (04.11)
// ---------------------------------------------------------------------------
//
// Как у телефонов: сеть, к которой подключились, запоминается с паролем, и в
// следующий раз пароль не спрашивают, а после поиска подключаются сами.
// Файл `/etc/wifi.cfg`, строка на сеть: имя, табуляция, пароль (в имени Wi-Fi
// бывают и пробелы, и `=`). Права `0600` — читает только root. На телефоне
// `/etc` пока в RAM-диске: запомненное живёт до перезагрузки.

const SAVED: &str = "wifi.cfg";

/// Запомненные сети: имя и пароль.
#[must_use]
pub fn saved() -> alloc::vec::Vec<(alloc::string::String, alloc::string::String)> {
    let text = crate::config::text(SAVED, 16 * 1024);
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(ssid, password)| (alloc::string::String::from(ssid), alloc::string::String::from(password)))
        .collect()
}

/// Пароль запомненной сети.
#[must_use]
pub fn saved_password(ssid: &str) -> Option<alloc::string::String> {
    saved().into_iter().find(|(name, _)| name == ssid).map(|(_, password)| password)
}

fn store(list: &[(alloc::string::String, alloc::string::String)]) {
    let mut text = alloc::string::String::new();
    for (ssid, password) in list {
        text.push_str(ssid);
        text.push('\t');
        text.push_str(password);
        text.push('\n');
    }
    if let Err(err) = crate::config::write_private(SAVED, &text) {
        crate::kprintln!("  wifi        : networks not saved: {err:?}");
    }
}

fn remember(ssid: &str, password: &str) {
    let mut list = saved();
    list.retain(|(name, _)| name != ssid);
    // Последняя удачная — первой: к ней и подключаться в первую очередь.
    list.insert(0, (alloc::string::String::from(ssid), alloc::string::String::from(password)));
    store(&list);
}

/// Забыть сеть («Забыть» в «Параметрах»); подключение к ней рвётся.
pub fn forget(ssid: &str) {
    let mut list = saved();
    list.retain(|(name, _)| name != ssid);
    store(&list);
    #[cfg(target_arch = "aarch64")]
    if connected().as_deref() == Some(ssid) {
        crate::arch::mtk_wlan::disconnect();
    }
    crate::kprintln!("  wifi        : forgot '{ssid}'");
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Сведения о подключении — для «Параметров»: канал, защита, точка.
#[must_use]
pub fn link() -> Option<(u8, &'static str, [u8; 6])> {
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::mtk_wlan::link()
    }
    #[cfg(not(target_arch = "aarch64"))]
    None
}

/// После поиска: не подключены — подключиться к запомненной сети, что видна.
fn auto_join() {
    if connected().is_some() || joining() {
        return;
    }
    let visible = networks();
    for (ssid, password) in saved() {
        if visible.iter().any(|(name, _)| *name == ssid) {
            crate::kprintln!("  wifi        : '{ssid}' is saved, joining");
            let _ = connect(&ssid, &password);
            return;
        }
    }
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
            Ok(lines) => {
                remember(&ssid, &password);
                lines
            }
            Err(err) => {
                crate::kprintln!("  wifi        : connect to '{ssid}' failed: {err}");
                let mut lines = alloc::vec![alloc::format!("Не подключиться к «{ssid}»: {}", human(err))];
                if let Some(status) = crate::arch::mtk_wlan::last_status() {
                    lines.push(alloc::format!("Код ответа роутера: {status}"));
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

/// Причина отказа драйвера — словами для человека (Роман 03.11: «при
/// включении Wi-Fi какие-то непонятные слова»).
#[must_use]
pub fn human(err: &str) -> &'static str {
    match err {
        "network not in the last scan" => "сети нет в последнем поиске — нажмите «Обновить»",
        "firmware did not grant the channel" => "модуль Wi-Fi не отдал канал",
        "no authentication answer" | "no association answer" => "роутер не ответил",
        "access point refused authentication" | "access point refused association" => "роутер отказал",
        "no EAPOL message 1 from the access point" => "роутер не начал обмен ключами",
        "no EAPOL message 3 — wrong password?" | "EAPOL message 3 MIC mismatch — wrong password" => "неверный пароль",
        "group cipher of this network is not supported" | "network offers no AES (CCMP) for the station" => {
            "у сети шифрование, которое мы пока не умеем"
        }
        "network has no WPA2 (RSN) element" => "у сети старый WPA — включите на роутере WPA2-PSK",
        "network wants WPA3 or 802.1X, not a password (PSK)" => "сети нужен WPA3 или корпоративный вход",
        _ => "ошибка драйвера Wi-Fi (подробности в журнале)",
    }
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
    auto_join();
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

/// Выключить: связь рвётся, блок связи обесточивается. Во время включения
/// или поиска — не трогать: там работает своя задача.
pub fn disable() {
    if !matches!(state(), State::Ready | State::FirmwareUp | State::Failed) || joining() {
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        crate::arch::mtk_wlan::disconnect();
        crate::arch::mtk_wlan::forget();
        crate::arch::mtk_consys::shut_down();
    }
    CONSYS_UP.store(false, Ordering::Relaxed);
    REPORT.lock().clear();
    crate::kprintln!("  wifi        : switched off");
    set(State::Off);
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
        // Технические строки драйвера — только в журнал: в «Параметрах»
        // человеку нужен итог, а не регистры.
        Ok(_) => {
            crate::kprintln!("  wifi        : connectivity firmware up");
            CONSYS_UP.store(true, Ordering::Relaxed);
            set(State::FirmwareUp);
            REPORT.lock().clear();
            // Вторая половина: сторона WLAN и поиск сетей.
            #[cfg(target_arch = "aarch64")]
            match crate::arch::mtk_wlan::bring_up() {
                Ok(_) => {
                    set(State::Scanning);
                    scan_task();
                }
                Err(err) => {
                    crate::kprintln!("  wifi        : WLAN side failed: {err}");
                    *REPORT.lock() = alloc::vec![alloc::format!("Wi-Fi не запустился: {}", human(err))];
                    set(State::Failed);
                }
            }
        }
        Err(err) => {
            crate::kprintln!("  wifi        : bring-up failed: {err}");
            *REPORT.lock() = alloc::vec![alloc::format!("Модуль Wi-Fi не запустился: {}", human(err))];
            set(State::Failed);
        }
    }
}
