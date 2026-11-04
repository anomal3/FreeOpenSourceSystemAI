// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Полоска жеста телефона — навигация как у iPhone (04.11).
//!
//! # Что это
//!
//! Пока на экране программа, дока нет: окно занимает всё от строки состояния
//! до тонкой полосы у низа экрана, а в полосе лежит светлая «пилюля». Палец,
//! опущенный у самого низа, ведут:
//!
//! - вверх и отпускают — домой: программа сворачивается в стопку дока;
//! - вверх и задерживают, или вверх выше двух пятых экрана — переключатель
//!   программ (лист свёрнутых, в который уходит и эта программа);
//! - вбок вдоль полосы — соседняя программа.
//!
//! Здесь только числа и разбор движения: что делать по итогу, решает стол
//! (`ui::gesture_end`), потому что сворачивать и открывать листы умеет он.
//!
//! # Почему жест начинается только у низа
//!
//! Палец, ведущий вверх по середине окна, прокручивает «Параметры» и тянет
//! выделение терминала. Отнять у окна этот жест значило бы сломать его; полоса
//! под окном ничья, и её можно отдать системе целиком.

use mini_ui::paint::Ctx;
use mini_ui::{Rect, theme};

/// Высота полосы жеста под окном во весь экран, в точках макета.
pub const BAR_H: u32 = 20;
/// Откуда начинается жест: столько точек от низа экрана (на 720×1600 —
/// сорок точек экрана).
pub const ZONE_H: u32 = 20;
/// Подъём пальца, после которого отпускание уводит домой.
pub const HOME_RISE: u32 = 40;
/// Сдвиг вбок, после которого отпускание переключает программу.
pub const SIDE_SHIFT: u32 = 40;
/// Подъём, после которого задержка пальца открывает переключатель.
pub const HOLD_RISE: u32 = 50;
/// Сколько палец должен простоять поднятым, чтобы открылся переключатель.
pub const HOLD_MS: u64 = 350;
/// Подъём выше этой доли экрана (в процентах) открывает переключатель и без
/// задержки.
pub const SWITCH_PERCENT: i32 = 40;
/// Подъём карточки в переключателе, после которого она закрывает программу.
pub const CARD_RISE: u32 = 40;

/// Высота полосы жеста в точках экрана.
#[must_use]
pub fn bar_height(scale: u32) -> u32 {
    Ctx::scaled(scale).px(BAR_H)
}

/// Где лежит пилюля: по середине ширины, у самого низа экрана — под
/// клавиатурой, когда та открыта (низ клавиатуры отстоит от края на поле
/// [`theme::M_INSET`]).
#[must_use]
pub fn pill(screen_w: u32, screen_h: u32, scale: u32) -> Rect {
    let ctx = Ctx::scaled(scale);
    // Всегда лёжа (05.11): не шире трети экрана и не толще четверти своей
    // длины — на узком экране или крупном масштабе пилюля не встанет торчком.
    let w = ctx.px(theme::M_HOME_W).min(screen_w / 3).max(4);
    let h = ctx.px(theme::M_HOME_H).min(w / 4).max(1);
    let y = screen_h as i32 - ctx.px(8) as i32 - h as i32;
    Rect::new((screen_w as i32 - w as i32) / 2, y, w, h)
}

/// Попадает ли нажатие в место, откуда начинается жест.
#[must_use]
pub fn in_zone(y: i32, screen_h: u32, scale: u32) -> bool {
    y >= screen_h as i32 - Ctx::scaled(scale).px(ZONE_H) as i32
}

/// Палец, опущенный у низа экрана, пока его ведут.
#[derive(Clone, Copy)]
pub struct Swipe {
    pub x0: i32,
    pub y0: i32,
    pub x: i32,
    pub y: i32,
    /// Когда палец сдвинулся последний раз — от этого мига считается задержка.
    pub moved_at: u64,
    /// Переключатель уже открыт задержкой: отпускание ничего не делает.
    pub done: bool,
}

/// Чем кончился жест.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Nothing,
    Home,
    Switcher,
    /// Палец ушёл вправо — к программе, открытой перед этой.
    Previous,
    /// Влево — к следующей.
    Next,
}

impl Swipe {
    #[must_use]
    pub const fn new(x: i32, y: i32, now: u64) -> Self {
        Self { x0: x, y0: y, x, y, moved_at: now, done: false }
    }

    /// Насколько палец поднялся.
    #[must_use]
    pub const fn rise(&self) -> i32 {
        self.y0 - self.y
    }

    /// Палец сдвинулся.
    pub fn move_to(&mut self, x: i32, y: i32, now: u64) {
        if (x, y) != (self.x, self.y) {
            self.x = x;
            self.y = y;
            self.moved_at = now;
        }
    }

    /// Простоял ли поднятый палец достаточно, чтобы открыть переключатель.
    #[must_use]
    pub fn held(&self, now: u64, scale: u32) -> bool {
        !self.done
            && self.rise() >= Ctx::scaled(scale).px(HOLD_RISE) as i32
            && now.saturating_sub(self.moved_at) >= HOLD_MS
    }

    /// Что значит жест, если палец отпустили здесь.
    #[must_use]
    pub fn outcome(&self, screen_h: u32, scale: u32) -> Outcome {
        if self.done {
            return Outcome::Nothing;
        }
        let ctx = Ctx::scaled(scale);
        let rise = self.rise();
        let side = self.x - self.x0;
        if rise * 100 >= screen_h as i32 * SWITCH_PERCENT {
            return Outcome::Switcher;
        }
        if rise >= ctx.px(HOME_RISE) as i32 && rise >= side.abs() {
            return Outcome::Home;
        }
        if side.abs() >= ctx.px(SIDE_SHIFT) as i32 && side.abs() > rise {
            return if side > 0 { Outcome::Previous } else { Outcome::Next };
        }
        Outcome::Nothing
    }
}
