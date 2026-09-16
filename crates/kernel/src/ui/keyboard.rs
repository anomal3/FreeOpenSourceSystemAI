// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Экранная клавиатура телефона (макет `FreeOS-mobile`, экран 06).
//!
//! # Что она посылает
//!
//! **Коды клавиш**, а не буквы, — в ту же очередь, куда их кладёт USB-клавиатура
//! (`input::post`). Буква «а» на кнопке — это нажатие `KeyCode::A`; заглавная —
//! то же нажатие внутри нажатого Shift; двоеточие — Shift и `;`.
//!
//! Так сделано не из экономии. Всё, что читает клавиатуру, — оболочка, редактор
//! строки, программы в строчном и сыром режиме, горячие клавиши стола, раскладка
//! RU/EN — уже понимает коды и модификаторы, и понимает одинаково. Клавиатура,
//! посылающая готовые символы, была бы вторым видом ввода, который каждому из них
//! пришлось бы учить отдельно, и разошёлся бы с первым на первом же Shift.
//!
//! # Когда она видна
//!
//! Когда на телефоне активно окно, в которое печатают, — пока это терминал (см.
//! `Compositor::sync_keyboard`). Док в это время спрятан: в макете окно с
//! клавиатурой занимает экран до низа, и уйти из него можно кнопкой «свернуть».
//!
//! # Чего в ней нет
//!
//! Автоповтора при удержании ⌫ и подписей под русскую раскладку: при раскладке
//! RU кнопки подписаны латиницей, а печатают кириллицу — так же, как физическая
//! клавиатура без наклеек. Оба пункта — следующие шаги, а не забытые.

use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use crate::input::KeyCode;

/// Высота строки быстрых команд вместе с полями (макет: 8 + 32 + 8).
const CHIPS_H: u32 = 48;
/// Высота клавиш и промежутки между рядами и кнопками.
const KEY_H: u32 = 38;
const ROW_GAP: u32 = 7;
const KEY_GAP: u32 = 5;
/// Поля панели клавиш: сверху, по бокам, снизу.
const PAD_TOP: u32 = 10;
const PAD_SIDE: u32 = 8;
const PAD_BOTTOM: u32 = 12;

/// Сколько места клавиатура занимает по высоте, в точках макета.
const HEIGHT: u32 = CHIPS_H + PAD_TOP + KEY_H * 4 + ROW_GAP * 3 + PAD_BOTTOM;

/// Промежуток между окном и клавиатурой, в точках макета.
pub const GAP: u32 = 8;

/// Быстрые команды над клавишами — те, что в макете.
const CHIPS: [&str; 4] = ["tasks", "pkg", "free", "ls"];

/// Что делает кнопка.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Клавиша: код и нужен ли для неё Shift (двоеточие, скобка).
    Key { code: KeyCode, shift: bool },
    /// Заглавная для следующей буквы.
    Shift,
    Backspace,
    /// Буквы ↔ цифры и знаки.
    Page,
    Space,
    Enter,
    Tab,
    /// Быстрая команда: набрать слово.
    Chip(&'static str),
}

/// Как кнопка выглядит.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    /// Обычная клавиша: стекло кнопки и корона.
    Plain,
    /// Служебная: приглушённая подложка.
    Ghost,
    /// Ввод: градиент акцента.
    Accent,
    /// Быстрая команда.
    Chip,
}

#[derive(Clone, Copy)]
struct Key {
    rect: Rect,
    action: Action,
    look: Look,
    /// Подпись: строчная и заглавная (у служебных одна и та же).
    label: &'static str,
    upper: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Letters,
    Symbols,
}

pub struct Keyboard {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    visible: bool,
    shift: bool,
    page: Page,
    keys: Vec<Key>,
    pressed: Option<usize>,
    damage: Rect,
    /// Нарисована ли поверхность в нынешнем виде (страница, Shift).
    ///
    /// Показ клавиатуры не перерисовывает её, если рисовать нечего: сорок
    /// клавиш с текстом и скруглениями стоили на телефоне худшего кадра в
    /// 179 мс при каждом разворачивании терминала.
    drawn: bool,
}

/// Заливка слоя, сведённая к непрозрачному цвету, — как у дока.
fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}

