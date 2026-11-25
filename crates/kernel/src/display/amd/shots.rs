//! Снимки экрана во время AMD-теста: «что видно на мониторе» (25.11, Роман: «изображения не было,
//! везде мерцания»). Лог этого не показывает, поэтому во время проверок ядро читает видимый
//! framebuffer прошивки (то, что сканирует монитор) и пишет кадры в `\FREEOS\SHOTS.BIN` на томе
//! логов флешки (файл заранее нужного размера, см. `usb::stick_log`).
//!
//! Серия - три кадра через 40 мс: так видно и «неверная картинка», и «меняется от кадра к кадру».
//! Каждый кадр - вдвое уменьшенный, RGB по 3 байта, в своём слоте по [`SLOT_BYTES`]:
//! 64 байта заголовка (`FSHT`, ширина, высота, время мс, имя шага) и пиксели.

use alloc::vec::Vec;

use boot_info::{Framebuffer, PixelFormat};

use crate::sync::SpinLock;

/// Байт на слот (кратно сектору): 400x300 - это 360 КБ.
const SLOT_BYTES: usize = 768 * 1024;
const HEADER: usize = 64;

static SCREEN: SpinLock<Option<Framebuffer>> = SpinLock::new(None);
static NEXT_SLOT: SpinLock<usize> = SpinLock::new(0);

/// Запомнить framebuffer прошивки (адрес уже виртуальный).
pub fn set_screen(framebuffer: Framebuffer) {
    if framebuffer.is_present() {
        *SCREEN.lock() = Some(framebuffer);
    }
}

/// Снять один кадр экрана в слот; `false` - нечего снимать или писать некуда.
fn one(tag: &str) -> bool {
    let Some(fb) = *SCREEN.lock() else { return false };
    let Some(file) = crate::usb::stick_log::shots_size() else { return false };
    let slot = {
        let mut next = NEXT_SLOT.lock();
        if (*next + 1) * SLOT_BYTES > file {
            return false;
        }
        let slot = *next;
        *next += 1;
        slot
    };
    // Уменьшить так, чтобы влезло в слот: каждый `step`-й пиксель по обеим осям.
    let mut step = 2usize;
    while (fb.width as usize / step) * (fb.height as usize / step) * 3 > SLOT_BYTES - HEADER {
        step += 1;
    }
    let (width, height) = (fb.width as usize / step, fb.height as usize / step);
    let mut out: Vec<u8> = Vec::new();
    if out.try_reserve_exact(HEADER + width * height * 3).is_err() {
        return false;
    }
    out.resize(HEADER, 0);
    out[..4].copy_from_slice(b"FSHT");
    out[4..8].copy_from_slice(&(width as u32).to_le_bytes());
    out[8..12].copy_from_slice(&(height as u32).to_le_bytes());
    out[12..20].copy_from_slice(&crate::time::uptime_ms().to_le_bytes());
    let name = tag.as_bytes();
    let take = name.len().min(40);
    out[20..20 + take].copy_from_slice(&name[..take]);
    let mut row = alloc::vec![0u32; fb.width as usize];
    for y in 0..height {
        let from = fb.base as usize + (y * step) * fb.stride as usize * 4;
        // SAFETY: строка внутри framebuffer прошивки (`height * step <= fb.height`, `width <= stride`),
        // он отображён ядром и жив всё время работы.
        unsafe { core::ptr::copy_nonoverlapping(from as *const u32, row.as_mut_ptr(), fb.width as usize) };
        for x in 0..width {
            let pixel = row[x * step];
            let (b0, b1, b2) = (pixel as u8, (pixel >> 8) as u8, (pixel >> 16) as u8);
            // Порядок на выходе всегда R, G, B.
            let rgb = if matches!(fb.format, PixelFormat::Rgb) { [b0, b1, b2] } else { [b2, b1, b0] };
            out.extend_from_slice(&rgb);
        }
    }
    crate::usb::stick_log::shots_write(slot * SLOT_BYTES, &out)
}

/// Серия из трёх кадров через 40 мс с подписью `tag` (имя шага проверки).
pub fn burst(tag: &str) {
    let mut saved = 0;
    for _ in 0..3 {
        if one(tag) {
            saved += 1;
        }
        crate::sched::sleep_ms(40);
    }
    if saved > 0 {
        crate::kprintln!("  amdgpu     : shots: {saved} screen frames of '{tag}' saved to FREEOS\\SHOTS.BIN");
    }
}
