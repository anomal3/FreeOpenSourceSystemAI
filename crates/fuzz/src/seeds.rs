//! Исходные образцы: правильные байты, которые фаззер портит.
//!
//! # Почему образцы правильные, а не случайные
//!
//! Потому что все здешние форматы отвергают мусор первым же полем. Случайные
//! байты не проходят проверку сигнатуры, и фаззер, начав с них, проверял бы
//! одну строку — ту, что сравнивает магическое число. Испорченный **правильный**
//! образец проходит дальше: сигнатура на месте, а длина внутри — нет, и разбор
//! доходит до арифметики, ради которой всё это затевается.
//!
//! # Откуда они берутся
//!
//! Тома ext2 и btrfs — из **нашего** форматировщика: он в этом же дереве, и
//! образец получается на месте, а не лежит двоичным файлом в репозитории.
//! Сертификаты — настоящие, снятые с github.com и rust-lang.org (они уже лежат
//! рядом с тестами крейта `x509`). Дескрипторы HID — от настоящих клавиатуры и
//! мыши QEMU. Контейнер `.fpk` и образ PE собираются здесь руками: писателя у
//! первого нет вовсе (его пишет `xtask`), а второй — чужой формат, образцов
//! которого в репозитории нет и быть не должно.

use disk::MemDisk;

/// Размер сектора образцов-томов.
const SECTOR: usize = 512;

/// Том ext2 на два мегабайта: наименьший, который принимает наш
/// форматировщик, и достаточный, чтобы в нём были суперблок, группа, битовые
/// карты и корневой каталог.
///
/// Два мегабайта на вход фаззера — много, но портится только начало (см.
/// `hot_bytes` у цели): разборщик дальше первых килобайт и не ходит, пока не
/// поверит суперблоку.
#[must_use]
pub fn ext2_volume() -> Vec<u8> {
    let sectors = 2 * 1024 * 1024 / SECTOR as u64;
    let mut disk = MemDisk::new(sectors).expect("образ ext2 в памяти");
    let options = ext2::FormatOptions {
        label: "fuzz",
        uuid: [0x5a; 16],
        // Фиксированное время — образец обязан быть побайтово одинаковым при
        // каждом запуске, иначе «зерно и итерация» не воспроизводят вход.
        time: 1_700_000_000,
    };
    let mut editor = ext2::format(&mut disk, 0, sectors, &options).expect("разметка ext2");
    // Один файл и один каталог: пустой том не проверяет разбор записей
    // каталога, а они — самая частая причина чтения за границей.
    editor
        .create_file(&mut disk, ext2::ROOT_INODE, "hello.txt", b"fuzz", 0o644, 0, 0)
        .expect("файл в образце ext2");
    editor
        .mkdir(&mut disk, ext2::ROOT_INODE, "dir", 0o755, 0, 0)
        .expect("каталог в образце ext2");
    editor.flush(&mut disk).expect("сброс образца ext2");
    disk.into_vec()
}

/// Том btrfs наименьшего допустимого размера.
#[must_use]
pub fn btrfs_volume() -> Vec<u8> {
    let bytes = btrfs::MIN_VOLUME_BYTES;
    let sectors = bytes / SECTOR as u64;
    let mut disk = MemDisk::new(sectors).expect("образ btrfs в памяти");
    let options =
        btrfs::FormatOptions { label: "fuzz", uuid: [0x5a; 16], time: 1_700_000_000 };
    btrfs::format(&mut disk, 0, sectors, &options).expect("разметка btrfs");
    disk.into_vec()
}

/// Настоящие сертификаты: лист, промежуточный и перекрёстный, с двух разных
/// цепочек.
///
/// Лежат рядом с тестами крейта `x509` и берутся оттуда, а не копируются сюда:
/// две копии одного сертификата разойдутся, когда один из них обновят.
#[must_use]
pub fn certificates() -> Vec<Vec<u8>> {
    vec![
        include_bytes!("../../x509/tests/certs/github-leaf.der").to_vec(),
        include_bytes!("../../x509/tests/certs/github-intermediate.der").to_vec(),
        include_bytes!("../../x509/tests/certs/rustlang-leaf.der").to_vec(),
        include_bytes!("../../x509/tests/certs/rustlang-cross.der").to_vec(),
    ]
}

/// Текст PEM с доверенными корнями — то, что система читает из `/etc`.
#[must_use]
pub fn roots_pem() -> Vec<u8> {
    include_bytes!("../../x509/tests/certs/roots.pem").to_vec()
}

