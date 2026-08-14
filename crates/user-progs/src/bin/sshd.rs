//! `sshd` — сервер SSH. Фаза 37: транспорт, то есть обмен ключами и шифрование.
//!
//! # Что он делает сегодня
//!
//! Принимает соединение, представляется, договаривается об алгоритмах,
//! проводит обмен ключами по Curve25519, подписывает его ключом хоста и
//! переходит на шифрование. Дальше отвечает на запрос службы аутентификации —
//! и отказывает в самой аутентификации, потому что её механизм появится в
//! следующей фазе. Клиент при этом видит ровно то, что должен видеть: `ssh -v`
//! доходит до `Authentications that can continue`.
//!
//! # Ключ хоста
//!
//! Тридцать два байта в `/etc/ssh_host_ed25519_key`, права `0600`. Формат свой,
//! а не OpenSSH: их формат — это PEM с внутренним шифрованием, разбирать
//! который ради тридцати двух байт незачем. Ключ создаётся при первом запуске
//! из системного источника случайности, и **в журнале говорится, какого он
//! качества**: на машине без аппаратного генератора это ключ, собранный из
//! дрожания прерываний, и человек имеет право об этом знать.
//!
//! Ключ лежит на разделе состояния и потому переживает обновление системы —
//! иначе каждое обновление меняло бы отпечаток машины, и любой клиент встречал
//! бы её предупреждением о подмене.
//!
//! # Про размер буфера
//!
//! RFC 4253 требует уметь принимать пакет с содержимым до 32768 байт. Здесь
//! буфер [`BUFFER`] меньше, и это осознанный предел: программа живёт в окне
//! 512 КиБ, кучи у неё нет, а пакеты рукопожатия и сеанса оболочки на порядок
//! короче. Пакет длиннее буфера обрывает соединение с записью в журнал — то
//! есть заметно, а не молча.

#![no_std]
#![no_main]

use ssh::{Error, Transport};
use user_progs::{
    ERR_AGAIN, accept, bind, close_socket, create, error, error_num, exit, listen, open,
    open_write, random, read, recv, send, shutdown, sleep_ms, stream, stream_state, uptime_ms,
    write,
};

/// Порт, на котором ждёт сервер.
const PORT: u16 = 22;

/// Где лежит ключ хоста.
const HOST_KEY_PATH: &str = "/etc/ssh_host_ed25519_key";

/// Размер приёмного и передающего буферов.
const BUFFER: usize = 8192;

/// Сколько ждать байт, прежде чем проверить состояние соединения.
const IDLE_MS: u64 = 5;

/// Сколько всего отводится на рукопожатие.
///
/// Клиент, подключившийся и замолчавший, не должен занимать сервер: соединение
/// обслуживается одно за раз.
const HANDSHAKE_TIMEOUT_MS: u64 = 30_000;

/// Приёмный буфер: то, что пришло с провода и ещё не разобрано.
static mut INPUT: [u8; BUFFER] = [0u8; BUFFER];
/// Сколько байт в приёмном буфере ждут разбора.
///
/// Хранится между вызовами, и это не оптимизация. Клиент шлёт `KEXINIT` и
/// `KEX_ECDH_INIT` одним куском, и TCP отдаёт их одним чтением: читатель,
/// который после разбора первого пакета начинает с пустого буфера, теряет
/// второй — а клиент его больше не пришлёт и будет ждать ответа до своего
/// таймаута. Ровно так это и выглядело: алгоритмы согласованы, и тишина.
static mut FILLED: usize = 0;
/// Разобранное содержимое последнего пакета.
///
/// Отдельно от приёмного буфера потому, что после разбора в нём остаётся хвост
/// — начало следующего пакета, — и его надо сдвинуть к началу, не потеряв то,
/// что вызывающий сейчас читает.
static mut PAYLOAD: [u8; BUFFER] = [0u8; BUFFER];
/// Буфер под собираемый пакет.
static mut OUTPUT: [u8; BUFFER] = [0u8; BUFFER];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let Some(host_seed) = host_key() else {
        error("sshd: no host key and cannot make one\n");
        exit(1);
    };

    let server = stream();
    if server < 0 || bind(server, PORT) < 0 || listen(server) < 0 {
        error("sshd: cannot listen on port 22\n");
        exit(1);
    }
    error("sshd: listening on port 22\n");

    loop {
        let client = accept(server);
        if client == ERR_AGAIN {
            sleep_ms(IDLE_MS);
            continue;
        }
        if client < 0 {
            error("sshd: accept failed with code ");
            error_num(client);
            error("\n");
            sleep_ms(100);
            continue;
        }

        error("sshd: client connected\n");
        serve(client, host_seed);
        close_socket(client);
        error("sshd: client gone\n");
    }
}

