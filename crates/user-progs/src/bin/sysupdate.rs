//! `sysupdate` — обновление системы по сети.
//!
//! ```text
//!   sysupdate check [путь]   что предлагает сервер и новее ли оно нашего
//!   sysupdate get   [путь]   скачать и проверить, ничего не устанавливая
//!   sysupdate apply [файл]   поставить скачанное в свободный слот
//! ```
//!
//! # Порядок, в котором машина обновляется
//!
//! ```text
//!   index → подпись индекса → сравнение версий → загрузка в /var/cache/updates
//!         → SHA-256 → apply (подпись контейнера проверяет ядро) → перезагрузка
//!         → три попытки → возврат на прежний слот сам, если не поднялось
//! ```
//!
//! # Почему качает программа, а ставит ядро
//!
//! Потому что это разные права. Скачать файл и положить его на раздел состояния
//! умеет третье кольцо: сокеты и файлы у него есть. А запись в слот идёт **мимо
//! файловой системы** — прямо в сектора неактивного раздела и в FAT-том ESP,
//! который никто не монтировал; отдать это программе значило бы отдать ей
//! блочное устройство. Поэтому последний шаг — системный вызов `SYS_UPDATE`, и
//! подпись контейнера проверяет ядро, а не мы.
//!
//! # Зачем тогда проверять подпись индекса здесь
//!
//! Она отвечает на другой вопрос: стоит ли вообще тащить по сети десятки
//! мегабайт и что именно тащить. Подменённый индекс без проверки означал бы
//! «качайте что попало и сколько попало» — отказ в обслуживании, который ядро
//! поймает только в конце, потратив полчаса и место на разделе. Ключи для этой
//! проверки те же самые, `/os-keys`, и разбор тот же (`osupdate`).
//!
//! # Серверов теперь несколько, и это главное в фазе 39a
//!
//! `update.cfg` содержит столько строк `server=`, сколько нужно; первый не
//! ответил — берём следующий. Второй канал — GitHub Releases, и он **обязан**
//! идти по HTTPS: другого GitHub не отдаёт. Отсюда весь TLS (крейты `tls` и
//! `x509`) и хранилище корней `ca.pem` рядом с этим файлом.
//!
//! Транспорт при этом ничего не решает: доверие как держалось на подписи
//! Ed25519, так и держится. TLS нужен затем, чтобы GitHub вообще ответил, а не
//! затем, чтобы ему поверить.
//!
//! # Откуда берутся настройки
//!
//! Из `update.cfg` и `ca.pem` — сначала `/etc`, потом эталон образа (см.
//! `config_path`). Ровно тот случай, ради которого умолчания и заведены: адреса
//! репозиториев и набор корней приезжают с образом, а машина, которой нужны
//! другие, кладёт свои файлы в `/etc`, и обновление образа их не трогает.

#![no_std]
#![no_main]

use osupdate::index::{self, Index};
use osupdate::keys::{self, Trusted};
use user_progs::{
    Args, ERR_UPDATE_REFUSED, SLOT_B, apply_update, close, config_path, error, error_num, exit,
    http, open, print, print_u64, println, read, time_now, uid, write,
};

/// Имя файла настроек.
const CONFIG: &str = "update.cfg";

/// Имя файла с доверенными корнями.
const CA: &str = "ca.pem";

/// Куда кладётся скачанное.
///
/// `/var` — раздел состояния: единственное место, куда система вообще пишет.
/// Имя фиксированное, а не из индекса, и это не мелочь: имя из индекса пришло бы
/// из сети, а вслед за ним пришлось бы решать, что делать с накопившимися
/// файлами. Один файл, перезаписываемый каждый раз, не накапливается.
const CACHE_DIR: &str = "/var/cache";
const CACHE_UPDATES: &str = "/var/cache/updates";
const CACHE_FILE: &str = "/var/cache/updates/system.fpk";

/// Где лежат доверенные ключи. Тот же файл, что читает ядро.
const KEYS: &str = "/os-keys";

/// Версия, с которой машина работает сейчас.
const OS_RELEASE: &str = "/os-release";

/// Сколько репозиториев помещается в список.
///
/// Четыре при двух используемых (свой сервер и GitHub). Предел существует
/// потому, что список лежит в массиве, а не потому, что больше не бывает.
const MAX_SERVERS: usize = 4;

