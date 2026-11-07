//! `rtls` — TLS на FreeOS (N3): `rustls` с криптографией `ring` (C и
//! ассемблер собраны нашим набором), сертификат — `rcgen`. Сервер и клиент
//! в одном процессе через петлю. Строки `rtls: ok …` ждёт сценарий `rust-std`.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

static FAILED: AtomicUsize = AtomicUsize::new(0);

fn check(name: &str, ok: bool) {
    if ok {
        println!("rtls: ok {name}");
    } else {
        FAILED.fetch_add(1, Ordering::SeqCst);
        println!("rtls: FAILED {name}");
    }
}

fn main() {
    println!("rtls: start");
    let t0 = Instant::now();
    let cert = match rcgen::generate_simple_self_signed(vec!["localhost".into()]) {
        Ok(c) => c,
        Err(e) => {
            check(&format!("self-signed certificate ({e})"), false);
            return finish();
        }
    };
    println!("rtls: certificate in {:?}", t0.elapsed());
    check("self-signed certificate (ring keygen, getentropy)", true);

    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_cfg = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key)
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).unwrap();
    let client_cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_cfg = Arc::new(server_cfg);
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        let (s, _) = listener.accept()?;
        let conn = rustls::ServerConnection::new(server_cfg).map_err(std::io::Error::other)?;
        let mut tls = rustls::StreamOwned::new(conn, s);
        let mut buf = [0u8; 5];
        tls.read_exact(&mut buf)?;
        tls.write_all(&buf)?;
        tls.flush()
    });

    let t1 = Instant::now();
    let result = (|| -> std::io::Result<([u8; 5], String)> {
        let s = std::net::TcpStream::connect(("127.0.0.1", port))?;
        let name = "localhost".try_into().map_err(std::io::Error::other)?;
        let conn = rustls::ClientConnection::new(Arc::new(client_cfg), name).map_err(std::io::Error::other)?;
        let mut tls = rustls::StreamOwned::new(conn, s);
        tls.write_all(b"hello")?;
        let mut back = [0u8; 5];
        tls.read_exact(&mut back)?;
        let suite = format!("{:?} {:?}", tls.conn.protocol_version(), tls.conn.negotiated_cipher_suite().map(|s| s.suite()));
        Ok((back, suite))
    })();
    let _ = server.join();
    match result {
        Ok((back, suite)) => {
            println!("rtls: handshake and echo in {:?}: {suite}", t1.elapsed());
            check("tls handshake and echo over loopback", &back == b"hello");
        }
        Err(e) => check(&format!("tls handshake and echo over loopback ({e})"), false),
    }
    finish();
}

fn finish() {
    let failed = FAILED.load(Ordering::SeqCst);
    println!("rtls: done, {failed} check(s) failed");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}
