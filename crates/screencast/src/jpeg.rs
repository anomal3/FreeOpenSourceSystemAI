//! Baseline JPEG encoder: YCbCr 4:2:0, standard Huffman tables (Annex K),
//! float AAN forward DCT, optional bilinear downscale on the fly.
//!
//! The source is a frame of packed 32-bit points as the screen keeps them
//! (red in the low byte, or blue — [`Source::bgr`]). The output is a complete
//! JFIF image (SOI .. EOI) with its own tables, as AVI MJPEG decoders expect.

use alloc::vec::Vec;

/// Natural (row-major) index -> zigzag position.
const ZIGZAG: [u8; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11, 18, 24, 31, 40, 44, 53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38,
    46, 51, 55, 60, 21, 34, 37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
];

/// Annex K luminance quantization, natural order.
const YQT: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24,
    35, 55, 64, 81, 104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

/// Annex K chrominance quantization, natural order.
const CQT: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];

const DC_Y_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_C_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_Y_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_Y_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1,
    0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39,
    0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75,
    0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8,
    0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];
const AC_C_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_C_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09,
    0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38,
    0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74,
    0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6,
    0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// AAN scale factors (times 2*sqrt(2)).
const AASF: [f32; 8] = [
    1.0 * 2.828_427_1,
    1.387_039_8 * 2.828_427_1,
    1.306_563 * 2.828_427_1,
    1.175_875_6 * 2.828_427_1,
    1.0 * 2.828_427_1,
    0.785_694_96 * 2.828_427_1,
    0.541_196_1 * 2.828_427_1,
    0.275_899_38 * 2.828_427_1,
];

/// Huffman code: bits and length.
#[derive(Clone, Copy, Default)]
struct Code {
    bits: u16,
    len: u8,
}

/// The frame to encode.
#[derive(Clone, Copy)]
pub struct Source<'a> {
    pub pixels: &'a [u32],
    pub width: u32,
    pub height: u32,
    /// Points per row (>= width).
    pub stride: u32,
    /// Blue in the low byte (`PIXEL_BGR`); otherwise red.
    pub bgr: bool,
}

/// Per-thread working memory for [`Encoder::encode`] at one output width.
pub struct Scratch {
    width: u32,
    /// Y of one MCU row: 16 rows of `padded` values.
    y: Vec<f32>,
    /// Cb and Cr of one MCU row: 8 rows of `padded / 2`.
    cb: Vec<f32>,
    cr: Vec<f32>,
    /// One output row of RGB (`r | g << 8 | b << 16`).
    row: Vec<u32>,
    /// Source column and weight (0..256) of the right neighbour, per output
    /// column (downscale only).
    xmap: Vec<(u32, u32)>,
    /// For which source width the map is.
    xmap_src: u32,
}

impl Scratch {
    /// Scratch for frames `width` points wide (after scaling).
    #[must_use]
    pub fn new(width: u32) -> Self {
        let padded = padded(width) as usize;
        Self {
            width,
            y: alloc::vec![0.0; padded * 16],
            cb: alloc::vec![0.0; padded / 2 * 8],
            cr: alloc::vec![0.0; padded / 2 * 8],
            row: alloc::vec![0; padded],
            xmap: alloc::vec![(0, 0); width as usize],
            xmap_src: 0,
        }
    }
}

const fn padded(width: u32) -> u32 {
    width.div_ceil(16) * 16
}

/// Room an output buffer must have for one frame (generous upper bound).
#[must_use]
pub const fn max_size(width: u32, height: u32) -> usize {
    padded(width) as usize * (height as usize + 16) * 3 + 2048
}

/// Encoder with its tables for one quality.
pub struct Encoder {
    /// Quantization tables in zigzag order (for DQT).
    qy: [u8; 64],
    qc: [u8; 64],
    /// Reciprocal scaled quantizers, natural order.
    fy: [f32; 64],
    fc: [f32; 64],
    dc_y: [Code; 12],
    dc_c: [Code; 12],
    ac_y: [Code; 256],
    ac_c: [Code; 256],
}

fn huffman(bits: &[u8; 16], vals: &[u8]) -> [Code; 256] {
    let mut table = [Code::default(); 256];
    let mut code = 0u16;
    let mut at = 0;
    for (index, count) in bits.iter().enumerate() {
        for _ in 0..*count {
            table[vals[at] as usize] = Code { bits: code, len: index as u8 + 1 };
            code += 1;
            at += 1;
        }
        code <<= 1;
    }
    table
}

