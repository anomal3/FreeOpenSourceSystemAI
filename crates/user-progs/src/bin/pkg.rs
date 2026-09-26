//! `pkg` — установка пакетов и учёт установленного.
//!
//! # Почему это программа, а не команда оболочки
//!
//! Потому что установка пакета — не системная операция. Ей не нужен ни один
//! регистр устройства, ни один сектор мимо файловой системы и ни одно право
//! сверх тех, что есть у человека за терминалом: она читает файл и раскладывает
//! его содержимое по каталогам. Всё, что можно сделать снаружи ядра, делается
//! снаружи — и проверяется это тем, что `pkg` получает отказ в правах ровно
//! там, где его получил бы любой другой: попытка поставить пакет в каталог, к
//! которому нет доступа, кончается ошибкой вызова, а не записью.
//!
//! Сравните с `fsck` и `sysupdate`: те живут в оболочке именно потому, что
//! работают с томом мимо файловой системы, и вынести их наружу означало бы
//! отдать программе блочное устройство.
//!
//! # Куда ставятся пакеты
//!
//! В `/opt/<имя>`, а не в `/bin` и не в `/usr`. Корень системы с фазы 32
//! смонтирован только на чтение и заменяется обновлением целиком — пакет,
//! положенный внутрь него, исчез бы при первом же обновлении, причём молча.
//! `/opt` живёт на разделе состояния, который обновление не трогает.
//!
//! Реестр — там же: `/var/lib/pkg/<имя>.pkg`, и в нём лежит **манифест как
//! есть**. Не выжимка из него: удалять и проверять нужно ровно то, что было
//! положено, и своя запись о том же самом разошлась бы с манифестом ровно в
//! той мере, в какой её потом правили бы порознь.
//!
//! # Чего здесь нет
//!
//! Разрешения зависимостей. Манифест их переносит, `install` проверяет, что
//! названные пакеты уже стоят, и отказывается ставить поверх пустоты — но
//! **искать** недостающее негде: репозитория нет, потому что нет сети.

#![no_std]
#![no_main]

use fpk::{Header, Kind, Manifest, Rights};
use osupdate::keys::Trusted;
use user_progs::{
    Args, Dirent, Line, Path, close, create, exit, file_size, mkdir, open, println,
    read, read_at, readdir, remove, stat, write,
};
use user_abi::Stat;

/// Куда раскладывается содержимое пакетов.
const OPT: &str = "/opt";

/// Где лежит реестр установленного.
const REGISTRY: &str = "/var/lib/pkg";

/// Буфер под манифест.
///
/// Статический, а не на стеке: стека у программы восемь страниц, и тридцать два
/// килобайта на нём — это треть всего, что у неё есть, отданная под то, что
/// живёт от начала работы до конца.
static mut MANIFEST: [u8; fpk::MAX_MANIFEST] = [0; fpk::MAX_MANIFEST];

/// Доверенные ключи (`/os-keys`) — для пакетов, просящих право `devices`.
static mut KEYFILE: [u8; 4096] = [0; 4096];

/// Буфер, которым переливается содержимое файлов.
static mut CHUNK: [u8; 4096] = [0; 4096];

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };

    let code = match args.get(1) {
        Some("install") => match args.get(2) {
            Some(path) => install(path),
            None => usage(),
        },
        Some("list") => list(),
        Some("verify") => verify(args.get(2)),
        Some("remove") => match args.get(2) {
            Some(name) => remove_package(name),
            None => usage(),
        },
        _ => usage(),
    };

    exit(code)
}

fn usage() -> i64 {
    println("  usage: pkg install <file.fpk>");
    println("         pkg list");
    println("         pkg verify [name]");
    println("         pkg remove <name>");
    1
}

// --- установка --------------------------------------------------------------

