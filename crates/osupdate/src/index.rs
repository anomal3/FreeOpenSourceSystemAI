//! Индекс репозитория обновлений: три файла на сервере и то, как их читать.
//!
//! # Что лежит на сервере
//!
//! ```text
//!   index       — текст: какие образы есть, каких версий и с каким хешем
//!   index.sig   — подпись индекса, одной строкой
//!   freeos-*.fpk — сами образы
//! ```
//!
//! # Почему текст
//!
//! Потому что разбирать его придётся и человеку. Репозиторий обновлений — это
//! место, куда смотрят, когда система говорит «новее ничего нет», а обновление
//! точно выложено; двоичный формат в этот момент означает «напишите утилиту,
//! чтобы посмотреть». Цена текста — разбор на стороне программы без кучи, и она
//! невелика: полей шесть.
//!
//! # Что подписано и зачем
//!
//! Подписан **весь файл индекса**, Ed25519 по SHA-256 от него с приставкой
//! [`DOMAIN`]. Приставка не украшение: без неё подпись индекса и подпись
//! контейнера — это подписи по одному и тому же алгоритму от одного и того же
//! хеша, и байты, годящиеся в одном месте, годились бы и в другом. С приставкой
//! эти два множества не пересекаются.
//!
//! Настоящую проверку всё равно делает ядро при `apply`, и подпись индекса её не
//! заменяет: она отвечает на другой вопрос — стоит ли вообще тащить по сети
//! десятки мегабайт и в какой раздел их писать. Подменённый индекс без неё
//! означал бы «качайте что попало и сколько попало», то есть отказ в
//! обслуживании и лишний повод ошибиться на входе разбора контейнера.
//!
//! # Чего в индексе нет
//!
//! Списка версий с историей. Индекс описывает то, что предлагается **сейчас**,
//! по одной записи на архитектуру: система, которая выбирает из истории, — это
//! система, которой можно предложить старую дырявую версию. Откат по версии
//! запрещён и в ядре, но начинать надо с того, чтобы его не предлагали.

use crate::{ALGORITHM, from_hex};

/// Приставка, отделяющая подпись индекса от подписи чего угодно другого.
pub const DOMAIN: &[u8] = b"freeos update index v1\n";

/// Версия формата, которую понимает этот разбор.
pub const FORMAT: u32 = 1;

/// Сколько байт индекса имеет смысл читать.
///
/// Шестнадцать килобайт при полусотне на запись — запас на сотни архитектур,
/// которых не будет. Предел назван потому, что файл приходит **из сети**: без
/// него сервер, отдающий бесконечный поток, съел бы всю память программы.
pub const LIMIT: usize = 16 * 1024;

/// Почему индекс не разобрался.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Первой значащей строкой обязана быть `format=<число>`.
    NoFormat,
    /// Индекс новее, чем эта система: разбирать его нечем.
    ///
    /// Отдельный случай, а не «испорченный файл»: он означает, что обновляться
    /// надо, и говорить об этом надо словами, а не «сервер прислал ерунду».
    Format(u32),
    /// Записи для этой архитектуры нет.
    NoImage,
    /// Запись есть, но в ней не хватает поля или поле не разбирается.
    Field(&'static str),
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NoFormat => "this is not an update index (no format= line)",
            Self::Format(_) => "the index is in a newer format than this system understands",
            Self::NoImage => "the index offers nothing for this architecture",
            Self::Field(name) => name,
        }
    }
}

/// Одна запись индекса: образ для одной архитектуры.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    pub version: &'a str,
    pub arch: &'a str,
    /// Имя файла на сервере, рядом с индексом. Только имя, без пути: путь к
    /// репозиторию знает тот, кто качает, и склеивать его на сервере значило бы
    /// разрешить индексу увести клиента куда угодно.
    pub file: &'a str,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Разобранный индекс.
///
/// Держит ссылку на исходный текст: поля записей — срезы поверх него. Копировать
/// их некуда, кучи у читателя нет.
#[derive(Debug, Clone, Copy)]
pub struct Index<'a> {
    text: &'a str,
}

impl<'a> Index<'a> {
    /// Проверить заголовок и приготовиться к чтению записей.
    pub fn parse(text: &'a str) -> Result<Self, Error> {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some(value) = line.strip_prefix("format=") else {
                return Err(Error::NoFormat);
            };
            let Ok(format) = value.trim().parse::<u32>() else {
                return Err(Error::NoFormat);
            };
            if format != FORMAT {
                return Err(Error::Format(format));
            }
            return Ok(Self { text });
        }
        Err(Error::NoFormat)
    }

    /// Найти запись для архитектуры.
    ///
    /// Первую подходящую, а не «лучшую»: записей на архитектуру ровно одна, и
    /// выбор из нескольких означал бы, что сервер предлагает системе решить, что
    /// ей ставить.
    pub fn image(&self, arch: &str) -> Result<Image<'a>, Error> {
        let mut record: Option<Partial<'a>> = None;
        for line in self.text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == RECORD {
                // Предыдущая запись кончилась. Годится она или нет, решается
                // здесь же: незаконченная запись не должна перетекать в
                // следующую полями, которых в ней не было.
                if let Some(partial) = record.take() {
                    if partial.arch == Some(arch) {
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
                // Поля до первой `[image]` — это заголовок файла (`format=`).
                continue;
            };
            match key {
                "version" => partial.version = Some(value),
                "arch" => partial.arch = Some(value),
                "file" => partial.file = Some(value),
                "size" => partial.size = value.parse::<u64>().ok(),
                "sha256" => partial.sha256 = from_hex::<32>(value),
                // Неизвестное поле — не ошибка: индекс пишет более новая
                // сборка, и она вправе сказать больше, чем эта система умеет
                // прочитать. Обратное (промолчать о непонятном формате) как раз
                // ошибка, и поэтому `format=` проверяется строго.
                _ => {}
            }
        }
        match record {
            Some(partial) if partial.arch == Some(arch) => partial.finish(),
            _ => Err(Error::NoImage),
        }
    }
}

