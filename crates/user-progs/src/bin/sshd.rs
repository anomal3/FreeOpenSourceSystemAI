//! `sshd` — сервер SSH. Транспорт (фаза 37), вход по ключу и сеанс (фаза 38).
//!
//! # Что он делает
//!
//! Принимает соединение, договаривается об алгоритмах, проводит обмен ключами
//! по Curve25519, подписывает его ключом хоста и переходит на шифрование. Затем
//! пускает в систему по открытому ключу — и открывает сеанс: канал, команда,
//! вывод, код завершения. Снаружи это выглядит как обычный SSH:
//!
//! ```text
//!   ssh -i ~/.ssh/id_ed25519 roman@адрес uptime
//!   ssh -i ~/.ssh/id_ed25519 roman@адрес           # и дальше построчно
//! ```
//!
//! # Вход только по ключу, и это навсегда
//!
//! Пароля не будет. В `/etc/passwd` этой системы лежит итерированный FNV-1a, а
//! не функция выведения ключа (см. `crates/installer/src/account.rs`, там это
//! сказано прямо). Пускать по такому отпечатку **по сети** значило бы выдать
//! его за защиту: перебор словаря по FNV стоит ровно столько же, сколько его
//! вычисление. Единственный метод — `publickey`, и отказ во всём остальном
//! жёсткий.
//!
//! # Кого он пускает
//!
//! Того, кто есть в `/etc/passwd` и чей ключ записан в
//! `<домашний каталог>/.ssh/authorized_keys`. Отсюда два следствия, оба
//! намеренные:
//!
//! * **в живую систему (загруженную с носителя) войти нельзя вовсе** — на
//!   образе initrd нет ни учётных записей, ни домашних каталогов, и класть их
//!   туда нельзя: этот образ уезжает в выпущенный ISO, а ключ, попавший в
//!   выпуск, пускает в систему всех, кто её скачал;
//! * **root не входит по сети никогда** — записи `root` в `/etc/passwd` нет,
//!   установщик её не пишет, и это ровно то, что советуют всем настоящим
//!   серверам SSH.
//!
//! Файл ключей проверяется так же, как это делает OpenSSH: он и каталоги над
//! ним обязаны принадлежать этому пользователю (или root) и не быть открытыми
//! на запись кому попало. Ключ, лежащий в каталоге, куда может писать чужой, —
//! это не разрешение владельца, а разрешение того, кто туда пишет.
//!
//! # Права проверяет он сам, и вот почему
//!
//! Команды сеанса исполняются **внутри** `sshd`, то есть от имени root: своей
//! задачей их не запустить, потому что перенаправить вывод чужой программы в
//! сокет нечем — каналов (`pipe`) в системе пока нет. Поэтому там, где команда
//! трогает файл, `sshd` спрашивает разрешение сам: проходит путь по каталогам и
//! сверяет режим с `uid`/`gid` того, кого впустили. Правило, которое из этого
//! следует, сформулировано и проверяется стендом: **по сети видно ровно то же,
//! что видно этому человеку за терминалом машины, — ни байтом больше.**
//!
//! Настоящий ответ на тот же вопрос — запускать `/bin/…` от имени вошедшего и
//! читать её вывод через канал. Это отдельная фаза (38b в дорожной карте):
//! нужны `pipe` и перенаправление при `spawn`. Выдавать сегодняшний набор
//! встроенных команд за оболочку нельзя, поэтому `help` говорит об этом прямо.
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
//! # Про размер буферов
//!
//! RFC 4253 требует уметь принимать пакет с содержимым до 32768 байт. Здесь
//! буфер [`BUFFER`] меньше, и это осознанный предел: программа живёт в окне
//! 1,5 МиБ, кучи у неё нет, а стек — 32 КиБ на всю работу. Пакет длиннее буфера
//! обрывает соединение с записью в журнал — то есть заметно, а не молча. По той
//! же причине большие буферы здесь статические, а не на стеке.

#![no_std]
#![no_main]

use ssh::auth;
use ssh::wire::{Reader, Writer};
use ssh::{Error, Transport};
use user_abi::Stat;
use user_progs::{
    Dirent, ERR_AGAIN, KIND_DIRECTORY, KIND_FILE, Path, accept, bind, close, close_socket, create,
    error, error_num, exit, listen, open, random, read, readdir, recv, send, shutdown, sleep_ms,
    stat, stream, stream_state, uptime_ms, write,
};

/// Порт, на котором ждёт сервер.
const PORT: u16 = 22;

/// Где лежит ключ хоста.
const HOST_KEY_PATH: &str = "/etc/ssh_host_ed25519_key";

/// Где лежат учётные записи.
const PASSWD_PATH: &str = "/etc/passwd";

/// Размер приёмного и передающего буферов.
const BUFFER: usize = 8192;

/// Сколько ждать байт, прежде чем проверить состояние соединения.
const IDLE_MS: u64 = 5;

/// Сколько всего отводится на рукопожатие и вход.
///
/// Клиент, подключившийся и замолчавший, не должен занимать сервер: соединение
/// обслуживается одно за раз.
const HANDSHAKE_TIMEOUT_MS: u64 = 30_000;

/// Сколько сеанс может молчать, прежде чем его закроют.
///
/// Отсчёт начинается заново после каждого пакета: человек, читающий вывод, не
/// шлёт ничего, и обрывать его на середине чтения было бы хамством.
const SESSION_TIMEOUT_MS: u64 = 300_000;

/// Сколько ждать прощания клиента после того, как сеанс закрыт с нашей стороны.
const GOODBYE_TIMEOUT_MS: u64 = 10_000;

/// Сколько неудачных попыток входа терпеть.
///
/// Столько же по умолчанию у OpenSSH. Число нужно не против перебора ключей —
/// перебрать Ed25519 нельзя, — а против клиента, у которого в агенте десяток
/// ключей и который переберёт их все, занимая единственное соединение.
const MAX_AUTH_ATTEMPTS: u32 = 6;

/// Номер канала с нашей стороны. Канал всегда один, поэтому номер постоянный.
const CHANNEL_ID: u32 = 0;

/// Сколько байт мы готовы принять в канал, не подтверждая приёма.
const OUR_WINDOW: u32 = 32 * 1024;

/// Наибольший пакет канала, который мы согласны принять.
const OUR_MAX_PACKET: u32 = 4096;

/// Сколько байт вывода копится, прежде чем уехать пакетом.
const STAGE: usize = 512;

