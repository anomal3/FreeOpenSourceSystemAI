// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Экранная клавиатура телефона (макет `FreeOS-mobile`, экран 06; жесты — эскиз
//! `swype`, утверждённый Романом 17.09).
//!
//! # Что она посылает
//!
//! **Знаки**, которые стол превращает в коды клавиш той раскладки, что действует
//! сейчас (`keymap::code_for`), — и кладёт их в ту же очередь, куда их кладёт
//! USB-клавиатура (`input::post`). Всё, что читает клавиатуру, — оболочка,
//! редактор строки, программы, горячие клавиши, раскладка RU/EN — понимает коды
//! и модификаторы одинаково, и второго вида ввода не появляется.
//!
//! Знак, а не код клавиши, — потому что код зависит от раскладки: «/» в
//! русской раскладке набирается не той клавишей, что в английской, а латиница
//! быстрой команды `free` в русской не набирается вовсе.
//!
//! # Жесты
//!
//! Буквенная клавиша и пробел ничего не печатают при нажатии. Палец ведёт путь,
//! и при отпускании клавиатура решает, что это было:
//!
//! - **нажатие** (путь короче полуклавиши) — буква или пробел;
//! - **протяжка по буквам** — слово: крейт `swipe` сравнивает путь с
//!   идеальными путями слов словаря, лучшее вставляется с пробелом, а три
//!   лучших встают над клавишами вместо быстрых команд; нажатие на соседнюю
//!   подсказку заменяет вставленное слово;
//! - **протяжка по пробелу** больше полутора клавиш — смена языка, как в Gboard.
//!
//! За пальцем тянется след цветом акцента. Он истончается со временем: точка
//! старше [`TRAIL_MS`] не рисуется, и после отпускания след уходит с хвоста.
//!
//! # Когда она видна
//!
//! Когда на телефоне активно окно, в которое печатают, — пока это терминал (см.
//! `Compositor::sync_keyboard`). Док в это время спрятан.
//!
//! # Долгое нажатие и ⌫
//!
//! Палец, задержанный на букве дольше [`LONG_MS`] и не сдвинутый, открывает над
//! клавишей варианты (эскиз, кадр 4): «е» → «ё», «ь» → «ъ», у верхнего ряда —
//! цифра из угла клавиши. Палец доводят до нужного и отпускают.
//!
//! ⌫ при удержании стирает с автоповтором. Сразу после слова, набранного
//! жестом, ⌫ стирает всё слово с его пробелом — как в Gboard: жест ошибается
//! словами, а не буквами.

use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};
use swipe::{Dictionary, Point};

use crate::input::keymap::{self, Layout as Lang};

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

/// Сколько живёт точка следа, мс. Столько же след уходит после отпускания.
const TRAIL_MS: u64 = 450;
/// Толщина следа у пальца (радиус), в точках макета.
const TRAIL_R: u32 = 5;
/// Сколько подсказок встаёт над клавишами.
const SUGGESTIONS: usize = 3;
/// Через сколько миллисекунд удержания открываются варианты буквы.
const LONG_MS: u64 = 400;
/// Пропуск между двумя шагами времени дольше этого — стол не работал (долгий
/// кадр, чужая задача), и пропущенное удержанием не считается.
///
/// Иначе палец, который уже ведут по буквам, получал долгое нажатие: события
/// движения лежат недоставленными, пока стол занят кадром, а шаг времени после
/// кадра видит «палец стоит 400 мс». Так падал сценарий `mobile` в отладочном
/// QEMU под нагрузкой — и так же вёл бы себя телефон на первом долгом кадре.
const STALL_MS: u64 = 100;
/// Автоповтор ⌫: задержка до первого повтора и шаг, мс.
const REPEAT_DELAY_MS: u64 = 450;
const REPEAT_MS: u64 = 60;
/// Ширина ячейки варианта и высота строки вариантов, в точках макета.
const ALT_CELL: u32 = 34;
const ALT_H: u32 = 46;

/// Что делает кнопка.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Знак: буква (заглавная — если нажат Shift), цифра, скобка.
    Char(char),
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
    /// Подсказка над клавишами: номер в списке подсказок.
    Suggest(usize),
    /// ⌫ сразу после слова жестом: стереть столько знаков (слово и пробел).
    EraseWord(usize),
}

