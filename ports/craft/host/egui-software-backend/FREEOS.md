# egui_software_backend 0.0.3 — копия для приложений Craft (egui 0.37)

Источник: crates.io `egui_software_backend` 0.0.3,
https://github.com/DGriffin91/egui_software_backend (MIT OR Apache-2.0,
тексты лицензий рядом). Та же копия, что у OBSOS
(`ports/servo/egui-software-backend`, egui 0.34), перенесённая на egui 0.37:
зависимость `egui = "0.37"` (у Craft её разрешает форк storytold/egui через
`[patch]` приложения), без окна winit/softbuffer и без `test_render`
(`src/winit.rs`, `src/test_render.rs` убраны) — окно даёт eframe FossAi
(`../eframe`).

Правка OBSOS (09.11.2026, `render_to_canvas`) сохранена: холст пересобирается и
тогда, когда фигура исчезла, а новых не появилось.
