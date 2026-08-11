//! FreeOS UEFI bootloader.
//!
//! Загрузчик доводит машину от прошивки до первой инструкции ядра:
//!
//!   1. Диагностика прошивки, GOP-фреймбуфер и тестовая картинка ([`graphics`]).
//!   2. Чтение `\kernel.elf` с того же тома, с которого стартовал сам загрузчик
//!      ([`kernel_image`]).
//!   3. Разбор ELF64, размещение PIE-образа в физической памяти и применение
//!      релокаций ([`elf`]).
//!   4. Снятие карты памяти, `ExitBootServices` и прыжок в ядро ([`handoff`]).
//!
//! Шаги 1–3 полностью обратимы: при любой ошибке загрузчик печатает, что именно
//! не сошлось, и возвращает управление прошивке. Шаг 4 — точка невозврата, и
//! всё, что нужно сказать пользователю, сказано до неё.
//!
//! Весь текст, который уходит в консоль, намеренно ASCII: прошивка выводит
//! stdout ещё и на последовательный порт, преобразуя UCS-2 в однобайтовую
//! кодировку, и не-ASCII символы превращаются там в мусор или в `?`.

#![no_std]
#![no_main]

extern crate alloc;

mod elf;
mod graphics;
mod handoff;
mod kernel_image;

use core::convert::Infallible;
use core::time::Duration;

use boot_info::{Arch, BootInfo, KernelImage, KernelSegment, MemoryKind};
use uefi::table::cfg::ConfigTableEntry;
use uefi::{Status, boot, entry, println, system};

use handoff::{Handoff, Override};

/// Единственная арх-специфичная строчка в крейте: всё остальное обязано быть
/// общим для `x86_64-unknown-uefi` и `aarch64-unknown-uefi`.
const ARCH: Arch = if cfg!(target_arch = "x86_64") {
    Arch::X86_64
} else {
    Arch::AArch64
};

/// `ARCH` молча схлопывается в `AArch64` на любой третьей архитектуре, поэтому
/// запрещаем сборку под неё явно.
const _: () = assert!(
    cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64"),
    "boot-uefi supports only the x86_64-unknown-uefi and aarch64-unknown-uefi targets"
);

/// Сколько секунд держать сообщение об ошибке на экране, прежде чем вернуть
/// управление прошивке: иначе меню boot manager'а затрёт диагностику мгновенно.
const ERROR_LINGER_SECONDS: u32 = 10;

/// Короткая пауза перед точкой невозврата, чтобы тестовая картинка и сводка
/// успели попасться на глаза. Держать здесь десятки секунд больше незачем —
/// загрузка должна идти дальше, а не ждать человека.
const HANDOFF_LINGER: Duration = Duration::from_millis(1500);

/// Провал шага загрузки. Значение намеренно пустое: подробности уже напечатаны
/// там, где они были известны, а вызывающему остаётся только свернуть загрузку.
#[derive(Debug, Clone, Copy)]
pub struct Aborted;

#[entry]
fn main() -> Status {
    // До успешного init() нет ни println!, ни глобального аллокатора, поэтому
    // сообщить о провале некуда — остаётся вернуть код ошибки прошивке.
    if uefi::helpers::init().is_err() {
        return Status::ABORTED;
    }

    let _ = system::with_stdout(|stdout| stdout.clear());

    print_banner();

    let mut info = BootInfo::new(ARCH);
    info.framebuffer = graphics::probe_framebuffer();
    info.acpi_rsdp = find_acpi_rsdp();

    print_boot_info(&info);

    match boot_kernel(info) {
        // Ok несёт `Infallible`: успешный путь заканчивается прыжком в ядро.
        Ok(never) => match never {},
        Err(Aborted) => {
            println!("");
            println!("!! boot aborted: the kernel was NOT started, returning to firmware");
            linger(ERROR_LINGER_SECONDS);
            Status::LOAD_ERROR
        }
    }
}

/// Загружает ядро и передаёт ему управление. Возвращается только при ошибке:
/// успешный путь заканчивается прыжком, поэтому `Ok` несёт [`Infallible`].
fn boot_kernel(mut info: BootInfo) -> Result<Infallible, Aborted> {
    println!("");
    println!("---- kernel load ------------------------------------------------");

    // Образ читается в пул прошивки и живёт ровно до конца размещения: держать
    // его дольше — значит занимать место в карте памяти, которую увидит ядро.
    let image = kernel_image::read()?;
    let kernel = elf::load(&image)?;
    drop(image);

    // Куда лёг образ, ядро само выяснить не может: адрес выбрала прошивка, а
    // права сегментов остались в program headers, которых в памяти уже нет.
    // Указатель на карту сегментов проставит `Handoff::allocate` — она же её и
    // копирует в память, переживающую ExitBootServices.
    info.kernel = KernelImage {
        base: kernel.base,
        size: kernel.size,
        segments_ptr: 0,
        segments_len: 0,
    };

    // Карта памяти оценивается до выделения hand-off блока, но снимается
    // окончательно только внутри ExitBootServices — см. модуль `handoff`.
    let capacity = Handoff::estimate_capacity()?;
    let handoff = Handoff::allocate(&info, capacity, kernel.segments())?;

    // Диапазоны, тип которых прошивка не знает. Образ ядра лежит в LOADER_DATA
    // и без подмены выглядел бы для ядра как reclaimable-память, которую можно
    // затереть под собой. Пустые override'ы (например, при headless-загрузке)
    // на разбиение диапазонов не влияют.
    let overrides = [
        Override::new(kernel.base, kernel.size, MemoryKind::Kernel),
        Override::new(
            info.framebuffer.base,
            info.framebuffer.size,
            MemoryKind::Framebuffer,
        ),
    ];

    println!("");
    println!("---- hand-off ---------------------------------------------------");
    println!("  BootInfo        : {:#018x}", handoff.info_address());
    println!(
        "  kernel image    : {:#018x}..{:#018x} -> MemoryKind::Kernel",
        kernel.base,
        kernel.end()
    );
    println!("  entry point     : {:#018x}", kernel.entry);
    print_kernel_segments(&handoff, kernel.segments());
    println!("  exiting boot services -- console output stops here");
    println!("-----------------------------------------------------------------");

    boot::stall(HANDOFF_LINGER);

    handoff::exit_and_jump(handoff, kernel.entry, &overrides)
}