impl Encoder {
    /// Encoder at `quality` 1..100 (the libjpeg scale: 75 is typical, 90 is
    /// near-lossless for screen text).
    #[must_use]
    pub fn new(quality: u32) -> Self {
        let quality = quality.clamp(1, 100);
        let scale = if quality < 50 { 5000 / quality } else { 200 - quality * 2 };
        let mut qy = [0u8; 64];
        let mut qc = [0u8; 64];
        for index in 0..64 {
            let q = |base: u8| ((u32::from(base) * scale + 50) / 100).clamp(1, 255) as u8;
            qy[ZIGZAG[index] as usize] = q(YQT[index]);
            qc[ZIGZAG[index] as usize] = q(CQT[index]);
        }
        let mut fy = [0f32; 64];
        let mut fc = [0f32; 64];
        for row in 0..8 {
            for col in 0..8 {
                let k = row * 8 + col;
                let z = ZIGZAG[k] as usize;
                fy[k] = 1.0 / (f32::from(qy[z]) * AASF[row] * AASF[col]);
                fc[k] = 1.0 / (f32::from(qc[z]) * AASF[row] * AASF[col]);
            }
        }
        let dc = |bits: &[u8; 16]| {
            let all = huffman(bits, &DC_VALS);
            let mut out = [Code::default(); 12];
            out.copy_from_slice(&all[..12]);
            out
        };
        Self { qy, qc, fy, fc, dc_y: dc(&DC_Y_BITS), dc_c: dc(&DC_C_BITS), ac_y: huffman(&AC_Y_BITS, &AC_Y_VALS), ac_c: huffman(&AC_C_BITS, &AC_C_VALS) }
    }

    /// Encode `src` scaled to `out_w`×`out_h` (the scratch must be for
    /// `out_w`) into `out`. Answer: bytes written, `None` — `out` too small
    /// or bad sizes.
    pub fn encode(&self, src: &Source, out_w: u32, out_h: u32, scratch: &mut Scratch, out: &mut [u8]) -> Option<usize> {
        if out_w == 0 || out_h == 0 || out_w > 65_535 || out_h > 65_535 || scratch.width != out_w || src.width == 0 || src.height == 0 {
            return None;
        }
        if (src.stride as usize) < src.width as usize || src.pixels.len() < src.stride as usize * (src.height as usize - 1) + src.width as usize {
            return None;
        }
        let mut w = Writer { out, pos: 0, acc: 0, nbits: 0 };
        self.headers(&mut w, out_w, out_h)?;
        let pw = padded(out_w) as usize;
        let cw = pw / 2;
        let scaled = src.width != out_w || src.height != out_h;
        if scaled && scratch.xmap_src != src.width {
            for (ox, slot) in scratch.xmap.iter_mut().enumerate() {
                let fx = ((ox as u64 * 2 + 1) * u64::from(src.width) * 256 / (u64::from(out_w) * 2)).saturating_sub(128);
                let x0 = ((fx >> 8) as u32).min(src.width - 1);
                *slot = (x0, if x0 + 1 < src.width { (fx & 255) as u32 } else { 0 });
            }
            scratch.xmap_src = src.width;
        }
        let (mut dc_y, mut dc_cb, mut dc_cr) = (0i32, 0i32, 0i32);
        let mut block = [0f32; 64];
        for mcu_y in 0..out_h.div_ceil(16) {
            // Planes of this MCU row.
            for r in 0..16u32 {
                let oy = (mcu_y * 16 + r).min(out_h - 1);
                fetch_row(src, oy, out_w, out_h, scaled, &scratch.xmap, &mut scratch.row[..out_w as usize]);
                let last = scratch.row[out_w as usize - 1];
                for x in out_w as usize..pw {
                    scratch.row[x] = last;
                }
                let yrow = &mut scratch.y[r as usize * pw..(r as usize + 1) * pw];
                let crow = (r / 2) as usize * cw;
                let first = r % 2 == 0;
                for x in 0..pw {
                    let p = scratch.row[x];
                    let (rr, gg, bb) = ((p & 255) as f32, ((p >> 8) & 255) as f32, ((p >> 16) & 255) as f32);
                    yrow[x] = 0.299 * rr + 0.587 * gg + 0.114 * bb - 128.0;
                    let cb = -0.168_736 * rr - 0.331_264 * gg + 0.5 * bb;
                    let cr = 0.5 * rr - 0.418_688 * gg - 0.081_312 * bb;
                    let c = crow + x / 2;
                    if first && x % 2 == 0 {
                        scratch.cb[c] = cb;
                        scratch.cr[c] = cr;
                    } else {
                        scratch.cb[c] += cb;
                        scratch.cr[c] += cr;
                    }
                }
            }
            for value in scratch.cb.iter_mut().chain(scratch.cr.iter_mut()) {
                *value *= 0.25;
            }
            // Blocks: four Y, Cb, Cr per MCU.
            for mcu_x in 0..pw / 16 {
                if w.out.len() < w.pos + 4096 {
                    return None;
                }
                for (by, bx) in [(0, 0), (0, 8), (8, 0), (8, 8)] {
                    for r in 0..8 {
                        let from = (by + r) * pw + mcu_x * 16 + bx;
                        block[r * 8..r * 8 + 8].copy_from_slice(&scratch.y[from..from + 8]);
                    }
                    dc_y = self.block(&mut w, &mut block, &self.fy, dc_y, &self.dc_y, &self.ac_y);
                }
                for r in 0..8 {
                    let from = r * cw + mcu_x * 8;
                    block[r * 8..r * 8 + 8].copy_from_slice(&scratch.cb[from..from + 8]);
                }
                dc_cb = self.block(&mut w, &mut block, &self.fc, dc_cb, &self.dc_c, &self.ac_c);
                for r in 0..8 {
                    let from = r * cw + mcu_x * 8;
                    block[r * 8..r * 8 + 8].copy_from_slice(&scratch.cr[from..from + 8]);
                }
                dc_cr = self.block(&mut w, &mut block, &self.fc, dc_cr, &self.dc_c, &self.ac_c);
            }
        }
        // Fill the last byte with ones, then EOI.
        w.put(0x7F, 7);
        w.raw(&[0xFF, 0xD9])?;
        Some(w.pos)
    }

