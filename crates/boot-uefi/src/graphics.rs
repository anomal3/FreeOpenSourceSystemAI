//! Поиск линейного фреймбуфера через GOP и тестовая картинка на нём.
//!
//! Модуль целиком относится к диагностике Phase 0: он доказывает, что графика
//! доступна и что геометрия, которую загрузчик кладёт в [`BootInfo`], описывает
//! именно то, что видно на экране.
//!
//! [`BootInfo`]: boot_info::BootInfo

use core::fmt::Write as _;

use boot_info::{Framebuffer, PixelFormat};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, SearchType};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};
use uefi::{Identify, println, system};

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

/// Разрешения, которые стол просит у прошивки, в порядке предпочтения.
///
/// Прошивка отдаёт по умолчанию то, что ей удобно, — обычно 800×600, а на
/// некоторых машинах и 640×480. Для системы, на которую человек смотрит, это
/// выглядит как неисправность, поэтому режим выбирается, а не принимается:
/// сначала привычные 1920×1080, затем 1280×720, затем 1024×768. Если ни одного
/// из них прошивка не предлагает, остаётся её собственный выбор — режим, в
/// котором она уже работает, заведомо рабочий.
///
/// Порядок начинается с 1280×720, а не с 1920×1080, по земной причине: окно
/// эмулятора размером в целый экран не помещается на экран, за которым сидит
/// человек, и половина стола оказывается за краем. Когда в «Параметрах»
/// появится выбор разрешения, порядок станет всего лишь значением по умолчанию.
///
/// Сам по себе список действует только на прошивке EDK II — см. [`choose_mode`]:
/// настоящая машина остаётся в режиме своей прошивки.
const WANTED_MODES: [(usize, usize); 3] = [(1280, 720), (1920, 1080), (1024, 768)];

/// Как выбирать режим экрана.
#[derive(Clone, Copy)]
pub enum Policy {
    /// Выбрать самим: сначала то, что человек просил в «Параметрах», затем
    /// [`WANTED_MODES`].
    Choose(Option<(usize, usize)>),
    /// Оставить режим прошивки — пятый пункт меню загрузчика.
    Keep,
}

/// Установить самый желанный из режимов, которые предлагает прошивка.
///
/// Молчаливого отказа здесь нет: если запрошенный режим не установился, об этом
/// печатается строка. «Экран не того размера» и «экран не переключился» — разные
/// неисправности, и различить их потом будет нечем.
fn choose_mode(gop: &mut GraphicsOutput, policy: Policy) {
    let (width, height) = gop.current_mode_info().resolution();
    let preferred = match policy {
        Policy::Keep => {
            println!("  [gop] keeping the firmware's {width}x{height}: asked in the boot menu");
            return;
        }
        Policy::Choose(preferred) => preferred,
    };
    // Сами, без просьбы человека, режим меняем только на прошивке EDK II:
    // это QEMU (OVMF и AAVMF) и VirtualBox, на которых переключение проверено
    // сотнями прогонов. Настоящая машина остаётся в режиме своей прошивки — так
    // же поступают загрузчик Windows и Linux. Найдено на ASUS K53SD (AMI,
    // UEFI 2.0, Intel HD 3000, матрица 1366×768): после смены режима экран
    // гаснет навсегда — и текст загрузчика, и всё, что рисует ядро. Установщик
    // режим не трогает, и у него экран был. Первая попытка лечения — «не
    // уводить вниз режим крупнее 1280×720» — не помогла: прошивка, видимо,
    // стартует мельче, а отличить «мелкий режим прошивки» от «родного режима
    // матрицы» по списку режимов нельзя. Просьба человека из «Параметров»
    // по-прежнему выполняется, а если от неё экран гаснет, пятый пункт меню
    // загрузчика её обходит.
    if preferred.is_none() && !switching_trusted() {
        println!("  [gop] keeping the firmware's {width}x{height}: modes are switched only on EDK II firmware");
        return;
    }
    // Просьба человека идёт первой, а список по умолчанию — за ней: если
    // прошивка такого режима не предлагает, выбор всё равно состоится, а не
    // оставит экран в том, что дала прошивка.
    let wanted = preferred
        .into_iter()
        .chain(WANTED_MODES)
        .collect::<alloc::vec::Vec<_>>();
    for (width, height) in wanted {
        // Режим забирается из итератора копией: итератор заимствует протокол, а
        // `set_mode` требует его целиком, и держать оба сразу нельзя.
        let found = gop.modes().find(|mode| {
            mode.info().resolution() == (width, height)
                && matches!(
                    mode.info().pixel_format(),
                    GopPixelFormat::Rgb | GopPixelFormat::Bgr
                )
        });
        let Some(mode) = found else {
            continue;
        };
        if gop.current_mode_info().resolution() == (width, height) {
            return;
        }
        match gop.set_mode(&mode) {
            Ok(()) => {
                println!("  [gop] switched to {width}x{height}");
                return;
            }
            Err(err) => println!("  [gop] cannot switch to {width}x{height} ({err:?})"),
        }
    }
}

