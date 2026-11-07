//! `rstd` — проверка `std` Rust на FreeOS (N2 браузера на Servo).
//!
//! Обычная программа на `std`, ни строчки под нашу ОС: если она работает,
//! работает и порт (`ports/rust-std`). Каждая проверка печатает строку
//! `rstd: ok …`, провал — `rstd: FAILED …`; сценарий `rust-std` ждёт их.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

static FAILED: AtomicUsize = AtomicUsize::new(0);

fn check(name: &str, ok: bool) {
    if ok {
        println!("rstd: ok {name}");
    } else {
        FAILED.fetch_add(1, Ordering::SeqCst);
        println!("rstd: FAILED {name}");
    }
}

fn main() {
    println!("rstd: std on {} {}", std::env::consts::OS, std::env::consts::ARCH);

    // Аргументы и окружение.
    let args: Vec<String> = std::env::args().collect();
    check("args come from argv", !args.is_empty());

    // Куча: Vec, String, HashMap (HashMap берёт случайный ключ — getentropy).
    let mut map = std::collections::HashMap::new();
    for i in 0..1000 {
        map.insert(i, format!("value {i}"));
    }
    check("heap and HashMap with random keys", map.len() == 1000 && map[&999] == "value 999");

    // Потоки: Mutex считает точно из четырёх потоков.
    let counter = Arc::new(Mutex::new(0u64));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let c = counter.clone();
            std::thread::spawn(move || {
                for _ in 0..10_000 {
                    *c.lock().unwrap() += 1;
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    check("mutex keeps a counter exact across 4 threads", *counter.lock().unwrap() == 40_000);

    // Condvar передаёт очередь от производителя потребителю.
    let pair = Arc::new((Mutex::new(Vec::<u32>::new()), Condvar::new()));
    let producer = {
        let p = pair.clone();
        std::thread::spawn(move || {
            for i in 0..100 {
                p.0.lock().unwrap().push(i);
                p.1.notify_one();
            }
        })
    };
    let mut got = 0u32;
    let mut sum = 0u32;
    while got < 100 {
        let mut q = pair.1.wait_while(pair.0.lock().unwrap(), |q| q.is_empty()).unwrap();
        for v in q.drain(..) {
            sum += v;
            got += 1;
        }
    }
    producer.join().unwrap();
    check("condvar carries a queue", sum == 4950);

    // Ожидание со сроком кончается по сроку.
    let lock = Mutex::new(());
    let cv = Condvar::new();
    let t0 = Instant::now();
    let (_g, res) = cv.wait_timeout(lock.lock().unwrap(), Duration::from_millis(100)).unwrap();
    let waited = t0.elapsed();
    check("timed wait ends at its deadline", res.timed_out() && waited >= Duration::from_millis(90));

    // RwLock и thread_local.
    let rw = RwLock::new(5);
    {
        let a = rw.read().unwrap();
        let b = rw.read().unwrap();
        check("rwlock lets two readers in", *a + *b == 10);
    }
    *rw.write().unwrap() = 7;
    check("rwlock writer", *rw.read().unwrap() == 7);
    thread_local!(static LOCAL: std::cell::Cell<u32> = const { std::cell::Cell::new(1) });
    LOCAL.with(|v| v.set(42));
    let other = std::thread::spawn(|| LOCAL.with(|v| v.get())).join().unwrap();
    check("thread_local is per thread", other == 1 && LOCAL.with(|v| v.get()) == 42);

    // Время.
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_millis(50));
    let slept = t0.elapsed();
    check("sleep and Instant", slept >= Duration::from_millis(45) && slept < Duration::from_secs(5));
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    check("SystemTime after 1970", now.is_ok());

    // Файлы на чтение — есть везде: сама программа лежит в /bin.
    let me = std::path::Path::new("/bin/rstd");
    let meta = std::fs::metadata(me);
    check("fs metadata of /bin/rstd", meta.as_ref().map(|m| m.is_file() && m.len() > 4096).unwrap_or(false));
    let mut head = [0u8; 4];
    let read = std::fs::File::open(me).and_then(|mut f| f.read_exact(&mut head));
    check("fs open and read an ELF header", read.is_ok() && head == *b"\x7fELF");
    let listed = std::fs::read_dir("/bin")
        .map(|it| it.filter_map(|e| e.ok()).any(|e| e.file_name() == "rstd"))
        .unwrap_or(false);
    check("fs read_dir of /bin sees rstd", listed);

    // Запись — где получится. Корень живой системы только для чтения (ядро
    // отвечает ERR_UNSUPPORTED → ENOSYS), там проверка пропускается; на
    // установленной системе пишется в дом.
    let candidates = [std::env::var_os("HOME").map(Into::into).unwrap_or_default(), std::env::temp_dir()];
    let mut dir = None;
    for base in candidates.iter().filter(|b| !b.as_os_str().is_empty()) {
        let d = base.join("rstd");
        match std::fs::create_dir_all(&d) {
            Ok(()) => {
                dir = Some(d);
                break;
            }
            Err(e) => println!("rstd: note: mkdir {} failed: {e}", d.display()),
        }
    }
    match dir {
        None => println!("rstd: skipped fs write: no writable directory (read-only root)"),
        Some(dir) => {
            let file = dir.join("hello.txt");
            let wrote = std::fs::write(&file, b"hello from std");
            if let Err(e) = &wrote {
                println!("rstd: note: write {} failed: {e}", file.display());
            }
            let back = std::fs::read_to_string(&file).unwrap_or_else(|e| format!("<{e}>"));
            check("fs write and read back", wrote.is_ok() && back == "hello from std");
            let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
            check("fs metadata size of a new file", size == 14);
            let listed = std::fs::read_dir(&dir)
                .map(|it| it.filter_map(|e| e.ok()).any(|e| e.file_name() == "hello.txt"))
                .unwrap_or(false);
            check("fs read_dir sees the new file", listed);
            check("fs remove", std::fs::remove_file(&file).is_ok() && !file.exists());
            let _ = std::fs::remove_dir(&dir);
        }
    }

    // Сеть: TCP внутри одного процесса — слушаем «любой» адрес, стучимся на
    // 127.0.0.1, а нет петли — на свой адрес (его даёт UDP-«соединение»).
    let own_ip = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("10.0.2.2:9").map(|()| s))
        .and_then(|s| s.local_addr())
        .map(|a| a.ip());
    println!("rstd: own address {own_ip:?}");
    match TcpListener::bind("0.0.0.0:0") {
        Ok(listener) => {
            let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
            println!("rstd: listening on port {port}");
            // Сервер отвечает эхом каждому, кто достучался.
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else { continue };
                    let mut buf = [0u8; 5];
                    if s.read_exact(&mut buf).is_ok() {
                        let _ = s.write_all(&buf);
                    }
                }
            });
            let mut targets = vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))];
            if let Ok(ip) = own_ip {
                targets.push(std::net::SocketAddr::new(ip, port));
            }
            let mut ok = false;
            for addr in targets {
                let echoed = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).and_then(|mut c| {
                    c.set_read_timeout(Some(Duration::from_secs(5)))?;
                    c.write_all(b"ping!")?;
                    let mut buf = [0u8; 5];
                    c.read_exact(&mut buf)?;
                    Ok(buf)
                });
                match echoed {
                    Ok(b) if &b == b"ping!" => {
                        println!("rstd: echo via {addr}");
                        ok = true;
                        break;
                    }
                    Ok(b) => println!("rstd: note: {addr} echoed {b:?}"),
                    Err(e) => println!("rstd: note: {addr}: {e}"),
                }
            }
            check("tcp echo inside one process", ok);
        }
        Err(e) => check(&format!("tcp bind ({e})"), false),
    }
    let local = TcpListener::bind("127.0.0.1:0");
    if let Err(e) = &local {
        println!("rstd: note: bind 127.0.0.1: {e}");
    }
    check("tcp bind on 127.0.0.1", local.is_ok());

    let failed = FAILED.load(Ordering::SeqCst);
    println!("rstd: done, {failed} check(s) failed");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
