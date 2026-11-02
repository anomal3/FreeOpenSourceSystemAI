//! Окно, которое не отвечает (02.11, проверка «Программа не отвечает»).
//!
//! Открывает окно, рисует его один раз и больше никогда не читает события:
//! просьба закрыться (`WIN_CLOSE`) остаётся в очереди, и стол через десять
//! секунд спрашивает «Завершить программу?» (макет 18). Нужна сценарию
//! `hang` — завешивать настоящую программу ради проверки нечем.

#![no_std]
#![no_main]

extern crate alloc;

use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, Spec};
use user_progs::{Window, nanosleep, println};

const SPEC: Spec = Spec { name: "stuck", title: "Stuck", period_ms: 1_000 };

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let _info = app::start(&SPEC);
    let (width, height) = (420, 160);
    let mut window = loop {
        match Window::open(SPEC.title, width, height) {
            Ok(window) => break window,
            Err(_) => {
                nanosleep(0, 50_000_000);
            }
        }
    };
    let base = window.pixels().as_mut_ptr();
    // SAFETY: ядро отобразило ровно `width * height` точек по этому адресу и
    // держит их, пока живо окно.
    if let Some(mut surface) = unsafe { Surface::from_raw(base, width, height) } {
        let area = Rect::new(0, 0, width, height);
        surface.fill(area, theme::window_bg());
        let ctx = Ctx::scaled(1);
        paint::text(ctx, &mut surface, Role::Body, 20, 60, "Это окно не отвечает на события.", ctx.palette.ink);
        let _ = window.commit();
    }
    println("stuck: window shown, not reading events");
    // Никогда не читать очередь: ни щелчков, ни просьбы закрыться.
    loop {
        let _ = nanosleep(1, 0);
    }
}
