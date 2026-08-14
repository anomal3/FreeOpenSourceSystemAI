//! Корневая ФС ext2 за интерфейсом [`crate::vfs`].
//!
//! Разбор формата здесь не повторяется: он весь в крейте `ext2`, том же самом,
//! которым установщик этот том создавал. Здесь только переходник — из типов
//! крейта в типы VFS и обратно.
//!
//! # Почему замок
//!
//! Трейты VFS отдают узлы по `&self`: файл можно читать, ничего не изменяя.
//! Диск устроен иначе — у него одна очередь запросов, и обращение к ней меняет
//! её состояние. Замок и есть то место, где эти два взгляда сходятся, и он же
//! ровно то, что понадобилось бы настоящему драйверу в любом случае: два
//! одновременных запроса в одну очередь не отправить.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::block::Counted;
use disk::BlockDevice as _;
use crate::sync::Mutex;
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Диск вместе с разобранной на нём файловой системой.
///
/// Читатель и редактор держатся оба и намеренно: они смотрят на один том с
/// разных сторон и ничего друг у друга не кэшируют. Читатель помнит только
/// расположение таблиц inode, которое не меняется никогда; редактор — счётчики
/// свободного, которые меняет он сам. Общей изменяемой памяти у них нет, а
/// значит нет и вопроса, кто чью копию не обновил.
struct Inner {
    /// Носитель — любой, лишь бы умел сектора.
    ///
    /// До Phase 26a здесь стоял `VirtioBlk`, и это было не упрощение, а
    /// ограничение: том, лежащий на диске SATA, смонтировать было нечем, потому
    /// что тип не совпадал. Крейт `ext2` всегда работал через
    /// `&mut dyn BlockDevice` — расходилось с ним только ядро.
    disk: Counted,
    fs: ext2::Ext2,
    /// Редактор — только у тома, смонтированного на запись.
    ///
    /// `None` — это не «редактор пока не создан», а «его не будет»: том открыт
    /// только на чтение, и записать в него нельзя даже по ошибке, потому что
    /// писать нечем. Флаг `read_only` рядом с редактором позволял бы забыть
    /// проверку в одном из путей записи; отсутствующий редактор — не позволяет.
    editor: Option<ext2::Editor>,
}

/// Смонтированный том.
pub struct Ext2Fs {
    inner: Mutex<Inner>,
}

/// То, что отдаётся в [`crate::fs::set_root`].
///
/// Отдельная обёртка вокруг `Arc`, потому что узлы обязаны держать ссылку на
/// том: [`Node::lookup`] возвращает новый узел, а тому нужен доступ к диску.
pub struct Ext2Mount(Arc<Ext2Fs>);

/// Перевести ошибку крейта в ошибку VFS.
///
/// Отображение не механическое: `Corrupt` и `Unsupported` означают разное для
/// того, кто увидит сообщение. Первое — испорченный носитель, второе — том,
/// созданный не нами.
fn convert(err: ext2::Error) -> VfsError {
    match err {
        ext2::Error::Io => VfsError::Io,
        ext2::Error::Corrupt => VfsError::Corrupt,
        ext2::Error::NotFound => VfsError::NotFound,
        ext2::Error::NotADirectory => VfsError::WrongKind,
        ext2::Error::BadName => VfsError::BadPath,
        ext2::Error::NoMemory => VfsError::OutOfMemory,
        ext2::Error::Unsupported => VfsError::Unsupported,
        ext2::Error::Exists => VfsError::Exists,
        ext2::Error::NotEmpty => VfsError::NotEmpty,
        ext2::Error::IsADirectory => VfsError::WrongKind,
        ext2::Error::NoSpace | ext2::Error::NoInodes => VfsError::NoSpace,
        _ => VfsError::Corrupt,
    }
}

/// Разбить путь на каталог и последнее имя.
///
/// Свой, а не общий с [`crate::fs`]: там он частный и разбирает путь для узла, а
/// здесь нужен ровно тот же ответ, но внутри реализации файловой системы.
/// Вынести один на двоих значило бы сделать частное решение общим интерфейсом.
fn split_parent(path: &str) -> VfsResult<(&str, &str)> {
    let trimmed = path.trim_end_matches('/');
    let (parent, name) = match trimmed.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", trimmed),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(VfsError::BadPath);
    }
    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn kind_of(kind: ext2::FileType) -> NodeKind {
    match kind {
        ext2::FileType::Directory => NodeKind::Directory,
        // Всё, что не каталог, показывается файлом. Символических ссылок и
        // устройств ext2 у нас не создаёт, а притворяться, что мы умеем их
        // различать, значило бы обещать чтение, которого нет.
        _ => NodeKind::File,
    }
}