/// Сколько байт файла (`/etc/passwd`, `authorized_keys`) читается целиком.
const FILE_LIMIT: usize = 4096;

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
/// Буфер под прочитанный файл: `/etc/passwd` или `authorized_keys`.
///
/// Статический, а не на стеке: стека у программы 32 КиБ на всё, и четыре
/// килобайта в кадре, живущем во время отправки пакета (а там свой буфер на
/// восемь), — это способ узнать про охранную страницу.
static mut FILE_BUFFER: [u8; FILE_LIMIT] = [0u8; FILE_LIMIT];

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
    // Кого вообще можно впустить, видно до первого соединения — и сказать об
    // этом лучше сразу. Система без учётных записей молча отказывает всем, и
    // «сервер работает, но не пускает» — худший из способов это узнать.
    announce_accounts();

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
        close(fd);
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
    close(fd);
    if written != 32 {
        error("sshd: the host key did not save fully\n");
    } else {
        error("sshd: new host key generated and saved\n");
    }
    Some(seed)
}

/// Сказать в журнал, есть ли вообще кого пускать.
fn announce_accounts() {
    match read_file(PASSWD_PATH) {
        Some(len) if len > 0 => {
            error("sshd: accounts come from ");
            error(PASSWD_PATH);
            error("\n");
        }
        _ => {
            error("sshd: no ");
            error(PASSWD_PATH);
            error(" here, so nobody can log in (this is a live image)\n");
        }
    }
}

/// Обслужить одно соединение от рукопожатия до конца сеанса.
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
    // Ровно на это ушла отладка фазы 37: обмен ключами проходил целиком, ключ
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
    error("sshd: kexinit received\n");
    if let Err(err) = transport.read_kexinit(payload) {
        report("sshd: no common algorithm with this client", err);
        // Отказ говорится вслух: клиент, не знающий curve25519, должен получить
        // причину, а не молчание.
        disconnect(
            client,
            &mut transport,
            ssh::DISCONNECT_KEY_EXCHANGE_FAILED,
            "no algorithm in common",
        );
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

    error("sshd: ecdh init received\n");
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let len = match transport.reply_to_kex(&init[..payload_len], out) {
        Ok(len) => len,
        Err(err) => {
            report("sshd: the key exchange failed", err);
            disconnect(
                client,
                &mut transport,
                ssh::DISCONNECT_KEY_EXCHANGE_FAILED,
                "key exchange failed",
            );
            return;
        }
    };
    if send_packet(client, &mut transport, len).is_none() {
        return;
    }
    error("sshd: ecdh reply sent\n");

    // --- переход на шифрование -------------------------------------------
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    out[0] = ssh::MSG_NEWKEYS;
    if send_packet(client, &mut transport, 1).is_none() {
        return;
    }
    let Some(payload) = read_packet(client, &mut transport, deadline) else {
        error("sshd: no newkeys from the client\n");
        return;
    };
    if payload.first() != Some(&ssh::MSG_NEWKEYS) {
        error("sshd: the client did not switch keys\n");
        return;
    }
    transport.enable_encryption();
    error("sshd: encrypted, curve25519-sha256 with chacha20-poly1305\n");

    session(client, &mut transport, deadline);
}

// --- сеанс -------------------------------------------------------------------

/// Открытый канал сеанса.
struct Channel {
    /// Номер канала с той стороны — его называют во всех сообщениях канала.
    peer: u32,
    /// Сколько байт нам ещё разрешено отправить.
    window: u32,
    /// Наибольший пакет, который согласен принять клиент.
    max_packet: u32,
    /// Сколько байт клиента принято с последней прибавки окна.
    consumed: u32,
    /// Сеанс построчный: команды приходят в канал, а не одной строкой в `exec`.
    interactive: bool,
    /// Незаконченная строка ввода.
    line: [u8; 256],
    line_len: usize,
}

impl Channel {
    fn new(peer: u32, window: u32, max_packet: u32) -> Self {
        Self {
            peer,
            window,
            max_packet,
            consumed: 0,
            interactive: false,
            line: [0u8; 256],
            line_len: 0,
        }
    }
}

/// Чем кончилась одна попытка входа.
enum Attempt {
    /// Впустили.
    Accepted(User),
    /// «Такой ключ подойдёт?» — это ещё не попытка, а вопрос.
    Query,
    /// Не впустили.
    Refused,
    /// Клиент нарушил протокол; разговаривать дальше не о чем.
    Fatal,
}