/// Прочитать ключ хоста, а если его нет — создать.
fn host_key() -> Option<[u8; 32]> {
    let mut seed = [0u8; 32];

    let fd = open(HOST_KEY_PATH);
    if fd >= 0 {
        let read_bytes = read(fd, &mut seed);
        user_progs::close(fd);
        if read_bytes == 32 {
            error("sshd: host key read from ");
            error(HOST_KEY_PATH);
            error("\n");
            return Some(seed);
        }
        // Файл есть, но не тот. Перезаписывать чужой файл молча нельзя: это
        // может быть чей угодно файл, и новый ключ сменил бы отпечаток машины
        // без ведома человека.
        error("sshd: ");
        error(HOST_KEY_PATH);
        error(" is not a 32-byte key; refusing to touch it\n");
        return None;
    }

    if !random(&mut seed) {
        return None;
    }
    // Права `0600` обязательны: ключ хоста — это удостоверение машины, и
    // прочитавший его может выдать себя за неё.
    let fd = create(HOST_KEY_PATH, 0o600);
    let fd = if fd < 0 {
        // Файла нет, а создать не вышло — например, потому что раздел состояния
        // смонтирован только на чтение (безопасный режим). Ключ в памяти
        // работает, но при следующем запуске сменится, и об этом надо сказать.
        error("sshd: cannot save the host key; it will change on the next start\n");
        return Some(seed);
    } else {
        fd
    };
    let written = write(fd, &seed);
    user_progs::close(fd);
    if written != 32 {
        error("sshd: the host key did not save fully\n");
    } else {
        error("sshd: new host key generated and saved\n");
    }
    Some(seed)
}

