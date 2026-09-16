//! Проверки против того, что рисует GDI+ на Windows.
//!
//! Числа сняты пробой 2026-09-16 (.NET 10, System.Drawing.Common, 96 dpi):
//! холст 40×40, залитый белым, одна фигура, строки точек через `GetPixel`.
//! Сверяются правила, от которых зависит, совпадут ли чужие программы:
//! центры точек на целых, правый и нижний край заливки не входят, тонкое перо
//! включает оба конца, округление смешивания. Края сглаженных кривых GDI+
//! считает приближённо (левый и правый край одного круга у него разные), и
//! их точные значения здесь не проверяются.

use std::vec::Vec;

use crate::draw::{fill_path, stroke_path};
use crate::geom::{Matrix, TYPE_CLOSE, TYPE_LINE, TYPE_START};
use crate::image::{Interpolation, Source};
use crate::paint::{Canvas, Compositing, Paint, PixelFormat, Target, Wrap};
use crate::region::{self, Combine};
use crate::scan::{Bounds, FillRule};
use crate::stroke::{Cap, Join, Pen};

const WHITE: u32 = 0xFFFF_FFFF;

struct Board {
    pixels: Vec<u32>,
}

impl Board {
    fn new() -> Self {
        Self { pixels: std::vec![WHITE; 40 * 40] }
    }

    fn target(&mut self) -> Target<'_, 'static> {
        Target {
            canvas: Canvas { pixels: &mut self.pixels, width: 40, height: 40, format: PixelFormat::Argb },
            compositing: Compositing::SourceOver,
            clip: Bounds::new(0, 0, 40, 40),
            mask: None,
            shift: 0.5,
        }
    }

    fn at(&self, x: usize, y: usize) -> u32 {
        self.pixels[y * 40 + x]
    }