/// Разговор после того, как ключи установлены.
///
/// Порядок здесь задан RFC 4252 и 4254 и обязателен: сначала запрос службы,
/// потом вход, и только потом каналы. Канал, открытый до входа, — это не
/// вольность клиента, а попытка обойти проверку, и отвечать на неё надо
/// разрывом.
fn session(client: i64, transport: &mut Transport, handshake_deadline: u64) {
    let mut user: Option<User> = None;
    let mut attempts = 0u32;
    let mut channel: Option<Channel> = None;
    let mut finished = false;

    loop {
        // Срок ожидания зависит от того, чего мы ждём. До входа — общий срок
        // рукопожатия; в сеансе — своя тишина на каждый пакет; после закрытия
        // канала — только прощание клиента.
        let deadline = if finished {
            uptime_ms() + GOODBYE_TIMEOUT_MS
        } else if user.is_some() {
            uptime_ms() + SESSION_TIMEOUT_MS
        } else {
            handshake_deadline
        };

        let Some(payload) = read_packet(client, transport, deadline) else {
            return;
        };
        let Some(kind) = payload.first().copied() else {
            return;
        };

        match kind {
            ssh::MSG_SERVICE_REQUEST => {
                if !accept_service(client, transport, payload) {
                    return;
                }
            }
            ssh::MSG_USERAUTH_REQUEST => {
                if user.is_some() {
                    // RFC 4252 §5.1: после успеха такие запросы игнорируются.
                    // Не отказ и не разрыв — именно тишина, иначе клиент,
                    // отправивший второй запрос вдогонку, получил бы отказ уже
                    // после того, как его впустили.
                    continue;
                }
                match authenticate(client, transport, payload) {
                    Attempt::Accepted(accepted) => {
                        error("sshd: ");
                        error(accepted.name());
                        error(" authenticated with a key from ");
                        error(accepted.home());
                        error("/.ssh/authorized_keys\n");
                        let out = unsafe { &mut *(&raw mut OUTPUT) };
                        out[0] = ssh::MSG_USERAUTH_SUCCESS;
                        if send_packet(client, transport, 1).is_none() {
                            return;
                        }
                        user = Some(accepted);
                    }
                    Attempt::Query => {}
                    Attempt::Refused => {
                        attempts += 1;
                        if attempts >= MAX_AUTH_ATTEMPTS {
                            error("sshd: too many failed attempts, closing\n");
                            disconnect(
                                client,
                                transport,
                                ssh::DISCONNECT_NO_MORE_AUTH,
                                "too many failed attempts",
                            );
                            return;
                        }
                    }
                    Attempt::Fatal => return,
                }
            }
            ssh::MSG_CHANNEL_OPEN => {
                if user.is_none() {
                    error("sshd: a channel before the login; closing\n");
                    disconnect(
                        client,
                        transport,
                        ssh::DISCONNECT_PROTOCOL_ERROR,
                        "authenticate first",
                    );
                    return;
                }
                match open_channel(client, transport, payload, channel.is_some()) {
                    Some(opened) => channel = Some(opened),
                    None => {
                        // Отказ уже отправлен; соединение живёт дальше — клиент
                        // вправе попробовать другой тип канала.
                    }
                }
            }
            ssh::MSG_CHANNEL_REQUEST => {
                let (Some(user), Some(open)) = (user.as_ref(), channel.as_mut()) else {
                    error("sshd: a channel request without a channel\n");
                    return;
                };
                match channel_request(client, transport, payload, user, open) {
                    Some(true) => {}
                    Some(false) => {
                        channel = None;
                        finished = true;
                    }
                    None => return,
                }
            }
            ssh::MSG_CHANNEL_DATA => {
                let (Some(user), Some(open)) = (user.as_ref(), channel.as_mut()) else {
                    continue;
                };
                match channel_data(client, transport, payload, user, open) {
                    Some(true) => {}
                    Some(false) => {
                        channel = None;
                        finished = true;
                    }
                    None => return,
                }
            }
            ssh::MSG_CHANNEL_EOF => {
                // Клиент закрыл свой ввод. Для построчного сеанса это конец
                // разговора — ровно как `Ctrl-D` в терминале.
                let Some(open) = channel.as_mut() else {
                    continue;
                };
                if open.interactive {
                    if finish_channel(client, transport, open, 0).is_none() {
                        return;
                    }
                    channel = None;
                    finished = true;
                }
            }
            ssh::MSG_CHANNEL_CLOSE => {
                if let Some(open) = channel.as_ref() {
                    // Отвечаем своим закрытием: канал закрыт, когда закрыты обе
                    // половины, и клиент ждёт подтверждения.
                    let out = unsafe { &mut *(&raw mut OUTPUT) };
                    let mut writer = Writer::new(out);
                    writer.byte(ssh::MSG_CHANNEL_CLOSE);
                    writer.u32(open.peer);
                    let len = writer.len();
                    send_packet(client, transport, len);
                }
                channel = None;
                finished = true;
            }
            ssh::MSG_CHANNEL_WINDOW_ADJUST => {
                let mut reader = Reader::new(&payload[1..]);
                let (Some(_), Some(more)) = (reader.u32(), reader.u32()) else {
                    return;
                };
                if let Some(open) = channel.as_mut() {
                    open.window = open.window.saturating_add(more);
                }
            }
            ssh::MSG_GLOBAL_REQUEST => {
                // Обычно это `keepalive@openssh.com` или проброс порта. Первое
                // требует ответа, второго здесь нет; общий отказ годится обоим.
                let mut reader = Reader::new(&payload[1..]);
                let want_reply = reader.string().is_some() && reader.byte() == Some(1);
                if want_reply {
                    let out = unsafe { &mut *(&raw mut OUTPUT) };
                    out[0] = ssh::MSG_REQUEST_FAILURE;
                    if send_packet(client, transport, 1).is_none() {
                        return;
                    }
                }
            }
            ssh::MSG_DISCONNECT => {
                error("sshd: the client said goodbye\n");
                return;
            }
            ssh::MSG_IGNORE | ssh::MSG_DEBUG | ssh::MSG_UNIMPLEMENTED => {}
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

/// Подтвердить запрос службы. `false` — разговаривать дальше не о чем.
fn accept_service(client: i64, transport: &mut Transport, payload: &[u8]) -> bool {
    let mut reader = Reader::new(&payload[1..]);
    let Some(service) = reader.string() else {
        return false;
    };
    // Служба у нас одна. Подтвердить чужое имя значило бы согласиться на
    // разговор, которого мы не понимаем.
    if service != auth::SERVICE.as_bytes() {
        error("sshd: the client asked for a service we do not have\n");
        disconnect(
            client,
            transport,
            ssh::DISCONNECT_PROTOCOL_ERROR,
            "no such service",
        );
        return false;
    }

    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_SERVICE_ACCEPT);
    writer.string(auth::SERVICE.as_bytes());
    if !writer.ok() {
        return false;
    }
    let len = writer.len();
    if send_packet(client, transport, len).is_none() {
        return false;
    }
    error("sshd: service accepted\n");
    true
}

/// Разобрать попытку входа и ответить на неё.
fn authenticate(client: i64, transport: &mut Transport, payload: &[u8]) -> Attempt {
    let Some(request) = auth::parse_request(payload) else {
        error("sshd: the login request does not parse\n");
        return Attempt::Fatal;
    };
    if request.service != auth::SERVICE.as_bytes() {
        error("sshd: the login is for a service we do not have\n");
        disconnect(
            client,
            transport,
            ssh::DISCONNECT_PROTOCOL_ERROR,
            "no such service",
        );
        return Attempt::Fatal;
    }

    // Метод `none` клиент присылает первым всегда: так он узнаёт список того,
    // чем можно продолжать. Отказ на него — это не отказ во входе, а ответ.
    if request.method != b"publickey" {
        error("sshd: login method ");
        error_bytes(request.method);
        error(" refused; only publickey is offered\n");
        return match refuse(client, transport) {
            Some(()) if request.method == b"none" => Attempt::Query,
            Some(()) => Attempt::Refused,
            None => Attempt::Fatal,
        };
    }
    if request.algorithm != auth::KEY_ALGORITHM.as_bytes() {
        error("sshd: key algorithm ");
        error_bytes(request.algorithm);
        error(" is not one we check\n");
        return finish_refusal(client, transport);
    }
    let Some(key) = auth::key_from_blob(request.key_blob) else {
        error("sshd: the key does not parse\n");
        return finish_refusal(client, transport);
    };
    let Some(user) = find_user(request.user) else {
        // Имя не называется в ответе клиенту — ему и так известно, что он
        // прислал, — но в журнал машины оно попадает: это единственный след
        // того, кто и подо что стучался.
        error("sshd: no account named ");
        error_bytes(request.user);
        error(" in ");
        error(PASSWD_PATH);
        error("\n");
        return finish_refusal(client, transport);
    };
    if !key_authorized(&user, &key) {
        return finish_refusal(client, transport);
    }

    if !request.has_signature {
        // Вопрос «годится ли такой ключ?». Ответ на него не пускает никуда:
        // клиент спрашивает, чтобы не заставлять человека трогать все ключи
        // подряд, — а вход будет со следующим пакетом, уже с подписью.
        let out = unsafe { &mut *(&raw mut OUTPUT) };
        let mut writer = Writer::new(out);
        writer.byte(ssh::MSG_USERAUTH_PK_OK);
        writer.string(request.algorithm);
        writer.string(request.key_blob);
        if !writer.ok() {
            return Attempt::Fatal;
        }
        let len = writer.len();
        if send_packet(client, transport, len).is_none() {
            return Attempt::Fatal;
        }
        error("sshd: this key would be accepted; asking for a signature\n");
        return Attempt::Query;
    }

    // Подписанное включает идентификатор сеанса, поэтому подпись годится ровно
    // для этого соединения и ни для какого другого.
    let mut scratch = [0u8; 512];
    if !auth::verify(&request, transport.session_id(), &mut scratch) {
        error("sshd: the signature does not check out\n");
        return finish_refusal(client, transport);
    }
    Attempt::Accepted(user)
}

/// Отказать во входе, назвав то, чем можно продолжать.
fn refuse(client: i64, transport: &mut Transport) -> Option<()> {
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_USERAUTH_FAILURE);
    // Вход по паролю не появится никогда — в `/etc/passwd` лежит не то, что
    // можно проверять по сети (см. заголовок).
    writer.string(b"publickey");
    // Частичный успех — это про многофакторный вход, которого здесь нет.
    writer.byte(0);
    if !writer.ok() {
        return None;
    }
    let len = writer.len();
    send_packet(client, transport, len)
}

/// Отказ, посчитанный попыткой.
fn finish_refusal(client: i64, transport: &mut Transport) -> Attempt {
    match refuse(client, transport) {
        Some(()) => Attempt::Refused,
        None => Attempt::Fatal,
    }
}

/// Открыть канал сеанса. `None` — отказали (и сказали об этом клиенту).
fn open_channel(
    client: i64,
    transport: &mut Transport,
    payload: &[u8],
    busy: bool,
) -> Option<Channel> {
    let mut reader = Reader::new(&payload[1..]);
    let kind = reader.string()?;
    let peer = reader.u32()?;
    let window = reader.u32()?;
    let max_packet = reader.u32()?;

    let reason = if kind != b"session" {
        Some((ssh::OPEN_UNKNOWN_CHANNEL_TYPE, "only session channels are served"))
    } else if busy {
        // Один канал за раз — не упрощение, а следствие устройства: команда
        // исполняется внутри сервера и занимает его целиком, поэтому второй
        // канал всё равно ждал бы первого, но клиент об этом не узнал бы.
        Some((ssh::OPEN_RESOURCE_SHORTAGE, "one session at a time"))
    } else {
        None
    };

    if let Some((code, text)) = reason {
        error("sshd: channel refused: ");
        error(text);
        error("\n");
        let out = unsafe { &mut *(&raw mut OUTPUT) };
        let mut writer = Writer::new(out);
        writer.byte(ssh::MSG_CHANNEL_OPEN_FAILURE);
        writer.u32(peer);
        writer.u32(code);
        writer.string(text.as_bytes());
        writer.string(b"");
        if writer.ok() {
            let len = writer.len();
            send_packet(client, transport, len);
        }
        return None;
    }

    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_CHANNEL_OPEN_CONFIRMATION);
    writer.u32(peer);
    writer.u32(CHANNEL_ID);
    writer.u32(OUR_WINDOW);
    writer.u32(OUR_MAX_PACKET);
    if !writer.ok() {
        return None;
    }
    let len = writer.len();
    send_packet(client, transport, len)?;
    error("sshd: session channel opened\n");
    Some(Channel::new(peer, window, max_packet))
}

