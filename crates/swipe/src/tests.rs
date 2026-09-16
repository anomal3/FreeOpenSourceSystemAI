// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Проверки распознавания на раскладках с размерами телефонной клавиатуры
//! (экран 720 точек, масштаб 2): клавиша около 54 точек, шаг 64.

extern crate std;

use alloc::vec::Vec;

use super::*;

fn qwerty() -> Layout {
    let mut layout = Layout::new(54);
    for (row, (letters, x0, step)) in [("qwertyuiop", 71, 64), ("asdfghjkl", 103, 64), ("zxcvbnm", 168, 63)].iter().enumerate() {
        for (i, c) in letters.chars().enumerate() {
            layout.add(c, Point::new(x0 + step * i as i32, row as i32 * 90));
        }
    }
    layout
}

fn jcuken() -> Layout {
    let mut layout = Layout::new(52);
    for (row, (letters, x0, step)) in
        [("йцукенгшщзх", 70, 58), ("фывапролджэ", 70, 58), ("ячсмитьбю", 166, 48)].iter().enumerate()
    {
        for (i, c) in letters.chars().enumerate() {
            layout.add(c, Point::new(x0 + step * i as i32, row as i32 * 90));
        }
    }
    let e = layout.center('е').unwrap();
    layout.add('ё', e);
    let soft = layout.center('ь').unwrap();
    layout.add('ъ', soft);
    layout
}

/// Жест «рукой»: через центры букв с точкой через каждые ~16 точек экрана (так
/// сенсор отдаёт касания на обычной скорости пальца), со сбитой на несколько
/// точек рукой и сдвигом всего жеста.
///
/// Шаг — по расстоянию, а не «шесть точек на отрезок»: при равном числе точек
/// на длинном отрезке сглаживание срезало разворот у `e` в «people» на полторы
/// клавиши, чего живой палец не делает — на развороте он замедляется.
fn gesture(layout: &Layout, word: &str, jitter: i32, shift: Point) -> Vec<Point> {
    let mut seed = 12345u32;
    let mut noise = || {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        ((seed >> 16) % (2 * jitter as u32 + 1)) as i32 - jitter
    };
    let centers: Vec<Point> = word.chars().map(|c| layout.center(c).unwrap()).collect();
    let mut path = Vec::new();
    for pair in centers.windows(2) {
        let steps = (pair[0].distance(pair[1]) / 16).max(1) as i32;
        for step in 0..steps {
            let x = pair[0].x + (pair[1].x - pair[0].x) * step / steps;
            let y = pair[0].y + (pair[1].y - pair[0].y) * step / steps;
            path.push(Point::new(x + shift.x + noise(), y + shift.y + noise()));
        }
    }
    let last = centers[centers.len() - 1];
    path.push(Point::new(last.x + shift.x, last.y + shift.y));
    path
}

fn top(dict: &Dictionary, layout: &Layout, path: &[Point]) -> Vec<&'static str> {
    recognize(dict, layout, path, 5).iter().map(|c| c.word).collect()
}

#[test]
fn dictionaries_parse_whole() {
    let en = Dictionary::parse(EN);
    let ru = Dictionary::parse(RU);
    assert_eq!(en.len(), 30_000);
    assert_eq!(ru.len(), 30_000);
    assert_eq!(en.word(0), Some("you"));
    assert_eq!(ru.word(0), Some("я"));
}

#[test]
fn clean_gestures_give_the_word_first() {
    let en = Dictionary::parse(EN);
    let layout = qwerty();
    for word in ["hello", "the", "keyboard", "system", "people", "world", "thanks", "phone"] {
        let path = gesture(&layout, word, 0, Point::new(0, 0));
        let got = top(&en, &layout, &path);
        assert_eq!(got.first().copied(), Some(word), "{word}: {got:?}");
    }
}

#[test]
fn shaky_and_shifted_gestures_still_find_the_word() {
    let en = Dictionary::parse(EN);
    let layout = qwerty();
    for word in ["hello", "keyboard", "world", "phone", "good"] {
        let path = gesture(&layout, word, 9, Point::new(12, -10));
        let got = top(&en, &layout, &path);
        assert!(got.iter().take(3).any(|w| *w == word), "{word}: {got:?}");
    }
}

