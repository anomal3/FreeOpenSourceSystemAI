//! Проверка сокетов-дескрипторов (фаза 65a): сеть через `read`, `write`,
//! `poll` и `close`, как у программы на C.
//!
//! `sockfd <адрес> <порт>` — клиент к эхо-серверу: блокирующее соединение,
//! запись, ожидание `poll`, чтение, конец потока после `shutdown`; затем
//! неблокирующее соединение с ожиданием `poll` на запись и вид дескриптора в
//! `fstat`. `sockfd serve <порт>` — сервер: блокирующий `accept` и эхо одного
//! клиента до конца его потока.
//!
//! Каждая проверка печатает строку, по ним сверяет сценарий `sockfd`.

#![no_std]
#![no_main]

use user_abi::{
    ERR_IN_PROGRESS, KIND_SOCKET, POLL_IN, POLL_OUT, PollFd, SOCKCTL_ACCEPT, SOCKCTL_BIND,
    SOCKCTL_CONNECT, SOCKCTL_LISTEN, SOCKCTL_SHUTDOWN, SockInfo, Stat,
};
use user_progs::{Args, Line, close, error, exit, fstat, poll, read, sock_info, sockctl, sockfd, write};

/// Сколько ждать ответа эхо-сервера.
const REPLY_MS: i64 = 10_000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: пара (argc, argv) приходит от ядра и живёт всё время работы.
    let args = unsafe { Args::new(argc, argv) };
    match (args.get(1), args.get(2)) {
        (Some("serve"), Some(port)) => match port.parse::<u16>() {
            Ok(port) => serve(port),
            Err(_) => usage(),
        },
        (Some(host), Some(port)) => match (parse_ip(host), port.parse::<u16>()) {
            (Some(address), Ok(port)) => client(address, port),
            _ => usage(),
        },
        _ => usage(),
    }
}

fn usage() -> ! {
    error("usage: sockfd <address> <port> | sockfd serve <port>\n");
    exit(2)
}

fn fail(what: &str, code: i64) -> ! {
    Line::to_log().str("sockfd: FAILED ").str(what).str(": ").signed(code).end();
    exit(1)
}

/// Клиент к эхо-серверу.
fn client(address: u32, port: u16) -> ! {
    let target = ((address as usize) << 16) | usize::from(port);

    // Блокирующее соединение: ответ — когда собеседник ответил.
    let fd = sockfd(true, false);
    if fd < 0 {
        fail("socket", fd);
    }
    let code = sockctl(fd, SOCKCTL_CONNECT, target);
    if code < 0 {
        fail("connect", code);
    }
    Line::new().str("sockfd: connected as descriptor ").signed(fd).end();

    let text = b"descriptor-hello";
    let written = write(fd, text);
    if written != text.len() as i64 {
        fail("write", written);
    }

    // Ответ ждётся `poll` — в ядре, без опроса.
    let mut entry = [PollFd { fd, wanted: POLL_IN, ready: 0 }];
    let ready = poll(&mut entry, REPLY_MS);
    if ready != 1 || entry[0].ready & POLL_IN == 0 {
        fail("poll for the reply", ready);
    }
    let mut buffer = [0u8; 64];
    let mut got = 0usize;
    while got < text.len() {
        let read = read(fd, &mut buffer[got..]);
        if read <= 0 {
            fail("read", read);
        }
        got += read as usize;
    }
    let echoed = core::str::from_utf8(&buffer[..got]).unwrap_or("?");
    Line::new().str("sockfd: echoed '").str(echoed).str("' through read and write").end();

    // Своя половина закрыта — эхо-сервер закроет свою, и `read` вернёт ноль.
    let code = sockctl(fd, SOCKCTL_SHUTDOWN, 0);
    if code < 0 {
        fail("shutdown", code);
    }
    let end = read(fd, &mut buffer);
    if end != 0 {
        fail("end of stream", end);
    }
    Line::new().str("sockfd: end of stream reads as zero").end();

    let mut stat = Stat::default();
    let code = fstat(fd, &mut stat);
    if code < 0 || stat.kind != KIND_SOCKET {
        fail("fstat", code);
    }
    Line::new().str("sockfd: fstat says socket").end();
    close(fd);

    // Неблокирующее соединение: «идёт», а дождаться — `poll` на запись.
    let fd = sockfd(true, true);
    let code = sockctl(fd, SOCKCTL_CONNECT, target);
    if code != ERR_IN_PROGRESS {
        fail("nonblocking connect", code);
    }
    let mut entry = [PollFd { fd, wanted: POLL_OUT, ready: 0 }];
    let ready = poll(&mut entry, REPLY_MS);
    let mut info = SockInfo::default();
    if ready != 1 || sock_info(fd, &mut info) < 0 || info.open != 1 {
        fail("nonblocking connect finished", ready);
    }
    Line::new()
        .str("sockfd: nonblocking connect finished, local port ")
        .num(u64::from(info.local_port))
        .end();
    close(fd);

    Line::new().str("sockfd: client done").end();
    exit(0)
}

/// Сервер: принять одного клиента и вернуть ему всё, что он прислал.
fn serve(port: u16) -> ! {
    let fd = sockfd(true, false);
    let bound = sockctl(fd, SOCKCTL_BIND, usize::from(port));
    if bound < 0 {
        fail("bind", bound);
    }
    let code = sockctl(fd, SOCKCTL_LISTEN, 0);
    if code < 0 {
        fail("listen", code);
    }
    Line::new().str("sockfd: listening on port ").signed(bound).end();

    // Блокирующий `accept`: ответ — новый дескриптор.
    let client = sockctl(fd, SOCKCTL_ACCEPT, 0);
    if client < 0 {
        fail("accept", client);
    }
    let mut info = SockInfo::default();
    sock_info(client, &mut info);
    let [a, b, c, d] = info.remote.to_be_bytes();
    Line::new()
        .str("sockfd: accepted descriptor ")
        .signed(client)
        .str(" from ")
        .num(u64::from(a))
        .str(".")
        .num(u64::from(b))
        .str(".")
        .num(u64::from(c))
        .str(".")
        .num(u64::from(d))
        .end();

    let mut buffer = [0u8; 512];
    let mut total = 0u64;
    loop {
        let read = read(client, &mut buffer);
        if read < 0 {
            fail("read from the client", read);
        }
        if read == 0 {
            break;
        }
        let written = write(client, &buffer[..read as usize]);
        if written != read {
            fail("write to the client", written);
        }
        total += read as u64;
    }
    Line::new().str("sockfd: served ").num(total).str(" bytes, client closed").end();
    close(client);
    close(fd);
    exit(0)
}

/// Разобрать адрес вида `10.0.2.2`.
fn parse_ip(text: &str) -> Option<u32> {
    let mut bytes = [0u8; 4];
    let mut seen = 0;
    for (index, part) in text.split('.').enumerate() {
        if index >= 4 {
            return None;
        }
        bytes[index] = part.parse::<u8>().ok()?;
        seen = index + 1;
    }
    (seen == 4).then(|| u32::from_be_bytes(bytes))
}
