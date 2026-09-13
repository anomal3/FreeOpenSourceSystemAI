//! Проверка тома: обход всех деревьев со сверкой всего, что можно сверить.
//!
//! # Зачем она, если суммы и так проверяются при чтении
//!
//! Потому что при чтении проверяется **прочитанное**. Файл, к которому полгода
//! никто не обращался, портится ровно так же, как тот, который читают каждый
//! день, — и узнать об этом при чтении можно только тогда, когда он
//! понадобился. Смысл контрольных сумм в том, чтобы узнать раньше.
//!
//! Поэтому здесь обход **всего**: каждый узел каждого дерева и каждый сектор
//! данных каждого файла.
//!
//! # Она ничего не чинит, и это не временно
//!
//! У `ext2` проверка умеет чинить (`Fix::Safe`), потому что там же живёт
//! писатель. Здесь писателя нет: том открывается только на чтение. Честный
//! ответ — «вот что не так», а чинить придётся `btrfs check --repair` на машине
//! с Linux, то есть тем же инструментом, который том создал.
//!
//! Пообещать починку, не умея писать, было бы хуже, чем не обещать: человек
//! запустил бы проверку и решил, что дело сделано.
//!
//! # Почему находок ограниченное число
//!
//! Испорченный том даёт их тысячами, а список живёт в памяти ядра. Предел —
//! не потеря: первая десятка говорит о причине ровно то же, что и тысяча, а
//! число остальных считается и называется отдельно.

use alloc::string::String;
use alloc::vec::Vec;

use disk::BlockDevice;

use crate::layout::*;
use crate::read::Btrfs;
use crate::tree::Cursor;
use crate::{Error, Result, try_zeroed};

/// Сколько находок держать в списке.
const MAX_PROBLEMS: usize = 32;

/// Сколько байт читать за раз при сверке данных.
///
/// Шестьдесят четыре килобайта — компромисс, названный числами: меньше значит
/// больше обращений к диску на тот же файл, больше — буфер, который в ядре
/// приходится где-то взять. На этом размере сверка четырёх мегабайт стоит
/// шестидесяти четырёх чтений вместо тысячи.
const SCAN_CHUNK: usize = 64 * 1024;

/// Что нашлось.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Узел дерева не читается или не сходится сам с собой.
    BadNode { logical: u64, why: Error },
    /// Ключи в дереве идут не по возрастанию.
    ///
    /// Это не косметика: весь поиск в дереве — двоичный, и на непорядке он
    /// находит не то, что есть, а то, что попалось. Ошибка при этом выглядит
    /// как «файла нет», а не как порча.
    KeysOutOfOrder { logical: u64 },
    /// Сектор данных не сошёлся со своей контрольной суммой.
    BadData { inode: u64, offset: u64 },
    /// У сектора данных нет записанной суммы, хотя файл на неё рассчитывает.
    MissingChecksum { inode: u64, offset: u64 },
    /// Запись каталога указывает на inode, которого в дереве нет.
    DanglingEntry { directory: u64, inode: u64 },
    /// Элемент дерева не разбирается.
    BadItem { logical: u64, why: Error },
}

impl core::fmt::Display for Problem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Problem::BadNode { logical, why } => {
                write!(f, "tree node at {logical} is unusable: {why}")
            }
            Problem::KeysOutOfOrder { logical } => {
                write!(f, "tree node at {logical} has its keys out of order")
            }
            Problem::BadData { inode, offset } => {
                write!(f, "inode {inode}: data at offset {offset} fails its checksum")
            }
            Problem::MissingChecksum { inode, offset } => {
                write!(f, "inode {inode}: data at offset {offset} has no checksum stored")
            }
            Problem::DanglingEntry { directory, inode } => {
                write!(f, "directory {directory} points at inode {inode}, which is not there")
            }
            Problem::BadItem { logical, why } => {
                write!(f, "tree node at {logical} holds an item this reader cannot parse: {why}")
            }
        }
    }
}

