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
    // Первый аргумент — каталог для записи (`run /bin/rstd /home/roman`).
    let candidates = [
        args.get(1).map(Into::into).unwrap_or_default(),
        std::env::var_os("HOME").map(Into::into).unwrap_or_default(),
        std::env::temp_dir(),
    ];
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

    poll_checks();
    stress_sync();

    let failed = FAILED.load(Ordering::SeqCst);
    println!("rstd: done, {failed} check(s) failed");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

/// poll(): то, на чём стоит `mio` (и с ним tokio) — пробуждение записью в
/// pipe из другого потока, срок ожидания, готовность принятого сокета.
fn poll_checks() {
    use std::os::fd::AsRawFd;
    fn poll1(fd: i32, ms: i32) -> (i32, i16, Duration) {
        let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let t0 = Instant::now();
        let n = unsafe { libc::poll(&mut p, 1, ms) };
        (n, p.revents, t0.elapsed())
    }

    let mut fds = [0i32; 2];
    let ok = unsafe { libc::pipe(fds.as_mut_ptr()) } == 0;
    check("pipe()", ok);
    if ok {
        let (n, _, took) = poll1(fds[0], 300);
        println!("rstd: poll empty pipe -> {n} after {took:?}");
        check("poll times out on an empty pipe", n == 0 && took >= Duration::from_millis(250) && took < Duration::from_secs(3));
        let w = fds[1];
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            unsafe { libc::write(w, b"x".as_ptr().cast(), 1) };
        });
        let (n, re, took) = poll1(fds[0], 5000);
        println!("rstd: poll pipe written by a thread -> {n} revents {re:#x} after {took:?}");
        check("poll wakes on a pipe written by another thread", n == 1 && took < Duration::from_secs(3));
        let _ = writer.join();
    }

    if let Ok(listener) = TcpListener::bind("127.0.0.1:0") {
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let client = std::thread::spawn(move || {
            let mut c = TcpStream::connect(("127.0.0.1", port)).ok()?;
            std::thread::sleep(Duration::from_millis(200));
            c.write_all(b"data").ok()?;
            std::thread::sleep(Duration::from_millis(500));
            Some(())
        });
        if let Ok((s, _)) = listener.accept() {
            let (n, re, took) = poll1(s.as_raw_fd(), 5000);
            println!("rstd: poll accepted socket -> {n} revents {re:#x} after {took:?}");
            check("poll wakes on data arriving at an accepted socket", n == 1 && took < Duration::from_secs(3));
            let _ = s.set_nonblocking(true);
            let mut buf = [0u8; 8];
            let got = (&s).read(&mut buf);
            println!("rstd: nonblocking read after poll -> {got:?}");
        }
        let _ = client.join();
    }

    nonblocking_checks();
}

