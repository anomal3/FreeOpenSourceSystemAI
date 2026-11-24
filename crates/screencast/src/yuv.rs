//! Captured pixels to I420 (planar 4:2:0) for the H.264 encoder (23.11).
//!
//! BT.709, limited range (Y 16–235, Cb/Cr 16–240) — what the encoder writes
//! into the VUI of the stream, so browsers and players show the colours the
//! desktop had. Fixed-point coefficients (×256), chroma — the average of a
//! 2×2 block. A size different from the capture is sampled bilinearly, like
//! the JPEG path (`jpeg::fetch_row`).
//!
//! No allocation on the hot path: [`Converter`] holds the sampling tables
//! (built once by the main thread), the caller gives two scratch rows.

use alloc::vec::Vec;

use crate::jpeg::Source;

/// Where each output column / row samples the source: index of the left
/// (top) neighbour and the weight of the right (bottom) one, of 256.
#[derive(Clone, Debug)]
pub struct Converter {
    pub out_w: u32,
    pub out_h: u32,
    xmap: Vec<(u32, u32)>,
    ymap: Vec<(u32, u32)>,
    scaled: bool,
}

fn map(src: u32, out: u32) -> Vec<(u32, u32)> {
    (0..out)
        .map(|o| {
            // Centre of the output pixel in source coordinates, ×256.
            let pos = ((u64::from(o) * 2 + 1) * u64::from(src) * 256 / (u64::from(out) * 2)).saturating_sub(128);
            let index = ((pos >> 8) as u32).min(src.saturating_sub(1));
            let weight = if index + 1 >= src { 0 } else { (pos & 255) as u32 };
            (index, weight)
        })
        .collect()
}

#[inline]
fn rgb(p: u32, bgr: bool) -> (i32, i32, i32) {
    let (a, g, c) = ((p & 255) as i32, ((p >> 8) & 255) as i32, ((p >> 16) & 255) as i32);
    if bgr { (c, g, a) } else { (a, g, c) }
}

#[inline]
fn luma(r: i32, g: i32, b: i32) -> u8 {
    ((47 * r + 157 * g + 16 * b + 128) >> 8) as u8 + 16
}

impl Converter {
    /// Sizes must be even (4:2:0 has one chroma sample per 2×2 block).
    #[must_use]
    pub fn new(src_w: u32, src_h: u32, out_w: u32, out_h: u32) -> Self {
        let scaled = src_w != out_w || src_h != out_h;
        Self { out_w, out_h, xmap: map(src_w, out_w), ymap: map(src_h, out_h), scaled }
    }

    /// Bytes of one I420 picture: Y, then U, then V.
    #[must_use]
    pub const fn frame_bytes(&self) -> usize {
        let luma = self.out_w as usize * self.out_h as usize;
        luma + luma / 2
    }

    /// One output row as packed `0x00BBGGRR`-like pixels (source order kept,
    /// `bgr` is applied later).
    fn row(&self, src: &Source, oy: u32, out: &mut [u32]) {
        if !self.scaled {
            let start = (oy * src.stride) as usize;
            out[..self.out_w as usize].copy_from_slice(&src.pixels[start..start + self.out_w as usize]);
            return;
        }
        let (sy, wy) = self.ymap[oy as usize];
        let sy1 = (sy + 1).min(src.height - 1);
        let r0 = &src.pixels[(sy * src.stride) as usize..];
        let r1 = &src.pixels[(sy1 * src.stride) as usize..];
        for (o, &(sx, wx)) in out.iter_mut().zip(&self.xmap) {
            let sx1 = (sx + if wx > 0 { 1 } else { 0 }) as usize;
            let sx = sx as usize;
            let (a, b, c, d) = (r0[sx], r0[sx1], r1[sx], r1[sx1]);
            let mut value = 0u32;
            for shift in [0u32, 8, 16] {
                let ch = |p: u32| (p >> shift) & 255;
                let top = ch(a) * (256 - wx) + ch(b) * wx;
                let bottom = ch(c) * (256 - wx) + ch(d) * wx;
                value |= (((top * (256 - wy) + bottom * wy) + 32768) >> 16) << shift;
            }
            *o = value;
        }
    }