fn arch_name() -> &'static str {
    match ARCH {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
    }
}

fn print_banner() {
    println!("================================================================");
    println!("  FreeOS bootloader (boot-uefi)");
    println!("================================================================");
    println!("  target arch     : {}", arch_name());
    println!("  UEFI revision   : {}", system::uefi_revision());
    println!("  firmware vendor : {}", system::firmware_vendor());
    println!("  firmware rev    : {:#010x}", system::firmware_revision());
    println!("");
}

/// Физический адрес ACPI RSDP из UEFI configuration table, либо `0`.
///
/// ACPI 2.0+ RSDP предпочтительнее: он содержит XSDT с 64-битными указателями,
/// тогда как RSDP версии 1.0 знает только 32-битный RSDT. Обе записи могут
/// присутствовать одновременно, поэтому 1.0 берём лишь как запасной вариант.
fn find_acpi_rsdp() -> u64 {
    system::with_config_table(|entries| {
        let mut legacy = 0u64;
        for entry in entries {
            if entry.guid == ConfigTableEntry::ACPI2_GUID {
                return entry.address as usize as u64;
            }
            if entry.guid == ConfigTableEntry::ACPI_GUID {
                legacy = entry.address as usize as u64;
            }
        }
        legacy
    })
}

fn print_boot_info(info: &BootInfo) {
    println!("");
    println!("---- BootInfo hand-off ------------------------------------------");
    println!("  address         : {info:p}");
    println!(
        "  magic / rev     : {:#018x} / {} (valid: {})",
        info.magic,
        info.revision,
        info.is_valid()
    );
    println!("  arch            : {:?}", info.arch);

    let fb = &info.framebuffer;
    if fb.is_present() {
        println!("  fb base / size  : {:#018x} / {} bytes", fb.base, fb.size);
        println!(
            "  fb geometry     : {}x{} px, stride {} px, format {:?}",
            fb.width, fb.height, fb.stride, fb.format
        );
    } else {
        println!("  framebuffer     : absent (headless)");
    }

    if info.acpi_rsdp != 0 {
        println!("  ACPI RSDP       : {:#018x}", info.acpi_rsdp);
    } else {
        println!("  ACPI RSDP       : not found");
    }
    println!("  device tree     : {:#018x}", info.device_tree);
    println!("-----------------------------------------------------------------");
}

/// Печатает карту прав, которая уходит в `BootInfo::kernel`.
///
/// Это единственное место, где видно, что именно ядро получит для W^X: после
/// выхода из boot services консоли уже нет, а ядро на этом этапе умеет
/// печатать далеко не сразу.
fn print_kernel_segments(handoff: &Handoff, segments: &[KernelSegment]) {
    if segments.is_empty() {
        println!("  kernel segments : none -- the kernel will not be able to apply W^X");
        return;
    }

    println!(
        "  kernel segments : {} at {:#018x}",
        segments.len(),
        handoff.segments_address()
    );
    for (index, seg) in segments.iter().enumerate() {
        println!(
            "    seg {index}        : {:#018x}..{:#018x} {} ({} bytes)",
            seg.base,
            seg.base + seg.len,
            elf::perms(seg.flags),
            seg.len
        );
    }

    // Страница, оказавшаяся одновременно записываемой и исполняемой, сводит
    // W^X на нет. Причины и разбор конфликта — в `elf::build_segments`; здесь
    // остаётся только итог, чтобы он не потерялся среди строк выше.
    let violations = segments
        .iter()
        .filter(|seg| seg.is_writable() && seg.is_executable())
        .count();
    if violations > 0 {
        println!("  !! {violations} segment(s) are both writable and executable -- W^X is degraded");
    }
}

/// Держит сообщение об ошибке на экране, не блокируя автоматический прогон.
fn linger(seconds: u32) {
    println!("");
    println!("Returning to the firmware in {seconds}s");
    boot::stall(Duration::from_secs(u64::from(seconds)));
}

// --- Заглушки, которые требует кодогенерация -------------------------------

/// Реализация `wcslen` для оптимизатора.
///
/// В release-сборке LLVM распознаёт цикл вычисления длины UTF-16 строки (крейт
/// `uefi` повсеместно работает с `CStr16`) и заменяет его вызовом libc-функции
/// `wcslen`. В bare-metal окружении libc нет, и линковка падает с `undefined
/// symbol: wcslen` — причём только в release, debug эту замену не делает.
/// В UEFI символ строки всегда 16-битный, поэтому реализация тривиальна.
#[unsafe(no_mangle)]
extern "C" fn wcslen(s: *const u16) -> usize {
    let mut len = 0;
    // SAFETY: контракт C-функции обязывает вызывающего передать указатель на
    // нуль-терминированную строку; за терминатор мы не читаем.
    unsafe {
        while *s.add(len) != 0 {
            len += 1;
        }
    }
    len
}
