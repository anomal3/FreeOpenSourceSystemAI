//! Корень программ Linux из пакетов Debian (фаза 71b): `cargo xtask
//! thirdparty --only debian`.
//!
//! # Что и откуда
//!
//! glibc, `ld-linux`, `bash`, `coreutils` и их библиотеки — пакеты Debian 12
//! (bookworm) для amd64, **с архива-снимка** `snapshot.debian.org` на
//! 2026-10-20. Не с зеркала: зеркало держит только последнюю правку
//! стабильного выпуска, и закреплённый здесь хеш сломался бы с первым же
//! исправлением glibc. Снимок не меняется никогда. Хеш каждого пакета — из
//! индекса `Packages` того же дня; пакет, приехавший другим, не
//! распаковывается.
//!
//! # Как раскладывается
//!
//! `.deb` — архив `ar`, в нём `data.tar.xz`: файлы пакета от корня. Разбирает
//! его здесь же, своим кодом, а не `dpkg` и не `tar` хоста: на машине сборки
//! (Windows) нет первого, а второй не умеет то, что нужно, — **символьные
//! ссылки** превращаются в копии файлов, на которые указывают. В файловых
//! системах FossAi ссылок нет, а `ld-linux` ищет себя по ссылке
//! `/lib64/ld-linux-x86-64.so.2`. Документация, справка и переводы
//! выбрасываются: в системе их читать нечем.
//!
//! Итог — пакет `debian-12.fpk` (`/opt/debian` после `pkg install`) — корень,
//! которым ядро перекрывает системный для программ Linux (`linux_path`).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use fpk::Kind;
use fpk::build::{Builder, Entry};

use crate::arch::Arch;
use crate::{cbuild, sdk};

/// Снимок архива Debian, с которого берутся пакеты.
const SNAPSHOT: &str = "https://snapshot.debian.org/archive/debian/20261020T000000Z/";

/// Пакеты: путь в архиве и SHA-256 из `Packages` bookworm того же дня.
const PACKAGES: [(&str, &str); 12] = [
    ("pool/main/g/glibc/libc6_2.36-9+deb12u14_amd64.deb", "ba4f88f73dbc3ae9055f3c20f4523bfdbaf1ad13ff95e258924f77d20b4fbedf"),
    ("pool/main/n/ncurses/libtinfo6_6.4-4_amd64.deb", "072d908f38f51090ca28ca5afa3b46b2957dc61fe35094c0b851426859a49a51"),
    ("pool/main/b/bash/bash_5.2.15-2+b13_amd64.deb", "82130bb6a560cd2a7234d8018baf73f188f5dd56413d5aa0accc987b2197a6a1"),
    ("pool/main/c/coreutils/coreutils_9.1-1_amd64.deb", "61038f857e346e8500adf53a2a0a20859f4d3a3b51570cc876b153a2d51a3091"),
    ("pool/main/libs/libselinux/libselinux1_3.4-1+b6_amd64.deb", "2b07f5287b9105f40158b56e4d70cc1652dac56a408f3507b4ab3d061eed425f"),
    ("pool/main/p/pcre2/libpcre2-8-0_10.42-1_amd64.deb", "030db54f4d76cdfe2bf0e8eb5f9efea0233ab3c7aa942d672c7b63b52dbaf935"),
    ("pool/main/a/acl/libacl1_2.3.1-3_amd64.deb", "8be9df5795114bfe90e2be3d208ef47a5edd3fc7b3e20d387a597486d444e5e2"),
    ("pool/main/a/attr/libattr1_2.5.1-4_amd64.deb", "c4945123d66d0503ba42e2fc0585abc76d0838978c6d277b9cc37a4da25d1a34"),
    ("pool/main/g/gmp/libgmp10_6.2.1+dfsg1-1.1_amd64.deb", "187aedef2ed763f425c1e523753b9719677633c7eede660401739e9c893482bd"),
    ("pool/main/b/base-files/base-files_12.4+deb12u15_amd64.deb", "3eb1ea6d85488f488cc2a163b98ad640ef88cee4c79287cf14e361aaf6206f47"),
    ("pool/main/libc/libcap2/libcap2_2.66-4+deb12u3+b1_amd64.deb", "8d684c673e7483802a5c447834b0df6fce006eb3a7109e1be412f5ad425bb24f"),
    // `libgcc_s.so.1`: glibc подгружает её для `pthread_exit` и отмены потоков
    // (раскрутка стека) — фаза 71c.
    ("pool/main/g/gcc-12/libgcc-s1_12.2.0-14+deb12u1_amd64.deb", "3016e62cb4b7cd8038822870601f5ed131befe942774d0f745622cc77d8a88f7"),
];

