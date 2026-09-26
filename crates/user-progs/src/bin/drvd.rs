// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! `drvd` — служба, которая находит драйвер устройству, которого ядро не знает
//! (веха «драйверы по VID:PID», часть Д3).
//!
//! # Что делает
//!
//! Один проход при запуске системы (служба `init`, кончается с кодом 0 и не
//! перезапускается):
//!
//! 1. Берёт перепись шины у ядра (`SYS_DEVICES`) и выбирает устройства, у
//!    которых нет драйвера (`-` и `none`) — то, что оболочка показывает словами
//!    `NO DRIVER IN THIS KERNEL`.
//! 2. Ищет среди установленных пакетов (`/var/lib/pkg`) тот, что называет это
//!    устройство в поле `drives`, и запускает его программу (поле `driver`).
//! 3. Не нашёл — ищет пакет на `/media` и ставит его обычным `pkg install`:
//!    подпись, права и файлы проверяет установщик, а не эта служба, — и тоже
//!    запускает.
//! 4. Нет и там — спрашивает репозиторий (Д4): `sysupdate driver vvvv:dddd`
//!    проверяет подпись каталога драйверов, качает пакет в
//!    `/var/cache/drivers/driver.fpk` и сверяет SHA-256, а ставит его снова
//!    `pkg`. Серверы — те же, что для обновлений (`update.cfg`).
//!
//! # Почему сеть последней
//!
//! Установленный пакет и носитель в руках человека не требуют ничего, а сеть
//! требует карты, аренды DHCP и ответа сервера. Служба стартует вместе с
//! клиентом DHCP, поэтому адреса ждёт — но только когда до сети дошло дело:
//! машина, у которой всё нашлось локально, сеть не ждёт вовсе.
//!
//! # Чего не делает
//!
//! Не следит за горячим подключением: устройство, воткнутое после загрузки,
//! заметит только следующий запуск. Не перезапускает упавший драйвер — это
//! дело `init`, когда драйверы станут службами.

#![no_std]
#![no_main]

use fpk::{Header, Kind, Manifest, parse_drive};
use user_progs::{
    Dirent, Line, NetInfo, close, devices, exit, netinfo, open, read, read_at, readdir, remove,
    sleep_ms, spawn, uptime_ms, wait,
};

const REGISTRY: &str = "/var/lib/pkg";
const MEDIA: &str = "/media";

/// Куда `sysupdate driver` кладёт скачанный пакет.
const DOWNLOADED: &str = "/var/cache/drivers/driver.fpk";

/// Сколько ждать адреса от DHCP, прежде чем сказать «сети нет».
///
/// Минута: на отладочном ядре под эмуляцией аренда приходит за десятки
/// секунд после старта служб.
const NETWORK_WAIT_MS: u64 = 60_000;

static mut CENSUS: [u8; 8192] = [0; 8192];
static mut MANIFEST: [u8; fpk::MAX_MANIFEST] = [0; fpk::MAX_MANIFEST];