fn metadata_of(inode: &ext2::Inode) -> Metadata {
    Metadata {
        kind: kind_of(inode.kind),
        size: inode.size,
        mode: inode.mode,
        uid: inode.uid,
        gid: inode.gid,
        mtime: inode.mtime,
    }
}

impl Ext2Fs {
    /// Смонтировать том, начинающийся с сектора `first_lba`.
    pub fn mount(
        device: alloc::boxed::Box<dyn disk::BlockDevice + Send>,
        first_lba: u64,
        writable: bool,
    ) -> VfsResult<Ext2Mount> {
        let mut disk = Counted::new(device);
        let fs = ext2::Ext2::mount(&mut disk, first_lba).map_err(convert)?;
        // На томе только для чтения редактор не открывается вовсе — и не
        // только потому, что писать не собираемся: само открытие помечает том
        // используемым, то есть **пишет** в суперблок. Безопасный режим,
        // трогающий том, которого он обещал не трогать, был бы издевательством
        // над словом «безопасный».
        let editor = if writable {
            Some(ext2::Editor::open(&mut disk, first_lba).map_err(convert)?)
        } else {
            None
        };
        Ok(Ext2Mount(Arc::new(Self {
            inner: Mutex::new(Inner { disk, fs, editor }),
        })))
    }

    /// Выполнить изменение тома и записать счётчики.
    ///
    /// Сброс после **каждой** операции, а не по закрытию файла: счётчики
    /// свободного живут в памяти редактора, и уйди машина в перезагрузку до
    /// сброса — новый редактор прочитал бы с диска устаревшие числа и выдал бы
    /// под новый файл блок, уже занятый старым. Не потеря счётчиков, а потеря
    /// данных. Цена — с десяток записей блоков на операцию, и это ровно то
    /// место, где такую цену стоит платить не глядя.
    fn change<R>(
        &self,
        action: impl FnOnce(&mut Counted, &mut ext2::Editor) -> VfsResult<R>,
    ) -> VfsResult<R> {
        let mut guard = self.inner.lock();
        // Штамп времени обновляется перед **каждой** правкой, а не один раз при
        // монтировании. [`ext2::Editor::open`] берёт его из суперблока — из
        // времени последней записи тома, то есть из момента установки, — и до
        // этой фазы все созданные системой файлы получали именно его: одну и ту
        // же дату, не двигавшуюся месяцами. Часы у системы теперь есть, и
        // спросить их дешевле, чем объяснять потом, почему файл создан раньше,
        // чем машина включилась.
        // Тому только для чтения редактора нет вовсе, и запись отказывает
        // здесь — до того, как что-нибудь на диске изменится.
        let Inner { disk, editor, .. } = &mut *guard;
        let Some(editor) = editor.as_mut() else {
            return Err(VfsError::ReadOnly);
        };
        editor.set_time(crate::time::now_unix_u32());
        // Признак «том используется» ставится **до** правки. Обычно он уже
        // стоит с самого монтирования и это ничего не стоит; смысл в другом
        // случае — если том успели пометить чистым (`sync` перед выключением),
        // а потом система продолжила работать, признак надо вернуть, и вернуть
        // до того, как на диске что-то изменится.
        editor.mark_dirty(disk).map_err(convert)?;
        let result = action(disk, editor)?;
        editor.flush(disk).map_err(convert)?;
        Ok(result)
    }

    fn root_inode(&self) -> VfsResult<ext2::Inode> {
        let mut guard = self.inner.lock();
        let Inner { disk, fs, .. } = &mut *guard;
        fs.root(disk).map_err(convert)
    }

    /// Номер inode каталога по абсолютному пути.
    ///
    /// Нужен переименованию, и только ему: остальные операции работают с узлом,
    /// который у них уже есть, а этой нужны **два** каталога сразу — и нужны
    /// именно номерами, потому что крейт `ext2` адресует каталоги ими.
    fn directory_number(&self, path: &str) -> VfsResult<u32> {
        let mut guard = self.inner.lock();
        let Inner { disk, fs, .. } = &mut *guard;
        let inode = fs.resolve(disk, path).map_err(convert)?;
        if inode.kind != ext2::FileType::Directory {
            return Err(VfsError::WrongKind);
        }
        Ok(inode.number)
    }
}

