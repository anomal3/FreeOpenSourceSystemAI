// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Слово жестом: палец ведёт по клавишам, и путь превращается в слово.
//!
//! # Как это устроено
//!
//! У каждого слова есть **идеальный путь** — ломаная через центры его клавиш
//! (повторы подряд схлопнуты: «hello» идёт через `h e l o`). Жест человека
//! сравнивается с идеальными путями слов из словаря, и ближайшие по форме, с
//! поправкой на частоту слова, становятся подсказками.
//!
//! Сравнивать со всеми тридцатью тысячами слов незачем и дорого. Отбор идёт
//! ступенями, от дешёвой к дорогой:
//!
//! 1. **Концы.** Жест начинается у первой буквы и кончается у последней: берутся
//!    слова, у которых первая буква — одна из клавиш рядом с началом жеста, а
//!    последняя — рядом с концом. Словарь заранее разложен по парам букв.
//! 2. **Длина.** Идеальный путь не может быть вдвое длиннее жеста.
//! 3. **Проход.** Каждая буква слова, по порядку, должна лежать рядом с
//!    какой-то точкой жеста — иначе «hello» подошло бы жесту, прошедшему мимо `e`.
//! 4. **Форма.** Оба пути разбиваются на одинаковое число равноотстоящих точек,
//!    и считается среднее расстояние между соответствующими точками.
//!
//! Итог — сумма формы (в процентах ширины клавиши) и цены редкости слова.
//!
//! # Почему целые числа
//!
//! Ядро, которое это вызывает, собирается без плавающей точки в регистрах (см.
//! `rust-toolchain.toml`): программная плавающая точка есть, но она медленна.
//! Координаты здесь — точки экрана, расстояния — корень из целого квадрата.
//!
//! # Почему отдельный крейт
//!
//! По той же причине, что `sysconf`: это чистые функции над числами, и
//! проверяются они на хосте за секунды, а не загрузкой телефона.

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Словарь английских слов: по слову на строку, от частого к редкому.
pub const EN: &str = include_str!("../data/en.txt");
/// Словарь русских слов, в том же виде.
pub const RU: &str = include_str!("../data/ru.txt");

/// На сколько точек делится путь при сравнении формы.
const SHAPE_POINTS: usize = 32;
/// На сколько точек делится жест при проверке прохода: гуще, чем для формы, —
/// короткая буква между двумя дальними иначе проскочила бы между точками.
const PASS_POINTS: usize = 64;
/// Сколько стоит удвоение ранга слова, в процентах ширины клавиши.
///
/// Подобрано тестами: при 4 частое «the» побеждает редкое слово той же формы,
/// но слово, нарисованное заметно точнее, не проигрывает частому.
const FREQ_WEIGHT: u32 = 4;

/// Точка экрана.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    #[must_use]
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    fn distance(self, other: Self) -> u32 {
        let dx = i64::from(self.x - other.x);
        let dy = i64::from(self.y - other.y);
        ((dx * dx + dy * dy) as u64).isqrt() as u32
    }
}

/// Где на клавиатуре стоят буквы.
///
/// Клавиатура знает свои кнопки сама — сюда она кладёт только центры букв и
/// ширину клавиши: от неё зависят все допуски.
pub struct Layout {
    keys: Vec<(char, Point)>,
    key_w: i32,
}

impl Layout {
    #[must_use]
    pub fn new(key_w: i32) -> Self {
        Self { keys: Vec::new(), key_w: key_w.max(1) }
    }

    /// Буква и центр её клавиши. Одна клавиша может нести две буквы — у «е»
    /// есть «ё», и слово «ещё» ведётся через ту же клавишу, что «еще».
    pub fn add(&mut self, letter: char, center: Point) {
        self.keys.push((letter, center));
    }

    #[must_use]
    pub fn center(&self, letter: char) -> Option<Point> {
        self.keys.iter().find(|(c, _)| *c == letter).map(|(_, p)| *p)
    }

    #[must_use]
    pub const fn key_width(&self) -> i32 {
        self.key_w
    }

    /// Буквы, центры которых ближе `radius` к точке.
    fn near(&self, point: Point, radius: u32) -> Vec<char> {
        self.keys.iter().filter(|(_, p)| p.distance(point) <= radius).map(|(c, _)| *c).collect()
    }
}

/// Словарь: слова по рангу и разложенные по паре «первая, последняя буква».
pub struct Dictionary {
    words: Vec<&'static str>,
    by_ends: BTreeMap<(char, char), Vec<u32>>,
}

impl Dictionary {
    /// Разобрать текст словаря: по слову на строку, ранг — номер строки.
    /// Пустые строки пропускаются.
    #[must_use]
    pub fn parse(text: &'static str) -> Self {
        let mut words = Vec::new();
        let mut by_ends: BTreeMap<(char, char), Vec<u32>> = BTreeMap::new();
        for word in text.lines().map(str::trim).filter(|w| !w.is_empty()) {
            let (Some(first), Some(last)) = (word.chars().next(), word.chars().last()) else {
                continue;
            };
            by_ends.entry((first, last)).or_default().push(words.len() as u32);
            words.push(word);
        }
        Self { words, by_ends }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.words.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Слово с этим рангом (ноль — самое частое).
    #[must_use]
    pub fn word(&self, rank: usize) -> Option<&'static str> {
        self.words.get(rank).copied()
    }
}

/// Подсказка: слово и его цена (меньше — лучше).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub word: &'static str,
    pub score: u32,
}

/// Длина ломаной.
fn length(path: &[Point]) -> u32 {
    path.windows(2).map(|pair| pair[0].distance(pair[1])).sum()
}

