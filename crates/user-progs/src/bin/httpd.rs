// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! `httpd` — веб-сервер: отдаёт файлы, проксирует чужие и считает сам себя.
//!
//! # Зачем он в этой системе
//!
//! Затем же, зачем был написан `echod`, только всерьёз. Сеть у нас своя от
//! кадра до потока, и проверялась она короткими обменами: рукопожатие, пара
//! килобайт, закрытие. Два дефекта TCP, из-за которых обновление по сети шло
//! три килобайта в секунду и обрывалось на сороковой секунде, нашлись **только**
//! под настоящей нагрузкой — когда через стек пошли десятки мегабайт сразу в
//! несколько соединений. Веб-сервер такую нагрузку создаёт сам, каждый день, и
//! создаёт её с той стороны, с которой её у нас ещё не было: не мы качаем, а у
//! нас качают, и не по одному соединению, а по нескольким разом.
//!
//! Всё остальное — статика, прокси, `/metrics` — это то, ради чего веб-сервер
//! ставят на машину вообще, и оно здесь настоящее, а не изображённое.
//!
//! # Несколько соединений сразу, но без потоков
//!
//! Потоков внутри программы у нас нет, а сервер, обслуживающий одно соединение
//! за раз, не нагружает сеть: пока он отдаёт файл одному, остальные ждут в
//! очереди ядра, и никакой одновременности в стеке не возникает. Поэтому здесь
//! опрос: [`MAX_CLIENTS`] соединений живут рядом, у каждого своё состояние, и
//! круг цикла продвигает каждое настолько, насколько оно готово продвинуться.
//! Ничто нигде не ждёт: чтение, отправка и приём от того, кого мы проксируем,
//! возвращают «пока нечего», и это нормальный ход дел, а не отказ.
//!
//! Цена решения названа вслух: соединений ровно четыре, пятому клиенту придётся
//! подождать в очереди ядра. Больше — это больше сокетов, а сокет в этой
//! системе стоит приёмного и отправного буфера по восемь килобайт, и их всего
//! шестнадцать на всю машину.
//!
//! # Путь в запросе — самое опасное место
//!
//! Разбор пути живёт не здесь, а в крейте `http`, где он фаззится и проверяется
//! на хосте. Здесь важно другое правило, и оно короткое: **к корню сайта
//! дописывается только то, что вернул разбор**. Ни одной строки от клиента
//! напрямую.
//!
//! # Чего этот сервер не умеет, и это сказано, а не скрыто
//!
//! * **Тела запроса.** `POST` получит `405`: тело потребовало бы либо кучи, либо
//!   временного файла, а отдавать наружу и то и другое по первому же запросу
//!   незнакомца — плохая мысль. Проксируются тоже только `GET` и `HEAD`.
//! * **`Transfer-Encoding: chunked`.** Ни в запросе, ни от того, кого мы
//!   проксируем: разбор кусков — это ещё один способ посчитать границу
//!   сообщения, то есть ещё одно место, где мы можем разойтись с собеседником.
//!   Наш ответ всегда с `Content-Length`; чужой, пришедший кусками, — `502`.
//! * **TLS.** Сервер слушает открытый порт. Крейт `tls` у нас клиентский: он
//!   умеет проверять чужую цепочку, но не умеет предъявлять свою. Сервер за
//!   TLS — это отдельная работа, и она не сделана.
//! * **Списка каталога.** Каталог без `index.html` — это `404`, а не
//!   перечисление файлов: раздавать наружу имена того, что лежит в каталоге,
//!   по умолчанию не следует.

#![no_std]
#![no_main]

use http::{Body, Method, Request, Response};
use user_abi::Stat;
use user_progs::{
    Args, ERR_AGAIN, KIND_DIRECTORY, KIND_FILE, Line, MAX_PATH, Path, accept, bind, close,
    close_socket, config_path, connect, error, exit, listen, open, peer, read, recv, resolve, send,
    shutdown, sleep_ms, stat, stream, stream_state, uptime_ms,
};

/// Порт, на котором сервер слушает, если не сказано иначе.
///
/// Восемь тысяч восемьдесят, а не восемьдесят: порт ниже 1024 в чужих системах
/// требует прав, а сервер, которому для работы нужны особые права, однажды их и
/// получит. Здесь их не нужно.
const DEFAULT_PORT: u16 = 8080;

/// Каталог сайта по умолчанию.
const DEFAULT_ROOT: &str = "/usr/share/httpd";

/// Файл, который отдаётся вместо каталога.
const INDEX: &str = "index.html";

/// Имя файла настроек в `/etc`.
const CONFIG: &str = "httpd.cfg";

/// Путь, по которому сервер рассказывает о себе числами.
const METRICS: &str = "/metrics";

/// Сколько соединений живёт одновременно.
const MAX_CLIENTS: usize = 4;

/// Сколько каталогов можно подставить в дерево сайта.
const MAX_MOUNTS: usize = 4;

/// Сколько приставок можно проксировать.
const MAX_PROXIES: usize = 2;

/// Сколько байт головы сообщения сервер согласен прочитать.
///
/// Две тысячи при десятке нужных строк. Предел существует потому, что голова
/// приходит **до** того, как о запросе известно хоть что-нибудь: без него
/// клиент, шлющий заголовки без остановки, съел бы память сервера.
const HEAD_MAX: usize = 2048;

/// Размер буфера, которым тело переливается наружу.
///
/// Восемь килобайт — ровно столько же, сколько отправной буфер соединения в
/// ядре: больший кусок всё равно не уйдёт за один раз, а меньший означал бы
/// лишние системные вызовы на каждый мегабайт.
const OUT: usize = 8192;

/// Сколько ждать голову запроса от подключившегося.
const HEAD_TIMEOUT_MS: u64 = 15_000;

/// Сколько держать соединение открытым между запросами.
const IDLE_TIMEOUT_MS: u64 = 20_000;

/// Сколько ждать, пока тело сдвинется с места.
const STALL_TIMEOUT_MS: u64 = 30_000;

/// Сколько ждать того, кого мы проксируем.
const UPSTREAM_TIMEOUT_MS: u64 = 20_000;

/// Сколько спать, когда ни одно соединение не продвинулось.
///
/// Две миллисекунды: сервер, крутящий пустой цикл, отнимает процессор у того,
/// кто в это время читает диск, — а читает диск он же, для того же ответа.
const IDLE_MS: u64 = 2;