/// Итог проверки.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub problems: Vec<Problem>,
    /// Сколько находок не поместилось в список.
    pub dropped: usize,
    /// Сколько inode встретилось в дереве.
    pub inodes: u64,
    pub files: u64,
    pub directories: u64,
    /// Сколько элементов дерева ФС просмотрено.
    pub items: u64,
    /// Сколько секторов данных сверено с суммами.
    pub sectors: u64,
    /// Суммарный размер файлов по данным inode.
    pub bytes: u64,
}

impl Report {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty() && self.dropped == 0
    }

    fn note(&mut self, problem: Problem) {
        if self.problems.len() >= MAX_PROBLEMS
            || self.problems.try_reserve(1).is_err()
        {
            self.dropped += 1;
            return;
        }
        self.problems.push(problem);
    }
}

impl Btrfs {
    /// Обойти том целиком и сверить всё, что можно сверить.
    ///
    /// Читает **все** данные всех файлов: без этого проверка сводилась бы к
    /// метаданным, а порча данных — как раз то, чего ext2 не умеет замечать и
    /// ради чего здесь btrfs.
    ///
    /// # Проход один, и это измерено
    ///
    /// Первая версия делала два: обходила дерево, а потом читала каждый файл
    /// обычным [`Btrfs::read_at`]. Выглядело чище, стоило вчетверо: на каждый
    /// из двух тысяч файлов приходился свой спуск по дереву — а с ним копия
    /// двух узлов по 16 КиБ из кэша. Под эмулятором на aarch64 проверка
    /// занимала **четыре минуты** вместо одной.
    ///
    /// Теперь экстенты разбираются там же, где встречаются, — в единственном
    /// обходе. Побочная выгода крупнее исходной: встроенные экстенты
    /// пропускаются даром (их данные лежат в узле, чья сумма уже сверена), а
    /// раньше каждый такой файл всё равно читался.
    pub fn check(&mut self, dev: &mut dyn BlockDevice) -> Result<Report> {
        let mut report = Report::default();
        let mut scratch = try_zeroed(SCAN_CHUNK)?;
        // Обход по дереву сумм живёт весь проход и двигается вместе с данными.
        let mut csums = None;
        // Флаги inode, чьи экстенты идут следом. Порядок ключей это
        // гарантирует: INODE_ITEM (1) стоит раньше EXTENT_DATA (108) того же
        // объекта. Нужен ровно один бит — NODATASUM, при котором отсутствие
        // суммы законно.
        let mut owner = 0u64;
        let mut owner_flags = 0u64;

        let (root, level) = self.fs_root();
        let mut cursor = Cursor::seek(self.volume_mut(), dev, root, level, Key::new(0, 0, 0))?;

        // Ключи обязаны идти строго по возрастанию — и внутри листа, и через
        // границу листьев. Второе проверяется только сквозным обходом: внутри
        // одного узла порядок ещё может быть верным, а склейка — уже нет.
        let mut previous: Option<Key> = None;
        // Номера inode, на которые ссылаются каталоги, и номера, которые в
        // дереве действительно есть. Сверяются в конце: обход идёт по
        // возрастанию ключа, и каталог может встретиться раньше своего файла.
        let mut declared: Vec<(u64, u64)> = Vec::new();
        let mut present: Vec<u64> = Vec::new();

        while let Some(key) = cursor.key() {
            if let Some(last) = previous {
                if key <= last {
                    report.note(Problem::KeysOutOfOrder { logical: cursor.leaf_address() });
                }
            }
            previous = Some(key);
            report.items += 1;

            let raw = match cursor.item() {
                Ok(raw) => raw,
                Err(why) => {
                    report.note(Problem::BadItem { logical: key.objectid, why });
                    break;
                }
            };

            match key.kind {
                item_type::INODE_ITEM => {
                    report.inodes += 1;
                    if present.try_reserve(1).is_ok() {
                        present.push(key.objectid);
                    }
                    if raw.len() >= INODE_ITEM_SIZE {
                        owner = key.objectid;
                        owner_flags = u64_at(raw, INODE_FLAGS);
                        let mode = u32_at(raw, INODE_MODE);
                        match mode & MODE_FORMAT_MASK {
                            MODE_DIRECTORY => report.directories += 1,
                            MODE_REGULAR => {
                                report.files += 1;
                                report.bytes += u64_at(raw, INODE_SIZE_FIELD);
                            }
                            _ => {}
                        }
                    }
                }
                item_type::EXTENT_DATA => {
                    // Сверять данные можно только у экстента, который знает,
                    // где его байты и какой суммой они накрыты. Встроенный
                    // лежит в узле — его сумма уже сверена при чтении узла;
                    // предвыделенный данных ещё не содержит; файл с
                    // NODATASUM отказался от сумм сам.
                    let verify = owner == key.objectid
                        && owner_flags & INODE_FLAG_NODATASUM == 0;
                    if verify {
                        if let Some((logical, bytes)) = extent_data_range(raw) {
                            match self.verify_range(dev, logical, bytes, &mut scratch, &mut csums) {
                                Ok(()) => {
                                    report.sectors +=
                                        bytes / u64::from(self.sector_size());
                                }
                                Err(Error::BadChecksum) => report.note(Problem::BadData {
                                    inode: key.objectid,
                                    offset: key.offset,
                                }),
                                Err(Error::Corrupt) => report.note(Problem::MissingChecksum {
                                    inode: key.objectid,
                                    offset: key.offset,
                                }),
                                Err(why) => report.note(Problem::BadItem {
                                    logical: cursor.leaf_address(),
                                    why,
                                }),
                            }
                        }
                    }
                }
                item_type::DIR_INDEX => {
                    if raw.len() >= DIR_ITEM_HEAD_SIZE {
                        let location = Key::parse(raw, DIR_ITEM_LOCATION);
                        if location.kind == item_type::INODE_ITEM
                            && declared.try_reserve(1).is_ok()
                        {
                            declared.push((key.objectid, location.objectid));
                        }
                    }
                }
                _ => {}
            }
            // Отказ на переходе к следующему элементу — это испорченный узел
            // дерева, и обход дальше не пойдёт. Но проверку это не отменяет:
            // данные файлов, до которых мы уже дошли, всё ещё можно сверить, и
            // отчёт «дерево оборвалось здесь, а файлы такие-то» полезнее, чем
            // одна ошибка вместо отчёта.
            if let Err(why) = cursor.next(self.volume_mut(), dev) {
                report.note(Problem::BadNode { logical: cursor.leaf_address(), why });
                break;
            }
        }

        present.sort_unstable();
        present.dedup();
        for (directory, inode) in declared {
            if present.binary_search(&inode).is_err() {
                report.note(Problem::DanglingEntry { directory, inode });
            }
        }

        Ok(report)
    }

}