/// Поставить пакет из файла.
///
/// Порядок шагов выбран так, чтобы отказ на любом из них не оставлял системе
/// половину пакета **без записи о нём**: реестр пишется последним, но каталог
/// пакета создаётся первым и под своим именем, поэтому неудавшаяся установка
/// видна как каталог в `/opt`, о котором `pkg list` молчит. Это хуже, чем
/// транзакция, и лучше, чем тишина; настоящая транзакция потребовала бы
/// переименования каталога целиком, а `rename` на непустом каталоге эта ФС не
/// умеет.
fn install(path: &str) -> i64 {
    let fd = open(path);
    if fd < 0 {
        Line::new()
            .str("pkg: cannot open ")
            .str(path)
            .end();
        return 1;
    }

    let header = match read_header(fd) {
        Ok(header) => header,
        Err(code) => {
            close(fd);
            return code;
        }
    };
    if header.kind != Kind::Package {
        println("pkg: this container holds a system image, not a package; use sysupdate");
        close(fd);
        return 1;
    }

    // SAFETY: программа однопоточна, и второго обращения к буферу за время
    // работы этой функции не существует.
    let manifest_bytes = unsafe { &mut *core::ptr::addr_of_mut!(MANIFEST) };
    let manifest = match read_manifest(fd, &header, manifest_bytes) {
        Ok(manifest) => manifest,
        Err(code) => {
            close(fd);
            return code;
        }
    };

    let (Ok(name), Ok(version)) = (manifest.name(), manifest.version()) else {
        println("pkg: the manifest lacks a usable name or version");
        close(fd);
        return 1;
    };

    // Права — до первой записи и до зависимостей: манифест, просящий
    // непонятное, отвергается целиком. Молча пропустить незнакомое имя значило
    // бы поставить пакет, который не заработает, а разбираться пришлось бы по
    // его поведению. Что означает каждое право и что из этого проверяется
    // сегодня — в `fpk::Rights`.
    let rights = match manifest.rights() {
        Ok(rights) => rights,
        Err(err) => {
            Line::new()
                .str("pkg: ")
                .str(err.text())
                .end();
            close(fd);
            return 1;
        }
    };

    // Право на устройство — только подписанному пакету (веха «драйверы», Д2).
    // Без IOMMU драйвер с DMA пишет в любую память машины, то есть такой пакет
    // равен ядру по силе, и верить ему можно ровно настолько, насколько
    // верят тому, кто подписал систему: ключи те же, `/os-keys`.
    if rights.contains(Rights::DEVICES) && !signed_by_trusted(fd, &header, manifest.text().as_bytes()) {
        Line::new()
            .str("pkg: ")
            .str(name)
            .str(" asks for the right to devices and is not signed by a key this system trusts; refusing")
            .end();
        close(fd);
        return 1;
    }
    for drive in manifest.drives() {
        match drive {
            Some((vendor, device)) => {
                Line::new().str("pkg: ").str(name).str(" drives ").hex4(vendor).str(":").hex4(device).end();
            }
            None => {
                Line::new().str("pkg: ").str(name).str(" names a device it drives in a form other than vendor:device").end();
                close(fd);
                return 1;
            }
        }
    }

    // Зависимости проверяются до первой записи: пакет, поставленный наполовину
    // и снесённый обратно, — это две операции там, где достаточно ни одной.
    for required in manifest.requires() {
        if !is_installed(required) {
            Line::new()
                .str("pkg: ")
                .str(name)
                .str(" requires ")
                .str(required)
                .str(", which is not installed")
                .end();
            close(fd);
            return 1;
        }
    }

    let mut root = Path::new();
    if !root.push(OPT) || !root.join(name) {
        println("pkg: the package name makes the install path too long");
        close(fd);
        return 1;
    }
    // Уже стоящий пакет не переустанавливается поверх: файлы прежней версии,
    // которых нет в новой, остались бы навсегда — и принадлежали бы пакету,
    // который их больше не помнит.
    if is_installed(name) {
        Line::new()
            .str("pkg: ")
            .str(name)
            .str(" is already installed; remove it first")
            .end();
        close(fd);
        return 1;
    }

    if !ensure_tree(OPT) || !ensure_tree(REGISTRY) {
        println("pkg: cannot create /opt or the registry directory");
        close(fd);
        return 1;
    }
    if mkdir(root.as_str(), 0o755) < 0 {
        Line::new()
            .str("pkg: cannot create ")
            .str(root.as_str())
            .end();
        close(fd);
        return 1;
    }

    let base = root.len();
    let mut files = 0u64;
    let mut bytes = 0u64;

    for entry in manifest.files() {
        let Ok(entry) = entry else {
            println("pkg: the manifest has a file line that does not parse");
            close(fd);
            return 1;
        };

        root.truncate(base);
        if !root.join(entry.path) {
            println("pkg: a path inside the package is too long");
            close(fd);
            return 1;
        }
        // Каталоги внутри пакета создаются правами `0755` и от имени того, кто
        // ставит: их права — свойство системы, а не архива. См. заголовок
        // крейта `fpk`.
        if !ensure_parents(root.as_str(), base) {
            Line::new()
                .str("pkg: cannot create the directory for ")
                .str(entry.path)
                .end();
            close(fd);
            return 1;
        }

        let written = copy_out(fd, header.payload_offset() + entry.offset, entry.size, root.as_str(), entry.mode);
        if written < 0 {
            Line::new()
                .str("pkg: cannot write ")
                .str(root.as_str())
                .end();
            close(fd);
            return 1;
        }
        files += 1;
        bytes += entry.size;
    }

    close(fd);

    // Реестр — последним: запись о пакете утверждает, что всё предыдущее
    // состоялось.
    if !write_registry(name, manifest.text()) {
        println("pkg: the files are in place but the registry entry could not be written");
        return 1;
    }

    Line::new()
        .str("pkg: installed ")
        .str(name)
        .str(" ")
        .str(version)
        .str(", ")
        .num(files)
        .str(" file(s), ")
        .num(bytes)
        .str(" bytes in ")
        .str(root_of(name).as_str())
        .end();

    // Права называются вслух при установке, и это не отчётность. Человек
    // ставит программу один раз, а решает по этой строке — «калькулятору нужна
    // сеть» видно здесь и больше нигде.
    let mut names = [0u8; 32];
    Line::new()
        .str("pkg: ")
        .str(name)
        .str(" may use ")
        .str(rights.write_names(&mut names))
        .end();
    0
}