    fn headers(&self, w: &mut Writer, width: u32, height: u32) -> Option<()> {
        w.raw(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0])?;
        w.raw(&[0xFF, 0xDB, 0, 132, 0])?;
        w.raw(&self.qy)?;
        w.raw(&[1])?;
        w.raw(&self.qc)?;
        let (h, v) = (height as u16, width as u16);
        w.raw(&[0xFF, 0xC0, 0, 17, 8, (h >> 8) as u8, h as u8, (v >> 8) as u8, v as u8, 3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1])?;
        w.raw(&[0xFF, 0xC4, 0x01, 0xA2, 0x00])?;
        w.raw(&DC_Y_BITS)?;
        w.raw(&DC_VALS)?;
        w.raw(&[0x10])?;
        w.raw(&AC_Y_BITS)?;
        w.raw(&AC_Y_VALS)?;
        w.raw(&[0x01])?;
        w.raw(&DC_C_BITS)?;
        w.raw(&DC_VALS)?;
        w.raw(&[0x11])?;
        w.raw(&AC_C_BITS)?;
        w.raw(&AC_C_VALS)?;
        w.raw(&[0xFF, 0xDA, 0, 12, 3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 0x3F, 0])
    }

    /// DCT, quantization and Huffman coding of one 8×8 block. Returns its DC.
    fn block(&self, w: &mut Writer, data: &mut [f32; 64], ftbl: &[f32; 64], dc_prev: i32, dc: &[Code; 12], ac: &[Code; 256]) -> i32 {
        for row in 0..8 {
            dct8(data, row * 8, 1);
        }
        for col in 0..8 {
            dct8(data, col, 8);
        }
        let mut du = [0i32; 64];
        for (k, value) in data.iter().enumerate() {
            let v = value * ftbl[k];
            du[ZIGZAG[k] as usize] = if v < 0.0 { (v - 0.5) as i32 } else { (v + 0.5) as i32 };
        }
        let diff = du[0] - dc_prev;
        if diff == 0 {
            w.code(dc[0]);
        } else {
            let (len, bits) = magnitude(diff);
            w.code(dc[len as usize]);
            w.put(bits, len);
        }
        let mut end = 63;
        while end > 0 && du[end] == 0 {
            end -= 1;
        }
        if end == 0 {
            w.code(ac[0x00]);
            return du[0];
        }
        let mut i = 1;
        while i <= end {
            let start = i;
            while du[i] == 0 && i <= end {
                i += 1;
            }
            let mut zeros = i - start;
            while zeros >= 16 {
                w.code(ac[0xF0]);
                zeros -= 16;
            }
            let (len, bits) = magnitude(du[i]);
            w.code(ac[(zeros << 4) + len as usize]);
            w.put(bits, len);
            i += 1;
        }
        if end != 63 {
            w.code(ac[0x00]);
        }
        du[0]
    }
}