/// Пакеты только для сборки проверочных программ (фаза 71c): заголовки glibc
/// и ядра, `crt1.o`, `libc.so`. В систему они не едут — из них и из корня
/// складывается sysroot, которым clang собирает программу для Linux.
const DEV_PACKAGES: [(&str, &str); 2] = [
    ("pool/main/g/glibc/libc6-dev_2.36-9+deb12u14_amd64.deb", "0218fc2befcd784c1b0c6292c0a137ce89fad054efaa579ad083bee0f2c01aae"),
    ("pool/main/l/linux/linux-libc-dev_6.1.176-1_amd64.deb", "8bb258735b9dffbb111da778ebdd024750878e435ffd9dfcadcb6762ede6b4cf"),
];

/// Проверочные программы на glibc: исходник в `libc/linux` и путь в корне.
const CHECKS: [(&str, &str); 1] = [("check.c", "usr/libexec/fossai/linuxcheck")];

/// Что в корень не попадает: справка, документация, переводы — читать их в
/// системе нечем; модули перекодировок glibc (`gconv`, 250 файлов) нужны
/// только `iconv`.
const SKIP: [&str; 7] = [
    "usr/share/doc/",
    "usr/share/man/",
    "usr/share/info/",
    "usr/share/locale/",
    "usr/share/lintian/",
    "usr/lib/x86_64-linux-gnu/gconv/",
    "usr/share/bash-completion/",
];

/// Имя пакета с корнем.
pub const PACKAGE_FILE: &str = "debian-12.fpk";

/// Где лежит собранный пакет (одна архитектура — x86-64).
pub fn package_path() -> PathBuf {
    cbuild::games_dir().join(PACKAGE_FILE)
}

/// Одна запись корня.
enum Node {
    File { data: Vec<u8>, mode: u32 },
    Link(String),
    Dir,
}

/// Скачать пакеты, разобрать их и собрать `debian-12.fpk`.
pub fn build(arch: Arch) -> Result<()> {
    if arch != Arch::X86_64 {
        say!("debian: только x86-64 — слой Linux на AArch64 ещё не умеет отличать вход программ Linux");
        return Ok(());
    }
    let cache = sdk::root().join("thirdparty/debian");
    fs::create_dir_all(&cache)?;
    let mut root: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut root)?;

    // Sysroot для сборки: корень плюс пакеты разработчика — на диск, ссылки
    // копиями (lld читает `libc.so` — сценарий с абсолютными путями внутри
    // sysroot).
    let mut dev: BTreeMap<String, Node> = BTreeMap::new();
    unpack(&cache, &PACKAGES, &mut dev)?;
    unpack(&cache, &DEV_PACKAGES, &mut dev)?;
    let sysroot = sdk::root().join("debian-sysroot");
    let _ = fs::remove_dir_all(&sysroot);
    for (path, node) in &dev {
        let data = match node {
            Node::Dir => continue,
            Node::File { data, .. } => data,
            Node::Link(_) => match follow(&dev, path, 0) {
                Some((data, _)) => data,
                None => continue,
            },
        };
        let target = sysroot.join(path);
        fs::create_dir_all(target.parent().unwrap())?;
        fs::write(&target, data)?;
    }
    say!("debian: sysroot для сборки — {}", sysroot.display());
    for (source, place) in CHECKS {
        let program = build_check(&sysroot, source)?;
        root.insert(String::from(place), Node::File { data: fs::read(&program)?, mode: 0o755 });
    }

    pack(&root)
}

