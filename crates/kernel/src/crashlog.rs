// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Снимок паники, который переживает перезагрузку.
//!
//! # Зачем
//!
//! Паника на чужой машине до сих пор разбиралась по фотографии экрана. На
//! экран помещается хвост журнала, а причина обычно выше — там, где драйвер
//! сказал, что устройство повело себя странно. Кольцо журнала ([`crate::klog`])
//! помнит последние 32 КиБ, но живёт в `.bss` и обнуляется следующей загрузкой.
//!
//! # Как
//!
//! Так же, как `ramoops` в Linux и «ram console» у загрузчиков MediaTek: при
//! панике хвост журнала копируется в участок физической памяти, который ядро
//! **никогда не раздаёт**, и помечается заголовком с контрольной суммой. Тёплый
//! сброс (кнопка, `Ctrl+Alt+Del` у виртуальной машины, сторожевой таймер)
//! питания с памяти не снимает, и следующая загрузка находит снимок, возвращает
//! его (`lastpanic`) и кладёт в `/var/log/last-panic.log`, если есть раздел
//! состояния. На x86-64 после паники ядро ждёт нажатия на клавиатуре PS/2 и
//! само делает тёплый сброс (`arch::reboot_on_key`): у ноутбука кнопки сброса
//! нет, а удержание кнопки питания снимает питание вместе со снимком.
//!
//! Писать при панике прямо на диск было бы надёжнее против выключения питания,
//! но требовало бы от каждого дискового драйвера отдельного пути без замков и
//! без прерываний — в ядре, которое только что доказало, что его состояние
//! испорчено. Память такого пути не требует: это одно копирование.
//!
//! # Чего это не умеет, и это надо знать
//!
//! Холодный старт (питание сняли, батарею вынули) память стирает: снимка не
//! будет. Прошивка вправе затереть участок при сбросе — поэтому заголовок с
//! суммой, и чужие байты никогда не выдаются за снимок. Где участок лежит, см.
//! [`REGIONS`]: на каждой архитектуре это место, проверенное на стенде, а не
//! догадка «наверняка свободно».

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use alloc::vec::Vec;

use crate::mm::PhysAddr;
use crate::sync::SpinLock;
use crate::kprintln;

/// Длина участка: кольцо журнала 32 КиБ, и вдвое — запас на случай, если оно
/// вырастет; заголовок занимает первые 64 байта.
const LENGTH: u64 = 0x1_0000;

/// Где участок может лежать — по порядку предпочтения. Берётся первый, вся
/// память которого описана картой как свободная; карта одной и той же машины
/// от загрузки к загрузке одна, поэтому и выбор один.
///
/// x86-64: 64 КиБ с `0x10000`, в нижних 640 КиБ. Прошивка UEFI раздаёт память
/// сверху вниз, и низ до конца загрузки не трогает; проверено сбросом на QEMU.
#[cfg(target_arch = "x86_64")]
const REGIONS: &[u64] = &[0x0001_0000];
/// AArch64 с прошивкой: выше первого мегабайта памяти. Первый мегабайт при
/// каждом сбросе заново занимает дерево устройств, которое QEMU кладёт в начало
/// памяти (`0x40040000` сброс не пережил — проверено). Без прошивки (договор
/// Linux, телефон) там лежит само ядро, и остаётся участок **ниже** его адреса
/// загрузки `0x40080000`.
#[cfg(target_arch = "aarch64")]
const REGIONS: &[u64] = &[0x4010_0000, 0x4004_0000];

/// Смещение текста от начала участка.
const TEXT: usize = 64;

/// «FOSPANIC» младшим байтом вперёд.
const MAGIC: u64 = u64::from_le_bytes(*b"FOSPANIC");
const VERSION: u32 = 1;

/// Заголовок снимка. Поля пишутся по одному и `volatile`: компилятор не должен
/// ни переставить их, ни выбросить как «никем не читаемые».
#[repr(C)]
struct Header {
    magic: u64,
    version: u32,
    len: u32,
    crc: u32,
    _reserved: u32,
}

