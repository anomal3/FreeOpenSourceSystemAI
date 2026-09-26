// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

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
//! # Подпись: что именно подписано и почему так
//!
//! Место под подпись стояло в заголовке с первого дня, пустое: добавить её
//! потом означало бы сдвинуть манифест, то есть выпустить второй формат и
//! научить систему читать оба. С фазы 39 оно заполняется — Ed25519 по
//! **SHA-256 от заголовка (с обнулённой подписью) и манифеста**.
//!
//! Полезная нагрузка в подписанное не входит, и это не упрощение. Она — десятки
//! мегабайт, а читается ровно один раз, отрезками, прямо в раздел слота:
//! посчитать по ней хеш до записи значило бы прочитать её дважды с того же
//! носителя, то есть удвоить самую долгую операцию в системе. Вместо этого
//! **каждый кусок нагрузки назван в манифесте своим SHA-256**, а манифест
//! подписан. Цепочка получается такая же прочная и укладывается в один проход:
//! подпись → манифест → хеш куска, считаемый по дороге.
//!
//! Отсюда правило, которое проверяется при разборе: **у подписанного контейнера
//! хеш обязателен у каждого куска**. Контейнер, где подпись есть, а хеша нет, —
//! это контейнер, у которого подписана только опись; принимать такой значит
//! оставить дверь, ради которой всё и затевалось.
//!
//! Контрольные суммы CRC-32 при этом остаются и никуда не денутся: они ловят
//! **порчу**, а не подмену, и ловят её дешевле — оборванная загрузка видна на
//! первом же куске, не дожидаясь хеша.
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

/// Где в заголовке лежит подпись.
pub const SIGNATURE_OFFSET: usize = 40;

/// Алгоритм подписи: Ed25519 по SHA-256 заголовка и манифеста.
///
/// Ноль означает «подписи нет» и остаётся значением по умолчанию: пакет,
/// собранный на этой машине для этой машины, подписывать нечем и незачем.
pub const SIGNATURE_ED25519: u16 = 1;

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
    /// Контейнер подписан, а у куска нагрузки нет хеша.
    MissingHash,
    /// Подпись не сошлась ни с одним из доверенных ключей.
    BadSignature,
    /// Алгоритм подписи не тот, который умеет эта система.
    UnknownSignature(u16),
    /// В списке прав названо то, чего система не знает.
    UnknownRight,
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
            Self::MissingHash => "the container is signed but a payload part carries no hash",
            Self::BadSignature => "the signature does not match any key this system trusts",
            Self::UnknownSignature(_) => "the signature algorithm is not one this system knows",
            Self::UnknownRight => "the manifest asks for a permission this system does not know",
        }
    }
}


/// Права, которые пакет просит у системы.
///
/// # Зачем они и что здесь на самом деле сделано
///
/// До сих пор программа, поставленная пакетом, могла ровно то же, что и любая
/// другая: открыть сокет, прочитать чужой файл, завести окно. Единственным
/// ограничителем была учётная запись, от имени которой её запустили, — то есть
/// «что можно этому человеку», а не «что нужно этой программе». Разница видна
/// на любом примере: калькулятору не нужна сеть, и то, что он её может, не
/// защищено ничем, кроме намерений автора.
///
/// Манифест позволяет автору сказать, что программе нужно, а системе —
/// отказать во всём остальном. Объявление стоит дёшево, проверка добавляется
/// постепенно; что проверяется **сегодня**, а что пока только объявляется,
/// написано у каждого права ниже и повторено в ROADMAP. Обещать больше, чем
/// проверяется, здесь нельзя: право, о котором сказано «работает», а оно не
/// работает, хуже отсутствия права — на него будут рассчитывать.
///
/// # Почему набор битов, а не список строк
///
/// Потому что этот набор едет с каждым запуском программы и пересекается с
/// набором родителя на каждом порождении. Список строк означал бы кучу в
/// ядре на пути, где её быть не должно, и сравнение строк там, где хватает
/// одной команды процессора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights(u8);

