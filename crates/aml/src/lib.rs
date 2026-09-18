// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Чтение тех объектов AML, без которых нельзя раздать прерывания устройствам
//! PCI: таблицы маршрутизации `_PRT` и устройств-связок, на которые она
//! ссылается.
//!
//! # Почему не интерпретатор
//!
//! AML — байт-код, и «правильный» способ узнать, что в нём написано, — его
//! исполнить. Полный интерпретатор AML — это ACPICA: сотни тысяч строк,
//! собственная объектная модель, области адресов, блокировки. Брать его целиком
//! ради одного вопроса «на какую линию подключён этот контроллер» — не размен, а
//! подмена задачи.
//!
//! Поэтому здесь **чтение, а не исполнение**, ровно как уже сделано для `_S5_` в
//! выключении питания: в потоке байт ищется объявление с нужным именем, и его
//! содержимое разбирается по правилам кодирования. Это работает, пока
//! интересующие нас объекты объявлены как данные, — и они объявлены, потому что
//! таблица маршрутизации у любой прошивки константа: она описывает разводку
//! платы, а разводка не считается во время работы.
//!
//! # Чего это не умеет, и это важно знать заранее
//!
//! - **Не исполняет методы.** Если прошивка объявила `_PRT` методом, читается не
//!   он, а таблица, которую он возвращает в режиме APIC. На машинах QEMU это
//!   `PRTA` — имя не выдумано, а взято из их собственных таблиц (см.
//!   [`APIC_TABLE`]). Прошивка, придумавшая другое имя, окажется неразобранной,
//!   и ядро об этом скажет, а не промолчит.
//! - **Не строит пространство имён.** Поиск линейный, по потоку байт, и
//!   опознаётся объявление по своему коду операции (`NameOp`, `DeviceOp`), а не
//!   по месту в дереве. Одноимённые объекты в разных областях видимости
//!   различить нельзя: берётся первый. Для `_PRT` это безвредно — шина PCI на
//!   машинах, которые нас интересуют, одна.
//! - **Не проверяет, что таблица описывает именно нулевую шину.** См. выше.
//!
//! Всё это записано не для оправдания, а чтобы следующий читатель не искал в
//! коде того, чего в нём нет.

#![no_std]

/// Имя таблицы маршрутизации в режиме APIC.
///
/// Прошивка, у которой `_PRT` — метод, обычно хранит два готовых пакета и
/// выбирает между ними по режиму контроллера прерываний: старый PIC и новый
/// APIC. Разбирать условие в методе мы не умеем, но и не нужно — режим выбираем
/// мы сами, и он всегда APIC.
///
/// Имя взято из таблиц QEMU (и i440fx, и Q35): там объявлены `PRTP` для PIC и
/// `PRTA` для APIC. Это соглашение, а не стандарт, — поэтому оно названо здесь
/// явно, а не спрятано в коде.
pub const APIC_TABLE: &[u8; 4] = b"PRTA";

/// Куда подключён один вывод прерывания одного устройства.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// Номер устройства на шине. Функция в `_PRT` не различается никогда:
    /// выводы прерываний общие у всех функций одного устройства.
    pub device: u8,
    /// Какой из четырёх выводов: 0 — INTA, 3 — INTD.
    pub pin: u8,
    /// Номер линии у контроллера прерываний.
    pub gsi: u32,
    /// Срабатывание по уровню, а не по фронту. У PCI всегда по уровню, но
    /// берётся это из дескриптора, а не из знания о PCI: прошивка описывает свою
    /// плату, и спорить с ней здесь не о чем.
    pub level: bool,
    /// Активный уровень — низкий.
    pub active_low: bool,
}

/// Что не получилось.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Ни `PRTA`, ни `_PRT` не объявлены данными.
    NoTable,
    /// Объявление есть, но за именем не пакет.
    NotAPackage,
    /// Байты кончились посреди объекта.
    Truncated,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoTable => f.write_str("no _PRT declared as data in this DSDT"),
            Self::NotAPackage => f.write_str("_PRT is declared, but not as a package"),
            Self::Truncated => f.write_str("the table ends in the middle of an object"),
        }
    }
}

