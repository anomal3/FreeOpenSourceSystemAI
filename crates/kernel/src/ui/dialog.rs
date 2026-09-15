//! Окна, которые сообщают и спрашивают: «О системе», «Выключение», «Перезагрузка».
//!
//! # Почему не сетка символов
//!
//! До фазы С1 все три окна были текстовыми, как терминал. Сетка терминала
//! хранит байты ASCII — это свойство потока, который в неё печатают, — и
//! русский текст в ней выходил знаками вопроса, а ответ давали буквами `Y` и
//! `N`. Человек с любой другой настольной системы ищет в таком окне кнопки и не
//! находит их. Здесь текст рисуется шрифтом интерфейса, а отвечают кнопкой.
//!
//! Клавиатура при этом не отнята: Enter нажимает выбранную кнопку, Tab и
//! стрелки переводят выбор, Esc закрывает окно. Буквы `Y` и `N` у вопроса о
//! питании тоже остались — их разбирает `route` в [`super`], и на них стоит
//! стенд.
//!
//! # Раскладка считается одной функцией
//!
//! Как в «Параметрах»: [`DialogView::layout`] отвечает и отрисовке, и попаданию
//! щелчка. Две раскладки расходятся молча, и расхождение выглядит как «кнопка
//! не нажимается», хотя нажимается соседняя.

use alloc::format;
use alloc::string::String;

use mini_ui::draw;
use mini_ui::glyphicon::Icon;
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use crate::input::KeyCode;

/// Поля окна до текста и кнопок.
const PAD: u32 = 24;
/// Сторона плитки со значком слева от текста.
const TILE: u32 = 44;
/// Высота кнопки.
const BUTTON_H: u32 = 32;
/// Наименьшая ширина кнопки: «ОК» шириной в два знака в неё не целятся.
const BUTTON_MIN_W: u32 = 104;

/// Что показывает окно.
pub enum Dialog {
    /// Сведения о системе.
    About(AboutFacts),
    /// Вопрос о выключении (`restart: false`) или перезагрузке.
    Power { restart: bool },
}

/// Сведения «О системе» — снимок на момент открытия окна.
///
/// Снимок, а не живые числа: окно не перерисовывается по таймеру, и время
/// работы, которое сдвигалось бы только от щелчка по окну, выглядело бы
/// сломанным. Подпись у строки говорит об этом прямо.
pub struct AboutFacts {
    pub version: &'static str,
    pub arch: &'static str,
    pub screen: (u32, u32),
    pub free_mib: u64,
    pub total_mib: u64,
    pub uptime_ms: u64,
}

/// Чем ответили окну.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    /// «Закрыть», «Отмена», Esc.
    Close,
    /// «Выключить», «Перезагрузить».
    Confirm,
}

/// Кнопка на своём месте.
#[derive(Clone, Copy)]
struct Button {
    rect: Rect,
    answer: Answer,
    weight: Weight,
    label: &'static str,
}

/// Содержимое окна-диалога.
pub struct DialogView {
    dialog: Dialog,
    /// Кнопка, которую нажмёт Enter.
    focus: usize,
    /// Ответ, который стол ещё не забрал.
    answer: Option<Answer>,
}

impl DialogView {
    #[must_use]
    pub fn new(dialog: Dialog) -> Self {
        // У вопроса о питании выбрана «Отмена», а не «Выключить»: Enter, нажатый
        // по привычке в только что открывшемся окне, не должен гасить машину.
        let focus = match dialog {
            Dialog::Power { .. } => 1,
            Dialog::About(_) => 0,
        };
        Self { dialog, focus, answer: None }
    }

    /// Сколько у окна кнопок.
    const fn count(&self) -> usize {
        match self.dialog {
            Dialog::About(_) => 1,
            Dialog::Power { .. } => 2,
        }
    }

    /// Ответ кнопки с этим номером.
    const fn answer_at(&self, index: usize) -> Answer {
        match (&self.dialog, index) {
            (Dialog::Power { .. }, 0) => Answer::Confirm,
            _ => Answer::Close,
        }
    }