/// Ответить на запрос в канале.
///
/// `Some(true)` — канал жив дальше, `Some(false)` — сеанс закончен, `None` —
/// соединение потеряно.
fn channel_request(
    client: i64,
    transport: &mut Transport,
    payload: &[u8],
    user: &User,
    channel: &mut Channel,
) -> Option<bool> {
    let mut reader = Reader::new(&payload[1..]);
    let _recipient = reader.u32()?;
    let kind = reader.string()?;
    let want_reply = reader.byte()? != 0;

    match kind {
        b"exec" => {
            let command = reader.string()?;
            if want_reply {
                reply_to_request(client, transport, channel.peer, true)?;
            }
            error("sshd: exec '");
            error_bytes(command);
            error("' for ");
            error(user.name());
            error("\n");
            let status = run_in_channel(client, transport, channel, user, command);
            finish_channel(client, transport, channel, status)?;
            Some(false)
        }
        b"shell" => {
            if want_reply {
                reply_to_request(client, transport, channel.peer, true)?;
            }
            channel.interactive = true;
            error("sshd: interactive session for ");
            error(user.name());
            error("\n");
            let mut sink = Sink::new(client, transport, channel);
            sink.out("FreeOS: commands here run inside sshd; 'help' lists them.\n");
            sink.out(PROMPT);
            sink.finish(channel);
            Some(true)
        }
        // Псевдотерминала здесь нет, и притворяться, что есть, нельзя: клиент,
        // получивший согласие, переключит свой терминал в неструктурированный
        // режим и станет ждать от нас управляющих последовательностей. Честный
        // отказ он переживает — и говорит об этом одной строкой.
        _ => {
            if want_reply {
                reply_to_request(client, transport, channel.peer, false)?;
            }
            error("sshd: channel request '");
            error_bytes(kind);
            error("' refused\n");
            Some(true)
        }
    }
}