impl Keyboard {
    /// Собрать клавиатуру под этот экран. `None` — не телефон или не хватило
    /// памяти; тогда клавиатуры просто нет.
    #[must_use]
    pub fn new(screen_w: u32, screen_h: u32, scale: u32) -> Option<Self> {
        if !theme::is_mobile() {
            return None;
        }
        let scale = scale.max(1);
        let ctx = Ctx::scaled(scale);
        let inset = ctx.px(theme::M_INSET);
        let width = screen_w.checked_sub(inset * 2).filter(|w| *w > 0)?;
        let height = ctx.px(HEIGHT);
        let y = screen_h as i32 - (inset + height) as i32;
        let surface = Surface::new(width, height, bg())?;
        let mut keyboard = Self {
            surface,
            rect: Rect::new(inset as i32, y, width, height),
            scale,
            visible: false,
            shift: false,
            page: Page::Letters,
            keys: Vec::new(),
            pressed: None,
            damage: Rect::EMPTY,
            drawn: false,
        };
        keyboard.layout();
        Some(keyboard)
    }

    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.visible
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Показать или спрятать. Возвращает `true`, если состояние изменилось.
    pub fn set_visible(&mut self, visible: bool) -> bool {
        if self.visible == visible {
            return false;
        }
        self.visible = visible;
        if visible {
            if let Some(index) = self.pressed.take() {
                self.redraw_key(index);
            }
            if self.drawn {
                self.damage = self.surface.bounds();
            } else {
                self.redraw();
            }
        }
        true
    }

