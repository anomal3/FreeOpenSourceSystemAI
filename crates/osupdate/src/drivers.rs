//! Каталог драйверов: какой пакет обслуживает устройство, которого ядро не
//! знает (веха «драйверы по VID:PID», часть Д4).
//!
//! # Что лежит на сервере
//!
//! ```text
//!   drivers      — текст: какое устройство каким пакетом, для какой архитектуры
//!   drivers.sig  — подпись каталога, одной строкой, как у index.sig
//!   <пакет>-<версия>-<арх>.fpk — сами пакеты-драйверы
//! ```
//!
//! Рядом с индексом обновлений, в том же каталоге репозитория, а не
//! подкаталогом: у ассетов релиза на GitHub путей нет, только имена, и
//! подкаталог закрыл бы каталогу запасной канал.
//!
//! # Почему отдельный файл, а не записи в индексе обновлений
//!
//! Индекс говорит «вот какая система предлагается сейчас», по записи на
//! архитектуру, и его читает `sysupdate check` на каждой машине. Драйверов
//! будет много больше, чем архитектур, и нужны они машине, у которой есть
//! соответствующее устройство, — то есть почти никому из тех, кто спрашивает
//! про обновление. Два вопроса, два файла.
//!
//! # Подпись и её приставка
//!
//! Подписан весь файл, Ed25519 по SHA-256 с приставкой [`DOMAIN`], ключами из
//! `/os-keys`. Приставка своя, не как у индекса: иначе подпись индекса
//! обновлений годилась бы как подпись каталога драйверов, если бы кто-то
//! выложил индекс под именем `drivers`. Оба разбора такой файл отвергли бы по
//! формату, но держаться это должно на подписи, а не на везении разбора.
//!
//! Подпись каталога отвечает на вопрос «какой файл качать». Подпись самого
//! пакета проверяет `pkg` при установке: право `devices` без подписи ключом
//! системы он не даёт (Д2). Каталог, подменённый по дороге, не сойдётся с
//! подписью; пакет, подменённый на сервере, не сойдётся с SHA-256 из
//! подписанного каталога, а собранный заново — с подписью пакета.

use crate::from_hex;

/// Приставка, отделяющая подпись каталога от подписи чего угодно другого.
pub const DOMAIN: &[u8] = b"freeos driver catalogue v1\n";

/// Версия формата, которую понимает этот разбор.
pub const FORMAT: u32 = 1;

/// Сколько байт каталога имеет смысл читать.
///
/// Столько же, сколько индекса: запись — около двухсот байт, восемьдесят
/// записей хватит надолго. Предел назван потому, что файл приходит из сети.
pub const LIMIT: usize = crate::index::LIMIT;

/// Имя файла каталога в репозитории.
pub const FILE: &str = "drivers";

/// Имя файла подписи каталога в репозитории.
pub const SIGNATURE_FILE: &str = "drivers.sig";

/// Почему каталог не разобрался или нужной записи в нём нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Первой значащей строкой обязана быть `format=<число>`.
    NoFormat,
    /// Каталог новее, чем эта система.
    Format(u32),
    /// Для этого устройства и этой архитектуры драйвера в каталоге нет.
    NoDriver,
    /// Запись нашлась, но в ней не хватает поля или поле не разбирается.
    Field(&'static str),
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NoFormat => "this is not a driver catalogue (no format= line)",
            Self::Format(_) => "the driver catalogue is in a newer format than this system understands",
            Self::NoDriver => "the driver catalogue has nothing for this device on this architecture",
            Self::Field(name) => name,
        }
    }
}

/// Одна запись каталога: пакет, который обслуживает устройство.
#[derive(Debug, Clone, Copy)]
pub struct Driver<'a> {
    /// Имя пакета — как в его манифесте.
    pub package: &'a str,
    pub version: &'a str,
    pub arch: &'a str,
    /// Имя файла на сервере, рядом с каталогом. Только имя, без пути — по той
    /// же причине, что у индекса (см. [`crate::Image::file`]).
    pub file: &'a str,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Разобранный каталог: ссылка на текст, записи — срезы поверх него.
#[derive(Debug, Clone, Copy)]
pub struct Catalogue<'a> {
    text: &'a str,
}

impl<'a> Catalogue<'a> {
    /// Проверить заголовок и приготовиться к поиску.
    pub fn parse(text: &'a str) -> Result<Self, Error> {
        match crate::format_of(text) {
            Some(FORMAT) => Ok(Self { text }),
            Some(other) => Err(Error::Format(other)),
            None => Err(Error::NoFormat),
        }
    }