/// Один и тот же том, смонтированный в нескольких точках.
///
/// Клонируется **ссылка**, а не файловая система: раздел состояния обслуживает
/// сразу `/etc`, `/home`, `/var` и `/opt`, и четыре независимых объекта поверх
/// одного диска означали бы четыре редактора со своими счётчиками свободного —
/// то есть четыре разных мнения о том, какие блоки заняты.
impl Clone for Ext2Mount {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl Ext2Mount {
    /// Геометрия тома и число обращений к диску — строка диагностики при
    /// монтировании. Счётчик запросов здесь не для красоты: он единственное
    /// доказательство, что чтение действительно дошло до устройства, а не было
    /// обслужено чем-то по дороге.
    #[must_use]
    pub fn stats(&self) -> (u32, u32, u32, u64) {
        let guard = self.0.inner.lock();
        let geometry = guard.fs.geometry();
        (
            geometry.blocks,
            geometry.block_size.bytes(),
            geometry.groups,
            guard.disk.requests(),
        )
    }

    /// Закрыли ли том чисто в прошлый раз.
    ///
    /// Спрашивается сразу после монтирования и только там: к этому моменту
    /// редактор уже пометил том используемым, и на диске лежит ответ про
    /// текущий сеанс, а не про прошлый.
    #[must_use]
    pub fn was_clean(&self) -> bool {
        self.0.inner.lock().fs.was_clean()
    }

    /// То же, но словами общего интерфейса — для оболочки.
    fn summary(&self) -> VfsResult<crate::vfs::CheckSummary> {
        let report = self.scan()?;
        let mut problems = Vec::new();
        problems
            .try_reserve_exact(report.problems.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for problem in &report.problems {
            let mut text = alloc::string::String::new();
            // Строка собирается здесь, а не в крейте `ext2`: там нет ни
            // аллокатора по умолчанию, ни причины знать, кому это показывают.
            core::fmt::Write::write_fmt(&mut text, format_args!("{problem}"))
                .map_err(|_| VfsError::OutOfMemory)?;
            problems.push(text);
        }
        Ok(crate::vfs::CheckSummary {
            problems,
            dropped: report.dropped,
            needs_attention: report.needs_attention(),
            inodes_used: report.inodes_used,
            blocks_used: report.blocks_used,
        })
    }

    /// Проверить смонтированный том, ничего не меняя.
    ///
    /// Только чтение, и это не осторожность, а необходимость: счётчики
    /// свободного живут в памяти редактора, и починка под ним оставила бы его с
    /// устаревшими числами — он выдал бы под новый файл только что
    /// освобождённый блок. Чинит система при монтировании, до того как редактор
    /// появился на свет.
    ///
    /// Замок держится всю проверку: без него другая задача успела бы записать
    /// файл посреди обхода, и «находка» описывала бы не том, а гонку.
    pub fn scan(&self) -> VfsResult<ext2::Report> {
        let mut guard = self.0.inner.lock();
        let first_lba = guard.fs.geometry().first_lba;
        let Inner { disk, .. } = &mut *guard;
        ext2::check(disk, first_lba, ext2::Fix::Nothing).map_err(convert)
    }
}

impl FileSystem for Ext2Mount {
    fn name(&self) -> &'static str {
        "ext2"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        let inode = self.0.root_inode()?;
        Ok(Box::new(Ext2Node { fs: Arc::clone(&self.0), inode }))
    }

    /// Сбросить счётчики, довести записи до носителя и закрыть том чисто.
    ///
    /// `flush` устройства здесь не менее важен, чем сброс счётчиков: у диска
    /// SATA есть кеш записи, и «записано» без него означает «лежит в
    /// микросхеме диска», что переживает не всякое выключение питания.
    ///
    /// Порядок трёх действий — не стиль. Признак «закрыт чисто» ставится
    /// последним, потому что он утверждает, что предыдущие два состоялись;
    /// поставленный раньше, он обещал бы следующей загрузке то, чего на диске
    /// ещё нет.
    fn check(&self) -> Option<VfsResult<crate::vfs::CheckSummary>> {
        Some(self.summary())
    }

    /// Переименовать: то же содержимое под другим именем.
    ///
    /// Номера каталогов выясняются **до** захвата редактора: разбор пути сам
    /// берёт замок тома, и держать его вложенно нельзя.
    fn rename(&self, old: &str, new: &str) -> VfsResult<()> {
        let (old_parent, old_name) = split_parent(old)?;
        let (new_parent, new_name) = split_parent(new)?;
        let old_dir = self.0.directory_number(old_parent)?;
        let new_dir = self.0.directory_number(new_parent)?;

        self.0.change(|disk, editor| {
            editor
                .rename(disk, old_dir, old_name, new_dir, new_name)
                .map_err(convert)
        })
    }

