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

use crate::{config, kprintln};
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

    /// Длина префикса маски. `None` — маска дырявая либо не задана.
    #[must_use]
    pub fn prefix(&self) -> Option<u32> {
        self.netmask.prefix()
    }

    /// Записать в том виде, в каком файл читается обратно.
    #[must_use]
    pub fn to_text(&self) -> String {
        match self.mode {
            Mode::Dhcp => String::from("mode=dhcp\n"),
            Mode::Static => {
                let mut out = String::from("mode=static\n");
                out.push_str(&format!("address={}\n", self.address));
                out.push_str(&format!("netmask={}\n", self.netmask));
                // Пустые строки пишутся, а не опускаются: файл, в котором ключ
                // отсутствует, и файл, в котором он пуст, читаются одинаково,
                // но человеку показывают разное — во втором видно, что поле
                // существует и его можно заполнить.
                if !self.gateway.is_unspecified() {
                    out.push_str(&format!("gateway={}\n", self.gateway));
                }
                if !self.dns.is_unspecified() {
                    out.push_str(&format!("dns={}\n", self.dns));
                }
                out
            }
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

/// Прочитать настройки. `None` — файла нет ни в `/etc`, ни в эталоне.
#[must_use]
pub fn load() -> Option<Settings> {
    let (bytes, _) = config::read(CONFIG, LIMIT)?;
    let text = core::str::from_utf8(&bytes).ok()?;
    Some(parse(text))
}

/// Записать настройки в `/etc/network.cfg`.
pub fn store(settings: &Settings) -> Result<(), crate::vfs::VfsError> {
    config::write(CONFIG, &settings.to_text())
}

/// Убрать файл: адрес снова спрашивается у сети.
pub fn forget() -> Result<bool, crate::vfs::VfsError> {
    config::reset(CONFIG)
}

/// Применить постоянные настройки, если они есть.
///
/// Зовётся один раз, после того как поднялась сетевая карта, и **до** запуска
/// служб: `/bin/dhcp` смотрит в этот же файл и, увидев `mode=static`,
/// отступается. Порядок обязателен — иначе аренда, приехавшая на секунду позже,
/// перезаписала бы то, что человек задал сам.
pub fn adopt() {
    let Some(settings) = load() else {
        return;
    };
    if settings.mode != Mode::Static {
        kprintln!("  network     : DHCP by {}/{CONFIG}", config::ETC);
        return;
    }
    match crate::net::configure_all(
        settings.address,
        settings.netmask,
        settings.gateway,
        settings.dns,
    ) {
        Ok(()) => kprintln!(
            "  network     : {}/{} static from {}/{CONFIG}",
            settings.address,
            settings.prefix().unwrap_or(0),
            config::ETC
        ),
        // Настройки с носителя могут быть какими угодно, и отказ здесь — не
        // повод не загрузиться. Сказать о нём надо вслух: молчание выглядело бы
        // как «адрес применён», а его нет.
        Err(err) => kprintln!("  network     : {}/{CONFIG} rejected: {err}", config::ETC),
    }
}

// Тестов здесь нет намеренно: ядро под хост не собирается, и `#[cfg(test)]`
// в нём никогда не запустится. Разбор ключей проверен в `sysconf`, а то, что
// остаётся здесь, — три вызова `Ipv4::parse` и одно правило про статику без
// адреса — проверяется сценарием стенда `settings`.
