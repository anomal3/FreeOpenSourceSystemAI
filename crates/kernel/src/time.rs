//! Время суток.
//!
//! # Чего у ядра нет
//!
//! Часов. Ни одна из целевых плат не даёт их одинаково: на x86-64 это CMOS за
//! портами `0x70`/`0x71`, на плате `virt` — PL031 по адресу из таблиц прошивки,
//! а на Raspberry Pi 4 батарейных часов нет вовсе, и время там знает только
//! тот, кто его откуда-то принёс. Писать три драйвера ради одного числа
//! незачем: прошивка все три уже спрятала за `GetTime`, и загрузчик спрашивает
//! её последним действием перед выходом из boot services
//! ([`boot_info::BootInfo::wall_clock`]).
//!
//! # Что здесь есть вместо них
//!
//! Точка отсчёта и счётчик тиков. Время суток — это `момент загрузки + время
//! работы`, где второе слагаемое считает системный таймер
//! ([`crate::irq::uptime_ms`]). Отсюда два свойства, которые лучше назвать, чем
//! обнаружить:
//!
//! * **Часы уходят ровно настолько, насколько врёт таймер.** Сверять их не с
//!   чем: второго источника времени в системе нет.
//! * **Перевести часы нечем.** Команды `date -s` не существует, потому что
//!   записать результат некуда — обратной дороги к `SetTime` после выхода из
//!   boot services нет.
//!
//! Когда прошивка часов не отдала, время суток остаётся **неизвестным**, и
//! система так и говорит. Это принципиально: выдуманная дата разошлась бы по
//! меткам файлов, и отличить её потом было бы не по чему.
//!
//! # Часовой пояс
//!
//! Внутри всё считается в UTC: метки файлов, журнал, сравнения. Смещение
//! появляется только там, где время показывают человеку, и приходит из
//! `/etc/system.cfg` — файла, который написал установщик, спросив пояс на
//! отдельном экране. На системе, загруженной с носителя, файла нет, и смещение
//! остаётся нулевым.
//!
//! Перехода на летнее время здесь нет и не будет до тех пор, пока в системе не
//! появится база часовых поясов: правило перехода — свойство страны и года, а
//! не числа в конфигурационном файле.

use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use calendar::DateTime;

use crate::{fs, irq, kprintln};

/// Путь к файлу настроек, который написал установщик.
const CONFIG: &str = "/etc/system.cfg";

/// Сколько байт файла ядро согласно прочитать. Файл маленький и свой, но пришёл
/// с носителя — то есть из-за границы доверия.
const CONFIG_LIMIT: usize = 8 * 1024;

/// Секунды эпохи Unix на момент, когда время стало известно. `0` — неизвестно.
static BOOT_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Сколько на тот момент показывал счётчик времени работы.
///
/// Хранится потому, что между запуском таймера и чтением `BootInfo` проходит
/// не ноль: ядро успевает разобрать карту памяти и поднять кучу. Без этой
/// поправки все часы системы отставали бы на это время навсегда.
static EPOCH_AT_UPTIME_MS: AtomicU64 = AtomicU64::new(0);

/// Смещение местного времени от UTC в минутах.
static OFFSET_MINUTES: AtomicI32 = AtomicI32::new(0);

/// Запомнить время, которое прошивка отдала загрузчику.
///
/// `0` означает «часов не было»; тогда система остаётся без времени суток, и
/// это состояние печатается при загрузке — молчаливое отсутствие даты выглядит
/// точно так же, как дата неверная.
pub fn adopt_boot_clock(seconds: u64) {
    if seconds == 0 {
        kprintln!("  clock       : the firmware had none; the date is unknown");
        return;
    }
    EPOCH_AT_UPTIME_MS.store(irq::uptime_ms(), Ordering::Relaxed);
    BOOT_EPOCH.store(seconds, Ordering::Release);
    // Число стоит сразу за меткой намеренно: эту строку читает не только
    // человек. Стенд берёт из неё секунды эпохи и сверяет их с часами хоста —
    // проверка «система знает время» иначе свелась бы к «система что-то
    // напечатала», а напечатать правдоподобное можно и не зная ничего.
    kprintln!(
        "  clock       : {seconds} s from the firmware = {} UTC",
        DateTime::from_unix(seconds as i64)
    );
}

/// Текущее время в секундах эпохи Unix, UTC. `None`, если часов не было.
#[must_use]
pub fn now_unix() -> Option<u64> {
    let epoch = BOOT_EPOCH.load(Ordering::Acquire);
    if epoch == 0 {
        return None;
    }
    let elapsed_ms = irq::uptime_ms().saturating_sub(EPOCH_AT_UPTIME_MS.load(Ordering::Relaxed));
    Some(epoch + elapsed_ms / 1000)
}