    /// Где стоят кнопки.
    ///
    /// Прижаты к правому нижнему углу и идут слева направо: действие, потом
    /// отмена — так стоят кнопки в диалогах, к которым человек привык.
    fn layout(&self, area: Rect, ctx: Ctx) -> [Option<Button>; 2] {
        let specs: [Option<(Answer, Weight, &'static str)>; 2] = match self.dialog {
            Dialog::About(_) => [Some((Answer::Close, Weight::Primary, "Закрыть")), None],
            Dialog::Power { restart } => [
                Some((
                    Answer::Confirm,
                    Weight::Danger,
                    if restart { "Перезагрузить" } else { "Выключить" },
                )),
                Some((Answer::Close, Weight::Normal, "Отмена")),
            ],
        };
        let pad = ctx.px(PAD);
        let h = ctx.px(BUTTON_H);
        let gap = ctx.px(8);
        let face = ctx.face(Role::Label);
        let widths = specs.map(|spec| {
            spec.map_or(0, |(_, _, label)| {
                (face.width(label) + ctx.px(36)).max(ctx.px(BUTTON_MIN_W))
            })
        });
        let total = widths[0] + widths[1] + if specs[1].is_some() { gap } else { 0 };
        let y = area.bottom() - (pad + h) as i32;
        let mut x = area.right() - (pad + total) as i32;
        let mut out = [None, None];
        for (slot, spec) in specs.iter().enumerate() {
            if let Some((answer, weight, label)) = *spec {
                out[slot] = Some(Button { rect: Rect::new(x, y, widths[slot], h), answer, weight, label });
                x += (widths[slot] + gap) as i32;
            }
        }
        out
    }

    /// Разобрать клавишу. `true` — окно ею воспользовалось.
    pub fn handle(&mut self, code: KeyCode) -> bool {
        let count = self.count();
        match code {
            KeyCode::Tab | KeyCode::Right => self.focus = (self.focus + 1) % count,
            KeyCode::Left => self.focus = (self.focus + count - 1) % count,
            KeyCode::Enter => self.answer = Some(self.answer_at(self.focus)),
            KeyCode::Escape => self.answer = Some(Answer::Close),
            _ => return false,
        }
        true
    }

    /// Разобрать щелчок в координатах поверхности окна.
    pub fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        for (index, button) in self.layout(area, ctx).iter().enumerate() {
            if let Some(button) = button {
                if button.rect.contains(x, y) {
                    self.focus = index;
                    self.answer = Some(button.answer);
                    return true;
                }
            }
        }
        false
    }

    /// Забрать ответ, если его дали.
    pub fn take_answer(&mut self) -> Option<Answer> {
        self.answer.take()
    }

    /// Нарисовать содержимое в области под заголовком.
    pub fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        let pad = ctx.px(PAD) as i32;
        let (icon, tone) = match self.dialog {
            Dialog::About(_) => (Icon::Info, Tone::Accent),
            Dialog::Power { .. } => (Icon::Power, Tone::Bad),
        };
        let tile = Rect::new(area.x + pad, area.y + pad, ctx.px(TILE), ctx.px(TILE));
        paint::icon_tile(ctx, s, tile, icon, tone, true);

        let x = tile.right() + ctx.px(16) as i32;
        let width = (area.right() - pad - x).max(0) as u32;
        let mut y = area.y + pad;
        match &self.dialog {
            Dialog::About(facts) => draw_about(ctx, s, x, &mut y, width, facts),
            Dialog::Power { restart } => draw_power(ctx, s, x, &mut y, width, *restart),
        }

        let buttons = self.layout(area, ctx);
        // Черта над кнопками отделяет ответ от сообщения: без неё кнопки
        // читаются как продолжение текста.
        if let Some(first) = buttons[0] {
            paint::separator(ctx, s, area.x, first.rect.y - ctx.px(16) as i32, area.w);
        }
        for (index, button) in buttons.iter().enumerate() {
            let Some(button) = button else {
                continue;
            };
            paint::button(ctx, s, button.rect, button.weight, button.label, false);
            // Выбранная кнопка обведена снаружи, а не перекрашена: её
            // собственный вид говорит, что она делает, а рамка — что нажмёт
            // Enter, и смешивать эти два сообщения нельзя.
            if index == self.focus {
                let ring = ctx.px(3);
                let around = Rect::new(
                    button.rect.x - ring as i32,
                    button.rect.y - ring as i32,
                    button.rect.w + ring * 2,
                    button.rect.h + ring * 2,
                );
                draw::rounded_stroke(s, around, ctx.px(theme::R_ROW) + ring, p.accedge, 255);
            }
        }
    }
}

