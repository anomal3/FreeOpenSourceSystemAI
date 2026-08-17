//! Чтение Flattened Device Tree — описания машины, которое отдаёт загрузчик
//! телефона вместо ACPI.
//!
//! # Зачем это здесь
//!
//! До сих пор ядро узнавало машину двумя способами: из `BootInfo`, который
//! составил наш UEFI-загрузчик, и из таблиц ACPI, на которые тот же `BootInfo`
//! указывает. На телефоне нет ни того, ни другого. Заводской загрузчик (LK у
//! MediaTek) входит в ядро по договору Linux: MMU выключен, а в `x0` лежит
//! адрес дерева устройств. Всё, что ядру нужно знать о машине — сколько памяти
//! и где, по какому адресу контроллер прерываний, куда писать в UART, где
//! кадровый буфер, — лежит в этом дереве.
//!
//! # Почему свой разборщик, а не готовый крейт
//!
//! По той же причине, по которой в проекте свои ext2, TLS и SSH: формат
//! описан на четырёх страницах, а чужой крейт — это чужие решения о том, что
//! делать с испорченными данными. Дерево приходит из-за границы доверия (его
//! составил чужой загрузчик), и здесь нет ни одного места, где неверная длина
//! или смещение за концом буфера превращались бы во что-то, кроме `None`.
//!
//! # Устройство формата, в двух абзацах
//!
//! Файл начинается заголовком из десяти 32-битных чисел **со старшим байтом
//! вперёд** — big-endian, всегда, независимо от машины. Заголовок указывает на
//! два блока: дерево (`struct`) и склад строк (`strings`).
//!
//! Дерево — это поток 32-битных меток: `BEGIN_NODE` (за ней имя узла, строкой с
//! нулём, дополненное до кратности четырём), `PROP` (за ней длина и смещение
//! имени в складе строк, затем значение), `END_NODE`, `NOP` и `END`. Никаких
//! указателей внутрь: чтобы найти узел, поток читают с начала. Это и есть
//! причина, по которой здесь всё — итераторы, а не таблицы: строить оглавление
//! означало бы просить память до того, как выяснили, сколько её в машине.

#![no_std]
#![forbid(unsafe_op_in_unsafe_fn)]

/// Метка в начале дерева: `d00dfeed`, «dood feed».
const MAGIC: u32 = 0xd00d_feed;

/// Версия формата, которую понимает этот разборщик.
///
/// Проверяется `last_comp_version`, а не `version`: дерево версии 17 обязано
/// читаться разборщиком версии 16, и наоборот — дерево, объявившее, что с
/// шестнадцатой оно несовместимо, читать нельзя даже если поля на месте.
const SUPPORTED_VERSION: u32 = 16;

/// Метки потока.
const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

/// Разобранное дерево устройств.
///
/// Держит только срез памяти: ни одного выделения. Дерево живёт там, куда его
/// положил загрузчик, и переписывать его в кучу незачем — тем более что куча
/// заводится **после** того, как из дерева прочитают карту памяти.
#[derive(Clone, Copy)]
pub struct Fdt<'a> {
    /// Блок дерева.
    structure: &'a [u8],
    /// Склад строк: имена свойств.
    strings: &'a [u8],
}

impl<'a> Fdt<'a> {
    /// Разобрать дерево, лежащее по этому срезу.
    ///
    /// `None`, если это не дерево, если версия чужая или если заголовок
    /// указывает за пределы среза. Ни одна из этих проверок не лишняя: адрес
    /// пришёл в регистре от чужого загрузчика, и «похоже на дерево» — это не
    /// то, на чём стоит строить карту памяти.
    #[must_use]
    pub fn new(blob: &'a [u8]) -> Option<Self> {
        let header: [u32; 10] = {
            let mut out = [0u32; 10];
            for (index, slot) in out.iter_mut().enumerate() {
                *slot = be32(blob, index * 4)?;
            }
            out
        };
        if header[0] != MAGIC {
            return None;
        }
        let total = header[1] as usize;
        let off_struct = header[2] as usize;
        let off_strings = header[3] as usize;
        let last_comp_version = header[6];
        let size_strings = header[8] as usize;
        let size_struct = header[9] as usize;

        if last_comp_version > SUPPORTED_VERSION {
            return None;
        }
        // Заявленный размер обязан помещаться в то, что нам дали, а оба блока —
        // в заявленный размер. Складывать без проверки переполнения нельзя:
        // числа пришли из файла.
        if total > blob.len() {
            return None;
        }
        let struct_end = off_struct.checked_add(size_struct)?;
        let strings_end = off_strings.checked_add(size_strings)?;
        if struct_end > total || strings_end > total {
            return None;
        }

        Some(Self {
            structure: blob.get(off_struct..struct_end)?,
            strings: blob.get(off_strings..strings_end)?,
        })
    }