/// Для какой архитектуры собрана эта программа.
///
/// Строка та же, что пишет в манифест и в индекс `xtask` (`Arch::name`), и та
/// же, с которой сверяется ядро при `apply`. Разъехавшись, они дали бы «в
/// индексе нет ничего для этой машины» на репозитории, где всё есть.
#[cfg(target_arch = "x86_64")]
const ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const ARCH: &str = "aarch64";

/// Рабочий буфер: сюда читается тело ответа.
///
/// Статик, а не массив на стеке: стека у программы 64 КиБ, и восьмикилобайтный
/// буфер в кадре — это восьмая его часть на всю глубину вызовов, включая
/// проверку подписи, которая и так близко ко дну (фаза 38, дефект 4).
static mut SCRATCH: [u8; 8 * 1024] = [0; 8 * 1024];

/// Буфер под байты **с провода**.
///
/// Отдельный от `SCRATCH` потому, что при HTTPS их два разных потока: в этом
/// лежит зашифрованное, только что прочитанное из сокета, в том —
/// расшифрованное. При обычном HTTP он не используется вовсе.
static mut WIRE: [u8; 4 * 1024] = [0; 4 * 1024];

/// Буфер под индекс и его подпись.
static mut TEXT: [u8; index::LIMIT] = [0; index::LIMIT];

/// Буфер под файл ключей.
static mut KEYFILE: [u8; keys::LIMIT] = [0; keys::LIMIT];

/// Буферы соединения TLS — почти шестьдесят килобайт.
///
/// В статике, а не на стеке, по той же причине, что и всё остальное здесь: стек
/// программы — 64 КиБ, и одно рукопожатие целиком его бы и заняло.
static mut TLS_IO: tls::Buffers = tls::Buffers::new();

/// Текст `ca.pem`, как он прочитан с диска.
static mut CA_TEXT: [u8; 16 * 1024] = [0; 16 * 1024];

/// Он же, разобранный в DER: сюда указывают корни хранилища.
static mut CA_DER: [u8; 12 * 1024] = [0; 12 * 1024];

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: пара (argc, argv) приходит от ядра, которое собрало её из
    // командной строки и держит живой всё время работы программы.
    let args = unsafe { Args::new(argc, argv) };
    let Some(command) = args.get(1) else {
        usage();
        exit(2);
    };
    let argument = args.get(2);

    match command {
        "check" => exit(check(argument)),
        "get" => exit(get(argument)),
        "apply" => exit(apply(argument.unwrap_or(CACHE_FILE))),
        _ => {
            usage();
            exit(2);
        }
    }
}

fn usage() {
    println("usage: sysupdate check|get [repository path]");
    println("       sysupdate apply [file]");
    println("  the servers are named in /etc/update.cfg, or in the image defaults");
}

/// Строка адреса, собираемая по кусочкам.
///
/// Своя, а не [`user_progs::Path`]: тот собирается под предел путей файловой
/// системы, а здесь предел другой и другой смысл — это адрес целиком, и у
/// GitHub он с подписью в тысячу знаков.
struct Text {
    buffer: [u8; http::MAX_URL],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self { buffer: [0; http::MAX_URL], len: 0 }
    }

    fn push(&mut self, text: &str) -> bool {
        if self.len + text.len() > self.buffer.len() {
            return false;
        }
        self.buffer[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        true
    }

    fn push_u16(&mut self, mut value: u16) -> bool {
        let mut digits = [0u8; 5];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        // SAFETY: в `digits` записаны только цифры ASCII.
        self.push(unsafe { core::str::from_utf8_unchecked(&digits[at..]) })
    }

    fn as_str(&self) -> &str {
        // SAFETY: в буфер попадают только байты из `&str`, то есть UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.buffer[..self.len]) }
    }
}

/// Один репозиторий: адрес, оканчивающийся косой чертой.
struct Server {
    base: Text,
}

impl Server {
    /// Собрать полный адрес файла в этом репозитории.
    fn url(&self, name: &str, out: &mut Text) -> bool {
        out.len = 0;
        out.push(self.base.as_str()) && out.push(name)
    }
}

/// Список репозиториев в том порядке, в котором их пробовать.
struct Servers {
    list: [Server; MAX_SERVERS],
    len: usize,
}

