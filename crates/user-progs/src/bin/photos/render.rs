//! Showing a picture fast at any zoom (24.11.2026).
//!
//! * Smaller than 100 %: a pyramid of halves (built once per picture, a third
//!   more memory) and, for the zoom on screen, a copy averaged by area from
//!   the nearest level at least as large — at most four source pixels per
//!   screen pixel, so a 24-megapixel photo fits the window in a blink and
//!   does not shimmer. The copy is kept until the zoom changes: panning and
//!   repainting are plain row copies.
//! * 100 % up to 400 %: bilinear, only for the visible part.
//! * Beyond: nearest pixel — the pixels are what one zooms in to see.
//!
//! All fixed point (16.16): the program has no FPU work to speak of.

use alloc::vec::Vec;

use mini_ui::{Rect, Surface};

use super::decode::Picture;

/// 100 % in 16.16.
pub const ONE: u64 = 1 << 16;

/// Half-size levels: `levels[0]` is the picture itself.
pub struct Pyramid {
    pub levels: Vec<Picture>,
}

impl Pyramid {
    pub fn new(base: Picture) -> Self {
        let mut levels = alloc::vec![base];
        loop {
            let last = levels.last().expect("level");
            if last.w < 640 && last.h < 640 || last.w < 2 || last.h < 2 {
                break;
            }
            match half(last) {
                Some(next) => levels.push(next),
                None => break,
            }
        }
        Self { levels }
    }

    #[must_use]
    pub fn base(&self) -> &Picture {
        &self.levels[0]
    }

    /// Turn by 90° (`clockwise`) — the base, and the levels again.
    pub fn rotate(&mut self, clockwise: bool) -> Result<(), &'static str> {
        let src = &self.levels[0];
        let (w, h) = (src.w, src.h);
        let mut out = Picture::alloc(h, w)?;
        for y in 0..h {
            let row = &src.px[y * w..(y + 1) * w];
            for (x, &p) in row.iter().enumerate() {
                // Clockwise: (x, y) -> (h-1-y, x); counter: (y, w-1-x).
                let (nx, ny) = if clockwise { (h - 1 - y, x) } else { (y, w - 1 - x) };
                out.px[ny * h + nx] = p;
            }
        }
        self.levels.clear();
        *self = Self::new(out);
        Ok(())
    }
}

/// 2×2 average.
fn half(src: &Picture) -> Option<Picture> {
    let (w, h) = (src.w / 2, src.h / 2);
    let mut out = Picture::alloc(w, h).ok()?;
    for y in 0..h {
        let a = &src.px[(y * 2) * src.w..];
        let b = &src.px[(y * 2 + 1) * src.w..];
        for x in 0..w {
            out.px[y * w + x] = avg4(a[x * 2], a[x * 2 + 1], b[x * 2], b[x * 2 + 1]);
        }
    }
    Some(out)
}

#[inline]
fn avg4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    // Per byte lane, the alpha lane stays opaque.
    let lo = |p: u32| p & 0x00FF_00FF;
    let mid = |p: u32| (p >> 8) & 0x0000_00FF;
    let rb = ((lo(a) + lo(b) + lo(c) + lo(d) + 0x0002_0002) >> 2) & 0x00FF_00FF;
    let g = ((mid(a) + mid(b) + mid(c) + mid(d) + 2) >> 2) & 0xFF;
    0xFF00_0000 | rb | (g << 8)
}

/// Area-average weights along one axis (as in `wallconv`).
struct Taps {
    first: Vec<u32>,
    count: Vec<u32>,
    offsets: Vec<u32>,
    weights: Vec<u32>,
}

fn taps(src: usize, dst: usize) -> Taps {
    let mut t = Taps { first: Vec::with_capacity(dst), count: Vec::with_capacity(dst), offsets: Vec::with_capacity(dst), weights: Vec::new() };
    for d in 0..dst {
        let (lo, hi) = (d * src, (d + 1) * src);
        let first = lo / dst;
        let last = (hi - 1) / dst;
        t.first.push(first as u32);
        t.count.push((last - first + 1) as u32);
        t.offsets.push(t.weights.len() as u32);
        let mut total = 0u32;
        for s in first..=last {
            let overlap = hi.min((s + 1) * dst) - lo.max(s * dst);
            let weight = ((overlap as u64 * 65_536) / src as u64) as u32;
            t.weights.push(weight);
            total += weight;
        }
        if let Some(fix) = t.weights.last_mut() {
            *fix += 65_536 - total.min(65_536);
        }
    }
    t
}