/// Скачать (или взять из кеша) пакеты, сверить хеши и разложить в `root`.
fn unpack(cache: &std::path::Path, packages: &[(&str, &str)], root: &mut BTreeMap<String, Node>) -> Result<()> {
    for &(pool, sha256) in packages {
        let name = pool.rsplit('/').next().unwrap_or(pool);
        let file = cache.join(name);
        if !file.is_file() || digest(&fs::read(&file)?) != sha256 {
            let url = format!("{SNAPSHOT}{pool}");
            say!("> curl {url}");
            let status = std::process::Command::new("curl")
                .args(["--location", "--fail", "--silent", "--show-error", "--output"])
                .arg(&file)
                .arg(&url)
                .status()
                .context("не удалось запустить curl")?;
            if !status.success() {
                bail!("не скачался {url} ({status})");
            }
        }
        let bytes = fs::read(&file)?;
        let got = digest(&bytes);
        if got != sha256 {
            bail!("хеш {name} не тот: приехало {got}, ожидалось {sha256}; файл оставлен: {}", file.display());
        }
        let data = data_tar(&bytes).with_context(|| format!("{name}: не разобран архив .deb"))?;
        let added = read_tar(&data, root).with_context(|| format!("{name}: не разобран data.tar"))?;
        say!("debian: {name} — {added} записей");
    }
    Ok(())
}

/// Собрать проверочную программу на glibc clang-ом с sysroot Debian.
fn build_check(sysroot: &std::path::Path, source: &str) -> Result<PathBuf> {
    let clang = cbuild::llvm_tool("clang")?;
    let work = sdk::root().join("linux/glibc");
    fs::create_dir_all(&work)?;
    let output = work.join(source.trim_end_matches(".c"));
    let lib = sysroot.join("usr/lib/x86_64-linux-gnu");
    let mut cmd = std::process::Command::new(&clang);
    cmd.arg("--target=x86_64-linux-gnu")
        .arg(format!("--sysroot={}", sysroot.display()))
        .args(["-O2", "-Wall", "-Werror", "-fPIE", "-pie", "-fuse-ld=lld", "-nostdlib"])
        .arg("-Wl,--dynamic-linker=/lib64/ld-linux-x86-64.so.2")
        .arg(lib.join("Scrt1.o"))
        .arg(lib.join("crti.o"))
        .arg(cbuild::libc_dir().join("linux").join(source))
        .arg(format!("-L{}", lib.display()))
        .arg(format!("-L{}", sysroot.join("lib/x86_64-linux-gnu").display()))
        .arg("-lc")
        .arg(lib.join("crtn.o"))
        .arg("-o")
        .arg(&output);
    say!("> clang (glibc) {source}");
    let status = cmd.status().context("не удалось запустить clang")?;
    if !status.success() {
        bail!("{source} не собрался ({status})");
    }
    Ok(output)
}

/// Собрать `debian-12.fpk` из корня.
fn pack(root: &BTreeMap<String, Node>) -> Result<()> {
    let mut builder = Builder::new(Kind::Package, "debian", "12");
    builder.field("summary", "Debian 12 (bookworm) root for Linux programs: glibc, bash, coreutils");
    builder.field("about", "корень для программ Linux из пакетов Debian 12");
    let (mut files, mut bytes, mut links) = (0usize, 0usize, 0usize);
    for (path, node) in root {
        if SKIP.iter().any(|skip| path.starts_with(skip)) {
            continue;
        }
        let (data, mode) = match node {
            Node::Dir => continue,
            Node::File { data, mode } => (data.clone(), *mode),
            Node::Link(_) => match follow(root, path, 0) {
                // Ссылка на файл — копия файла. На каталог (их в этих пакетах
                // нет) или в никуда — пропуск, с именем в журнале.
                Some((data, mode)) => {
                    links += 1;
                    (data.clone(), mode)
                }
                None => {
                    say!("debian: ссылка {path} никуда не ведёт — пропущена");
                    continue;
                }
            },
        };
        files += 1;
        bytes += data.len();
        builder.file(&Entry { path: path.clone(), mode: (mode & 0o7777) as u16, uid: 0, gid: 0, data });
    }
    let out = package_path();
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, builder.finish())?;
    say!(
        "debian: {} — {files} файлов ({links} из ссылок), {} МиБ",
        out.display(),
        bytes / (1024 * 1024)
    );
    Ok(())
}

/// Куда ведёт ссылка `path` — до файла; `None` — в никуда, на каталог или по
/// кругу.
fn follow<'a>(root: &'a BTreeMap<String, Node>, path: &str, depth: usize) -> Option<(&'a Vec<u8>, u32)> {
    if depth > 16 {
        return None;
    }
    match root.get(path)? {
        Node::File { data, mode } => Some((data, *mode)),
        Node::Dir => None,
        Node::Link(target) => {
            let next = if let Some(absolute) = target.strip_prefix('/') {
                normalize(absolute)
            } else {
                let parent = path.rsplit_once('/').map_or("", |(dir, _)| dir);
                normalize(&format!("{parent}/{target}"))
            };
            follow(root, &next, depth + 1)
        }
    }
}

