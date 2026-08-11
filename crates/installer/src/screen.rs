//! Экран установщика: фреймбуфер от GOP и поверхность, из которой он
//! собирается.
//!
//! # Почему рисуем сами, а не через `GOP::blt`
//!
//! `Blt` умеет всё, что нужно, и делает это силами прошивки. Но он же —
//! единственное, что у нас общего с прошивкой в области графики, и опираться
//! на него значило бы иметь два разных механизма рисования: один в
//! установщике, другой в ядре, где никакого `Blt` уже нет. Установщик берёт у
//! GOP только адрес и геометрию линейного буфера — ровно то же, что берёт
//! загрузчик, — и дальше рисует тем же кодом, что и композитор системы.
//!
//! # Полная перерисовка
//!
//! Экран собирается целиком на каждое нажатие. Учёт изменённого, ради которого
//! в композиторе ядра заведён отдельный механизм, здесь не нужен: там за
//! кадром стоит поток вывода терминала, а тут — человек, который нажимает
//! клавишу раз в секунду. Полторы тысячи килобайт на нажатие незаметны, а вот
//! рассинхронизация экрана с состоянием была бы заметна очень.

use boot_info::{Framebuffer, PixelFormat};
use mini_ui::widget::Metrics;
use mini_ui::{Rect, Screen, Surface};
use uefi::boot;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat as GopPixelFormat};

use crate::logln;

/// Экран вместе с поверхностью, в которую он собирается.
pub struct Display {
    screen: Screen,
    surface: Surface,
    metrics: Metrics,
}

impl Display {
    /// Найти фреймбуфер и подготовить поверхность.
    ///
    /// `None` означает, что рисовать негде: у машины нет GOP, режим Blt-only
    /// (линейного буфера в адресном пространстве не существует) или не хватило
    /// памяти под поверхность размером с экран. Установщик в этом случае
    /// обязан сказать об этом словами, а не показать чёрный экран.
    #[must_use]
    pub fn open() -> Option<Self> {
        let framebuffer = probe()?;
        let screen = Screen::new(&framebuffer)?;
        let metrics = Metrics::for_screen(screen.width(), screen.height());
        let surface = Surface::new(
            screen.width(),
            screen.height(),
            mini_ui::widget::DARK.background,
        )?;
        logln!(
            "[gop] {}x{} px, glyph scale {}",
            screen.width(),
            screen.height(),
            metrics.scale
        );
        Some(Self { screen, surface, metrics })
    }

    #[must_use]
    pub const fn metrics(&self) -> Metrics {
        self.metrics
    }

    #[must_use]
    pub fn surface(&mut self) -> &mut Surface {
        &mut self.surface
    }

    /// Вывести собранную поверхность на экран.
    pub fn present(&self) {
        let bounds = Rect::new(0, 0, self.surface.width(), self.surface.height());
        self.screen.blit(&self.surface, (0, 0), bounds);
    }
}

/// Спросить у прошивки линейный фреймбуфер.
fn probe() -> Option<Framebuffer> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>()
        .inspect_err(|err| logln!("[gop] no GraphicsOutput handle ({err:?})"))
        .ok()?;
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle)
        .inspect_err(|err| logln!("[gop] cannot open GraphicsOutput ({err:?})"))
        .ok()?;

    let mode = gop.current_mode_info();
    let (width, height) = mode.resolution();
    let stride = mode.stride();

    // Порядок принципиален: у uefi-rs `frame_buffer()` паникует в Blt-only
    // режиме, поэтому формат проверяется ДО обращения к памяти. Это не
    // теоретический случай — именно так ведёт себя virtio-gpu на QEMU virt.
    let format = match mode.pixel_format() {
        GopPixelFormat::Rgb => PixelFormat::Rgb,
        GopPixelFormat::Bgr => PixelFormat::Bgr,
        GopPixelFormat::Bitmask => {
            logln!("[gop] channel-mask format: no linear framebuffer");
            return None;
        }
        GopPixelFormat::BltOnly => {
            logln!("[gop] Blt-only mode: no linear framebuffer");
            return None;
        }
    };

    let mut raw = gop.frame_buffer();
    let framebuffer = Framebuffer {
        base: raw.as_mut_ptr() as usize as u64,
        size: raw.size() as u64,
        width: width as u32,
        height: height as u32,
        stride: stride as u32,
        format,
    };

    // Протокол закрывается здесь же: дальше буфер адресуется физически. Он
    // остаётся действительным, потому что установщик не выходит из boot
    // services и режим не переключает, — то же допущение, на котором стоит
    // загрузчик.
    drop(gop);
    Some(framebuffer)
}
