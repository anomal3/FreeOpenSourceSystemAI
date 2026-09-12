//! Открытые файлы программы.
//!
//! # Таблица принадлежит программе, а не системе
//!
//! С Phase 13a программ одновременно бывает несколько, и таблица у каждой своя:
//! она живёт в [`super::Program`], то есть в задаче. Общая таблица означала бы,
//! что дескриптор, выданный одной программе, виден другой — доступ, права на
//! который проверялись не для неё.
//!
//! Уничтожается таблица вместе с программой, на всех путях выхода, включая
//! отказ.
//!
//! # Почему предел маленький
//!
//! [`MAX_OPEN_FILES`] — восемь. Таблица не растёт, и это записано в договоре с
//! программой: вызов, который иногда отвечает «слишком много открытых файлов»,
//! понятнее вызова, который иногда съедает кучу ядра. Узел ext2 держит `Arc` на
//! том и разобранный inode — сотни байт, но выделяет их куча ядра, а не
//! программа, и предела у неё нет.

use core::sync::atomic::{AtomicU64, Ordering};

use alloc::sync::Arc;
use alloc::vec::Vec;

use user_abi::{FD_FIRST, MAX_OPEN_FILES, O_CREATE, O_TRUNC, O_WRITE};

/// Права, с которыми создаётся файл по [`O_CREATE`].
///
/// Постоянная, а не аргумент вызова: `umask` и режим создания — это уже
/// политика, а её место там, где есть кому её задавать. Читать всем, писать
/// владельцу — то же, что даёт `touch` под обычным `umask` 022.
const DEFAULT_MODE: u16 = 0o644;

use crate::vfs::perm::{Access, Credentials};
use crate::vfs::{DirEntry, Node, NodeKind, VfsError};

use super::pipe::{self, PipeError};

/// Открытый файл: узел, то, докуда программа его дочитала, и можно ли в него
/// писать.
struct Open {
    /// Узел, а не путь: путь может быть переименован под руками, открытый файл
    /// — нет.
    ///
    /// `Arc`, а не `Box`, с фазы 41: отображение файла в память живёт своей
    /// жизнью и обязано пережить закрытие дескриптора. Разделяемое владение
    /// здесь — не удобство, а само обещание `SYS_MMAP_FILE`: программа,
    /// отобразившая файл, закрывает дескриптор сразу же, потому что он ей
    /// больше не нужен, и страницы обязаны продолжать подкачиваться.
    node: Arc<dyn Node>,
    /// Докуда программа дочитала — и **общее** у всех копий дескриптора.
    ///
    /// Атомик, а не обычное число, с фазы 44: `SYS_DUP` выдаёт второй номер на
    /// тот же открытый файл, и позиция у них обязана быть одна. Отдельные
    /// позиции выглядели бы как работающий `dup` ровно до первой дописи в
    /// конец — и это тот род ошибки, который находят не в отладчике, а в
    /// испорченном файле.
    ///
    /// Взаимного исключения этот атомик **не** даёт и не обязан: «прочитать и
    /// сдвинуть» — две операции, и атомарны они не сами по себе, а потому, что
    /// обе идут под локом программы ([`super::with_current`]). Двух задач у
    /// одной таблицы не бывает: дескрипторы не наследуются при запуске (см.
    /// договор `SYS_SPAWN`), так что делить позицию может только сама программа
    /// с собой.
    offset: AtomicU64,
    /// Право писать спрошено при открытии и запомнено здесь. Перепроверять его
    /// на каждой записи не нужно и неверно: в Unix смена прав не отбирает уже
    /// открытый файл, и ровно на это рассчитывает всякий, кто держит файл
    /// открытым дольше одной операции.
    writable: bool,
    /// Снимок каталога, если открыт каталог. У файла — `None`.
    ///
    /// Список читается один раз, при открытии, и дальше не обновляется. Это не
    /// экономия, а обещание, записанное в договоре: перечисление, которое
    /// меняется под руками у того, кто его читает, невозможно ни закончить, ни
    /// объяснить — POSIX решает это тем же снимком. Заодно исчезает
    /// квадратичность: иначе каждая запись стоила бы полного перечисления, а
    /// каждое перечисление — чтения inode на каждое имя.
    entries: Option<Vec<DirEntry>>,
}