#[test]
fn russian_words() {
    let ru = Dictionary::parse(RU);
    let layout = jcuken();
    for word in ["привет", "спасибо", "хорошо", "система", "телефон", "что"] {
        let path = gesture(&layout, word, 6, Point::new(-8, 7));
        let got = top(&ru, &layout, &path);
        assert!(got.iter().take(3).any(|w| *w == word), "{word}: {got:?}");
    }
}

#[test]
fn yo_goes_through_the_e_key() {
    let ru = Dictionary::parse(RU);
    let layout = jcuken();
    let path = gesture(&layout, "еще", 0, Point::new(0, 0));
    let got = top(&ru, &layout, &path);
    assert!(got.contains(&"ещё") || got.contains(&"еще"), "{got:?}");
}

#[test]
fn a_gesture_missing_a_letter_does_not_match_it() {
    let en = Dictionary::parse(EN);
    let layout = qwerty();
    // «hlo» не проходит мимо `e` — «hello» подсказываться не должно первым.
    let path = gesture(&layout, "hlo", 0, Point::new(0, 0));
    let got = top(&en, &layout, &path);
    assert_ne!(got.first().copied(), Some("hello"), "{got:?}");
}

#[test]
fn a_short_touch_is_a_tap() {
    let en = Dictionary::parse(EN);
    let layout = qwerty();
    let path = [Point::new(100, 10), Point::new(110, 14)];
    assert!(is_tap(&layout, &path));
    assert!(recognize(&en, &layout, &path, 5).is_empty());
}

#[test]
fn resample_keeps_the_ends_and_spacing() {
    let path = [Point::new(0, 0), Point::new(100, 0), Point::new(100, 100)];
    let out = resample(&path, 5);
    assert_eq!(out, [Point::new(0, 0), Point::new(50, 0), Point::new(100, 0), Point::new(100, 50), Point::new(100, 100)]);
}

#[test]
fn rarity_grows_by_quarters() {
    assert_eq!(rarity(0), 0);
    assert_eq!(rarity(1), 4);
    assert_eq!(rarity(3), 8);
    assert!(rarity(1000) < rarity(20000));
}

#[test]
fn recognition_is_fast_enough() {
    let en = Dictionary::parse(EN);
    let layout = qwerty();
    let path = gesture(&layout, "something", 5, Point::new(0, 0));
    let started = std::time::Instant::now();
    for _ in 0..20 {
        let _ = recognize(&en, &layout, &path, 5);
    }
    let per = started.elapsed() / 20;
    std::println!("recognize: {per:?} per gesture");
}

/// Палец не доезжает до центров, а срезает углы: жест сглажен скользящим
/// средним по пяти точкам, концы на месте — палец ставят на клавишу и снимают
/// с клавиши. У «hello» угол при `e` срезается так на 0,8 клавиши.
#[test]
fn corner_cutting_gestures() {
    let en = Dictionary::parse(EN);
    let ru = Dictionary::parse(RU);
    let (q, j) = (qwerty(), jcuken());
    let smooth = |path: Vec<Point>| -> Vec<Point> {
        (0..path.len())
            .map(|i| {
                if i == 0 || i + 1 == path.len() {
                    return path[i];
                }
                let window = &path[i.saturating_sub(2)..(i + 3).min(path.len())];
                let n = window.len() as i32;
                Point::new(window.iter().map(|p| p.x).sum::<i32>() / n, window.iter().map(|p| p.y).sum::<i32>() / n)
            })
            .collect()
    };
    for word in ["hello", "phone", "keyboard", "people", "thanks"] {
        let got = top(&en, &q, &smooth(gesture(&q, word, 5, Point::new(0, 0))));
        assert!(got.iter().take(3).any(|w| *w == word), "{word}: {got:?}");
    }
    for word in ["привет", "спасибо", "хорошо", "телефон"] {
        let got = top(&ru, &j, &smooth(gesture(&j, word, 5, Point::new(0, 0))));
        assert!(got.iter().take(3).any(|w| *w == word), "{word}: {got:?}");
    }
}