/// Принять данные, пришедшие в канал: это ввод построчного сеанса.
fn channel_data(
    client: i64,
    transport: &mut Transport,
    payload: &[u8],
    user: &User,
    channel: &mut Channel,
) -> Option<bool> {
    let mut reader = Reader::new(&payload[1..]);
    let _recipient = reader.u32()?;
    let data = reader.string()?;

    // Окно приёма: клиент вправе слать, пока не выберет объявленное. Прибавка
    // отправляется заранее, на половине, — иначе клиент, приславший ровно окно,
    // замолчал бы до нашего ответа.
    channel.consumed = channel.consumed.saturating_add(data.len() as u32);
    if channel.consumed * 2 >= OUR_WINDOW {
        let out = unsafe { &mut *(&raw mut OUTPUT) };
        let mut writer = Writer::new(out);
        writer.byte(ssh::MSG_CHANNEL_WINDOW_ADJUST);
        writer.u32(channel.peer);
        writer.u32(channel.consumed);
        let len = writer.len();
        send_packet(client, transport, len)?;
        channel.consumed = 0;
    }

    if !channel.interactive {
        // Ввод для `exec`: команде читать его нечем, и молча копить его в
        // памяти было бы хуже, чем не принимать.
        return Some(true);
    }

    for byte in data {
        match byte {
            b'\n' => {
                let mut line = [0u8; 256];
                let len = channel.line_len;
                line[..len].copy_from_slice(&channel.line[..len]);
                channel.line_len = 0;

                let trimmed = trim(&line[..len]);
                if trimmed == b"exit" || trimmed == b"logout" {
                    finish_channel(client, transport, channel, 0)?;
                    return Some(false);
                }
                let mut sink = Sink::new(client, transport, channel);
                // Код завершения построчный сеанс не показывает: показывать его
                // было бы нечем — переменных здесь нет, а печатать число после
                // каждой команды не делает ни одна оболочка.
                let _status = run_command(trimmed, user, &mut sink);
                sink.out(PROMPT);
                sink.finish(channel);
            }
            // Возврат каретки приезжает от клиентов, работающих через терминал.
            // Он часть перевода строки, а не ввода.
            b'\r' => {}
            byte => {
                if channel.line_len < channel.line.len() {
                    channel.line[channel.line_len] = *byte;
                    channel.line_len += 1;
                }
                // Строка длиннее буфера обрезается молча только здесь: сказать
                // об этом можно лишь в ответе на неё, а ответ ещё не начат.
            }
        }
    }
    Some(true)
}

/// Ответить на запрос в канале согласием или отказом.
fn reply_to_request(
    client: i64,
    transport: &mut Transport,
    peer: u32,
    accepted: bool,
) -> Option<()> {
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(if accepted {
        ssh::MSG_CHANNEL_SUCCESS
    } else {
        ssh::MSG_CHANNEL_FAILURE
    });
    writer.u32(peer);
    if !writer.ok() {
        return None;
    }
    let len = writer.len();
    send_packet(client, transport, len)
}

/// Закрыть канал: код завершения, конец вывода, закрытие.
///
/// Порядок обязателен. `exit-status` после `close` клиент уже не прочитает, и
/// команда, отработавшая с ошибкой, выглядела бы у него успешной — а `ssh` в
/// чужом скрипте проверяет именно код.
fn finish_channel(
    client: i64,
    transport: &mut Transport,
    channel: &Channel,
    status: u32,
) -> Option<()> {
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_CHANNEL_REQUEST);
    writer.u32(channel.peer);
    writer.string(b"exit-status");
    // Ответа не ждут: на `exit-status` его не бывает.
    writer.byte(0);
    writer.u32(status);
    if !writer.ok() {
        return None;
    }
    let len = writer.len();
    send_packet(client, transport, len)?;

    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_CHANNEL_EOF);
    writer.u32(channel.peer);
    let len = writer.len();
    send_packet(client, transport, len)?;

    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_CHANNEL_CLOSE);
    writer.u32(channel.peer);
    let len = writer.len();
    send_packet(client, transport, len)?;

    error("sshd: session closed with status ");
    error_num(i64::from(status));
    error("\n");
    Some(())
}

/// Выполнить команду `exec` и вернуть её код завершения.
fn run_in_channel(
    client: i64,
    transport: &mut Transport,
    channel: &mut Channel,
    user: &User,
    command: &[u8],
) -> u32 {
    let mut sink = Sink::new(client, transport, channel);
    let status = run_command(trim(command), user, &mut sink);
    sink.finish(channel);
    status
}

/// Приглашение построчного сеанса.
const PROMPT: &str = "freeos-ssh> ";

// --- вывод в канал -----------------------------------------------------------

/// Куда команда пишет свой вывод.
///
/// Копит в буфере и отправляет пакетами: канал — это не файл, у каждой отправки
/// есть цена (шифрование, подпись, системный вызов), и печать по полю на пакет
/// превратила бы `ls` в сотню пакетов.
struct Sink<'a> {
    client: i64,
    transport: &'a mut Transport,
    peer: u32,
    /// Сколько ещё разрешено отправить. Копия окна канала: она меняется по ходу
    /// и возвращается на место в [`Sink::finish`].
    window: u32,
    max_packet: u32,
    staging: [u8; STAGE],
    len: usize,
    /// 0 — обычный вывод, [`ssh::EXTENDED_DATA_STDERR`] — поток ошибок.
    stream: u32,
    broken: bool,
    truncated: bool,
}

impl<'a> Sink<'a> {
    fn new(client: i64, transport: &'a mut Transport, channel: &Channel) -> Self {
        Self {
            client,
            transport,
            peer: channel.peer,
            window: channel.window,
            max_packet: channel.max_packet,
            staging: [0u8; STAGE],
            len: 0,
            stream: 0,
            broken: false,
            truncated: false,
        }
    }

    /// Дописать вывод и вернуть каналу его окно.
    fn finish(mut self, channel: &mut Channel) {
        self.flush();
        if self.truncated {
            error("sshd: the client's window filled up; output was cut\n");
        }
        channel.window = self.window;
    }

    fn out(&mut self, text: &str) {
        self.switch(0);
        self.push(text.as_bytes());
    }

    fn out_bytes(&mut self, data: &[u8]) {
        self.switch(0);
        self.push(data);
    }

    fn err(&mut self, text: &str) {
        self.switch(ssh::EXTENDED_DATA_STDERR);
        self.push(text.as_bytes());
    }

    fn err_bytes(&mut self, data: &[u8]) {
        self.switch(ssh::EXTENDED_DATA_STDERR);
        self.push(data);
    }