/// `src` averaged down to `dw`×`dh` (never up).
fn resample(src: &Picture, dw: usize, dh: usize) -> Option<Picture> {
    let across = taps(src.w, dw);
    let down = taps(src.h, dh);
    let mut wide: Vec<u32> = Vec::new();
    wide.try_reserve_exact(dw * src.h).ok()?;
    for y in 0..src.h {
        let line = &src.px[y * src.w..(y + 1) * src.w];
        for d in 0..dw {
            let (first, count, at) = (across.first[d] as usize, across.count[d] as usize, across.offsets[d] as usize);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for k in 0..count {
                let weight = across.weights[at + k];
                let p = line[first + k];
                r += (p & 0xFF) * weight;
                g += ((p >> 8) & 0xFF) * weight;
                b += ((p >> 16) & 0xFF) * weight;
            }
            wide.push(0xFF00_0000 | ((r + 32_768) >> 16).min(255) | (((g + 32_768) >> 16).min(255) << 8) | (((b + 32_768) >> 16).min(255) << 16));
        }
    }
    let mut out = Picture::alloc(dw, dh).ok()?;
    let mut sums: Vec<[u32; 3]> = alloc::vec![[0; 3]; dw];
    for d in 0..dh {
        let (first, count, at) = (down.first[d] as usize, down.count[d] as usize, down.offsets[d] as usize);
        for s in sums.iter_mut() {
            *s = [0; 3];
        }
        for k in 0..count {
            let weight = down.weights[at + k];
            let line = &wide[(first + k) * dw..(first + k + 1) * dw];
            for (s, &p) in sums.iter_mut().zip(line) {
                s[0] += (p & 0xFF) * weight;
                s[1] += ((p >> 8) & 0xFF) * weight;
                s[2] += ((p >> 16) & 0xFF) * weight;
            }
        }
        let row = &mut out.px[d * dw..(d + 1) * dw];
        for (slot, s) in row.iter_mut().zip(sums.iter()) {
            *slot = 0xFF00_0000 | ((s[0] + 32_768) >> 16).min(255) | (((s[1] + 32_768) >> 16).min(255) << 8) | (((s[2] + 32_768) >> 16).min(255) << 16);
        }
    }
    Some(out)
}

/// The picture at a zoom below 100 %, ready to copy.
pub struct Scaled {
    pub zoom: u64,
    pub picture: Picture,
}

/// Size of the picture `w`×`h` at `zoom` (at least one pixel).
#[must_use]
pub fn scaled_size(w: usize, h: usize, zoom: u64) -> (usize, usize) {
    (((w as u64 * zoom + ONE / 2) >> 16).max(1) as usize, ((h as u64 * zoom + ONE / 2) >> 16).max(1) as usize)
}

/// Make the copy for `zoom` (< 100 %).
pub fn make_scaled(pyramid: &Pyramid, zoom: u64) -> Option<Scaled> {
    let base = pyramid.base();
    let (dw, dh) = scaled_size(base.w, base.h, zoom);
    // The smallest level still at least as large as the target.
    let level = pyramid.levels.iter().rev().find(|level| level.w >= dw && level.h >= dh).unwrap_or(base);
    let picture = if level.w == dw && level.h == dh {
        Picture { w: dw, h: dh, px: level.px.clone() }
    } else {
        resample(level, dw, dh)?
    };
    Some(Scaled { zoom, picture })
}

