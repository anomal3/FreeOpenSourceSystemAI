//! Ядро FreeOS, Phase 1: приём управления от UEFI-загрузчика.
//!
//! # Как ядро сюда попадает
//!
//! Загрузчик читает ELF-образ ядра, выделяет память, копирует `PT_LOAD`
//! сегменты, обнуляет BSS (хвост сегмента, где `memsz > filesz`), применяет
//! relocations из `.rela.dyn`, вызывает `ExitBootServices` и передаёт
//! управление на `e_entry`, положив в первый аргумент указатель на `BootInfo`.
//!
//! # Почему образ position-independent
//!
//! Куда именно UEFI-прошивка даст выделить память, заранее неизвестно:
//! `AllocatePages` возвращает то, что свободно, и на разных машинах это разные
//! адреса. Чтобы ядро не зависело от адреса загрузки, оно собирается как PIE
//! (`ET_DYN`): все внутренние обращения — RIP/PC-относительные, а немногие
//! оставшиеся абсолютные адреса (таблицы указателей, `&'static str` в
//! диагностике) вынесены в `.rela.dyn` как `R_X86_64_RELATIVE` /
//! `R_AARCH64_RELATIVE`, которые загрузчик правит прибавлением базы.
//!
//! Конкретные флаги лежат в `.cargo/config.toml` (секции
//! `[target.x86_64-unknown-none]` и `[target.aarch64-unknown-none]`) и там же
//! прокомментированы. Отдельный linker script не нужен: раскладка по умолчанию
//! от `rust-lld` уже даёт ровно один `PT_LOAD`-набор с корректными `filesz` и
//! `memsz`, а границы BSS загрузчик берёт из разницы между ними — заводить ради
//! этого символы `__bss_start`/`__bss_end` смысла нет.
//!
//! # Точка входа
//!
//! [`kernel_main`] и есть `e_entry` ELF-заголовка: линкеру передан
//! `--entry=kernel_main`. Символ дополнительно экспортируется под своим именем,
//! так что загрузчик может найти его и через таблицу символов.

#![no_std]
#![no_main]

mod arch;
mod console;
mod mm;
mod print;
mod serial;
mod sync;

extern crate alloc;

use alloc::vec::Vec;
use boot_info::{BOOT_INFO_MAGIC, BOOT_INFO_REVISION, BootInfo, MemoryKind, MemoryMap};
use core::mem::{align_of, size_of};
use core::panic::PanicInfo;
use core::ptr;

use crate::mm::{AddressSpace, VirtAddr};

/// Точка входа ядра на x86-64.
///
/// ABI указан явно и `extern "C"` здесь был бы ошибкой. Загрузчик собран под
/// `x86_64-unknown-uefi`, где `extern "C"` — это Microsoft x64 (первый аргумент
/// в `RCX`), а ядро под `x86_64-unknown-none`, где то же `extern "C"` — System V
/// (первый аргумент в `RDI`). Обе стороны компилируются молча, а в рантайме
/// ядро читает регистр с мусором и сообщает о «повреждённом BootInfo», уводя
/// расследование в сторону. Соглашение фиксирует [`boot_info::KernelEntry`].
///
/// # Safety
///
/// Загрузчик обязан передать либо валидный указатель на инициализированный
/// [`BootInfo`], либо null. Любое другое значение (мусорный, но выровненный и
/// ненулевой адрес) ядро отличить не в состоянии — против этого и работает
/// проверка magic/revision ниже.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
pub extern "sysv64" fn kernel_main(boot_info: *const BootInfo) -> ! {
    start(boot_info)
}

/// Точка входа ядра на AArch64.
///
/// Здесь `extern "C"` корректен: и UEFI-таргет, и freestanding используют
/// AAPCS64, поэтому расхождения, описанного у x86-64 варианта, не возникает.
///
/// # Safety
///
/// Те же требования, что и у x86-64 варианта выше.
#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(boot_info: *const BootInfo) -> ! {
    start(boot_info)
}