    /// Разобрать дерево по адресу, полученному от загрузчика.
    ///
    /// Длина читается из самого дерева: другого источника нет — загрузчик
    /// передаёт один указатель. Поэтому сначала читается заголовок (сорок
    /// байт), из него берётся `totalsize`, и только потом образуется срез на
    /// всю длину.
    ///
    /// # Safety
    ///
    /// `ptr` обязан указывать на отображённую память, в которой доступно хотя
    /// бы сорок байт; дальше длина проверяется по заголовку. Вызывающий
    /// отвечает и за то, что дерево никто не переписывает, пока им пользуются.
    #[must_use]
    pub unsafe fn from_ptr(ptr: *const u8) -> Option<Self> {
        if ptr.is_null() || (ptr as usize) % 8 != 0 {
            // Выравнивание — часть договора: дерево кладут по восьми байтам, и
            // указатель, не кратный восьми, означает, что в регистре не то, что
            // мы думаем.
            return None;
        }
        // SAFETY: контракт функции обещает сорок доступных байт.
        let head = unsafe { core::slice::from_raw_parts(ptr, 40) };
        if be32(head, 0)? != MAGIC {
            return None;
        }
        let total = be32(head, 4)? as usize;
        if total < 40 || total > MAX_BLOB {
            return None;
        }
        // SAFETY: длина прочитана из заголовка, который только что признан
        // нашим, и ограничена сверху [`MAX_BLOB`].
        let blob = unsafe { core::slice::from_raw_parts(ptr, total) };
        Self::new(blob)
    }

    /// Пройти по всем узлам дерева сверху вниз.
    #[must_use]
    pub fn nodes(&self) -> Nodes<'a> {
        Nodes { structure: self.structure, strings: self.strings, offset: 0, depth: 0 }
    }

    /// Найти узел по полному пути, например `/chosen` или `/soc/serial@11002000`.
    ///
    /// Путь сравнивается по звеньям, и звено совпадает либо целиком, либо до
    /// `@`: в дереве узел зовётся `memory@40000000`, а в тексте про него пишут
    /// `/memory`. Требовать от вызывающего знать адрес значило бы прибивать
    /// код к конкретной машине — ровно к тому, от чего дерево и спасает.
    #[must_use]
    pub fn find(&self, path: &str) -> Option<Node<'a>> {
        let mut wanted = path.split('/').filter(|part| !part.is_empty());
        let mut want = wanted.next();
        // Корень пути (`/`) — это узел глубины ноль с пустым именем.
        if want.is_none() {
            return self.nodes().next();
        }
        let mut depth_wanted = 1;
        for node in self.nodes() {
            if node.depth != depth_wanted {
                continue;
            }
            let Some(part) = want else {
                break;
            };
            if !name_matches(node.name, part) {
                continue;
            }
            want = wanted.next();
            if want.is_none() {
                return Some(node);
            }
            depth_wanted += 1;
        }
        None
    }

    /// Найти первый узел, чей `compatible` содержит эту строку.
    ///
    /// Именно «содержит»: `compatible` — это список строк от частного к общему
    /// (`"mediatek,mt6758-uart", "mediatek,mt6577-uart"`), и драйвер обязан
    /// узнавать себя по любой из них, иначе он не запустится на соседней
    /// машине того же семейства.
    #[must_use]
    pub fn find_compatible(&self, with: &str) -> Option<Node<'a>> {
        self.nodes().find(|node| node.is_compatible(with))
    }
}