/// Дескрипторы HID настоящих устройств: клавиатура и мышь QEMU и планшет с
/// абсолютными координатами.
///
/// Выписаны байтами, а не сняты с устройства на ходу: образец обязан быть
/// одним и тем же, а живое устройство здесь взять негде.
#[must_use]
pub fn hid_descriptors() -> Vec<Vec<u8>> {
    vec![
        // Клавиатура: восемь бит модификаторов, байт запаса, шесть кодов.
        vec![
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x01, 0x95, 0x08, 0x05, 0x07, 0x19, 0xe0,
            0x29, 0xe7, 0x15, 0x00, 0x25, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x03,
            0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff,
            0x81, 0x00, 0xc0,
        ],
        // Мышь: три кнопки и относительные X, Y, колесо.
        vec![
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
            0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01,
            0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81,
            0x25, 0x7f, 0x75, 0x08, 0x95, 0x03, 0x81, 0x06, 0xc0, 0xc0,
        ],
        // Планшет: абсолютные координаты в шестнадцати битах.
        vec![
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
            0x29, 0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01,
            0x75, 0x05, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff,
            0x7f, 0x35, 0x00, 0x46, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01,
            0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
        ],
    ]
}

/// Таблицы DSDT с настоящих машин — в том виде, в каком их пишут прошивки.
///
/// Две, и это важно: ArmVirtQemu описывает маршрутизацию переменным пакетом
/// (`0x13`), OVMF — обычным (`0x12`), и объявляет её не именем, а методом,
/// рядом с которым лежат две готовые таблицы. Один образец проверял бы половину
/// разборщика.
///
/// Байты сняты дампом из загруженной системы, длины пересчитаны под укороченные
/// образцы: целая таблица — восемь килобайт, и портить в ней имеет смысл тот же
/// килобайт.
#[must_use]
pub fn dsdt_tables() -> Vec<Vec<u8>> {
    /// Заголовок таблицы ACPI: разборщик его пропускает, и без него смещения
    /// сошлись бы случайно.
    fn header() -> Vec<u8> {
        let mut bytes = vec![0u8; 36];
        bytes[..4].copy_from_slice(b"DSDT");
        bytes
    }

    /// `Device (<имя>)` со связкой, сидящей на линии `gsi`.
    fn link(name: &[u8; 4], gsi: u8, flags: u8) -> Vec<u8> {
        let mut body = vec![0x08, b'_', b'H', b'I', b'D', 0x0D];
        body.extend_from_slice(b"PNP0C0F ");
        // `_PRS` идёт раньше `_CRS` и содержит такой же дескриптор: разборщик,
        // берущий первый попавшийся, обязан ошибиться именно здесь.
        for name in [b"_PRS", b"_CRS"] {
            body.extend_from_slice(&[0x08]);
            body.extend_from_slice(name);
            body.extend_from_slice(&[0x11, 0x0E, 0x0A, 0x0B]);
            body.extend_from_slice(&[0x89, 0x06, 0x00, flags, 0x01, gsi, 0x00, 0x00, 0x00, 0x79, 0x00]);
        }
        let mut out = vec![0x5B, 0x82, (1 + 4 + body.len()) as u8];
        out.extend_from_slice(name);
        out.extend_from_slice(&body);
        out
    }

    /// Одна запись маршрутизации: устройство, вывод, имя связки.
    fn entry(device: u8, pin: u8, name: &[u8; 4]) -> Vec<u8> {
        let mut body = vec![0x0C, 0xFF, 0xFF, device, 0x00];
        body.extend_from_slice(&[0x0A, pin]);
        body.extend_from_slice(name);
        body.push(0x00);
        let mut out = vec![0x12, (1 + 1 + body.len()) as u8, 4];
        out.extend_from_slice(&body);
        out
    }

    fn table(name: &[u8; 4], variable: bool, entries: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        if variable {
            body.extend_from_slice(&[0x0A, entries.len() as u8]);
        } else {
            body.push(entries.len() as u8);
        }
        for one in entries {
            body.extend_from_slice(one);
        }
        let mut out = vec![0x08];
        out.extend_from_slice(name);
        out.push(if variable { 0x13 } else { 0x12 });
        out.push((1 + body.len()) as u8);
        out.extend_from_slice(&body);
        out
    }

    // `virt` под ArmVirtQemu: связки `L00x` на SPI 35–38, переменный пакет.
    let mut arm = header();
    arm.extend_from_slice(&link(b"L000", 35, 0x01));
    arm.extend_from_slice(&link(b"L001", 36, 0x01));
    arm.extend_from_slice(&table(
        b"_PRT",
        true,
        &[entry(0, 0, b"L000"), entry(0, 1, b"L001"), entry(1, 0, b"L001")],
    ));

    // Q35 под OVMF: связки `GSIx` на линиях 16–23, обычный пакет, и рядом с
    // таблицей режима APIC лежит таблица режима PIC — ту брать нельзя.
    let mut x86 = header();
    x86.extend_from_slice(&link(b"GSIE", 20, 0x09));
    x86.extend_from_slice(&link(b"LNKE", 11, 0x09));
    x86.extend_from_slice(&table(b"PRTP", false, &[entry(3, 0, b"LNKE")]));
    x86.extend_from_slice(&table(b"PRTA", false, &[entry(3, 0, b"GSIE")]));

    vec![arm, x86]
}