/// Строка на месте: путь, приставка, имя.
#[derive(Clone, Copy)]
struct Text<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    const fn new() -> Self {
        Self { bytes: [0; N], len: 0 }
    }

    /// Запомнить строку. `false` — не поместилась, и прежняя осталась прежней.
    fn set(&mut self, text: &str) -> bool {
        if text.len() > N {
            return false;
        }
        self.bytes[..text.len()].copy_from_slice(text.as_bytes());
        self.len = text.len();
        true
    }

    /// Запомнить столько, сколько поместится.
    ///
    /// Обрезается по границе знака, а не по байту: половина буквы в журнале —
    /// это строка, которую не прочитает ни человек, ни стенд.
    fn set_clipped(&mut self, text: &str) {
        let mut end = text.len().min(N);
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        self.bytes[..end].copy_from_slice(text[..end].as_bytes());
        self.len = end;
    }

    /// Дописать в конец. `false` — не поместилось.
    fn push(&mut self, text: &str) -> bool {
        if self.len + text.len() > N {
            return false;
        }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        true
    }

    /// Дописать десятичное число.
    fn push_num(&mut self, value: u64) -> bool {
        let mut digits = [0u8; 20];
        let mut index = digits.len();
        let mut rest = value;
        loop {
            index -= 1;
            digits[index] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        // SAFETY: в буфер записаны только цифры ASCII.
        self.push(unsafe { core::str::from_utf8_unchecked(&digits[index..]) })
    }

    fn as_str(&self) -> &str {
        // SAFETY: в буфер попадают только байты из `&str`, то есть UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.bytes[..self.len]) }
    }

    const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Каталог, подставленный в дерево сайта: `/files` ведёт в `/media`.
#[derive(Clone, Copy)]
struct Mount {
    prefix: Text<64>,
    dir: Text<MAX_PATH>,
}

/// Приставка, которую сервер не обслуживает сам, а спрашивает у другого.
#[derive(Clone, Copy)]
struct Proxy {
    prefix: Text<64>,
    /// Как собеседник назван в настройках — это же уезжает в его `Host:`.
    name: Text<64>,
    address: u32,
    port: u16,
}

/// Настройки сервера.
struct Settings {
    port: u16,
    root: Text<MAX_PATH>,
    mounts: [Mount; MAX_MOUNTS],
    mounts_len: usize,
    proxies: [Proxy; MAX_PROXIES],
    proxies_len: usize,
}

impl Settings {
    const fn new() -> Self {
        Self {
            port: DEFAULT_PORT,
            root: Text::new(),
            mounts: [const { Mount { prefix: Text::new(), dir: Text::new() } }; MAX_MOUNTS],
            mounts_len: 0,
            proxies: [const {
                Proxy { prefix: Text::new(), name: Text::new(), address: 0, port: 0 }
            }; MAX_PROXIES],
            proxies_len: 0,
        }
    }

    fn add_mount(&mut self, prefix: &str, dir: &str) -> bool {
        if self.mounts_len == MAX_MOUNTS {
            return false;
        }
        let mount = &mut self.mounts[self.mounts_len];
        if !mount.prefix.set(prefix) || !mount.dir.set(dir) {
            return false;
        }
        self.mounts_len += 1;
        true
    }

    fn add_proxy(&mut self, prefix: &str, host: &str, port: u16) -> bool {
        if self.proxies_len == MAX_PROXIES {
            return false;
        }
        // Имя разрешается один раз, при чтении настроек, и это осознанно:
        // спрашивать DNS на каждый запрос значит добавить к ответу чужой отказ,
        // а держать сервер незапущенным, пока не поднялась сеть, — честнее.
        let address = match user_progs::http::parse_ip(host) {
            Some(address) => address,
            None => match resolve(host) {
                Some(address) => address,
                None => return false,
            },
        };
        let proxy = &mut self.proxies[self.proxies_len];
        if !proxy.prefix.set(prefix) || !proxy.name.set(host) {
            return false;
        }
        proxy.address = address;
        proxy.port = port;
        self.proxies_len += 1;
        true
    }
}

/// Что сервер насчитал про себя.
#[derive(Clone, Copy)]
struct Stats {
    started_ms: u64,
    accepted: u64,
    requests: u64,
    answers: [u64; 4],
    sent: u64,
    proxied: u64,
    upstream_failed: u64,
}

impl Stats {
    const fn new() -> Self {
        Self {
            started_ms: 0,
            accepted: 0,
            requests: 0,
            answers: [0; 4],
            sent: 0,
            proxied: 0,
            upstream_failed: 0,
        }
    }

    /// Учесть ответ. Классы — те же, что у всех: 2xx, 3xx, 4xx, 5xx.
    fn answered(&mut self, status: u16) {
        self.requests += 1;
        let class = match status {
            200..=299 => 0,
            300..=399 => 1,
            400..=499 => 2,
            _ => 3,
        };
        self.answers[class] += 1;
    }
}

/// Чем занято соединение.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Слот свободен.
    Free,
    /// Читаем голову запроса.
    Reading,
    /// Ждём, пока установится связь с тем, кого проксируем.
    Connecting,
    /// Дописываем ему запрос.
    Asking,
    /// Читаем голову его ответа.
    Listening,
    /// Переливаем тело клиенту; когда переливать станет нечего, соединение
    /// либо вернётся к чтению следующего запроса, либо закроется.
    Sending,
}

/// Откуда берётся тело ответа.
#[derive(Clone, Copy)]
enum Source {
    /// Ниоткуда: всё, что надо отдать, уже лежит в буфере.
    Done,
    /// Из файла: столько-то байт, начиная с текущей позиции.
    File { fd: i64, left: u64 },
    /// От того, кого мы проксируем.
    Upstream { socket: i64, left: Left },
}

/// Сколько ещё тела ждать от проксируемого.
#[derive(Clone, Copy)]
enum Left {
    Bytes(u64),
    /// Пока он не закроет соединение. Так отвечает `HTTP/1.0`.
    UntilClose,
}

/// Одно соединение со всем, что при нём есть.
struct Conn {
    socket: i64,
    phase: Phase,
    /// Голова сообщения: сперва запрос клиента, потом — ответ проксируемого.
    head: [u8; HEAD_MAX],
    head_len: usize,
    /// То, что уходит наружу: `out[at..len]` ещё не отправлено.
    out: [u8; OUT],
    out_at: usize,
    out_len: usize,
    source: Source,
    keep_alive: bool,
    /// Метод запроса: у ответа на `HEAD` тела нет, что бы ни было объявлено.
    method: Method,
    /// Когда в этом соединении последний раз что-то происходило.
    touched: u64,
    /// Сколько байт тела отдано в текущем ответе.
    sent: u64,
    /// Запрос, который сейчас обслуживается, — для журнала.
    what: Text<96>,
    status: u16,
}