/// One output row of RGB from the source, scaled bilinearly if needed.
fn fetch_row(src: &Source, oy: u32, out_w: u32, out_h: u32, scaled: bool, xmap: &[(u32, u32)], row: &mut [u32]) {
    let rgb = |p: u32| if src.bgr { ((p >> 16) & 0xFF) | (p & 0xFF00) | ((p & 0xFF) << 16) } else { p & 0x00FF_FFFF };
    if !scaled {
        let from = oy as usize * src.stride as usize;
        for (out, p) in row.iter_mut().zip(&src.pixels[from..from + out_w as usize]) {
            *out = rgb(*p);
        }
        return;
    }
    let fy = ((u64::from(oy) * 2 + 1) * u64::from(src.height) * 256 / (u64::from(out_h) * 2)).saturating_sub(128);
    let y0 = ((fy >> 8) as u32).min(src.height - 1);
    let wy = if y0 + 1 < src.height { (fy & 255) as u32 } else { 0 };
    let y1 = (y0 + 1).min(src.height - 1);
    let top = &src.pixels[y0 as usize * src.stride as usize..];
    let bottom = &src.pixels[y1 as usize * src.stride as usize..];
    for (out, &(x0, wx)) in row.iter_mut().zip(xmap) {
        let x1 = (x0 + u32::from(wx > 0)) as usize;
        let (a, b, c, d) = (rgb(top[x0 as usize]), rgb(top[x1]), rgb(bottom[x0 as usize]), rgb(bottom[x1]));
        let mut value = 0;
        for shift in [0, 8, 16] {
            let ch = |p: u32| (p >> shift) & 0xFF;
            let t = ch(a) * (256 - wx) + ch(b) * wx;
            let u = ch(c) * (256 - wx) + ch(d) * wx;
            value |= (((t * (256 - wy) + u * wy) + 32768) >> 16) << shift;
        }
        *out = value;
    }
}

/// Bits of a coefficient: length and the value bits (JPEG one's complement
/// for negatives).
fn magnitude(value: i32) -> (u8, u16) {
    let abs = value.unsigned_abs();
    let len = (32 - abs.leading_zeros()) as u8;
    let bits = if value < 0 { (value - 1) as u32 & ((1 << len) - 1) } else { value as u32 };
    (len, bits as u16)
}

/// Float AAN forward DCT of 8 values at `at`, `at + step`, ...
#[inline(always)]
fn dct8(d: &mut [f32; 64], at: usize, step: usize) {
    let i = |k: usize| at + k * step;
    let (d0, d1, d2, d3, d4, d5, d6, d7) = (d[i(0)], d[i(1)], d[i(2)], d[i(3)], d[i(4)], d[i(5)], d[i(6)], d[i(7)]);
    let tmp0 = d0 + d7;
    let tmp7 = d0 - d7;
    let tmp1 = d1 + d6;
    let tmp6 = d1 - d6;
    let tmp2 = d2 + d5;
    let tmp5 = d2 - d5;
    let tmp3 = d3 + d4;
    let tmp4 = d3 - d4;
    // Even part.
    let tmp10 = tmp0 + tmp3;
    let tmp13 = tmp0 - tmp3;
    let tmp11 = tmp1 + tmp2;
    let tmp12 = tmp1 - tmp2;
    d[i(0)] = tmp10 + tmp11;
    d[i(4)] = tmp10 - tmp11;
    let z1 = (tmp12 + tmp13) * 0.707_106_77;
    d[i(2)] = tmp13 + z1;
    d[i(6)] = tmp13 - z1;
    // Odd part.
    let tmp10 = tmp4 + tmp5;
    let tmp11 = tmp5 + tmp6;
    let tmp12 = tmp6 + tmp7;
    let z5 = (tmp10 - tmp12) * 0.382_683_43;
    let z2 = tmp10 * 0.541_196_1 + z5;
    let z4 = tmp12 * 1.306_563 + z5;
    let z3 = tmp11 * 0.707_106_77;
    let z11 = tmp7 + z3;
    let z13 = tmp7 - z3;
    d[i(5)] = z13 + z2;
    d[i(3)] = z13 - z2;
    d[i(1)] = z11 + z4;
    d[i(7)] = z11 - z4;
}