/// Что лежит в месте таблицы.
///
/// Файл или конец канала — и это единственное место, где они различаются.
/// Дальше по коду дескриптор остаётся числом, а `read` и `write` спрашивают у
/// таблицы, куда именно идти. Иначе программе пришлось бы знать, чем её
/// стандартный ввод оказался сегодня, — а весь смысл канала в том, что не
/// приходится.
enum Slot {
    /// Открытый файл. `Arc`, потому что дескрипторов на него бывает несколько:
    /// `SYS_DUP` кладёт в другое место таблицы ссылку на **тот же** `Open`, и
    /// закрытие одного номера оставляет файл живым, пока цел хоть один.
    File(Arc<Open>),
    /// Читающий конец: из него берёт `read`.
    PipeRead(pipe::Reader),
    /// Пишущий конец: в него отдаёт `write`.
    PipeWrite(pipe::Writer),
}

/// Почему не получилось.
#[derive(Debug, Clone, Copy)]
pub enum FileError {
    /// Файловая система не смонтирована.
    NoFilesystem,
    /// Дескриптора с таким номером у программы нет.
    BadFd,
    /// Все [`MAX_OPEN_FILES`] мест заняты.
    TooManyFiles,
    /// Отказала файловая система — включая отказ в правах.
    Vfs(VfsError),
    /// Позиция ушла за пределы того, что представимо: до начала файла или за
    /// границу 64 бит.
    BadOffset,
    /// У конца канала нет ни позиции, ни списка имён: `seek` и перечисление к
    /// нему не относятся.
    NotSeekable,
    /// Канал сказал своё: писать некому либо прямо сейчас нечего читать.
    Pipe(PipeError),
}

/// Чем оказался дескриптор — ответ [`Table::describe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FdKind {
    File,
    Directory,
    /// Конец канала. Отдельный вид, а не «файл нулевой длины»: библиотека
    /// выбирает по нему способ буферизации, и труба, назвавшаяся файлом,
    /// получила бы вывод, который не появляется, пока не наберётся килобайт.
    Pipe,
}

/// Что ядро знает об открытом дескрипторе — ответ [`Table::describe`].
#[derive(Debug, Clone, Copy)]
pub struct FdInfo {
    pub size: u64,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub kind: FdKind,
}

/// От чего считается смещение в [`Table::seek`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whence {
    /// От начала файла.
    Set,
    /// От текущей позиции.
    Current,
    /// От конца файла.
    End,
}

/// Таблица дескрипторов одной программы. Место `i` — это дескриптор
/// `i + FD_FIRST`.
pub struct Table {
    slots: [Option<Slot>; MAX_OPEN_FILES],
}