/// Обслужить одно соединение до конца рукопожатия.
fn serve(client: i64, host_seed: [u8; 32]) {
    let mut kex_seed = [0u8; 32];
    if !random(&mut kex_seed) {
        error("sshd: no randomness for the exchange\n");
        return;
    }
    let mut cookie = [0u8; 16];
    random(&mut cookie);

    let mut transport = Transport::new(host_seed, kex_seed);
    let deadline = uptime_ms() + HANDSHAKE_TIMEOUT_MS;
    // Каждый клиент начинает с чистого буфера: то, что осталось от прошлого, —
    // это чужие байты, и разбирать их как начало нового разговора нельзя.
    unsafe { FILLED = 0 };

    // --- обмен версиями ---------------------------------------------------
    if send_all(client, ssh::VERSION_LINE.as_bytes()).is_none() {
        return;
    }
    let Some((peer, peer_len)) = read_version(client, deadline) else {
        error("sshd: the client never introduced itself\n");
        return;
    };
    // Длина здесь важнее, чем кажется: строка версии клиента входит в хеш
    // обмена **как есть**, и нулевой хвост буфера, уехавший туда вместе с ней,
    // даёт подпись, которую клиент честно отвергает — `incorrect signature`.
    // Ровно на это ушла отладка этой фазы: обмен ключами проходил целиком, ключ
    // хоста принимался, и связь обрывалась на последнем шаге.
    if transport.set_peer_version(&peer[..peer_len]).is_err() {
        return;
    }
    error("sshd: version exchanged\n");

    // --- согласование алгоритмов -----------------------------------------
    // SAFETY: программа однопоточная, буферы используются последовательно и
    // только здесь. Ссылка не переживает вызова, внутри которого мог бы
    // возникнуть второй заимствователь.
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let len = match transport.write_kexinit(cookie, out) {
        Ok(len) => len,
        Err(_) => return,
    };
    if send_packet(client, &mut transport, len).is_none() {
        return;
    }

    // Дальше три пакета клиента подряд, и порядок их задан протоколом.
    let Some(payload) = read_packet(client, &mut transport, deadline) else {
        return;
    };
    error("sshd: kexinit received
");
    if let Err(err) = transport.read_kexinit(payload) {
        report("sshd: no common algorithm with this client", err);
        // Отказ говорится вслух: клиент, не знающий curve25519, должен получить
        // причину, а не молчание.
        disconnect(client, &mut transport, ssh::DISCONNECT_KEY_EXCHANGE_FAILED);
        return;
    }

    let Some(payload) = read_packet(client, &mut transport, deadline) else {
        return;
    };
    // Разбор и ответ идут в один буфер по очереди: сначала из входного читаем,
    // потом в выходной пишем.
    let mut init = [0u8; 64];
    let payload_len = payload.len().min(init.len());
    init[..payload_len].copy_from_slice(&payload[..payload_len]);

    error("sshd: ecdh init received
");
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let len = match transport.reply_to_kex(&init[..payload_len], out) {
        Ok(len) => len,
        Err(err) => {
            report("sshd: the key exchange failed", err);
            disconnect(client, &mut transport, ssh::DISCONNECT_KEY_EXCHANGE_FAILED);
            return;
        }
    };
    if send_packet(client, &mut transport, len).is_none() {
        return;
    }

    error("sshd: ecdh reply sent
");

    // --- переход на шифрование -------------------------------------------
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    out[0] = ssh::MSG_NEWKEYS;
    if send_packet(client, &mut transport, 1).is_none() {
        return;
    }
    let Some(payload) = read_packet(client, &mut transport, deadline) else {
        error("sshd: no newkeys from the client
");
        return;
    };
    if payload.first() != Some(&ssh::MSG_NEWKEYS) {
        error("sshd: the client did not switch keys\n");
        return;
    }
    transport.enable_encryption();
    error("sshd: encrypted, curve25519-sha256 with chacha20-poly1305\n");

    // --- дальше пока только служба аутентификации -------------------------
    session(client, &mut transport, deadline);
}

/// Разговор после того, как ключи установлены.
///
/// В этой фазе он короткий: подтверждаем запрос службы аутентификации и
/// отказываем в самой аутентификации. Механизм входа — следующая фаза, и
/// притворяться, что он есть, было бы хуже, чем честно отказать.
fn session(client: i64, transport: &mut Transport, deadline: u64) {
    loop {
        let Some(payload) = read_packet(client, transport, deadline) else {
            return;
        };
        let Some(kind) = payload.first().copied() else {
            return;
        };

        match kind {
            ssh::MSG_SERVICE_REQUEST => {
                // Имя службы читаем и подтверждаем ровно то, что попросили:
                // ответ с другим именем клиент сочтёт ошибкой протокола.
                let mut reader = ssh::wire::Reader::new(&payload[1..]);
                let Some(service) = reader.string() else {
                    return;
                };
                let mut name = [0u8; 64];
                let len = service.len().min(name.len());
                name[..len].copy_from_slice(&service[..len]);

                let out = unsafe { &mut *(&raw mut OUTPUT) };
                let mut writer = ssh::wire::Writer::new(out);
                writer.byte(ssh::MSG_SERVICE_ACCEPT);
                writer.string(&name[..len]);
                if !writer.ok() {
                    return;
                }
                let written = writer.len();
                if send_packet(client, transport, written).is_none() {
                    return;
                }
                error("sshd: service accepted\n");
            }
            ssh::MSG_USERAUTH_REQUEST => {
                let out = unsafe { &mut *(&raw mut OUTPUT) };
                let mut writer = ssh::wire::Writer::new(out);
                writer.byte(ssh::MSG_USERAUTH_FAILURE);
                // Список того, чем можно продолжать. `publickey` названо
                // потому, что именно оно появится в следующей фазе; вход по
                // паролю не появится никогда — в `/etc/passwd` лежит не то,
                // что можно проверять по сети.
                writer.string(b"publickey");
                writer.byte(0);
                if !writer.ok() {
                    return;
                }
                let written = writer.len();
                if send_packet(client, transport, written).is_none() {
                    return;
                }
                error("sshd: authentication refused, phase 38 will answer this\n");
            }
            ssh::MSG_DISCONNECT => {
                error("sshd: the client said goodbye\n");
                return;
            }
            ssh::MSG_IGNORE | ssh::MSG_DEBUG => {}
            other => {
                // Неизвестное сообщение — не повод рвать связь: протокол прямо
                // предусматривает ответ «не умею», и клиент продолжит.
                let out = unsafe { &mut *(&raw mut OUTPUT) };
                out[0] = ssh::MSG_UNIMPLEMENTED;
                out[1..5].copy_from_slice(&0u32.to_be_bytes());
                if send_packet(client, transport, 5).is_none() {
                    return;
                }
                error("sshd: unimplemented message ");
                error_num(i64::from(other));
                error("\n");
            }
        }
    }
}

/// Отправить сообщение о разрыве и закрыть свою половину.
fn disconnect(client: i64, transport: &mut Transport, reason: u32) {
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = ssh::wire::Writer::new(out);
    writer.byte(ssh::MSG_DISCONNECT);
    writer.u32(reason);
    writer.string(b"no algorithm in common");
    writer.string(b"");
    if !writer.ok() {
        return;
    }
    let len = writer.len();
    send_packet(client, transport, len);
    shutdown(client);
}

/// Собрать пакет из первых `len` байт выходного буфера и отправить.
fn send_packet(client: i64, transport: &mut Transport, len: usize) -> Option<()> {
    // Содержимое лежит в начале `OUTPUT`, а пакет собирается вокруг него —
    // поэтому нужен второй буфер: заголовок, набивка и подпись не помещаются
    // «на месте».
    let mut framed = [0u8; BUFFER];
    // SAFETY: см. `serve`.
    let payload = unsafe { &(&raw const OUTPUT).as_ref().unwrap()[..len] };
    let total = transport.seal_packet(payload, &mut framed).ok()?;
    send_all(client, &framed[..total])
}

/// Прочитать один пакет целиком. Возвращает срез содержимого.
fn read_packet<'a>(
    client: i64,
    transport: &mut Transport,
    deadline: u64,
) -> Option<&'a [u8]> {
    // SAFETY: программа однопоточная, буферы используются последовательно.
    let input = unsafe { &mut *(&raw mut INPUT) };
    let payload_buffer = unsafe { &mut *(&raw mut PAYLOAD) };

    loop {
        // Сколько байт уже лежит в буфере — считая хвост, оставшийся от
        // прошлого пакета. Именно он и был потерян в первой версии: клиент
        // присылает `KEXINIT` и `KEX_ECDH_INIT` одним куском, TCP отдаёт их
        // одним чтением, и читатель, начинающий с пустого буфера, выбрасывал
        // второй пакет. Снаружи это выглядело как «алгоритмы согласованы, и
        // тишина»: обе стороны ждали друг друга.
        let filled = unsafe { FILLED };

        // Сколько нужно — знает транспорт: до шифрования длина видна прямо,
        // после — её приходится расшифровать.
        if let Some(size) = transport.packet_size(&input[..filled]) {
            let size = match size {
                Ok(size) => size,
                Err(err) => {
                    // Первый зашифрованный пакет, разобранный в мусор, — это
                    // почти всегда разошедшиеся ключи, а не испорченная сеть.
                    // Молчаливый выход отсюда стоил бы часа догадок.
                    report("sshd: cannot read the packet length", err);
                    return None;
                }
            };
            if size > input.len() {
                error("sshd: the client sent a packet larger than the buffer\n");
                return None;
            }
            if filled >= size {
                let (payload_len, _) = match transport.open_packet(&mut input[..size]) {
                    Ok(result) => result,
                    Err(err) => {
                        report("sshd: cannot open the packet", err);
                        return None;
                    }
                };
                payload_buffer[..payload_len].copy_from_slice(&input[..payload_len]);
                // Хвост переезжает в начало: это начало следующего пакета, и
                // оно обязано пережить разбор текущего.
                input.copy_within(size..filled, 0);
                unsafe { FILLED = filled - size };

                // Тип пакета — в журнал. Рукопожатие SSH это разговор из шести
                // сообщений, и когда одна сторона замолкает, единственный
                // вопрос — на каком именно.
                error("sshd: packet type ");
                error_num(i64::from(payload_buffer[0]));
                error("\n");

                // SAFETY: буфер статический и живёт всё время работы программы;
                // вызывающий разбирает содержимое до следующего чтения.
                return Some(unsafe { &(&raw const PAYLOAD).as_ref().unwrap()[..payload_len] });
            }
        }

        if filled >= input.len() {
            error("sshd: the buffer filled up without a whole packet\n");
            return None;
        }
        let got = recv(client, &mut input[filled..]);
        if got > 0 {
            unsafe { FILLED = filled + got as usize };
            continue;
        }
        // Молчание бывает трёх видов, и лечатся они по-разному: собеседник
        // закрылся, связь оборвана или мы ждём дольше отведённого. Одна строка
        // «ничего не пришло» на все три заставляла бы гадать.
        if got != ERR_AGAIN && got < 0 {
            error("sshd: recv failed with code ");
            error_num(got);
            error("\n");
            return None;
        }
        match stream_state(client) {
            Some(state) if state.reset != 0 => {
                error("sshd: the connection was reset\n");
                return None;
            }
            Some(state) if state.peer_closed != 0 => {
                error("sshd: the client closed its side\n");
                return None;
            }
            Some(_) => {}
            None => {
                error("sshd: the connection vanished\n");
                return None;
            }
        }
        if uptime_ms() >= deadline {
            error("sshd: the client went quiet\n");
            return None;
        }
        sleep_ms(IDLE_MS);
    }
}

/// Прочитать строку версии клиента: всё до `\r\n`.
fn read_version(client: i64, deadline: u64) -> Option<([u8; 255], usize)> {
    let mut line = [0u8; 255];
    let mut filled = 0usize;

    loop {
        let mut byte = [0u8; 1];
        let got = recv(client, &mut byte);
        if got == 1 {
            if byte[0] == b'\n' {
                // Отрезаем `\r`: в хеш обмена версия входит **без** конца
                // строки, и лишний байт там означает подпись, которая не
                // сойдётся у клиента.
                if filled > 0 && line[filled - 1] == b'\r' {
                    filled -= 1;
                }
                let mut out = [0u8; 255];
                out[..filled].copy_from_slice(&line[..filled]);
                // Длина возвращается **числом**, а не подразумевается по
                // нулевому хвосту: буфер, отданный целиком, однажды уже уехал
                // в хеш обмена вместе с нулями.
                return Some((out, filled));
            }
            if filled < line.len() {
                line[filled] = byte[0];
                filled += 1;
            }
            continue;
        }
        if !still_open(client) || uptime_ms() >= deadline {
            return None;
        }
        sleep_ms(IDLE_MS);
    }
}

/// Отправить всё, дописывая остаток.
fn send_all(client: i64, data: &[u8]) -> Option<()> {
    let mut sent = 0usize;
    while sent < data.len() {
        let wrote = send(client, &data[sent..]);
        if wrote == ERR_AGAIN {
            sleep_ms(IDLE_MS);
            continue;
        }
        if wrote < 0 {
            return None;
        }
        sent += wrote as usize;
    }
    Some(())
}

/// Жив ли ещё собеседник.
fn still_open(client: i64) -> bool {
    match stream_state(client) {
        Some(state) => state.reset == 0 && state.peer_closed == 0,
        None => false,
    }
}

fn report(text: &str, err: Error) {
    error(text);
    error(" (");
    error(match err {
        Error::BadLength => "bad length",
        Error::BadTag => "bad tag",
        Error::Malformed => "malformed",
        Error::NoCommonAlgorithm => "no common algorithm",
        Error::NoRoom => "no room",
        Error::OutOfOrder => "out of order",
    });
    error(")\n");
}

/// Заглушка, чтобы `open_write` не оказался неиспользованным: он понадобится
/// фазе 38 для `authorized_keys`.
#[allow(dead_code)]
fn unused() {
    let _ = open_write;
}