// --- перечисление -----------------------------------------------------------

fn list() -> i64 {
    let fd = open(REGISTRY);
    if fd < 0 {
        println("pkg: no packages are installed");
        return 0;
    }

    let mut entry = Dirent::default();
    let mut count = 0u64;
    while readdir(fd, &mut entry) {
        let Some(file) = entry.name() else { continue };
        let Some(name) = file.strip_suffix(".pkg") else { continue };
        // Версия читается из самой записи реестра: держать её ещё и в имени
        // файла значило бы иметь два источника одного факта.
        Line::new()
            .str("  ")
            .str(name)
            .str("  ")
            .str(version_of(name).as_str())
            .end();
        count += 1;
    }
    close(fd);

    Line::new()
        .str("pkg: ")
        .num(count)
        .str(" package(s) installed")
        .end();
    0
}

// --- проверка ---------------------------------------------------------------

/// Сверить установленное с тем, что записано в реестре.
///
/// Без имени — все пакеты; с именем — один. Проверяется размер и контрольная
/// сумма каждого файла: размер отвечает на вопрос «файл тот», сумма — «файл не
/// менялся». Права намеренно не проверяются: их вправе поменять администратор
/// машины, и объявлять это порчей пакета было бы неверно.
fn verify(only: Option<&str>) -> i64 {
    if let Some(name) = only {
        return verify_one(name);
    }

    let fd = open(REGISTRY);
    if fd < 0 {
        println("pkg: no packages are installed");
        return 0;
    }
    // Имена собираются по одному и проверяются сразу: списка в памяти у
    // программы без кучи всё равно не построить, а каталог реестра при проверке
    // не меняется.
    let mut entry = Dirent::default();
    let mut worst = 0i64;
    let mut checked = 0u64;
    while readdir(fd, &mut entry) {
        let Some(file) = entry.name() else { continue };
        let Some(name) = file.strip_suffix(".pkg") else { continue };
        let mut owned = [0u8; 64];
        let Some(name) = keep(name, &mut owned) else { continue };
        if verify_one(name) != 0 {
            worst = 1;
        }
        checked += 1;
    }
    close(fd);

    Line::new()
        .str("pkg: verified ")
        .num(checked)
        .str(" package(s)")
        .end();
    worst
}

fn verify_one(name: &str) -> i64 {
    // SAFETY: программа однопоточна; буфер не используется ничем другим, пока
    // исполняется эта функция.
    let manifest_bytes = unsafe { &mut *core::ptr::addr_of_mut!(MANIFEST) };
    let Some(manifest) = read_registry(name, manifest_bytes) else {
        Line::new()
            .str("pkg: ")
            .str(name)
            .str(" is not installed")
            .end();
        return 1;
    };

    let mut root = root_of(name);
    let base = root.len();
    let mut bad = 0u64;
    let mut good = 0u64;

    for entry in manifest.files() {
        let Ok(entry) = entry else {
            println("pkg: the registry entry has a line that does not parse");
            return 1;
        };
        root.truncate(base);
        if !root.join(entry.path) {
            bad += 1;
            continue;
        }

        match check_file(root.as_str(), entry.size, entry.crc) {
            Ok(()) => good += 1,
            Err(why) => {
                Line::new()
                    .str("  ")
                    .str(root.as_str())
                    .str(": ")
                    .str(why)
                    .end();
                bad += 1;
            }
        }
    }

    Line::new()
        .str("pkg: ")
        .str(name)
        .str(": ")
        .num(good)
        .str(" file(s) intact, ")
        .num(bad)
        .str(" changed or missing")
        .end();
    i64::from(bad != 0)
}