    fn channel(&self, y: usize, xs: core::ops::RangeInclusive<usize>, shift: u32) -> Vec<u32> {
        xs.map(|x| (self.at(x, y) >> shift) & 0xFF).collect()
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> (Vec<f32>, Vec<u8>) {
    (std::vec![x, y, x + w, y, x + w, y + h, x, y + h], std::vec![TYPE_START, TYPE_LINE, TYPE_LINE, TYPE_LINE | TYPE_CLOSE])
}

fn line(x0: f32, y0: f32, x1: f32, y1: f32) -> (Vec<f32>, Vec<u8>) {
    (std::vec![x0, y0, x1, y1], std::vec![TYPE_START, TYPE_LINE])
}

fn pen(width: f64) -> Pen<'static> {
    Pen { width, join: Join::Miter, start_cap: Cap::Flat, end_cap: Cap::Flat, miter_limit: 10.0, dashes: &[], dash_offset: 0.0 }
}

const RED: Paint<'static> = Paint::Solid(0xFFFF_0000);
const BLACK: Paint<'static> = Paint::Solid(0xFF00_0000);

#[test]
fn fill_excludes_the_right_and_bottom_edge() {
    let mut board = Board::new();
    let (points, types) = rect(10.0, 10.0, 20.0, 10.0);
    fill_path(&mut board.target(), &RED, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, false).unwrap();
    // fillrect y10: 255 255 0 0 0 / 0 0 255 255; y19 и y20 у столбцов 9, 10.
    assert_eq!(board.channel(10, 8..=12, 8), [255, 255, 0, 0, 0]);
    assert_eq!(board.channel(10, 28..=31, 8), [0, 0, 255, 255]);
    assert_eq!(board.channel(19, 9..=10, 8), [255, 0]);
    assert_eq!(board.channel(20, 9..=10, 8), [255, 255]);
}

#[test]
fn antialiased_edges_on_whole_coordinates_are_half_covered() {
    let mut board = Board::new();
    let (points, types) = rect(10.0, 10.0, 20.0, 10.0);
    fill_path(&mut board.target(), &RED, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, true).unwrap();
    assert_eq!(board.channel(12, 8..=12, 8), [255, 255, 127, 0, 0]);
    assert_eq!(board.channel(12, 28..=31, 8), [0, 0, 127, 255]);
    let mut board = Board::new();
    let (points, types) = rect(10.5, 10.0, 20.0, 10.0);
    fill_path(&mut board.target(), &RED, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, true).unwrap();
    assert_eq!(board.channel(12, 8..=12, 8), [255, 255, 255, 0, 0]);
    assert_eq!(board.channel(12, 28..=31, 8), [0, 0, 0, 255]);
}

#[test]
fn a_thin_pen_includes_both_ends() {
    let mut board = Board::new();
    let (points, types) = rect(5.0, 5.0, 10.0, 10.0);
    stroke_path(&mut board.target(), &Paint::Solid(0xFF00_00FF), &points, &types, &Matrix::IDENTITY, &pen(1.0), false).unwrap();
    assert_eq!(board.channel(5, 3..=17, 16), [255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255]);
    assert_eq!(board.channel(10, 3..=17, 16), [255, 255, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 255, 255]);
    assert_eq!(board.channel(15, 3..=17, 16), [255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255]);
    assert_eq!(board.channel(16, 3..=17, 16), [255; 15]);

    let mut board = Board::new();
    for (x0, y0, x1, y1) in [(2.0, 10.0, 30.0, 10.0), (2.0, 20.0, 30.0, 27.0)] {
        let (points, types) = line(x0, y0, x1, y1);
        stroke_path(&mut board.target(), &BLACK, &points, &types, &Matrix::IDENTITY, &pen(1.0), false).unwrap();
    }
    assert_eq!(board.channel(9, 0..=4, 16), [255; 5]);
    assert_eq!(board.channel(10, 0..=4, 16), [255, 255, 0, 0, 0]);
    assert_eq!(board.channel(10, 28..=32, 16), [0, 0, 0, 255, 255]);
    // dline: строка, где закрашены столбцы (снято пробой).
    let expected = [(20, 2, 4), (21, 5, 8), (22, 9, 12), (23, 13, 16), (24, 17, 20), (25, 21, 24), (26, 25, 28), (27, 29, 30)];
    for (y, first, last) in expected {
        for x in 0..=32 {
            let black = (board.at(x, y) >> 16) & 0xFF == 0;
            assert_eq!(black, x >= first && x <= last, "diagonal at ({x}, {y})");
        }
    }
}

#[test]
fn a_wide_pen_is_a_rectangle_around_the_line() {
    for (width, rows, antialias) in [(5.0, 18..=22, false), (4.0, 18..=21, false), (5.0, 18..=22, true)] {
        let mut board = Board::new();
        let (points, types) = line(5.0, 20.0, 35.0, 20.0);
        stroke_path(&mut board.target(), &BLACK, &points, &types, &Matrix::IDENTITY, &pen(width), antialias).unwrap();
        for y in 16..=24 {
            let expected: Vec<u32> = if rows.contains(&y) {
                if antialias { std::vec![255, 255, 255, 127, 0, 0, 0] } else { std::vec![255, 255, 255, 0, 0, 0, 0] }
            } else {
                std::vec![255; 7]
            };
            assert_eq!(board.channel(y, 2..=8, 16), expected, "width {width} aa {antialias} row {y}");
        }
    }
}

#[test]
fn blending_rounds_like_gdiplus() {
    let mut board = Board::new();
    let (points, types) = rect(0.0, 0.0, 10.0, 10.0);
    let fill = |board: &mut Board, argb: u32| {
        fill_path(&mut board.target(), &Paint::Solid(argb), &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, false).unwrap();
    };
    fill(&mut board, 0x8000_00FF);
    assert_eq!(board.at(5, 5), 0xFF7F_7FFF);
    let mut board = Board::new();
    fill(&mut board, 0x64FF_0000);
    fill(&mut board, 0xC800_8000);
    assert_eq!(board.at(5, 5), 0xFF37_8521, "alpha twice 255,55,133,33");
    let mut board = Board { pixels: std::vec![0; 40 * 40] };
    fill(&mut board, 0x8000_00FF);
    assert_eq!(board.at(5, 5), 0x8000_00FF);
    fill(&mut board, 0x64FF_0000);
    assert_eq!(board.at(5, 5), 0xB28F_006F, "alpha on alpha 178,143,0,111");
}

#[test]
fn polygon_edges_follow_the_center_rule() {
    let mut board = Board::new();
    let points = [0.0, 0.0, 30.0, 0.0, 0.0, 30.0];
    let types = [TYPE_START, TYPE_LINE, TYPE_LINE | TYPE_CLOSE];
    fill_path(&mut board.target(), &BLACK, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, false).unwrap();
    for (y, filled) in [(0, 30), (5, 25), (10, 20), (15, 15), (20, 10), (25, 5), (30, 0)] {
        for x in 0..=32 {
            assert_eq!((board.at(x, y) >> 16) & 0xFF == 0, x < filled, "triangle at ({x}, {y})");
        }
    }
}

#[test]
fn linear_gradient_matches_gdiplus() {
    let mut board = Board::new();
    let (points, types) = rect(0.0, 0.0, 40.0, 10.0);
    let paint = Paint::Linear {
        to_brush: Matrix::IDENTITY,
        start: crate::geom::Point::new(0.0, 0.0),
        end: crate::geom::Point::new(40.0, 0.0),
        positions: &[0.0, 1.0],
        colors: &[0xFF00_0000, WHITE],
        wrap: Wrap::Tile,
    };
    fill_path(&mut board.target(), &paint, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, false).unwrap();
    let expected = [
        0, 6, 13, 19, 26, 32, 38, 45, 51, 58, 64, 70, 77, 83, 90, 96, 102, 109, 115, 122, 128, 134, 140, 146, 153, 159, 165,
        172, 178, 185, 191, 197, 204, 210, 217, 223, 229, 236, 242, 249,
    ];
    // Совпадают 39 точек из 40: в двадцать первой (t = 0.525) GDI+ ещё
    // считает от чёрного конца и даёт 134, а правило «от ближнего конца» —
    // 133. Где у GDI+ проходит граница между половинами, одной пробой не
    // установить; расхождение в единицу канала глазу не видно.
    let ours = board.channel(5, 0..=39, 16);
    let exact = ours.iter().zip(expected).filter(|(a, b)| **a == *b).count();
    assert!(ours.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1), "{ours:?}");
    assert_eq!(exact, 39, "{ours:?}");
}

