//! Реализации конкретных файловых систем.
//!
// Заголовок тома разбирается целиком, хотя используется пока не всё: неполный
// разбор означал бы, что противоречивый образ проходит проверку. То же
// обоснование, что в `vfs`.
#![allow(dead_code)]
//!
//! Модуль отделён от [`crate::vfs`] намеренно: там живёт контракт (трейты
//! [`BlockDevice`](crate::vfs::BlockDevice), [`Node`](crate::vfs::Node),
//! [`FileSystem`](crate::vfs::FileSystem)) и то, что нужно всем реализациям —
//! разбор путей и RAM-диск; здесь — сами реализации. Пока их одна, FAT32, но по
//! дорожной карте рядом встанет собственная inode-based ФС, и разводить их
//! задним числом дороже, чем сразу.
//!
//! Всё, что читается с носителя, — внешние данные. Разбор устроен так, чтобы
//! испорченный образ приводил к [`VfsError::Corrupt`](crate::vfs::VfsError), а
//! не к панике или зацикливанию ядра.
//!
//! # Два входа: с проверкой прав и без
//!
//! [`resolve_as`] спрашивает права у каждого каталога на пути и у самого узла;
//! [`read`] и [`list`] не спрашивают ничего. Это не две степени аккуратности, а
//! два разных вызывающих. Первый — системный вызов пользовательской программы,
//! то есть недоверенная сторона. Второй — код ядра: оболочка, файловый
//! менеджер, чтение `/etc/passwd` при загрузке. Проверять код ядра значило бы
//! проверять того, кто в любом случае может прочитать диск сектор за сектором;
//! граница проходит по кольцу привилегий, а не по слою кода. Подробнее — в
//! [`crate::vfs::perm`].

pub mod ext2fs;
pub mod fat;

pub use ext2fs::Ext2Fs;
pub use fat::Fat32;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sync::Mutex;
use crate::vfs::perm::{Access, Credentials, permits};
use crate::vfs::{DirEntry, FileSystem, Node, NodeKind, VfsError, VfsResult};

/// Одна точка монтирования.
///
/// # Почему таблица появилась только сейчас
///
/// Потому что до фазы 32 второй файловой системы не было. Заготовка под таблицу
/// монтирования, сделанная заранее, была бы обещанием — «пространство имён
/// устроено так-то», — которое никто не проверял; теперь у неё есть ровно один
/// потребитель и ровно одна форма, продиктованная им.
///
/// # Как устроено сопоставление
///
/// Префикс — начало пути (`/etc`), и путь уезжает в найденную ФС **целиком**, а
/// не остатком. Это не экономия на подстроке: раздел состояния несёт у себя
/// настоящие `/etc`, `/home`, `/var` и `/opt`, то есть его собственное дерево
/// совпадает с тем, что он обслуживает. Резать путь пришлось бы там, где
/// смонтированный том лежит по другому имени, — а такого случая в системе нет,
/// и заводить под него механизм значило бы заводить второй способ ошибиться.
struct Mount {
    /// Начало пути без завершающей косой: `""` у корня, `"/etc"` у ветки.
    prefix: &'static str,
    fs: Arc<dyn FileSystem>,
}

/// Смонтированные файловые системы, в порядке от длинного префикса к короткому.
///
/// Порядок держится при вставке, а не при поиске: путь `/etc/passwd` подходит и
/// под `/etc`, и под корень, и выбрать надо первое. Сортировка при каждом
/// обращении к файлу стоила бы дороже, чем один раз при монтировании.
static MOUNTS: Mutex<Vec<Mount>> = Mutex::new(Vec::new());

/// Запомнить смонтированную ФС как корневую.
pub fn set_root(fs: Box<dyn FileSystem>) {
    mount_at("", Arc::from(fs));
}

/// Смонтировать ФС по префиксу пути.
///
/// Принимает **`Arc`, а не `Box`**, и это не мелочь. Раздел состояния
/// обслуживает пять веток, и передай мы сюда пять коробок с клонами одного и
/// того же тома, получилось бы пять разных `Arc` — то есть пять «разных» томов
/// для всякого, кто сравнивает их по указателю. `fsck` проверял бы один том
/// пять раз и печатал одни и те же находки пятикратно, а выключение записывало
/// бы «закрыт чисто» пять раз подряд. Ровно это и происходило, пока подпись
/// была другой.
///
/// Повторное монтирование того же префикса заменяет прежнюю: так корень с диска
/// сменяет образ initrd, не оставляя за собой второй записи.
pub fn mount_at(prefix: &'static str, fs: Arc<dyn FileSystem>) {
    let mut mounts = MOUNTS.lock();
    mounts.retain(|mount| mount.prefix != prefix);
    // Место находится по длине префикса: длинные впереди. `Vec::insert`, а не
    // сортировка, — таблица из пяти записей, и заводить ради неё порядок,
    // который надо поддерживать отдельно, незачем.
    let at = mounts
        .iter()
        .position(|mount| mount.prefix.len() < prefix.len())
        .unwrap_or(mounts.len());
    // Отказ выделения не должен ронять ядро: том просто не смонтируется, и об
    // этом скажет вызывающий.
    if mounts.try_reserve(1).is_err() {
        return;
    }
    mounts.insert(at, Mount { prefix, fs });
}

