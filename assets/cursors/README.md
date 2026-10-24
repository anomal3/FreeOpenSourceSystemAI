# Курсоры FossAi

Из набора Романа «FossAi: оформление ОС» (`cursors/*.svg` — 32×32,
`cursors/large/*.svg` — 48×48; нарисованы с нуля, лицензия CC0). PNG здесь —
те же SVG, отрисованные headless Chrome с прозрачным фоном; `*.fimg` рядом —
то, что вшивает ядро (`crates/kernel/src/ui/pointer.rs`). Точки прицела — из
`hotspots.json` набора, они же записаны в `pointer.rs`.

Пересобрать FIMG после замены PNG: `cargo xtask wallpaper`.
