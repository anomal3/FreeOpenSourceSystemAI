//! Контейнер `.fpk`: то, в чём FreeOS переносит программы и системы целиком.
//!
//! # Почему формат один на пакет и на систему
//!
//! Потому что различаются они содержимым, а не упаковкой. Пакет несёт файлы,
//! которые лягут в `/opt`; обновление системы несёт образ корня, ядро и initrd,
//! которые лягут в неактивный слот. Заголовок, манифест, контрольные суммы и
//! место под подпись у них общие — и держать под это два формата значило бы
//! чинить в двух местах всякую ошибку разбора.
//!
//! # Место под подпись есть с самого начала
//!
//! Подписи пока нет: проверять её нечем — ни ключей в системе, ни кода
//! асимметричной криптографии. Но поле под неё в заголовке стоит **уже сейчас**,
//! и это не задел «на будущее», а решение о совместимости: добавить подпись
//! потом означало бы сдвинуть манифест, то есть выпустить второй формат и
//! научить систему читать оба. Шестьдесят четыре байта нулей стоят дешевле.
//!
//! # Что этот формат не обещает
//!
//! Он не сжат. Сжатие имеет смысл там, где пакеты ходят по сети, а до сети ещё
//! четыре фазы; сжатый контейнер сегодня означал бы декомпрессор в программе,
//! у которой нет ни кучи, ни возможности его проверить.
//!
//! Он не хранит прав на каталоги — только на файлы. Каталоги создаёт тот, кто
//! ставит, и создаёт их своими правами: каталог пакета принадлежит системе, а
//! не пакету, и разрешать содержимому архива задавать права на `/opt` значит
//! отдать распаковщику право на чужие каталоги.
//!
//! # Разбор без кучи
//!
//! Ни одна функция разбора не выделяет памяти: манифест разбирается срезами
//! поверх буфера вызывающего, а полезная нагрузка вовсе не читается — на неё
//! указывают смещение и длина, и вычитывает её тот, кто умеет двигаться по
//! файлу. Так и должно быть: разбирает пакет программа вне ядра, а у неё
//! аллокатора нет.

#![no_std]

#[cfg(feature = "build")]
extern crate alloc;

#[cfg(feature = "build")]
pub mod build;

/// Подпись формата в первых четырёх байтах.
pub const MAGIC: [u8; 4] = *b"FPK\x01";

/// Версия формата. Разбор отказывает на любой другой.
///
/// Отказывает, а не «пробует прочитать»: контейнер с неизвестной версией мог
/// поменять смысл любого поля, и прочитать из него что-нибудь правдоподобное
/// хуже, чем не прочитать ничего.
pub const FORMAT_VERSION: u16 = 1;

/// Длина заголовка в байтах — она же смещение манифеста.
pub const HEADER_SIZE: usize = 128;

/// Сколько байт отведено под подпись.
pub const SIGNATURE_SIZE: usize = 64;

/// Самый длинный манифест, который согласны разбирать.
///
/// Предел не свойство формата, а свойство разбирающего: манифест целиком
/// ложится в буфер программы, а буфер у неё на стеке или в `.bss`. Тридцать
/// два килобайта — это около трёхсот файлов с путями, то есть заведомо больше
/// всего, что кто-нибудь соберёт вручную.
pub const MAX_MANIFEST: usize = 32 * 1024;

/// Что лежит внутри: набор файлов или система целиком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Файлы, которые лягут в `/opt/<имя>`.
    Package,
    /// Образ корня, ядро и initrd — то, что уезжает в неактивный слот.
    System,
}

impl Kind {
    const PACKAGE: u16 = 0;
    const SYSTEM: u16 = 1;

    #[must_use]
    pub const fn code(self) -> u16 {
        match self {
            Self::Package => Self::PACKAGE,
            Self::System => Self::SYSTEM,
        }
    }

    const fn from_code(code: u16) -> Option<Self> {
        match code {
            Self::PACKAGE => Some(Self::Package),
            Self::SYSTEM => Some(Self::System),
            _ => None,
        }
    }

    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Package => "package",
            Self::System => "system",
        }
    }
}

/// Почему контейнер не разобрался.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Прочитано меньше, чем занимает заголовок.
    Short,
    /// Первые четыре байта не те: это не `.fpk`.
    NotAPackage,
    /// Версия формата другая.
    Version(u16),
    /// Поле `kind` не соответствует ни одному известному виду.
    UnknownKind(u16),
    /// Длины в заголовке не сходятся между собой или с размером файла.
    BadLength,
    /// Контрольная сумма манифеста не сошлась.
    ManifestCorrupt,
    /// Манифест длиннее, чем [`MAX_MANIFEST`], либо не UTF-8.
    ManifestUnreadable,
    /// В манифесте нет обязательного поля.
    MissingField,
    /// Строка манифеста не разбирается.
    BadLine,
}