/// Общее тело точки входа: всё, что не зависит от соглашения о вызове.
fn start(boot_info: *const BootInfo) -> ! {
    // Первым делом — serial: без него ни одна последующая ошибка не будет видна.
    serial::init();

    let Some(info) = validate(boot_info) else {
        kprintln!();
        kprintln!("FreeOS kernel: refusing to boot, see above. Halting.");
        arch::halt();
    };

    if info.framebuffer.is_present() {
        console::init(&info.framebuffer);
    }

    banner(&info, boot_info as usize);
    dump_memory_map(&info.memory_map);

    // С этого момента памятью распоряжается ядро, а не прошивка.
    take_over_memory(&info);

    // Переключение на собственный стек — последнее, что делается на стеке
    // загрузчика. Дальше в `info` заглядывать нельзя: это копия, лежащая на
    // покидаемом стеке. Поэтому продолжению уезжает физический адрес исходной
    // структуры, а не ссылка на копию.
    //
    // SAFETY: `STACK_TOP` — вершина области, которую `build_kernel_address_space`
    // только что отобразил на запись, а `resume_on_kernel_stack` — обычная
    // функция ядра, лежащая в исполняемом сегменте.
    unsafe {
        arch::switch_stack(
            VirtAddr::new(mm::STACK_TOP),
            resume_on_kernel_stack,
            boot_info as usize,
        )
    }
}

/// Забрать управление памятью у прошивки: пул кадров, собственные таблицы
/// страниц с W^X и куча.
///
/// Порядок шагов здесь не переставляется. Пул кадров нужен раньше таблиц,
/// потому что таблицы из него и строятся. Куча инициализируется последней:
/// её диапазон становится доступным только после того, как процессор
/// переключён на новые таблицы.
fn take_over_memory(info: &BootInfo) {
    kprintln!();
    kprintln!("---- memory subsystem -------------------------------------------");

    // SAFETY: карта памяти приехала от загрузчика и уже проверена вместе с
    // остальным хэндоффом; аллокатор инициализируется ровно один раз.
    let stats = match unsafe { mm::frame::init(info) } {
        Ok(stats) => stats,
        Err(err) => {
            kprintln!("FATAL: frame allocator init failed: {err:?}");
            arch::halt();
        }
    };
    kprintln!(
        "  frames      : {} total, {} free ({} MiB usable)",
        stats.total,
        stats.free,
        stats.free_bytes() / (1024 * 1024)
    );

    let space: Option<Result<arch::KernelSpace, arch::SpaceError>> =
        mm::frame::with(|frames| arch::build_kernel_address_space(info, frames));
    let space = match space {
        Some(Ok(space)) => space,
        Some(Err(err)) => {
            kprintln!("FATAL: building the kernel address space failed: {err:?}");
            arch::halt();
        }
        None => {
            kprintln!("FATAL: frame allocator unavailable while building page tables");
            arch::halt();
        }
    };
    kprintln!("  page tables : root at {:?}", space.root());

    // Точка невозврата номер два за загрузку: со следующей инструкции трансляцию
    // адресов определяют наши таблицы, а не прошивочные. Если в них чего-то не
    // хватает — кода, стека или UART, — отказ произойдёт молча и немедленно.
    //
    // SAFETY: адресное пространство содержит identity-отображение всей
    // физической памяти, поэтому и текущий код, и текущий стек остаются
    // доступными по тем же адресам.
    unsafe { space.activate() };
    kprintln!("  page tables : active, W^X applied to the kernel image");

    probe_framebuffer(info);

    // Физическая память теперь доступна и через прямое отображение; аллокатор
    // переводится на него, чтобы пережить снятие identity в следующей фазе.
    // SAFETY: `build_kernel_address_space` отобразил всю описанную картой
    // физическую память по `PHYS_MAP_BASE`, а таблицы уже активны.
    unsafe { mm::frame::use_direct_map() };

    // SAFETY: диапазон кучи отображён на запись и подкреплён отдельными
    // кадрами; вызывается однократно и только после активации таблиц.
    match unsafe { mm::heap::init() } {
        Ok(stats) => kprintln!(
            "  heap        : {} MiB at {:#018x} ({} bytes free)",
            mm::HEAP_SIZE / (1024 * 1024),
            mm::HEAP_BASE,
            stats.free
        ),
        Err(err) => {
            kprintln!("FATAL: heap init failed: {err:?}");
            arch::halt();
        }
    }
}