/// Предел на размер дерева, принимаемого по указателю.
///
/// Два мегабайта — с запасом: настоящие деревья телефонов не доходят и до
/// двухсот килобайт. Предел существует не ради экономии, а потому, что длина
/// пришла из-за границы доверия: без него испорченный заголовок превратился бы
/// в срез на четыре гигабайта и в чтение чужой памяти.
const MAX_BLOB: usize = 2 * 1024 * 1024;

/// Узел дерева.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    /// Имя вместе с адресом (`memory@40000000`), без пути.
    pub name: &'a str,
    /// Глубина: у корня ноль.
    pub depth: usize,
    /// Свойства этого узла.
    properties: &'a [u8],
    strings: &'a [u8],
}

impl<'a> Node<'a> {
    /// Свойства узла по порядку.
    #[must_use]
    pub fn properties(&self) -> Properties<'a> {
        Properties { structure: self.properties, strings: self.strings, offset: 0 }
    }

    /// Значение свойства с таким именем.
    #[must_use]
    pub fn property(&self, name: &str) -> Option<&'a [u8]> {
        self.properties()
            .find(|property| property.name == name)
            .map(|property| property.value)
    }

    /// Свойство как число: 32 или 64 бита, в зависимости от длины.
    ///
    /// Обе длины разбираются одним методом намеренно: одно и то же свойство
    /// (`reg`, адрес) в разных деревьях лежит и четырьмя байтами, и восемью, а
    /// вызывающему нужен адрес, а не разрядность записи.
    #[must_use]
    pub fn property_u64(&self, name: &str) -> Option<u64> {
        match self.property(name)? {
            value if value.len() == 4 => Some(u64::from(be32(value, 0)?)),
            value if value.len() == 8 => Some(be64(value, 0)?),
            _ => None,
        }
    }

    /// Свойство как строка без завершающего нуля.
    #[must_use]
    pub fn property_str(&self, name: &str) -> Option<&'a str> {
        let value = self.property(name)?;
        let end = value.iter().position(|byte| *byte == 0).unwrap_or(value.len());
        core::str::from_utf8(&value[..end]).ok()
    }

    /// Перечислить строки списочного свойства (`compatible`).
    #[must_use]
    pub fn strings(&self, name: &str) -> StringList<'a> {
        StringList { bytes: self.property(name).unwrap_or(&[]) }
    }

    /// Объявляет ли узел совместимость с этим именем.
    #[must_use]
    pub fn is_compatible(&self, with: &str) -> bool {
        self.strings("compatible").any(|value| value == with)
    }

    /// Разобрать `reg` как пары «адрес, длина».
    ///
    /// Сколько ячеек занимают адрес и длина, знает не сам узел, а его родитель
    /// (`#address-cells`, `#size-cells`), поэтому их приходится передавать
    /// снаружи. Это не неудобство ради чистоты: узел, прочитавший `reg` с
    /// чужими размерами ячеек, получит правдоподобный и неверный адрес — самый
    /// дорогой вид ошибки в этом формате.
    #[must_use]
    pub fn reg(&self, address_cells: usize, size_cells: usize) -> Regions<'a> {
        Regions {
            bytes: self.property("reg").unwrap_or(&[]),
            offset: 0,
            address_cells,
            size_cells,
        }
    }
}

/// Свойство: имя и значение как есть.
#[derive(Clone, Copy)]
pub struct Property<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

/// Обход узлов дерева.
pub struct Nodes<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
    offset: usize,
    depth: usize,
}

impl<'a> Iterator for Nodes<'a> {
    type Item = Node<'a>;

    fn next(&mut self) -> Option<Node<'a>> {
        loop {
            let token = be32(self.structure, self.offset)?;
            match token {
                FDT_NOP => self.offset += 4,
                FDT_END => return None,
                FDT_END_NODE => {
                    self.offset += 4;
                    // Глубина уходит в минус только на испорченном дереве;
                    // тогда обход просто кончается, а не считает по кругу.
                    self.depth = self.depth.checked_sub(1)?;
                }
                FDT_BEGIN_NODE => {
                    let name_at = self.offset + 4;
                    let name = cstr(self.structure, name_at)?;
                    let properties_at = align4(name_at + name.len() + 1);
                    let depth = self.depth;
                    self.offset = properties_at;
                    self.depth += 1;
                    return Some(Node {
                        name,
                        depth,
                        properties: self.structure.get(properties_at..)?,
                        strings: self.strings,
                    });
                }
                FDT_PROP => {
                    let len = be32(self.structure, self.offset + 4)? as usize;
                    self.offset = align4(self.offset + 12 + len);
                }
                // Незнакомая метка — это не «пропустим и пойдём дальше»: шаг
                // потока задаётся самой меткой, и не зная её, идти некуда.
                _ => return None,
            }
        }
    }
}

