//! `echoc` — клиент к эхо-серверу: подключиться, сказать, дослушать, закрыть.
//!
//! Нужен затем, что `echod` проверяет только **входящие** соединения: там `SYN`
//! присылают нам. Активное открытие — другая половина автомата (`SYN_SENT`,
//! ожидание `SYN+ACK`, наше подтверждение), и без этой программы она осталась бы
//! написанной, но непроверенной.
//!
//! Использование: `echoc <адрес> <порт> <текст>`.

#![no_std]
#![no_main]

use user_progs::{
    Args, close_socket, connect, error, error_num, exit, print, println, recv, send, shutdown,
    sleep_ms, stream, stream_state, uptime_ms, wait_connected, ERR_AGAIN,
};

/// Сколько ждать установления связи.
const CONNECT_TIMEOUT_MS: u64 = 10_000;

/// Сколько ждать ответа после того, как всё отправлено.
const REPLY_TIMEOUT_MS: u64 = 10_000;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: пара (argc, argv) приходит от ядра, которое собрало её из
    // командной строки и держит живой всё время работы программы.
    let args = unsafe { Args::new(argc, argv) };
    let (Some(host), Some(port), Some(text)) = (args.get(1), args.get(2), args.get(3)) else {
        error("usage: echoc <address> <port> <text>\n");
        exit(2);
    };
    let (Some(address), Ok(port)) = (parse_ip(host), port.parse::<u16>()) else {
        error("echoc: that is not an address and a port\n");
        exit(2);
    };

    let socket = stream();
    if socket < 0 {
        error("echoc: cannot open a socket\n");
        exit(1);
    }
    if connect(socket, address, port) < 0 {
        error("echoc: cannot start the connection\n");
        exit(1);
    }
    if !wait_connected(socket, CONNECT_TIMEOUT_MS) {
        error("echoc: nobody answered\n");
        close_socket(socket);
        exit(1);
    }

    let mut sent = 0usize;
    while sent < text.len() {
        let wrote = send(socket, &text.as_bytes()[sent..]);
        if wrote == ERR_AGAIN {
            sleep_ms(5);
            continue;
        }
        if wrote < 0 {
            error("echoc: send failed with code ");
            error_num(wrote);
            error("\n");
            close_socket(socket);
            exit(1);
        }
        sent += wrote as usize;
    }
    // Сказать «я всё» обязательно: эхо-сервер на той стороне читает, пока не
    // увидит наш `FIN`, и без него оба будут ждать друг друга до таймаута.
    shutdown(socket);

    let mut buffer = [0u8; 256];
    let mut got = 0usize;
    let deadline = uptime_ms() + REPLY_TIMEOUT_MS;
    while got < text.len() && uptime_ms() < deadline {
        let read = recv(socket, &mut buffer[got..]);
        if read > 0 {
            got += read as usize;
            continue;
        }
        match stream_state(socket) {
            // Собеседник закрылся, и добавить ему больше нечего.
            Some(state) if state.peer_closed != 0 => break,
            Some(state) if state.reset != 0 => {
                error("echoc: the connection was reset\n");
                close_socket(socket);
                exit(1);
            }
            _ => sleep_ms(5),
        }
    }
    close_socket(socket);

    print("echoc: got back ");
    // Печатается длина и само эхо: длина отвечает на вопрос «сколько дошло»,
    // текст — на вопрос «то ли самое».
    error_num(got as i64);
    print(" bytes: ");
    match core::str::from_utf8(&buffer[..got]) {
        Ok(text) => println(text),
        Err(_) => println("<not utf-8>"),
    }
    // Код возврата — не украшение: по нему видно, совпало ли эхо, даже когда
    // строку не с чем сравнить глазом.
    exit(i64::from(got != text.len()));
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