impl Error {
    /// Текст для человека. Свой, а не `Display`: `core::fmt` в программе вне
    /// ядра тянет за собой заметный кусок кода ради одной строки.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Short => "the file is shorter than a package header",
            Self::NotAPackage => "this is not a FreeOS package",
            Self::Version(_) => "the package format version is not supported",
            Self::UnknownKind(_) => "the package declares a kind this system does not know",
            Self::BadLength => "the lengths in the header do not add up",
            Self::ManifestCorrupt => "the manifest checksum does not match",
            Self::ManifestUnreadable => "the manifest is too long or not valid UTF-8",
            Self::MissingField => "the manifest lacks a required field",
            Self::BadLine => "the manifest has a line that does not parse",
        }
    }
}

/// Заголовок контейнера — всё, что нужно знать до чтения манифеста.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub kind: Kind,
    /// Длина манифеста в байтах. Лежит сразу за заголовком.
    pub manifest_len: u32,
    /// Длина полезной нагрузки. Лежит сразу за манифестом.
    pub payload_len: u64,
    pub manifest_crc: u32,
    /// Сумма всей полезной нагрузки разом.
    ///
    /// Существует рядом с суммами отдельных файлов не из перестраховки: файлы
    /// в сумме короче нагрузки на выравнивание, и «все файлы целы» не означает
    /// «контейнер дочитан до конца». Ровно этим отличается оборванная загрузка
    /// от испорченной.
    pub payload_crc: u32,
    /// Алгоритм подписи; ноль означает «подписи нет».
    pub signature_algorithm: u16,
    pub signature_len: u16,
}

impl Header {
    /// Смещение манифеста от начала файла.
    #[must_use]
    pub const fn manifest_offset(&self) -> u64 {
        HEADER_SIZE as u64
    }

    /// Смещение полезной нагрузки от начала файла.
    #[must_use]
    pub const fn payload_offset(&self) -> u64 {
        HEADER_SIZE as u64 + self.manifest_len as u64
    }

    /// Сколько всего байт занимает контейнер.
    #[must_use]
    pub const fn total_len(&self) -> u64 {
        self.payload_offset() + self.payload_len
    }

    /// Подписан ли контейнер. Сегодня всегда `false` — см. заголовок модуля.
    #[must_use]
    pub const fn is_signed(&self) -> bool {
        self.signature_algorithm != 0
    }

    /// Разобрать заголовок.
    ///
    /// Проверок здесь больше, чем полей: всё, что приходит из файла, выбрал не
    /// разбирающий, и длина, переполняющая сложение, обязана стать ошибкой, а
    /// не смещением в чужую память.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() < HEADER_SIZE {
            return Err(Error::Short);
        }
        if bytes[..4] != MAGIC {
            return Err(Error::NotAPackage);
        }
        let format = read_u16(bytes, 4);
        if format != FORMAT_VERSION {
            return Err(Error::Version(format));
        }
        let kind_code = read_u16(bytes, 6);
        let Some(kind) = Kind::from_code(kind_code) else {
            return Err(Error::UnknownKind(kind_code));
        };
        // Длина заголовка записана в самом заголовке — но пока она обязана
        // совпадать с константой. Поле существует ради версии формата, в
        // которой заголовок вырастет: читающий сможет пропустить незнакомый
        // хвост, не гадая, где начинается манифест.
        if read_u32(bytes, 8) as usize != HEADER_SIZE {
            return Err(Error::BadLength);
        }

        let manifest_len = read_u32(bytes, 12);
        if manifest_len as usize > MAX_MANIFEST {
            return Err(Error::ManifestUnreadable);
        }
        let payload_len = read_u64(bytes, 16);
        // Сумма проверяется здесь, чтобы `total_len` дальше не переполнялась
        // ни у кого из вызывающих.
        if (HEADER_SIZE as u64)
            .checked_add(u64::from(manifest_len))
            .and_then(|at| at.checked_add(payload_len))
            .is_none()
        {
            return Err(Error::BadLength);
        }

        let signature_len = read_u16(bytes, 34);
        if signature_len as usize > SIGNATURE_SIZE {
            return Err(Error::BadLength);
        }

        Ok(Self {
            kind,
            manifest_len,
            payload_len,
            manifest_crc: read_u32(bytes, 24),
            payload_crc: read_u32(bytes, 28),
            signature_algorithm: read_u16(bytes, 32),
            signature_len,
        })
    }
}