/// Прочитать настройки и собрать список репозиториев.
///
/// `override_path` — второй аргумент командной строки. Он **не** заменяет
/// сервер, только путь на нём: адрес репозитория — это решение о машине, и
/// принимать его из строки, набранной в чужой оболочке, не стоит. Применяется он
/// только к записям, заданным старым способом (`server=` без схемы): у записи,
/// заданной полным адресом, путь — часть адреса.
fn servers(override_path: Option<&str>) -> Option<Servers> {
    let Some(path) = config_path(CONFIG) else {
        error("sysupdate: no ");
        error(CONFIG);
        error(" in /etc nor in the image defaults\n");
        return None;
    };
    let fd = open(path.as_str());
    if fd < 0 {
        error("sysupdate: cannot read ");
        error(path.as_str());
        error("\n");
        return None;
    }
    // SAFETY: статик читается и разбирается только здесь, и только этой задачей:
    // программа однопоточная, а до второй копии себя ей не дотянуться.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(SCRATCH) };
    let got = read(fd, buffer);
    close(fd);
    if got <= 0 {
        error("sysupdate: the configuration file is empty\n");
        return None;
    }
    let Ok(text) = core::str::from_utf8(&buffer[..got as usize]) else {
        error("sysupdate: the configuration file is not text\n");
        return None;
    };

    let mut out = Servers {
        list: [const { Server { base: Text::new() } }; MAX_SERVERS],
        len: 0,
    };
    // Старая запись — три строки: `server=` с именем, `port=` и `path=`. Она
    // осталась рабочей намеренно: этот файл лежит на разделе состояния у каждой
    // уже установленной машины, и обновление до него не дотягивается. Ломать
    // его — значит выключить обновления ровно тем, кто уже обновлялся.
    let mut host: Option<&str> = None;
    let mut port = 80u16;
    let mut repo: Option<&str> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "server" => {
                if value.starts_with("http://") || value.starts_with("https://") {
                    // Полный адрес — сам себе репозиторий.
                    if out.len == MAX_SERVERS {
                        error("sysupdate: more server= lines than this system will hold\n");
                        continue;
                    }
                    let entry = &mut out.list[out.len];
                    entry.base.len = 0;
                    if !entry.base.push(value) {
                        error("sysupdate: that repository address is too long\n");
                        continue;
                    }
                    if !value.ends_with('/') && !entry.base.push("/") {
                        continue;
                    }
                    out.len += 1;
                } else {
                    // Старая запись: собирается после того, как прочитаны все
                    // строки, — `port=` и `path=` вправе стоять после неё.
                    host = Some(value);
                }
            }
            "port" => port = value.parse::<u16>().unwrap_or(80),
            "path" => repo = Some(value),
            _ => {}
        }
    }

    if let Some(host) = host {
        // Старая запись становится **первой** в списке: до фазы 39a она была
        // единственной, и машина, у которой в `/etc` лежит своя, обязана
        // по-прежнему ходить туда в первую очередь.
        let mut entry = Server { base: Text::new() };
        let mut ok = entry.base.push("http://") && entry.base.push(host);
        if port != 80 {
            ok = ok && entry.base.push(":") && entry.base.push_u16(port);
        }
        let wanted = override_path.or(repo).unwrap_or("/");
        if !wanted.starts_with('/') {
            ok = ok && entry.base.push("/");
        }
        ok = ok && entry.base.push(wanted);
        if !wanted.ends_with('/') {
            ok = ok && entry.base.push("/");
        }
        if !ok {
            error("sysupdate: that repository path is too long\n");
            return None;
        }
        if out.len == MAX_SERVERS {
            out.len -= 1;
        }
        out.list[..out.len + 1].rotate_right(1);
        out.list[0] = entry;
        out.len += 1;
    }

    if out.len == 0 {
        error("sysupdate: no server= line in the configuration\n");
        return None;
    }
    for index in 0..out.len {
        print("sysupdate: repository ");
        println(out.list[index].base.as_str());
    }
    Some(out)
}