/// Убедиться, что фреймбуфер пережил переключение на собственные таблицы.
///
/// Проверка не декоративная: экранная консоль пишет по физическому адресу,
/// который до активации давало отображение прошивки. Если новое отображение
/// уводит этот адрес в другое место, записи будут молча уходить в никуда — на
/// экране останется последний кадр, нарисованный до переключения, и выглядеть
/// это будет как «ядро зависло», хотя serial продолжит работать.
fn probe_framebuffer(info: &BootInfo) {
    let fb = &info.framebuffer;
    if !fb.is_present() {
        return;
    }
    // Проверяются начало, середина и конец: если отображён лишь первый кусок
    // буфера, запись в пиксель (0,0) пройдёт, а нижние строки экрана окажутся
    // недостижимы — ровно тот случай, который выглядит как «консоль замерла».
    let words = (fb.size / 4).max(1);
    let probes = [0usize, (words / 2) as usize, (words - 1) as usize];
    let mut failures = 0;

    for offset in probes {
        let ptr = unsafe { (fb.base as *mut u32).add(offset) };
        // SAFETY: смещение лежит внутри буфера, длину которого сообщил
        // загрузчик, а диапазон только что отображён на запись. Значение
        // восстанавливается, поэтому картинка не портится. `volatile`
        // обязателен: это память устройства.
        let readback = unsafe {
            let saved = ptr.read_volatile();
            ptr.write_volatile(0x00FF_00FF);
            let readback = ptr.read_volatile();
            ptr.write_volatile(saved);
            readback
        };
        if readback != 0x00FF_00FF {
            failures += 1;
            kprintln!("  framebuffer : word {offset} unreachable (read {readback:#010x})");
        }
    }

    if failures == 0 {
        kprintln!("  framebuffer : writable end to end after the switch");
    }
}

/// Продолжение, исполняемое уже на собственном стеке ядра.
///
/// Стек загрузчика к этому моменту покинут: он лежит в памяти, помеченной
/// `BootloaderReclaimable`, и ядро вправе её переиспользовать.
extern "C" fn resume_on_kernel_stack(boot_info: usize) -> ! {
    // Копия `BootInfo` осталась на старом стеке, поэтому структура читается
    // заново по исходному физическому адресу — он по-прежнему отображён.
    // SAFETY: адрес прошёл полную проверку в `validate` до переключения стека,
    // а память под ним отображена как `BootloaderReclaimable` и пока цела.
    let info = unsafe { ptr::read(boot_info as *const BootInfo) };

    kprintln!();
    kprintln!("---- running on the kernel's own stack --------------------------");
    kprintln!("  stack top   : {:#018x}", mm::STACK_TOP);
    kprintln!("  guard page  : {:#018x} (unmapped)", mm::STACK_TOP - mm::STACK_SIZE - mm::PAGE_SIZE);

    verify_heap();

    let stats = mm::frame::stats();
    kprintln!();
    kprintln!(
        "  frames used : {} of {} ({} MiB still free)",
        stats.used(),
        stats.total,
        stats.free_bytes() / (1024 * 1024)
    );
    if info.framebuffer.is_present() {
        kprintln!("  framebuffer : still reachable at {:#018x}", info.framebuffer.base);
    }

    kprintln!();
    kprintln!("Phase 2 complete: memory is under kernel control. CPU halted.");
    arch::halt();
}

/// Проверить, что куча действительно работает.
///
/// Аллокации здесь не декоративные: до этого момента ни одна строчка ядра не
/// пользовалась `alloc`, и первая же настоящая аллокация — самый дешёвый способ
/// убедиться, что диапазон кучи отображён на разные физические кадры, а не на
/// один и тот же.
fn verify_heap() {
    let mut values: Vec<u64> = Vec::new();
    for i in 0..1024 {
        values.push(i * i);
    }
    let sum: u64 = values.iter().sum();

    let text = alloc::format!("{} values, checksum {:#x}", values.len(), sum);
    kprintln!("  heap check  : {text}");
}