/// Высота строки начертания в точках.
fn line(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

/// Написать абзац с переносом по словам и сдвинуть `y` на его высоту.
#[allow(clippy::too_many_arguments)]
fn paragraph(ctx: Ctx, s: &mut Surface, role: Role, x: i32, y: &mut i32, width: u32, text: &str, color: Color) {
    let face = ctx.face(role);
    // Перенос по настоящей ширине слов, а не по числу знаков. Первая версия
    // считала колонки по самой широкой букве и рвала строку на трети окна:
    // пропорциональный шрифт в среднем вдвое уже своей «ш».
    let space = face.width(" ");
    let mut current = String::new();
    let mut current_w = 0u32;
    for word in text.split(' ') {
        let word_w = face.width(word);
        if !current.is_empty() && current_w + space + word_w > width {
            paint::text_clipped(ctx, s, role, x, *y, width, &current, color);
            *y += line(ctx, role);
            current.clear();
            current_w = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_w += space;
        }
        current.push_str(word);
        current_w += word_w;
    }
    // Слово длиннее строки не рвётся посередине, а обрезается многоточием:
    // в этих окнах таких слов нет, и заводить разрыв слова ради гипотетического
    // значило бы писать код, которого никто не увидит.
    if !current.is_empty() {
        paint::text_clipped(ctx, s, role, x, *y, width, &current, color);
        *y += line(ctx, role);
    }
}

fn draw_about(ctx: Ctx, s: &mut Surface, x: i32, y: &mut i32, width: u32, facts: &AboutFacts) {
    let p = ctx.palette;
    paint::text(ctx, s, Role::Heading, x, *y, &format!("FreeOS {}", facts.version), p.ink);
    *y += line(ctx, Role::Heading) + ctx.px(4) as i32;
    paragraph(
        ctx,
        s,
        Role::Body,
        x,
        y,
        width,
        "Операционная система, написанная на Rust с пустого места.",
        p.ink3,
    );
    *y += ctx.px(14) as i32;

    let label_w = ctx.px(128);
    let value_w = width.saturating_sub(label_w);
    let rows: [(&str, String); 4] = [
        ("Архитектура", String::from(facts.arch)),
        ("Экран", format!("{} x {}", facts.screen.0, facts.screen.1)),
        ("Память", format!("{} МиБ свободно из {}", facts.free_mib, facts.total_mib)),
        ("Время работы", format!("{} на момент открытия", uptime(facts.uptime_ms))),
    ];
    for (label, value) in &rows {
        paint::text(ctx, s, Role::Body, x, *y, label, p.ink4);
        paint::text_clipped(ctx, s, Role::Body, x + label_w as i32, *y, value_w, value, p.ink);
        *y += line(ctx, Role::Body) + ctx.px(6) as i32;
    }

    *y += ctx.px(10) as i32;
    paint::caps(ctx, s, x, *y, "СОЧЕТАНИЯ КЛАВИШ");
    *y += line(ctx, Role::MonoCaps) + ctx.px(8) as i32;
    for (keys, what) in [
        ("Win или F1", "меню запуска"),
        ("Alt+Tab", "следующее окно"),
        ("Ctrl+W", "закрыть окно"),
        ("Ctrl+стрелки", "подвинуть окно"),
    ] {
        paint::text(ctx, s, Role::Mono, x, *y, keys, p.ink2);
        paint::text_clipped(ctx, s, Role::Body, x + label_w as i32, *y, value_w, what, p.ink3);
        *y += line(ctx, Role::Body) + ctx.px(4) as i32;
    }
}

fn draw_power(ctx: Ctx, s: &mut Surface, x: i32, y: &mut i32, width: u32, restart: bool) {
    let p = ctx.palette;
    let (question, then) = if restart {
        ("Перезагрузить компьютер?", "Он запустится снова с того же диска.")
    } else {
        ("Выключить компьютер?", "Включать его придётся кнопкой питания.")
    };
    paint::text(ctx, s, Role::Heading, x, *y, question, p.ink);
    *y += line(ctx, Role::Heading) + ctx.px(8) as i32;
    // Последствие объясняется, а не только спрашивается: порядок выключения
    // существует ради закрытых томов, и человеку стоит знать, почему машину не
    // гасят кнопкой на корпусе.
    paragraph(
        ctx,
        s,
        Role::Body,
        x,
        y,
        width,
        "Тома будут закрыты первыми, чтобы следующая загрузка нашла их целыми.",
        p.ink3,
    );
    *y += ctx.px(4) as i32;
    paragraph(ctx, s, Role::Body, x, y, width, then, p.ink3);
}

/// Время работы в виде `Ч:ММ:СС`.
fn uptime(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}