impl Conn {
    const fn new() -> Self {
        Self {
            socket: -1,
            phase: Phase::Free,
            head: [0; HEAD_MAX],
            head_len: 0,
            out: [0; OUT],
            out_at: 0,
            out_len: 0,
            source: Source::Done,
            keep_alive: false,
            method: Method::Get,
            touched: 0,
            sent: 0,
            what: Text::new(),
            status: 0,
        }
    }

    /// Сколько байт ещё не отправлено клиенту.
    const fn pending(&self) -> usize {
        self.out_len - self.out_at
    }

    /// Положить текст в исходящий буфер. `false` — не поместился.
    fn put(&mut self, text: &str) -> bool {
        let bytes = text.as_bytes();
        if self.out_len + bytes.len() > OUT {
            return false;
        }
        self.out[self.out_len..self.out_len + bytes.len()].copy_from_slice(bytes);
        self.out_len += bytes.len();
        true
    }

    /// Положить десятичное число.
    fn put_num(&mut self, value: u64) -> bool {
        let mut digits = [0u8; 20];
        let mut index = digits.len();
        let mut rest = value;
        loop {
            index -= 1;
            digits[index] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        // SAFETY: в буфер записаны только цифры ASCII.
        self.put(unsafe { core::str::from_utf8_unchecked(&digits[index..]) })
    }

    /// Освободить слот: закрыть всё, что в нём открыто.
    fn release(&mut self) {
        if let Source::File { fd, .. } = self.source {
            close(fd);
        }
        if let Source::Upstream { socket, .. } = self.source {
            close_socket(socket);
        }
        if self.socket >= 0 {
            close_socket(self.socket);
        }
        self.socket = -1;
        self.phase = Phase::Free;
        self.source = Source::Done;
        self.head_len = 0;
        self.out_at = 0;
        self.out_len = 0;
        self.sent = 0;
        self.status = 0;
        self.what.len = 0;
    }

    /// Приготовиться к следующему запросу в том же соединении.
    fn recycle(&mut self, extra: usize) {
        if let Source::File { fd, .. } = self.source {
            close(fd);
        }
        if let Source::Upstream { socket, .. } = self.source {
            close_socket(socket);
        }
        self.source = Source::Done;
        // Хвост, приехавший вместе с предыдущим запросом, — это уже начало
        // следующего: потерять его значит ждать байты, которые уже пришли.
        self.head.copy_within(self.head_len - extra..self.head_len, 0);
        self.head_len = extra;
        self.out_at = 0;
        self.out_len = 0;
        self.sent = 0;
        self.status = 0;
        self.what.len = 0;
        self.phase = Phase::Reading;
        self.touched = uptime_ms();
    }
}

/// Соединения живут в статике: четыре по восемь килобайт буфера — это сорок
/// килобайт, а стека у программы шестьдесят четыре.
static mut CONNS: [Conn; MAX_CLIENTS] = [const { Conn::new() }; MAX_CLIENTS];

/// Текст настроек читается сюда же, в статику, и по той же причине.
static mut CONFIG_TEXT: [u8; 4096] = [0; 4096];

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно такими, какими их положило ядро.
    let args = unsafe { Args::new(argc, argv) };
    if args.get(1).is_some_and(|first| first == "--help" || first == "-h") {
        usage();
        exit(0);
    }

    let mut settings = Settings::new();
    if !settings.root.set(DEFAULT_ROOT) {
        error("httpd: the default root does not fit in a path\n");
        exit(1);
    }
    read_config(&mut settings);
    if !read_args(&args, &mut settings) {
        exit(1);
    }

    let server = stream();
    if server < 0 {
        error("httpd: cannot open a socket\n");
        exit(1);
    }
    if bind(server, settings.port) < 0 {
        let mut line = Line::to_log();
        line.str("httpd: port ").num(u64::from(settings.port)).str(" is taken").end();
        exit(1);
    }
    if listen(server) < 0 {
        error("httpd: cannot listen\n");
        exit(1);
    }

    let mut line = Line::to_log();
    line.str("httpd: listening on port ")
        .num(u64::from(settings.port))
        .str(", root ")
        .str(settings.root.as_str())
        .end();
    for index in 0..settings.mounts_len {
        let mount = &settings.mounts[index];
        let mut line = Line::to_log();
        line.str("httpd: mount ")
            .str(mount.prefix.as_str())
            .str(" -> ")
            .str(mount.dir.as_str())
            .end();
    }
    for index in 0..settings.proxies_len {
        let proxy = &settings.proxies[index];
        let mut line = Line::to_log();
        line.str("httpd: proxy ")
            .str(proxy.prefix.as_str())
            .str(" -> ")
            .str(proxy.name.as_str())
            .str(":")
            .num(u64::from(proxy.port))
            .end();
    }

    serve(server, &settings)
}

fn usage() {
    error("usage: httpd [--port N] [--root DIR] [--mount PREFIX DIR] [--proxy PREFIX HOST PORT]\n");
    error("       settings not given on the command line are read from /etc/httpd.cfg\n");
}

/// Прочитать `/etc/httpd.cfg`, если он есть.
///
/// Отсутствие файла — не ошибка: у сервера есть умолчания, и запуск без
/// настроек обязан работать. А вот строка, которую не удалось понять, называется
/// вслух: молча пропущенная настройка выглядит как «сервер меня не слушает».
fn read_config(settings: &mut Settings) {
    let Some(path) = config_path(CONFIG) else {
        return;
    };
    let fd = open(path.as_str());
    if fd < 0 {
        return;
    }
    // SAFETY: буфер в статике, ссылка на него берётся один раз и здесь.
    let buffer = unsafe { &mut *core::ptr::addr_of_mut!(CONFIG_TEXT) };
    let got = read(fd, buffer);
    close(fd);
    if got <= 0 {
        return;
    }
    let Ok(text) = core::str::from_utf8(&buffer[..got as usize]) else {
        error("httpd: the settings file is not text; ignoring it\n");
        return;
    };

    let mut line = Line::to_log();
    line.str("httpd: settings from ").str(path.as_str()).end();

    for row in text.lines() {
        let row = row.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let Some((key, value)) = row.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let ok = match key {
            "port" => match value.parse::<u16>() {
                Ok(port) if port != 0 => {
                    settings.port = port;
                    true
                }
                _ => false,
            },
            "root" => settings.root.set(value),
            // `mount=/files /media` — приставка и каталог через пробел.
            "mount" => match value.split_once(' ') {
                Some((prefix, dir)) => settings.add_mount(prefix.trim(), dir.trim()),
                None => false,
            },
            // `proxy=/up 10.0.2.2 2002` — приставка, собеседник и его порт.
            "proxy" => parse_proxy(value, settings),
            _ => {
                let mut line = Line::to_log();
                line.str("httpd: unknown setting '").str(key).str("', ignored").end();
                continue;
            }
        };
        if !ok {
            let mut line = Line::to_log();
            line.str("httpd: cannot use setting '").str(row).str("'").end();
        }
    }
}