/// Чем кончилось касание буквенной клавиши или пробела.
pub enum Release {
    /// Касания не было, или жест не похож ни на одно слово.
    Nothing,
    /// Нажатие: напечатать то, что на клавише.
    Key(Action),
    /// Жест: вставить слово (с заглавной, если был нажат Shift) и пробел.
    Word { word: &'static str, capital: bool },
    /// Протяжка по пробелу: раскладка уже переключена.
    Language,
}

/// Как кнопка выглядит.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    /// Обычная клавиша: стекло кнопки и корона.
    Plain,
    /// Служебная: приглушённая подложка.
    Ghost,
    /// Ввод и вставленная подсказка: градиент акцента.
    Accent,
    /// Быстрая команда.
    Chip,
    /// Подсказка, которая не вставлена: текст без подложки.
    Word,
}

#[derive(Clone, Copy)]
struct Key {
    rect: Rect,
    action: Action,
    look: Look,
    /// Подпись: строчная и заглавная (у служебных одна и та же).
    label: &'static str,
    upper: &'static str,
    /// Буква, через которую идёт жест. `None` — клавиша не буквенная.
    letter: Option<char>,
    /// Цифра в углу клавиши верхнего ряда — вариант долгого нажатия.
    digit: Option<char>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Letters,
    Symbols,
}

/// Касание, которое ещё не отпустили.
struct Stroke {
    /// Клавиша, на которую палец опустился.
    key: usize,
    /// Путь пальца в точках экрана.
    points: Vec<Point>,
    /// Когда палец опустился (мс).
    at: u64,
    /// Открытые варианты долгого нажатия.
    popup: Option<Popup>,
}

/// Строка вариантов над клавишей.
struct Popup {
    /// Где она на поверхности клавиатуры.
    rect: Rect,
    chars: Vec<char>,
    selected: usize,
}

pub struct Keyboard {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    visible: bool,
    shift: bool,
    page: Page,
    /// Язык подписей и жестов — тот, что у раскладки (`keymap::layout`).
    lang: Lang,
    keys: Vec<Key>,
    pressed: Option<usize>,
    damage: Rect,
    /// Нарисована ли поверхность в нынешнем виде (страница, Shift).
    ///
    /// Показ клавиатуры не перерисовывает её, если рисовать нечего: сорок
    /// клавиш с текстом и скруглениями стоили на телефоне худшего кадра в
    /// 179 мс при каждом разворачивании терминала.
    drawn: bool,
    stroke: Option<Stroke>,
    /// След пальца: точки экрана и когда палец в них был (мс).
    trail: Vec<(Point, u64)>,
    /// Где след был нарисован прошлым кадром — стереть это надо и в этом.
    trail_drawn: Rect,
    /// Подсказки последнего жеста, лучшая первой, и какая из них вставлена.
    suggestions: Vec<&'static str>,
    current: usize,
    /// Была ли вставленная подсказка с заглавной — замена сохраняет это.
    capital: bool,
    /// Словари, разобранные при первом жесте на своём языке: английский и
    /// русский. Разбор тридцати тысяч строк — не то, за что платят при
    /// каждом показе клавиатуры.
    dicts: [Option<Dictionary>; 2],
    /// Удерживаемый ⌫: когда стереть следующий знак (мс).
    repeat: Option<u64>,
    /// Когда был прошлый шаг времени (мс) — чтобы узнать пропуск.
    last_tick: u64,
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
            lang: keymap::layout(),
            keys: Vec::new(),
            pressed: None,
            damage: Rect::EMPTY,
            drawn: false,
            stroke: None,
            trail: Vec::new(),
            trail_drawn: Rect::EMPTY,
            suggestions: Vec::new(),
            current: 0,
            capital: false,
            dicts: [None, None],
            repeat: None,
            last_tick: 0,
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
        self.stroke = None;
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