/// Прочитать файл целиком и сверить длину с суммой.
fn check_file(path: &str, size: u64, crc: u32) -> Result<(), &'static str> {
    let fd = open(path);
    if fd < 0 {
        return Err("missing");
    }
    let actual = file_size(fd);
    if actual < 0 || actual as u64 != size {
        close(fd);
        return Err("size differs");
    }

    // SAFETY: программа однопоточна; буфер переливания в это время свободен.
    let chunk = unsafe { &mut *core::ptr::addr_of_mut!(CHUNK) };
    let mut sum = fpk::CRC32_INIT;
    loop {
        let read_bytes = read(fd, chunk);
        if read_bytes < 0 {
            close(fd);
            return Err("unreadable");
        }
        if read_bytes == 0 {
            break;
        }
        sum = fpk::crc32_update(sum, &chunk[..read_bytes as usize]);
    }
    close(fd);

    if sum == crc { Ok(()) } else { Err("contents changed") }
}

// --- удаление ---------------------------------------------------------------

/// Снести пакет: ровно те файлы, что в нём были, и ни одного чужого.
///
/// Каталоги удаляются только пустыми и только те, что были созданы под пакет.
/// Порядок обратный установке — от самых глубоких, — и делается это повторными
/// проходами, а не сортировкой: списка каталогов без кучи не построить, а
/// проходов по манифесту в пакете из десятка файлов всё равно два-три.
fn remove_package(name: &str) -> i64 {
    // SAFETY: программа однопоточна.
    let manifest_bytes = unsafe { &mut *core::ptr::addr_of_mut!(MANIFEST) };
    let Some(manifest) = read_registry(name, manifest_bytes) else {
        Line::new()
            .str("pkg: ")
            .str(name)
            .str(" is not installed")
            .end();
        return 1;
    };

    let mut root = root_of(name);
    let base = root.len();
    let mut removed = 0u64;
    let mut failed = 0u64;

    for entry in manifest.files() {
        let Ok(entry) = entry else { continue };
        root.truncate(base);
        if !root.join(entry.path) {
            failed += 1;
            continue;
        }
        // Отсутствующий файл — не отказ: его мог снести человек, и жаловаться
        // на то, что работа уже сделана, незачем.
        if remove(root.as_str()) < 0 && exists(root.as_str()) {
            Line::new()
                .str("  cannot remove ")
                .str(root.as_str())
                .end();
            failed += 1;
        } else {
            removed += 1;
        }
    }

    // Пустые каталоги пакета, изнутри наружу. Проходы повторяются, пока хоть
    // что-нибудь удаляется: каталог становится пустым только после того, как
    // опустеют все его подкаталоги.
    let mut progress = true;
    while progress {
        progress = false;
        for entry in manifest.files() {
            let Ok(entry) = entry else { continue };
            let mut at = 0usize;
            // Каждый префикс пути внутри пакета — кандидат в пустые каталоги.
            while let Some(cut) = entry.path[at..].find('/') {
                at += cut;
                root.truncate(base);
                if root.join(&entry.path[..at]) && remove(root.as_str()) >= 0 {
                    progress = true;
                }
                at += 1;
            }
        }
    }
    // И сам каталог пакета — последним.
    root.truncate(base);
    remove(root.as_str());

    // Файл остался — запись реестра остаётся тоже. Стёртая, она сделала бы
    // пакет «не установленным», а поставить его заново не дал бы оставшийся
    // каталог: машина застряла бы между двумя состояниями. Так случалось с
    // драйвером, который поставил `drvd` от root, а снять пытался человек.
    if failed != 0 {
        Line::new()
            .str("pkg: ")
            .str(name)
            .str(" stays installed: ")
            .num(failed)
            .str(" file(s) could not be removed")
            .end();
        return 1;
    }

    let mut registry = registry_of(name);
    if remove(registry.as_str()) < 0 {
        println("pkg: the files are gone but the registry entry stayed");
        failed += 1;
    }
    registry.truncate(0);

    Line::new()
        .str("pkg: removed ")
        .str(name)
        .str(", ")
        .num(removed)
        .str(" file(s), ")
        .num(failed)
        .str(" failure(s)")
        .end();
    i64::from(failed != 0)
}

