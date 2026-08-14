//! `init` — супервизор служб: то, что поднимает упавшее обратно.
//!
//! # Зачем он вообще
//!
//! Ради раздела «Идеология» в дорожной карте: сбой службы не должен ронять
//! систему. Чтобы это стало правдой, нужны две вещи, и обе появились в этой
//! фазе. Первая — служба обязана жить **вне ядра**, то есть быть программой:
//! упавший DHCP-клиент внутри ядра — это мёртвая машина, а упавшая программа —
//! это завершившаяся программа. Вторая — кто-то должен поднимать её обратно, и
//! это он.
//!
//! Стоит он **перед** сетью намеренно: первая же служба, которую мы напишем,
//! появится уже в мире, где падение перезапускается, а не в ядре, откуда её
//! потом пришлось бы выковыривать.
//!
//! # Почему перезапуск с задержкой и с пределом
//!
//! Служба, падающая мгновенно, без задержки съела бы машину: цикл
//! «запустить — упасть — запустить» крутится со скоростью планировщика.
//! Задержка делает его дешёвым.
//!
//! Предел важнее задержки. Бесконечный перезапуск сломанной службы — это не
//! устойчивость, а сокрытие поломки: снаружи система выглядит работающей, в
//! журнале растёт одна и та же строка, и никто не узнает, что службы нет уже
//! час. Поэтому после [`LIMIT`] подряд неудач служба останавливается, и об этом
//! говорится один раз и внятно.
//!
//! Счётчик сбрасывается, когда служба **прожила** дольше [`STABLE_MS`]: три
//! падения за минуту и три падения за три месяца — разные события, и считать их
//! одинаково значило бы однажды остановить исправную службу.
//!
//! # От чьего имени работает служба
//!
//! От того, что записано в её описании, а не от того, кто её запустил. Права
//! запуска — свойство службы: супервизор исполняется от root, потому что иначе
//! он не смог бы запустить ничего, кроме своего, — и ровно поэтому наследование
//! его прав сделало бы описание украшением. Ядро проверяет это со своей стороны:
//! понизить права вправе кто угодно, повысить — никто.
//!
//! Строка **без** полей `uid`/`gid` — не «от root», а «от того же, что и
//! супервизор». Разница видна ровно там, где супервизор запущен обычным
//! пользователем: подставленный root дал бы отказ в правах на строке, где о
//! правах не сказано ни слова.
//!
//! # Чего здесь нет
//!
//! Зависимостей между службами, сокетов, групп и целей. Всё это осмысленно там,
//! где служб десятки; здесь их единицы, а порядок запуска ни на что не влияет,
//! потому что ни одна не ждёт другую.

#![no_std]
#![no_main]

use user_progs::{
    Args, close, error, error_num, exit, open, read, sleep_ms, spawn, spawn_as, uptime_ms,
    wait_now,
};
use user_abi::ERR_AGAIN;

/// Откуда берутся описания служб, если не сказано иначе.
const SERVICES: &str = "/etc/services";

/// Сколько служб супервизор согласен вести.
///
/// Массив на стеке, а не список в куче: кучи у программы нет. Восемь — это
/// заведомо больше, чем есть служб в системе без сети, и предел назван вслух,
/// потому что девятая строка файла молча не пропадёт (о ней будет сказано).
const MAX_SERVICES: usize = 8;

/// Предел длины имени службы и пути к ней.
const MAX_NAME: usize = 24;
const MAX_EXEC: usize = 96;

/// Сколько ждать перед перезапуском.
///
/// Полсекунды — достаточно, чтобы цикл «падает при каждом старте» не занимал
/// машину, и мало, чтобы человек не заметил перерыва в работе исправной службы,
/// которую сняли.
const BACKOFF_MS: u64 = 500;

/// Сколько служба должна прожить, чтобы счётчик неудач обнулился.
const STABLE_MS: u64 = 10_000;

/// Сколько неудач подряд считается поломкой.
const LIMIT: u32 = 3;

/// Как часто супервизор осматривает своих.
///
/// Опрос, а не ожидание: ждать пришлось бы на **одной** службе, и падение
/// второй осталось бы незамеченным до тех пор, пока не упадёт первая. Почему у
/// ядра нет «дождаться любой» — сказано в договоре у `SYS_WAIT`.
const POLL_MS: u64 = 200;

