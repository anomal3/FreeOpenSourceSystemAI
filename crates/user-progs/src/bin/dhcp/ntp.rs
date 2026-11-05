//! SNTP — время из сети (05.11.2026, RFC 4330).
//!
//! # Зачем
//!
//! Телефон загружается без времени суток: часов PMIC ядро ещё не читает, а
//! прошивка времени не отдаёт. HTTPS без даты не проверит ни одного
//! сертификата — NetSurf отказывает любому сайту. Время из сети — первое, что
//! получается сразу после адреса.
//!
//! # Когда спрашивать
//!
//! * Времени нет (или оно до 2025 года — севшая батарейка CMOS) — сразу после
//!   аренды и до первого ответа, с паузами 30 с, 1, 2, 4… минуты, но не реже
//!   раза в 15 минут.
//! * Время от прошивки есть — первая сверка через 15 минут после аренды, не
//!   при загрузке: машина со своими часами не обязана ждать сети, а сценарии
//!   стенда не обязаны видеть лишних пакетов. Часы переводятся, только если
//!   разошлись больше чем на 2 с; иначе в журнале строка «сходятся».
//! * Дальше — раз в 6 часов: счётчик уходит на секунды в сутки.
//!
//! # У кого спрашивать
//!
//! По порядку, до первого годного ответа:
//!
//! 1. Сервер из опции DHCP 42 — его назвала сама сеть.
//! 2. Шлюз: домашние роутеры (MikroTik, Keenetic, многие прошивки OpenWrt)
//!    отвечают NTP сами.
//! 3. Публичные по имени через DNS ядра: `pool.ntp.org`, `time.google.com`,
//!    `time.cloudflare.com`.
//! 4. Они же числом — на случай, когда DNS не отвечает: `162.159.200.1`
//!    (Cloudflare) и `216.239.35.0` (Google).
//!
//! # Чему верить
//!
//! Ответу сервера (режим 4) на **наш** запрос: в поле «originate» обязан
//! вернуться наш «transmit», иначе это чужой или запоздалый ответ. Индикатор
//! LI не 3 («часы сервера не синхронизированы»), слой 1..15 (ноль — «kiss of
//! death», отказ), время не раньше 2025 года. Задержка в сети делится
//! пополам — как и в полном NTP, без фильтров и подстройки хода.

use user_progs::{
    CLOCK_MONOTONIC, CLOCK_REALTIME, Line, Timespec, bind, clock, close_socket, connect,
    recv_waiting, resolve, send_waiting, set_clock, socket, time_now, uptime_ms,
};

/// Порт NTP.
const PORT: u16 = 123;
/// Длина пакета SNTP без расширений и подписи.
const PACKET: usize = 48;
/// Секунд от эпохи NTP (1900) до эпохи Unix (1970).
const NTP_TO_UNIX: u64 = 2_208_988_800;
/// Раньше этого (2025-01-01) время не бывает: ответ — мусор, часы машины —
/// севшая батарейка.
const EARLIEST: u64 = user_abi::SETCLOCK_EARLIEST;

/// Сколько ждать ответа на один запрос.
const REPLY_TIMEOUT_MS: u64 = 1_500;
/// Сколько раз спросить один сервер.
const ATTEMPTS: u32 = 2;
/// На сколько часам позволено разойтись с сервером, прежде чем их переведут.
const TOLERANCE_NS: i128 = 2_000_000_000;

/// Первая сверка машины со своими часами — через столько после аренды.
const FIRST_CHECK_MS: u64 = 15 * 60 * 1000;
/// Как часто сверяться потом.
const RESYNC_MS: u64 = 6 * 60 * 60 * 1000;
/// Первая пауза после неудачи; дальше вдвое, до [`MOST_RETRY_MS`].
const FIRST_RETRY_MS: u64 = 30_000;
const MOST_RETRY_MS: u64 = 15 * 60 * 1000;

/// Публичные серверы по имени, в порядке опроса.
const NAMES: [&str; 3] = ["pool.ntp.org", "time.google.com", "time.cloudflare.com"];
/// Они же числом: Cloudflare и Google. Последняя надежда, когда не отвечает
/// DNS.
const LITERALS: [u32; 2] = [0xA29F_C801, 0xD8EF_2300];

/// Расписание сверок.
pub struct Schedule {
    /// Когда сверяться в следующий раз (время работы, мс); `u64::MAX` — пока
    /// незачем (адреса ещё нет).
    next: u64,
    /// Пауза после следующей неудачи.
    retry: u64,
    gateway: u32,
    server: u32,
}

impl Schedule {
    pub const fn new() -> Self {
        Self { next: u64::MAX, retry: FIRST_RETRY_MS, gateway: 0, server: 0 }
    }