impl Rights {
    /// Ничего сверх своего каталога. Это же — умолчание для пакета, который о
    /// правах не сказал.
    pub const NONE: Self = Self(0);

    /// Сеть: сокеты, соединения, имена. **Проверяется.**
    pub const NET: Self = Self(1 << 0);

    /// Файлы вне своего каталога. **Объявляется, но пока не проверяется** —
    /// проверка означает разбор пути в каждом файловом вызове, и делать её
    /// заодно с объявлением значило бы сделать обе вещи наспех.
    pub const FILES: Self = Self(1 << 1);

    /// Своё окно на рабочем столе. **Проверяется.**
    pub const WINDOWS: Self = Self(1 << 2);

    /// Устройство PCI целиком: его регистры, прерывания и память для DMA —
    /// драйвер, который программа (веха «драйверы по VID:PID»). **Проверяется.**
    ///
    /// Самое широкое право из всех: без IOMMU устройство с DMA пишет в любую
    /// память машины, то есть программа с этим правом равна ядру по силе. Поэтому
    /// пакет, который его просит, обязан быть подписан (часть Д2).
    pub const DEVICES: Self = Self(1 << 3);

    /// Всё, что бывает. Столько получает всякая программа, запущенная не как
    /// пакет: системная из `/bin`, команда оболочки, служба. Ограничивать их
    /// этим набором нечем и незачем — они и есть система.
    pub const ALL: Self = Self(0b1111);

    /// Разобрать список имён через пробел: `net files windows`.
    pub fn parse(list: &str) -> Result<Self, Error> {
        let mut rights = Self::NONE;
        for name in list.split_whitespace() {
            rights = rights.union(Self::by_name(name).ok_or(Error::UnknownRight)?);
        }
        Ok(rights)
    }

    /// Право по имени, которым его называет манифест.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "net" => Some(Self::NET),
            "files" => Some(Self::FILES),
            "windows" => Some(Self::WINDOWS),
            "devices" => Some(Self::DEVICES),
            _ => None,
        }
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Пересечение — то, чем порождённая программа не может оказаться шире
    /// породившей. Без него модель была бы фикцией: программа без сети
    /// запускала бы `/bin/fetch` и получала бы сеть его руками.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Биты как число — ядру, чтобы положить набор в заявку на запуск.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Обратно из числа. Незнакомые биты отбрасываются: набор мог прийти от
    /// системы постарше, и «непонятный бит» обязан значить «нет права», а не
    /// «какое-то право».
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & Self::ALL.0)
    }

    /// Имена прав через пробел — для журнала и для человека.
    ///
    /// Буфер даётся вызывающим: строку здесь собирать не на что, кучи у этого
    /// крейта нет и не будет. Возвращает срез этого же буфера.
    pub fn write_names<'a>(self, out: &'a mut [u8; 32]) -> &'a str {
        let mut used = 0;
        for (right, name) in [
            (Self::NET, "net"),
            (Self::FILES, "files"),
            (Self::WINDOWS, "windows"),
            (Self::DEVICES, "devices"),
        ] {
            if !self.contains(right) {
                continue;
            }
            if used > 0 && used < out.len() {
                out[used] = b' ';
                used += 1;
            }
            let bytes = name.as_bytes();
            if used + bytes.len() > out.len() {
                break;
            }
            out[used..used + bytes.len()].copy_from_slice(bytes);
            used += bytes.len();
        }
        if used == 0 {
            let none = b"none";
            out[..none.len()].copy_from_slice(none);
            used = none.len();
        }
        // SAFETY: в буфер попадают только имена из списка выше и пробелы.
        unsafe { core::str::from_utf8_unchecked(&out[..used]) }
    }
}