/// Строка, с которой начинается запись.
const RECORD: &str = "[image]";

#[derive(Default)]
struct Partial<'a> {
    version: Option<&'a str>,
    arch: Option<&'a str>,
    file: Option<&'a str>,
    size: Option<u64>,
    sha256: Option<[u8; 32]>,
}

impl<'a> Partial<'a> {
    fn finish(self) -> Result<Image<'a>, Error> {
        let version = self.version.ok_or(Error::Field("the index entry has no version"))?;
        let arch = self.arch.ok_or(Error::Field("the index entry has no arch"))?;
        let file = self.file.ok_or(Error::Field("the index entry has no file name"))?;
        let size = self.size.ok_or(Error::Field("the index entry has no usable size"))?;
        let sha256 = self.sha256.ok_or(Error::Field("the index entry has no usable sha256"))?;
        if file.is_empty() || file.contains('/') || file.contains('\\') || file.starts_with('.') {
            // Имя файла приходит из сети и превращается в путь на **нашей**
            // стороне — и в запрос, и в имя в `/var/cache/updates`. Разделитель
            // в нём означал бы, что индекс волен назвать любой файл на сервере и
            // любой каталог у нас.
            return Err(Error::Field("the index names a file with a path in it"));
        }
        Ok(Image { version, arch, file, size, sha256 })
    }
}

/// Хеш, который подписывается и проверяется.
#[must_use]
pub fn digest(index: &[u8]) -> [u8; 32] {
    let mut hasher = fpk::Hasher::new();
    hasher.update(DOMAIN);
    hasher.update(index);
    hasher.finish()
}

/// Разобрать `index.sig`: строка `ed25519 <128 шестнадцатеричных знаков>`.
#[must_use]
pub fn parse_signature(text: &str) -> Option<[u8; 64]> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        if fields.next() != Some(ALGORITHM) {
            return None;
        }
        return from_hex::<64>(fields.next()?);
    }
    None
}

/// Сборка индекса — то, что делает машина, выкладывающая репозиторий.
#[cfg(feature = "build")]
pub mod build {
    use alloc::string::String;

    use crate::to_hex;

    /// Один образ, который надо предложить.
    pub struct Offer<'a> {
        pub version: &'a str,
        pub arch: &'a str,
        pub file: &'a str,
        pub size: u64,
        pub sha256: [u8; 32],
    }

    /// Составить текст индекса.
    ///
    /// Функция здесь, а не в `xtask`, по той же причине, по которой здесь разбор:
    /// у формата обязан быть один хозяин. Разошедшись, две стороны дали бы
    /// «индекс не разбирается» на машине, которую уже не спросить.
    #[must_use]
    pub fn render(offers: &[Offer<'_>]) -> String {
        let mut out = String::new();
        out.push_str("# FreeOS update repository index.\n");
        out.push_str("# Signed by index.sig; the signature covers this file byte for byte.\n");
        out.push_str("# One [image] record per architecture, and only what is offered now.\n");
        out.push_str(&alloc::format!("format={}\n", super::FORMAT));
        for offer in offers {
            out.push_str("\n[image]\n");
            out.push_str(&alloc::format!("version={}\n", offer.version));
            out.push_str(&alloc::format!("arch={}\n", offer.arch));
            out.push_str(&alloc::format!("file={}\n", offer.file));
            out.push_str(&alloc::format!("size={}\n", offer.size));
            out.push_str(&alloc::format!("sha256={}\n", to_hex(&offer.sha256)));
        }
        out
    }

    /// Составить текст `index.sig`.
    #[must_use]
    pub fn render_signature(signature: &[u8; 64]) -> String {
        let mut out = String::new();
        out.push_str("# Signature of the index next to this file, Ed25519 over SHA-256 of it.\n");
        out.push_str(&alloc::format!("{} {}\n", crate::ALGORITHM, to_hex(signature)));
        out
    }

    /// Хеш файла — тот же, что попадает в поле `sha256` записи.
    #[must_use]
    pub fn hash(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = fpk::Hasher::new();
        hasher.update(bytes);
        hasher.finish()
    }
}