/// Проверено ли переключение режима на этой прошивке. См. [`choose_mode`].
fn switching_trusted() -> bool {
    let mut vendor = alloc::string::String::new();
    let _ = write!(vendor, "{}", system::firmware_vendor());
    vendor.contains("EDK II")
}

/// Напечатать, в каком режиме прошивка и какие режимы она предлагает.
///
/// Для пятого пункта меню. С машины, у которой гаснет экран, журнала нет —
/// есть фотография экрана, поэтому всё печатается до того, как хоть что-то
/// меняется. Протокол открывается без захвата: захват отключил бы от него
/// графическую консоль прошивки, и напечатанное пропало бы с экрана.
pub fn show_modes() {
    let handles = boot::locate_handle_buffer(SearchType::ByProtocol(&GraphicsOutput::GUID))
        .map_or(0, |handles| handles.len());
    let Ok(handle) = boot::get_handle_for_protocol::<GraphicsOutput>() else {
        println!("  [gop] no GraphicsOutput handle");
        return;
    };
    let params = OpenProtocolParams {
        handle,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: `GetProtocol` не делает загрузчик потребителем устройства и
    // ничего от него не отключает; протокол только читается и закрывается в
    // конце функции.
    let Ok(gop) = (unsafe { boot::open_protocol::<GraphicsOutput>(params, OpenProtocolAttributes::GetProtocol) })
    else {
        println!("  [gop] cannot open GraphicsOutput");
        return;
    };
    let info = gop.current_mode_info();
    let (width, height) = info.resolution();
    println!(
        "  [gop] {handles} GraphicsOutput handle(s); firmware mode {width}x{height}, stride {}, {:?}",
        info.stride(),
        info.pixel_format()
    );
    // По шесть режимов в строке: у OVMF их три десятка, а экран в текстовом
    // режиме прошивки бывает высотой в двадцать пять строк.
    let mut line = alloc::string::String::new();
    let mut in_line = 0;
    for (index, mode) in gop.modes().enumerate() {
        let (mode_width, mode_height) = mode.info().resolution();
        let linear = matches!(mode.info().pixel_format(), GopPixelFormat::Rgb | GopPixelFormat::Bgr);
        let _ = write!(line, " {index}:{mode_width}x{mode_height}{}", if linear { "" } else { "*" });
        in_line += 1;
        if in_line == 6 {
            println!("  [gop] modes{line}");
            line.clear();
            in_line = 0;
        }
    }
    if !line.is_empty() {
        println!("  [gop] modes{line}");
    }
    println!("  [gop] (* = no linear framebuffer)");
}

/// Что человек выбрал в «Параметрах» — из файла `\FREEOS\DISPLAY.CFG`.
///
/// Файл пишет ядро (см. `kernel::slot::request_screen_mode`), а читается он
/// здесь и только здесь: разрешение задаётся до того, как ядро существует.
/// Ошибок эта функция не знает — нет файла, нечитаемая строка, чужие цифры —
/// всё это означает одно: человек ничего не просил.
pub fn requested_mode() -> Option<(usize, usize)> {
    let mut volume = crate::volume::BootVolume::open().ok()?;
    let path = uefi::cstr16!("\\FREEOS\\DISPLAY.CFG");
    let mut file = volume.open_regular(path).ok()??;
    let mut buffer = [0u8; 32];
    let read = file.read(&mut buffer).ok()?;
    let text = core::str::from_utf8(&buffer[..read]).ok()?;
    let line = text.lines().next()?.trim();
    let (width, height) = line.split_once(['x', 'X'])?;
    let width: usize = width.trim().parse().ok()?;
    let height: usize = height.trim().parse().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    println!("  [gop] {width}x{height} requested by the system");
    Some((width, height))
}

/// Открывает GOP, описывает текущий режим и рисует тестовую картинку.
///
/// Headless-машина (или прошивка без GOP) — не ошибка: возвращаем
/// [`Framebuffer::NONE`], ядро потом само решит, что делать без экрана.
pub fn probe_framebuffer(policy: Policy) -> Framebuffer {
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

    choose_mode(&mut gop, policy);

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
