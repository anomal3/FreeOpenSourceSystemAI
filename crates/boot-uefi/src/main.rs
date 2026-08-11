//! FreeOS UEFI bootloader — Phase 0.
//!
//! Задача этой фазы: доказать, что образ грузится обеими прошивками (OVMF на
//! x86-64 и pftf/RPi4 на ARM64), что консоль и графика доступны, и что
//! [`boot_info::BootInfo`] можно собрать целиком, кроме карты памяти.
//!
//! Ядра ещё не существует, поэтому `ExitBootServices` здесь НЕ вызывается:
//! после выхода из boot services нет ни консоли, ни аллокатора, ни куда
//! передать управление — приложение просто вернуло бы `Status::SUCCESS` в
//! мёртвом окружении. Место, где это появится, помечено ниже как Phase 1.
//!
//! Весь текст, который уходит в консоль, намеренно ASCII: прошивка выводит
//! stdout ещё и на последовательный порт, преобразуя UCS-2 в однобайтовую
//! кодировку, и не-ASCII символы превращаются там в мусор или в `?`.

#![no_std]
#![no_main]

use core::time::Duration;

use boot_info::{Arch, BootInfo, Framebuffer, PixelFormat};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};
use uefi::table::cfg::ConfigTableEntry;
use uefi::{Status, boot, entry, print, println, system};

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

/// UEFI GOP всегда отдаёт 32 бита на пиксель для форматов Rgb/Bgr.
const BYTES_PER_PIXEL: usize = 4;

/// Толщина рамки по краю экрана, в пикселях.
const BORDER: usize = 4;

/// Классические цветные полосы в логическом порядке (r, g, b). Набор подобран
/// так, чтобы перепутанный порядок каналов бросался в глаза: при подмене
/// R и B жёлтый станет голубым, а красный — синим.
const BARS: [(u8, u8, u8); 8] = [
    (255, 255, 255), // white
    (255, 255, 0),   // yellow
    (0, 255, 255),   // cyan
    (0, 255, 0),     // green
    (255, 0, 255),   // magenta
    (255, 0, 0),     // red
    (0, 0, 255),     // blue
    (0, 0, 0),       // black
];

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
    info.framebuffer = probe_framebuffer();
    info.acpi_rsdp = find_acpi_rsdp();

    print_boot_info(&info);

    // ─────────────────────────── Phase 1 ────────────────────────────────────
    // TODO(Phase 1): здесь появляется настоящая передача управления ядру.
    // Порядок строго такой, и всё это должно уместиться между последним
    // выводом в консоль и первой инструкцией ядра:
    //
    //   1. Загрузить образ ядра (SimpleFileSystem) и разметить его сегменты.
    //   2. `boot::memory_map(MemoryType::LOADER_DATA)` — снимок карты памяти.
    //      Буфер выделяется boot services, поэтому он сам попадает в карту как
    //      LOADER_DATA => MemoryKind::BootloaderReclaimable.
    //   3. Сконвертировать `MemoryDescriptor` -> `boot_info::MemoryRegion`
    //      (слияние соседних одинаковых диапазонов, сортировка по `start`) и
    //      записать `info.memory_map = MemoryMap { ptr, len }`.
    //   4. `unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) }`.
    //      После этого вызова консоль, аллокатор и любые протоколы мертвы —
    //      ничего из uefi::boot/uefi::system больше вызывать нельзя.
    //   5. Прыжок: `kernel_main(&info)` по загруженному entry point.
    //
    // До появления ядра ничего из этого делать нельзя: без п.5 выход из boot
    // services оставил бы машину без единственного способа что-либо сообщить.
    // ────────────────────────────────────────────────────────────────────────

    pause(10);

    Status::SUCCESS
}

fn arch_name() -> &'static str {
    match ARCH {
        Arch::X86_64 => "x86_64",
        Arch::AArch64 => "aarch64",
    }
}