#[test]
fn images_land_pixel_for_pixel() {
    let mut source = std::vec![0xFFFF_0000u32; 100];
    source[0] = 0xFF00_00FF;
    let mut board = Board::new();
    let place = Matrix { dx: 5.0, dy: 5.0, ..Matrix::IDENTITY };
    let src = Source { pixels: &source, width: 10, height: 10 };
    crate::image::draw(&mut board.target(), &src, [0.0, 0.0, 10.0, 10.0], &place, Interpolation::Bilinear, 255);
    assert_eq!(board.channel(5, 3..=16, 8), [255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255]);
    assert_eq!(board.at(5, 5), 0xFF00_00FF);
    assert_eq!(board.at(6, 6), 0xFFFF_0000);

    // Увеличение вдвое (imgs и imgn пробы): смесь — мягкий правый край и
    // смешанный угол, ближайшая — пустая последняя точка.
    let double = Matrix { m11: 2.0, m22: 2.0, ..Matrix::IDENTITY };
    let mut board = Board::new();
    crate::image::draw(&mut board.target(), &src, [0.0, 0.0, 10.0, 10.0], &double, Interpolation::Bilinear, 255);
    let mut expected = std::vec![0u32; 19];
    expected.extend_from_slice(&[127, 255, 255, 255]);
    assert_eq!(board.channel(10, 0..=22, 8), expected);
    assert_eq!(board.at(0, 0), 0xFF00_00FF);
    assert_eq!(board.at(1, 1), 0xFFBF_0040);
    assert_eq!(board.at(2, 2), 0xFFFF_0000);
    let mut board = Board::new();
    crate::image::draw(&mut board.target(), &src, [0.0, 0.0, 10.0, 10.0], &double, Interpolation::Nearest, 255);
    let mut expected = std::vec![0u32; 19];
    expected.extend_from_slice(&[255, 255, 255, 255]);
    assert_eq!(board.channel(10, 0..=22, 8), expected);
    assert_eq!(board.at(0, 0), 0xFF00_00FF);
    assert_eq!(board.at(1, 1), 0xFFFF_0000);
}

#[test]
fn transforms_scale_the_pen_and_move_the_shape() {
    let mut board = Board::new();
    let (points, types) = rect(0.0, 0.0, 10.0, 5.0);
    // Поворот на 90° и сдвиг на (30, 0): прямоугольник встаёт в x 25..30, y 0..10.
    let matrix = Matrix { m11: 0.0, m12: 1.0, m21: -1.0, m22: 0.0, dx: 30.0, dy: 0.0 };
    fill_path(&mut board.target(), &BLACK, &points, &types, &matrix, FillRule::EvenOdd, false).unwrap();
    assert_eq!(board.at(27, 5), 0xFF00_0000);
    assert_eq!(board.at(24, 5), WHITE);
    assert_eq!(board.at(27, 11), WHITE);
}