/// Описание одной службы и всё, что супервизор о ней помнит.
struct Service {
    name: [u8; MAX_NAME],
    name_len: usize,
    exec: [u8; MAX_EXEC],
    exec_len: usize,
    uid: u32,
    gid: u32,
    /// Запускать от того же имени, что и супервизор.
    ///
    /// Так понимается описание **без** полей `uid`/`gid`. Это не поблажка: в
    /// строке, где их нет, ничего не сказано о правах, и подставлять туда root
    /// значило бы читать в файле то, чего в нём не написано, — а супервизор,
    /// запущенный обычным пользователем, получил бы на этом отказ, которого
    /// никто не заказывал.
    inherit: bool,
    /// Номер задачи, пока служба работает. Ноль — не работает.
    task: i64,
    /// Когда её запустили: по этому времени решается, была ли она устойчивой.
    started_at: u64,
    /// Сколько раз подряд она кончалась, не прожив [`STABLE_MS`].
    failures: u32,
    /// Не раньше этого времени пробовать снова. Ноль — прямо сейчас.
    retry_at: u64,
    /// Перестали ли пробовать вовсе.
    stopped: bool,
}

impl Service {
    const fn empty() -> Self {
        Self {
            name: [0; MAX_NAME],
            name_len: 0,
            exec: [0; MAX_EXEC],
            exec_len: 0,
            uid: 0,
            gid: 0,
            inherit: true,
            task: 0,
            started_at: 0,
            failures: 0,
            retry_at: 0,
            stopped: false,
        }
    }

    fn name(&self) -> &str {
        // SAFETY: в буфер попадают только байты из `&str`.
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len]) }
    }

    fn exec(&self) -> &str {
        // SAFETY: см. выше.
        unsafe { core::str::from_utf8_unchecked(&self.exec[..self.exec_len]) }
    }
}

/// Буфер под файл описаний.
static mut CONFIG: [u8; 4096] = [0; 4096];

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let path = args.get(1).unwrap_or(SERVICES);

    let mut services = [const { Service::empty() }; MAX_SERVICES];
    let count = match load(path, &mut services) {
        Ok(count) => count,
        Err(()) => exit(1),
    };
    if count == 0 {
        log("init: no services described in ");
        log(path);
        log("\n");
        exit(0);
    }

    log("init: supervising ");
    error_num(count as i64);
    log(" service(s) from ");
    log(path);
    log("\n");

    supervise(&mut services[..count]);

    // Возвращаться из супервизора незачем, пока есть за кем следить; сюда
    // приходят только тогда, когда следить стало не за кем.
    log("init: nothing left to supervise, finishing\n");
    exit(0)
}

/// Главный цикл: запустить недостающих, похоронить упавших, поспать.
fn supervise(services: &mut [Service]) {
    loop {
        let now = uptime_ms();
        let mut live = 0usize;

        for service in services.iter_mut() {
            if service.task != 0 {
                if reap(service, now) {
                    live += 1;
                }
                continue;
            }
            if service.stopped {
                continue;
            }
            if now < service.retry_at {
                // Ещё рано — но служба жива в смысле «за ней ещё следят».
                live += 1;
                continue;
            }
            start(service, now);
            if !service.stopped {
                live += 1;
            }
        }

        if live == 0 {
            return;
        }
        sleep_ms(POLL_MS);
    }
}

/// Запустить службу и запомнить, когда это случилось.
fn start(service: &mut Service, now: u64) {
    let task = if service.inherit {
        spawn(service.exec())
    } else {
        spawn_as(service.exec(), service.uid, service.gid)
    };
    if task < 0 {
        service.failures += 1;
        log("init: cannot start '");
        log(service.name());
        log("': error ");
        error_num(task);
        log("\n");
        if service.failures >= LIMIT {
            give_up(service);
        } else {
            service.retry_at = now + BACKOFF_MS;
        }
        return;
    }

    service.task = task;
    service.started_at = now;
    log("init: started '");
    log(service.name());
    log("' as #");
    error_num(task);
    log("\n");
}