/// Какая ФС отвечает за этот путь.
///
/// Возвращается `Arc`, а не ссылка под локом, и это важно: файловая система
/// читает диск, а чтение диска ждёт прерывания — держать в это время лок
/// таблицы монтирования значило бы остановить всех остальных на время
/// обращения к носителю.
fn for_path(path: &str) -> Option<Arc<dyn FileSystem>> {
    let mounts = MOUNTS.lock();
    mounts
        .iter()
        .find(|mount| covers(mount.prefix, path))
        .map(|mount| Arc::clone(&mount.fs))
}

/// Обслуживает ли префикс этот путь.
///
/// `/etc` обслуживает `/etc` и `/etc/passwd`, но не `/etcetera`: совпадение
/// по началу строки без проверки границы компонента — классическая ошибка, и
/// стоит она чужого файла, прочитанного вместо своего.
fn covers(prefix: &str, path: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let Some(rest) = path.strip_prefix(prefix) else {
        return false;
    };
    rest.is_empty() || rest.starts_with('/')
}

/// Сделать что-нибудь с корневой ФС. `None`, если ничего не смонтировано.
///
/// Замыкание исполняется **без** лока таблицы монтирования, но сама ФС внутри
/// себя запирается как ей нужно.
pub fn with_root<R>(f: impl FnOnce(&dyn FileSystem) -> R) -> Option<R> {
    let fs = for_path("/")?;
    Some(f(&*fs))
}

/// Сделать что-нибудь с той ФС, которая отвечает за путь.
pub fn with_fs<R>(path: &str, f: impl FnOnce(&dyn FileSystem) -> R) -> Option<R> {
    let fs = for_path(path)?;
    Some(f(&*fs))
}

/// Перечислить смонтированное: префикс и имя ФС.
///
/// Нужно оболочке и загрузке: «система работает» и «состояние на своём разделе»
/// — разные утверждения, и второе обязано быть видно строкой, а не
/// подразумеваться по тому, что файлы читаются.
pub fn mounted() -> Vec<(&'static str, &'static str)> {
    let mounts = MOUNTS.lock();
    let mut out = Vec::new();
    if out.try_reserve_exact(mounts.len()).is_err() {
        return Vec::new();
    }
    // Обратный порядок: корень первым, ветки за ним — так это читает человек.
    for mount in mounts.iter().rev() {
        out.push((mount.prefix, mount.fs.name()));
    }
    out
}

/// Найти узел по пути, проверяя права на каждом шаге.
///
/// Проверяется не только сам файл. Чтобы дойти до `/root/notes.txt`, нужно
/// пройти сквозь `/` и `/root`, а для этого нужен бит поиска
/// ([`Access::SEARCH`]) на каждом из них. Каталог `0700`, принадлежащий чужому,
/// закрывает всё, что внутри, — сколько бы прав ни стояло на самих файлах. Без
/// прохода по каталогам проверка выглядела бы работающей и не работала бы:
/// установщик выставляет права именно так, как это принято в Unix, то есть
/// рассчитывая на такую семантику.
///
/// Внешний `None` означает «ничего не смонтировано»: это не отказ в правах и не
/// отсутствие файла, и путать их нельзя.
pub fn resolve_as(
    cred: Credentials,
    path: &str,
    want: Access,
) -> Option<VfsResult<Box<dyn Node>>> {
    with_fs(path, |fs| {
        let mut node = fs.root()?;
        for component in crate::vfs::path::components(path)? {
            // Право пройти спрашивается у каталога, в котором мы стоим, — до
            // того, как станет известно, есть ли там такое имя. Иначе ответ
            // «нет такого файла» рассказывал бы о содержимом каталога, в
            // который спрашивающему не дали заглянуть.
            if !permits(cred, &node.metadata(), Access::SEARCH) {
                return Err(VfsError::PermissionDenied);
            }
            node = node.lookup(component)?;
        }
        if !permits(cred, &node.metadata(), want) {
            return Err(VfsError::PermissionDenied);
        }
        Ok(node)
    })
}

/// Разделить путь на «каталог» и «последнее имя».
///
/// Создание и удаление работают не с путём целиком, а с именем внутри
/// каталога: право спрашивается у **каталога**, а не у того, чего в нём пока
/// нет либо уже не будет.
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