/// Свернуть `.` и `..` в пути от корня (без ведущей косой).
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Достать из `.deb` (`ar`) и распаковать `data.tar.xz`.
fn data_tar(deb: &[u8]) -> Result<Vec<u8>> {
    if !deb.starts_with(b"!<arch>\n") {
        bail!("не архив ar");
    }
    let mut at = 8;
    while at + 60 <= deb.len() {
        let header = &deb[at..at + 60];
        let name = String::from_utf8_lossy(&header[..16]).trim().trim_end_matches('/').to_string();
        let size: usize = String::from_utf8_lossy(&header[48..58]).trim().parse().context("размер члена ar")?;
        let start = at + 60;
        let end = start.checked_add(size).filter(|end| *end <= deb.len()).context("член ar за концом файла")?;
        if name == "data.tar.xz" {
            let mut out = Vec::new();
            lzma_rs::xz_decompress(&mut std::io::BufReader::new(&deb[start..end]), &mut out)
                .map_err(|err| anyhow::anyhow!("xz: {err:?}"))?;
            return Ok(out);
        }
        at = end + (size & 1);
    }
    bail!("в пакете нет data.tar.xz")
}

/// Разобрать tar (ustar, pax и длинные имена GNU) в `root`. Возвращает число
/// записей.
fn read_tar(tar: &[u8], root: &mut BTreeMap<String, Node>) -> Result<usize> {
    let mut at = 0;
    let mut count = 0;
    let mut long_name: Option<String> = None;
    let mut long_link: Option<String> = None;
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let field = |from: usize, len: usize| {
            let raw = &header[from..from + len];
            let end = raw.iter().position(|byte| *byte == 0).unwrap_or(len);
            String::from_utf8_lossy(&raw[..end]).into_owned()
        };
        let octal = |from: usize, len: usize| -> Result<usize> {
            let text = field(from, len);
            let text = text.trim();
            if text.is_empty() {
                return Ok(0);
            }
            usize::from_str_radix(text, 8).with_context(|| format!("число в заголовке tar: {text:?}"))
        };
        let size = octal(124, 12)?;
        let mode = octal(100, 8)? as u32;
        let kind = header[156];
        let data_start = at + 512;
        let data_end = data_start.checked_add(size).filter(|end| *end <= tar.len()).context("запись tar за концом")?;
        let data = &tar[data_start..data_end];
        at = data_start + size.next_multiple_of(512);

        match kind {
            b'x' => {
                // Расширенный заголовок pax: «длина ключ=значение\n».
                let text = String::from_utf8_lossy(data);
                for record in text.split('\n') {
                    let Some((_, pair)) = record.split_once(' ') else { continue };
                    if let Some(value) = pair.strip_prefix("path=") {
                        long_name = Some(value.to_string());
                    } else if let Some(value) = pair.strip_prefix("linkpath=") {
                        long_link = Some(value.to_string());
                    }
                }
                continue;
            }
            b'g' => continue,
            b'L' => {
                long_name = Some(String::from_utf8_lossy(data).trim_end_matches('\0').to_string());
                continue;
            }
            b'K' => {
                long_link = Some(String::from_utf8_lossy(data).trim_end_matches('\0').to_string());
                continue;
            }
            _ => {}
        }
        let name = long_name.take().unwrap_or_else(|| {
            let prefix = field(345, 155);
            let name = field(0, 100);
            if prefix.is_empty() { name } else { format!("{prefix}/{name}") }
        });
        let link = long_link.take().unwrap_or_else(|| field(157, 100));
        let path = normalize(&name);
        if path.is_empty() {
            continue;
        }
        let node = match kind {
            b'0' | 0 | b'7' => Node::File { data: data.to_vec(), mode },
            b'2' => Node::Link(link),
            // Жёсткая ссылка — копия того, что уже лежит под тем именем.
            b'1' => match root.get(&normalize(&link)) {
                Some(Node::File { data, mode }) => Node::File { data: data.clone(), mode: *mode },
                _ => continue,
            },
            b'5' => Node::Dir,
            _ => continue,
        };
        root.insert(path, node);
        count += 1;
    }
    Ok(count)
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = fpk::Hasher::new();
    hasher.update(bytes);
    hasher.finish().iter().map(|byte| format!("{byte:02x}")).collect()
}