fn print_banner() {
    println!("================================================================");
    println!("  FreeOS bootloader (boot-uefi) -- Phase 0 bring-up");
    println!("================================================================");
    println!("  target arch     : {}", arch_name());
    println!("  UEFI revision   : {}", system::uefi_revision());
    println!("  firmware vendor : {}", system::firmware_vendor());
    println!("  firmware rev    : {:#010x}", system::firmware_revision());
    println!("");
}

/// Открывает GOP, описывает текущий режим и рисует тестовую картинку.
///
/// Headless-машина (или прошивка без GOP) — не ошибка на этой фазе: возвращаем
/// [`Framebuffer::NONE`], ядро потом само решит, что делать без экрана.
fn probe_framebuffer() -> Framebuffer {
    let handle = match boot::get_handle_for_protocol::<GraphicsOutput>() {
        Ok(handle) => handle,
        Err(err) => {
            println!("  [gop] no GraphicsOutput handle ({err:?}) -- headless boot");
            return Framebuffer::NONE;
        }
    };

    let mut gop = match boot::open_protocol_exclusive::<GraphicsOutput>(handle) {
        Ok(gop) => gop,
        Err(err) => {
            println!("  [gop] cannot open GraphicsOutput ({err:?}) -- headless boot");
            return Framebuffer::NONE;
        }
    };

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    // stride может быть больше width: прошивка выравнивает начало строки, и
    // невидимый «хвост» каждой строки всё равно занимает память.
    let stride = mode.stride();

    // Порядок здесь принципиален: у uefi-rs `frame_buffer()` паникует в
    // Blt-only режиме, поэтому формат проверяем ДО обращения к памяти. Это не
    // теоретический случай — именно так ведёт себя virtio-gpu на QEMU virt.
    // Bitmask потребовал бы разбора масок каналов, чего контракт BootInfo не
    // передаёт; оба режима означают «линейного фреймбуфера нет».
    let format = match mode.pixel_format() {
        GopPixelFormat::Rgb => PixelFormat::Rgb,
        GopPixelFormat::Bgr => PixelFormat::Bgr,
        GopPixelFormat::Bitmask => {
            println!("  [gop] {width}x{height} px, channel-mask format -- no linear framebuffer");
            return Framebuffer::NONE;
        }
        GopPixelFormat::BltOnly => {
            println!("  [gop] {width}x{height} px, Blt-only mode -- no linear framebuffer");
            return Framebuffer::NONE;
        }
    };

    let mut raw = gop.frame_buffer();
    let base = raw.as_mut_ptr();
    let size = raw.size();

    let framebuffer = Framebuffer {
        base: base as usize as u64,
        size: size as u64,
        width: width as u32,
        height: height as u32,
        stride: stride as u32,
        format,
    };

    println!(
        "  [gop] {}x{} px, stride {} px, {} bytes @ {:#018x}",
        framebuffer.width, framebuffer.height, framebuffer.stride, framebuffer.size, framebuffer.base
    );

    println!("  [gop] drawing test pattern: bars are white/yellow/cyan/green/magenta/red/blue/black");
    // SAFETY: `base`/`size` только что получены у живого GOP, описывают
    // линейный фреймбуфер текущего режима и остаются валидными, пока `raw`
    // и `gop` не уронены — а роняются они ниже по стеку. Формат к этому месту
    // заведомо Rgb или Bgr, то есть 32-битные пиксели; геометрия из того же `mode`.
    unsafe { draw_test_pattern(base, &framebuffer) };

    // `raw` ронять отдельно не нужно: FrameBuffer ничем не владеет, а
    // заимствование `gop` заканчивается на последнем обращении к нему.
    // Протокол закрываем явно — дальше фреймбуфер адресуется физически.
    drop(gop);

    framebuffer
}