/// Проверить хэндофф и вернуть **копию** [`BootInfo`].
///
/// Копия, а не ссылка: оригинал лежит в памяти типа
/// [`MemoryKind::BootloaderReclaimable`], которую ядро вправе переиспользовать,
/// как только заберёт из неё всё нужное.
///
/// Возвращает `None`, объяснив причину в serial, если хэндофф невалиден.
fn validate(raw: *const BootInfo) -> Option<BootInfo> {
    if raw.is_null() {
        kprintln!("FATAL: bootloader passed a null BootInfo pointer");
        return None;
    }
    if !raw.is_aligned() {
        kprintln!(
            "FATAL: BootInfo pointer {:#018x} is not {}-byte aligned",
            raw as usize,
            align_of::<BootInfo>()
        );
        return None;
    }

    // Сначала читаются только два скалярных поля, и лишь потом — структура
    // целиком. Причина тонкая: `BootInfo` содержит поля-`enum` (`Arch`,
    // `PixelFormat`), и чтение структуры из мусорной памяти создало бы значение
    // enum вне списка вариантов, то есть UB — ещё до того, как мы успели бы
    // сообщить о несовпадении magic. Скалярные `u64`/`u32` валидны при любом
    // битовом узоре, поэтому их читать безопасно всегда.
    //
    // SAFETY: указатель ненулевой и выровнен; загрузчик обязался передать
    // читаемую память (см. контракт `kernel_main`). `&raw const` не создаёт
    // промежуточной ссылки, поэтому требования к валидности `&BootInfo` не
    // возникает.
    let (magic, revision) = unsafe {
        (ptr::read(&raw const (*raw).magic), ptr::read(&raw const (*raw).revision))
    };

    if magic != BOOT_INFO_MAGIC || revision != BOOT_INFO_REVISION {
        kprintln!("FATAL: BootInfo handoff mismatch at {:#018x}", raw as usize);
        kprintln!("  expected magic {BOOT_INFO_MAGIC:#018x} revision {BOOT_INFO_REVISION}");
        kprintln!("  observed magic {magic:#018x} revision {revision}");
        kprintln!("  bootloader and kernel were built from different boot-info versions");
        return None;
    }

    // SAFETY: magic и revision совпали, значит структуру писал загрузчик,
    // собранный с тем же самым `boot-info`, — а он записывает в enum-поля
    // только объявленные варианты. Дополнительно: `BootInfo: Copy`, поэтому
    // `ptr::read` не создаёт двойного владения.
    let info = unsafe { ptr::read(raw) };

    if info.arch != arch::ARCH_ID {
        kprintln!(
            "WARNING: bootloader reports arch {:?}, kernel was built for {}",
            info.arch,
            arch::ARCH_NAME
        );
    }
    Some(info)
}

/// Баннер: кто стартовал и что именно приехало в `BootInfo`.
fn banner(info: &BootInfo, addr: usize) {
    kprintln!("================================================================");
    kprintln!(" FreeOS kernel v{} - Phase 2 bring-up", env!("CARGO_PKG_VERSION"));
    kprintln!(" architecture : {}", arch::ARCH_NAME);
    kprintln!("================================================================");
    kprintln!("BootInfo @ {addr:#018x}");
    kprintln!("  magic       : {:#018x} (valid)", info.magic);
    kprintln!("  revision    : {}", info.revision);

    let fb = &info.framebuffer;
    if fb.is_present() {
        kprintln!(
            "  framebuffer : {}x{} stride {} {:?}",
            fb.width,
            fb.height,
            fb.stride,
            fb.format
        );
        kprintln!("                base {:#018x}, {} KiB", fb.base, fb.size / 1024);
    } else {
        kprintln!("  framebuffer : absent (headless boot, serial only)");
    }

    if info.acpi_rsdp != 0 {
        kprintln!("  ACPI RSDP   : {:#018x}", info.acpi_rsdp);
    } else {
        kprintln!("  ACPI RSDP   : none");
    }
    if info.device_tree != 0 {
        kprintln!("  device tree : {:#018x}", info.device_tree);
    } else {
        kprintln!("  device tree : none");
    }
    kprintln!("  mem regions : {}", info.memory_map.len);
}

/// Зеркало [`boot_info::MemoryRegion`] со всеми полями скалярного типа.
///
/// Нужно ровно по той же причине, что и посегментное чтение в [`validate`]:
/// массив регионов лежит по адресу, пришедшему из-за границы доверия, и
/// восстанавливать из него `MemoryRegion` (у которого поле `kind` — `enum`)
/// значило бы рисковать невалидным дискриминантом. Совпадение раскладки
/// проверяется статически ниже.
#[repr(C)]
#[derive(Clone, Copy)]
struct RawRegion {
    start: u64,
    len: u64,
    kind: u32,
    _reserved: u32,
}

const _: () = assert!(size_of::<RawRegion>() == size_of::<boot_info::MemoryRegion>());
const _: () = assert!(align_of::<RawRegion>() == align_of::<boot_info::MemoryRegion>());