/// То же число, но в виде, в котором его хранит ext2: 32 бита, ноль при
/// отсутствии часов.
///
/// Ноль здесь — не «1970 год», а «неизвестно»: именно так его и следует читать
/// в метке файла, и именно это стоит в файлах, созданных системой, которая
/// времени не знала.
#[must_use]
pub fn now_unix_u32() -> u32 {
    now_unix().and_then(|seconds| u32::try_from(seconds).ok()).unwrap_or(0)
}

/// Текущее время по Гринвичу.
#[must_use]
pub fn now_utc() -> Option<DateTime> {
    now_unix().map(|seconds| DateTime::from_unix(seconds as i64))
}

/// Текущее местное время — то, что показывают человеку.
#[must_use]
pub fn now_local() -> Option<DateTime> {
    let seconds = now_unix()? as i64;
    Some(DateTime::from_unix(seconds + i64::from(offset_minutes()) * 60))
}

/// Смещение местного времени от UTC в минутах.
#[must_use]
pub fn offset_minutes() -> i32 {
    OFFSET_MINUTES.load(Ordering::Relaxed)
}

/// Смещение в том виде, в каком его пишут рядом со временем: `+03:00`.
#[must_use]
pub fn offset_text() -> alloc::string::String {
    let minutes = offset_minutes();
    let sign = if minutes < 0 { '-' } else { '+' };
    alloc::format!("{sign}{:02}:{:02}", minutes.abs() / 60, minutes.abs() % 60)
}

/// Местное время в виде `ЧЧ:ММ` — то, что показывает панель.
///
/// `None`, когда часов не было: панель тогда показывает только время работы, а
/// не прочерк на месте часов. Пустое место говорит «часов нет» ничуть не хуже и
/// не отнимает ширину у того, что можно показать.
#[must_use]
pub fn clock_text() -> Option<alloc::string::String> {
    let local = now_local()?;
    Some(alloc::format!("{:02}:{:02}", local.hour, local.minute))
}

/// Метка времени файла в том виде, в каком её печатает `ls`.
///
/// Секунд здесь нет: в списке файлов они занимают три знака и не значат
/// ничего. Ноль — не 1970 год, а «время неизвестно», и печатается прочерком:
/// так помечены файлы на ФС, которая времени не хранит, и файлы, созданные
/// системой без часов.
#[must_use]
pub fn stamp_text(seconds: u32) -> alloc::string::String {
    if seconds == 0 {
        return alloc::string::String::from("               -");
    }
    let local = DateTime::from_unix(i64::from(seconds) + i64::from(offset_minutes()) * 60);
    alloc::format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.year,
        local.month,
        local.day,
        local.hour,
        local.minute
    )
}

/// Прочитать часовой пояс из `/etc/system.cfg`.
///
/// Вызывается один раз, после монтирования корня. Читает мимо проверки прав по
/// той же причине, что и `/etc/passwd`: спрашивать разрешение пока не у кого —
/// личность сеанса ещё не установлена.
///
/// Отсутствие файла — обычное дело, а не отказ: на системе, загруженной с
/// установочного носителя, корень это образ initrd, и настроек там нет.
pub fn adopt_timezone() {
    let Some(Ok((bytes, _))) = fs::read(CONFIG, CONFIG_LIMIT) else {
        return;
    };
    let text = core::str::from_utf8(&bytes).unwrap_or("");
    let Some(minutes) = text.lines().filter_map(parse_timezone).next() else {
        return;
    };

    OFFSET_MINUTES.store(minutes, Ordering::Relaxed);
    kprintln!("  timezone    : UTC{} from {CONFIG}", offset_text());
}

/// Разобрать строку `timezone=UTC+03:00`.
///
/// Формат пишет `crates/installer/src/ui.rs`, и разбирается он терпимо: строка
/// пришла с носителя, поэтому непонятная строка пропускается, а не роняет
/// загрузку. Минуты в записи есть всегда, но читаются честно — часовые пояса с
/// получасовым сдвигом существуют, и когда установщик научится их предлагать,
/// это место менять не придётся.
fn parse_timezone(line: &str) -> Option<i32> {
    let value = line.trim().strip_prefix("timezone=")?.trim();
    let rest = value.strip_prefix("UTC").unwrap_or(value);
    let (sign, rest) = match rest.as_bytes().first()? {
        b'+' => (1, &rest[1..]),
        b'-' => (-1, &rest[1..]),
        _ => (1, rest),
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None => (rest, "0"),
    };
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}