    /// Число в текущий поток.
    fn num(&mut self, value: u64) {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        let mut rest = value;
        loop {
            at -= 1;
            digits[at] = b'0' + (rest % 10) as u8;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        self.push(&digits[at..]);
    }

    /// Права восьмеричной записью, как их показывает `ls`.
    fn octal(&mut self, value: u32) {
        let mut digits = [b'0'; 11];
        let mut at = digits.len();
        let mut rest = value;
        loop {
            at -= 1;
            digits[at] = b'0' + (rest % 8) as u8;
            rest /= 8;
            if rest == 0 {
                break;
            }
        }
        let start = at.min(digits.len() - 4);
        self.push(&digits[start..]);
    }

    /// Сменить поток. Смена — это граница пакета: смешивать в одном пакете
    /// вывод и ошибки нечем, у них разные типы сообщений.
    fn switch(&mut self, stream: u32) {
        if self.stream != stream {
            self.flush();
            self.stream = stream;
        }
    }

    fn push(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.len == STAGE {
                self.flush();
                if self.broken {
                    return;
                }
            }
            let take = (STAGE - self.len).min(data.len());
            self.staging[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
        }
    }

    fn flush(&mut self) {
        if self.len == 0 {
            return;
        }
        if self.broken {
            self.len = 0;
            return;
        }

        let mut at = 0usize;
        while at < self.len {
            // Порция не должна превысить ни окна собеседника, ни объявленного
            // им предела пакета, ни нашего буфера. Заголовок канала — тринадцать
            // байт с запасом; отнимаем их, а не подгоняем впритык.
            let room = (self.len - at)
                .min(self.window as usize)
                .min(self.max_packet.saturating_sub(64) as usize)
                .min(BUFFER - 64);
            if room == 0 {
                // Окно кончилось. Ждать прибавки посреди вывода нечем: пакеты
                // читает тот же цикл, который сейчас нас вызвал. Вывод
                // обрывается, и об этом говорится вслух.
                self.truncated = true;
                break;
            }

            let len = {
                let out = unsafe { &mut *(&raw mut OUTPUT) };
                let mut writer = Writer::new(out);
                if self.stream == 0 {
                    writer.byte(ssh::MSG_CHANNEL_DATA);
                    writer.u32(self.peer);
                } else {
                    writer.byte(ssh::MSG_CHANNEL_EXTENDED_DATA);
                    writer.u32(self.peer);
                    writer.u32(self.stream);
                }
                writer.string(&self.staging[at..at + room]);
                if !writer.ok() {
                    self.broken = true;
                    break;
                }
                writer.len()
            };
            if send_packet(self.client, self.transport, len).is_none() {
                self.broken = true;
                break;
            }
            self.window -= room as u32;
            at += room;
        }
        self.len = 0;
    }
}

// --- команды -----------------------------------------------------------------

/// Выполнить одну команду сеанса. Возвращает код завершения.
///
/// Разбор строки простейший: слова через пробел, без кавычек и без подстановок.
/// Это не оболочка и не притворяется ею — кавычки без оболочки создали бы
/// впечатление, что здесь есть и всё остальное.
fn run_command(line: &[u8], user: &User, sink: &mut Sink<'_>) -> u32 {
    let (word, rest) = split_word(line);
    match word {
        b"" => 0,
        b"help" => {
            help(sink);
            0
        }
        b"whoami" => {
            sink.out(user.name());
            sink.out("\n");
            0
        }
        b"id" => {
            sink.out("uid=");
            sink.num(u64::from(user.uid));
            sink.out("(");
            sink.out(user.name());
            sink.out(") gid=");
            sink.num(u64::from(user.gid));
            sink.out(" home=");
            sink.out(user.home());
            sink.out("\n");
            0
        }
        b"uptime" => {
            sink.out("up ");
            sink.num(uptime_ms());
            sink.out(" ms\n");
            0
        }
        b"echo" => {
            sink.out_bytes(rest);
            sink.out("\n");
            0
        }
        b"ls" => list(rest, user, sink),
        b"cat" => concatenate(rest, user, sink),
        b"exit" | b"logout" => 0,
        other => {
            sink.err("sshd: no such command: ");
            sink.err_bytes(other);
            sink.err("\ntry 'help'\n");
            // Тот же код, которым отвечает всякий Unix на ненайденную команду.
            127
        }
    }
}

fn help(sink: &mut Sink<'_>) {
    sink.out(
        "These commands run inside sshd itself, as root, with this account's\n\
         permissions checked on every path. Programs in /bin are not reachable\n\
         over the network yet: that needs pipes, and pipes are a phase of their own.\n\
         \n  help            this list\
         \n  whoami          the account this session belongs to\
         \n  id              its uid, gid and home directory\
         \n  uptime          milliseconds since the machine started\
         \n  echo <text>     the text back\
         \n  ls [path]       a directory, home by default\
         \n  cat <path>      a file, if this account may read it\
         \n  exit            end the session\n",
    );
}

/// `ls`: перечислить каталог.
fn list(argument: &[u8], user: &User, sink: &mut Sink<'_>) -> u32 {
    let mut path = Path::new();
    if !resolve(argument, user, &mut path) {
        sink.err("sshd: bad path\n");
        return 2;
    }
    if let Err(denial) = check_path(path.as_str(), user, WANT_READ) {
        return complain(sink, denial, path.as_str());
    }

    let fd = open(path.as_str());
    if fd < 0 {
        sink.err("sshd: cannot open ");
        sink.err(path.as_str());
        sink.err("\n");
        return 1;
    }

    let mut entry = Dirent::default();
    let mut files = 0u64;
    let mut directories = 0u64;
    // Столбцы те же, что у `/bin/ls`: права, владелец, размер, имя. Совпадение
    // не эстетическое — два вывода одного и того же, расходящиеся в мелочах,
    // заставляют читателя гадать, какой он видит.
    while readdir(fd, &mut entry) {
        let Some(name) = entry.name() else {
            continue;
        };
        sink.out("  ");
        sink.octal(entry.mode);
        sink.out(" ");
        sink.num(u64::from(entry.uid));
        sink.out(":");
        sink.num(u64::from(entry.gid));
        sink.out(" ");
        sink.num(entry.size);
        sink.out("  ");
        sink.out(name);
        if entry.kind == KIND_DIRECTORY {
            sink.out("/");
            directories += 1;
        } else {
            files += 1;
        }
        sink.out("\n");
    }
    close(fd);

    sink.out("ls: ");
    sink.num(files);
    sink.out(" files, ");
    sink.num(directories);
    sink.out(" directories in ");
    sink.out(path.as_str());
    sink.out("\n");
    0
}

/// `cat`: отдать файл.
fn concatenate(argument: &[u8], user: &User, sink: &mut Sink<'_>) -> u32 {
    if argument.is_empty() {
        sink.err("sshd: cat needs a path\n");
        return 2;
    }
    let mut path = Path::new();
    if !resolve(argument, user, &mut path) {
        sink.err("sshd: bad path\n");
        return 2;
    }
    if let Err(denial) = check_path(path.as_str(), user, WANT_READ) {
        return complain(sink, denial, path.as_str());
    }

    let mut meta = Stat::default();
    if stat(path.as_str(), &mut meta) < 0 {
        return complain(sink, Denial::Missing, path.as_str());
    }
    if meta.kind != KIND_FILE {
        sink.err("sshd: ");
        sink.err(path.as_str());
        sink.err(" is not a file\n");
        return 1;
    }

    let fd = open(path.as_str());
    if fd < 0 {
        sink.err("sshd: cannot open ");
        sink.err(path.as_str());
        sink.err("\n");
        return 1;
    }
    // Порциями, а не целиком: файл может быть каким угодно, а памяти у
    // программы — окно и стек.
    let mut buffer = [0u8; 512];
    loop {
        let got = read(fd, &mut buffer);
        if got <= 0 {
            break;
        }
        sink.out_bytes(&buffer[..got as usize]);
    }
    close(fd);
    0
}

/// Сказать в канал, почему не вышло.
fn complain(sink: &mut Sink<'_>, denial: Denial, path: &str) -> u32 {
    match denial {
        Denial::Missing => sink.err("sshd: no such file: "),
        // Отказ назван отказом, а не «файла нет». Прятать разницу имеет смысл
        // там, где имя файла — секрет; здесь спрашивает тот, кого мы уже
        // впустили, и вводить его в заблуждение незачем.
        Denial::Forbidden => sink.err("sshd: permission denied: "),
    }
    sink.err(path);
    sink.err("\n");
    1
}

/// Достроить путь: пустой — домашний каталог, относительный — от него же.
///
/// Текущего каталога в системе нет вовсе (см. `/bin/ls`), поэтому «от чего
/// считать относительный путь» — решение сеанса. Домашний каталог выбран
/// потому, что именно туда попадает человек, вошедший по SSH в любую систему.
fn resolve(argument: &[u8], user: &User, path: &mut Path) -> bool {
    let Ok(text) = core::str::from_utf8(argument) else {
        return false;
    };
    let text = text.trim();
    if text.is_empty() {
        return path.push(user.home());
    }
    if text.starts_with('/') {
        return path.push(text);
    }
    path.push(user.home()) && path.join(text)
}

// --- права -------------------------------------------------------------------

/// Право прочитать: тот же бит, что в ext2.
const WANT_READ: u32 = 0b100;
/// Право пройти сквозь каталог.
const WANT_SEARCH: u32 = 0b001;

/// Почему нельзя.
enum Denial {
    Missing,
    Forbidden,
}

/// Вправе ли вошедший добраться до этого пути и сделать с ним `want`.
///
/// Проверяется **каждое** звено пути, а не только последний файл. Иначе
/// каталог `0700` перестал бы быть непроницаемым: файл `0644` внутри него виден
/// по правам самого файла, и разница между «права файла разрешают» и «до файла
/// не дойти» — это ровно то, ради чего установщик кладёт `/root/notes.txt`.
///
/// Между проверкой и открытием файл, вообще говоря, может смениться. В этой
/// системе смена имени требует прав на каталог — то есть того же, что здесь
/// проверяется, — а настоящий ответ на такую гонку один: открывать от имени
/// пользователя, а не проверять за него. Это и появится вместе с каналами.
fn check_path(path: &str, user: &User, want: u32) -> Result<(), Denial> {
    let mut meta = Stat::default();
    if user.uid == 0 {
        // Суперпользователю разрешено всё; проверить остаётся только, что файл
        // существует.
        return if stat(path, &mut meta) < 0 {
            Err(Denial::Missing)
        } else {
            Ok(())
        };
    }

    // Корень дерева: он общий для всех путей, и без права пройти сквозь него не
    // виден ни один файл.
    if stat("/", &mut meta) >= 0 && !allows(&meta, user, WANT_SEARCH) {
        return Err(Denial::Forbidden);
    }

    let mut walked = Path::new();
    if !walked.push("/") {
        return Err(Denial::Missing);
    }
    let mut components = path.split('/').filter(|part| !part.is_empty()).peekable();
    while let Some(component) = components.next() {
        if !walked.join(component) {
            return Err(Denial::Missing);
        }
        if stat(walked.as_str(), &mut meta) < 0 {
            return Err(Denial::Missing);
        }
        let last = components.peek().is_none();
        let needed = if last { want } else { WANT_SEARCH };
        if !allows(&meta, user, needed) {
            return Err(Denial::Forbidden);
        }
    }
    Ok(())
}

/// Разрешает ли режим этому пользователю то, что он хочет.
///
/// Класс выбирается **первым совпавшим**, а не объединением — так устроен Unix
/// и так устроена проверка в ядре (`vfs::perm`). Разойтись с ней здесь значило
/// бы пускать по сети туда, куда не пускают за терминалом, или наоборот.
fn allows(meta: &Stat, user: &User, want: u32) -> bool {
    if user.uid == 0 {
        return true;
    }
    let shift = if user.uid == meta.uid {
        6
    } else if user.gid == meta.gid {
        3
    } else {
        0
    };
    (meta.mode >> shift) & want == want
}

// --- учётные записи ----------------------------------------------------------

/// Наибольшая длина имени, которую мы храним.
const MAX_NAME: usize = 32;
/// Наибольшая длина домашнего каталога.
const MAX_HOME: usize = 96;

/// Учётная запись, от имени которой идёт сеанс.
#[derive(Clone, Copy)]
struct User {
    name: [u8; MAX_NAME],
    name_len: usize,
    home: [u8; MAX_HOME],
    home_len: usize,
    uid: u32,
    gid: u32,
}

impl User {
    fn name(&self) -> &str {
        // SAFETY: при разборе принимаются только печатные ASCII-байты.
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len]) }
    }

    fn home(&self) -> &str {
        // SAFETY: то же.
        unsafe { core::str::from_utf8_unchecked(&self.home[..self.home_len]) }
    }
}

