//! `rtokio` — проверка асинхронной экосистемы на FreeOS (N3): `tokio`
//! (многопоточный рантайм) поверх `mio` в режиме `poll()` и `socket2`.
//! Строки `rtokio: ok …` / `rtokio: FAILED …` ждёт сценарий `rust-std`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

static FAILED: AtomicUsize = AtomicUsize::new(0);

fn check(name: &str, ok: bool) {
    if ok {
        println!("rtokio: ok {name}");
    } else {
        FAILED.fetch_add(1, Ordering::SeqCst);
        println!("rtokio: FAILED {name}");
    }
}

fn main() {
    // Сначала однопоточный рантайм, потом многопоточный с одним и с двумя
    // рабочими потоками: так видно, в какой связке теряется пробуждение.
    for (name, workers) in [("current_thread", 0usize), ("multi_thread x1", 1), ("multi_thread x2", 2)] {
        println!("rtokio: === runtime {name}");
        let rt = if workers == 0 {
            tokio::runtime::Builder::new_current_thread().enable_all().build()
        } else {
            tokio::runtime::Builder::new_multi_thread().worker_threads(workers).enable_all().build()
        }
        .unwrap();
        rt.block_on(suite());
        drop(rt);
    }
    finish();
}

async fn suite() {
    println!("rtokio: runtime up");
    // Сторож: рантайм завис — сказать об этом, а не молчать до таймаута стенда.
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watch = done.clone();
    std::thread::spawn(move || {
        for _ in 0..25 {
            std::thread::sleep(Duration::from_secs(1));
            if watch.load(Ordering::SeqCst) {
                return;
            }
        }
        println!("rtokio: FAILED watchdog: runtime stuck for 25 s");
        std::process::exit(2);
    });
    let _guard = DoneOnDrop(done);

    // Таймер рантайма.
    let t0 = std::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    check("tokio sleep", t0.elapsed() >= Duration::from_millis(45));

    // Задачи на двух рабочих потоках и канал.
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    for i in 0..8u32 {
        let tx = tx.clone();
        tokio::spawn(async move {
            tx.send(i).await.unwrap();
        });
    }
    drop(tx);
    let mut sum = 0;
    while let Some(v) = rx.recv().await {
        sum += v;
    }
    check("tasks and mpsc channel", sum == 28);

    // Сеть: асинхронный TCP-сервер и клиенты через петлю — тут работает
    // selector mio на poll() и пробуждение через pipe.
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => {
            check(&format!("tokio tcp bind ({e})"), false);
            return;
        }
    };
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { break };
            tokio::spawn(async move {
                let mut buf = [0u8; 64];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    let mut clients = Vec::new();
    for i in 0..4u8 {
        clients.push(tokio::spawn(async move {
            let mut c = tokio::net::TcpStream::connect(addr).await?;
            let msg = [b'a' + i; 16];
            c.write_all(&msg).await?;
            let mut back = [0u8; 16];
            c.read_exact(&mut back).await?;
            Ok::<bool, std::io::Error>(back == msg)
        }));
    }
    let mut good = 0;
    for c in clients {
        match tokio::time::timeout(Duration::from_secs(10), c).await {
            Ok(Ok(Ok(true))) => good += 1,
            other => println!("rtokio: note: client {other:?}"),
        }
    }
    check("4 async tcp clients echoed", good == 4);
}

fn finish() {
    let failed = FAILED.load(Ordering::SeqCst);
    println!("rtokio: done, {failed} check(s) failed");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

struct DoneOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