    /// Найти пакет для устройства `vendor:device` и архитектуры.
    ///
    /// Первую подходящую запись: на пару «устройство, архитектура» каталог
    /// обязан предлагать один пакет, и выбор из нескольких означал бы, что
    /// сервер предлагает машине решить, что ей ставить.
    pub fn find(&self, vendor: u16, device: u16, arch: &str) -> Result<Driver<'a>, Error> {
        let mut record: Option<Partial<'a>> = None;
        for line in self.text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == RECORD {
                if let Some(partial) = record.take() {
                    if partial.serves(vendor, device, arch) {
                        return partial.finish();
                    }
                }
                record = Some(Partial::default());
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let Some(partial) = record.as_mut() else {
                // Поля до первой `[driver]` — заголовок файла (`format=`).
                continue;
            };
            match key {
                "drives" => partial.drives = Some(value),
                "arch" => partial.arch = Some(value),
                "package" => partial.package = Some(value),
                "version" => partial.version = Some(value),
                "file" => partial.file = Some(value),
                "size" => partial.size = value.parse::<u64>().ok(),
                "sha256" => partial.sha256 = from_hex::<32>(value),
                // Неизвестное поле — не ошибка, как и в индексе: каталог пишет
                // более новая сборка, и она вправе сказать больше.
                _ => {}
            }
        }
        match record {
            Some(partial) if partial.serves(vendor, device, arch) => partial.finish(),
            _ => Err(Error::NoDriver),
        }
    }
}

/// Строка, с которой начинается запись.
const RECORD: &str = "[driver]";

#[derive(Default)]
struct Partial<'a> {
    drives: Option<&'a str>,
    arch: Option<&'a str>,
    package: Option<&'a str>,
    version: Option<&'a str>,
    file: Option<&'a str>,
    size: Option<u64>,
    sha256: Option<[u8; 32]>,
}

impl<'a> Partial<'a> {
    /// Та ли это запись. Разбор `vendor:device` — общий с манифестом пакета
    /// (`fpk::parse_drive`): разойдись они, каталог называл бы одно, а
    /// установленный пакет — другое.
    fn serves(&self, vendor: u16, device: u16, arch: &str) -> bool {
        self.arch == Some(arch)
            && self
                .drives
                .unwrap_or("")
                .split_whitespace()
                .any(|drive| fpk::parse_drive(drive) == Some((vendor, device)))
    }

    fn finish(self) -> Result<Driver<'a>, Error> {
        let package = self.package.ok_or(Error::Field("the catalogue entry has no package"))?;
        let version = self.version.ok_or(Error::Field("the catalogue entry has no version"))?;
        let arch = self.arch.ok_or(Error::Field("the catalogue entry has no arch"))?;
        let file = self.file.ok_or(Error::Field("the catalogue entry has no file name"))?;
        let size = self.size.ok_or(Error::Field("the catalogue entry has no usable size"))?;
        let sha256 = self.sha256.ok_or(Error::Field("the catalogue entry has no usable sha256"))?;
        if file.is_empty() || file.contains('/') || file.contains('\\') || file.starts_with('.') {
            // Имя приходит из сети и становится запросом к серверу; разделитель
            // в нём означал бы, что каталог волен назвать любой файл.
            return Err(Error::Field("the catalogue names a file with a path in it"));
        }
        Ok(Driver { package, version, arch, file, size, sha256 })
    }
}

/// Хеш, который подписывается и проверяется.
#[must_use]
pub fn digest(catalogue: &[u8]) -> [u8; 32] {
    let mut hasher = fpk::Hasher::new();
    hasher.update(DOMAIN);
    hasher.update(catalogue);
    hasher.finish()
}

/// Сборка каталога — то, что делает машина, выкладывающая репозиторий.
#[cfg(feature = "build")]
pub mod build {
    use alloc::string::String;

    use crate::to_hex;

    /// Один пакет-драйвер, который надо предложить.
    pub struct Offer<'a> {
        /// Устройства через пробел, `vendor:device` — как поле `drives`
        /// манифеста пакета, из которого запись и составляется.
        pub drives: &'a str,
        pub arch: &'a str,
        pub package: &'a str,
        pub version: &'a str,
        pub file: &'a str,
        pub size: u64,
        pub sha256: [u8; 32],
    }

    /// Составить текст каталога. Хозяин формата один — этот крейт.
    #[must_use]
    pub fn render(offers: &[Offer<'_>]) -> String {
        let mut out = String::new();
        out.push_str("# FreeOS driver catalogue: which package drives a device the kernel does not know.\n");
        out.push_str("# Signed by drivers.sig; the signature covers this file byte for byte.\n");
        out.push_str("# One [driver] record per package and architecture.\n");
        out.push_str(&alloc::format!("format={}\n", super::FORMAT));
        for offer in offers {
            out.push_str("\n[driver]\n");
            out.push_str(&alloc::format!("drives={}\n", offer.drives));
            out.push_str(&alloc::format!("arch={}\n", offer.arch));
            out.push_str(&alloc::format!("package={}\n", offer.package));
            out.push_str(&alloc::format!("version={}\n", offer.version));
            out.push_str(&alloc::format!("file={}\n", offer.file));
            out.push_str(&alloc::format!("size={}\n", offer.size));
            out.push_str(&alloc::format!("sha256={}\n", to_hex(&offer.sha256)));
        }
        out
    }

    /// Составить текст `drivers.sig`. Формат строки тот же, что у `index.sig`,
    /// и читает его тот же [`crate::index::parse_signature`].
    #[must_use]
    pub fn render_signature(signature: &[u8; 64]) -> String {
        let mut out = String::new();
        out.push_str("# Signature of the driver catalogue next to this file, Ed25519 over SHA-256 of it.\n");
        out.push_str(&alloc::format!("{} {}\n", crate::ALGORITHM, to_hex(signature)));
        out
    }
}