/// Прочитать `ca.pem` и построить хранилище корней.
///
/// Отсутствие файла — не ошибка: репозиторий по обычному HTTP работает и без
/// него. Ошибка — молчание об этом, поэтому здесь строка в журнал.
fn trusted_roots() -> Option<x509::Store<'static>> {
    let path = config_path(CA)?;
    let fd = open(path.as_str());
    if fd < 0 {
        return None;
    }
    // SAFETY: статик принадлежит этой задаче, см. пояснение в `servers`.
    let text_buffer = unsafe { &mut *core::ptr::addr_of_mut!(CA_TEXT) };
    let got = read(fd, text_buffer);
    close(fd);
    let Ok(text) = core::str::from_utf8(&text_buffer[..got.max(0) as usize]) else {
        error("sysupdate: ca.pem is not text\n");
        return None;
    };
    // SAFETY: статик принадлежит этой задаче, см. пояснение в `servers`.
    let der = unsafe { &mut *core::ptr::addr_of_mut!(CA_DER) };
    match x509::Store::parse_pem(text, der) {
        Ok(store) if store.is_empty() => {
            error("sysupdate: ca.pem holds no certificate\n");
            None
        }
        Ok(store) => Some(store),
        Err(err) => {
            error("sysupdate: ca.pem: ");
            error(err.text());
            error("\n");
            None
        }
    }
}

/// Что предлагает сервер.
struct Offer {
    version: [u8; 32],
    version_len: usize,
    file: [u8; 64],
    file_len: usize,
    size: u64,
    sha256: [u8; 32],
    /// Который из репозиториев ответил.
    server: usize,
}

impl Offer {
    fn version(&self) -> &str {
        // SAFETY: копировалось из `&str`.
        unsafe { core::str::from_utf8_unchecked(&self.version[..self.version_len]) }
    }

    fn file(&self) -> &str {
        // SAFETY: копировалось из `&str`.
        unsafe { core::str::from_utf8_unchecked(&self.file[..self.file_len]) }
    }
}

/// Обойти репозитории по очереди и взять предложение у первого, кто ответил.
///
/// Порядок обязателен: **сначала подпись, потом содержимое**. Разобрать сначала,
/// а проверить потом — значит принять решение по неподписанным данным и
/// объяснять потом, почему машина полезла качать файл, которого никто не
/// подписывал.
fn offer(servers: &Servers, trust: &mut Option<http::Trust<'static>>) -> Option<Offer> {
    let trusted = trusted_keys()?;

    for index in 0..servers.len {
        let server = &servers.list[index];
        let Some(index_text) = fetch_text(server, "index", trust) else {
            continue;
        };
        // Подпись читается **после** индекса и в тот же буфер нельзя: индекс
        // нужен целиком, чтобы посчитать по нему хеш. Поэтому под подпись —
        // стек: она короткая, строка на 140 знаков.
        let mut signature_text = [0u8; 256];
        let Some(signature_len) = fetch_into(server, "index.sig", &mut signature_text, trust)
        else {
            continue;
        };
        let Ok(signature_text) = core::str::from_utf8(&signature_text[..signature_len]) else {
            error("sysupdate: index.sig is not text\n");
            continue;
        };
        let Some(signature) = index::parse_signature(signature_text) else {
            error("sysupdate: index.sig does not hold an ed25519 signature\n");
            continue;
        };

        let digest = index::digest(index_text.as_bytes());
        if !trusted.verifies(&digest, &signature) {
            error("sysupdate: the index signature does not match any key this system trusts\n");
            continue;
        }
        println("sysupdate: the index is signed by a key this system trusts");

        let parsed = match Index::parse(index_text) {
            Ok(parsed) => parsed,
            Err(err) => {
                error("sysupdate: ");
                error(err.text());
                error("\n");
                continue;
            }
        };
        let image = match parsed.image(ARCH) {
            Ok(image) => image,
            Err(err) => {
                error("sysupdate: ");
                error(err.text());
                error("\n");
                continue;
            }
        };

        let mut offer = Offer {
            version: [0; 32],
            version_len: 0,
            file: [0; 64],
            file_len: 0,
            size: image.size,
            sha256: image.sha256,
            server: index,
        };
        if image.version.len() > offer.version.len() || image.file.len() > offer.file.len() {
            error("sysupdate: the index entry names things too long to be real\n");
            continue;
        }
        offer.version_len = image.version.len();
        offer.version[..offer.version_len].copy_from_slice(image.version.as_bytes());
        offer.file_len = image.file.len();
        offer.file[..offer.file_len].copy_from_slice(image.file.as_bytes());
        return Some(offer);
    }

    println("sysupdate: no repository in the configuration offered an update");
    None
}

