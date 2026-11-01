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
use alloc::vec::Vec;

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
    /// «Как открыть этот файл?» (макет 18): программы списком, флажок
    /// «Всегда открывать этой программой».
    OpenWith(OpenWith),
}

/// Выбор программы для файла.
pub struct OpenWith {
    /// Полный путь файла — его и откроет выбранная программа.
    pub path: String,
    /// Расширение — для него запоминается выбор.
    pub ext: String,
    pub kind: sysconf::filetypes::Kind,
    /// Подпись и команда, к которой допишется путь. Первая — рекомендуемая.
    pub choices: Vec<(String, String)>,
    pub selected: usize,
    pub always: bool,
}

/// Строк списка, не больше: окно не растёт под длинный список.
const OPEN_WITH_ROWS: usize = 5;
/// Высота строки программы (макет: 48).
const CHOICE_H: u32 = 48;

/// Сведения «О системе» — снимок на момент открытия окна.
///
/// Снимок, а не живые числа: окно не перерисовывается по таймеру, и время
/// работы, которое сдвигалось бы только от щелчка по окну, выглядело бы
/// сломанным. Подпись у строки говорит об этом прямо.
pub struct AboutFacts {
    /// Версия с номером сборки: `0.8.482`.
    pub version: alloc::string::String,
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
    /// «ОК» в выборе программы.
    Open,
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
            Dialog::About(_) | Dialog::OpenWith(_) => 0,
        };
        Self { dialog, focus, answer: None }
    }

    /// Сколько у окна кнопок.
    const fn count(&self) -> usize {
        match self.dialog {
            Dialog::About(_) => 1,
            Dialog::Power { .. } | Dialog::OpenWith(_) => 2,
        }
    }

    /// Ответ кнопки с этим номером.
    const fn answer_at(&self, index: usize) -> Answer {
        match (&self.dialog, index) {
            (Dialog::Power { .. }, 0) => Answer::Confirm,
            (Dialog::OpenWith(_), 0) => Answer::Open,
            _ => Answer::Close,
        }
    }

    /// Где стоят кнопки.
    ///
    /// Прижаты к правому нижнему углу и идут слева направо: действие, потом
    /// отмена — так стоят кнопки в диалогах, к которым человек привык.
    fn layout(&self, area: Rect, ctx: Ctx) -> [Option<Button>; 2] {
        let specs: [Option<(Answer, Weight, &'static str)>; 2] = match &self.dialog {
            Dialog::About(_) => [Some((Answer::Close, Weight::Primary, "Закрыть")), None],
            Dialog::Power { restart } => [
                Some((
                    Answer::Confirm,
                    Weight::Danger,
                    if *restart { "Перезагрузить" } else { "Выключить" },
                )),
                Some((Answer::Close, Weight::Normal, "Отмена")),
            ],
            Dialog::OpenWith(_) => [
                Some((Answer::Open, Weight::Primary, "ОК")),
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
        if let Dialog::OpenWith(choice) = &mut self.dialog {
            let shown = choice.choices.len().min(OPEN_WITH_ROWS);
            match code {
                KeyCode::Up => {
                    choice.selected = choice.selected.saturating_sub(1);
                    return true;
                }
                KeyCode::Down => {
                    if choice.selected + 1 < shown {
                        choice.selected += 1;
                    }
                    return true;
                }
                KeyCode::Space => {
                    choice.always = !choice.always;
                    return true;
                }
                _ => {}
            }
        }
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
        if let Dialog::OpenWith(choice) = &mut self.dialog {
            let (rows, check) = choice_layout(area, ctx, choice.choices.len());
            if let Some(index) = rows.iter().position(|row| row.contains(x, y)) {
                choice.selected = index;
                return true;
            }
            if check.contains(x, y) {
                choice.always = !choice.always;
                return true;
            }
        }
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

    /// Что выбрано в «Как открыть этот файл?»: путь, расширение, команда и
    /// «всегда».
    #[must_use]
    pub fn open_with_choice(&self) -> Option<(String, String, String, bool)> {
        let Dialog::OpenWith(choice) = &self.dialog else {
            return None;
        };
        let (_, command) = choice.choices.get(choice.selected)?;
        Some((choice.path.clone(), choice.ext.clone(), command.clone(), choice.always))
    }

    /// Забрать ответ, если его дали.
    pub fn take_answer(&mut self) -> Option<Answer> {
        self.answer.take()
    }

    /// Нарисовать содержимое в области под заголовком.
    pub fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let pad = ctx.px(PAD) as i32;
        if let Dialog::OpenWith(choice) = &self.dialog {
            draw_open_with(ctx, s, area, choice);
            self.draw_buttons(s, area, ctx);
            return;
        }
        let (icon, tone) = match self.dialog {
            Dialog::About(_) | Dialog::OpenWith(_) => (Icon::Info, Tone::Accent),
            Dialog::Power { .. } => (Icon::Power, Tone::Bad),
        };
        let tile = Rect::new(area.x + pad, area.y + pad, ctx.px(TILE), ctx.px(TILE));
        // «О системе» — логотип FossAi на плитке (фаза 72a).
        match super::appicon::logo_tile().filter(|_| matches!(self.dialog, Dialog::About(_))) {
            Some(logo) => logo.draw(s, tile),
            None => paint::icon_tile(ctx, s, tile, icon, tone, true),
        }

        let x = tile.right() + ctx.px(16) as i32;
        let width = (area.right() - pad - x).max(0) as u32;
        let mut y = area.y + pad;
        match &self.dialog {
            Dialog::About(facts) => draw_about(ctx, s, x, &mut y, width, facts),
            Dialog::Power { restart } => draw_power(ctx, s, x, &mut y, width, *restart),
            Dialog::OpenWith(_) => {}
        }
        self.draw_buttons(s, area, ctx);
    }

    /// Черта и кнопки внизу.
    fn draw_buttons(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
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

/// Где строки программ и флажок «всегда» в окне выбора программы.
fn choice_layout(area: Rect, ctx: Ctx, count: usize) -> (Vec<Rect>, Rect) {
    let pad = ctx.px(PAD) as i32;
    let x = area.x + pad;
    let w = area.w.saturating_sub(ctx.px(PAD) * 2);
    // Шапка: значок типа 40 и две строки рядом с ним.
    let mut y = area.y + pad + ctx.px(40 + 16) as i32;
    let mut rows = Vec::new();
    for _ in 0..count.min(OPEN_WITH_ROWS) {
        rows.push(Rect::new(x, y, w, ctx.px(CHOICE_H)));
        y += ctx.px(CHOICE_H + 2) as i32;
    }
    y += ctx.px(14) as i32;
    let check = Rect::new(x, y, w, ctx.px(24));
    (rows, check)
}

/// «Как открыть этот файл?» по макету 18.
fn draw_open_with(ctx: Ctx, s: &mut Surface, area: Rect, choice: &OpenWith) {
    let p = ctx.palette;
    let pad = ctx.px(PAD) as i32;
    let icon = Rect::new(area.x + pad, area.y + pad, ctx.px(40), ctx.px(40));
    match super::appicon::design(choice.kind.icon) {
        Some(image) => image.draw(s, icon),
        None => paint::icon_tile(ctx, s, icon, Icon::File, Tone::Accent, false),
    }
    let x = icon.right() + ctx.px(14) as i32;
    let width = (area.right() - pad - x).max(0) as u32;
    paint::text_clipped(ctx, s, Role::Heading, x, icon.y, width, "Как открыть этот файл?", p.ink);
    let name = choice.path.rsplit('/').next().unwrap_or(&choice.path);
    let what = format!("{name} — {}", choice.kind.name.to_lowercase());
    paint::text_clipped(ctx, s, Role::Caption, x, icon.y + line(ctx, Role::Heading) + ctx.px(2) as i32, width, &what, p.ink3);

    let (rows, check) = choice_layout(area, ctx, choice.choices.len());
    let r = ctx.px(6);
    for (index, (row, (label, _))) in rows.iter().zip(&choice.choices).enumerate() {
        let selected = index == choice.selected;
        if selected {
            draw::rounded(s, *row, r, p.acc, 60);
            draw::rounded_stroke(s, *row, r, p.acc, 255);
        }
        let tile = Rect::new(row.x + ctx.px(12) as i32, row.y + (row.h as i32 - ctx.px(28) as i32) / 2, ctx.px(28), ctx.px(28));
        let glyph = if label == "Блокнот" { Icon::File } else { Icon::Package };
        paint::icon_tile(ctx, s, tile, glyph, Tone::Accent, false);
        let text_x = tile.right() + ctx.px(14) as i32;
        let radio = ctx.px(8);
        let text_w = (row.right() - text_x - ctx.px(40) as i32).max(0) as u32;
        if index == 0 {
            let top = row.y + (row.h as i32 - line(ctx, Role::Body) - line(ctx, Role::Caption)) / 2;
            paint::text_clipped(ctx, s, Role::Body, text_x, top, text_w, label, p.ink);
            paint::text_clipped(ctx, s, Role::Caption, text_x, top + line(ctx, Role::Body), text_w, "рекомендуется", p.ink4);
        } else {
            paint::text_clipped(ctx, s, Role::Body, text_x, paint::baseline(ctx, Role::Body, *row), text_w, label, p.ink);
        }
        let cx = row.right() - ctx.px(24) as i32;
        let cy = row.y + row.h as i32 / 2;
        draw::circle_outline(s, cx, cy, radio, ctx.px(2).max(1), if selected { p.acc } else { p.ink4 }, 255);
        if selected {
            draw::circle(s, cx, cy, radio / 2, p.acc, 255);
        }
    }

    let side = ctx.px(14);
    let bx = Rect::new(check.x, check.y + (check.h as i32 - side as i32) / 2, side, side);
    if choice.always {
        draw::rounded(s, bx, ctx.px(3), p.acc, 255);
        glyphicon_check(s, bx, p.acc_ink);
    } else {
        draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255);
    }
    let label_x = bx.right() + ctx.px(10) as i32;
    let label_w = (check.right() - label_x).max(0) as u32;
    paint::text_clipped(ctx, s, Role::Body, label_x, paint::baseline(ctx, Role::Body, check), label_w, "Всегда открывать этой программой", p.ink2);
}

/// Галочка во флажке.
fn glyphicon_check(s: &mut Surface, rect: Rect, color: Color) {
    mini_ui::glyphicon::draw(s, Icon::Check, rect.x, rect.y, rect.w, color, 255);
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
    paint::text(ctx, s, Role::Heading, x, *y, &format!("FossAi {}", facts.version), p.ink);
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
