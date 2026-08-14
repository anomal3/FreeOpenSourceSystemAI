//! `echod` — сервер, возвращающий то, что ему прислали.
//!
//! Существует затем, чтобы фазу с TCP было чем предъявить: к системе
//! подключается **чужой** клиент с хоста, шлёт байты и получает их обратно.
//! Всё, что при этом должно сработать, работает или не работает целиком:
//! рукопожатие, окно, подтверждения, повторная передача и закрытие с обеих
//! сторон.
//!
//! # Одно соединение за раз, и это не упрощение ради краткости
//!
//! Несколько одновременных соединений потребовали бы либо потока на каждое
//! (потоков внутри программы у нас нет), либо цикла опроса по всем принятым
//! сразу. Второе несложно и когда-нибудь появится; сегодня же смысл программы в
//! том, чтобы доказать, что поток байт ходит туда и обратно без потерь, а для
//! этого одного соединения достаточно. Очередь входящих при этом ведёт ядро,
//! так что второй клиент не получит отказ — он подождёт.
//!
//! # Почему конец потока проверяется отдельным вызовом
//!
//! Потому что «данных пока нет» и «данных больше не будет» — разные вещи, а
//! приём отвечает на оба одинаково: ничем. Первое означает подождать, второе —
//! ответить своим `FIN` и закрыть соединение; спутать их значит либо повесить
//! сервер навсегда, либо оборвать клиента на середине.

#![no_std]
#![no_main]

use user_progs::{
    accept, bind, close_socket, error, error_num, exit, listen, recv, send, shutdown, sleep_ms,
    stream, stream_state, uptime_ms, ERR_AGAIN,
};

/// Порт, на котором сервер ждёт.
///
/// Две тысячи — не привилегированный порт (ниже 1024 в чужих системах требуют
/// root) и не эфемерный (те начинаются с 49152). Стенд пробрасывает его с
/// хоста, и число записано с обеих сторон.
const PORT: u16 = 2000;

/// Сколько ждать байт, прежде чем проверить, не закрылся ли клиент.
const IDLE_MS: u64 = 5;

/// Сколько сервер согласен ждать первого байта от подключившегося.
///
/// Клиент, подключившийся и замолчавший, не должен занимать сервер навсегда:
/// соединение одно, и следующему пришлось бы ждать столько же.
const CLIENT_TIMEOUT_MS: u64 = 30_000;

/// Наибольший кусок, который читается за раз.
const CHUNK: usize = 1024;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let server = stream();
    if server < 0 {
        error("echod: cannot open a socket\n");
        exit(1);
    }
    if bind(server, PORT) < 0 {
        error("echod: port 2000 is taken\n");
        exit(1);
    }
    if listen(server) < 0 {
        error("echod: cannot listen\n");
        exit(1);
    }
    error("echod: listening on port 2000\n");

    loop {
        let client = accept(server);
        if client == ERR_AGAIN {
            sleep_ms(IDLE_MS);
            continue;
        }
        if client < 0 {
            error("echod: accept failed with code ");
            error_num(client);
            error("\n");
            sleep_ms(100);
            continue;
        }

        error("echod: client accepted\n");
        let echoed = serve(client);
        close_socket(client);
        error("echod: client done, ");
        error_num(echoed as i64);
        error(" bytes echoed\n");
    }
}

/// Обслужить одно соединение. Возвращает, сколько байт вернули.
fn serve(client: i64) -> usize {
    let mut buffer = [0u8; CHUNK];
    let mut echoed = 0usize;
    let mut last_activity = uptime_ms();

    loop {
        let got = recv(client, &mut buffer);
        if got > 0 {
            let mut sent = 0usize;
            // Отправка может принять не всё сразу: буфер отправки конечен, и
            // это нормальный ход дел, а не отказ. Дописываем остаток, дав ядру
            // время вытолкнуть уже принятое.
            while sent < got as usize {
                let wrote = send(client, &buffer[sent..got as usize]);
                if wrote == ERR_AGAIN {
                    sleep_ms(IDLE_MS);
                    continue;
                }
                if wrote < 0 {
                    error("echod: send failed with code ");
                    error_num(wrote);
                    error("\n");
                    return echoed;
                }
                sent += wrote as usize;
            }
            echoed += sent;
            last_activity = uptime_ms();
            continue;
        }

        // Данных нет. Это либо пауза, либо конец — различает их состояние.
        let Some(state) = stream_state(client) else {
            return echoed;
        };
        if state.reset != 0 {
            error("echod: the client vanished\n");
            return echoed;
        }
        if state.peer_closed != 0 {
            // Клиент сказал всё. Отвечаем тем же: наш `FIN` уйдёт, когда
            // кончится то, что мы ещё не отправили.
            shutdown(client);
            return echoed;
        }
        if uptime_ms().saturating_sub(last_activity) > CLIENT_TIMEOUT_MS {
            error("echod: the client went quiet, dropping it\n");
            shutdown(client);
            return echoed;
        }
        sleep_ms(IDLE_MS);
    }
}