/// Прочитать таблицу маршрутизации.
///
/// Записывает найденное в `out` и возвращает, сколько записей легло и сколько не
/// поместилось. Второе число — не роскошь: молча обрезанная таблица выглядит как
/// «у этого устройства нет прерывания», и искать такую причину пришлось бы долго.
///
/// # Errors
///
/// [`Error::NoTable`], если ни `PRTA`, ни `_PRT` не объявлены как данные.
pub fn routing(dsdt: &[u8], out: &mut [Route]) -> Result<(usize, usize), Error> {
    // Сначала — таблица режима APIC: если она есть, то `_PRT` наверняка метод, и
    // разбирать его нечем. Если её нет, `_PRT` объявлен данными, и он сам и
    // нужен.
    let package = find_name(dsdt, APIC_TABLE)
        .or_else(|| find_name(dsdt, b"_PRT"))
        .ok_or(Error::NoTable)?;

    let mut cursor = Cursor::new(dsdt, package);
    let mut entries = cursor.package_body()?;

    let mut written = 0usize;
    let mut dropped = 0usize;
    while let Some(entry) = entries.next_package() {
        let Some(route) = entry_to_route(dsdt, entry) else {
            continue;
        };
        if written < out.len() {
            out[written] = route;
            written += 1;
        } else {
            dropped += 1;
        }
    }
    Ok((written, dropped))
}

/// Разобрать одну запись `_PRT`: адрес, вывод, источник, номер в источнике.
fn entry_to_route(dsdt: &[u8], mut entry: Cursor<'_>) -> Option<Route> {
    let address = entry.integer()?;
    let pin = u8::try_from(entry.integer()?).ok()?;
    if pin > 3 {
        return None;
    }
    // Номер устройства лежит в старшем слове адреса; младшее — номер функции, и
    // в таблице оно всегда `0xFFFF`, то есть «все».
    let device = u8::try_from((address >> 16) & 0xFF).ok()?;

    // Источник — либо имя устройства-связки, либо ноль. Ноль означает «линия
    // названа прямо здесь», и тогда её номер лежит в следующем поле.
    match entry.source()? {
        Source::Direct => {
            let gsi = u32::try_from(entry.integer()?).ok()?;
            // Прямо названная линия описана только числом, поэтому вид
            // срабатывания берётся из того, как устроен PCI: уровень, активный
            // низкий. Это единственное место, где мы что-то знаем за прошивку, и
            // другого выбора она не оставила.
            Some(Route { device, pin, gsi, level: true, active_low: true })
        }
        Source::Link(name) => {
            let interrupt = link_interrupt(dsdt, &name)?;
            Some(Route {
                device,
                pin,
                gsi: interrupt.number,
                level: interrupt.level,
                active_low: interrupt.active_low,
            })
        }
    }
}

/// Что стоит в поле «источник» записи маршрутизации.
enum Source {
    /// Ноль: линия названа номером в следующем поле.
    Direct,
    /// Имя устройства-связки, у которого линию надо спросить.
    Link([u8; 4]),
}

/// Описание линии из дескриптора Extended Interrupt.
struct Interrupt {
    number: u32,
    level: bool,
    active_low: bool,
}

/// Спросить у устройства-связки, на какой линии оно сидит.
///
/// Берётся `_CRS` — «текущие ресурсы», то есть то, куда связка подключена
/// сейчас. Есть ещё `_PRS` («возможные»), и у прошивок QEMU они совпадают, но
/// совпадать не обязаны: `_PRS` перечисляет, куда связку **можно** переключить,
/// и читать его вместо `_CRS` значило бы взять первый вариант из списка вместо
/// действующего.
fn link_interrupt(dsdt: &[u8], name: &[u8; 4]) -> Option<Interrupt> {
    let body = find_device(dsdt, name)?;
    let crs = find_name_from(dsdt, body.start, body.end, b"_CRS")?;
    let mut cursor = Cursor::new(dsdt, crs);
    let buffer = cursor.buffer_body()?;
    extended_interrupt(buffer)
}

