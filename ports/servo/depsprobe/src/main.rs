//! `depsprobe` — C/C++-зависимости Servo на FreeOS (N4, после SpiderMonkey).
//!
//! Каждая библиотека делает то, ради чего она в Servo, на настоящих данных:
//! HarfBuzz раскладывает строку системным шрифтом, FreeType растрирует
//! глиф, OTS (`fontsan`) проверяет шрифт, как перед веб-шрифтом, aws-lc
//! считает хеш/шифрует/подписывает и держит TLS 1.3 в `rustls` (как
//! servo-net), SQLite пишет базу (servo-net, servo-storage), zstd и zlib
//! жмут и разжимают. Строки `depsprobe: ok …` ждёт сценарий `servo-deps`.
//!
//! Аргумент — каталог для файла базы SQLite (корень живой системы только
//! для чтения); без него проверяется только база в памяти.

use std::ffi::CStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

static FAILED: AtomicUsize = AtomicUsize::new(0);

fn check(name: &str, ok: bool) {
    if ok {
        println!("depsprobe: ok {name}");
    } else {
        FAILED.fetch_add(1, Ordering::SeqCst);
        println!("depsprobe: FAILED {name}");
    }
}

const FONT_DIRS: [&str; 2] = ["/usr/share/fonts/dejavu/DejaVuSans.ttf", "/usr/share/fonts/fossai/DejaVuSans.ttf"];

fn main() {
    println!("depsprobe: start");
    let font = FONT_DIRS.iter().find_map(|p| std::fs::read(p).ok().map(|d| (p, d)));
    match &font {
        Some((p, d)) => println!("depsprobe: font {p} ({} bytes)", d.len()),
        None => check("system font found", false),
    }
    font_dirs();
    if let Some((_, data)) = &font {
        harfbuzz(data);
        freetype(data);
        ots(data);
    }
    zlib();
    zstd_check();
    awslc();
    tls();
    let dir = std::env::args().nth(1).map(PathBuf::from);
    if let Some(dir) = &dir {
        locks(dir);
    }
    sqlite(dir.as_deref());
    let failed = FAILED.load(Ordering::SeqCst);
    println!("depsprobe: done, {failed} check(s) failed");
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

/// Список шрифтов обходом каталогов — так будет работать `font_list` Servo
/// для FreeOS (как у Android, без fontconfig).
fn font_dirs() {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ttf") || x.eq_ignore_ascii_case("otf")) {
                out.push(p);
            }
        }
    }
    let mut fonts = Vec::new();
    walk(Path::new("/usr/share/fonts"), &mut fonts);
    println!("depsprobe: {} font file(s) under /usr/share/fonts", fonts.len());
    check("font directory scan", !fonts.is_empty());
}

fn harfbuzz(data: &[u8]) {
    use harfbuzz_sys::*;
    let text = "Hello, мир! office";
    unsafe {
        let blob = hb_blob_create(data.as_ptr().cast(), data.len() as u32, HB_MEMORY_MODE_READONLY, std::ptr::null_mut(), None);
        let face = hb_face_create(blob, 0);
        let font = hb_font_create(face);
        let buf = hb_buffer_create();
        hb_buffer_add_utf8(buf, text.as_ptr().cast(), text.len() as i32, 0, text.len() as i32);
        hb_buffer_guess_segment_properties(buf);
        hb_shape(font, buf, std::ptr::null(), 0);
        let mut n = 0u32;
        let infos = hb_buffer_get_glyph_infos(buf, &mut n);
        let mut m = 0u32;
        let pos = hb_buffer_get_glyph_positions(buf, &mut m);
        let infos = std::slice::from_raw_parts(infos, n as usize);
        let pos = std::slice::from_raw_parts(pos, m as usize);
        let width: i64 = pos.iter().map(|p| i64::from(p.x_advance)).sum();
        let missing = infos.iter().filter(|g| g.codepoint == 0).count();
        // «ffi» в «office» DejaVu собирает в лигатуру — глифов меньше букв.
        let chars = text.chars().count();
        println!("depsprobe: harfbuzz {chars} chars -> {n} glyphs, width {width}, missing {missing}");
        check("harfbuzz shaping (latin + cyrillic, ligature)", n > 0 && (n as usize) < chars && missing == 0 && width > 0);
        hb_buffer_destroy(buf);
        hb_font_destroy(font);
        hb_face_destroy(face);
        hb_blob_destroy(blob);
    }
}