/// Найти учётную запись по имени.
///
/// Формат строки — `name:uid:gid:mode:home:algorithm:salt:digest`, тот же, что
/// пишет установщик и читает ядро. Поля про пароль не читаются вовсе: пароль
/// здесь не проверяется никогда.
fn find_user(name: &[u8]) -> Option<User> {
    if name.is_empty() || name.len() > MAX_NAME {
        return None;
    }
    let len = read_file(PASSWD_PATH)?;
    // SAFETY: буфер статический, программа однопоточная, и `find_user` не
    // вызывается изнутри работы с ним.
    let text = unsafe { &(&raw const FILE_BUFFER).as_ref().unwrap()[..len] };

    for line in text.split(|byte| *byte == b'\n') {
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let mut fields = line.split(|byte| *byte == b':');
        let field_name = fields.next()?;
        if field_name != name {
            continue;
        }
        let uid = parse_u32(fields.next()?)?;
        let gid = parse_u32(fields.next()?)?;
        let _mode = fields.next()?;
        let home = fields.next()?;

        // Учётная запись с нулевым uid по сети не пускается: `root` не входит
        // снаружи, и это не наша строгость, а общее правило для всякого сервера
        // SSH. Заодно оно означает, что никакая испорченная строка не даст
        // сеансу прав суперпользователя.
        if uid == 0 {
            error("sshd: refusing a root account from ");
            error(PASSWD_PATH);
            error("\n");
            return None;
        }
        if home.is_empty() || home[0] != b'/' || home.len() > MAX_HOME {
            return None;
        }
        if !field_name.iter().all(|byte| byte.is_ascii_graphic())
            || !home.iter().all(|byte| byte.is_ascii_graphic())
        {
            return None;
        }

        let mut user = User {
            name: [0u8; MAX_NAME],
            name_len: field_name.len(),
            home: [0u8; MAX_HOME],
            home_len: home.len(),
            uid,
            gid,
        };
        user.name[..field_name.len()].copy_from_slice(field_name);
        user.home[..home.len()].copy_from_slice(home);
        return Some(user);
    }
    None
}