/// Найти в шаблоне ресурсов дескриптор Extended Interrupt и прочитать его.
///
/// Шаблон — это последовательность дескрипторов; нас интересует один, с тегом
/// `0x89`. Короткие дескрипторы (в том числе старый `IRQ`, тег `0x22`) здесь не
/// разбираются намеренно: они умеют называть только линии 0–15, а устройства PCI
/// на машинах с APIC сидят выше.
fn extended_interrupt(bytes: &[u8]) -> Option<Interrupt> {
    let mut at = 0usize;
    while at < bytes.len() {
        let tag = bytes[at];
        if tag == END_TAG {
            return None;
        }
        if tag & 0x80 == 0 {
            // Короткий дескриптор: длина в младших трёх битах тега.
            at += 1 + usize::from(tag & 0x07);
            continue;
        }
        // Длинный дескриптор: два байта длины за тегом.
        let len = usize::from(u16::from_le_bytes([*bytes.get(at + 1)?, *bytes.get(at + 2)?]));
        let body = bytes.get(at + 3..at + 3 + len)?;
        if tag == EXTENDED_INTERRUPT && body.len() >= 6 && body[1] >= 1 {
            let flags = body[0];
            let number = u32::from_le_bytes([body[2], body[3], body[4], body[5]]);
            return Some(Interrupt {
                // Бит 1: срабатывание. Ноль — по уровню, единица — по фронту.
                level: flags & 0b10 == 0,
                // Бит 2: активный уровень. Единица — низкий.
                active_low: flags & 0b100 != 0,
                number,
            });
        }
        at += 3 + len;
    }
    None
}

// --- поиск объявлений --------------------------------------------------------

/// Найти `Name(<имя>, ...)` и вернуть смещение того, что за именем.
///
/// Опознаётся именно объявление: байт `NameOp` (0x08), за ним ровно наше имя.
/// Совпадение четырёх байт само по себе ничего не значит — те же четыре байта
/// могут оказаться внутри строки или пакета.
fn find_name(dsdt: &[u8], name: &[u8; 4]) -> Option<usize> {
    find_name_from(dsdt, SDT_HEADER, dsdt.len(), name)
}

/// То же, но в заданных границах: нужно для `_CRS`, которых в таблице много, а
/// интересен тот, что внутри своего устройства.
fn find_name_from(dsdt: &[u8], from: usize, to: usize, name: &[u8; 4]) -> Option<usize> {
    let to = to.min(dsdt.len());
    let mut at = from;
    while at + 5 <= to {
        if dsdt[at] == OP_NAME && &dsdt[at + 1..at + 5] == name {
            return Some(at + 5);
        }
        at += 1;
    }
    None
}

/// Границы тела `Device(<имя>)`.
struct Body {
    start: usize,
    end: usize,
}

/// Найти `Device(<имя>)` и вернуть границы его тела.
fn find_device(dsdt: &[u8], name: &[u8; 4]) -> Option<Body> {
    let mut at = SDT_HEADER;
    while at + 3 <= dsdt.len() {
        if dsdt[at] == OP_EXT && dsdt[at + 1] == OP_DEVICE {
            let mut cursor = Cursor::new(dsdt, at + 2);
            if let Some(len) = cursor.pkg_length() {
                let after_length = cursor.at;
                if dsdt.get(after_length..after_length + 4) == Some(&name[..]) {
                    // Длина считается от первого байта самой длины.
                    let end = (at + 2 + len).min(dsdt.len());
                    return Some(Body { start: after_length + 4, end });
                }
            }
        }
        at += 1;
    }
    None
}

// --- разбор потока -----------------------------------------------------------

/// Длина заголовка таблицы ACPI: до него AML не начинается.
const SDT_HEADER: usize = 36;

/// Конец шаблона ресурсов.
const END_TAG: u8 = 0x79;
/// Дескриптор Extended Interrupt.
const EXTENDED_INTERRUPT: u8 = 0x89;

const OP_ZERO: u8 = 0x00;
const OP_ONE: u8 = 0x01;
const OP_NAME: u8 = 0x08;
const OP_BYTE: u8 = 0x0A;
const OP_WORD: u8 = 0x0B;
const OP_DWORD: u8 = 0x0C;
const OP_QWORD: u8 = 0x0E;
const OP_BUFFER: u8 = 0x11;
const OP_PACKAGE: u8 = 0x12;
const OP_VAR_PACKAGE: u8 = 0x13;
const OP_EXT: u8 = 0x5B;
const OP_DEVICE: u8 = 0x82;
const OP_ONES: u8 = 0xFF;