/// Разобрать `vendor:device` — два шестнадцатеричных числа по 16 бит.
///
/// Одно место на установщик пакетов и на службу, которая ищет драйвер по
/// устройству: разбери они запись по-разному, пакет ставился бы, а драйвер не
/// находился.
#[must_use]
pub fn parse_drive(text: &str) -> Option<(u16, u16)> {
    let (vendor, device) = text.split_once(':')?;
    if vendor.len() != 4 || device.len() != 4 {
        return None;
    }
    Some((u16::from_str_radix(vendor, 16).ok()?, u16::from_str_radix(device, 16).ok()?))
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
    /// Сама подпись — столько байт, сколько назвал `signature_len`.
    pub signature: [u8; SIGNATURE_SIZE],
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
        let mut signature = [0u8; SIGNATURE_SIZE];
        signature.copy_from_slice(&bytes[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE_SIZE]);

        Ok(Self {
            kind,
            manifest_len,
            payload_len,
            manifest_crc: read_u32(bytes, 24),
            payload_crc: read_u32(bytes, 28),
            signature_algorithm: read_u16(bytes, 32),
            signature_len,
            signature,
        })
    }
}

/// Что именно подписывается: SHA-256 заголовка (с обнулённой подписью) и
/// манифеста.
///
/// Живёт здесь, а не у подписывающего и не у проверяющего, по одной причине:
/// это **общее знание двух сторон**, и разойтись оно может только молча —
/// подпись просто перестанет сходиться, и виноватым будет выглядеть ключ.
///
/// Обнуляется не только подпись, но и её длина с алгоритмом: иначе подписанное
/// зависело бы от того, подписан ли уже контейнер, а это круг.
#[must_use]
pub fn signed_digest(head: &[u8], manifest: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hash = Sha256::new();
    let head = &head[..HEADER_SIZE.min(head.len())];
    for (at, byte) in head.iter().enumerate() {
        let blanked = (32..36).contains(&at)
            || (SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE_SIZE).contains(&at);
        hash.update([if blanked { 0 } else { *byte }]);
    }
    hash.update(manifest);
    let digest = hash.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// SHA-256 куска нагрузки, посчитанный по частям.
///
/// Обёртка вокруг чужого типа существует затем, чтобы у ядра, у программы и у
/// сборщика был один и тот же способ считать хеш по дороге, а не три похожих.
pub struct Hasher(sha2::Sha256);

impl Hasher {
    #[must_use]
    pub fn new() -> Self {
        use sha2::Digest;
        Self(sha2::Sha256::new())
    }

    pub fn update(&mut self, chunk: &[u8]) {
        use sha2::Digest;
        self.0.update(chunk);
    }

    #[must_use]
    pub fn finish(self) -> [u8; 32] {
        use sha2::Digest;
        let digest = self.0.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
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
    /// Контейнер подписан — значит у каждого куска обязан быть хеш.
    signed: bool,
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
        Ok(Self { text, signed: header.is_signed() })
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

    /// Права, которые пакет просит у системы.
    ///
    /// Отсутствие строки означает **ничего**, а не «всё», и это главное
    /// решение здесь: пакет, который о правах не сказал, их не получает.
    /// Умолчание «всё» превратило бы манифест в украшение — старый пакет
    /// продолжал бы делать что угодно, и модель прав существовала бы только
    /// для тех, кто сам согласился ограничиться.
    ///
    /// Непонятное имя — ошибка, а не пропуск: опечатка в `netwrok` иначе
    /// молча оставила бы пакет без сети, и разбираться с этим пришлось бы по
    /// поведению, а не по отказу установки.
    /// Устройства, которые обслуживает пакет-драйвер: поле `drives`, пары
    /// `vendor:device` шестнадцатеричными числами через пробел (`1234:11e8`).
    /// `None` в итераторе — запись, которую разобрать нельзя: установщик
    /// обязан отвергнуть такой пакет, а не молча пропустить запись.
    pub fn drives(&self) -> impl Iterator<Item = Option<(u16, u16)>> + 'a {
        self.field("drives").unwrap_or("").split_whitespace().map(parse_drive)
    }

    pub fn rights(&self) -> Result<Rights, Error> {
        match self.field("permissions") {
            None => Ok(Rights::NONE),
            Some(list) => Rights::parse(list),
        }
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
        let blob = Blob::parse(self.required(key)?)?;
        // Требование проверяется здесь, а не у вызывающего: забыть его значит
        // принять контейнер, у которого подписана одна опись, — а именно так
        // поступил бы всякий, кто про это требование не знает.
        if self.signed && blob.hash.is_none() {
            return Err(Error::MissingHash);
        }
        Ok(blob)
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
    /// SHA-256 содержимого. `None` — у неподписанного контейнера.
    ///
    /// Именно он связывает подписанный манифест с нагрузкой; CRC рядом ловит
    /// порчу и обрыв, а не подмену (см. заголовок крейта).
    pub hash: Option<[u8; 32]>,
}

impl Blob {
    /// `<смещение> <длина> <crc>` и, если контейнер подписан, `<sha256>`.
    fn parse(value: &str) -> Result<Self, Error> {
        let mut fields = value.trim().split_whitespace();
        let offset = parse_u64(next(&mut fields)?)?;
        let size = parse_u64(next(&mut fields)?)?;
        let crc = u32::from_str_radix(next(&mut fields)?, 16).map_err(|_| Error::BadLine)?;
        let hash = match fields.next() {
            Some(text) => Some(parse_hash(text)?),
            None => None,
        };
        if offset.checked_add(size).is_none() {
            return Err(Error::BadLine);
        }
        Ok(Self { offset, size, crc, hash })
    }
}

/// Разобрать SHA-256, записанный шестнадцатеричными цифрами.
fn parse_hash(text: &str) -> Result<[u8; 32], Error> {
    if text.len() != 64 {
        return Err(Error::BadLine);
    }
    let bytes = text.as_bytes();
    let mut out = [0u8; 32];
    for (at, pair) in bytes.chunks_exact(2).enumerate() {
        let high = digit(pair[0])?;
        let low = digit(pair[1])?;
        out[at] = (high << 4) | low;
    }
    Ok(out)
}

const fn digit(byte: u8) -> Result<u8, Error> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(Error::BadLine),
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

    /// Хеш из манифеста читается ровно теми же байтами, какими его записали.
    #[test]
    fn a_blob_line_carries_its_hash() {
        let blob = Blob::parse(
            "0 1024 deadbeef \
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .expect("строка разбирается");
        assert_eq!(blob.offset, 0);
        assert_eq!(blob.size, 1024);
        assert_eq!(blob.crc, 0xdead_beef);
        let hash = blob.hash.expect("хеш есть");
        assert_eq!(hash[0], 0xe3);
        assert_eq!(hash[31], 0x55);
    }

    /// Строка без хеша разбирается — так выглядят пакеты, собранные до подписи
    /// и без неё.
    #[test]
    fn a_blob_line_without_a_hash_still_parses() {
        let blob = Blob::parse("16 32 00000000").expect("строка разбирается");
        assert!(blob.hash.is_none());
    }

    /// Хеш не той длины или не из шестнадцатеричных цифр — отказ, а не
    /// молчаливое «прочитали, сколько было».
    #[test]
    fn a_hash_that_is_not_a_hash_is_refused() {
        assert!(Blob::parse("0 1 0 abcd").is_err());
        assert!(
            Blob::parse(
                "0 1 0 zzb0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )
            .is_err()
        );
    }

    /// Подписанное не должно зависеть от того, подписан ли уже контейнер:
    /// поля подписи в хеш не входят.
    #[test]
    fn the_signature_fields_do_not_change_what_is_signed() {
        let mut head = [7u8; HEADER_SIZE];
        let manifest = b"name=freeos\n";
        let before = signed_digest(&head, manifest);
        head[32..36].copy_from_slice(&[1, 0, 64, 0]);
        for byte in &mut head[SIGNATURE_OFFSET..SIGNATURE_OFFSET + SIGNATURE_SIZE] {
            *byte = 0xAB;
        }
        assert_eq!(before, signed_digest(&head, manifest));

        // А вот всё остальное входит: правка длины манифеста обязана менять
        // хеш, иначе подпись не защищает ничего.
        head[12] ^= 1;
        assert_ne!(before, signed_digest(&head, manifest));
    }

    /// Счёт по частям обязан давать то же, что счёт целиком: нагрузка
    /// считается по дороге, отрезками по четверти мегабайта.
    #[test]
    fn hashing_by_chunks_equals_hashing_at_once() {
        let data: [u8; 500] = core::array::from_fn(|index| (index * 7) as u8);
        let mut whole = Hasher::new();
        whole.update(&data);
        let mut partial = Hasher::new();
        for chunk in data.chunks(64) {
            partial.update(chunk);
        }
        assert_eq!(whole.finish(), partial.finish());
    }

    #[test]
    fn a_path_that_escapes_the_package_is_rejected() {
        assert!(is_safe_path("bin/hello"));
        assert!(!is_safe_path("/bin/hello"));
        assert!(!is_safe_path("../etc/passwd"));
        assert!(!is_safe_path("bin/../../etc/passwd"));
        assert!(!is_safe_path(""));
    }

    /// Пакет, не сказавший о правах, не получает ничего.
    ///
    /// Умолчание здесь — единственное решение, которое стоит проверять тестом:
    /// сделай оно «всё», и модель прав действовала бы только на тех, кто сам
    /// согласился ограничиться, а старый пакет продолжал бы мочь что угодно.
    #[test]
    fn a_package_that_says_nothing_gets_nothing() {
        let text = "name=quiet
version=1.0
";
        let header = Header {
            kind: Kind::Package,
            manifest_len: text.len() as u32,
            payload_len: 0,
            manifest_crc: crc32(text.as_bytes()),
            payload_crc: 0,
            signature_algorithm: 0,
            signature_len: 0,
            signature: [0; SIGNATURE_SIZE],
        };
        let manifest = Manifest::parse(&header, text.as_bytes()).expect("манифест");
        assert_eq!(manifest.rights().expect("права"), Rights::NONE);
    }

    /// Имена прав разбираются, а незнакомое — отказ, а не пропуск.
    #[test]
    fn permissions_parse_by_name_and_a_typo_is_refused() {
        assert_eq!(Rights::parse("").expect("пусто"), Rights::NONE);
        assert_eq!(Rights::parse("net").expect("сеть"), Rights::NET);
        assert_eq!(
            Rights::parse("net windows").expect("два"),
            Rights::NET.union(Rights::WINDOWS)
        );
        assert_eq!(Rights::parse("  files   net  ").expect("пробелы"), Rights::NET.union(Rights::FILES));
        // Опечатка молча оставила бы пакет без права, и разбираться пришлось бы
        // по поведению программы, а не по отказу установки.
        assert_eq!(Rights::parse("netwrok"), Err(Error::UnknownRight));
        assert_eq!(Rights::parse("net root"), Err(Error::UnknownRight));
    }

    /// Пересечение сужает и никогда не расширяет — на этом держится
    /// наследование прав порождённой программой.
    #[test]
    fn rights_only_ever_narrow() {
        let net = Rights::NET;
        assert_eq!(Rights::ALL.intersect(net), net);
        assert_eq!(net.intersect(Rights::WINDOWS), Rights::NONE);
        assert!(Rights::ALL.contains(net));
        assert!(!net.contains(Rights::ALL));
        assert!(Rights::NONE.is_empty());
        // Число и обратно — то, чем набор едет в заявке на запуск. Лишние биты
        // отбрасываются: «непонятный бит» обязан значить «нет права».
        assert_eq!(Rights::from_bits(Rights::ALL.bits()), Rights::ALL);
        assert_eq!(Rights::from_bits(0xff), Rights::ALL);
    }

    /// Имена для журнала: пустой набор называется словом, а не пустотой.
    #[test]
    fn rights_have_names_for_the_log() {
        let mut buffer = [0u8; 32];
        assert_eq!(Rights::NONE.write_names(&mut buffer), "none");
        let mut buffer = [0u8; 32];
        assert_eq!(Rights::ALL.write_names(&mut buffer), "net files windows devices");
        let mut buffer = [0u8; 32];
        assert_eq!(Rights::WINDOWS.write_names(&mut buffer), "windows");
    }
}