// --- вспомогательное --------------------------------------------------------

fn read_header(fd: i64) -> Result<Header, i64> {
    let mut bytes = [0u8; fpk::HEADER_SIZE];
    let read_bytes = read_at(fd, 0, &mut bytes);
    if read_bytes != fpk::HEADER_SIZE as i64 {
        println("pkg: the file is shorter than a package header");
        return Err(1);
    }
    Header::parse(&bytes).map_err(|err| {
        Line::new()
            .str("pkg: ")
            .str(err.text())
            .end();
        1
    })
}

fn read_manifest<'a>(
    fd: i64,
    header: &Header,
    buffer: &'a mut [u8],
) -> Result<Manifest<'a>, i64> {
    let len = header.manifest_len as usize;
    if len > buffer.len() {
        println("pkg: the manifest is longer than this program can hold");
        return Err(1);
    }
    let read_bytes = read_at(fd, header.manifest_offset(), &mut buffer[..len]);
    if read_bytes != len as i64 {
        println("pkg: the manifest is truncated");
        return Err(1);
    }
    Manifest::parse(header, &buffer[..len]).map_err(|err| {
        Line::new()
            .str("pkg: ")
            .str(err.text())
            .end();
        1
    })
}

/// Перелить кусок контейнера в файл на диске.
///
/// Возвращает записанное или отрицательный код. Читается по частям намеренно:
/// пакет бывает в десятки мегабайт, а памяти у программы полмегабайта на всё.
fn copy_out(source: i64, offset: u64, size: u64, path: &str, mode: u16) -> i64 {
    // Права берутся из манифеста — иначе программа легла бы без бита исполнения
    // и не запустилась бы. Занятое имя здесь отказ, а не «обрежу и открою»:
    // пакет, ставящийся поверх чужого файла, — это не установка, а порча.
    let fd = create(path, mode);
    if fd < 0 {
        return fd;
    }
    // SAFETY: программа однопоточна; буфер свободен на время этого вызова.
    let chunk = unsafe { &mut *core::ptr::addr_of_mut!(CHUNK) };

    let mut left = size;
    let mut at = offset;
    while left > 0 {
        let want = left.min(chunk.len() as u64) as usize;
        let got = read_at(source, at, &mut chunk[..want]);
        if got <= 0 {
            close(fd);
            return -1;
        }
        let got = got as usize;
        if write(fd, &chunk[..got]) != got as i64 {
            close(fd);
            return -1;
        }
        at += got as u64;
        left -= got as u64;
    }
    close(fd);
    size as i64
}

/// Создать каталог вместе со всеми недостающими звеньями пути.
fn ensure_tree(path: &str) -> bool {
    let mut at = 1usize;
    let mut built = Path::new();
    if !built.push("/") {
        return false;
    }
    while at <= path.len() {
        let cut = path[at..].find('/').map_or(path.len(), |offset| at + offset);
        let component = &path[at..cut];
        if !component.is_empty() {
            if !built.join(component) {
                return false;
            }
            // `ERR_EXISTS` — не отказ: каталог уже есть, и это то, чего мы
            // добивались.
            if mkdir(built.as_str(), 0o755) < 0 && !exists(built.as_str()) {
                return false;
            }
        }
        at = cut + 1;
    }
    true
}

/// Создать каталоги для файла, чей путь уже собран, начиная с `base`.
fn ensure_parents(path: &str, base: usize) -> bool {
    let mut at = base;
    while let Some(cut) = path[at..].find('/') {
        let end = at + cut;
        if mkdir(&path[..end], 0o755) < 0 && !exists(&path[..end]) {
            return false;
        }
        at = end + 1;
    }
    true
}

fn exists(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) == 0
}

fn is_installed(name: &str) -> bool {
    exists(registry_of(name).as_str())
}

fn root_of(name: &str) -> Path {
    let mut path = Path::new();
    path.push(OPT);
    path.join(name);
    path
}

fn registry_of(name: &str) -> Path {
    let mut path = Path::new();
    path.push(REGISTRY);
    path.join(name);
    path.push(".pkg");
    path
}