fn freetype(data: &[u8]) {
    use freetype_sys::*;
    unsafe {
        let mut lib: FT_Library = std::ptr::null_mut();
        if FT_Init_FreeType(&mut lib) != 0 {
            return check("freetype init", false);
        }
        let mut face: FT_Face = std::ptr::null_mut();
        if FT_New_Memory_Face(lib, data.as_ptr(), data.len() as FT_Long, 0, &mut face) != 0 {
            return check("freetype open face", false);
        }
        let family = CStr::from_ptr((*face).family_name).to_string_lossy().into_owned();
        FT_Set_Pixel_Sizes(face, 0, 32);
        let mut lit = 0usize;
        let mut size = (0, 0);
        if FT_Load_Char(face, 'Ж' as FT_ULong, FT_LOAD_RENDER) == 0 {
            let bm = &(*(*face).glyph).bitmap;
            size = (bm.width, bm.rows);
            for y in 0..bm.rows {
                let row = bm.buffer.offset((y * bm.pitch) as isize);
                for x in 0..bm.width {
                    if *row.offset(x as isize) > 127 {
                        lit += 1;
                    }
                }
            }
        }
        println!("depsprobe: freetype '{family}', {} glyphs, 'Ж' at 32px = {}x{}, {lit} lit pixels", (*face).num_glyphs, size.0, size.1);
        check("freetype render glyph", family == "DejaVu Sans" && size.0 > 10 && size.1 > 10 && lit > 50);
        FT_Done_Face(face);
        FT_Done_FreeType(lib);
    }
}

fn ots(data: &[u8]) {
    match fontsan::process(data) {
        Ok(out) => {
            println!("depsprobe: ots {} -> {} bytes", data.len(), out.len());
            check("ots (fontsan) sanitizes font", out.len() > 1000);
        }
        Err(e) => check(&format!("ots (fontsan) sanitizes font ({e:?})"), false),
    }
    check("ots rejects garbage", fontsan::process(b"not a font at all, just text").is_err());
}

fn zlib() {
    use libz_sys::*;
    let src: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    unsafe {
        let mut clen = compressBound(src.len() as uLong);
        let mut c = vec![0u8; clen as usize];
        let r1 = compress2(c.as_mut_ptr(), &mut clen, src.as_ptr(), src.len() as uLong, 6);
        let mut dlen = src.len() as uLong;
        let mut d = vec![0u8; src.len()];
        let r2 = uncompress(d.as_mut_ptr(), &mut dlen, c.as_ptr(), clen);
        println!("depsprobe: zlib {} -> {clen} bytes", src.len());
        check("zlib compress/uncompress", r1 == Z_OK && r2 == Z_OK && d == src && (clen as usize) < src.len() / 10);
    }
}

fn zstd_check() {
    let src: Vec<u8> = (0..100_000u32).flat_map(|i| (i / 7).to_le_bytes()).collect();
    let c = zstd::bulk::compress(&src, 3);
    let ok = match &c {
        Ok(c) => {
            println!("depsprobe: zstd {} -> {} bytes", src.len(), c.len());
            zstd::bulk::decompress(c, src.len()).is_ok_and(|d| d == src)
        }
        Err(_) => false,
    };
    check("zstd compress/decompress", ok);
}

fn awslc() {
    use aws_lc_rs::{aead, digest, rand, signature};
    use aws_lc_rs::rand::SecureRandom;
    use aws_lc_rs::signature::KeyPair;
    let h = digest::digest(&digest::SHA256, b"abc");
    let hex: String = h.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    check("aws-lc sha256", hex == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");

    let rng = rand::SystemRandom::new();
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    let rnd = rng.fill(&mut key).is_ok() && rng.fill(&mut nonce).is_ok() && key != [0; 32];
    check("aws-lc random (getentropy)", rnd);

    let k = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, &key).unwrap());
    let mut msg = b"servo on freeos".to_vec();
    let sealed = k.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut msg);
    let opened = sealed.is_ok()
        && k.open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::empty(), &mut msg)
            .is_ok_and(|p| p == b"servo on freeos");
    check("aws-lc aes-256-gcm seal/open", opened);

    let t = Instant::now();
    let ok = (|| {
        let pair = signature::EcdsaKeyPair::generate(&signature::ECDSA_P256_SHA256_FIXED_SIGNING).ok()?;
        let sig = pair.sign(&rng, b"message").ok()?;
        let pk = signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, pair.public_key().as_ref());
        let good = pk.verify(b"message", sig.as_ref()).is_ok();
        let bad = pk.verify(b"massage", sig.as_ref()).is_err();
        Some(good && bad)
    })();
    println!("depsprobe: ecdsa p-256 in {:?}", t.elapsed());
    check("aws-lc ecdsa p-256 sign/verify", ok == Some(true));
}

/// TLS 1.3 через петлю — как `rtls`, но криптография aws-lc (servo-net).
fn tls() {
    let t0 = Instant::now();
    let cert = match rcgen::generate_simple_self_signed(vec!["localhost".into()]) {
        Ok(c) => c,
        Err(e) => return check(&format!("tls certificate ({e})"), false),
    };
    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
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
            println!("depsprobe: tls in {:?}: {suite}", t0.elapsed());
            check("rustls + aws-lc handshake over loopback", &back == b"hello");
        }
        Err(e) => check(&format!("rustls + aws-lc handshake over loopback ({e})"), false),
    }
}