    /// Аренда получена (или продлена): запомнить шлюз и сервер сети. Первая
    /// аренда назначает первую сверку — сразу, если времени нет, и через
    /// [`FIRST_CHECK_MS`], если оно есть.
    pub fn adopt(&mut self, gateway: u32, server: u32) {
        self.gateway = gateway;
        self.server = server;
        if self.next == u64::MAX {
            let wait = if clock_known() { FIRST_CHECK_MS } else { 0 };
            self.next = uptime_ms() + wait;
        }
    }

    pub fn due(&self, now: u64) -> bool {
        now >= self.next
    }

    pub fn next_at(&self) -> u64 {
        self.next
    }

    /// Сверить часы и назначить следующую сверку.
    pub fn sync(&mut self) {
        if self.try_servers() {
            self.retry = FIRST_RETRY_MS;
            self.next = uptime_ms() + RESYNC_MS;
            return;
        }
        Line::to_log()
            .str("dhcp: ntp: no server answered; next try in ")
            .num(self.retry / 1000)
            .str(" s")
            .end();
        self.next = uptime_ms() + self.retry;
        self.retry = (self.retry * 2).min(MOST_RETRY_MS);
    }

    /// Пройти серверы по порядку; `true` — время получено (и, если надо,
    /// часы переведены).
    fn try_servers(&self) -> bool {
        let mut source = Source::new();
        if self.server != 0 {
            source.address(self.server).text(" (dhcp)");
            if ask(self.server, &source) {
                return true;
            }
        }
        if self.gateway != 0 && self.gateway != self.server {
            source.clear().address(self.gateway).text(" (gateway)");
            if ask(self.gateway, &source) {
                return true;
            }
        }
        for name in NAMES {
            let Some(address) = resolve(name) else {
                Line::to_log().str("dhcp: ntp: cannot resolve ").str(name).end();
                continue;
            };
            source.clear().text(name).text(" ").address(address);
            if ask(address, &source) {
                return true;
            }
        }
        for address in LITERALS {
            source.clear().address(address);
            if ask(address, &source) {
                return true;
            }
        }
        false
    }
}

/// Знает ли система время суток — и правдоподобное ли оно.
fn clock_known() -> bool {
    time_now() >= EARLIEST
}

/// Спросить один сервер и, если ответ годный, решить про часы.
fn ask(address: u32, source: &Source) -> bool {
    let Some(sample) = query(address) else {
        Line::to_log().str("dhcp: ntp: no answer from ").str(source.as_str()).end();
        return false;
    };
    // Время сервера, пересчитанное на «сейчас»: между ответом и этой строкой
    // прошло не ноль.
    let now_ns = sample.unix_ns + u128::from(mono_ns().saturating_sub(sample.mono_ns));
    let at = Timespec {
        seconds: (now_ns / 1_000_000_000) as u64,
        nanos: (now_ns % 1_000_000_000) as u32,
        _reserved: 0,
    };

    if clock_known() {
        if let Some(ours) = clock(CLOCK_REALTIME) {
            let ours = i128::from(ours.seconds) * 1_000_000_000 + i128::from(ours.nanos);
            let offset = now_ns as i128 - ours;
            if offset.abs() <= TOLERANCE_NS {
                Line::to_log()
                    .str("dhcp: ntp: the clock agrees with ")
                    .str(source.as_str())
                    .str(" within ")
                    .signed((offset / 1_000_000) as i64)
                    .str(" ms")
                    .end();
                return true;
            }
        }
    }
    // Строку с числом и датой пишет ядро — оно знает, на сколько часы
    // сдвинулись на самом деле.
    let result = set_clock(&at, source.as_str());
    if result != 0 {
        Line::to_log()
            .str("dhcp: ntp: the kernel refused the time, code ")
            .signed(result)
            .end();
        return false;
    }
    true
}

/// Годный ответ сервера.
struct Sample {
    /// Время сервера на момент [`Sample::mono_ns`], наносекунды эпохи Unix.
    unix_ns: u128,
    /// Монотонные часы в момент приёма ответа.
    mono_ns: u64,
}

/// Один обмен с сервером: до [`ATTEMPTS`] запросов.
fn query(address: u32) -> Option<Sample> {
    let socket = socket();
    if socket < 0 {
        return None;
    }
    let result = if bind(socket, 0) < 0 || connect(socket, address, PORT) < 0 {
        None
    } else {
        (0..ATTEMPTS).find_map(|_| exchange(socket))
    };
    // Сокет закрывается на любом пути: служба живёт неделями, и сокет на
    // каждую сверку кончился бы раньше, чем она.
    close_socket(socket);
    result
}