/// Прочитать `/os-keys` — те же ключи, которыми ядро проверяет контейнер.
fn trusted_keys() -> Option<Trusted> {
    let fd = open(KEYS);
    if fd < 0 {
        error("sysupdate: this system trusts no update keys (no ");
        error(KEYS);
        error("); refusing\n");
        return None;
    }
    // SAFETY: статик принадлежит этой задаче, см. пояснение в `servers`.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(KEYFILE) };
    let got = read(fd, buffer);
    close(fd);
    let text = match core::str::from_utf8(&buffer[..got.max(0) as usize]) {
        Ok(text) => text,
        Err(_) => {
            error("sysupdate: /os-keys is not text\n");
            return None;
        }
    };
    let trusted = Trusted::parse(text);
    if trusted.is_empty() {
        error("sysupdate: /os-keys lists no usable key; refusing\n");
        return None;
    }
    Some(trusted)
}

/// Скачать небольшой текстовый файл в общий буфер и вернуть его как строку.
fn fetch_text(
    server: &Server,
    name: &str,
    trust: &mut Option<http::Trust<'static>>,
) -> Option<&'static str> {
    // SAFETY: статик принадлежит этой задаче, см. пояснение в `servers`.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(TEXT) };
    let len = fetch_into(server, name, buffer, trust)?;
    match core::str::from_utf8(&buffer[..len]) {
        Ok(text) => Some(text),
        Err(_) => {
            error("sysupdate: the index is not text\n");
            None
        }
    }
}

/// Скачать небольшой файл в буфер вызывающего. Возвращает длину.
fn fetch_into(
    server: &Server,
    name: &str,
    out: &mut [u8],
    trust: &mut Option<http::Trust<'static>>,
) -> Option<usize> {
    let mut url = Text::new();
    if !server.url(name, &mut url) {
        error("sysupdate: that path is too long\n");
        return None;
    }
    let mut filled = 0usize;
    let mut overflow = false;
    let result = with_buffers(|io| {
        http::get(url.as_str(), trust.as_mut(), io, &mut |chunk: &[u8]| {
            if filled + chunk.len() > out.len() {
                overflow = true;
                return false;
            }
            out[filled..filled + chunk.len()].copy_from_slice(chunk);
            filled += chunk.len();
            true
        })
    });
    match result {
        Ok(_) => Some(filled),
        Err(err) => {
            if overflow {
                error("sysupdate: ");
                error(name);
                error(" is larger than this system will read\n");
            } else {
                complain(name, err);
            }
            None
        }
    }
}

