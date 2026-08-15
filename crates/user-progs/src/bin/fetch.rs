//! `fetch` — забрать файл по адресу.
//!
//! ```text
//!   fetch <адрес>            напечатать содержимое
//!   fetch <адрес> <файл>     записать в файл
//!   fetch <адрес> -          сосчитать байты и выбросить их
//! ```
//!
//! Третья форма нужна ровно затем, зачем и вся программа: спросить «достаёт ли
//! эта машина до того сервера и с какой скоростью», не занимая места на диске и
//! не заливая экран содержимым.
//!
//! # Зачем она есть
//!
//! Затем, что до фазы 39a единственным, кто ходил в сеть за файлом, был
//! `sysupdate`, и проверить сеть отдельно от обновления было нечем. Теперь
//! HTTPS есть, и вопрос «а достаёт ли эта машина до того сервера» обязан иметь
//! ответ короче, чем «попробуйте обновиться».
//!
//! Заодно это единственное место, где человек видит работу TLS своими глазами:
//! адрес на `https://`, строка про проверенную цепочку и число байт.
//!
//! # Чему эта программа **не** доверяет
//!
//! Ничему, и это стоит сказать прямо. Она проверяет цепочку сертификатов — имя,
//! сроки, подписи, корень из `ca.pem`, — но проверенный канал не делает
//! содержимое годным. Файл, приехавший сюда, — это просто байты с чужой машины.
//! Всё, что система ставит себе, проверяется подписью Ed25519 отдельно и не
//! этой программой.

#![no_std]
#![no_main]

use user_progs::{
    Args, close, config_path, error, exit, http, open, print, print_u64, println, read, time_now,
    uptime_ms, write,
};

/// Имя файла с доверенными корнями. Тот же, что читает `sysupdate`.
const CA: &str = "ca.pem";

/// Буфер под расшифрованное содержимое.
static mut BODY: [u8; 8 * 1024] = [0; 8 * 1024];

/// Буфер под байты с провода.
static mut WIRE: [u8; 4 * 1024] = [0; 4 * 1024];

/// Буферы соединения TLS.
static mut TLS_IO: tls::Buffers = tls::Buffers::new();

/// Текст `ca.pem` и он же, разобранный в DER.
static mut CA_TEXT: [u8; 16 * 1024] = [0; 16 * 1024];
static mut CA_DER: [u8; 12 * 1024] = [0; 12 * 1024];

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: пара (argc, argv) приходит от ядра, которое собрало её из
    // командной строки и держит живой всё время работы программы.
    let args = unsafe { Args::new(argc, argv) };
    let Some(url) = args.get(1) else {
        println("usage: fetch <url> [file|-]");
        println("  '-' counts the bytes and throws them away");
        println("  http:// and https:// ; the roots come from /etc/ca.pem");
        exit(2);
    };
    exit(fetch(url, args.get(2)));
}

fn fetch(url: &str, target: Option<&str>) -> i64 {
    let mut trust = roots().map(|roots| {
        // SAFETY: статик принадлежит этой задаче: программа однопоточная, а до
        // второй копии себя ей не дотянуться.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(TLS_IO) };
        let now = time_now() as i64;
        if now == 0 {
            println("fetch: this system does not know the date; certificate dates unchecked");
        }
        http::Trust { buffers, roots, now }
    });
    if trust.is_none() && url.starts_with("https://") {
        println("fetch: no ca.pem in /etc nor in the image defaults; https is not available");
        return 1;
    }

    // Куда складывать: в файл или на экран. Файл открывается **до** первого
    // байта из сети, чтобы «некуда писать» выяснилось до загрузки, а не после.
    let fd = match target {
        // Чёрточка — «посчитать и выбросить». Отдельно от «печатать на экран»:
        // полмегабайта в серийную линию — это не проверка сети, а полчаса
        // вывода.
        Some("-") => None,
        Some(path) => {
            let fd = user_progs::open_write(path, true, true);
            if fd < 0 {
                error("fetch: cannot write ");
                error(path);
                error("\n");
                return 1;
            }
            Some(fd)
        }
        None => None,
    };

    let discard = target == Some("-");
    let started = uptime_ms();
    let mut total = 0u64;
    let mut failed = false;

    // SAFETY: статики принадлежат этой задаче, см. пояснение выше.
    let wire = unsafe { &mut *core::ptr::addr_of_mut!(WIRE) };
    // SAFETY: то же самое.
    let body = unsafe { &mut *core::ptr::addr_of_mut!(BODY) };
    let mut io = http::Buffers { wire, body };

    let result = http::get(url, trust.as_mut(), &mut io, &mut |chunk: &[u8]| {
        total += chunk.len() as u64;
        match fd {
            Some(fd) => {
                let mut done = 0usize;
                while done < chunk.len() {
                    let wrote = write(fd, &chunk[done..]);
                    if wrote <= 0 {
                        failed = true;
                        return false;
                    }
                    done += wrote as usize;
                }
            }
            // На экран отдаётся как есть: `fetch` без второго аргумента — это
            // «покажи, что там», и текст индекса читается глазами.
            None if !discard => {
                if let Ok(text) = core::str::from_utf8(chunk) {
                    print(text);
                } else {
                    print("<binary>");
                }
            }
            None => {}
        }
        true
    });

    if let Some(fd) = fd {
        close(fd);
    }

    match result {
        Ok(_) => {
            let elapsed = uptime_ms().saturating_sub(started).max(1);
            print("fetch: ");
            print_u64(total);
            print(" bytes in ");
            print_u64(elapsed);
            print(" ms (");
            print_u64(total * 1000 / elapsed / 1024);
            println(" KiB/s)");
            0
        }
        Err(err) => {
            if failed {
                println("fetch: writing failed; is there room left?");
            } else {
                error("fetch: ");
                error(err.text());
                // Число там, где оно есть: «ответ кончился раньше» без пары
                // «сколько приехало из скольких» отправляет искать обрыв связи,
                // а причина бывает совсем в другом.
                if let http::Error::Short { got, want } = err {
                    error(" (");
                    user_progs::error_num(got as i64);
                    error(" of ");
                    user_progs::error_num(want as i64);
                    error(" bytes)");
                }
                if let http::Error::Status(code) = err {
                    error(" (");
                    user_progs::error_num(i64::from(code));
                    error(")");
                }
                error("\n");
            }
            1
        }
    }
}

/// Прочитать `ca.pem` и построить хранилище корней.
fn roots() -> Option<x509::Store<'static>> {
    let path = config_path(CA)?;
    let fd = open(path.as_str());
    if fd < 0 {
        return None;
    }
    // SAFETY: статик принадлежит этой задаче, см. пояснение выше.
    let text_buffer = unsafe { &mut *core::ptr::addr_of_mut!(CA_TEXT) };
    let got = read(fd, text_buffer);
    close(fd);
    let text = core::str::from_utf8(&text_buffer[..got.max(0) as usize]).ok()?;
    // SAFETY: то же самое.
    let der = unsafe { &mut *core::ptr::addr_of_mut!(CA_DER) };
    match x509::Store::parse_pem(text, der) {
        Ok(store) if !store.is_empty() => Some(store),
        _ => None,
    }
}