/// Строка ограниченной длины без кучи: имена пакетов и пути короткие.
struct Text {
    bytes: [u8; 128],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self { bytes: [0; 128], len: 0 }
    }

    fn push(&mut self, text: &str) -> &mut Self {
        let take = text.len().min(self.bytes.len() - self.len);
        self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
        self.len += take;
        self
    }

    /// Дописать `vendor:device` тем же видом, что в манифесте (`1234:11e8`).
    fn push_id(&mut self, vendor: u16, device: u16) -> &mut Self {
        let digits = b"0123456789abcdef";
        for (at, value) in [vendor, device].into_iter().enumerate() {
            if at == 1 {
                self.push(":");
            }
            for shift in [12u16, 8, 4, 0] {
                let digit = [digits[usize::from((value >> shift) & 0xf)]];
                // SAFETY: цифра ASCII.
                self.push(unsafe { core::str::from_utf8_unchecked(&digit) });
            }
        }
        self
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// Найденный драйвер: имя пакета и путь программы внутри него.
struct Found {
    package: Text,
    driver: Text,
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // SAFETY: программа однопоточна.
    let census = unsafe { &mut *core::ptr::addr_of_mut!(CENSUS) };
    let got = devices(census);
    let text = core::str::from_utf8(&census[..got.max(0) as usize]).unwrap_or("");

    let mut missing = 0u64;
    for line in text.lines() {
        let mut fields = line.split('\t');
        if fields.next() != Some("pci") {
            continue;
        }
        let (_address, id, _class, driver, state) =
            (fields.next(), fields.next(), fields.next(), fields.next(), fields.next());
        if driver != Some("-") || state != Some("none") {
            continue;
        }
        let Some((vendor, device)) = id.and_then(parse_drive) else {
            continue;
        };
        missing += 1;
        serve(vendor, device);
    }
    if missing == 0 {
        user_progs::println("drvd: every device on the bus has a driver");
    }
    exit(0)
}

/// Найти драйвер устройству и запустить его.
fn serve(vendor: u16, device: u16) {
    if let Some(found) = installed(vendor, device) {
        start(&found, vendor, device);
        return;
    }
    let Some(file) = on_media(vendor, device) else {
        from_repository(vendor, device);
        return;
    };
    Line::new()
        .str("drvd: ")
        .hex4(vendor)
        .str(":")
        .hex4(device)
        .str(" is driven by ")
        .str(MEDIA)
        .str("/")
        .str(file.as_str())
        .str("; installing it")
        .end();
    let mut command = Text::new();
    command.push("/bin/pkg install ").push(MEDIA).push("/").push(file.as_str());
    let code = wait(spawn(command.as_str()));
    if code != 0 {
        Line::new().str("drvd: pkg refused the package (").signed(code).str(")").end();
        return;
    }
    match installed(vendor, device) {
        Some(found) => start(&found, vendor, device),
        None => user_progs::println("drvd: the package installed, but does not name a driver program"),
    }
}

/// Спросить репозиторий (Д4): скачать, поставить, запустить.
fn from_repository(vendor: u16, device: u16) {
    if !network_ready() {
        Line::new()
            .str("drvd: no driver for ")
            .hex4(vendor)
            .str(":")
            .hex4(device)
            .str(", neither installed nor on /media, and there is no network to ask")
            .end();
        return;
    }
    Line::new()
        .str("drvd: ")
        .hex4(vendor)
        .str(":")
        .hex4(device)
        .str(" is neither installed nor on /media; asking the repository")
        .end();

    let mut command = Text::new();
    command.push("/bin/sysupdate driver ").push_id(vendor, device);
    match wait(spawn(command.as_str())) {
        0 => {}
        3 => {
            Line::new()
                .str("drvd: no driver for ")
                .hex4(vendor)
                .str(":")
                .hex4(device)
                .str(" anywhere: not installed, not on /media, not in the repository")
                .end();
            return;
        }
        code => {
            Line::new().str("drvd: the download failed (").signed(code).str(")").end();
            return;
        }
    }

    // Ставит `pkg`, как с носителя: подпись пакета и право `devices` —
    // его дело (Д2), а не этой службы и не качавшей программы.
    let mut install = Text::new();
    install.push("/bin/pkg install ").push(DOWNLOADED);
    let code = wait(spawn(install.as_str()));
    // Скачанный файл больше не нужен: установленное лежит в `/opt`, а
    // оставленный, он занимал бы раздел состояния до следующей загрузки.
    let _ = remove(DOWNLOADED);
    if code != 0 {
        Line::new().str("drvd: pkg refused the downloaded package (").signed(code).str(")").end();
        return;
    }
    match installed(vendor, device) {
        Some(found) => start(&found, vendor, device),
        None => user_progs::println("drvd: the package installed, but does not name a driver program"),
    }
}

/// Есть ли сеть, у которой можно что-то спросить.
///
/// Карты нет — ответ сразу. Карта есть, а адреса ещё нет — ждём аренды: служба
/// стартовала вместе с `dhcp`, и отказаться, не подождав, значило бы не найти
/// драйвер на каждой загрузке.
fn network_ready() -> bool {
    let mut info = NetInfo::default();
    if netinfo(&mut info) < 0 || info.present == 0 {
        return false;
    }
    if info.address != 0 {
        return true;
    }
    user_progs::println("drvd: waiting for the network to get an address");
    let deadline = uptime_ms() + NETWORK_WAIT_MS;
    while uptime_ms() < deadline {
        sleep_ms(500);
        if netinfo(&mut info) >= 0 && info.address != 0 {
            return true;
        }
    }
    false
}

/// Запустить драйвер. Не ждём: драйвер живёт столько, сколько устройство.
fn start(found: &Found, vendor: u16, device: u16) {
    let mut path = Text::new();
    path.push("/opt/").push(found.package.as_str()).push("/").push(found.driver.as_str());
    let task = spawn(path.as_str());
    Line::new()
        .str("drvd: started ")
        .str(path.as_str())
        .str(" for ")
        .hex4(vendor)
        .str(":")
        .hex4(device)
        .str(" as #")
        .signed(task)
        .end();
}

/// Установленный пакет, который обслуживает устройство.
fn installed(vendor: u16, device: u16) -> Option<Found> {
    let dir = open(REGISTRY);
    if dir < 0 {
        return None;
    }
    let mut entry = Dirent::default();
    let mut result = None;
    while readdir(dir, &mut entry) {
        let Some(file) = entry.name() else { continue };
        let Some(name) = file.strip_suffix(".pkg") else { continue };
        let mut path = Text::new();
        path.push(REGISTRY).push("/").push(file);
        let fd = open(path.as_str());
        if fd < 0 {
            continue;
        }
        // SAFETY: программа однопоточна.
        let buffer = unsafe { &mut *core::ptr::addr_of_mut!(MANIFEST) };
        let mut filled = 0usize;
        while filled < buffer.len() {
            let got = read(fd, &mut buffer[filled..]);
            if got <= 0 {
                break;
            }
            filled += got as usize;
        }
        close(fd);
        // Запись реестра — сохранённый манифест без контейнера; заголовок здесь
        // только ради разбора (так же читает реестр `pkg`).
        let header = Header {
            kind: Kind::Package,
            manifest_len: filled as u32,
            payload_len: 0,
            manifest_crc: fpk::crc32(&buffer[..filled]),
            payload_crc: 0,
            signature_algorithm: 0,
            signature_len: 0,
            signature: [0u8; fpk::SIGNATURE_SIZE],
        };
        let Ok(manifest) = Manifest::parse(&header, &buffer[..filled]) else { continue };
        if let Some(found) = serves(&manifest, name, vendor, device) {
            result = Some(found);
            break;
        }
    }
    close(dir);
    result
}

/// Пакет на `/media`, который обслуживает устройство, — имя файла.
fn on_media(vendor: u16, device: u16) -> Option<Text> {
    let dir = open(MEDIA);
    if dir < 0 {
        return None;
    }
    let mut entry = Dirent::default();
    let mut result = None;
    while readdir(dir, &mut entry) {
        let Some(file) = entry.name() else { continue };
        if !file.ends_with(".fpk") {
            continue;
        }
        let mut path = Text::new();
        path.push(MEDIA).push("/").push(file);
        let fd = open(path.as_str());
        if fd < 0 {
            continue;
        }
        let mut head = [0u8; fpk::HEADER_SIZE];
        let ok = read_at(fd, 0, &mut head) == fpk::HEADER_SIZE as i64;
        let header = if ok { Header::parse(&head).ok() } else { None };
        let mut matched = false;
        if let Some(header) = header.filter(|header| header.kind == Kind::Package) {
            // SAFETY: программа однопоточна.
            let buffer = unsafe { &mut *core::ptr::addr_of_mut!(MANIFEST) };
            let len = (header.manifest_len as usize).min(buffer.len());
            if read_at(fd, header.manifest_offset(), &mut buffer[..len]) == len as i64 {
                if let Ok(manifest) = Manifest::parse(&header, &buffer[..len]) {
                    matched = manifest.drives().any(|drive| drive == Some((vendor, device)));
                }
            }
        }
        close(fd);
        if matched {
            let mut name = Text::new();
            name.push(file);
            result = Some(name);
            break;
        }
    }
    close(dir);
    result
}

/// Обслуживает ли пакет устройство, и какой программой.
fn serves(manifest: &Manifest<'_>, name: &str, vendor: u16, device: u16) -> Option<Found> {
    if !manifest.drives().any(|drive| drive == Some((vendor, device))) {
        return None;
    }
    let driver = manifest.field("driver")?;
    let mut found = Found { package: Text::new(), driver: Text::new() };
    found.package.push(name);
    found.driver.push(driver);
    Some(found)
}