    /// Раскладку переключили не жестом (Alt+Shift на USB-клавиатуре, настройки):
    /// подписи и словарь — за ней. `true` — клавиатура перерисована.
    pub fn sync_lang(&mut self) -> bool {
        let lang = keymap::layout();
        if lang == self.lang {
            return false;
        }
        self.lang = lang;
        self.suggestions.clear();
        self.layout();
        if self.drawn {
            self.redraw();
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
    /// Буквенная клавиша и пробел отвечают `None`: что это было — нажатие или
    /// жест, — станет ясно при отпускании ([`Self::release`]).
    ///
    /// Смена страницы и Shift обрабатываются здесь же — это состояние самой
    /// клавиатуры, и наружу им уходить незачем.
    pub fn press(&mut self, x: i32, y: i32) -> Option<Action> {
        let local = (x - self.rect.x, y - self.rect.y);
        let index = self.keys.iter().position(|key| key.rect.contains(local.0, local.1))?;
        let key = self.keys[index];
        self.pressed = Some(index);
        let now = crate::time::uptime_ms();
        if key.letter.is_some() || key.action == Action::Space {
            self.stroke = Some(Stroke { key: index, points: alloc::vec![Point::new(x, y)], at: now, popup: None });
            self.trail.clear();
            self.trail.push((Point::new(x, y), crate::time::uptime_ms()));
            self.redraw_key(index);
            return None;
        }
        // ⌫ сразу после слова жестом стирает слово целиком.
        if key.action == Action::Backspace && !self.suggestions.is_empty() {
            let count = self.suggestions[self.current].chars().count() + 1;
            self.set_suggestions(Vec::new());
            self.redraw_key(index);
            return Some(Action::EraseWord(count));
        }
        if key.action == Action::Backspace {
            self.repeat = Some(now + REPEAT_DELAY_MS);
        }
        // Любая другая клавиша, кроме подсказки, принимает вставленное слово:
        // строка подсказок снова становится быстрыми командами.
        if !matches!(key.action, Action::Suggest(_)) {
            self.set_suggestions(Vec::new());
        }
        let action = match key.action {
            Action::Char(_) => Action::Char(self.char_of(&key)),
            other => other,
        };
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

    /// Знак клавиши с учётом Shift.
    fn char_of(&self, key: &Key) -> char {
        let label = if self.shift { key.upper } else { key.label };
        label.chars().next().unwrap_or(' ')
    }

    /// Палец сдвинулся, не отрываясь. `true` — идёт жест, и след надо показать.
    pub fn stroke_to(&mut self, x: i32, y: i32) -> bool {
        let Some(stroke) = self.stroke.as_mut() else {
            return false;
        };
        // Открыты варианты: палец выбирает среди них, а не ведёт жест.
        if let Some(popup) = stroke.popup.as_mut() {
            let ctx = Ctx::scaled(self.scale);
            let (cell, pad) = (ctx.px(ALT_CELL) as i32, ctx.px(4) as i32);
            let local = x - self.rect.x - popup.rect.x - pad;
            let slot = ((local.max(0) / cell.max(1)) as usize).min(popup.chars.len() - 1);
            if slot != popup.selected {
                popup.selected = slot;
                self.draw_popup();
            }
            return true;
        }
        let point = Point::new(x, y);
        let last = stroke.points.last().copied().unwrap_or(point);
        // Точки ближе трёх — дрожь пальца, а не путь.
        if (last.x - x).abs() + (last.y - y).abs() < 3 {
            return true;
        }
        stroke.points.push(point);
        let start = stroke.points[0];
        self.trail.push((point, crate::time::uptime_ms()));
        // Палец ушёл с клавиши дальше полуклавиши — это жест, а не нажатие:
        // подсветка первой клавиши гаснет, путь показывает след.
        if let Some(index) = self.pressed {
            let half = self.keys.get(index).map_or(0, |key| key.rect.w as i32 / 2);
            if (x - start.x).abs() + (y - start.y).abs() > half {
                self.pressed = None;
                self.redraw_key(index);
            }
        }
        true
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

    /// Палец отпустили: снять подсветку и решить, чем было касание.
    pub fn release(&mut self) -> Release {
        self.repeat = None;
        if let Some(index) = self.pressed.take() {
            self.redraw_key(index);
        }
        let Some(stroke) = self.stroke.take() else {
            return Release::Nothing;
        };
        let Some(key) = self.keys.get(stroke.key).copied() else {
            return Release::Nothing;
        };
        if let Some(popup) = stroke.popup {
            // Строка вариантов лежала поверх клавиш — клавиатура рисуется заново.
            self.trail.clear();
            self.set_suggestions(Vec::new());
            self.redraw();
            let ch = popup.chars[popup.selected];
            crate::kprintln!("  keyboard    : long press '{ch}'");
            return Release::Key(Action::Char(ch));
        }
        let layout = self.swipe_layout();
        let key_w = layout.key_width();
        let (first, last) = (stroke.points[0], stroke.points[stroke.points.len() - 1]);

        if key.action == Action::Space {
            self.trail.clear();
            if (last.x - first.x).abs() > key_w * 3 / 2 {
                keymap::toggle_layout();
                self.sync_lang();
                return Release::Language;
            }
            self.set_suggestions(Vec::new());
            return Release::Key(Action::Space);
        }

        if swipe::is_tap(&layout, &stroke.points) {
            // Точка следа от нажатия не остаётся: это была буква, не жест.
            self.trail.clear();
            self.set_suggestions(Vec::new());
            return Release::Key(Action::Char(self.char_of(&key)));
        }

        let started = crate::time::uptime_ns();
        let lang = self.lang;
        let dict = self.dict(lang);
        let found = swipe::recognize(dict, &layout, &stroke.points, SUGGESTIONS);
        let micros = crate::time::uptime_ns().wrapping_sub(started) / 1000;
        let words: Vec<&'static str> = found.iter().map(|c| c.word).collect();
        crate::kprintln!(
            "  keyboard    : swipe of {} points -> {:?} in {} us",
            stroke.points.len(),
            words,
            micros
        );
        let Some(&word) = words.first() else {
            return Release::Nothing;
        };
        let capital = self.shift;
        self.set_suggestions(words);
        self.capital = capital;
        Release::Word { word, capital }
    }

    /// Нажата подсказка: `(вставленное, новое)` слово и заглавная ли первая
    /// буква. `None` — нажата та, что уже вставлена.
    pub fn suggest(&mut self, index: usize) -> Option<(&'static str, &'static str, bool)> {
        if index == self.current || index >= self.suggestions.len() {
            return None;
        }
        let old = self.suggestions[self.current];
        self.current = index;
        self.layout();
        self.redraw();
        Some((old, self.suggestions[index], self.capital))
    }

    /// Поставить подсказки над клавишами (пустой список — вернуть быстрые команды).
    fn set_suggestions(&mut self, words: Vec<&'static str>) {
        if words.is_empty() && self.suggestions.is_empty() {
            return;
        }
        self.suggestions = words;
        self.current = 0;
        self.layout();
        if self.drawn {
            self.redraw();
        }
    }

    /// Словарь языка — разобранный при первом обращении.
    fn dict(&mut self, lang: Lang) -> &Dictionary {
        let (slot, text) = match lang {
            Lang::Us => (0, swipe::EN),
            Lang::Ru => (1, swipe::RU),
        };
        self.dicts[slot].get_or_insert_with(|| {
            let started = crate::time::uptime_ns();
            let dict = Dictionary::parse(text);
            crate::kprintln!(
                "  keyboard    : {} dictionary of {} words ready in {} ms",
                lang.label(),
                dict.len(),
                crate::time::uptime_ns().wrapping_sub(started) / 1_000_000
            );
            dict
        })
    }

    /// Буквы и их центры в точках экрана — для распознавания жеста.
    fn swipe_layout(&self) -> swipe::Layout {
        let key_w = self
            .keys
            .iter()
            .find(|key| key.letter.is_some())
            .map_or(1, |key| key.rect.w as i32);
        let mut layout = swipe::Layout::new(key_w);
        for key in self.keys.iter() {
            let Some(letter) = key.letter else {
                continue;
            };
            let center = Point::new(
                self.rect.x + key.rect.x + key.rect.w as i32 / 2,
                self.rect.y + key.rect.y + key.rect.h as i32 / 2,
            );
            layout.add(letter, center);
            // «ё» и «ъ» — на клавишах «е» и «ь»: слово «ещё» ведут через «е».
            match letter {
                'е' => layout.add('ё', center),
                'ь' => layout.add('ъ', center),
                _ => {}
            }
        }
        layout
    }

    /// Держат ли палец на клавиатуре: пока держат, стол будит кадры — иначе
    /// долгое нажатие и автоповтор ⌫ ждали бы следующего события пальца.
    #[must_use]
    pub const fn holding(&self) -> bool {
        self.stroke.is_some() || self.repeat.is_some()
    }

    /// Шаг времени перед кадром: открыть варианты задержанной буквы, стереть
    /// следующий знак удерживаемым ⌫.
    pub fn tick(&mut self, now_ms: u64) {
        let gap = now_ms.saturating_sub(self.last_tick);
        self.last_tick = now_ms;
        if gap > STALL_MS {
            if let Some(stroke) = self.stroke.as_mut() {
                // Из пропуска засчитывается не больше обычного шага.
                stroke.at += gap - STALL_MS;
            }
        }
        if let Some(due) = self.repeat {
            if now_ms >= due {
                crate::input::post(crate::input::KeyCode::Backspace, true);
                crate::input::post(crate::input::KeyCode::Backspace, false);
                self.repeat = Some(now_ms + REPEAT_MS);
            }
        }
        let Some(stroke) = self.stroke.as_ref() else {
            return;
        };
        if stroke.popup.is_some() || now_ms.saturating_sub(stroke.at) < LONG_MS {
            return;
        }
        let Some(key) = self.keys.get(stroke.key).copied() else {
            return;
        };
        // Сдвинутый палец — начало жеста, а не долгое нажатие.
        let moved: i32 = stroke.points.windows(2).map(|p| (p[0].x - p[1].x).abs() + (p[0].y - p[1].y).abs()).sum();
        if moved > key.rect.w as i32 / 2 {
            return;
        }
        let chars = self.alternates(&key);
        if chars.len() < 2 {
            return;
        }
        let ctx = Ctx::scaled(self.scale);
        let cell = ctx.px(ALT_CELL);
        let pad = ctx.px(4);
        let w = cell * chars.len() as u32 + pad * 2;
        let h = ctx.px(ALT_H);
        let x = (key.rect.x + key.rect.w as i32 / 2 - (cell / 2 + pad) as i32)
            .clamp(0, self.surface.width().saturating_sub(w) as i32);
        let y = (key.rect.y - h as i32 - ctx.px(4) as i32).max(0);
        crate::kprintln!("  keyboard    : alternates {:?}", chars);
        // Первый вариант — сама буква, выбран второй: ради него и держали.
        let popup = Popup { rect: Rect::new(x, y, w, h), chars, selected: 1 };
        self.trail.clear();
        if let Some(index) = self.pressed.take() {
            self.redraw_key(index);
        }
        if let Some(stroke) = self.stroke.as_mut() {
            stroke.popup = Some(popup);
        }
        self.draw_popup();
    }

    /// Варианты буквы: она сама, особая буква её клавиши и цифра из угла.
    fn alternates(&self, key: &Key) -> Vec<char> {
        let Some(letter) = key.letter else {
            return Vec::new();
        };
        let upper = self.shift;
        let case = |c: char| if upper { c.to_uppercase().next().unwrap_or(c) } else { c };
        let mut chars = alloc::vec![case(letter)];
        match letter {
            'е' => chars.push(case('ё')),
            'ь' => chars.push(case('ъ')),
            _ => {}
        }
        if let Some(digit) = key.digit {
            chars.push(digit);
        }
        chars
    }

    /// Нарисовать строку вариантов поверх клавиш.
    fn draw_popup(&mut self) {
        let Some(popup) = self.stroke.as_ref().and_then(|s| s.popup.as_ref()) else {
            return;
        };
        let (rect, chars, selected) = (popup.rect, popup.chars.clone(), popup.selected);
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let radius = ctx.px(14);
        draw::rounded(&mut self.surface, rect, radius, ctx.under, 255);
        draw::rounded(&mut self.surface, rect, radius, p.btn.color, p.btn.alpha);
        draw::rounded_stroke(&mut self.surface, rect, radius, p.accline.color, p.accline.alpha);
        let cell = ctx.px(ALT_CELL);
        let pad = ctx.px(4);
        let mut buf = [0u8; 4];
        for (index, ch) in chars.iter().enumerate() {
            let slot = Rect::new(rect.x + (pad + cell * index as u32) as i32, rect.y + pad as i32, cell, rect.h - pad * 2);
            let ink = if index == selected {
                draw::rounded_gradient(&mut self.surface, slot, ctx.px(10), p.acc, p.acc2, 255);
                Color::rgb(0xFF, 0xFF, 0xFF)
            } else {
                p.ink
            };
            let label = ch.encode_utf8(&mut buf);
            let w = ctx.face(Role::Strong).width(label);
            paint::text(ctx, &mut self.surface, Role::Strong, slot.x + (slot.w as i32 - w as i32) / 2, paint::baseline(ctx, Role::Strong, slot), label, ink);
        }
        self.damage = self.damage.union(&rect);
    }

    // -----------------------------------------------------------------------
    // След пальца
    // -----------------------------------------------------------------------

    /// Шаг следа перед кадром: забыть истлевшие точки и сказать, что
    /// перерисовать, — прошлое место следа вместе с нынешним. Пусто — следа
    /// нет и не было.
    pub fn advance_trail(&mut self, now_ms: u64) -> Rect {
        let live = self.stroke.is_some();
        // Пока палец на стекле, путь нужен целиком — для распознавания он
        // хранится отдельно, а след теряет только хвост.
        self.trail.retain(|(_, at)| now_ms.saturating_sub(*at) < TRAIL_MS);
        if !live && self.trail.len() == 1 {
            self.trail.clear();
        }
        let r = Ctx::scaled(self.scale).px(TRAIL_R) as i32 + 1;
        let mut now = Rect::EMPTY;
        for (point, _) in self.trail.iter() {
            now = now.union(&Rect::new(point.x - r, point.y - r, (r * 2) as u32, (r * 2) as u32));
        }
        let damage = now.union(&self.trail_drawn);
        self.trail_drawn = now;
        damage
    }

    /// Есть ли что рисовать: след ещё не истлел.
    #[must_use]
    pub fn trail_visible(&self) -> bool {
        self.trail.len() > 1
    }

    /// Нарисовать след в полосу кадра. Он толще у пальца и истончается к
    /// хвосту: радиус точки убывает с её возрастом.
    ///
    /// След рисуется кружками, сплошным цветом. Полупрозрачные кружки внахлёст
    /// темнели бы на каждом наложении, и путь выглядел бы бусами.
    pub fn draw_trail(&self, back: &mut Surface, band: Rect, dy: i32, now_ms: u64) {
        let full = Ctx::scaled(self.scale).px(TRAIL_R) as i64;
        let color = theme::palette().acc;
        for pair in self.trail.windows(2) {
            let ((a, _), (b, at)) = (pair[0], pair[1]);
            let age = now_ms.saturating_sub(at).min(TRAIL_MS) as i64;
            let r = (full * (TRAIL_MS as i64 - age) / TRAIL_MS as i64).max(1) as i32;
            let reach = Rect::new(a.x.min(b.x) - r, a.y.min(b.y) - r, ((a.x - b.x).abs() + r * 2) as u32, ((a.y - b.y).abs() + r * 2) as u32);
            if reach.intersect(&band).is_empty() {
                continue;
            }
            let dx = i64::from(b.x - a.x);
            let dy_seg = i64::from(b.y - a.y);
            let len = ((dx * dx + dy_seg * dy_seg) as u64).isqrt() as i64;
            let steps = (len / i64::from((r / 2).max(1))).max(1);
            for step in 0..=steps {
                let x = a.x + (dx * step / steps) as i32;
                let y = a.y + (dy_seg * step / steps) as i32;
                let disc = Rect::new(x - r, y - r, (r * 2) as u32, (r * 2) as u32);
                if disc.intersect(&band).is_empty() {
                    continue;
                }
                draw::rounded(back, disc.translate(0, dy), r as u32, color, 255);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Раскладка кнопок и рисование
    // -----------------------------------------------------------------------

    /// Разложить кнопки. Числа — макет, в его точках.
    fn layout(&mut self) {
        let ctx = Ctx::scaled(self.scale);
        let width = self.surface.width();
        let mut keys = Vec::new();

        let chip_y = ctx.px(8) as i32;
        let chip_h = ctx.px(32);
        if self.suggestions.is_empty() {
            // Строка быстрых команд: ширина кнопки — по слову.
            let mono = ctx.face(Role::Mono);
            let mut x = ctx.px(12) as i32;
            for word in CHIPS {
                let w = mono.width(word) + ctx.px(24);
                keys.push(Key {
                    rect: Rect::new(x, chip_y, w, chip_h),
                    action: Action::Chip(word),
                    look: Look::Chip,
                    label: word,
                    upper: word,
                    letter: None,
                    digit: None,
                });
                x += (w + ctx.px(7)) as i32;
            }
            keys.push(Key {
                rect: Rect::new(x, chip_y, ctx.px(36), chip_h),
                action: Action::Tab,
                look: Look::Chip,
                label: "Tab",
                upper: "Tab",
                letter: None,
            digit: None,
            });
        } else {
            // Подсказки: три равные ячейки, лучшая посередине (эскиз, кадр 1).
            let side = ctx.px(12);
            let slot = width.saturating_sub(side * 2) / SUGGESTIONS as u32;
            for (place, index) in [1usize, 0, 2].into_iter().enumerate() {
                let Some(word) = self.suggestions.get(index).copied() else {
                    continue;
                };
                keys.push(Key {
                    rect: Rect::new(side as i32 + (place as u32 * slot) as i32, chip_y, slot, chip_h),
                    action: Action::Suggest(index),
                    look: if index == self.current { Look::Accent } else { Look::Word },
                    label: word,
                    upper: word,
                    letter: None,
                    digit: None,
                });
            }
        }

        let side = ctx.px(PAD_SIDE) as i32;
        let inner = width.saturating_sub(ctx.px(PAD_SIDE) * 2);
        let key_h = ctx.px(KEY_H);
        let gap = ctx.px(KEY_GAP);
        let mut y = ctx.px(CHIPS_H + PAD_TOP) as i32;

        let letters = self.page == Page::Letters;
        let rows: [(&'static str, &'static str); 3] = match (self.page, self.lang) {
            (Page::Letters, Lang::Us) => EN_ROWS,
            (Page::Letters, Lang::Ru) => RU_ROWS,
            (Page::Symbols, _) => SYMBOL_ROWS,
        };

        // Ряд 1 — во всю ширину.
        push_row(&mut keys, rows[0], side, y, inner, key_h, gap, letters, letters);
        y += (key_h + ctx.px(ROW_GAP)) as i32;

        // Ряд 2 — с полями по 16, как в макете: так клавиши второго ряда стоят
        // между клавишами первого, а не под ними. В русской раскладке во втором
        // ряду одиннадцать клавиш, как и в первом, и полей нет.
        let indent = if rows[1].0.chars().count() < rows[0].0.chars().count() { ctx.px(16) } else { 0 };
        push_row(&mut keys, rows[1], side + indent as i32, y, inner.saturating_sub(indent * 2), key_h, gap, letters, false);
        y += (key_h + ctx.px(ROW_GAP)) as i32;

        // Ряд 3 — ⇧ и ⌫ по краям, по 44.
        let wide = ctx.px(44);
        keys.push(Key {
            rect: Rect::new(side, y, wide, key_h),
            action: Action::Shift,
            look: Look::Ghost,
            label: "",
            upper: "",
            letter: None,
            digit: None,
        });
        let middle_w = inner.saturating_sub((wide + gap) * 2);
        push_row(&mut keys, rows[2], side + (wide + gap) as i32, y, middle_w, key_h, gap, letters, false);
        keys.push(Key {
            rect: Rect::new(side + (inner - wide) as i32, y, wide, key_h),
            action: Action::Backspace,
            look: Look::Ghost,
            label: "",
            upper: "",
            letter: None,
            digit: None,
        });
        y += (key_h + ctx.px(ROW_GAP)) as i32;

        // Ряд 4 — ?123, /, пробел с именем языка, -, ввод.
        let fixed = [
            (ctx.px(52), Action::Page, Look::Ghost, self.page_label()),
            (wide, Action::Char('/'), Look::Ghost, "/"),
        ];
        let tail = [(wide, Action::Char('-'), Look::Ghost, "-"), (ctx.px(62), Action::Enter, Look::Accent, "")];
        let fixed_w: u32 = fixed.iter().chain(tail.iter()).map(|k| k.0 + gap).sum();
        let space_w = inner.saturating_sub(fixed_w);
        let mut x = side;
        for (w, action, look, label) in fixed {
            keys.push(Key { rect: Rect::new(x, y, w, key_h), action, look, label, upper: label, letter: None, digit: None });
            x += (w + gap) as i32;
        }
        let name = match self.lang {
            Lang::Us => "English",
            Lang::Ru => "Русский",
        };
        keys.push(Key {
            rect: Rect::new(x, y, space_w, key_h),
            action: Action::Space,
            look: Look::Plain,
            label: name,
            upper: name,
            letter: None,
            digit: None,
        });
        x += (space_w + gap) as i32;
        for (w, action, look, label) in tail {
            keys.push(Key { rect: Rect::new(x, y, w, key_h), action, look, label, upper: label, letter: None, digit: None });
            x += (w + gap) as i32;
        }

        self.keys = keys;
    }

    const fn page_label(&self) -> &'static str {
        match self.page {
            Page::Letters => "?123",
            Page::Symbols => match self.lang {
                Lang::Us => "ABC",
                Lang::Ru => "АБВ",
            },
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
        let radius = ctx.px(if matches!(key.look, Look::Chip | Look::Word) { 11 } else { 10 });
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
            Look::Word => p.ink3,
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
        // Подсказки — обычным текстом, имя языка на пробеле — мелким и тусклым.
        let (role, ink) = match key.action {
            Action::Space => (Role::Caption, p.ink4),
            Action::Suggest(_) => (Role::Strong, ink),
            _ => match key.look {
                Look::Chip => (Role::Mono, ink),
                _ if key.label.chars().count() > 1 => (Role::Mono, ink),
                _ => (Role::Strong, ink),
            },
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
        // Цифра в углу — подсказка, что у клавиши есть долгое нажатие.
        if let Some(digit) = key.digit {
            let mut buf = [0u8; 4];
            let text = digit.encode_utf8(&mut buf);
            let small = ctx.face(Role::MonoSmall);
            let dw = small.width(text) as i32;
            paint::text(
                ctx,
                &mut self.surface,
                Role::MonoSmall,
                key.rect.right() - dw - ctx.px(4) as i32,
                key.rect.y + ctx.px(1) as i32,
                text,
                p.ink4,
            );
        }
    }
}

/// Разложить ряд одинаковых клавиш по ширине. `row` — строчные и заглавные
/// подписи, по знаку на клавишу; `letters` — буквы ли это (через них идёт жест).
#[allow(clippy::too_many_arguments)]
fn push_row(
    keys: &mut Vec<Key>,
    row: (&'static str, &'static str),
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    gap: u32,
    letters: bool,
    digits: bool,
) {
    let (lower, upper) = row;
    let count = lower.chars().count() as u32;
    if count == 0 {
        return;
    }
    let key_w = width.saturating_sub(gap * (count - 1)) / count;
    // Остаток от деления раздаётся последней клавише, чтобы ряд доходил до края
    // ровно: иначе правый край ряда гулял бы на пару точек от ряда к ряду.
    let spare = width.saturating_sub(key_w * count + gap * (count - 1));
    for (index, ((at, ch), (up_at, up))) in lower.char_indices().zip(upper.char_indices()).enumerate() {
        let last = index as u32 + 1 == count;
        keys.push(Key {
            rect: Rect::new(
                x + (index as u32 * (key_w + gap)) as i32,
                y,
                key_w + if last { spare } else { 0 },
                height,
            ),
            action: Action::Char(ch),
            look: Look::Plain,
            label: &lower[at..at + ch.len_utf8()],
            upper: &upper[up_at..up_at + up.len_utf8()],
            letter: letters.then_some(ch),
            digit: if digits { "1234567890".chars().nth(index) } else { None },
        });
    }
}

/// Ряды букв: строчные и заглавные, по знаку на клавишу.
const EN_ROWS: [(&str, &str); 3] =
    [("qwertyuiop", "QWERTYUIOP"), ("asdfghjkl", "ASDFGHJKL"), ("zxcvbnm", "ZXCVBNM")];

/// ЙЦУКЕН: одиннадцать, одиннадцать и девять — «х», «ж», «э», «б», «ю» стоят
/// на своих местах, а не прячутся за долгим нажатием.
const RU_ROWS: [(&str, &str); 3] =
    [("йцукенгшщзх", "ЙЦУКЕНГШЩЗХ"), ("фывапролджэ", "ФЫВАПРОЛДЖЭ"), ("ячсмитьбю", "ЯЧСМИТЬБЮ")];

/// Цифры и знаки — одни для обоих языков: стол сам найдёт, какой клавишей
/// знак набирается в действующей раскладке.
const SYMBOL_ROWS: [(&str, &str); 3] =
    [("1234567890", "1234567890"), (":;()$&@\"=", ":;()$&@\"="), (".,?!'|_", ".,?!'|_")];

/// Сколько места клавиатура отнимает снизу у окна над ней, в точках экрана.
#[must_use]
pub fn reserved(scale: u32) -> u32 {
    Ctx::scaled(scale).px(HEIGHT + theme::M_INSET + GAP)
}