    /// Попадает ли точка экрана в клавиатуру.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.visible && self.rect.contains(x, y)
    }

    /// Забрать накопленные изменения (в координатах поверхности).
    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    /// Нажатие в точке экрана: подсветить кнопку и сказать, что она делает.
    ///
    /// Смена страницы и Shift обрабатываются здесь же — это состояние самой
    /// клавиатуры, и наружу им уходить незачем.
    pub fn press(&mut self, x: i32, y: i32) -> Option<Action> {
        let local = (x - self.rect.x, y - self.rect.y);
        let index = self.keys.iter().position(|key| key.rect.contains(local.0, local.1))?;
        let action = self.keys[index].action;
        self.pressed = Some(index);
        match action {
            Action::Shift => {
                self.shift = !self.shift;
                self.redraw();
            }
            Action::Page => {
                self.page = match self.page {
                    Page::Letters => Page::Symbols,
                    Page::Symbols => Page::Letters,
                };
                self.shift = false;
                self.layout();
                self.redraw();
            }
            _ => self.redraw_key(index),
        }
        Some(action)
    }

    /// Буква напечатана: одноразовый Shift снимается, как у всех телефонных
    /// клавиатур.
    pub fn consume_shift(&mut self) -> bool {
        if !self.shift {
            return false;
        }
        self.shift = false;
        self.redraw();
        true
    }

    /// Нажат ли одноразовый Shift.
    #[must_use]
    pub const fn shifted(&self) -> bool {
        self.shift
    }

    /// Палец отпустили: снять подсветку.
    pub fn release(&mut self) {
        if let Some(index) = self.pressed.take() {
            self.redraw_key(index);
        }
    }

    /// Разложить кнопки. Числа — макет, в его точках.
    fn layout(&mut self) {
        let ctx = Ctx::scaled(self.scale);
        let width = self.surface.width();
        let mut keys = Vec::new();

        // Строка быстрых команд: ширина кнопки — по слову.
        let mono = ctx.face(Role::Mono);
        let mut x = ctx.px(12) as i32;
        let chip_y = ctx.px(8) as i32;
        let chip_h = ctx.px(32);
        for word in CHIPS {
            let w = mono.width(word) + ctx.px(24);
            keys.push(Key {
                rect: Rect::new(x, chip_y, w, chip_h),
                action: Action::Chip(word),
                look: Look::Chip,
                label: word,
                upper: word,
            });
            x += (w + ctx.px(7)) as i32;
        }
        keys.push(Key {
            rect: Rect::new(x, chip_y, ctx.px(36), chip_h),
            action: Action::Tab,
            look: Look::Chip,
            label: "Tab",
            upper: "Tab",
        });

        let side = ctx.px(PAD_SIDE) as i32;
        let inner = width.saturating_sub(ctx.px(PAD_SIDE) * 2);
        let key_h = ctx.px(KEY_H);
        let gap = ctx.px(KEY_GAP);
        let mut y = ctx.px(CHIPS_H + PAD_TOP) as i32;

        let rows: [&[(&'static str, &'static str, KeyCode, bool)]; 3] = match self.page {
            Page::Letters => [&LETTERS_1, &LETTERS_2, &LETTERS_3],
            Page::Symbols => [&SYMBOLS_1, &SYMBOLS_2, &SYMBOLS_3],
        };

        // Ряд 1 — во всю ширину.
        push_row(&mut keys, rows[0], side, y, inner, key_h, gap, Look::Plain);
        y += (key_h + ctx.px(ROW_GAP)) as i32;

        // Ряд 2 — с полями по 16, как в макете: так клавиши второго ряда стоят
        // между клавишами первого, а не под ними.
        let indent = ctx.px(16);
        push_row(
            &mut keys,
            rows[1],
            side + indent as i32,
            y,
            inner.saturating_sub(indent * 2),
            key_h,
            gap,
            Look::Plain,
        );
        y += (key_h + ctx.px(ROW_GAP)) as i32;

        // Ряд 3 — ⇧ и ⌫ по краям, по 44.
        let wide = ctx.px(44);
        keys.push(Key {
            rect: Rect::new(side, y, wide, key_h),
            action: Action::Shift,
            look: Look::Ghost,
            label: "",
            upper: "",
        });
        let middle_w = inner.saturating_sub((wide + gap) * 2);
        push_row(&mut keys, rows[2], side + (wide + gap) as i32, y, middle_w, key_h, gap, Look::Plain);
        keys.push(Key {
            rect: Rect::new(side + (inner - wide) as i32, y, wide, key_h),
            action: Action::Backspace,
            look: Look::Ghost,
            label: "",
            upper: "",
        });
        y += (key_h + ctx.px(ROW_GAP)) as i32;

        // Ряд 4 — ?123, /, пробел, -, ввод.
        let fixed = [
            (ctx.px(52), Action::Page, Look::Ghost, self.page_label()),
            (wide, Action::Key { code: KeyCode::Slash, shift: false }, Look::Ghost, "/"),
        ];
        let tail = [
            (wide, Action::Key { code: KeyCode::Minus, shift: false }, Look::Ghost, "-"),
            (ctx.px(62), Action::Enter, Look::Accent, ""),
        ];
        let fixed_w: u32 = fixed.iter().chain(tail.iter()).map(|k| k.0 + gap).sum();
        let space_w = inner.saturating_sub(fixed_w);
        let mut x = side;
        for (w, action, look, label) in fixed {
            keys.push(Key { rect: Rect::new(x, y, w, key_h), action, look, label, upper: label });
            x += (w + gap) as i32;
        }
        keys.push(Key {
            rect: Rect::new(x, y, space_w, key_h),
            action: Action::Space,
            look: Look::Plain,
            label: "",
            upper: "",
        });
        x += (space_w + gap) as i32;
        for (w, action, look, label) in tail {
            keys.push(Key { rect: Rect::new(x, y, w, key_h), action, look, label, upper: label });
            x += (w + gap) as i32;
        }

        self.keys = keys;
    }

    const fn page_label(&self) -> &'static str {
        match self.page {
            Page::Letters => "?123",
            Page::Symbols => "ABC",
        }
    }

    /// Нарисовать всё заново.
    fn redraw(&mut self) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let card = self.surface.bounds();
        let round = ctx.px(34);
        self.surface.fill(card, ctx.under);
        // Панель клавиш — своей заливкой, под строкой быстрых команд, с чертой
        // между ними: в макете это два яруса одного окна.
        let panel = Rect::new(0, ctx.px(CHIPS_H) as i32, card.w, card.h.saturating_sub(ctx.px(CHIPS_H)));
        draw::blend_rect(&mut self.surface, panel, p.panel.color, p.panel.alpha);
        draw::hline(&mut self.surface, 0, panel.y, card.w, p.line2.color, p.line2.alpha);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);
        for index in 0..self.keys.len() {
            self.draw_key(ctx, index);
        }
        self.damage = card;
        self.drawn = true;
    }

    /// Перерисовать одну кнопку — нажатие и отпускание.
    fn redraw_key(&mut self, index: usize) {
        let Some(key) = self.keys.get(index).copied() else {
            return;
        };
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        // Под кнопкой — заливка её яруса: иначе подсветка, снятая с кнопки,
        // оставила бы полупрозрачный след поверх прежней.
        self.surface.fill(key.rect, ctx.under);
        if key.rect.y >= ctx.px(CHIPS_H) as i32 {
            draw::blend_rect(&mut self.surface, key.rect, p.panel.color, p.panel.alpha);
        }
        self.draw_key(ctx, index);
        self.damage = self.damage.union(&key.rect);
    }

    fn draw_key(&mut self, ctx: Ctx, index: usize) {
        let Some(key) = self.keys.get(index).copied() else {
            return;
        };
        let p = ctx.palette;
        let pressed = self.pressed == Some(index);
        let radius = ctx.px(if key.look == Look::Chip { 11 } else { 10 });
        let ink = match key.look {
            Look::Accent => {
                draw::rounded_gradient(&mut self.surface, key.rect, radius, p.acc, p.acc2, 255);
                draw::rounded_stroke(&mut self.surface, key.rect, radius, p.accline.color, p.accline.alpha);
                Color::rgb(0xFF, 0xFF, 0xFF)
            }
            Look::Ghost => {
                draw::rounded(&mut self.surface, key.rect, radius, p.ghost.color, p.ghost.alpha);
                draw::rounded_stroke(&mut self.surface, key.rect, radius, p.line2.color, p.line2.alpha);
                p.ink3
            }
            Look::Plain | Look::Chip => {
                draw::rounded(&mut self.surface, key.rect, radius, p.btn.color, p.btn.alpha);
                draw::rounded_stroke(&mut self.surface, key.rect, radius, p.btnline.color, p.btnline.alpha);
                if key.look == Look::Plain {
                    draw::crown(&mut self.surface, key.rect, radius, p.crown.color, p.crown.alpha);
                }
                p.ink2
            }
        };
        // Нажатая кнопка светлеет: палец закрывает саму кнопку, и видно только
        // то, что вокруг, — поэтому подсветка заметная, а не намёк.
        if pressed {
            draw::rounded(&mut self.surface, key.rect, radius, p.acc, 90);
        }
        // Shift, когда включён, горит акцентом — иначе не понять, заглавная ли
        // будет следующая буква.
        let ink = if key.action == Action::Shift && self.shift { p.acc } else { ink };
        // Служебные клавиши — значками: знаков ⇧, ⌫ и ↵ в нашем шрифте нет, а
        // знак, которого нет, рисуется вопросом.
        let icon = match key.action {
            Action::Shift => Some(Icon::Up),
            Action::Backspace => Some(Icon::Back),
            Action::Enter => Some(Icon::Enter),
            _ => None,
        };
        if let Some(icon) = icon {
            let size = ctx.px(16);
            glyphicon::draw(
                &mut self.surface,
                icon,
                key.rect.x + (key.rect.w as i32 - size as i32) / 2,
                key.rect.y + (key.rect.h as i32 - size as i32) / 2,
                size,
                ink,
                255,
            );
            return;
        }
        let label = if self.shift { key.upper } else { key.label };
        if label.is_empty() {
            return;
        }
        // Буквы — самым крупным из того, что растеризовано на этот ряд: в макете
        // клавиша подписана 14 точками моноширинного, у нас это 26 точек, а
        // моноширинный крупнее 18 не собран. Быстрые команды — моноширинным: это
        // команды оболочки, и выглядеть они должны так, как в терминале.
        let role = match key.look {
            Look::Chip => Role::Mono,
            _ if key.label.len() > 1 => Role::Mono,
            _ => Role::Strong,
        };
        let face = ctx.face(role);
        let w = face.width(label);
        paint::text(
            ctx,
            &mut self.surface,
            role,
            key.rect.x + (key.rect.w as i32 - w as i32) / 2,
            paint::baseline(ctx, role, key.rect),
            label,
            ink,
        );
    }
}

