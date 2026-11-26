//! Сетевые настройки, которые переживают выключение.
//!
//! # Чего не хватало до этой фазы
//!
//! Адрес существовал только в памяти. `ip 10.0.2.15/24 10.0.2.2` работал ровно
//! до перезагрузки, а после машина снова ждала DHCP — то есть постоянного
//! адреса в системе не было вовсе, хотя команда для его задания была. Это
//! обычная беда полусделанной настройки: она выглядит работающей ровно до тех
//! пор, пока никто не выключил машину.
//!
//! # Почему файл, а не переменная в образе
//!
//! `/etc` живёт на разделе состояния, и обновление системы до него не
//! дотягивается (см. [`crate::config`]). Адрес машины — это ровно то, что
//! обязано пережить смену версии: сеть, отвалившаяся после обновления, чинится
//! только с клавиатуры у самой машины, а у машины без монитора — никак.
//!
//! # Почему `mode=`, а не «есть файл — значит статика»
//!
//! Потому что «получать по DHCP» — это тоже выбор человека, а не отсутствие
//! выбора. Различить их надо: у машины, где файла нет, DHCP работает по
//! умолчанию; у машины, где написано `mode=dhcp`, он работает потому, что так
//! решили. Разница видна в журнале, и она отвечает на вопрос «кто это включил».

use alloc::format;
use alloc::string::String;

use crate::config;
use crate::net::ipv4::Ipv4;

/// Имя файла настроек — в `/etc` и в эталоне.
pub const CONFIG: &str = "network.cfg";

/// Сколько байт читать. Файл — десяток строк; предел на случай, если это не он.
const LIMIT: usize = 4 * 1024;

/// Что человек выбрал.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Адрес приносит служба DHCP.
    Dhcp,
    /// Адрес задан здесь.
    Static,
}

/// Разобранные настройки.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub mode: Mode,
    pub address: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns: Ipv4,
}

impl Settings {
    /// Настройки по умолчанию: адрес спрашиваем у сети.
    #[must_use]
    pub const fn dhcp() -> Self {
        Self {
            mode: Mode::Dhcp,
            address: Ipv4::UNSPECIFIED,
            netmask: Ipv4::UNSPECIFIED,
            gateway: Ipv4::UNSPECIFIED,
            dns: Ipv4::UNSPECIFIED,
        }
    }
}

/// Разобрать текст настроек.
///
/// Чтение ключей — из `sysconf` (там же его тесты); здесь остаётся то, что
/// требует знать адреса. Разбор терпимый: файл лежит на носителе и его правят
/// руками, поэтому непонятное значение читается как «не задано», а не роняет
/// загрузку. То же рассуждение, что у часового пояса.
#[must_use]
pub fn parse(text: &str) -> Settings {
    let address = |key| {
        sysconf::value(text, key)
            .and_then(Ipv4::parse)
            .unwrap_or(Ipv4::UNSPECIFIED)
    };
    let out = Settings {
        mode: if sysconf::network_is_static(text) {
            Mode::Static
        } else {
            Mode::Dhcp
        },
        address: address("address"),
        netmask: address("netmask"),
        gateway: address("gateway"),
        dns: address("dns"),
    };
    // Статика без адреса — это не статика, а испорченный файл. Считать её
    // статикой значило бы оставить машину без адреса **и** без DHCP разом, то
    // есть без сети вовсе — отказ, который выглядит как поломка драйвера.
    if out.mode == Mode::Static && out.address.is_unspecified() {
        return Settings::dhcp();
    }
    out
}

/// Прочитать старый файл `/etc/network.cfg`. `None` — файла нет ни в `/etc`,
/// ни в эталоне. Since 25.11 only a fallback of [`super::adapter::load`].
#[must_use]
pub fn load_file() -> Option<Settings> {
    let (bytes, _) = config::read(CONFIG, LIMIT)?;
    let text = core::str::from_utf8(&bytes).ok()?;
    Some(parse(text))
}

/// Remove the old file (the settings store speaks for the card now).
pub fn forget_file() -> Result<bool, crate::vfs::VfsError> {
    config::reset(CONFIG)
}

/// The card's IPv4 settings (25.11: from the settings store, see
/// [`super::adapter`]). `None` — no card.
#[must_use]
pub fn load() -> Option<Settings> {
    let status = crate::net::status()?;
    let saved = super::adapter::load(status.mac);
    Some(Settings {
        mode: if saved.dhcp { Mode::Dhcp } else { Mode::Static },
        address: saved.address,
        netmask: saved.netmask,
        gateway: saved.gateway,
        dns: saved.dns1,
    })
}

/// A static address from «Параметры»: applied at once (DHCP stands down) and
/// kept in the settings store; a DNS server given is kept as set by hand.
pub fn store(settings: &Settings) -> Result<(), String> {
    let gateway = if settings.gateway.is_unspecified() { String::from("-") } else { format!("{}", settings.gateway) };
    let line = format!("ipv4 static {} {} {gateway}", settings.address, settings.netmask);
    let answer = super::adapter::command(&line);
    if let Some(why) = answer.iter().find_map(|line| line.strip_prefix("error ")) {
        return Err(String::from(why));
    }
    if !settings.dns.is_unspecified() {
        let answer = super::adapter::command(&format!("dns {}", settings.dns));
        if let Some(why) = answer.iter().find_map(|line| line.strip_prefix("error ")) {
            return Err(String::from(why));
        }
    }
    Ok(())
}

/// Back to DHCP — at once. `Ok(false)` — it was DHCP already.
pub fn forget() -> Result<bool, String> {
    let was_static = load().is_some_and(|settings| settings.mode == Mode::Static);
    let answer = super::adapter::command("ipv4 dhcp");
    if let Some(why) = answer.iter().find_map(|line| line.strip_prefix("error ")) {
        return Err(String::from(why));
    }
    let _ = super::adapter::command("dns auto");
    Ok(was_static)
}

/// Применить постоянные настройки, если они есть (25.11: per card, from the
/// settings store — [`super::adapter::adopt`]).
///
/// Зовётся один раз, после того как поднялась сетевая карта, и **до** запуска
/// служб: `/bin/dhcp` смотрит на флаги `NetInfo` и, увидев ручной адрес,
/// ждёт. Порядок обязателен — иначе аренда, приехавшая на секунду позже,
/// перезаписала бы то, что человек задал сам.
pub fn adopt() {
    super::adapter::adopt();
}

// Тестов здесь нет намеренно: ядро под хост не собирается, и `#[cfg(test)]`
// в нём никогда не запустится. Разбор ключей проверен в `sysconf`, а то, что
// остаётся здесь, — три вызова `Ipv4::parse` и одно правило про статику без
// адреса — проверяется сценарием стенда `settings`.
