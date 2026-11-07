# `std` Rust для FreeOS — цель `<арх>-unknown-freeos`

Первый шаг нативного браузера на Servo (N2 в ROADMAP, «Браузер на Servo —
НАТИВНО»): обычные крейты (Servo, winit, egui, tokio…) требуют `std`.

**Схема — как у Redox и Managarm:** ОС объявлена семейством `unix` поверх
нашей libc (picolibc + `libc/freeos`), и `std` идёт готовой веткой
`std::sys::pal::unix`. picolibc — потомок newlib, поэтому `env = "newlib"`, и
в списках `std` FreeOS стоит рядом с Horizon (3DS) и Vita.

| Файл | Что это |
|---|---|
| `targets/<арх>-unknown-freeos.json` | спецификация цели; компоновка как у C++-программ набора (`freeos-cxx.ld`, `crt0.o`, `-lunwind -lfreeos -lc`) |
| `libc/freeos.rs` | модуль крейта `libc` (0.2.189): типы и раскладки структур сняты с заголовков набора пробой `sizeof`/`offsetof` (x86_64 и aarch64 совпали, кроме знака `wchar_t`), ≈950 констант — оттуда же |
| `std-os-freeos/` | модуль `std::os::freeos` (как у Horizon, `stat` со `st_atim`) |
| `check/` | программа `rstd` → `/bin/rstd`, сценарий `rust-std` |

Сборка — `xtask/src/ruststd.rs`, вместе с образом (`cargo xtask build`,
`cargo xtask test -s rust-std -a x86_64`): копия `library/` из `rust-src`
тулчейна с правками по якорям, `libc` с crates.io (хеш сверяется) с нашим
модулем, затем `-Zbuild-std=std,panic_abort`. Всё — в `build/rust-std`.

Чего пока нет: запуска процессов из `std::process` (у нас `spawn`, а не
`fork/exec`), сигналов, `current_exe`, `openat`-семейства, раскрутки паники
(`panic = abort`).

Лицензии: `libc` и `std` — MIT OR Apache-2.0 (правки здесь — под теми же).