/// Блокировки файлов ядра (SYS_FLOCK, 08.11.2026). Два открытия одного файла —
/// два владельца у `flock` (конфликт), а POSIX-блокировки `fcntl` у процесса
/// одни на все его дескрипторы (не конфликтуют) и снимаются закрытием любого.
fn locks(dir: &Path) {
    use std::os::fd::AsRawFd;
    let path = dir.join("depsprobe.lock");
    let (Ok(a), Ok(b)) = (std::fs::File::create(&path), std::fs::File::open(&path)) else {
        return check("locks: open file twice", false);
    };
    let (fa, fb) = (a.as_raw_fd(), b.as_raw_fd());
    unsafe {
        let ex = libc::flock(fa, libc::LOCK_EX | libc::LOCK_NB) == 0;
        let busy = libc::flock(fb, libc::LOCK_SH | libc::LOCK_NB) == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK);
        let un = libc::flock(fa, libc::LOCK_UN) == 0;
        let then = libc::flock(fb, libc::LOCK_SH | libc::LOCK_NB) == 0;
        println!("depsprobe: flock ex={ex} other-open-busy={busy} unlock={un} then-shared={then}");
        check("locks: flock between two opens", ex && busy && un && then);
        libc::flock(fb, libc::LOCK_UN);

        let mut fl: libc::flock = std::mem::zeroed();
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = 100;
        fl.l_len = 10;
        let set = libc::fcntl(fa, libc::F_SETLK, &fl as *const libc::flock) == 0;
        // Свой процесс — не мешает себе, и через другой дескриптор тоже.
        let mut q: libc::flock = std::mem::zeroed();
        q.l_type = libc::F_WRLCK as _;
        q.l_start = 0;
        q.l_len = 0;
        let own = libc::fcntl(fb, libc::F_GETLK, &mut q as *mut libc::flock) == 0 && q.l_type == libc::F_UNLCK as _;
        let again = libc::fcntl(fb, libc::F_SETLK, &fl as *const libc::flock) == 0;
        drop(b); // закрытие любого дескриптора снимает POSIX-блокировки процесса
        let mut un: libc::flock = std::mem::zeroed();
        un.l_type = libc::F_UNLCK as _;
        un.l_len = 0;
        let unlock = libc::fcntl(fa, libc::F_SETLK, &un as *const libc::flock) == 0;
        println!("depsprobe: fcntl setlk={set} own-getlk-free={own} same-process-again={again} unlock={unlock}");
        check("locks: fcntl F_SETLK/F_GETLK", set && own && again && unlock);
    }
    drop(a);
    let _ = std::fs::remove_file(&path);
}

fn sqlite(dir: Option<&Path>) {
    use rusqlite::Connection;
    let run = |db: &Connection| -> rusqlite::Result<i64> {
        db.execute_batch("CREATE TABLE IF NOT EXISTS kv (k INTEGER PRIMARY KEY, v TEXT NOT NULL); DELETE FROM kv;")?;
        let tx = db.unchecked_transaction()?;
        {
            let mut st = tx.prepare("INSERT INTO kv (k, v) VALUES (?1, ?2)")?;
            for i in 1..=1000i64 {
                st.execute(rusqlite::params![i, format!("значение {i}")])?;
            }
        }
        tx.commit()?;
        db.query_row("SELECT SUM(k) FROM kv WHERE v LIKE 'значение%'", [], |r| r.get(0))
    };
    let version = rusqlite::version();
    match Connection::open_in_memory().and_then(|db| run(&db)) {
        Ok(sum) => {
            println!("depsprobe: sqlite {version} in memory, sum {sum}");
            check("sqlite in memory", sum == 500_500);
        }
        Err(e) => check(&format!("sqlite in memory ({e})"), false),
    }
    let Some(dir) = dir else { return };
    let path = dir.join("depsprobe.sqlite");
    let _ = std::fs::remove_file(&path);
    // VFS «unix» — с блокировками fcntl(F_SETLK) ядра (SYS_FLOCK): так
    // базу профиля могут открывать несколько окон и запусков браузера.
    let vfs = unsafe {
        let v = rusqlite::ffi::sqlite3_vfs_find(std::ptr::null());
        if v.is_null() { String::from("?") } else { CStr::from_ptr((*v).zName).to_string_lossy().into_owned() }
    };
    let _ = std::fs::remove_file(&path);
    let t = Instant::now();
    let res = (|| -> rusqlite::Result<(i64, i64)> {
        let first = { let db = Connection::open(&path)?; run(&db)? };
        // Открыть заново — данные на диске, а не в кеше соединения.
        let db = Connection::open(&path)?;
        let n = db.query_row("SELECT COUNT(*) FROM kv", [], |r| r.get(0))?;
        Ok((first, n))
    })();
    match res {
        Ok((sum, n)) => {
            println!("depsprobe: sqlite file (default vfs {vfs}) {} in {:?}, sum {sum}, rows {n}", path.display(), t.elapsed());
            check("sqlite file database", vfs == "unix" && sum == 500_500 && n == 1000);
        }
        Err(e) => check(&format!("sqlite file database (vfs {vfs}: {e:?})"), false),
    }
    let _ = std::fs::remove_file(&path);
}