/// Рисует цветные полосы с градиентом сверху и рамку по периметру экрана.
///
/// # Safety
///
/// `base` должен указывать на доступный для записи линейный фреймбуфер длиной
/// не менее `fb.size` байт, геометрия которого в точности описана `fb`
/// (32 бита на пиксель, `fb.stride` пикселей на строку). `fb.format` не должен
/// быть [`PixelFormat::Unknown`].
unsafe fn draw_test_pattern(base: *mut u8, fb: &Framebuffer) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    if width == 0 || height == 0 {
        return;
    }

    // Полоса сверху: достаточно заметная, но не закрывающая консольный текст.
    let band = (height / 4).clamp(16, 160).min(height);
    let bars_end = band / 2;

    for y in 0..band {
        for x in 0..width {
            let (r, g, b) = if y < bars_end {
                BARS[(x * BARS.len() / width).min(BARS.len() - 1)]
            } else {
                let level = (x * 255 / width) as u8;
                (level, level, level)
            };
            // SAFETY: требования делегированы вызывающему (см. контракт этой
            // функции); `put_pixel` дополнительно отсекает запись за `fb.size`.
            unsafe { put_pixel(base, fb, x, y, r, g, b) };
        }
    }

    // Рамка доказывает, что видны настоящие границы экрана, а не первые
    // несколько строк памяти.
    let thickness = BORDER.min(height / 2).min(width / 2).max(1);
    for y in 0..height {
        if y < thickness || y + thickness >= height {
            for x in 0..width {
                // SAFETY: см. выше.
                unsafe { put_pixel(base, fb, x, y, 0, 255, 128) };
            }
        } else {
            for x in 0..thickness {
                // SAFETY: см. выше.
                unsafe { put_pixel(base, fb, x, y, 0, 255, 128) };
                // SAFETY: см. выше; `thickness <= width / 2`, поэтому
                // `width - 1 - x` не уходит в underflow.
                unsafe { put_pixel(base, fb, width - 1 - x, y, 0, 255, 128) };
            }
        }
    }
}

/// # Safety
///
/// Те же требования, что и у [`draw_test_pattern`].
#[inline]
unsafe fn put_pixel(base: *mut u8, fb: &Framebuffer, x: usize, y: usize, r: u8, g: u8, b: u8) {
    // Адресация идёт через stride, а не через width. Строки фреймбуфера часто
    // дополнены невидимыми пикселями, и `y * width + x` на таком мониторе даёт
    // характерный «косой» сдвиг картинки с каждой следующей строкой.
    let offset = (y * fb.stride as usize + x) * BYTES_PER_PIXEL;
    if offset + BYTES_PER_PIXEL > fb.size as usize {
        return;
    }

    // Порядок байт канала задаёт прошивка, и ошибиться здесь — значит получить
    // синее вместо красного; конвертируем явно, а не полагаясь на «обычно BGR».
    let pixel: [u8; 4] = match fb.format {
        PixelFormat::Bgr => [b, g, r, 0],
        _ => [r, g, b, 0],
    };

    // SAFETY: проверка выше гарантирует `offset + 4 <= fb.size`, поэтому запись
    // целиком попадает внутрь фреймбуфера, валидность которого гарантирует
    // вызывающий. У `[u8; 4]` выравнивание 1, так что любое смещение корректно
    // выровнено. `write_volatile` не даёт оптимизатору выбросить запись в
    // память устройства, которую он считает никем не читаемой.
    unsafe {
        core::ptr::write_volatile(base.add(offset).cast::<[u8; 4]>(), pixel);
    }
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
    println!(
        "  memory map      : {} regions @ {:#018x} (filled in Phase 1)",
        info.memory_map.len, info.memory_map.ptr
    );
    println!("-----------------------------------------------------------------");
}

/// Держит картинку на экране `seconds` секунд, прерываясь на первой клавише.
///
/// Опрос вместо ожидания события: `read_key` возвращает `Ok(None)` вместо
/// блокировки, поэтому пауза гарантированно заканчивается сама и не вешает
/// автоматический прогон в QEMU, где клавиши нажимать некому.
fn pause(seconds: u32) {
    println!("");
    print!("Holding for {seconds}s (press any key to continue)");

    'outer: for _ in 0..seconds {
        for _ in 0..10 {
            if key_pressed() {
                break 'outer;
            }
            boot::stall(Duration::from_millis(100));
        }
        print!(".");
    }

    println!("");
}

fn key_pressed() -> bool {
    system::with_stdin(|stdin| matches!(stdin.read_key(), Ok(Some(_))))
}