/// Создать файл по пути от имени `cred`.
///
/// Право писать спрашивается у каталога — там, где появляется новая запись.
/// Прав на сам файл не существует: его ещё нет.
pub fn create_as(cred: Credentials, path: &str, mode: u16) -> Option<VfsResult<Box<dyn Node>>> {
    let (parent, name) = match split_parent(path) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let dir = match resolve_as(cred, parent, Access::WRITE)? {
        Ok(dir) => dir,
        Err(err) => return Some(Err(err)),
    };
    Some(dir.create(name, mode, cred.uid, cred.gid))
}

/// Создать каталог по пути от имени `cred`.
pub fn mkdir_as(cred: Credentials, path: &str, mode: u16) -> Option<VfsResult<()>> {
    let (parent, name) = match split_parent(path) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let dir = match resolve_as(cred, parent, Access::WRITE)? {
        Ok(dir) => dir,
        Err(err) => return Some(Err(err)),
    };
    Some(dir.mkdir(name, mode, cred.uid, cred.gid).map(|_| ()))
}

/// Удалить файл или пустой каталог по пути от имени `cred`.
///
/// Что именно удалять, решает не вызывающий, а тип того, что нашлось: два
/// вызова на одно действие означали бы, что `rm` каталога отказывает не потому,
/// что каталог не пуст, а потому, что человек выбрал не ту команду.
pub fn remove_as(cred: Credentials, path: &str) -> Option<VfsResult<()>> {
    let (parent, name) = match split_parent(path) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let dir = match resolve_as(cred, parent, Access::WRITE)? {
        Ok(dir) => dir,
        Err(err) => return Some(Err(err)),
    };
    let kind = match dir.lookup(name) {
        Ok(node) => node.metadata().kind,
        Err(err) => return Some(Err(err)),
    };
    Some(match kind {
        NodeKind::Directory => dir.rmdir(name),
        NodeKind::File => dir.unlink(name),
    })
}

/// Переименовать файл или каталог от имени `cred`.
///
/// Право писать спрашивается у **обоих** каталогов: имя исчезает в одном и
/// появляется в другом, и каждое из этих действий — запись в свой каталог. Прав
/// на сам файл не требуется, ровно как в Unix: переименование меняет запись
/// каталога, а не содержимое.
///
/// Оба вопроса задаются до того, как файловая система что-нибудь сделает.
/// Порядок обязателен: отказ на середине оставил бы файл видимым под двумя
/// именами или ни под одним.
pub fn rename_as(cred: Credentials, old: &str, new: &str) -> Option<VfsResult<()>> {
    let (old_parent, _) = match split_parent(old) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let (new_parent, _) = match split_parent(new) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };

    // Узлы каталогов дальше не нужны — нужен ответ «пустят ли». Сама операция
    // идёт через файловую систему, потому что затрагивает оба каталога сразу.
    if let Err(err) = resolve_as(cred, old_parent, Access::WRITE)? {
        return Some(Err(err));
    }
    if let Err(err) = resolve_as(cred, new_parent, Access::WRITE)? {
        return Some(Err(err));
    }

    // Переименование через границу монтирования — отказ, а не молчаливое
    // копирование. Причина не в лени: `rename` обещает, что содержимое не
    // читается и не пишется, а перенос между томами — это чтение и запись
    // целиком, то есть другая операция с другой ценой и другими способами
    // не удаться. Ровно так же ведёт себя `rename(2)` в Unix (`EXDEV`).
    let (Some(source), Some(target)) = (for_path(old), for_path(new)) else {
        return None;
    };
    if !Arc::ptr_eq(&source, &target) {
        return Some(Err(VfsError::Unsupported));
    }
    Some(source.rename(old, new))
}

/// Перечислить каталог корневой ФС.
///
/// Внешний `None` означает «ничего не смонтировано» — это не ошибка пути, и
/// сообщение о ней должно быть другим.
pub fn list(path: &str) -> Option<VfsResult<Vec<DirEntry>>> {
    with_fs(path, |fs| {
        let node = fs.resolve(path)?;
        node.list()
    })
}

/// Проверить корневую ФС, ничего не меняя.
///
/// Внешний `None` — «ничего не смонтировано», внутренний — «эта файловая
/// система себя проверять не умеет». Разные ответы на разные вопросы, и
/// сливать их в один было бы враньём: в первом случае корня нет вовсе, во
/// втором он есть и с ним всё в порядке настолько, насколько это вообще можно
/// узнать.
pub fn check_root() -> Option<Option<VfsResult<crate::vfs::CheckSummary>>> {
    with_root(|fs| fs.check())
}