/// Читалка по потоку AML: помнит, где остановилась.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
    end: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Self { bytes, at, end: bytes.len() }
    }

    fn byte(&mut self) -> Option<u8> {
        if self.at >= self.end {
            return None;
        }
        let value = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(value)
    }

    /// Прочитать `PkgLength` — длину объекта вместе с самой длиной.
    ///
    /// Кодировка своеобразная: старшие два бита первого байта говорят, сколько
    /// байт идёт следом, и при их наличии из первого байта в дело идут только
    /// младшие четыре бита. Старшие два при этом **не** часть числа, и брать
    /// первый байт целиком — обычная ошибка, дающая правдоподобную длину.
    fn pkg_length(&mut self) -> Option<usize> {
        let lead = self.byte()?;
        let extra = usize::from(lead >> 6);
        if extra == 0 {
            return Some(usize::from(lead & 0x3F));
        }
        let mut value = usize::from(lead & 0x0F);
        for index in 0..extra {
            value |= usize::from(self.byte()?) << (4 + 8 * index);
        }
        Some(value)
    }

    /// Прочитать целую константу.
    fn integer(&mut self) -> Option<u64> {
        match self.byte()? {
            OP_ZERO => Some(0),
            OP_ONE => Some(1),
            OP_ONES => Some(u64::MAX),
            OP_BYTE => Some(u64::from(self.byte()?)),
            OP_WORD => {
                let low = self.byte()?;
                Some(u64::from(u16::from_le_bytes([low, self.byte()?])))
            }
            OP_DWORD => {
                let mut value = [0u8; 4];
                for slot in &mut value {
                    *slot = self.byte()?;
                }
                Some(u64::from(u32::from_le_bytes(value)))
            }
            OP_QWORD => {
                let mut value = [0u8; 8];
                for slot in &mut value {
                    *slot = self.byte()?;
                }
                Some(u64::from_le_bytes(value))
            }
            _ => None,
        }
    }

    /// Прочитать поле «источник» записи маршрутизации.
    fn source(&mut self) -> Option<Source> {
        let op = *self.bytes.get(self.at)?;
        if op == OP_ZERO {
            self.at += 1;
            return Some(Source::Direct);
        }
        // Имя. Полные имена с путями (обратная косая, шапочка, составные) здесь
        // не встречаются — связка объявлена рядом с таблицей, — но встретиться
        // могут, и тогда разбирать нечего: лучше отказаться от записи, чем
        // принять за имя первые четыре байта пути.
        let name = self.bytes.get(self.at..self.at + 4)?;
        let plain = name
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_');
        if !plain {
            return None;
        }
        self.at += 4;
        Some(Source::Link([name[0], name[1], name[2], name[3]]))
    }

    /// Войти внутрь пакета: вернуть читалку по его элементам.
    fn package_body(&mut self) -> Result<Cursor<'a>, Error> {
        let op = self.byte().ok_or(Error::Truncated)?;
        if op != OP_PACKAGE && op != OP_VAR_PACKAGE {
            return Err(Error::NotAPackage);
        }
        let start = self.at;
        let len = self.pkg_length().ok_or(Error::Truncated)?;
        // Число элементов: у обычного пакета это голый байт, у переменного —
        // выражение, и на практике константа. Отличие только в наличии префикса.
        if op == OP_PACKAGE {
            self.byte().ok_or(Error::Truncated)?;
        } else {
            self.integer().ok_or(Error::Truncated)?;
        }
        let end = (start + len).min(self.bytes.len());
        Ok(Cursor { bytes: self.bytes, at: self.at, end })
    }

    /// Войти внутрь буфера: вернуть его байты.
    fn buffer_body(&mut self) -> Option<&'a [u8]> {
        if self.byte()? != OP_BUFFER {
            return None;
        }
        let start = self.at;
        let len = self.pkg_length()?;
        // Размер буфера — выражение; нам достаточно константы.
        self.integer()?;
        let end = (start + len).min(self.bytes.len());
        self.bytes.get(self.at..end)
    }

    /// Следующий вложенный пакет, если он есть.
    fn next_package(&mut self) -> Option<Cursor<'a>> {
        while self.at < self.end {
            if self.bytes.get(self.at) == Some(&OP_PACKAGE) {
                let mut probe = Cursor { bytes: self.bytes, at: self.at, end: self.end };
                if let Ok(inner) = probe.package_body() {
                    self.at = inner.end;
                    return Some(inner);
                }
            }
            self.at += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests;
