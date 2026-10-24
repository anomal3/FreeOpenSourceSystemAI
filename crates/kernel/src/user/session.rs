//! От чьего имени работает система.
//!
//! # Откуда берётся личность
//!
//! Из `/etc/passwd`, который написал установщик. Ничего похожего на вход в
//! систему здесь нет: пароль не спрашивается, сменить пользователя нечем.
//! Сказать об этом прямо важнее, чем изобразить обратное, — файл учётных
//! записей несёт отпечаток пароля, и проверять его будет тот, кто умеет
//! спросить пароль, не показав его на экране. Такого кода в системе пока нет.
//!
//! Что здесь есть — **на кого записаны действия программ**. Этого достаточно,
//! чтобы права перестали быть числами в выводе `ls`: программа, запущенная с
//! `uid 1000`, получает отказ на чужом файле, и отказ этот выдаёт блок проверки
//! в ядре, а не добрая воля программы.
//!
//! # Когда личность — root
//!
//! Когда `/etc/passwd` нет: система загружена с установочного носителя, корень
//! — образ initrd, учётных записей на нём не существует. Тогда сеанс идёт от
//! имени `root`, и проверки прав никому ни в чём не отказывают. Это состояние
//! печатается при загрузке, потому что «проверки есть, но сегодня они молчат»
//! обязано быть видно, а не подразумеваться.

use crate::config;
use crate::sync::SpinLock;
use crate::vfs::perm::Credentials;
use crate::kprintln;

/// Имя файла учётных записей.
///
/// Ищется он не только в `/etc`: с фазы 39 настройка берётся сначала оттуда, а
/// потом из эталона образа — см. [`crate::config`]. Эталонного `passwd` система
/// не выпускает намеренно (учётная запись — это не умолчание, а решение о
/// машине), но путь чтения один на все настройки: второй, «особенный», рано или
/// поздно разошёлся бы с первым.
const PASSWD: &str = "passwd";

/// Сколько байт файла ядро согласно прочитать.
const PASSWD_LIMIT: usize = 8 * 1024;

/// Имя пользователя сеанса. Пустое, пока сеанс идёт от root.
///
/// Хранится отдельно от [`Credentials`], потому что права выражаются числами, а
/// человеку нужно имя. Длина ограничена: строка приходит с носителя, то есть
/// из-за границы доверия, и класть её в кучу целиком незачем.
const MAX_NAME: usize = 32;

/// Кто мы, в терминах, которые понимает файловая система.
static CREDENTIALS: SpinLock<Credentials> = SpinLock::new(Credentials::ROOT);

/// Имя и его длина — `heapless`-строка на стеке статика.
static NAME: SpinLock<([u8; MAX_NAME], usize)> = SpinLock::new(([0; MAX_NAME], 0));

/// Права, с которыми исполняется всё, что запускает система.
#[must_use]
pub fn credentials() -> Credentials {
    *CREDENTIALS.lock()
}

/// Выполнить что-нибудь с именем пользователя.
///
/// Замыкание, а не `String`: имя лежит в статике, копировать его в кучу ради
/// одной печати незачем.
pub fn with_name<R>(f: impl FnOnce(&str) -> R) -> R {
    let guard = NAME.lock();
    let (bytes, len) = &*guard;
    // Имя проверено на ASCII при разборе, поэтому срез — валидный UTF-8.
    f(core::str::from_utf8(&bytes[..*len]).unwrap_or("?"))
}