/// Bit writer with 0xFF byte stuffing.
struct Writer<'a> {
    out: &'a mut [u8],
    pos: usize,
    acc: u32,
    nbits: u32,
}

impl Writer<'_> {
    fn raw(&mut self, bytes: &[u8]) -> Option<()> {
        let end = self.pos.checked_add(bytes.len())?;
        self.out.get_mut(self.pos..end)?.copy_from_slice(bytes);
        self.pos = end;
        Some(())
    }

    #[inline(always)]
    fn code(&mut self, code: Code) {
        self.put(code.bits, code.len);
    }

    /// Append `len` (<= 16) bits. The caller keeps 4 KiB of room per block.
    #[inline(always)]
    fn put(&mut self, bits: u16, len: u8) {
        self.acc = (self.acc << len) | u32::from(bits);
        self.nbits += u32::from(len);
        while self.nbits >= 8 {
            let byte = (self.acc >> (self.nbits - 8)) as u8;
            if self.pos + 1 < self.out.len() {
                self.out[self.pos] = byte;
                self.pos += 1;
                if byte == 0xFF {
                    self.out[self.pos] = 0;
                    self.pos += 1;
                }
            }
            self.nbits -= 8;
        }
        self.acc &= (1 << self.nbits) - 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_complete() {
        assert_eq!(AC_Y_BITS.iter().map(|&b| usize::from(b)).sum::<usize>(), AC_Y_VALS.len());
        assert_eq!(AC_C_BITS.iter().map(|&b| usize::from(b)).sum::<usize>(), AC_C_VALS.len());
        assert_eq!(DC_Y_BITS.iter().map(|&b| usize::from(b)).sum::<usize>(), 12);
        assert_eq!(DC_C_BITS.iter().map(|&b| usize::from(b)).sum::<usize>(), 12);
    }

    /// Speed of one thread on a 2560x1440 desktop-like frame (gradient,
    /// "text" noise, flat panels): `cargo test -p screencast --release --
    /// --ignored --nocapture bench`.
    #[test]
    #[ignore = "benchmark"]
    fn bench_1440p() {
        let (w, h) = (2560u32, 1440u32);
        let mut pixels = std::vec![0u32; (w * h) as usize];
        let mut seed = 12345u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                let text = (y / 18) % 3 == 0 && x % 700 < 500 && seed >> 28 < 6;
                let p = if text { 0x0020_2020 } else if x < 400 { 0x0030_2a28 } else { (x * 255 / w) | ((y * 255 / h) << 8) | (0x60 << 16) };
                pixels[(y * w + x) as usize] = 0xFF00_0000 | p;
            }
        }
        let src = Source { pixels: &pixels, width: w, height: h, stride: w, bgr: true };
        let encoder = Encoder::new(80);
        for (ow, oh) in [(2560u32, 1440u32), (1920, 1080), (1280, 720)] {
            let mut scratch = Scratch::new(ow);
            let mut out = std::vec![0u8; max_size(ow, oh)];
            let runs = 10;
            let started = std::time::Instant::now();
            let mut size = 0;
            for _ in 0..runs {
                size = encoder.encode(&src, ow, oh, &mut scratch, &mut out).expect("encode");
            }
            let ms = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
            std::println!("bench: 2560x1440 -> {ow}x{oh}: {ms:.1} ms/frame on one thread, {} KB", size / 1024);
        }
    }

    #[test]
    fn magnitudes() {
        assert_eq!(magnitude(1), (1, 1));
        assert_eq!(magnitude(-1), (1, 0));
        assert_eq!(magnitude(5), (3, 5));
        assert_eq!(magnitude(-5), (3, 2));
    }
}