/// Проверить, не кончилась ли служба. `true` — ещё есть за кем следить.
fn reap(service: &mut Service, now: u64) -> bool {
    let code = wait_now(service.task);
    if code == ERR_AGAIN {
        return true;
    }

    let lived = now.saturating_sub(service.started_at);
    service.task = 0;

    // Прожившая долго считается исправной, чем бы она ни кончилась: счётчик
    // неудач меряет **повторяющуюся** поломку, а не всякий конец.
    if lived >= STABLE_MS {
        service.failures = 0;
    }
    service.failures += 1;

    log("init: '");
    log(service.name());
    log("' ended with code ");
    error_num(code);
    log(" after ");
    error_num(lived as i64);
    log(" ms\n");

    if service.failures >= LIMIT {
        give_up(service);
        return false;
    }

    service.retry_at = now + BACKOFF_MS;
    log("init: restarting '");
    log(service.name());
    log("' in ");
    error_num(BACKOFF_MS as i64);
    log(" ms\n");
    true
}

/// Перестать пробовать — и сказать об этом один раз и внятно.
fn give_up(service: &mut Service) {
    service.stopped = true;
    log("init: '");
    log(service.name());
    log("' failed ");
    error_num(i64::from(service.failures));
    log(" time(s) in a row, giving up\n");
}

/// Прочитать файл описаний.
///
/// Возвращает, сколько служб разобрано. Непонятная строка не роняет разбор:
/// файл лежит на диске, то есть за границей доверия, и одна испорченная строка
/// не должна означать систему без единой службы. О каждой такой строке
/// говорится вслух — молчаливо пропущенная служба хуже отсутствующей.
fn load(path: &str, services: &mut [Service; MAX_SERVICES]) -> Result<usize, ()> {
    let fd = open(path);
    if fd < 0 {
        log("init: cannot open ");
        log(path);
        log("\n");
        return Err(());
    }

    // SAFETY: программа однопоточна, буфер не используется больше нигде.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(CONFIG) };
    let mut filled = 0usize;
    while filled < buffer.len() {
        let got = read(fd, &mut buffer[filled..]);
        if got <= 0 {
            break;
        }
        filled += got as usize;
    }
    close(fd);

    let Ok(text) = core::str::from_utf8(&buffer[..filled]) else {
        log("init: the services file is not valid UTF-8\n");
        return Err(());
    };

    let mut count = 0usize;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if count == MAX_SERVICES {
            log("init: too many services, the rest of the file is ignored\n");
            break;
        }
        match parse(line, &mut services[count]) {
            true => count += 1,
            false => {
                log("init: cannot parse this line, skipping it: ");
                log(line);
                log("\n");
            }
        }
    }

    Ok(count)
}

/// Разобрать строку `<имя> <путь> [uid] [gid]`.
///
/// Порядок полей — от того, что чаще читают глазом, к тому, что реже правят.
/// `uid` и `gid` необязательны: без них служба идёт от имени супервизора (см.
/// [`Service::inherit`]).
///
/// Предела перезапусков в строке нет: он один на все службы ([`LIMIT`]), и
/// поле, которое во всех строках одинаково, — это не настройка, а шум.
fn parse(line: &str, service: &mut Service) -> bool {
    let mut fields = line.split_whitespace();
    let (Some(name), Some(exec)) = (fields.next(), fields.next()) else {
        return false;
    };
    // Путь обязан быть абсолютным: относительного «текущего каталога» в этой
    // системе не существует, и `bin/svclog` означал бы не то, что подумал
    // человек, а ничего.
    if name.len() > MAX_NAME || exec.len() > MAX_EXEC || !exec.starts_with('/') {
        return false;
    }
    let (uid, inherit) = match fields.next() {
        Some(text) => match text.parse::<u32>() {
            Ok(uid) => (uid, false),
            Err(_) => return false,
        },
        None => (0, true),
    };
    let gid = match fields.next() {
        Some(text) => match text.parse::<u32>() {
            Ok(gid) => gid,
            Err(_) => return false,
        },
        None => uid,
    };

    service.name[..name.len()].copy_from_slice(name.as_bytes());
    service.name_len = name.len();
    service.exec[..exec.len()].copy_from_slice(exec.as_bytes());
    service.exec_len = exec.len();
    service.uid = uid;
    service.gid = gid;
    service.inherit = inherit;
    true
}

/// Всё, что говорит супервизор, уходит в журнал, а не в окно.
///
/// Причина та же, по которой туда пишут службы: супервизор работает всё время
/// работы системы, в том числе тогда, когда окна оболочки нет вовсе, — а
/// проверяется он снаружи, по серийной линии.
fn log(text: &str) {
    error(text);
}
