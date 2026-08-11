//! Поиск линейного фреймбуфера через GOP и тестовая картинка на нём.
//!
//! Модуль целиком относится к диагностике Phase 0: он доказывает, что графика
//! доступна и что геометрия, которую загрузчик кладёт в [`BootInfo`], описывает
//! именно то, что видно на экране.
//!
//! [`BootInfo`]: boot_info::BootInfo

use boot_info::{Framebuffer, PixelFormat};
use uefi::boot;
use uefi::println;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};

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

/// Открывает GOP, описывает текущий режим и рисует тестовую картинку.
///
/// Headless-машина (или прошивка без GOP) — не ошибка: возвращаем
/// [`Framebuffer::NONE`], ядро потом само решит, что делать без экрана.
pub fn probe_framebuffer() -> Framebuffer {
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