/// Разобранный манифест: срезы поверх буфера вызывающего.
///
/// Ничего не копирует и ничего не выделяет. Время жизни привязано к тексту, из
/// которого он разобран, — то есть к буферу, в который вызывающий прочитал
/// манифест из файла.
#[derive(Debug, Clone, Copy)]
pub struct Manifest<'a> {
    text: &'a str,
}

impl<'a> Manifest<'a> {
    /// Проверить контрольную сумму и обернуть текст.
    ///
    /// Сумма проверяется здесь, а не при разборе заголовка: заголовок читается
    /// первым и отдельно, а манифест может и не понадобиться — например, когда
    /// спрашивают только вид контейнера.
    pub fn parse(header: &Header, bytes: &'a [u8]) -> Result<Self, Error> {
        if bytes.len() != header.manifest_len as usize {
            return Err(Error::BadLength);
        }
        if crc32(bytes) != header.manifest_crc {
            return Err(Error::ManifestCorrupt);
        }
        let text = core::str::from_utf8(bytes).map_err(|_| Error::ManifestUnreadable)?;
        Ok(Self { text })
    }

    /// Значение поля `ключ=значение`. Первое вхождение, без пробелов по краям.
    #[must_use]
    pub fn field(&self, key: &str) -> Option<&'a str> {
        self.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim())
        })
    }

    /// То же, но отсутствие поля — ошибка.
    pub fn required(&self, key: &str) -> Result<&'a str, Error> {
        self.field(key).ok_or(Error::MissingField)
    }

    /// Имя пакета.
    pub fn name(&self) -> Result<&'a str, Error> {
        let name = self.required("name")?;
        // Имя становится каталогом в `/opt`, поэтому проверяется как имя, а не
        // как строка: разделитель пути или `..` внутри превратили бы установку
        // пакета в запись куда угодно.
        if !is_safe_component(name) {
            return Err(Error::BadLine);
        }
        Ok(name)
    }

    /// Версия пакета — как строка: сравнивать версии этот формат не умеет и не
    /// обещает.
    pub fn version(&self) -> Result<&'a str, Error> {
        self.required("version")
    }

    /// Имена пакетов, без которых этот не имеет смысла.
    ///
    /// Проверять их — дело того, кто ставит; формат лишь переносит список.
    #[must_use]
    pub fn requires(&self) -> impl Iterator<Item = &'a str> {
        self.field("requires")
            .unwrap_or("")
            .split_whitespace()
            .filter(|name| !name.is_empty())
    }

    /// Файлы пакета в порядке, в котором лежит их содержимое.
    #[must_use]
    pub fn files(&self) -> impl Iterator<Item = Result<FileEntry<'a>, Error>> {
        self.lines()
            .filter_map(|line| line.strip_prefix("file="))
            .map(FileEntry::parse)
    }

    /// Кусок полезной нагрузки, названный ключом: `image`, `kernel`, `initrd`.
    pub fn blob(&self, key: &str) -> Result<Blob, Error> {
        Blob::parse(self.required(key)?)
    }

    /// То же, но отсутствие — не ошибка.
    #[must_use]
    pub fn optional_blob(&self, key: &str) -> Option<Blob> {
        self.field(key).and_then(|value| Blob::parse(value).ok())
    }

    /// Содержательные строки: без пустых и без комментариев.
    fn lines(&self) -> impl Iterator<Item = &'a str> {
        self.text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
    }

    /// Текст манифеста как есть — его кладут в реестр установленного.
    #[must_use]
    pub const fn text(&self) -> &'a str {
        self.text
    }
}

/// Один файл пакета.
#[derive(Debug, Clone, Copy)]
pub struct FileEntry<'a> {
    /// Права в unix-нотации, младшие девять бит.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    /// Смещение содержимого от начала полезной нагрузки.
    pub offset: u64,
    pub size: u64,
    pub crc: u32,
    /// Путь внутри пакета: относительный, без `.` и `..`.
    pub path: &'a str,
}