/// Обход свойств одного узла.
pub struct Properties<'a> {
    structure: &'a [u8],
    strings: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for Properties<'a> {
    type Item = Property<'a>;

    fn next(&mut self) -> Option<Property<'a>> {
        loop {
            let token = be32(self.structure, self.offset)?;
            match token {
                FDT_NOP => self.offset += 4,
                // Свойства узла кончаются на первом же вложенном узле или на
                // его собственном конце: в дереве они всегда идут подряд сразу
                // за именем.
                FDT_BEGIN_NODE | FDT_END_NODE | FDT_END => return None,
                FDT_PROP => {
                    let len = be32(self.structure, self.offset + 4)? as usize;
                    let name_offset = be32(self.structure, self.offset + 8)? as usize;
                    let value_at = self.offset + 12;
                    let value = self.structure.get(value_at..value_at + len)?;
                    self.offset = align4(value_at + len);
                    return Some(Property {
                        name: cstr(self.strings, name_offset)?,
                        value,
                    });
                }
                _ => return None,
            }
        }
    }
}

/// Перечисление строк списочного свойства.
pub struct StringList<'a> {
    bytes: &'a [u8],
}

impl<'a> Iterator for StringList<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.bytes.is_empty() {
            return None;
        }
        let end = self.bytes.iter().position(|byte| *byte == 0)?;
        let (head, tail) = self.bytes.split_at(end);
        self.bytes = tail.get(1..).unwrap_or(&[]);
        core::str::from_utf8(head).ok()
    }
}

/// Область памяти из свойства `reg`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub address: u64,
    pub size: u64,
}

/// Перечисление областей `reg`.
pub struct Regions<'a> {
    bytes: &'a [u8],
    offset: usize,
    address_cells: usize,
    size_cells: usize,
}

impl Iterator for Regions<'_> {
    type Item = Region;

    fn next(&mut self) -> Option<Region> {
        let address = self.cells(self.address_cells)?;
        let size = self.cells(self.size_cells)?;
        Some(Region { address, size })
    }
}

impl Regions<'_> {
    /// Прочитать число из `count` ячеек по четыре байта.
    ///
    /// Ячеек бывает одна (32 бита) и две (64); три и больше формат допускает, а
    /// смысла в них для адреса памяти нет — такое дерево мы читать отказываемся,
    /// а не берём младшие разряды наугад.
    fn cells(&mut self, count: usize) -> Option<u64> {
        if count == 0 || count > 2 {
            return None;
        }
        let mut value = 0u64;
        for _ in 0..count {
            value = (value << 32) | u64::from(be32(self.bytes, self.offset)?);
            self.offset += 4;
        }
        Some(value)
    }
}

/// Совпадает ли имя узла с звеном пути.
///
/// `memory@40000000` совпадает и с `memory`, и с самим собой целиком.
fn name_matches(name: &str, wanted: &str) -> bool {
    if name == wanted {
        return true;
    }
    match name.split_once('@') {
        Some((base, _)) => base == wanted,
        None => false,
    }
}

/// Прочитать 32 бита со старшим байтом вперёд.
fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Прочитать 64 бита со старшим байтом вперёд.
fn be64(bytes: &[u8], at: usize) -> Option<u64> {
    Some((u64::from(be32(bytes, at)?) << 32) | u64::from(be32(bytes, at + 4)?))
}

/// Строка с завершающим нулём, начиная с этого смещения.
fn cstr(bytes: &[u8], at: usize) -> Option<&str> {
    let tail = bytes.get(at..)?;
    let end = tail.iter().position(|byte| *byte == 0)?;
    core::str::from_utf8(&tail[..end]).ok()
}

/// Ближайшее сверху кратное четырём: поток дерева выровнен по ячейке.
const fn align4(value: usize) -> usize {
    (value + 3) & !3
}