    fn sync(&self) -> VfsResult<()> {
        let mut guard = self.0.inner.lock();
        let Inner { disk, editor, .. } = &mut *guard;
        // Тому, открытому на чтение, сбрасывать нечего: он не менялся и не
        // помечался используемым, а значит и закрывать его не надо.
        let Some(editor) = editor.as_mut() else {
            return Ok(());
        };
        editor.flush(disk).map_err(convert)?;
        disk.flush().map_err(|_| VfsError::Io)?;
        editor.mark_clean(disk).map_err(convert)
    }
}

/// Узел тома: файл или каталог.
struct Ext2Node {
    fs: Arc<Ext2Fs>,
    inode: ext2::Inode,
}

impl Node for Ext2Node {
    fn metadata(&self) -> Metadata {
        metadata_of(&self.inode)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.inode.kind != ext2::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        let mut guard = self.fs.inner.lock();
        let Inner { disk, fs, .. } = &mut *guard;
        fs.read_at(disk, &self.inode, offset, buf).map_err(convert)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let mut guard = self.fs.inner.lock();
        let Inner { disk, fs, .. } = &mut *guard;
        let entries = fs.list(disk, &self.inode).map_err(convert)?;

        let mut out = Vec::new();
        out.try_reserve_exact(entries.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for entry in entries {
            // Ни размера, ни прав запись каталога не хранит — всё это в inode,
            // и его приходится читать на каждое имя. Лишнее обращение к диску
            // на запись заметно на большом каталоге, но перечисление вызывает
            // человек командой `ls`, а не горячий путь.
            let node = fs.inode(disk, entry.inode).map_err(convert)?;
            out.push(DirEntry {
                name: entry.name,
                kind: kind_of(entry.kind),
                size: node.size,
                mode: node.mode,
                uid: node.uid,
                gid: node.gid,
                mtime: node.mtime,
            });
        }
        Ok(out)
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        let inode = {
            let mut guard = self.fs.inner.lock();
            let Inner { disk, fs, .. } = &mut *guard;
            let entry = fs
                .lookup(disk, &self.inode, name)
                .map_err(convert)?
                .ok_or(VfsError::NotFound)?;
            fs.inode(disk, entry.inode).map_err(convert)?
        };
        Ok(Box::new(Ext2Node { fs: Arc::clone(&self.fs), inode }))
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        if self.inode.kind != ext2::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        self.fs.change(|disk, editor| {
            editor
                .write_at(disk, self.inode.number, offset, data)
                .map_err(convert)
        })
    }

    fn truncate(&self, size: u64) -> VfsResult<()> {
        if self.inode.kind != ext2::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        self.fs.change(|disk, editor| {
            editor
                .truncate(disk, self.inode.number, size)
                .map_err(convert)
        })
    }

    fn create(&self, name: &str, mode: u16, uid: u32, gid: u32) -> VfsResult<Box<dyn Node>> {
        let number = self.fs.change(|disk, editor| {
            editor
                .create(disk, self.inode.number, name, mode, uid, gid)
                .map_err(convert)
        })?;
        self.child(number)
    }

    fn mkdir(&self, name: &str, mode: u16, uid: u32, gid: u32) -> VfsResult<Box<dyn Node>> {
        let number = self.fs.change(|disk, editor| {
            editor
                .mkdir(disk, self.inode.number, name, mode, uid, gid)
                .map_err(convert)
        })?;
        self.child(number)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        self.fs.change(|disk, editor| {
            editor
                .unlink(disk, self.inode.number, name)
                .map_err(convert)
        })
    }

    fn rmdir(&self, name: &str) -> VfsResult<()> {
        self.fs.change(|disk, editor| {
            editor
                .rmdir(disk, self.inode.number, name)
                .map_err(convert)
        })
    }
}

impl Ext2Node {
    /// Узел по номеру inode — то, что возвращают создающие операции.
    ///
    /// Inode перечитывается с диска, а не собирается из того, что мы только что
    /// записали: так возвращённый узел описывает том, а не наши намерения
    /// насчёт него.
    fn child(&self, number: u32) -> VfsResult<Box<dyn Node>> {
        let inode = {
            let mut guard = self.fs.inner.lock();
            let Inner { disk, fs, .. } = &mut *guard;
            fs.inode(disk, number).map_err(convert)?
        };
        Ok(Box::new(Ext2Node { fs: Arc::clone(&self.fs), inode }))
    }
}