/// Где лежат байты экстента и сколько их.
///
/// `None` — у экстента нет данных на диске: он встроенный, предвыделенный
/// или описывает дыру. Сверять там нечего, и это не находка.
fn extent_data_range(raw: &[u8]) -> Option<(u64, u64)> {
    if raw.len() < EXTENT_REGULAR_SIZE || raw[EXTENT_TYPE] != EXTENT_TYPE_REGULAR {
        return None;
    }
    // Сжатый экстент сверить нечем: сумма накрывает сжатые байты, а мы не
    // умеем их разжать, чтобы сказать про содержимое хоть что-то осмысленное.
    if raw[EXTENT_COMPRESSION] != 0 || raw[EXTENT_ENCRYPTION] != 0 {
        return None;
    }
    let disk_bytenr = u64_at(raw, EXTENT_DISK_BYTENR);
    if disk_bytenr == 0 {
        return None;
    }
    let offset = u64_at(raw, EXTENT_OFFSET);
    let bytes = u64_at(raw, EXTENT_NUM_BYTES);
    if bytes == 0 { None } else { Some((disk_bytenr + offset, bytes)) }
}

/// Текст находок — то, что показывают человеку.
///
/// Собирается здесь, а не в ядре: там нет причины знать, как устроен `Problem`,
/// а формат сообщения — часть этого крейта.
#[must_use]
pub fn describe(problem: &Problem) -> Result<String> {
    let mut text = String::new();
    core::fmt::Write::write_fmt(&mut text, format_args!("{problem}"))
        .map_err(|_| Error::NoMemory)?;
    Ok(text)
}