impl Table {
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: [const { None }; MAX_OPEN_FILES] }
    }

    /// Открыть файл от имени `cred`.
    ///
    /// Права проверяются здесь и только здесь: дальше дескриптор уже открыт, и
    /// перепроверять его на каждом чтении не нужно — ровно так же, как в Unix,
    /// где смена прав не отбирает уже открытый файл.
    pub fn open(&mut self, cred: Credentials, path: &str, flags: usize) -> Result<usize, FileError> {
        let writable = flags & O_WRITE != 0;
        let want = if writable { Access::WRITE } else { Access::READ };

        let node = match crate::fs::resolve_as(cred, path, want) {
            Some(Ok(node)) => node,
            // Файла нет, но просили создать. Создаём — от имени того же
            // сеанса и в том каталоге, который назвал путь; право писать в этот
            // каталог спросит `create_as`.
            Some(Err(VfsError::NotFound)) if flags & O_CREATE != 0 => {
                crate::fs::create_as(cred, path, DEFAULT_MODE)
                    .ok_or(FileError::NoFilesystem)?
                    .map_err(FileError::Vfs)?
            }
            Some(Err(err)) => return Err(FileError::Vfs(err)),
            None => return Err(FileError::NoFilesystem),
        };

        // Каталог открывается на перечисление, и только на него: писать в
        // каталог программе нечем — имена в нём заводит `mkdir`, а не запись
        // байтов, — и открытый на запись каталог был бы дескриптором, любое
        // использование которого ошибка.
        let directory = node.metadata().kind == NodeKind::Directory;
        if directory && writable {
            return Err(FileError::Vfs(VfsError::WrongKind));
        }

        // Список читается сразу: снимок фиксируется в момент открытия. См.
        // `Open::entries` — там же сказано, почему это обещание, а не экономия.
        let entries = if directory {
            Some(node.list().map_err(FileError::Vfs)?)
        } else {
            None
        };

        if flags & O_TRUNC != 0 && writable {
            node.truncate(0).map_err(FileError::Vfs)?;
        }

        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(FileError::TooManyFiles)?;
        self.slots[slot] = Some(Slot::File(Arc::new(Open {
            node: Arc::from(node),
            offset: AtomicU64::new(0),
            writable,
            entries,
        })));
        Ok(slot + FD_FIRST)
    }

    /// Создать файл с заданными правами и открыть его на запись.
    ///
    /// Отличается от [`Table::open`] с `O_CREATE` двумя вещами, и обе намеренны:
    /// права берутся у вызывающего, а занятое имя — отказ, а не «обрежу». См.
    /// `SYS_CREATE` в договоре, где сказано, кому это нужно и почему.
    ///
    /// Права обрезаются до девяти бит **здесь**, а не у вызывающего: число
    /// пришло из третьего кольца, и тип узла задаёт файловая система, а не
    /// программа.
    pub fn create(
        &mut self,
        cred: Credentials,
        path: &str,
        mode: u16,
    ) -> Result<usize, FileError> {
        let node = crate::fs::create_as(cred, path, mode & 0o777)
            .ok_or(FileError::NoFilesystem)?
            .map_err(FileError::Vfs)?;

        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(FileError::TooManyFiles)?;
        self.slots[slot] = Some(Slot::File(Arc::new(Open {
            node: Arc::from(node),
            offset: AtomicU64::new(0),
            writable: true,
            entries: None,
        })));
        Ok(slot + FD_FIRST)
    }

    /// Записать в дескриптор. Возвращает, сколько записано.
    pub fn write(&mut self, fd: usize, data: &[u8]) -> Result<usize, FileError> {
        let index = index_of(fd)?;
        match self.slots[index].as_mut().ok_or(FileError::BadFd)? {
            Slot::File(open) => {
                if !open.writable {
                    return Err(FileError::Vfs(VfsError::PermissionDenied));
                }
                let at = open.offset.load(Ordering::Relaxed);
                let written = open.node.write_at(at, data).map_err(FileError::Vfs)?;
                open.offset.store(at + written as u64, Ordering::Relaxed);
                Ok(written)
            }
            // Запись в канал через дескриптор **не ждёт** места, и это не
            // упрощение. Таблица дескрипторов лежит под локом всей программы:
            // задача, уснувшая с ним в руках, останавливает вместе с собой
            // всякого, кто в эту таблицу заглянет. Ждать умеет стандартный
            // вывод (см. `Program::stdout`) — он живёт не в таблице.
            Slot::PipeWrite(writer) => writer.write(data, false).map_err(FileError::Pipe),
            Slot::PipeRead(_) => Err(FileError::Vfs(VfsError::PermissionDenied)),
        }
    }

    /// Взять очередную запись каталога. `None` — записи кончились.
    ///
    /// Позиция та же, что у файла: перечисление проходит каталог один раз, и
    /// заводить для него второй счётчик значило бы объяснять, какой из двух
    /// двигает `seek`.
    pub fn next_entry(&mut self, fd: usize) -> Result<Option<DirEntry>, FileError> {
        let index = index_of(fd)?;
        let open = Self::as_file(self.slots[index].as_mut())?;
        let entries = open.entries.as_ref().ok_or(FileError::Vfs(VfsError::WrongKind))?;

        let at = open.offset.load(Ordering::Relaxed) as usize;
        let Some(entry) = entries.get(at) else {
            return Ok(None);
        };
        open.offset.store(at as u64 + 1, Ordering::Relaxed);
        Ok(Some(entry.clone()))
    }

    /// Узел за дескриптором — чтобы отобразить его в память.
    ///
    /// Отдаётся клон `Arc`, и в этом весь смысл: с этого мгновения у файла два
    /// владельца, дескриптор и отображение, и закрытие первого второго не
    /// трогает. Смещение дескриптора при этом **не** участвует — у отображения
    /// своё, пришедшее аргументом; читать один файл через `read` и через
    /// отображение одновременно позволено, и позиции у них разные.
    ///
    /// Отображать можно только обычный файл. Каталог и канал отвергаются здесь,
    /// а не в вызывающем: что такое «страница каталога», не знает никто, а у
    /// канала нет ни длины, ни смещения — читать его можно только вперёд и
    /// только один раз.
    pub fn node(&self, fd: usize) -> Result<Arc<dyn Node>, FileError> {
        match self.slots.get(index_of(fd)?).and_then(Option::as_ref) {
            Some(Slot::File(open)) => {
                if open.entries.is_some() {
                    return Err(FileError::Vfs(VfsError::WrongKind));
                }
                Ok(Arc::clone(&open.node))
            }
            Some(Slot::PipeRead(_) | Slot::PipeWrite(_)) => {
                Err(FileError::Vfs(VfsError::WrongKind))
            }
            None => Err(FileError::BadFd),
        }
    }

    /// Прочитать из дескриптора в буфер. Возвращает, сколько прочитано; ноль —
    /// это конец файла, а не ошибка.
    pub fn read(&mut self, fd: usize, buf: &mut [u8]) -> Result<usize, FileError> {
        let index = index_of(fd)?;
        match self.slots[index].as_mut().ok_or(FileError::BadFd)? {
            Slot::File(open) => {
                // У каталога байтов нет: читать его как файл — это вопрос не к
                // правам, а к тому, что такое каталог. Программа, спутавшая одно
                // с другим, узнает об этом здесь, а не получит содержимое чужого
                // формата.
                if open.entries.is_some() {
                    return Err(FileError::Vfs(VfsError::WrongKind));
                }
                let at = open.offset.load(Ordering::Relaxed);
                let read = open.node.read_at(at, buf).map_err(FileError::Vfs)?;
                // Смещение двигается на прочитанное, а не на запрошенное: у
                // конца файла это разные числа, и второе увело бы следующее
                // чтение за конец.
                open.offset.store(at + read as u64, Ordering::Relaxed);
                Ok(read)
            }
            // Не ждёт по той же причине, что и запись выше.
            Slot::PipeRead(reader) => reader.read(buf, false).map_err(FileError::Pipe),
            Slot::PipeWrite(_) => Err(FileError::Vfs(VfsError::PermissionDenied)),
        }
    }

    /// Передвинуть позицию и вернуть новую.
    ///
    /// Позиция за концом файла разрешена: так делают все, кто пишет разрежённые
    /// файлы, и запрещать это на уровне дескриптора нельзя — вопрос, что
    /// произойдёт при записи туда, решает файловая система, а не таблица.
    /// Отрицательная позиция запрещена: она не значит ничего, и её молчаливое
    /// обрезание до нуля превратило бы ошибку в счёте программы в тихо неверные
    /// данные.
    pub fn seek(&mut self, fd: usize, offset: i64, whence: Whence) -> Result<u64, FileError> {
        let index = index_of(fd)?;
        let open = Self::as_file(self.slots[index].as_mut())?;
        let base = match whence {
            Whence::Set => 0,
            Whence::Current => open.offset.load(Ordering::Relaxed),
            // Размер спрашивается у узла, а не запоминается при открытии: файл
            // мог вырасти с тех пор — в том числе от записи через этот же
            // дескриптор.
            Whence::End => open.node.metadata().size,
        };

        let Some(position) = base.checked_add_signed(offset) else {
            return Err(FileError::BadOffset);
        };
        open.offset.store(position, Ordering::Relaxed);
        Ok(position)
    }

    /// Закрыть дескриптор.
    pub fn close(&mut self, fd: usize) -> Result<(), FileError> {
        let index = index_of(fd)?;
        match self.slots[index].take() {
            Some(_) => Ok(()),
            None => Err(FileError::BadFd),
        }
    }

    /// Занять место под читающий конец канала.
    ///
    /// Отдельно от [`Table::open`] потому, что открывать нечего: канал уже
    /// существует, таблице нужно только место под его конец.
    pub fn install_read(&mut self, reader: pipe::Reader) -> Result<usize, FileError> {
        let slot = self.free_slot()?;
        self.slots[slot] = Some(Slot::PipeRead(reader));
        Ok(slot + FD_FIRST)
    }

    /// То же для пишущего конца.
    pub fn install_write(&mut self, writer: pipe::Writer) -> Result<usize, FileError> {
        let slot = self.free_slot()?;
        self.slots[slot] = Some(Slot::PipeWrite(writer));
        Ok(slot + FD_FIRST)
    }

    /// Копия читающего конца из этого дескриптора.
    ///
    /// Копия, а не изъятие: тот, кто отдаёт конец запускаемой задаче, свой
    /// дескриптор сохраняет — и обязан закрыть его сам. Правило неудобное, но
    /// честное: закрыть чужой дескриптор внутри `launch` значило бы сделать то,
    /// о чём программа не просила. Цена ошибки при этом названа в договоре:
    /// незакрытый конец — это конец файла, который не наступит никогда.
    pub fn read_end(&self, fd: usize) -> Result<pipe::Reader, FileError> {
        let index = index_of(fd)?;
        match self.slots[index].as_ref().ok_or(FileError::BadFd)? {
            Slot::PipeRead(reader) => Ok(reader.clone()),
            _ => Err(FileError::Vfs(VfsError::WrongKind)),
        }
    }

    /// Копия пишущего конца из этого дескриптора.
    pub fn write_end(&self, fd: usize) -> Result<pipe::Writer, FileError> {
        let index = index_of(fd)?;
        match self.slots[index].as_ref().ok_or(FileError::BadFd)? {
            Slot::PipeWrite(writer) => Ok(writer.clone()),
            _ => Err(FileError::Vfs(VfsError::WrongKind)),
        }
    }

    /// Свободное место или отказ.
    fn free_slot(&self) -> Result<usize, FileError> {
        self.slots.iter().position(Option::is_none).ok_or(FileError::TooManyFiles)
    }

    /// Место таблицы как файл.
    fn as_file(slot: Option<&mut Slot>) -> Result<&Open, FileError> {
        match slot.ok_or(FileError::BadFd)? {
            Slot::File(open) => Ok(open),
            _ => Err(FileError::NotSeekable),
        }
    }

    /// Продублировать дескриптор. `to` — [`user_abi::DUP_ANY`] либо номер,
    /// который надо занять.
    ///
    /// Копируется **ссылка**, а не открытый файл: позиция, права и снимок
    /// каталога у копий общие. Для концов канала «копия» означает ещё и то,
    /// что живых концов стало на один больше, — их считает сам канал, и без
    /// этого закрытие первой копии объявило бы конец файла тому, кто читает с
    /// другой стороны.
    ///
    /// Занятое место закрывается до того, как в него положат копию, и это
    /// единственный способ выполнить просьбу: два места с одним номером не
    /// бывают. Если копируют дескриптор сам в себя, не делается ничего — иначе
    /// закрытие места уничтожило бы то, что мы собираемся в него положить.
    pub fn dup(&mut self, fd: usize, to: usize) -> Result<usize, FileError> {
        let from = index_of(fd)?;
        let copy = match self.slots[from].as_ref().ok_or(FileError::BadFd)? {
            Slot::File(open) => Slot::File(Arc::clone(open)),
            Slot::PipeRead(reader) => Slot::PipeRead(reader.clone()),
            Slot::PipeWrite(writer) => Slot::PipeWrite(writer.clone()),
        };

        let target = if to == user_abi::DUP_ANY {
            self.free_slot()?
        } else {
            // Стандартные потоки местами таблицы не являются, и подменить их
            // записью в ней нечем — см. договор `SYS_DUP`.
            let target = index_of(to).map_err(|_| FileError::Vfs(VfsError::Unsupported))?;
            if target == from {
                return Ok(fd);
            }
            self.slots[target] = None;
            target
        };

        self.slots[target] = Some(copy);
        Ok(target + FD_FIRST)
    }

    /// Что лежит за дескриптором — для `SYS_FSTAT` и `SYS_ISATTY`.
    ///
    /// Отдаёт готовые числа, а не узел: у канала узла нет вовсе, и вызывающему
    /// пришлось бы разбирать два случая там, где вопрос у него один.
    pub fn describe(&self, fd: usize) -> Result<FdInfo, FileError> {
        match self.slots.get(index_of(fd)?).and_then(Option::as_ref) {
            Some(Slot::File(open)) => {
                let meta = open.node.metadata();
                Ok(FdInfo {
                    size: meta.size,
                    mode: meta.mode,
                    uid: meta.uid,
                    gid: meta.gid,
                    kind: match meta.kind {
                        NodeKind::Directory => FdKind::Directory,
                        NodeKind::File => FdKind::File,
                    },
                })
            }
            // У канала нет ни длины, ни владельца, ни прав: он не лежит на
            // носителе. Нули здесь — не заглушка, а единственный честный ответ,
            // и отличает канал от пустого файла поле `kind`.
            Some(Slot::PipeRead(_) | Slot::PipeWrite(_)) => Ok(FdInfo {
                size: 0,
                mode: 0,
                uid: 0,
                gid: 0,
                kind: FdKind::Pipe,
            }),
            None => Err(FileError::BadFd),
        }
    }

    /// Готовность дескриптора: `(есть что читать, есть куда писать, другой
    /// конец закрыт)` — для `SYS_POLL`.
    ///
    /// Обычный файл готов всегда, и это не упрощение: у него нечего ждать —
    /// чтение с конца немедленно возвращает ноль, а запись идёт на носитель.
    /// Так отвечает `poll` на файл везде, где он есть.
    pub fn readiness(&self, fd: usize) -> Result<(bool, bool, bool), FileError> {
        match self.slots.get(index_of(fd)?).and_then(Option::as_ref) {
            Some(Slot::File(open)) => Ok((true, open.writable, false)),
            Some(Slot::PipeRead(reader)) => Ok((reader.ready(), false, reader.hangup())),
            Some(Slot::PipeWrite(writer)) => Ok((false, writer.ready(), writer.hangup())),
            None => Err(FileError::BadFd),
        }
    }

    /// Сколько дескрипторов осталось открытыми.
    ///
    /// Спрашивается при завершении программы — и на пути отказа тоже.
    /// Программа, забывшая закрыть файлы, ничем не отличается от снятой посреди
    /// чтения, и в журнале это видно.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

/// Номер места в таблице по дескриптору.
fn index_of(fd: usize) -> Result<usize, FileError> {
    fd.checked_sub(FD_FIRST)
        .filter(|index| *index < MAX_OPEN_FILES)
        .ok_or(FileError::BadFd)
}