/// Paint the picture into `view` of `s`: its top-left corner at `(ox, oy)`
/// (view coordinates, may be negative), `zoom` 16.16. `scaled` — the copy
/// for this zoom when it is below 100 %.
pub fn paint(s: &mut Surface, view: Rect, base: &Picture, scaled: Option<&Scaled>, zoom: u64, ox: i64, oy: i64) {
    let (dw, dh) = scaled_size(base.w, base.h, zoom);
    let placed = Rect::new(view.x + ox as i32, view.y + oy as i32, dw as u32, dh as u32);
    let visible = placed.intersect(&view).intersect(&s.bounds());
    if visible.is_empty() {
        return;
    }
    if let Some(scaled) = scaled.filter(|scaled| scaled.zoom == zoom && scaled.picture.w == dw && scaled.picture.h == dh) {
        let pic = &scaled.picture;
        let sx = (visible.x - placed.x) as usize;
        for row in 0..visible.h {
            let sy = (visible.y - placed.y) as usize + row as usize;
            let src = &pic.px[sy * pic.w + sx..sy * pic.w + sx + visible.w as usize];
            let dst = s.row_mut((visible.y + row as i32) as u32);
            let x0 = visible.x as usize;
            if let Some(dst) = dst.get_mut(x0..x0 + visible.w as usize) {
                dst.copy_from_slice(src);
            }
        }
        return;
    }
    // Source step per screen pixel, 16.16.
    let inv = (ONE << 16) / zoom.max(1);
    let (bw, bh) = (base.w as i64, base.h as i64);
    let x_first = (visible.x - placed.x) as i64;
    let y_first = (visible.y - placed.y) as i64;
    if zoom >= 4 * ONE || zoom == ONE {
        let xs: Vec<usize> = (0..visible.w as i64).map(|i| (((x_first + i) * inv as i64) >> 16).clamp(0, bw - 1) as usize).collect();
        for row in 0..visible.h as i64 {
            let sy = (((y_first + row) * inv as i64) >> 16).clamp(0, bh - 1) as usize;
            let src = &base.px[sy * base.w..(sy + 1) * base.w];
            let dst = s.row_mut((visible.y as i64 + row) as u32);
            let x0 = visible.x as usize;
            if let Some(dst) = dst.get_mut(x0..x0 + visible.w as usize) {
                for (slot, &sx) in dst.iter_mut().zip(&xs) {
                    *slot = src[sx];
                }
            }
        }
        return;
    }
    // Bilinear: sample at pixel centres.
    let at = |i: i64| -> (usize, usize, u32) {
        let pos = (i * inv as i64 + inv as i64 / 2 - (ONE as i64) / 2).max(0);
        let whole = pos >> 16;
        let frac = ((pos & 0xFFFF) >> 8) as u32;
        (whole.min(bw - 1) as usize, (whole + 1).min(bw - 1) as usize, frac)
    };
    let xs: Vec<(usize, usize, u32)> = (0..visible.w as i64).map(|i| at(x_first + i)).collect();
    for row in 0..visible.h as i64 {
        let pos = ((y_first + row) * inv as i64 + inv as i64 / 2 - (ONE as i64) / 2).max(0);
        let y0 = ((pos >> 16).min(bh - 1)) as usize;
        let y1 = ((pos >> 16) + 1).min(bh - 1) as usize;
        let fy = ((pos & 0xFFFF) >> 8) as u32;
        let a = &base.px[y0 * base.w..(y0 + 1) * base.w];
        let b = &base.px[y1 * base.w..(y1 + 1) * base.w];
        let dst = s.row_mut((visible.y as i64 + row) as u32);
        let x0 = visible.x as usize;
        if let Some(dst) = dst.get_mut(x0..x0 + visible.w as usize) {
            for (slot, &(sx0, sx1, fx)) in dst.iter_mut().zip(&xs) {
                let top = lerp(a[sx0], a[sx1], fx);
                let bottom = lerp(b[sx0], b[sx1], fx);
                *slot = lerp(top, bottom, fy);
            }
        }
    }
}

/// Mix two packed pixels, `t` of 256 towards `b`.
#[inline]
fn lerp(a: u32, b: u32, t: u32) -> u32 {
    if t == 0 || a == b {
        return a;
    }
    let u = 256 - t;
    let rb = (((a & 0x00FF_00FF) * u + (b & 0x00FF_00FF) * t) >> 8) & 0x00FF_00FF;
    let g = ((((a >> 8) & 0xFF) * u + ((b >> 8) & 0xFF) * t) >> 8) & 0xFF;
    0xFF00_0000 | rb | (g << 8)
}