/// Разложить ряд одинаковых клавиш по ширине.
#[allow(clippy::too_many_arguments)]
fn push_row(
    keys: &mut Vec<Key>,
    row: &[(&'static str, &'static str, KeyCode, bool)],
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    gap: u32,
    look: Look,
) {
    let count = row.len() as u32;
    if count == 0 {
        return;
    }
    let key_w = width.saturating_sub(gap * (count - 1)) / count;
    // Остаток от деления раздаётся последней клавише, чтобы ряд доходил до края
    // ровно: иначе правый край ряда гулял бы на пару точек от ряда к ряду.
    let spare = width.saturating_sub(key_w * count + gap * (count - 1));
    for (index, (label, upper, code, shift)) in row.iter().enumerate() {
        let last = index as u32 + 1 == count;
        keys.push(Key {
            rect: Rect::new(
                x + (index as u32 * (key_w + gap)) as i32,
                y,
                key_w + if last { spare } else { 0 },
                height,
            ),
            action: Action::Key { code: *code, shift: *shift },
            look,
            label,
            upper,
        });
    }
}

/// Подпись, заглавная подпись, код, нужен ли Shift.
type Row<const N: usize> = [(&'static str, &'static str, KeyCode, bool); N];

const LETTERS_1: Row<10> = [
    ("q", "Q", KeyCode::Q, false),
    ("w", "W", KeyCode::W, false),
    ("e", "E", KeyCode::E, false),
    ("r", "R", KeyCode::R, false),
    ("t", "T", KeyCode::T, false),
    ("y", "Y", KeyCode::Y, false),
    ("u", "U", KeyCode::U, false),
    ("i", "I", KeyCode::I, false),
    ("o", "O", KeyCode::O, false),
    ("p", "P", KeyCode::P, false),
];