const KIND_USABLE: u32 = MemoryKind::Usable as u32;
const KIND_RESERVED: u32 = MemoryKind::Reserved as u32;
const KIND_ACPI_RECLAIM: u32 = MemoryKind::AcpiReclaimable as u32;
const KIND_ACPI_NVS: u32 = MemoryKind::AcpiNvs as u32;
const KIND_BOOTLOADER: u32 = MemoryKind::BootloaderReclaimable as u32;
const KIND_KERNEL: u32 = MemoryKind::Kernel as u32;
const KIND_FRAMEBUFFER: u32 = MemoryKind::Framebuffer as u32;

fn kind_name(kind: u32) -> &'static str {
    match kind {
        KIND_USABLE => "usable",
        KIND_RESERVED => "reserved",
        KIND_ACPI_RECLAIM => "acpi-reclaim",
        KIND_ACPI_NVS => "acpi-nvs",
        KIND_BOOTLOADER => "boot-reclaim",
        KIND_KERNEL => "kernel",
        KIND_FRAMEBUFFER => "framebuffer",
        _ => "unknown",
    }
}

/// Верхняя граница на число регионов, которые ядро согласно обойти.
///
/// Реальные карты UEFI — это десятки, изредка сотни записей. Если `len`
/// окажется абсурдным (повреждённый хэндофф, переполнение у загрузчика), лучше
/// напечатать усечённую сводку, чем уйти читать чужую память на гигабайты.
const MAX_REGIONS: u64 = 1024;

/// Сколько регионов показать подробно — остальные сворачиваются в счётчик.
const REGIONS_SHOWN: usize = 8;

fn dump_memory_map(map: &MemoryMap) {
    kprintln!();
    kprintln!("Memory map:");
    if map.ptr == 0 || map.len == 0 {
        kprintln!("  bootloader passed an empty memory map");
        return;
    }
    if map.ptr % align_of::<RawRegion>() as u64 != 0 {
        kprintln!("  region array at {:#018x} is misaligned, skipping", map.ptr);
        return;
    }

    let count = map.len.min(MAX_REGIONS);
    if count != map.len {
        kprintln!("  len={} looks implausible, inspecting first {}", map.len, count);
    }

    let base = map.ptr as *const RawRegion;
    let mut usable_bytes: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut usable_regions: u64 = 0;

    for index in 0..count {
        // SAFETY: массив построен загрузчиком, который заявил `len` записей по
        // адресу `ptr`, и лежит в BootloaderReclaimable-памяти, ещё никем не
        // переиспользованной — на Phase 1 ядро вообще ничего не выделяет.
        // Индекс ограничен `count <= len`, выравнивание проверено выше.
        // `read` (а не ссылка на массив) — чтобы не строить `&[MemoryRegion]`
        // с потенциально невалидными дискриминантами `kind`.
        let region = unsafe { ptr::read(base.add(index as usize)) };
        // saturating, а не обычное сложение: длины приходят снаружи, и на
        // повреждённой карте переполнение u64 уронило бы ядро паникой прямо
        // внутри диагностики — то есть ровно там, где она нужнее всего.
        total_bytes = total_bytes.saturating_add(region.len);
        if region.kind == KIND_USABLE {
            usable_bytes = usable_bytes.saturating_add(region.len);
            usable_regions += 1;
        }
        if (index as usize) < REGIONS_SHOWN {
            kprintln!(
                "  [{:02}] {:#014x}-{:#014x} {:>7} KiB  {}",
                index,
                region.start,
                region.start.wrapping_add(region.len),
                region.len / 1024,
                kind_name(region.kind)
            );
        }
    }

    if count as usize > REGIONS_SHOWN {
        kprintln!("  ... {} more regions", count as usize - REGIONS_SHOWN);
    }
    kprintln!(
        "  usable: {} MiB in {} regions; described total: {} MiB",
        usable_bytes / (1024 * 1024),
        usable_regions,
        total_bytes / (1024 * 1024)
    );
}

/// Обработчик паники: рассказать всё, что знаем, и остановиться.
///
/// `kprintln!` сам разбирается, куда писать: в serial всегда, на экран — если
/// консоль уже поднята. Так паника до инициализации фреймбуфера всё равно
/// оказывается видимой.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    kprintln!();
    kprintln!("*** KERNEL PANIC ***");
    if let Some(location) = info.location() {
        kprintln!("at {}:{}:{}", location.file(), location.line(), location.column());
    }
    kprintln!("{}", info.message());
    arch::halt();
}
