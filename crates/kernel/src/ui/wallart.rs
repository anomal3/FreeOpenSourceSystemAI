//! Обои FossAi из макета Романа (фаза 72a): картинка, вшитая в ядро.
//!
//! В ядре, а не файлом в `/usr/share`: обои нужны с первого кадра стола — и
//! на живой системе, и в установщике, и на машине, где корень ещё не
//! смонтирован, — а файл до этого места доехал бы тремя путями (RAM-диск,
//! установочный носитель, обновление). Картинка — QOI 1920×1080, 2,4 МиБ;
//! собирает её `cargo xtask wallpaper` из PNG макета.

use mini_ui::Surface;
use mini_ui::qoi::Picture;

use crate::kprintln;

/// Обои 1920×1080 (`assets/wallpaper/fossai-1920x1080.qoi`).
static FOSSAI: &[u8] = include_bytes!("../../../../assets/wallpaper/fossai-1920x1080.qoi");

/// Залить `surface` обоями FossAi с обрезкой по краям. `false` — картинку не
/// распаковать (нет памяти): стол нарисует обои по точкам.
pub fn paint(surface: &mut Surface) -> bool {
    let started = crate::time::uptime_ms();
    let Some(picture) = Picture::decode(FOSSAI) else {
        kprintln!("  desktop     : wallpaper picture did not unpack; drawing the gradient");
        return false;
    };
    picture.cover(surface);
    kprintln!(
        "  desktop     : wallpaper fossai {}x{} -> {}x{} in {} ms",
        picture.width,
        picture.height,
        surface.width(),
        surface.height(),
        crate::time::uptime_ms().saturating_sub(started)
    );
    true
}