/// `<приставка> <хост> <порт>` из настроек.
fn parse_proxy(value: &str, settings: &mut Settings) -> bool {
    let mut parts = value.split_whitespace();
    let (Some(prefix), Some(host), Some(port)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let Ok(port) = port.parse::<u16>() else {
        return false;
    };
    settings.add_proxy(prefix, host, port)
}

/// Разобрать командную строку. Она сильнее файла настроек.
fn read_args(args: &Args, settings: &mut Settings) -> bool {
    let mut index = 1usize;
    while index < args.len() {
        let Some(key) = args.get(index) else {
            return fail_arg("an argument is not text");
        };
        match key {
            "--port" => {
                let Some(value) = args.get(index + 1).and_then(|text| text.parse::<u16>().ok())
                else {
                    return fail_arg("--port wants a number");
                };
                settings.port = value;
                index += 2;
            }
            "--root" => {
                let Some(value) = args.get(index + 1) else {
                    return fail_arg("--root wants a directory");
                };
                if !settings.root.set(value) {
                    return fail_arg("that root does not fit in a path");
                }
                index += 2;
            }
            "--mount" => {
                let (Some(prefix), Some(dir)) = (args.get(index + 1), args.get(index + 2)) else {
                    return fail_arg("--mount wants a prefix and a directory");
                };
                if !settings.add_mount(prefix, dir) {
                    return fail_arg("that mount does not fit");
                }
                index += 3;
            }
            "--proxy" => {
                let (Some(prefix), Some(host), Some(port)) =
                    (args.get(index + 1), args.get(index + 2), args.get(index + 3))
                else {
                    return fail_arg("--proxy wants a prefix, a host and a port");
                };
                let Ok(port) = port.parse::<u16>() else {
                    return fail_arg("--proxy wants a number for the port");
                };
                if !settings.add_proxy(prefix, host, port) {
                    return fail_arg("that upstream is neither an address nor a name we can resolve");
                }
                index += 4;
            }
            other => {
                let mut line = Line::to_log();
                line.str("httpd: what is '").str(other).str("'?").end();
                usage();
                return false;
            }
        }
    }
    true
}

fn fail_arg(why: &str) -> bool {
    let mut line = Line::to_log();
    line.str("httpd: ").str(why).end();
    usage();
    false
}

/// Главный круг: принимать, продвигать, повторять.
fn serve(server: i64, settings: &Settings) -> ! {
    let mut stats = Stats::new();
    stats.started_ms = uptime_ms();
    // SAFETY: соединения живут в статике, и ссылка на них берётся один раз —
    // здесь. Эта функция не возвращается, поэтому второй такой ссылки нет.
    let conns = unsafe { &mut *core::ptr::addr_of_mut!(CONNS) };

    loop {
        let mut moved = false;

        // Принять нового, если есть куда. Если некуда — не принимать вовсе:
        // принятое и тут же закрытое соединение выглядит у клиента как
        // «сервер сбросил связь», а очередь ядра подержит его честно.
        if let Some(slot) = conns.iter().position(|conn| conn.phase == Phase::Free) {
            let client = accept(server);
            if client >= 0 {
                let conn = &mut conns[slot];
                conn.socket = client;
                conn.phase = Phase::Reading;
                conn.head_len = 0;
                conn.touched = uptime_ms();
                conn.keep_alive = false;
                stats.accepted += 1;
                moved = true;
            } else if client != ERR_AGAIN {
                let mut line = Line::to_log();
                line.str("httpd: accept failed with code ").signed(client).end();
                sleep_ms(100);
            }
        }

        for conn in conns.iter_mut() {
            if conn.phase == Phase::Free {
                continue;
            }
            moved |= step(conn, settings, &mut stats);
        }

        if !moved {
            sleep_ms(IDLE_MS);
        }
    }
}

/// Продвинуть одно соединение. Возвращает `true`, если что-то произошло.
fn step(conn: &mut Conn, settings: &Settings, stats: &mut Stats) -> bool {
    // Оборванное соединение — не ошибка сервера и не повод для жалобы: клиент
    // вправе уйти посреди ответа, и часто именно так и делает.
    if let Some(state) = stream_state(conn.socket) {
        if state.reset != 0 {
            if conn.sent > 0 {
                let mut line = Line::to_log();
                line.str("httpd: the client went away after ").num(conn.sent).str(" bytes").end();
            }
            conn.release();
            return true;
        }
    }

    match conn.phase {
        Phase::Free => false,
        Phase::Reading => read_request(conn, settings, stats),
        Phase::Connecting => wait_upstream(conn, stats),
        Phase::Asking => ask_upstream(conn, stats),
        Phase::Listening => hear_upstream(conn, stats),
        Phase::Sending => pump(conn, stats),
    }
}

/// Читать голову запроса, пока она не придёт целиком.
fn read_request(conn: &mut Conn, settings: &Settings, stats: &mut Stats) -> bool {
    if conn.head_len < HEAD_MAX {
        let got = recv(conn.socket, &mut conn.head[conn.head_len..]);
        if got > 0 {
            conn.head_len += got as usize;
            conn.touched = uptime_ms();
        } else if let Some(state) = stream_state(conn.socket) {
            // Клиент сказал всё и закрылся, не дослав запрос. Если он не
            // прислал ничего — это обычное закрытие живого соединения, о
            // котором нечего сообщать.
            if state.peer_closed != 0 && http::head_end(&conn.head[..conn.head_len]).is_none() {
                conn.release();
                return true;
            }
        }
    }

    if let Some(end) = http::head_end(&conn.head[..conn.head_len]) {
        return answer(conn, end, settings, stats);
    }

    if conn.head_len == HEAD_MAX {
        // Голова не помещается — и это тот случай, когда ответить надо, а
        // читать дальше нельзя: следующий байт всё равно некуда положить.
        conn.keep_alive = false;
        return refuse(conn, 431, "Request Header Fields Too Large", stats);
    }

    let waiting = uptime_ms().saturating_sub(conn.touched);
    let limit = if conn.head_len == 0 { IDLE_TIMEOUT_MS } else { HEAD_TIMEOUT_MS };
    if waiting > limit {
        // Молчащее соединение занимает слот, который нужен следующему. Тому,
        // кто не сказал ни слова, отвечать нечем — его просто отпускают.
        if conn.head_len == 0 {
            conn.release();
            return true;
        }
        conn.keep_alive = false;
        return refuse(conn, 408, "Request Timeout", stats);
    }
    false
}

/// Ответить на пришедший запрос.
fn answer(conn: &mut Conn, end: usize, settings: &Settings, stats: &mut Stats) -> bool {
    // Всё, что понадобится после того, как буфер головы будет занят другим,
    // копируется сюда сразу: разобранный запрос ссылается на тот же буфер.
    let mut path_room = [0u8; MAX_PATH];
    let mut clean = Text::<MAX_PATH>::new();
    let mut query = Text::<128>::new();
    let mut asked = Text::<96>::new();

    let route = {
        let request = match Request::parse(&conn.head[..end]) {
            Ok(request) => request,
            Err(_) => {
                // Где кончился такой запрос, неизвестно: разобрать его не
                // удалось. Значит неизвестно и где начнётся следующий.
                conn.keep_alive = false;
                return refuse(conn, 400, "Bad Request", stats);
            }
        };
        conn.method = request.method;
        conn.keep_alive = request.keep_alive();
        // Первая строка запроса уезжает в журнал: по ней видно и что просили,
        // и как просили.
        asked.set_clipped(request.head.first_line());

        let (raw_path, raw_query) = request.split_target();
        let framing = request.body();
        if !matches!(framing, Ok(Body::None)) {
            // Тела мы не читаем, а значит и не можем пропустить его мимо, чтобы
            // добраться до следующего запроса в том же соединении. Поэтому
            // после любого такого ответа соединение закрывается.
            conn.keep_alive = false;
        }
        if framing.is_err() {
            // Запрос сам себе противоречит: две длины тела или длина вместе с
            // кусками. Это `400`, а не `405`: дело не в методе, а в том, что
            // где кончается такой запрос, не знает никто — ни мы, ни тот, кому
            // мы передали бы его дальше.
            Route::Refuse(400, "Bad Request")
        } else if matches!(request.method, Method::Other) {
            Route::Refuse(405, "Method Not Allowed")
        } else if !matches!(framing, Ok(Body::None)) {
            Route::Refuse(405, "Method Not Allowed")
        } else if let Some(text) = http::path::normalize(raw_path, &mut path_room) {
            if clean.set(text) && query.set(raw_query) {
                Route::Serve
            } else {
                Route::Refuse(414, "URI Too Long")
            }
        } else {
            // Путь не разобрался: `%` без цифр, управляющий знак или попытка
            // выйти за корень. Ответ на все три один — подробность здесь была
            // бы подсказкой тому, кто перебирает.
            Route::Refuse(400, "Bad Request")
        }
    };

    // Голова разобрана; хвост за ней — это уже следующий запрос.
    let extra = conn.head_len - end;
    conn.recycle(extra);
    conn.what = asked;

    match route {
        Route::Refuse(status, reason) => refuse(conn, status, reason, stats),
        Route::Serve => {
            let path = clean.as_str();
            if path == METRICS {
                return metrics(conn, stats);
            }
            for index in 0..settings.proxies_len {
                let proxy = &settings.proxies[index];
                if let Some(rest) = http::path::under(path, proxy.prefix.as_str()) {
                    return proxy_to(conn, proxy, rest, query.as_str(), stats);
                }
            }
            for index in 0..settings.mounts_len {
                let mount = &settings.mounts[index];
                if let Some(rest) = http::path::under(path, mount.prefix.as_str()) {
                    return from_disk(conn, mount.dir.as_str(), rest, stats);
                }
            }
            from_disk(conn, settings.root.as_str(), path, stats)
        }
    }
}

/// Куда отправился запрос после разбора.
#[derive(Clone, Copy)]
enum Route {
    Serve,
    Refuse(u16, &'static str),
}

/// Отдать файл из каталога.
fn from_disk(conn: &mut Conn, dir: &str, rest: &str, stats: &mut Stats) -> bool {
    let Some(mut path) = Path::from(dir) else {
        return refuse(conn, 500, "Internal Server Error", stats);
    };
    // К корню дописывается **только** то, что вернул разбор пути: он уже
    // развернул `%XX` и убрал `..`, и никакая другая строка сюда не попадает.
    if rest != "/" && !path.push(rest) {
        return refuse(conn, 414, "URI Too Long", stats);
    }

    let mut info = Stat::default();
    if stat(path.as_str(), &mut info) != 0 {
        return refuse(conn, 404, "Not Found", stats);
    }
    if info.kind == KIND_DIRECTORY {
        // Каталог отдаётся своим `index.html`, а списка файлов в нём наружу не
        // показывают: имена того, что лежит на диске, — это уже сведения о
        // машине, и раздавать их по умолчанию не следует.
        if !path.join(INDEX) {
            return refuse(conn, 414, "URI Too Long", stats);
        }
        if stat(path.as_str(), &mut info) != 0 || info.kind != KIND_FILE {
            return refuse(conn, 404, "Not Found", stats);
        }
    } else if info.kind != KIND_FILE {
        return refuse(conn, 404, "Not Found", stats);
    }

    let fd = open(path.as_str());
    if fd < 0 {
        // Файл есть, а открыть нельзя — это про права, а не про отсутствие.
        return refuse(conn, 403, "Forbidden", stats);
    }
    let length = info.size;

    conn.status = 200;
    conn.out_at = 0;
    conn.out_len = 0;
    let ok = conn.put("HTTP/1.1 200 OK\r\nContent-Length: ")
        && conn.put_num(length)
        && conn.put("\r\nContent-Type: ")
        && conn.put(http::mime::of(path.as_str()))
        && conn.put("\r\nConnection: ")
        && conn.put(if conn.keep_alive { "keep-alive" } else { "close" })
        && conn.put("\r\nServer: FreeOS-httpd/1\r\n\r\n");
    if !ok {
        close(fd);
        return refuse(conn, 500, "Internal Server Error", stats);
    }

    if matches!(conn.method, Method::Head) {
        close(fd);
        conn.source = Source::Done;
    } else {
        conn.source = Source::File { fd, left: length };
    }
    conn.phase = Phase::Sending;
    conn.touched = uptime_ms();
    stats.answered(200);
    said(conn, length);
    true
}

/// Рассказать о себе числами — в том виде, в каком это читает Prometheus.
///
/// Формат чужой и описан коротко: строка `# TYPE` про имя, следом имя и число.
/// Метки (`{class="2xx"}`) здесь не нужны: столбцов у нас девять, и разложить
/// их по именам честнее, чем заводить разбор меток ради трёх значений.
fn metrics(conn: &mut Conn, stats: &mut Stats) -> bool {
    conn.status = 200;
    conn.out_at = 0;
    conn.out_len = 0;

    // Тело собирается целиком до головы, и иначе нельзя: в голове стоит его
    // длина, а узнать её, не собрав тело, неоткуда.
    let mut body = Text::<{ OUT / 2 }>::new();
    let uptime = uptime_ms().saturating_sub(stats.started_ms) / 1000;
    let rows: [(&str, &str, u64); 9] = [
        ("freeos_httpd_connections_accepted_total", "counter", stats.accepted),
        ("freeos_httpd_upstream_failed_total", "counter", stats.upstream_failed),
        ("freeos_httpd_requests_total", "counter", stats.requests),
        ("freeos_httpd_responses_2xx_total", "counter", stats.answers[0]),
        ("freeos_httpd_responses_4xx_total", "counter", stats.answers[2]),
        ("freeos_httpd_responses_5xx_total", "counter", stats.answers[3]),
        ("freeos_httpd_sent_bytes_total", "counter", stats.sent),
        ("freeos_httpd_proxied_total", "counter", stats.proxied),
        ("freeos_httpd_uptime_seconds", "gauge", uptime),
    ];
    let mut ok = true;
    for (name, kind, value) in rows {
        // Перевод строки в теле — одинокий `\n`, и это не оплошность: так
        // устроен формат Prometheus, и так его читают все, кто его читает.
        // В **голове** сообщения переводы строк другие — `\r\n`, как велит HTTP.
        ok = ok
            && body.push("# TYPE ")
            && body.push(name)
            && body.push(" ")
            && body.push(kind)
            && body.push("\n")
            && body.push(name)
            && body.push(" ")
            && body.push_num(value)
            && body.push("\n");
    }

    ok = ok
        && conn.put("HTTP/1.1 200 OK\r\nContent-Length: ")
        && conn.put_num(body.len as u64)
        && conn.put("\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nConnection: ")
        && conn.put(if conn.keep_alive { "keep-alive" } else { "close" })
        && conn.put("\r\nServer: FreeOS-httpd/1\r\n\r\n")
        && (matches!(conn.method, Method::Head) || conn.put(body.as_str()));
    if !ok {
        conn.keep_alive = false;
        return refuse(conn, 500, "Internal Server Error", stats);
    }
    conn.source = Source::Done;
    conn.phase = Phase::Sending;
    conn.touched = uptime_ms();
    stats.answered(200);
    said(conn, body.len as u64);
    true
}

/// Короткий ответ без тела из файла: код, объяснение и страница о нём же.
fn refuse(conn: &mut Conn, status: u16, reason: &'static str, stats: &mut Stats) -> bool {
    conn.status = status;
    conn.out_at = 0;
    conn.out_len = 0;
    conn.source = Source::Done;
    // Жив ли после этого ответа разговор, решает не код отказа, а то, знаем ли
    // мы, где кончился запрос. Там, где не знаем (`400`, `408`, `431`),
    // вызывающий уже поставил `keep_alive = false`: следующий запрос в таком
    // соединении начался бы неизвестно откуда.

    // Тело — тот же текст, что и в строке состояния: показать человеку, что
    // ответил сервер, а не браузер.
    let body_len = reason.len() + 1;
    let ok = conn.put("HTTP/1.1 ")
        && conn.put_num(u64::from(status))
        && conn.put(" ")
        && conn.put(reason)
        && conn.put("\r\nContent-Length: ")
        && conn.put_num(body_len as u64)
        && conn.put("\r\nContent-Type: text/plain; charset=utf-8\r\nConnection: close\r\nServer: FreeOS-httpd/1\r\n\r\n")
        && (matches!(conn.method, Method::Head) || (conn.put(reason) && conn.put("\n")));
    if !ok {
        // Даже отказ не поместился — сказать нечего, кроме как закрыться.
        conn.release();
        return true;
    }
    conn.phase = Phase::Sending;
    conn.touched = uptime_ms();
    stats.answered(status);
    said(conn, body_len as u64);
    true
}

/// Строка в журнал о том, что и чем ответили.
fn said(conn: &Conn, length: u64) {
    let mut line = Line::to_log();
    line.str("httpd: ")
        .str(if conn.what.is_empty() { "(no request line)" } else { conn.what.as_str() })
        .str(" -> ")
        .num(u64::from(conn.status))
        .str(", ")
        .num(length)
        .str(if matches!(conn.method, Method::Head) { " bytes declared" } else { " bytes" })
        .end();
}

/// Начать разговор с тем, кого мы проксируем.
fn proxy_to(
    conn: &mut Conn,
    proxy: &Proxy,
    rest: &str,
    query: &str,
    stats: &mut Stats,
) -> bool {
    let socket = stream();
    if socket < 0 {
        stats.upstream_failed += 1;
        return refuse(conn, 503, "Service Unavailable", stats);
    }
    if connect(socket, proxy.address, proxy.port) < 0 {
        close_socket(socket);
        stats.upstream_failed += 1;
        return refuse(conn, 502, "Bad Gateway", stats);
    }

    // Запрос собирается сразу, пока известны и путь, и приставка; уйдёт он,
    // когда связь установится.
    conn.out_at = 0;
    conn.out_len = 0;
    let method = if matches!(conn.method, Method::Head) { "HEAD " } else { "GET " };
    let mut ok = conn.put(method) && conn.put(rest);
    if ok && !query.is_empty() {
        ok = conn.put("?") && conn.put(query);
    }
    ok = ok && conn.put(" HTTP/1.1\r\nHost: ") && conn.put(proxy.name.as_str());
    // Собеседнику говорим `close`: держать с ним живое соединение означало бы
    // вести учёт чужих соединений, а их у нас столько же, сколько своих.
    ok = ok && conn.put("\r\nConnection: close\r\nAccept: */*\r\nUser-Agent: FreeOS-httpd/1\r\n");
    // Кто на самом деле спрашивает. Поле не выдумано нами — так его пишут все
    // прокси, и по нему тот, кого мы проксируем, видит настоящего клиента, а
    // не нас.
    if let Some(client) = peer(conn.socket) {
        ok = ok && conn.put("X-Forwarded-For: ") && put_ip(conn, client.address) && conn.put("\r\n");
    }
    ok = ok && conn.put("\r\n");
    if !ok {
        close_socket(socket);
        stats.upstream_failed += 1;
        return refuse(conn, 500, "Internal Server Error", stats);
    }

    conn.source = Source::Upstream { socket, left: Left::UntilClose };
    conn.phase = Phase::Connecting;
    conn.touched = uptime_ms();
    stats.proxied += 1;
    true
}

/// Записать адрес IPv4 в исходящий буфер.
fn put_ip(conn: &mut Conn, address: u32) -> bool {
    let bytes = address.to_be_bytes();
    let mut ok = true;
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            ok = ok && conn.put(".");
        }
        ok = ok && conn.put_num(u64::from(*byte));
    }
    ok
}