/// Прочитать `/etc/passwd` и запомнить, от чьего имени работать дальше.
///
/// Вызывается один раз, сразу после того, как корневая ФС смонтирована. Читает
/// **мимо проверки прав** — и обязан: проверять нечем, пока не известно, кто
/// спрашивает, а `/etc/passwd` с правами `0640` и владельцем root иначе не
/// прочитал бы никто.
pub fn adopt_account() {
    let Some((bytes, source)) = config::read(PASSWD, PASSWD_LIMIT) else {
        kprintln!("  session     : root, no account file to read");
        kprintln!("  session     : permission checks will deny nothing");
        return;
    };
    let from = config::path(PASSWD, source);

    let text = core::str::from_utf8(&bytes).unwrap_or("");
    let Some(account) = text.lines().filter_map(parse_line).next() else {
        kprintln!("  session     : root, {from} has no usable account line");
        kprintln!("  session     : permission checks will deny nothing");
        return;
    };

    *CREDENTIALS.lock() = Credentials::new(account.uid, account.gid);
    {
        let mut guard = NAME.lock();
        let (bytes, len) = &mut *guard;
        *len = account.name.len().min(MAX_NAME);
        bytes[..*len].copy_from_slice(&account.name.as_bytes()[..*len]);
    }

    kprintln!(
        "  session     : {} (uid {}, gid {}) from {from}",
        account.name,
        account.uid,
        account.gid
    );
}

/// Разобранная строка учётной записи — только то, что нужно правам.
struct Account<'a> {
    name: &'a str,
    uid: u32,
    gid: u32,
}

/// Разобрать строку `name:uid:gid:mode:home:algorithm:salt:digest`.
///
/// Формат описан в `crates/installer/src/account.rs`, и здесь читаются первые
/// три поля: остальные — про пароль, а пароль спрашивать некому. Строка пришла
/// с носителя, поэтому любое поле может оказаться любым: непонятная строка
/// пропускается, а не роняет загрузку.
fn parse_line(line: &str) -> Option<Account<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split(':');
    let name = fields.next()?;
    let uid = fields.next()?.parse::<u32>().ok()?;
    let gid = fields.next()?.parse::<u32>().ok()?;

    // Имя попадает в вывод ядра, то есть на серийную линию и в окно оболочки.
    // Управляющие байты испортили бы и то и другое, а имя с ними — верный
    // признак того, что файл читается не как файл учётных записей.
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }
    // Учётная запись root в файле есть у всех Unix, но принимать её здесь
    // значило бы «войти» суперпользователем на системе, где входа нет вовсе.
    if uid == 0 {
        return None;
    }

    Some(Account { name, uid, gid })
}

/// Проверить пароль того, от чьего имени идёт сеанс (экран блокировки, фаза
/// 72h-4). `None` — пароля у учётной записи нет (живая система, `root`) или
/// файла нет: тогда спрашивать нечего.
///
/// Отпечаток — тот, что пишет установщик (`crates/installer/src/account.rs`):
/// соль и многократный FNV-1a, алгоритм в строке, `fnv1a64-<раунды>`. Это не
/// KDF и от подбора не защищает; экран блокировки защищает ровно от того,
/// от чего защищает отпечаток, — от случайного прохожего.
#[must_use]
pub fn verify_password(password: &str) -> Option<bool> {
    let (bytes, _) = config::read(PASSWD, PASSWD_LIMIT)?;
    let text = core::str::from_utf8(&bytes).ok()?;
    let name = with_name(|name| alloc::string::String::from(name));
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let fields: alloc::vec::Vec<&str> = line.split(':').collect();
        if fields.len() < 8 || fields[0] != name {
            continue;
        }
        let rounds = fields[5].strip_prefix("fnv1a64-")?.parse::<u32>().ok()?;
        let salt = u64::from_str_radix(fields[6], 16).ok()?;
        let digest = u64::from_str_radix(fields[7], 16).ok()?;
        return Some(derive(password, salt, rounds) == digest);
    }
    None
}

/// Многократный прогон хеша — тот же, что у установщика.
fn derive(password: &str, salt: u64, rounds: u32) -> u64 {
    let mut state = salt;
    for round in 0..rounds {
        state = mix(state ^ hash_bytes(password.as_bytes()));
        state = mix(state ^ u64::from(round));
    }
    state
}

/// FNV-1a, 64 бита.
fn hash_bytes(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Финализатор SplitMix64.
const fn mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