#[test]
fn regions_combine_and_mask() {
    let (a, at) = rect(0.0, 0.0, 20.0, 20.0);
    let (b, bt) = rect(10.0, 10.0, 20.0, 20.0);
    let mut points = a.clone();
    points.extend_from_slice(&b);
    let mut types = at.clone();
    types.extend_from_slice(&bt);
    // Обратная польская запись: A, B, объединение.
    let ops = [2, 0, 0, 4, 0, 2, 0, 4, 4, 0, 3, 2, 0, 0, 0];
    let elements = region::decode(&ops, &points, &types).unwrap();
    assert!(region::contains(&elements, 25.0, 25.0).unwrap());
    assert!(!region::contains(&elements, 25.0, 5.0).unwrap());
    let ops = [2, 0, 0, 4, 0, 2, 0, 4, 4, 0, 3, 3, 0, 0, 0];
    assert_eq!(Combine::from_code(3), Combine::Xor);
    let elements = region::decode(&ops, &points, &types).unwrap();
    assert!(!region::contains(&elements, 15.0, 15.0).unwrap());
    assert!(region::contains(&elements, 5.0, 5.0).unwrap());
    assert_eq!(region::extent(&elements).unwrap(), region::Extent::Box([0.0, 0.0, 30.0, 30.0]));
    // Пустая программа — бесконечная область, как `new Region()`.
    assert!(region::contains(&[], 1e6, -1e6).unwrap());

    let mask = region::mask(&elements, &Matrix::IDENTITY.translated(0.5, 0.5), Bounds::new(0, 0, 40, 40)).unwrap();
    let mut board = Board::new();
    let (all, all_types) = rect(0.0, 0.0, 40.0, 40.0);
    let mut target = board.target();
    target.mask = Some(&mask);
    fill_path(&mut target, &BLACK, &all, &all_types, &Matrix::IDENTITY, FillRule::EvenOdd, false).unwrap();
    assert_eq!(board.at(5, 5), 0xFF00_0000);
    assert_eq!(board.at(15, 15), WHITE);
    assert_eq!(board.at(25, 25), 0xFF00_0000);
    assert_eq!(board.at(35, 35), WHITE);
}

#[test]
fn bezier_ellipse_fills_its_inside() {
    // Эллипс так, как его пишет GDI+: 13 точек, начало справа, по часовой.
    let k = 0.552_284_8f32 * 15.0;
    let (cx, cy, r) = (20.0f32, 20.0f32, 15.0f32);
    let points = [
        cx + r, cy, cx + r, cy + k, cx + k, cy + r, cx, cy + r, cx - k, cy + r, cx - r, cy + k, cx - r, cy, cx - r, cy - k, cx - k,
        cy - r, cx, cy - r, cx + k, cy - r, cx + r, cy - k, cx + r, cy,
    ];
    let mut types = [3u8; 13];
    types[0] = TYPE_START;
    types[12] |= TYPE_CLOSE;
    let mut board = Board::new();
    fill_path(&mut board.target(), &BLACK, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, false).unwrap();
    // ell y20: столбцы 5..34.
    for x in 0..40 {
        assert_eq!((board.at(x, 20) >> 16) & 0xFF == 0, (5..=34).contains(&x), "ellipse row 20 at {x}");
    }
    assert_eq!(board.at(6, 6), WHITE);
    let mut board = Board::new();
    fill_path(&mut board.target(), &BLACK, &points, &types, &Matrix::IDENTITY, FillRule::EvenOdd, true).unwrap();
    let partial = (0..40).filter(|&x| {
        let v = (board.at(x, 20) >> 16) & 0xFF;
        v > 0 && v < 255
    });
    assert_eq!(partial.count(), 2);
}

#[test]
fn dashes_leave_gaps() {
    let mut board = Board::new();
    let (points, types) = line(0.0, 20.0, 40.0, 20.0);
    let dashed = Pen { dashes: &[3.0, 1.0], ..pen(2.0) };
    stroke_path(&mut board.target(), &BLACK, &points, &types, &Matrix::IDENTITY, &dashed, false).unwrap();
    // Штрих 6 точек, промежуток 2: 0..6 закрашено, 6..8 нет.
    let row: Vec<bool> = (0..16).map(|x| (board.at(x, 20) >> 16) & 0xFF == 0).collect();
    assert_eq!(row, [true, true, true, true, true, true, false, false, true, true, true, true, true, true, false, false]);
}
