//! Монитор системы: счётчики ядра в окне, которое рисует программа.
//!
//! # Что здесь изменилось по сравнению с прошлой фазой
//!
//! Это окно было **частью ядра**. Модуль `ui` заводил его при старте стола,
//! оболочка раз в секунду собирала строку из `mm::frame`, `mm::heap` и
//! планировщика и печатала её в текстовую сетку. Теперь окно заводит программа,
//! числа приезжают одним системным вызовом, а рисует их тоже она.
//!
//! Смысл переезда не в красоте. Монитор — это разбор и показ чужих чисел, и
//! падать он обязан вместе со своим окном, а не вместе с машиной.
//!
//! # Фаза С8: окно, а не терминал
//!
//! До неё монитор рисовал семь строк встроенным шрифтом 8×8 по точке — и
//! выглядел кусочком консоли посреди стола, где всё остальное нарисовано
//! сглаженным шрифтом, полосами и карточками. Теперь он собран из того же
//! набора, что диспетчер задач (`mini_ui::kit`), а окно и цикл — у
//! `user_progs::app`. Строки журнала прежние: на окне «System» стоит десяток
//! шагов стенда.
//!
//! # Почему числа приезжают одной структурой
//!
//! Потому что показываются они **рядом**, и картина, склеенная из десяти
//! снимков, взятых в разные мгновения, врёт тем убедительнее, чем быстрее
//! меняется система. Один вызов — один миг.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use mini_ui::kit::{self, Frame};
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{SYSINFO_DARK, SysInfo, println};

/// Имя окна — латиницей и именно это слово: по нему стенд наводит мышь
/// (`Aim::Title("System")`), и оно же стояло у окна, пока оно было частью ядра.
const SPEC: Spec = Spec {
    name: "sysmon",
    title: "System",
    // Кадр монитора стоит заметно в отладочной сборке, а числа, меняющиеся
    // быстрее, чем их успевают прочесть, ничего не сообщают.
    period_ms: 5_000,
};

/// Размер окна. Три полосы и строка фактов — и не больше: окно, которое
/// стоит дорого, съедает машину, а монитор съедать её не должен.
const WIDTH: u32 = 360;
const HEIGHT: u32 = 300;

struct Monitor {
    info: SysInfo,
}

impl App for Monitor {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        let info = &self.info;
        s.fill(area, theme::window_bg());

        // Панели сверху у монитора нет: заголовок окна уже говорит, что это,
        // а место под три полосы в маленьком окне дороже.
        let frame = Frame::new(ctx, area);
        let body = Rect::new(area.x, area.y, area.w, (frame.status.y - area.y).max(0) as u32);
        let pad = ctx.px(14);
        let x = body.x + pad as i32;
        let room = body.w.saturating_sub(pad * 2);
        let mut y = body.y + ctx.px(18) as i32;

        let mib = 1024 * 1024;
        let memory_used = info.frames_total.saturating_sub(info.frames_free);
        let heap_used = info.heap_size.saturating_sub(info.heap_free);
        let bars = [
            ("ПАМЯТЬ", memory_used, info.frames_total, format!("{} МиБ свободно из {}", info.frames_free / mib, info.frames_total / mib), Tone::Accent),
            ("КУЧА ЯДРА", heap_used, info.heap_size, format!("{} КиБ свободно", info.heap_free / 1024), Tone::Ok),
            ("ПАМЯТЬ УСТРОЙСТВ", info.dma_used, info.dma_total, format!("{} из {} КиБ", info.dma_used / 1024, info.dma_total / 1024), Tone::Warn),
        ];
        let scale = |value: u64| u32::try_from(value / 4096).unwrap_or(u32::MAX);
        // Подпись и полоса — с тем же шагом, что во вкладке производительности
        // диспетчера задач. Первая версия ставила полосу через восемь точек, и
        // на снимке она срезала подписи снизу.
        for (title, done, total, text, tone) in bars {
            paint::caps(ctx, s, x, y, title);
            paint::text_right(ctx, s, Role::Caption, x + room as i32, y, &text, p.ink3);
            y += ctx.px(20) as i32;
            paint::progress(ctx, s, Rect::new(x, y, room, ctx.px(10)), scale(done), scale(total), tone);
            y += ctx.px(32) as i32;
        }

        let facts = [
            format!("Работает {}", uptime_text(info.uptime_ms)),
            format!("задач {} · окон {}", info.tasks_alive, info.windows),
            format!("клавиш {} принято, {} потеряно", info.keys_posted, info.keys_dropped),
        ];
        let step = i32::from(ctx.face(Role::Body).line) + ctx.px(2) as i32;
        for fact in facts {
            if y + step > body.bottom() {
                break;
            }
            paint::text_clipped(ctx, s, Role::Body, x, y, room, &fact, p.ink2);
            y += step;
        }

        let status: String = format!("Обновляется раз в {} с    Q — закрыть", SPEC.period_ms / 1000);
        kit::status_bar(ctx, s, frame.status, &status);
    }

    fn tick(&mut self, info: &SysInfo) -> bool {
        self.info = *info;
        true
    }
}

/// Напечатать счётчики в журнал — по разу, при запуске.
///
/// Печатается то, чего **не может быть у программы своего**: свободная память
/// машины, живые задачи, тема стола. Число, взятое из воздуха, выглядело бы
/// здесь точно так же, поэтому в журнал едут именно они.
fn report(info: &SysInfo) {
    println(&format!(
        "sysmon: memory {} MiB free of {} MiB",
        info.frames_free / (1024 * 1024),
        info.frames_total / (1024 * 1024)
    ));
    println(&format!("sysmon: tasks {} alive", info.tasks_alive));
    println(&format!("sysmon: theme {}", if info.flags & SYSINFO_DARK != 0 { "dark" } else { "light" }));
}

fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    // Окно не больше экрана: на крошечном экране монитор всё равно откроется.
    let size = (WIDTH.min(info.screen_w.max(320)), HEIGHT.min(info.screen_h.max(200)));
    app::run(&SPEC, &info, size, move |_, _| {
        // Первые числа — в журнал, по одному разу и после строки об окне. Это
        // и есть доказательство, что счётчики ядра доехали до программы.
        report(&info);
        Monitor { info }
    })
}