/// Нажатие, а не жест: палец почти не сдвинулся.
///
/// Полширины клавиши — столько проезжает палец, просто опускаясь на стекло.
#[must_use]
pub fn is_tap(layout: &Layout, path: &[Point]) -> bool {
    length(path) < (layout.key_w / 2) as u32
}

/// Разбить ломаную на `count` равноотстоящих по длине точек.
fn resample(path: &[Point], count: usize) -> Vec<Point> {
    let mut out = Vec::with_capacity(count);
    let Some(&first) = path.first() else {
        return out;
    };
    let total = u64::from(length(path));
    if total == 0 || count < 2 {
        out.resize(count, first);
        return out;
    }
    let mut segment = 0;
    let mut walked = 0u64; // длина до начала текущего отрезка
    for index in 0..count {
        let target = total * index as u64 / (count as u64 - 1);
        // Дойти до отрезка, внутри которого лежит цель.
        while segment + 1 < path.len() - 1 {
            let seg = u64::from(path[segment].distance(path[segment + 1]));
            if walked + seg >= target {
                break;
            }
            walked += seg;
            segment += 1;
        }
        let (a, b) = (path[segment], path[(segment + 1).min(path.len() - 1)]);
        let seg = u64::from(a.distance(b));
        let along = target.saturating_sub(walked).min(seg);
        let point = if seg == 0 {
            a
        } else {
            Point::new(
                a.x + ((i64::from(b.x - a.x) * along as i64) / seg as i64) as i32,
                a.y + ((i64::from(b.y - a.y) * along as i64) / seg as i64) as i32,
            )
        };
        out.push(point);
    }
    out
}

/// Идеальный путь слова: центры его клавиш, повторы подряд схлопнуты.
/// `None` — в слове есть буква, которой на клавиатуре нет.
fn ideal(layout: &Layout, word: &str) -> Option<Vec<Point>> {
    let mut path: Vec<Point> = Vec::new();
    for letter in word.chars() {
        let center = layout.center(letter)?;
        if path.last() != Some(&center) {
            path.push(center);
        }
    }
    Some(path)
}

/// Проходит ли жест возле каждой клавиши слова, по порядку.
fn passes(dense: &[Point], keys: &[Point], tolerance: u32) -> bool {
    let mut from = 0;
    for key in keys {
        let Some(found) = dense[from..].iter().position(|p| p.distance(*key) <= tolerance) else {
            return false;
        };
        from += found;
    }
    true
}

/// Цена редкости: `FREQ_WEIGHT` за каждое удвоение ранга, с шагом в четверть.
fn rarity(rank: u32) -> u32 {
    let x = u64::from(rank) + 1;
    let msb = 63 - x.leading_zeros();
    let quarters = if msb >= 2 { (x >> (msb - 2)) & 3 } else { (x << (2 - msb)) & 3 };
    (msb * 4 + quarters as u32) * FREQ_WEIGHT / 4
}

/// Узнать слово по жесту: не больше `max` подсказок, лучшая первой.
///
/// Жест короче полуклавиши — это нажатие ([`is_tap`]), и подсказок нет.
#[must_use]
pub fn recognize(dict: &Dictionary, layout: &Layout, path: &[Point], max: usize) -> Vec<Candidate> {
    let mut found: Vec<Candidate> = Vec::new();
    if path.len() < 2 || is_tap(layout, path) || max == 0 {
        return found;
    }
    let key_w = layout.key_w as u32;
    let gesture_len = length(path);
    // Концы жеста: палец начинает и кончает не точно в центре, а где-то на
    // клавише, — и нередко на соседней.
    let ends = key_w * 6 / 5;
    let starts = layout.near(path[0], ends);
    let stops = layout.near(path[path.len() - 1], ends);
    let dense = resample(path, PASS_POINTS);
    let shape = resample(path, SHAPE_POINTS);

    for first in &starts {
        for last in &stops {
            let Some(ranks) = dict.by_ends.get(&(*first, *last)) else {
                continue;
            };
            for &rank in ranks {
                let word = dict.words[rank as usize];
                if let Some(score) = score(layout, word, rank, gesture_len, &dense, &shape) {
                    found.push(Candidate { word, score });
                }
            }
        }
    }
    found.sort_by_key(|c| c.score);
    found.dedup_by_key(|c| c.word);
    found.truncate(max);
    found
}

fn score(layout: &Layout, word: &'static str, rank: u32, gesture_len: u32, dense: &[Point], shape: &[Point]) -> Option<u32> {
    let key_w = layout.key_w as u32;
    let keys = ideal(layout, word)?;
    // Слово из одной клавиши жестом не рисуют — его нажимают.
    if keys.len() < 2 {
        return None;
    }
    let ideal_len = length(&keys);
    if ideal_len > gesture_len * 2 + key_w * 2 || ideal_len * 2 + key_w * 2 < gesture_len {
        return None;
    }
    // Допуск — полторы клавиши: палец срезает углы, и буква на изломе пути
    // остаётся в стороне от него (у «hello» — `e`). Слово, прошедшее мимо
    // буквы ближе полутора клавиш, дальше судится по форме, а не отбрасывается.
    if !passes(dense, &keys, key_w * 3 / 2) {
        return None;
    }
    let target = resample(&keys, SHAPE_POINTS);
    let sum: u32 = shape.iter().zip(target.iter()).map(|(a, b)| a.distance(*b)).sum();
    let mean = sum / SHAPE_POINTS as u32;
    Some(mean * 100 / key_w + rarity(rank))
}

#[cfg(test)]
mod tests;