impl<'a> FileEntry<'a> {
    /// `file=<права> <uid> <gid> <смещение> <длина> <crc> <путь>`.
    ///
    /// Путь идёт последним и берётся целиком до конца строки: пробел в имени
    /// файла возможен, а перенос строки — нет.
    fn parse(value: &'a str) -> Result<Self, Error> {
        let mut fields = value.trim().splitn(7, char::is_whitespace);
        let mode = u16::from_str_radix(next(&mut fields)?, 8).map_err(|_| Error::BadLine)?;
        let uid = parse_u32(next(&mut fields)?)?;
        let gid = parse_u32(next(&mut fields)?)?;
        let offset = parse_u64(next(&mut fields)?)?;
        let size = parse_u64(next(&mut fields)?)?;
        let crc = u32::from_str_radix(next(&mut fields)?, 16).map_err(|_| Error::BadLine)?;
        let path = next(&mut fields)?.trim();

        if !is_safe_path(path) {
            return Err(Error::BadLine);
        }
        if offset.checked_add(size).is_none() {
            return Err(Error::BadLine);
        }
        Ok(Self { mode: mode & 0o777, uid, gid, offset, size, crc, path })
    }
}

/// Именованный кусок полезной нагрузки: образ корня, ядро, initrd.
#[derive(Debug, Clone, Copy)]
pub struct Blob {
    pub offset: u64,
    pub size: u64,
    pub crc: u32,
}

impl Blob {
    /// `<смещение> <длина> <crc>`.
    fn parse(value: &str) -> Result<Self, Error> {
        let mut fields = value.trim().split_whitespace();
        let offset = parse_u64(next(&mut fields)?)?;
        let size = parse_u64(next(&mut fields)?)?;
        let crc = u32::from_str_radix(next(&mut fields)?, 16).map_err(|_| Error::BadLine)?;
        if offset.checked_add(size).is_none() {
            return Err(Error::BadLine);
        }
        Ok(Self { offset, size, crc })
    }
}

fn next<'a>(fields: &mut impl Iterator<Item = &'a str>) -> Result<&'a str, Error> {
    fields.next().ok_or(Error::BadLine)
}

fn parse_u32(text: &str) -> Result<u32, Error> {
    text.parse().map_err(|_| Error::BadLine)
}

fn parse_u64(text: &str) -> Result<u64, Error> {
    text.parse().map_err(|_| Error::BadLine)
}

/// Годится ли строка на одно имя внутри пути.
///
/// Запрещены пустое имя, `.`, `..` и всё, что содержит разделитель. Проверка
/// живёт здесь, а не у того, кто ставит, потому что она про формат: путь,
/// выводящий за пределы каталога пакета, — это не «странный пакет», а
/// недопустимый.
#[must_use]
pub fn is_safe_component(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
}

/// Годится ли строка на относительный путь внутри пакета.
#[must_use]
pub fn is_safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path.split('/').all(is_safe_component)
}

/// CRC-32 (полином IEEE 802.3, отражённый) — тот же, которым GPT защищает свои
/// заголовки.
///
/// Своя реализация, а не заимствованная у крейта `disk`: тот тянет за собой
/// `alloc`, а считать сумму приходится и в программе вне ядра, где кучи нет.
/// Двадцать строк без таблицы — цена, которую видно целиком.
#[must_use]
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Продолжить счёт суммы по следующему куску.
///
/// Нужно тому, кто читает содержимое по частям: пакет в сорок мегабайт не
/// помещается в память ни у программы, ни у ядра, а сумма обязана быть посчитана
/// по всему файлу.
#[must_use]
pub fn crc32_update(crc: u32, bytes: &[u8]) -> u32 {
    let mut crc = !crc;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Начальное значение для [`crc32_update`].
pub const CRC32_INIT: u32 = 0;

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сумма сверяется с известным значением, а не сама с собой: своя
    /// реализация, проверенная своим же вызовом, доказывает только то, что она
    /// детерминирована.
    #[test]
    fn crc32_matches_the_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// Счёт по частям обязан давать то же, что счёт целиком, — иначе пакет,
    /// прочитанный кусками, никогда не сойдётся с пакетом, записанным разом.
    #[test]
    fn crc32_by_chunks_equals_crc32_at_once() {
        let data: [u8; 300] = core::array::from_fn(|index| index as u8);
        let whole = crc32(&data);
        let mut partial = CRC32_INIT;
        for chunk in data.chunks(37) {
            partial = crc32_update(partial, chunk);
        }
        assert_eq!(whole, partial);
    }

    #[test]
    fn a_path_that_escapes_the_package_is_rejected() {
        assert!(is_safe_path("bin/hello"));
        assert!(!is_safe_path("/bin/hello"));
        assert!(!is_safe_path("../etc/passwd"));
        assert!(!is_safe_path("bin/../../etc/passwd"));
        assert!(!is_safe_path(""));
    }
}