const LETTERS_2: Row<9> = [
    ("a", "A", KeyCode::A, false),
    ("s", "S", KeyCode::S, false),
    ("d", "D", KeyCode::D, false),
    ("f", "F", KeyCode::F, false),
    ("g", "G", KeyCode::G, false),
    ("h", "H", KeyCode::H, false),
    ("j", "J", KeyCode::J, false),
    ("k", "K", KeyCode::K, false),
    ("l", "L", KeyCode::L, false),
];

const LETTERS_3: Row<7> = [
    ("z", "Z", KeyCode::Z, false),
    ("x", "X", KeyCode::X, false),
    ("c", "C", KeyCode::C, false),
    ("v", "V", KeyCode::V, false),
    ("b", "B", KeyCode::B, false),
    ("n", "N", KeyCode::N, false),
    ("m", "M", KeyCode::M, false),
];

const SYMBOLS_1: Row<10> = [
    ("1", "1", KeyCode::Digit1, false),
    ("2", "2", KeyCode::Digit2, false),
    ("3", "3", KeyCode::Digit3, false),
    ("4", "4", KeyCode::Digit4, false),
    ("5", "5", KeyCode::Digit5, false),
    ("6", "6", KeyCode::Digit6, false),
    ("7", "7", KeyCode::Digit7, false),
    ("8", "8", KeyCode::Digit8, false),
    ("9", "9", KeyCode::Digit9, false),
    ("0", "0", KeyCode::Digit0, false),
];

// Знаки, за которыми на американской раскладке стоит Shift, посылаются с ним:
// раскладку за нас переводит `input::keymap`, и так же поступает физическая
// клавиатура.
const SYMBOLS_2: Row<9> = [
    (":", ":", KeyCode::Semicolon, true),
    (";", ";", KeyCode::Semicolon, false),
    ("(", "(", KeyCode::Digit9, true),
    (")", ")", KeyCode::Digit0, true),
    ("$", "$", KeyCode::Digit4, true),
    ("&", "&", KeyCode::Digit7, true),
    ("@", "@", KeyCode::Digit2, true),
    ("\"", "\"", KeyCode::Apostrophe, true),
    ("=", "=", KeyCode::Equal, false),
];

const SYMBOLS_3: Row<7> = [
    (".", ".", KeyCode::Period, false),
    (",", ",", KeyCode::Comma, false),
    ("?", "?", KeyCode::Slash, true),
    ("!", "!", KeyCode::Digit1, true),
    ("'", "'", KeyCode::Apostrophe, false),
    ("|", "|", KeyCode::Backslash, true),
    ("_", "_", KeyCode::Minus, true),
];

/// Какой клавишей набирается буква быстрой команды.
#[must_use]
pub fn code_for(letter: char) -> Option<KeyCode> {
    Some(match letter {
        'a' => KeyCode::A, 'b' => KeyCode::B, 'c' => KeyCode::C, 'd' => KeyCode::D,
        'e' => KeyCode::E, 'f' => KeyCode::F, 'g' => KeyCode::G, 'h' => KeyCode::H,
        'i' => KeyCode::I, 'j' => KeyCode::J, 'k' => KeyCode::K, 'l' => KeyCode::L,
        'm' => KeyCode::M, 'n' => KeyCode::N, 'o' => KeyCode::O, 'p' => KeyCode::P,
        'q' => KeyCode::Q, 'r' => KeyCode::R, 's' => KeyCode::S, 't' => KeyCode::T,
        'u' => KeyCode::U, 'v' => KeyCode::V, 'w' => KeyCode::W, 'x' => KeyCode::X,
        'y' => KeyCode::Y, 'z' => KeyCode::Z,
        _ => return None,
    })
}

/// Сколько места клавиатура отнимает снизу у окна над ней, в точках экрана.
#[must_use]
pub fn reserved(scale: u32) -> u32 {
    Ctx::scaled(scale).px(HEIGHT + theme::M_INSET + GAP)
}