    /// Convert output rows `2*pair_from .. 2*pair_to` (pairs of rows: one
    /// chroma row each). `y` starts at luma row `2*pair_from`, `u`/`v` at
    /// chroma row `pair_from`; `scratch` holds at least `2 * out_w` points.
    pub fn convert(&self, src: &Source, pair_from: u32, pair_to: u32, y: &mut [u8], u: &mut [u8], v: &mut [u8], scratch: &mut [u32]) {
        let w = self.out_w as usize;
        let cw = w / 2;
        let (top, bottom) = scratch[..2 * w].split_at_mut(w);
        for pair in pair_from..pair_to.min(self.out_h / 2) {
            self.row(src, pair * 2, top);
            self.row(src, pair * 2 + 1, bottom);
            let at = (pair - pair_from) as usize;
            let (y0, rest) = y[at * 2 * w..at * 2 * w + 2 * w].split_at_mut(w);
            let urow = &mut u[at * cw..at * cw + cw];
            let vrow = &mut v[at * cw..at * cw + cw];
            for x in 0..cw {
                let (r0, g0, b0) = rgb(top[2 * x], src.bgr);
                let (r1, g1, b1) = rgb(top[2 * x + 1], src.bgr);
                let (r2, g2, b2) = rgb(bottom[2 * x], src.bgr);
                let (r3, g3, b3) = rgb(bottom[2 * x + 1], src.bgr);
                y0[2 * x] = luma(r0, g0, b0);
                y0[2 * x + 1] = luma(r1, g1, b1);
                rest[2 * x] = luma(r2, g2, b2);
                rest[2 * x + 1] = luma(r3, g3, b3);
                let (r, g, b) = (r0 + r1 + r2 + r3, g0 + g1 + g2 + g3, b0 + b1 + b2 + b3);
                // Sums of four: ×256 coefficients, /4 folded into >> 10.
                urow[x] = ((-26 * r - 86 * g + 112 * b + 512) >> 10).wrapping_add(128).clamp(16, 240) as u8;
                vrow[x] = ((112 * r - 102 * g - 10 * b + 512) >> 10).wrapping_add(128).clamp(16, 240) as u8;
            }
        }
    }

    /// The whole picture into `out` ([`Self::frame_bytes`]).
    pub fn convert_all(&self, src: &Source, out: &mut [u8], scratch: &mut [u32]) {
        let luma = self.out_w as usize * self.out_h as usize;
        let (y, chroma) = out[..luma + luma / 2].split_at_mut(luma);
        let (u, v) = chroma.split_at_mut(luma / 4);
        self.convert(src, 0, self.out_h / 2, y, u, v, scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn white_black_and_primaries() {
        // 2×2 of one colour each, 4×2 picture: white | red.
        let px = |r: u32, g: u32, b: u32| r | (g << 8) | (b << 16);
        let pixels = vec![px(255, 255, 255), px(255, 255, 255), px(255, 0, 0), px(255, 0, 0), px(255, 255, 255), px(255, 255, 255), px(255, 0, 0), px(255, 0, 0)];
        let src = Source { pixels: &pixels, width: 4, height: 2, stride: 4, bgr: false };
        let conv = Converter::new(4, 2, 4, 2);
        let mut out = vec![0u8; conv.frame_bytes()];
        let mut scratch = vec![0u32; 8];
        conv.convert_all(&src, &mut out, &mut scratch);
        assert_eq!(&out[..2], &[235, 235]);
        // BT.709 red: Y 63, Cb 102, Cr 240.
        assert!((i32::from(out[2]) - 63).abs() <= 1, "Y of red {}", out[2]);
        let (u, v) = (out[8], out[10]);
        assert_eq!((out[8], out[10]), (128, 128), "white chroma");
        let (u_red, v_red) = (out[9], out[11]);
        assert!((i32::from(u_red) - 102).abs() <= 1 && (i32::from(v_red) - 240).abs() <= 1, "red chroma {u_red} {v_red} ({u} {v})");
    }

    #[test]
    fn downscale_keeps_flat_colour() {
        let pixels = vec![0x0040_8020u32; 64 * 36];
        let src = Source { pixels: &pixels, width: 64, height: 36, stride: 64, bgr: false };
        let conv = Converter::new(64, 36, 32, 18);
        let mut out = vec![0u8; conv.frame_bytes()];
        let mut scratch = vec![0u32; 64];
        conv.convert_all(&src, &mut out, &mut scratch);
        let first = out[0];
        assert!(out[..32 * 18].iter().all(|&y| y == first));
    }
}