/// Адрес участка в прямом отображении. Ноль — участка нет.
static BASE: AtomicUsize = AtomicUsize::new(0);
/// Длина участка.
static LEN: AtomicUsize = AtomicUsize::new(0);
/// Снимок уже пишется: паника внутри паники снимок не трогает.
static SAVING: AtomicBool = AtomicBool::new(false);
/// Снимок прошлой загрузки, если он был.
static PREVIOUS: SpinLock<Option<Vec<u8>>> = SpinLock::new(None);
/// Физический адрес участка, если его удалось занять.
static CLAIMED: SpinLock<Option<(u64, u64)>> = SpinLock::new(None);

/// Занять участок у распределителя кадров.
///
/// Зовётся сразу после того, как распределитель поднят, и **до** того, как он
/// выдал хоть один кадр: первые же таблицы страниц легли бы поверх снимка.
/// Участок занимается, только если вся его память описана картой как свободная:
/// память, которую прошивка оставила себе, трогать нельзя ни при панике, ни
/// тем более при чтении.
pub fn claim() {
    for &start in REGIONS {
        let taken = crate::mm::frame::with(|frames| frames.claim_range(start, LENGTH)).unwrap_or(false);
        if taken {
            *CLAIMED.lock() = Some((start, LENGTH));
            return;
        }
    }
    kprintln!("  crashlog    : no candidate range is free memory here; no panic snapshot on this machine");
}

/// Прочитать снимок прошлой загрузки и приготовить участок к следующему.
///
/// Зовётся, когда уже есть прямое отображение и куча: снимок копируется в кучу,
/// а заголовок в памяти стирается — иначе одну и ту же панику сообщала бы каждая
/// следующая загрузка.
pub fn recover() {
    let Some((start, len)) = *CLAIMED.lock() else {
        return;
    };
    let base = PhysAddr::new(start).to_direct_map().as_usize();
    let header = base as *mut Header;
    // SAFETY: участок занят в `claim`, отображён прямым отображением и никем,
    // кроме этого модуля, не используется.
    let found = unsafe {
        let magic = core::ptr::addr_of!((*header).magic).read_volatile();
        let version = core::ptr::addr_of!((*header).version).read_volatile();
        let text_len = core::ptr::addr_of!((*header).len).read_volatile() as usize;
        let crc = core::ptr::addr_of!((*header).crc).read_volatile();
        if magic == MAGIC && version == VERSION && text_len <= len as usize - TEXT {
            let text = core::slice::from_raw_parts((base + TEXT) as *const u8, text_len);
            (checksum(text) == crc).then(|| text.to_vec())
        } else {
            None
        }
    };
    // SAFETY: см. выше.
    unsafe { core::ptr::addr_of_mut!((*header).magic).write_volatile(0) };
    LEN.store(len as usize, Ordering::Relaxed);
    BASE.store(base, Ordering::Release);

    match found {
        Some(text) => {
            kprintln!(
                "  crashlog    : the previous boot panicked; {} bytes of its log are back (`lastpanic`)",
                text.len()
            );
            // Причина — сразу в журнал этой загрузки: строка после заголовка
            // паники — место, а следующая — сообщение.
            if let Some(reason) = reason(&text) {
                kprintln!("  crashlog    : {reason}");
            }
            *PREVIOUS.lock() = Some(text);
        }
        None => kprintln!("  crashlog    : {} KiB at {start:#x} kept for a panic snapshot", len / 1024),
    }
}

/// Место и сообщение паники из текста снимка — одной строкой.
fn reason(text: &[u8]) -> Option<alloc::string::String> {
    let text = core::str::from_utf8(text).ok()?;
    let after = text.rsplit_once("*** KERNEL PANIC ***")?.1;
    let mut lines = after.lines().map(str::trim).filter(|line| !line.is_empty());
    let place = lines.next()?;
    let message = lines.next().unwrap_or("");
    Some(alloc::format!("{place}: {message}"))
}

/// Снимок прошлой загрузки.
#[must_use]
pub fn previous() -> Option<Vec<u8>> {
    PREVIOUS.lock().clone()
}