/// Записать манифест в реестр.
fn write_registry(name: &str, text: &str) -> bool {
    let path = registry_of(name);
    // `0644`: запись реестра читают все (`pkg list` работает от любого имени),
    // а меняет только тот, кто ставит.
    let fd = create(path.as_str(), 0o644);
    if fd < 0 {
        return false;
    }
    let ok = write(fd, text.as_bytes()) == text.len() as i64;
    close(fd);
    ok
}

/// Прочитать запись реестра как манифест.
///
/// Заголовка у записи нет — это чистый текст, — поэтому сумма подставляется по
/// самому тексту: проверять реестр контрольной суммой пакета было бы нечем, а
/// разбор манифеста требует заголовка. Порча реестра ловится не здесь, а
/// проверкой файлов: испорченная запись даст пути, которых нет.
fn read_registry<'a>(name: &str, buffer: &'a mut [u8]) -> Option<Manifest<'a>> {
    let path = registry_of(name);
    let fd = open(path.as_str());
    if fd < 0 {
        return None;
    }
    let mut filled = 0usize;
    loop {
        if filled == buffer.len() {
            break;
        }
        let got = read(fd, &mut buffer[filled..]);
        if got <= 0 {
            break;
        }
        filled += got as usize;
    }
    close(fd);

    let header = Header {
        kind: Kind::Package,
        manifest_len: filled as u32,
        payload_len: 0,
        manifest_crc: fpk::crc32(&buffer[..filled]),
        payload_crc: 0,
        // Подписи у записи реестра нет и быть не может: это не контейнер, а
        // сохранённый манифест уже установленного пакета. Заголовок здесь —
        // способ переиспользовать разбор, а не описание файла.
        signature_algorithm: 0,
        signature_len: 0,
        signature: [0u8; fpk::SIGNATURE_SIZE],
    };
    Manifest::parse(&header, &buffer[..filled]).ok()
}

/// Версия установленного пакета — строкой, пригодной для печати.
fn version_of(name: &str) -> Path {
    let mut line = [0u8; 512];
    let path = registry_of(name);
    let fd = open(path.as_str());
    let mut out = Path::new();
    if fd < 0 {
        out.push("(unreadable)");
        return out;
    }
    let got = read(fd, &mut line);
    close(fd);
    if got <= 0 {
        out.push("(unreadable)");
        return out;
    }
    let text = core::str::from_utf8(&line[..got as usize]).unwrap_or("");
    for entry in text.lines() {
        if let Some(value) = entry.trim().strip_prefix("version=") {
            out.push(value.trim());
            return out;
        }
    }
    out.push("(no version)");
    out
}

/// Скопировать имя в буфер вызывающего.
///
/// Нужно ровно затем, чтобы имя пережило следующий `readdir`: запись каталога
/// приезжает в одну и ту же структуру, и срез поверх неё указывает на имя
/// **следующего** файла, как только цикл сделает шаг.
fn keep<'a>(name: &str, buffer: &'a mut [u8]) -> Option<&'a str> {
    if name.len() > buffer.len() {
        return None;
    }
    buffer[..name.len()].copy_from_slice(name.as_bytes());
    core::str::from_utf8(&buffer[..name.len()]).ok()
}

/// Подписан ли пакет ключом, которому верит эта система.
///
/// Подписывается заголовок и манифест (`fpk::signed_digest`); нагрузка входит
/// в подпись через свои SHA-256, записанные в манифесте, — их сверяет разбор
/// при чтении файлов.
fn signed_by_trusted(fd: i64, header: &Header, manifest: &[u8]) -> bool {
    if header.signature_algorithm != fpk::SIGNATURE_ED25519 {
        return false;
    }
    let mut head = [0u8; fpk::HEADER_SIZE];
    if read_at(fd, 0, &mut head) != fpk::HEADER_SIZE as i64 {
        return false;
    }
    let digest = fpk::signed_digest(&head, manifest);
    let signature = &header.signature[..usize::from(header.signature_len).min(header.signature.len())];

    let keys = open("/os-keys");
    if keys < 0 {
        return false;
    }
    // SAFETY: программа однопоточна.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(KEYFILE) };
    let got = read(keys, buffer);
    close(keys);
    let Ok(text) = core::str::from_utf8(&buffer[..got.max(0) as usize]) else {
        return false;
    };
    Trusted::parse(text).verifies(&digest, signature)
}