/// Записан ли ключ в `authorized_keys` этого пользователя.
///
/// Проверяется не только содержимое файла, но и то, кому он принадлежит и кто
/// вправе в него писать, — так же, как это делает OpenSSH (`StrictModes`).
/// Ключ, лежащий в каталоге, открытом на запись посторонним, — это разрешение,
/// выданное не владельцем.
fn key_authorized(user: &User, key: &[u8; 32]) -> bool {
    let mut path = Path::new();
    if !path.push(user.home()) {
        return false;
    }
    if !trustworthy(path.as_str(), user, KIND_DIRECTORY) {
        return false;
    }
    if !path.join(".ssh") {
        return false;
    }
    if !trustworthy(path.as_str(), user, KIND_DIRECTORY) {
        return false;
    }
    if !path.join("authorized_keys") {
        return false;
    }
    if !trustworthy(path.as_str(), user, KIND_FILE) {
        return false;
    }

    let Some(len) = read_file(path.as_str()) else {
        error("sshd: cannot read ");
        error(path.as_str());
        error("\n");
        return false;
    };
    // SAFETY: буфер статический, программа однопоточная; учётная запись уже
    // скопирована из него в `User`.
    let file = unsafe { &(&raw const FILE_BUFFER).as_ref().unwrap()[..len] };
    if auth::authorized(file, key) {
        return true;
    }
    error("sshd: this key is not in ");
    error(path.as_str());
    error("\n");
    false
}

/// Годится ли узел на пути к ключам: тот ли это тип, тот ли владелец, не открыт
/// ли он на запись посторонним.
fn trustworthy(path: &str, user: &User, kind: u32) -> bool {
    let mut meta = Stat::default();
    if stat(path, &mut meta) < 0 {
        error("sshd: no ");
        error(path);
        error("\n");
        return false;
    }
    if meta.kind != kind {
        error("sshd: ");
        error(path);
        error(" is not what it should be\n");
        return false;
    }
    if meta.uid != 0 && meta.uid != user.uid {
        error("sshd: ");
        error(path);
        error(" belongs to somebody else; refusing to trust it\n");
        return false;
    }
    if meta.mode & 0o022 != 0 {
        error("sshd: ");
        error(path);
        error(" is writable by others; refusing to trust it\n");
        return false;
    }
    true
}

// --- мелочи ------------------------------------------------------------------

/// Прочитать файл целиком в статический буфер. Возвращает длину.
fn read_file(path: &str) -> Option<usize> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    // SAFETY: буфер статический, программа однопоточная.
    let buffer = unsafe { &mut *(&raw mut FILE_BUFFER) };
    let mut filled = 0usize;
    loop {
        if filled == buffer.len() {
            // Файл длиннее буфера. Прочитанный кусок использовать нельзя: он
            // может обрываться посреди строки ключа, и такой ключ не совпадёт
            // ни с чем — то есть отказ будет выглядеть как чужой ключ.
            close(fd);
            error("sshd: ");
            error(path);
            error(" is too big to read\n");
            return None;
        }
        let got = read(fd, &mut buffer[filled..]);
        if got <= 0 {
            break;
        }
        filled += got as usize;
    }
    close(fd);
    Some(filled)
}

/// Разобрать десятичное число.
fn parse_u32(text: &[u8]) -> Option<u32> {
    if text.is_empty() {
        return None;
    }
    let mut value = 0u32;
    for byte in text {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    Some(value)
}

/// Отрезать пробелы с обоих концов.
fn trim(text: &[u8]) -> &[u8] {
    let start = text
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    &text[start..end]
}

/// Разбить строку на первое слово и остаток.
fn split_word(line: &[u8]) -> (&[u8], &[u8]) {
    let line = trim(line);
    match line.iter().position(|byte| *byte == b' ' || *byte == b'\t') {
        Some(at) => (&line[..at], trim(&line[at..])),
        None => (line, &line[line.len()..]),
    }
}

/// Написать в журнал то, что пришло с провода.
///
/// Байты недоверенные, поэтому не-UTF-8 не печатается вовсе: управляющая
/// последовательность, пришедшая снаружи и попавшая в терминал человека, —
/// это уже не диагностика.
fn error_bytes(data: &[u8]) {
    match core::str::from_utf8(data) {
        Ok(text) if text.bytes().all(|byte| byte.is_ascii_graphic() || byte == b' ') => {
            error(text);
        }
        _ => error("<unprintable>"),
    }
}

/// Отправить сообщение о разрыве и закрыть свою половину.
fn disconnect(client: i64, transport: &mut Transport, reason: u32, text: &str) {
    let out = unsafe { &mut *(&raw mut OUTPUT) };
    let mut writer = Writer::new(out);
    writer.byte(ssh::MSG_DISCONNECT);
    writer.u32(reason);
    writer.string(text.as_bytes());
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
fn read_packet<'a>(client: i64, transport: &mut Transport, deadline: u64) -> Option<&'a [u8]> {
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

                // Тип пакета — в журнал, но только до шифрования. Рукопожатие
                // SSH это разговор из шести сообщений, и когда одна сторона
                // замолкает, единственный вопрос — на каком именно. В сеансе же
                // таких строк были бы сотни, и полезной среди них ни одной.
                if !transport.is_encrypted() {
                    error("sshd: packet type ");
                    error_num(i64::from(payload_buffer[0]));
                    error("\n");
                }

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