/// Неблокирующие сокеты так, как их зовёт mio: socket(SOCK_NONBLOCK),
/// connect → EINPROGRESS, accept4(SOCK_NONBLOCK), read без данных → EAGAIN.
fn nonblocking_checks() {
    use std::os::fd::AsRawFd;
    let Ok(listener) = TcpListener::bind("127.0.0.1:0") else { return };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let _ = listener.set_nonblocking(true);
    let t0 = Instant::now();
    let r = unsafe { libc::accept4(listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK) };
    let e = std::io::Error::last_os_error();
    println!("rstd: accept4 on an empty queue -> {r} {e} after {:?}", t0.elapsed());
    check("nonblocking accept on an empty queue returns EAGAIN", r < 0 && e.kind() == std::io::ErrorKind::WouldBlock);

    // Как создаёт слушающий сокет mio: SOCK_NONBLOCK при создании, потом
    // bind и listen — accept на пустой очереди обязан вернуть EAGAIN.
    unsafe {
        let l = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
        let mut sa: libc::sockaddr_in = std::mem::zeroed();
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_addr.s_addr = u32::from_be_bytes([127, 0, 0, 1]).to_be();
        libc::bind(l, (&sa as *const libc::sockaddr_in).cast(), std::mem::size_of::<libc::sockaddr_in>() as u32);
        libc::listen(l, 16);
        let watchdog = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(5));
            println!("rstd: FAILED accept on a SOCK_NONBLOCK listener blocked for 5 s");
            std::process::exit(3);
        });
        let r = libc::accept4(l, std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK);
        let e = std::io::Error::last_os_error();
        drop(watchdog);
        check("accept on a SOCK_NONBLOCK listener returns EAGAIN", r < 0 && e.kind() == std::io::ErrorKind::WouldBlock);
        libc::close(l);
    }

    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
        let mut sa: libc::sockaddr_in = std::mem::zeroed();
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_port = port.to_be();
        sa.sin_addr.s_addr = u32::from_be_bytes([127, 0, 0, 1]).to_be();
        let t0 = Instant::now();
        let r = libc::connect(fd, (&sa as *const libc::sockaddr_in).cast(), std::mem::size_of::<libc::sockaddr_in>() as u32);
        let e = std::io::Error::last_os_error();
        println!("rstd: nonblocking connect -> {r} {e} after {:?}", t0.elapsed());
        check("nonblocking connect does not wait", t0.elapsed() < Duration::from_millis(500) && (r == 0 || e.raw_os_error() == Some(libc::EINPROGRESS)));
        let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        let n = libc::poll(&mut p, 1, 3000);
        println!("rstd: poll POLLOUT on connecting socket -> {n} revents {:#x}", p.revents);
        check("connecting socket becomes writable", n == 1 && p.revents & libc::POLLOUT != 0);

        std::thread::sleep(Duration::from_millis(100));
        let a = libc::accept4(listener.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC);
        println!("rstd: accept4 with a pending connection -> {a}");
        check("nonblocking accept takes the pending connection", a >= 0);
        if a >= 0 {
            let mut buf = [0u8; 8];
            let t0 = Instant::now();
            let r = libc::read(a, buf.as_mut_ptr().cast(), 8);
            let e = std::io::Error::last_os_error();
            println!("rstd: read on an accepted nonblocking socket without data -> {r} {e} after {:?}", t0.elapsed());
            check("read without data on an accepted nonblocking socket returns EAGAIN", r < 0 && e.kind() == std::io::ErrorKind::WouldBlock);
            let r = libc::recv(a, buf.as_mut_ptr().cast(), 8, 0);
            let e = std::io::Error::last_os_error();
            println!("rstd: recv on it -> {r} {e}");
            // Данные пришли и прочитаны — следующее чтение снова EAGAIN, а не сон.
            libc::write(fd, b"abcd".as_ptr().cast(), 4);
            std::thread::sleep(Duration::from_millis(100));
            let r = libc::read(a, buf.as_mut_ptr().cast(), 8);
            println!("rstd: read after data arrived -> {r}");
            let watchdog = std::thread::spawn(|| {
                std::thread::sleep(Duration::from_secs(5));
                println!("rstd: FAILED read after drained socket blocked for 5 s");
                std::process::exit(3);
            });
            let t0 = Instant::now();
            let r = libc::read(a, buf.as_mut_ptr().cast(), 8);
            let e = std::io::Error::last_os_error();
            println!("rstd: read on the drained socket -> {r} {e} after {:?}", t0.elapsed());
            check("read on a drained nonblocking socket returns EAGAIN", r < 0 && e.kind() == std::io::ErrorKind::WouldBlock);
            drop(watchdog);
            libc::close(a);
        }
        libc::close(fd);
    }
}

/// Нагрузка на Mutex/Condvar: 2 производителя, 2 потребителя, очередь в 8
/// мест, 100 000 сообщений. Потерянное пробуждение здесь — вечный сон, и его
/// ловит сторож.
fn stress_sync() {
    use std::collections::VecDeque;
    const N: u64 = 50_000;
    struct Q {
        items: Mutex<VecDeque<u64>>,
        not_empty: Condvar,
        not_full: Condvar,
    }
    let q = Arc::new(Q { items: Mutex::new(VecDeque::new()), not_empty: Condvar::new(), not_full: Condvar::new() });
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let done = done.clone();
        std::thread::spawn(move || {
            for _ in 0..60 {
                std::thread::sleep(Duration::from_secs(1));
                if done.load(Ordering::SeqCst) {
                    return;
                }
            }
            println!("rstd: FAILED stress: producers/consumers stuck for 60 s (lost wake-up)");
            std::process::exit(4);
        });
    }
    let t0 = Instant::now();
    let mut hs = Vec::new();
    for p in 0..2u64 {
        let q = q.clone();
        hs.push(std::thread::spawn(move || {
            for i in 0..N {
                let mut g = q.items.lock().unwrap();
                while g.len() >= 8 {
                    g = q.not_full.wait(g).unwrap();
                }
                g.push_back(p * N + i);
                drop(g);
                q.not_empty.notify_one();
            }
            0u64
        }));
    }
    for _ in 0..2 {
        let q = q.clone();
        hs.push(std::thread::spawn(move || {
            let mut sum = 0u64;
            for _ in 0..N {
                let mut g = q.items.lock().unwrap();
                while g.is_empty() {
                    g = q.not_empty.wait(g).unwrap();
                }
                sum += g.pop_front().unwrap();
                drop(g);
                q.not_full.notify_one();
            }
            sum
        }));
    }
    let total: u64 = hs.into_iter().map(|h| h.join().unwrap()).sum();
    done.store(true, Ordering::SeqCst);
    let want: u64 = (0..2 * N).sum();
    println!("rstd: stress took {:?}", t0.elapsed());
    check("mutex+condvar stress, 100000 messages, 4 threads", total == want);
}