/// Одолжить оба рабочих буфера тому, кто качает.
///
/// Замыканием, а не двумя ссылками наружу: статики берутся в одном месте, и
/// одно место проще проверить глазами, чем шесть.
fn with_buffers<T>(body: impl FnOnce(&mut http::Buffers<'_>) -> T) -> T {
    // SAFETY: статики принадлежат этой задаче, см. пояснение в `servers`.
    let wire = unsafe { &mut *core::ptr::addr_of_mut!(WIRE) };
    // SAFETY: то же самое.
    let scratch = unsafe { &mut *core::ptr::addr_of_mut!(SCRATCH) };
    let mut io = http::Buffers { wire, body: scratch };
    body(&mut io)
}

/// Сказать про отказ HTTP вслух и с числом там, где число есть.
fn complain(name: &str, err: http::Error) {
    error("sysupdate: ");
    error(name);
    error(": ");
    error(err.text());
    if let http::Error::Status(code) = err {
        error(" (");
        error_num(i64::from(code));
        error(")");
    }
    if let http::Error::Short { got, want } = err {
        error(" (");
        error_num(got as i64);
        error(" of ");
        error_num(want as i64);
        error(" bytes)");
    }
    error("\n");
}

/// Собрать всё, что нужно для похода в сеть: список серверов и доверие.
fn prepare(path: Option<&str>) -> Option<(Servers, Option<http::Trust<'static>>)> {
    let servers = servers(path)?;
    let roots = trusted_roots();
    let trust = roots.map(|roots| {
        println("sysupdate: https is available; the roots come from ca.pem");
        // SAFETY: статик принадлежит этой задаче, см. пояснение в `servers`.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(TLS_IO) };
        // Часы нужны, чтобы проверять сроки действия сертификатов. Ноль
        // означает «часы неизвестны», и тогда сроки не проверяются — сказать об
        // этом надо вслух, а не молча пропустить проверку.
        let now = time_now() as i64;
        if now == 0 {
            println("sysupdate: this system does not know the date; certificate dates unchecked");
        }
        http::Trust { buffers, roots, now }
    });
    if trust.is_none() {
        println("sysupdate: no ca.pem, so https repositories will be skipped");
    }
    Some((servers, trust))
}

/// `check`: что предлагает сервер и новее ли оно нашего.
fn check(path: Option<&str>) -> i64 {
    let Some((servers, mut trust)) = prepare(path) else { return 1 };
    let Some(offer) = offer(&servers, &mut trust) else { return 1 };

    print("sysupdate: the server offers FreeOS ");
    print(offer.version());
    print(" for ");
    print(ARCH);
    print(", ");
    print_u64(offer.size / (1024 * 1024));
    println(" MiB");

    match installed_version() {
        Some(installed) => {
            let have = installed.as_str();
            print("sysupdate: this system runs ");
            println(have);
            if osupdate::newer(offer.version(), have) {
                println("sysupdate: that is newer; run 'sysupdate get' to download it");
                0
            } else {
                // Не ошибка: «новее ничего нет» — это нормальный, самый частый
                // ответ. Код возврата всё-таки отличается от нуля, потому что
                // спрашивать об этом будет не только человек.
                println("sysupdate: nothing newer is offered");
                3
            }
        }
        None => {
            // Версии своей системы не знаем — значит сравнивать не с чем.
            // Ставить в таком состоянии нельзя: запрет отката держится именно на
            // этом сравнении, и «версия неизвестна, ставим что дают» открыло бы
            // ровно ту дверь, которую он закрывает.
            println("sysupdate: this system does not say which version it is; refusing");
            1
        }
    }
}

/// `get`: скачать и проверить, ничего не устанавливая.
fn get(path: Option<&str>) -> i64 {
    if uid() != 0 {
        println("sysupdate: only root downloads updates");
        return 1;
    }
    let Some((servers, mut trust)) = prepare(path) else { return 1 };
    let Some(offer) = offer(&servers, &mut trust) else { return 1 };

    match installed_version() {
        Some(installed) if !osupdate::newer(offer.version(), installed.as_str()) => {
            print("sysupdate: the server offers ");
            print(offer.version());
            print(" and this system runs ");
            println(installed.as_str());
            println("sysupdate: nothing to do");
            return 3;
        }
        Some(_) => {}
        None => {
            println("sysupdate: this system does not say which version it is; refusing");
            return 1;
        }
    }

    // Каталог заводится молча: он мог остаться от прошлого раза, и это не
    // ошибка. Отсутствие каталога — тоже: раздел состояния мог быть создан
    // установщиком без него.
    let _ = user_progs::mkdir(CACHE_DIR, 0o755);
    let _ = user_progs::mkdir(CACHE_UPDATES, 0o755);

    let fd = create_truncated(CACHE_FILE);
    if fd < 0 {
        error("sysupdate: cannot write ");
        error(CACHE_FILE);
        error("\n");
        return 1;
    }

    let mut url = Text::new();
    // Образ берётся у **того же** репозитория, который отдал подписанный индекс:
    // взять индекс у одного, а файл у другого значило бы проверять хеш не того,
    // что качали.
    if !servers.list[offer.server].url(offer.file(), &mut url) {
        error("sysupdate: that path is too long\n");
        close(fd);
        return 1;
    }

    print("sysupdate: downloading ");
    print(offer.file());
    print(" (");
    print_u64(offer.size);
    println(" bytes)");

    let mut hasher = fpk::Hasher::new();
    let mut written = 0u64;
    let mut failed = false;
    // Отчёт о ходе — каждые четыре мегабайта. Молчащая на десять минут
    // программа неотличима от повисшей, а мерить надо тем, что видно снаружи:
    // строкой в журнале.
    let mut reported = 0u64;
    let result = with_buffers(|io| {
        http::get(url.as_str(), trust.as_mut(), io, &mut |chunk: &[u8]| {
            // Хеш считается **по дороге**, а не по записанному файлу: второй
            // проход означал бы прочитать двадцать пять мегабайт ещё раз — и
            // проверить не то, что приехало, а то, что прочиталось со своего же
            // диска.
            hasher.update(chunk);
            let mut done = 0usize;
            while done < chunk.len() {
                let wrote = write(fd, &chunk[done..]);
                if wrote <= 0 {
                    failed = true;
                    return false;
                }
                done += wrote as usize;
            }
            written += chunk.len() as u64;
            if written - reported >= 4 * 1024 * 1024 {
                reported = written;
                print("sysupdate: ");
                print_u64(written / (1024 * 1024));
                print(" of ");
                print_u64(offer.size / (1024 * 1024));
                println(" MiB");
            }
            true
        })
    });
    close(fd);

    if let Err(err) = result {
        if failed {
            println("sysupdate: writing to /var failed; is there room left?");
        } else {
            complain(offer.file(), err);
        }
        // Недокачанный файл убирается: оставленный, он выглядит как готовое
        // обновление, и следующий `apply` наткнётся на него, а не на отказ.
        let _ = user_progs::remove(CACHE_FILE);
        return 1;
    }

    if written != offer.size {
        print("sysupdate: got ");
        print_u64(written);
        print(" bytes and the index promised ");
        print_u64(offer.size);
        println("");
        let _ = user_progs::remove(CACHE_FILE);
        return 1;
    }
    if hasher.finish() != offer.sha256 {
        // Подпись контейнера проверит ядро, но сказать об этом надо здесь и
        // сейчас: хеш из **подписанного** индекса не сошёлся, значит по дороге
        // приехало не то, что выложили.
        println("sysupdate: the download does not match the sha256 in the signed index");
        let _ = user_progs::remove(CACHE_FILE);
        return 1;
    }

    print("sysupdate: downloaded and verified ");
    print(offer.version());
    print(" into ");
    println(CACHE_FILE);
    println("sysupdate: run 'sysupdate apply' to put it into the free slot");
    0
}

/// `apply`: отдать файл ядру.
fn apply(path: &str) -> i64 {
    print("sysupdate: applying ");
    println(path);
    println("sysupdate: this takes a while; the machine stays usable meanwhile");
    let result = apply_update(path);
    match result {
        slot if slot >= 0 => {
            print("sysupdate: slot ");
            print(if slot == SLOT_B { "B" } else { "A" });
            println(" is active from the next boot");
            println("sysupdate: reboot to use it; the previous slot returns by itself if it fails");
            0
        }
        ERR_UPDATE_REFUSED => {
            // Причина уже напечатана ядром — оно единственное её знает целиком.
            println("sysupdate: the system refused this update; the reason is in the log above");
            1
        }
        code => {
            error("sysupdate: apply failed with code ");
            error_num(code);
            error("\n");
            1
        }
    }
}

/// Версия установленной системы — из `/os-release` её образа.
struct Version {
    buffer: [u8; 32],
    len: usize,
}

impl Version {
    fn as_str(&self) -> &str {
        // SAFETY: копировалось из `&str`.
        unsafe { core::str::from_utf8_unchecked(&self.buffer[..self.len]) }
    }
}

fn installed_version() -> Option<Version> {
    let fd = open(OS_RELEASE);
    if fd < 0 {
        return None;
    }
    let mut buffer = [0u8; 512];
    let got = read(fd, &mut buffer);
    close(fd);
    let text = core::str::from_utf8(&buffer[..got.max(0) as usize]).ok()?;
    for line in text.lines() {
        let Some(value) = line.trim().strip_prefix("version=") else {
            continue;
        };
        let value = value.trim();
        let mut version = Version { buffer: [0; 32], len: value.len().min(32) };
        version.buffer[..version.len].copy_from_slice(&value.as_bytes()[..version.len]);
        return Some(version);
    }
    None
}

/// Создать файл заново, даже если он уже есть.
///
/// Не `create`: тот отказывается на занятом имени намеренно (см. договор
/// `SYS_CREATE`), а здесь занятое имя — обычное дело: там лежит прошлая
/// загрузка, и она нам не нужна.
fn create_truncated(path: &str) -> i64 {
    user_progs::open_write(path, true, true)
}