/// Дождаться, пока установится связь с проксируемым.
fn wait_upstream(conn: &mut Conn, stats: &mut Stats) -> bool {
    let Source::Upstream { socket, .. } = conn.source else {
        conn.release();
        return true;
    };
    match stream_state(socket) {
        Some(state) if state.open != 0 => {
            conn.phase = Phase::Asking;
            conn.touched = uptime_ms();
            true
        }
        Some(state) if state.reset != 0 => upstream_gone(conn, stats),
        _ => {
            if uptime_ms().saturating_sub(conn.touched) > UPSTREAM_TIMEOUT_MS {
                return upstream_gone(conn, stats);
            }
            false
        }
    }
}

/// Дописать запрос проксируемому.
fn ask_upstream(conn: &mut Conn, stats: &mut Stats) -> bool {
    let Source::Upstream { socket, .. } = conn.source else {
        conn.release();
        return true;
    };
    if conn.pending() == 0 {
        conn.out_at = 0;
        conn.out_len = 0;
        conn.head_len = 0;
        conn.phase = Phase::Listening;
        conn.touched = uptime_ms();
        return true;
    }
    let sent = send(socket, &conn.out[conn.out_at..conn.out_len]);
    if sent > 0 {
        conn.out_at += sent as usize;
        conn.touched = uptime_ms();
        return true;
    }
    if sent < 0 && sent != ERR_AGAIN {
        return upstream_gone(conn, stats);
    }
    if uptime_ms().saturating_sub(conn.touched) > UPSTREAM_TIMEOUT_MS {
        return upstream_gone(conn, stats);
    }
    false
}