/// Правильный контейнер `.fpk` с одним файлом внутри.
///
/// Собран здесь руками, потому что писателя у крейта нет: контейнер пишет
/// `xtask`, а зависеть от него этот крейт не может — `xtask` сам зависит от
/// него. Зато формат от этого проверяется дважды двумя независимыми
/// реализациями записи, и расхождение между ними — тоже находка.
#[must_use]
pub fn fpk_package() -> Vec<u8> {
    let manifest = "name=fuzz\nversion=1.0\nfile=bin/fuzz 4 0 0755\n";
    let payload: &[u8] = b"fuzz";

    let mut bytes = vec![0u8; fpk::HEADER_SIZE];
    bytes[..4].copy_from_slice(&fpk::MAGIC);
    bytes[4..6].copy_from_slice(&fpk::FORMAT_VERSION.to_le_bytes());
    bytes[6..8].copy_from_slice(&fpk::Kind::Package.code().to_le_bytes());
    bytes[8..12].copy_from_slice(&(fpk::HEADER_SIZE as u32).to_le_bytes());
    bytes[12..16].copy_from_slice(&(manifest.len() as u32).to_le_bytes());
    bytes[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes[24..28].copy_from_slice(&fpk::crc32(manifest.as_bytes()).to_le_bytes());
    bytes[28..32].copy_from_slice(&fpk::crc32(payload).to_le_bytes());
    // Подписи нет: ноль в алгоритме — это «не подписан», и проверка подписи в
    // разборе не участвует.
    bytes.extend_from_slice(manifest.as_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// Образ PE, каким его видит разборщик сборок .NET: заголовок DOS, подпись PE,
/// таблица секций и заголовок CLI с корнем метаданных.
///
/// Собран руками и нарочно небольшой. Настоящей сборки в репозитории нет —
/// `.dll` берутся у установленного .NET (`cargo xtask clr-check`), то есть на
/// машине, где он есть, а фаззер обязан работать и там, где его нет.
/// Испорченный такой образец проверяет ровно то, что нужно: смещения и длины,
/// которые разбор складывает, прежде чем что-нибудь прочитать.
#[must_use]
pub fn pe_image() -> Vec<u8> {
    /// Где начинается подпись PE — значение из заголовка DOS.
    const PE_AT: usize = 0x80;
    /// Начало необязательного заголовка.
    const OPTIONAL_AT: usize = PE_AT + 24;
    /// Размер необязательного заголовка PE32+ с шестнадцатью каталогами.
    const OPTIONAL_SIZE: usize = 240;
    const SECTIONS_AT: usize = OPTIONAL_AT + OPTIONAL_SIZE;

    let mut bytes = vec![0u8; 0x400];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&(PE_AT as u32).to_le_bytes());

    bytes[PE_AT..PE_AT + 4].copy_from_slice(b"PE\0\0");
    // Машина AMD64, одна секция, размер необязательного заголовка.
    bytes[PE_AT + 4..PE_AT + 6].copy_from_slice(&0x8664u16.to_le_bytes());
    bytes[PE_AT + 6..PE_AT + 8].copy_from_slice(&1u16.to_le_bytes());
    bytes[PE_AT + 20..PE_AT + 22].copy_from_slice(&(OPTIONAL_SIZE as u16).to_le_bytes());
    // PE32+.
    bytes[OPTIONAL_AT..OPTIONAL_AT + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    // Число каталогов.
    bytes[OPTIONAL_AT + 108..OPTIONAL_AT + 112].copy_from_slice(&16u32.to_le_bytes());

    // Секция `.text`: виртуальный адрес 0x2000, данные с 0x200.
    let section = SECTIONS_AT;
    bytes[section..section + 5].copy_from_slice(b".text");
    bytes[section + 8..section + 12].copy_from_slice(&0x200u32.to_le_bytes());
    bytes[section + 12..section + 16].copy_from_slice(&0x2000u32.to_le_bytes());
    bytes[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
    bytes[section + 20..section + 24].copy_from_slice(&0x200u32.to_le_bytes());

    // Каталог CLI — пятнадцатый по счёту — указывает внутрь `.text`.
    let cli_dir = OPTIONAL_AT + 112 + 14 * 8;
    bytes[cli_dir..cli_dir + 4].copy_from_slice(&0x2000u32.to_le_bytes());
    bytes[cli_dir + 4..cli_dir + 8].copy_from_slice(&72u32.to_le_bytes());

    // Заголовок CLI в начале данных секции: размер, версия и каталог
    // метаданных сразу за ним.
    let cli = 0x200;
    bytes[cli..cli + 4].copy_from_slice(&72u32.to_le_bytes());
    bytes[cli + 4..cli + 6].copy_from_slice(&2u16.to_le_bytes());
    bytes[cli + 8..cli + 12].copy_from_slice(&0x2048u32.to_le_bytes());
    bytes[cli + 12..cli + 16].copy_from_slice(&64u32.to_le_bytes());

    // Корень метаданных: подпись, версия и один поток `#~`.
    let root = 0x200 + 72;
    bytes[root..root + 4].copy_from_slice(&0x424a_5342u32.to_le_bytes());
    bytes[root + 12..root + 16].copy_from_slice(&4u32.to_le_bytes());
    bytes[root + 16..root + 20].copy_from_slice(b"v4\0\0");
    bytes[root + 22..root + 24].copy_from_slice(&1u16.to_le_bytes());
    bytes[root + 24..root + 28].copy_from_slice(&64u32.to_le_bytes());
    bytes[root + 28..root + 32].copy_from_slice(&16u32.to_le_bytes());
    bytes[root + 32..root + 36].copy_from_slice(b"#~\0\0");
    bytes
}

/// Записи рукопожатия TLS, какими их видит клиент: правильная оболочка записи
/// и внутри — начало `ServerHello`.
///
/// Полного рукопожатия здесь нет и быть не может: за `ServerHello` идёт
/// шифрованное, а ключ зависит от нашего же закрытого ключа. Поэтому образец
/// доходит до первой записи — ровно до той части, которую разбирают **до**
/// того, как поверили собеседнику, и именно она интересна.
#[must_use]
pub fn tls_records() -> Vec<Vec<u8>> {
    let mut hello = Vec::new();
    // `ServerHello`: версия, случайное, длина идентификатора сеанса,
    // шифронабор, сжатие, расширения.
    hello.extend_from_slice(&[0x03, 0x03]);
    hello.extend_from_slice(&[0x5a; 32]);
    hello.push(32);
    hello.extend_from_slice(&[0x5a; 32]);
    hello.extend_from_slice(&[0x13, 0x01]);
    hello.push(0);
    // Расширения: `supported_versions` и `key_share` с точкой X25519.
    let mut extensions = Vec::new();
    extensions.extend_from_slice(&[0x00, 0x2b, 0x00, 0x02, 0x03, 0x04]);
    extensions.extend_from_slice(&[0x00, 0x33, 0x00, 0x24, 0x00, 0x1d, 0x00, 0x20]);
    extensions.extend_from_slice(&[0x5a; 32]);
    hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
    hello.extend_from_slice(&extensions);

    let mut handshake = vec![0x02];
    handshake.extend_from_slice(&(hello.len() as u32).to_be_bytes()[1..]);
    handshake.extend_from_slice(&hello);

    let mut record = vec![0x16, 0x03, 0x03];
    record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
    record.extend_from_slice(&handshake);

    // Вторая запись — предупреждение о закрытии в открытом виде: короткая
    // оболочка, на которой проверяется арифметика длин.
    let alert = vec![0x15, 0x03, 0x03, 0x00, 0x02, 0x01, 0x00];
    vec![record, alert]
}

/// Правильные сообщения HTTP: запрос к серверу и ответ ему.
///
/// Оба короткие, и это здесь не экономия, а смысл: голова сообщения — это
/// текст, у которого разбор спотыкается не на длине, а на знаках. Порча
/// подставляет в них `\r`, `\n`, `%` и двоеточия в тех местах, где их быть не
/// должно, — то есть ровно то, чем ломают серверы в жизни.
#[must_use]
pub fn http_messages() -> Vec<Vec<u8>> {
    vec![
        b"GET /index.html?a=1 HTTP/1.1\r\nHost: freeos\r\nUser-Agent: curl/8.5.0\r\nAccept: */*\r\nConnection: keep-alive\r\n\r\n"
            .to_vec(),
        b"GET /%D0%BF%D1%83%D1%82%D1%8C/../file.txt HTTP/1.1\r\nHost: freeos\r\n\r\n".to_vec(),
        b"POST /upload HTTP/1.1\r\nHost: freeos\r\nContent-Length: 12\r\nContent-Type: text/plain\r\n\r\nhello world\n"
            .to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: text/html; charset=utf-8\r\nServer: nginx\r\n\r\nhello"
            .to_vec(),
        b"HTTP/1.1 302 Found\r\nLocation: http://elsewhere/x\r\nTransfer-Encoding: chunked\r\n\r\n"
            .to_vec(),
    ]
}