fn exchange(socket: i64) -> Option<Sample> {
    let mut packet = [0u8; PACKET];
    // LI 0, версия 4, режим 3 (клиент).
    packet[0] = 0b00_100_011;
    // В «transmit» — наша метка, а не наше время: времени у нас как раз нет.
    // Сервер вернёт её в «originate», и по ней ответ узнаётся как свой.
    let cookie = mono_ns() | 1;
    packet[40..48].copy_from_slice(&cookie.to_be_bytes());

    let sent = mono_ns();
    if send_waiting(socket, &packet, 40) < 0 {
        return None;
    }
    let deadline = uptime_ms() + REPLY_TIMEOUT_MS;
    let mut reply = [0u8; 128];
    loop {
        let left = deadline.saturating_sub(uptime_ms());
        if left == 0 {
            return None;
        }
        let got = recv_waiting(socket, &mut reply, left);
        if got <= 0 {
            return None;
        }
        let received = mono_ns();
        let Some((server_rx, server_tx)) = parse(&reply[..got as usize], cookie) else {
            // Чужой или запоздалый ответ — ждать дальше своего.
            continue;
        };
        // Задержка в сети туда и обратно: всё время обмена минус то, что
        // сервер держал запрос у себя.
        let round_trip = u128::from(received.saturating_sub(sent));
        let held = server_tx.saturating_sub(server_rx);
        let delay = round_trip.saturating_sub(held);
        return Some(Sample { unix_ns: server_tx + delay / 2, mono_ns: received });
    }
}

/// Разобрать ответ: время приёма и время отправки у сервера, наносекунды
/// эпохи Unix.
fn parse(reply: &[u8], cookie: u64) -> Option<(u128, u128)> {
    if reply.len() < PACKET {
        return None;
    }
    let leap = reply[0] >> 6;
    let version = (reply[0] >> 3) & 7;
    let mode = reply[0] & 7;
    let stratum = reply[1];
    if leap == 3 || !(1..=4).contains(&version) || mode != 4 || !(1..=15).contains(&stratum) {
        return None;
    }
    if be64(&reply[24..32]) != cookie {
        return None;
    }
    let received = timestamp(&reply[32..40])?;
    let transmitted = timestamp(&reply[40..48])?;
    if transmitted / 1_000_000_000 < u128::from(EARLIEST) {
        return None;
    }
    Some((received, transmitted))
}

/// Метка времени NTP (32 бита секунд с 1900 года и 32 бита долей) —
/// наносекунды эпохи Unix. `None` — нулевая метка, «время неизвестно».
fn timestamp(bytes: &[u8]) -> Option<u128> {
    let seconds = u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    let fraction = u64::from(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]));
    if seconds == 0 && fraction == 0 {
        return None;
    }
    // Счёт секунд переполнится в феврале 2036 года (эра 1). Метка меньше
    // 2^31 — уже новая эра: до 1968 года ответов не бывает.
    let seconds = if seconds < 0x8000_0000 { seconds + (1 << 32) } else { seconds };
    let unix = seconds.checked_sub(NTP_TO_UNIX)?;
    let nanos = (fraction * 1_000_000_000) >> 32;
    Some(u128::from(unix) * 1_000_000_000 + u128::from(nanos))
}

fn be64(bytes: &[u8]) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(word)
}

/// Монотонные часы в наносекундах.
fn mono_ns() -> u64 {
    clock(CLOCK_MONOTONIC)
        .map_or(uptime_ms() * 1_000_000, |at| at.seconds * 1_000_000_000 + u64::from(at.nanos))
}

/// Откуда время — строкой для журнала, без кучи.
struct Source {
    bytes: [u8; user_abi::SETCLOCK_SOURCE_MAX],
    used: usize,
}

impl Source {
    const fn new() -> Self {
        Self { bytes: [0; user_abi::SETCLOCK_SOURCE_MAX], used: 0 }
    }

    fn clear(&mut self) -> &mut Self {
        self.used = 0;
        self
    }

    fn text(&mut self, text: &str) -> &mut Self {
        let take = text.len().min(self.bytes.len() - self.used);
        self.bytes[self.used..self.used + take].copy_from_slice(&text.as_bytes()[..take]);
        self.used += take;
        self
    }

    fn address(&mut self, address: u32) -> &mut Self {
        for (index, byte) in address.to_be_bytes().iter().enumerate() {
            if index > 0 {
                self.text(".");
            }
            let mut digits = [0u8; 3];
            let mut value = *byte;
            let mut at = digits.len();
            loop {
                at -= 1;
                digits[at] = b'0' + value % 10;
                value /= 10;
                if value == 0 {
                    break;
                }
            }
            // SAFETY: цифры ASCII.
            self.text(unsafe { core::str::from_utf8_unchecked(&digits[at..]) });
        }
        self
    }

    fn as_str(&self) -> &str {
        // Сюда пишутся только ASCII-имена и цифры, а обрезка идёт по байтам
        // ASCII — строка всегда цельная.
        core::str::from_utf8(&self.bytes[..self.used]).unwrap_or("?")
    }
}