/// Прочитать голову ответа проксируемого и передать её клиенту.
fn hear_upstream(conn: &mut Conn, stats: &mut Stats) -> bool {
    let Source::Upstream { socket, .. } = conn.source else {
        conn.release();
        return true;
    };

    if conn.head_len < HEAD_MAX {
        let got = recv(socket, &mut conn.head[conn.head_len..]);
        if got > 0 {
            conn.head_len += got as usize;
            conn.touched = uptime_ms();
        }
    }

    let Some(end) = http::head_end(&conn.head[..conn.head_len]) else {
        if conn.head_len == HEAD_MAX {
            return upstream_bad(conn, stats);
        }
        if let Some(state) = stream_state(socket) {
            if state.reset != 0 || (state.peer_closed != 0 && conn.head_len == 0) {
                return upstream_gone(conn, stats);
            }
        }
        if uptime_ms().saturating_sub(conn.touched) > UPSTREAM_TIMEOUT_MS {
            return upstream_gone(conn, stats);
        }
        return false;
    };

    // Ответ разбирается **нами**, а не передаётся как есть: прокси, который
    // пересылает голову не читая, передаёт дальше и чужое понимание границ
    // сообщения — то самое, из-за которого между нами и клиентом появляется
    // запрос, которого никто не посылал.
    let mut status = 0u16;
    let mut framing = Left::UntilClose;
    let mut fail = false;
    let mut kind = Text::<96>::new();
    {
        match Response::parse(&conn.head[..end]) {
            Ok(answer) => {
                status = answer.status;
                match answer.body(conn.method) {
                    Ok(Body::Length(bytes)) => framing = Left::Bytes(bytes),
                    Ok(Body::None) => framing = Left::Bytes(0),
                    Ok(Body::ToClose) => framing = Left::UntilClose,
                    // Куски и противоречивая длина — это отказ, а не догадка.
                    Ok(Body::Chunked) | Err(_) => fail = true,
                }
                if let Some(text) = answer.head.value("content-type") {
                    let _ = kind.set(text);
                }
            }
            Err(_) => fail = true,
        }
    }
    if fail {
        return upstream_bad(conn, stats);
    }

    let body_start = end;
    let extra = conn.head_len - body_start;
    // Хвост, приехавший вместе с головой, — это уже тело: потерять его значит
    // отдать клиенту файл, который короче ровно на первый сегмент.
    let mut tail = [0u8; HEAD_MAX];
    tail[..extra].copy_from_slice(&conn.head[body_start..conn.head_len]);
    // И только теперь буфер головы можно опустошить. Он больше не наш: в нём
    // лежал ответ собеседника, и оставить его значит дать `finish()` принять
    // этот ответ за начало следующего запроса клиента — живое соединение после
    // проксирования отвечало бы `400` на совершенно исправный запрос.
    //
    // Цена названа вслух: запрос, присланный клиентом **вперёд**, пока мы
    // ходили к собеседнику, теряется. Такого клиента у нас нет — запросы шлют
    // по одному, дождавшись ответа, — а конвейер (pipelining) этот сервер и не
    // обещает.
    conn.head_len = 0;

    conn.status = status;
    conn.out_at = 0;
    conn.out_len = 0;
    let mut ok = conn.put("HTTP/1.1 ") && conn.put_num(u64::from(status)) && conn.put(" ");
    ok = ok && conn.put(reason_for(status));
    match framing {
        Left::Bytes(bytes) => {
            ok = ok && conn.put("\r\nContent-Length: ") && conn.put_num(bytes);
        }
        Left::UntilClose => {
            // Длины нет и у нас: раз собеседник кончает тело закрытием, то же
            // придётся сделать и нам, а живым такое соединение не остаётся.
            conn.keep_alive = false;
        }
    }
    if !kind.is_empty() {
        ok = ok && conn.put("\r\nContent-Type: ") && conn.put(kind.as_str());
    }
    ok = ok
        && conn.put("\r\nConnection: ")
        && conn.put(if conn.keep_alive { "keep-alive" } else { "close" })
        && conn.put("\r\nServer: FreeOS-httpd/1\r\n\r\n");
    if ok && extra > 0 && !matches!(conn.method, Method::Head) {
        ok = conn.out_len + extra <= OUT;
        if ok {
            conn.out[conn.out_len..conn.out_len + extra].copy_from_slice(&tail[..extra]);
            conn.out_len += extra;
        }
    }
    if !ok {
        return upstream_bad(conn, stats);
    }

    let left = match framing {
        Left::Bytes(bytes) => Left::Bytes(bytes.saturating_sub(extra as u64)),
        Left::UntilClose => Left::UntilClose,
    };
    conn.source = Source::Upstream { socket, left };
    conn.phase = Phase::Sending;
    conn.touched = uptime_ms();
    stats.answered(status);
    let mut line = Line::to_log();
    line.str("httpd: ")
        .str(conn.what.as_str())
        .str(" -> upstream ")
        .num(u64::from(status))
        .end();
    true
}