/// Положить снимок прошлой загрузки в `/var/log/last-panic.log`.
///
/// Зовётся после монтирования раздела состояния. Без него писать некуда — корень
/// живой системы только для чтения, — и снимок остаётся в памяти до выключения.
pub fn store() {
    use crate::fs;
    use crate::vfs::perm::{Access, Credentials};
    use crate::vfs::VfsError;

    let Some(text) = previous() else {
        return;
    };
    const DIR: &str = "/var/log";
    const FILE: &str = "/var/log/last-panic.log";
    // Каталога может не быть: установщик создаёт только `/var`.
    let _ = fs::mkdir_as(Credentials::ROOT, DIR, 0o755);
    let node = match fs::resolve_as(Credentials::ROOT, FILE, Access::WRITE) {
        Some(Ok(node)) => node.truncate(0).map(|()| node),
        Some(Err(VfsError::NotFound)) => match fs::create_as(Credentials::ROOT, FILE, 0o644) {
            Some(result) => result,
            None => Err(VfsError::NotFound),
        },
        Some(Err(err)) => Err(err),
        None => Err(VfsError::NotFound),
    };
    // Причина — первой строкой: файл открывают, чтобы узнать, **почему**, а
    // сама паника лежит в самом конце шестнадцати килобайт журнала.
    let head = alloc::format!(
        "FreeOS panic snapshot: {}
{} bytes of the kernel log follow; the panic is at the end.

",
        reason(&text).unwrap_or_else(|| "reason not found in the log".into()),
        text.len()
    );
    let written = node.and_then(|node| {
        node.write_at(0, head.as_bytes())?;
        node.write_at(head.len() as u64, &text)
    });
    match written {
        Ok(_) => {
            let _ = fs::sync_all();
            kprintln!("  crashlog    : saved to {FILE}");
        }
        Err(err) => kprintln!("  crashlog    : not saved to {FILE} ({err}); `lastpanic` still has it"),
    }
}

/// Записать снимок. Зовётся обработчиком паники, после того как сообщение
/// напечатано — тогда оно уже лежит в хвосте журнала.
///
/// Возвращает `true`, если снимок записан.
pub fn save(info: &core::panic::PanicInfo<'_>) -> bool {
    let base = BASE.load(Ordering::Acquire);
    if base == 0 || SAVING.swap(true, Ordering::AcqRel) {
        return false;
    }
    let len = LEN.load(Ordering::Relaxed);
    // SAFETY: участок занят, отображён и принадлежит только этому модулю;
    // остальные процессоры уже остановлены обработчиком паники.
    let text = unsafe { core::slice::from_raw_parts_mut((base + TEXT) as *mut u8, len - TEXT) };

    // Хвост журнала без ожидания замка: паника посреди печати держит его сама,
    // и ждать было бы некого. Тогда пишется хотя бы само сообщение.
    let used = match crate::klog::tail_nowait(text) {
        Some(used) => used,
        None => {
            let mut sink = Slice { bytes: text, used: 0 };
            let _ = write!(sink, "\n*** KERNEL PANIC ***\n");
            if let Some(location) = info.location() {
                let _ = write!(sink, "at {}:{}:{}\n", location.file(), location.line(), location.column());
            }
            let _ = write!(sink, "{}\n", info.message());
            sink.used
        }
    };
    let crc = checksum(&text[..used]);
    let header = base as *mut Header;
    // SAFETY: см. выше. Метка пишется последней и после барьера: снимок,
    // прерванный на середине, не должен выглядеть целым.
    unsafe {
        core::ptr::addr_of_mut!((*header).version).write_volatile(VERSION);
        core::ptr::addr_of_mut!((*header).len).write_volatile(used as u32);
        core::ptr::addr_of_mut!((*header).crc).write_volatile(crc);
        core::sync::atomic::fence(Ordering::SeqCst);
        core::ptr::addr_of_mut!((*header).magic).write_volatile(MAGIC);
    }
    true
}

fn checksum(bytes: &[u8]) -> u32 {
    fpk::crc32_update(0, bytes)
}

/// Запись в срез без выделения памяти: при панике куча может быть испорчена.
struct Slice<'a> {
    bytes: &'a mut [u8],
    used: usize,
}

impl Write for Slice<'_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let room = self.bytes.len() - self.used;
        let take = text.len().min(room);
        self.bytes[self.used..self.used + take].copy_from_slice(&text.as_bytes()[..take]);
        self.used += take;
        Ok(())
    }
}