/// Проверить **каждый** смонтированный том.
///
/// Существует потому, что с фазы 32 корень смонтирован только на чтение, а всё,
/// что пишется, живёт на разделе состояния. Проверка, которая смотрит только на
/// корень, с этого момента проверяет ровно тот том, с которым ничего не может
/// случиться, — то есть не проверяет ничего.
pub fn check_all() -> Vec<(&'static str, Option<VfsResult<crate::vfs::CheckSummary>>)> {
    // Список берётся под локом и тут же отпускается: сама проверка читает диск
    // целиком, и держать таблицу монтирования всё это время нельзя.
    let taken = distinct(true);

    let mut results = Vec::new();
    if results.try_reserve_exact(taken.len()).is_err() {
        return Vec::new();
    }
    for (prefix, fs) in taken {
        results.push((prefix, fs.check()));
    }
    results
}

/// Смонтированные тома **по одному разу каждый**.
///
/// Раздел состояния обслуживает пять веток одним объектом, и обход таблицы
/// монтирования подряд проверял бы его пять раз. Это не расточительство, а
/// неверный ответ: пятикратный `fsck` одного тома напечатал бы одни и те же
/// находки пять раз, а пятикратная пометка «закрыт чисто» — пять записей в
/// суперблок вместо одной.
///
/// `deep_first` = `true` отдаёт ветки раньше корня (так проверяют и сбрасывают),
/// `false` — наоборот (так печатают человеку).
fn distinct(deep_first: bool) -> Vec<(&'static str, Arc<dyn FileSystem>)> {
    let mounts = MOUNTS.lock();
    let mut out: Vec<(&'static str, Arc<dyn FileSystem>)> = Vec::new();
    if out.try_reserve(mounts.len()).is_err() {
        return Vec::new();
    }
    for mount in mounts.iter() {
        let name = if mount.prefix.is_empty() { "/" } else { mount.prefix };
        // Сравнение по указателю, а не по имени ФС: два разных тома ext2 — это
        // два разных `Arc`, а один том, смонтированный пять раз, — один и тот
        // же.
        if let Some(seen) = out.iter_mut().find(|(_, fs)| Arc::ptr_eq(fs, &mount.fs)) {
            // Том называется по самой короткой из своих точек монтирования:
            // раздел состояния виден в пяти местах, и звать его «/home» только
            // потому, что это имя длиннее прочих, — значит выбрать наугад.
            if name.len() < seen.0.len() {
                seen.0 = name;
            }
            continue;
        }
        out.push((name, Arc::clone(&mount.fs)));
    }
    if !deep_first {
        out.reverse();
    }
    out
}

/// Сбросить корневую ФС на носитель.
///
/// `None` означает «ничего не смонтировано» — это не отказ, а обычное
/// состояние живого ISO, и путать его с ошибкой нельзя.
pub fn sync_root() -> Option<VfsResult<()>> {
    with_root(|fs| fs.sync())
}

/// Сбросить **все** смонтированные тома и пометить их чистыми.
///
/// Вызывается перед выключением. Сбрасывать один корень с фазы 32 недостаточно
/// и даже вредно: раздел состояния — единственный, куда система вообще пишет, и
/// оставить его несброшенным значило бы, что каждое выключение требует `fsck`
/// ровно того тома, где лежат данные человека.
///
/// Возвращает первый отказ вместе с точкой монтирования: тот, кто гасит машину,
/// обязан о нём сказать, а не проглотить.
pub fn sync_all() -> Vec<(&'static str, VfsResult<()>)> {
    // Ветки сбрасываются раньше корня: на корне их точки монтирования, и
    // порядок «сначала то, что глубже» — тот же, в каком размонтируют том
    // руками.
    let taken = distinct(true);

    let mut results = Vec::new();
    if results.try_reserve_exact(taken.len()).is_err() {
        return Vec::new();
    }
    for (prefix, fs) in taken {
        results.push((prefix, fs.sync()));
    }
    results
}

/// Прочитать не более `limit` байт файла. Возвращает прочитанное и полный размер
/// файла — чтобы вызывающий мог сказать, что показал не всё.
pub fn read(path: &str, limit: usize) -> Option<VfsResult<(Vec<u8>, u64)>> {
    with_fs(path, |fs| {
        let node = fs.resolve(path)?;
        let meta = node.metadata();
        if meta.kind != NodeKind::File {
            return Err(VfsError::WrongKind);
        }
        let want = (meta.size as usize).min(limit);
        let mut buf = Vec::new();
        // `try_reserve`, а не `vec![]`: размер приходит из файловой системы, то
        // есть снаружи, и отказ аллокатора обязан стать ошибкой, а не паникой.
        buf.try_reserve_exact(want).map_err(|_| VfsError::OutOfMemory)?;
        buf.resize(want, 0);
        let read = node.read_at(0, &mut buf)?;
        buf.truncate(read);
        Ok((buf, meta.size))
    })
}