/// Собеседник не отозвался.
fn upstream_gone(conn: &mut Conn, stats: &mut Stats) -> bool {
    if let Source::Upstream { socket, .. } = conn.source {
        close_socket(socket);
    }
    conn.source = Source::Done;
    stats.upstream_failed += 1;
    refuse(conn, 502, "Bad Gateway", stats)
}

/// Собеседник ответил тем, чего мы не понимаем.
fn upstream_bad(conn: &mut Conn, stats: &mut Stats) -> bool {
    if let Source::Upstream { socket, .. } = conn.source {
        close_socket(socket);
    }
    conn.source = Source::Done;
    stats.upstream_failed += 1;
    refuse(conn, 502, "Bad Gateway", stats)
}

/// Перелить в клиента то, что для него есть.
fn pump(conn: &mut Conn, stats: &mut Stats) -> bool {
    // Сперва — то, что уже собрано: отправка принимает не всё сразу, и это
    // нормальный ход дел, а не отказ.
    if conn.pending() > 0 {
        let sent = send(conn.socket, &conn.out[conn.out_at..conn.out_len]);
        if sent > 0 {
            conn.out_at += sent as usize;
            conn.sent += sent as u64;
            stats.sent += sent as u64;
            conn.touched = uptime_ms();
            return true;
        }
        if sent < 0 && sent != ERR_AGAIN {
            conn.release();
            return true;
        }
        if uptime_ms().saturating_sub(conn.touched) > STALL_TIMEOUT_MS {
            let mut line = Line::to_log();
            line.str("httpd: the client stopped taking the answer after ")
                .num(conn.sent)
                .str(" bytes")
                .end();
            conn.release();
            return true;
        }
        return false;
    }

    conn.out_at = 0;
    conn.out_len = 0;

    match conn.source {
        Source::Done => finish(conn),
        Source::File { fd, left } => {
            if left == 0 {
                close(fd);
                conn.source = Source::Done;
                return finish(conn);
            }
            let want = (left.min(OUT as u64)) as usize;
            let got = read(fd, &mut conn.out[..want]);
            if got <= 0 {
                // Файл кончился раньше, чем обещал его размер. Дописать
                // нечего, а объявленную длину уже видел клиент — соединение
                // придётся оборвать, и сказать об этом честно.
                let mut line = Line::to_log();
                line.str("httpd: the file ended ").num(left).str(" bytes early").end();
                close(fd);
                conn.source = Source::Done;
                conn.release();
                return true;
            }
            conn.out_len = got as usize;
            conn.source = Source::File { fd, left: left - got as u64 };
            conn.touched = uptime_ms();
            true
        }
        Source::Upstream { socket, left } => {
            let room = match left {
                Left::Bytes(0) => {
                    close_socket(socket);
                    conn.source = Source::Done;
                    return finish(conn);
                }
                Left::Bytes(bytes) => (bytes.min(OUT as u64)) as usize,
                Left::UntilClose => OUT,
            };
            let got = recv(socket, &mut conn.out[..room]);
            if got > 0 {
                conn.out_len = got as usize;
                conn.source = Source::Upstream {
                    socket,
                    left: match left {
                        Left::Bytes(bytes) => Left::Bytes(bytes - got as u64),
                        Left::UntilClose => Left::UntilClose,
                    },
                };
                conn.touched = uptime_ms();
                return true;
            }
            if let Some(state) = stream_state(socket) {
                if state.peer_closed != 0 || state.reset != 0 {
                    // Для `UntilClose` закрытие и есть конец тела; для
                    // объявленной длины — обрыв, но сказать об этом клиенту
                    // уже нечем: голова ушла.
                    if let Left::Bytes(bytes) = left {
                        if bytes > 0 {
                            let mut line = Line::to_log();
                            line.str("httpd: the upstream stopped ")
                                .num(bytes)
                                .str(" bytes early")
                                .end();
                        }
                    }
                    close_socket(socket);
                    conn.source = Source::Done;
                    return finish(conn);
                }
            }
            if uptime_ms().saturating_sub(conn.touched) > UPSTREAM_TIMEOUT_MS {
                close_socket(socket);
                conn.source = Source::Done;
                let mut line = Line::to_log();
                line.str("httpd: the upstream went quiet mid-body").end();
                return finish(conn);
            }
            false
        }
    }
}

/// Ответ отдан целиком.
fn finish(conn: &mut Conn) -> bool {
    if conn.keep_alive {
        conn.recycle(conn.head_len);
        return true;
    }
    // Своя половина закрывается первой: `FIN` от нас — это то, по чему клиент
    // понимает, что ответ кончился, если длина ему неизвестна.
    shutdown(conn.socket);
    conn.release();
    true
}

/// Пояснение к коду — то, что стоит в строке состояния после числа.
const fn reason_for(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        414 => "URI Too Long",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Answer",
    }
}
