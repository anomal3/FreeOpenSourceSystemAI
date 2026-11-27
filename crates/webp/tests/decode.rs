//! Host tests for the no_std WebP decoder. Sample files were produced with
//! libwebp (via Pillow); expected pixels are libwebp's own decode of them.

const LOSSY: &[u8] = include_bytes!("data/lossy.webp");
const LOSSLESS: &[u8] = include_bytes!("data/lossless.webp");
const ALPHA: &[u8] = include_bytes!("data/alpha.webp");
const ANIM: &[u8] = include_bytes!("data/anim.webp");

fn px(img: &webp::Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    img.rgba[i..i + 4].try_into().unwrap()
}

fn close(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b.iter()).all(|(x, y)| x.abs_diff(*y) <= tol)
}

#[test]
fn lossy_vp8() {
    assert_eq!(webp::dimensions(LOSSY).unwrap(), (32, 24));
    let img = webp::decode(LOSSY).unwrap();
    assert_eq!((img.width, img.height, img.frames), (32, 24, 1));
    assert_eq!(img.rgba.len(), 32 * 24 * 4);
    for (x, y, want) in [
        (0, 0, [221, 21, 20, 255]),
        (8, 8, [221, 21, 20, 255]),
        (24, 8, [21, 20, 221, 255]),
        (31, 23, [0, 200, 1, 255]),
        (10, 20, [0, 201, 0, 255]),
    ] {
        let got = px(&img, x, y);
        assert!(close(got, want, 2), "({x},{y}) got {got:?} want {want:?}");
    }
}

#[test]
fn lossless_vp8l_exact() {
    assert_eq!(webp::dimensions(LOSSLESS).unwrap(), (7, 5));
    let img = webp::decode(LOSSLESS).unwrap();
    assert_eq!((img.width, img.height, img.frames), (7, 5, 1));
    for y in 0..5u32 {
        for x in 0..7u32 {
            let want = [(x * 36) as u8, (y * 60) as u8, ((x * y * 13) % 256) as u8, 255];
            assert_eq!(px(&img, x, y), want, "({x},{y})");
        }
    }
}

#[test]
fn lossy_with_alpha_vp8x() {
    assert_eq!(webp::dimensions(ALPHA).unwrap(), (16, 16));
    let img = webp::decode(ALPHA).unwrap();
    assert_eq!((img.width, img.height, img.frames), (16, 16, 1));
    for y in 0..16 {
        for x in 0..16 {
            let p = px(&img, x, y);
            assert_eq!(p[3], if x >= 8 { 255 } else { 64 }, "alpha ({x},{y})");
            assert!(close(p, [251, 128, 0, p[3]], 2), "({x},{y}) {p:?}");
        }
    }
}

#[test]
fn animation() {
    assert_eq!(webp::dimensions(ANIM).unwrap(), (8, 8));
    let img = webp::decode(ANIM).unwrap();
    assert_eq!((img.width, img.height, img.frames), (8, 8, 2));
    assert!(img.rgba.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));

    let (w, h, frames) = webp::decode_frames(ANIM, 1 << 20).unwrap();
    assert_eq!((w, h, frames.len()), (8, 8, 2));
    assert_eq!(frames[0].1, 100);
    assert_eq!(frames[1].1, 200);
    assert!(frames[0].0.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
    // Frame 2 is alpha-blended over frame 1; upstream's integer blend gives
    // 254 instead of 255 for an opaque source, so allow +-1.
    assert!(frames[1]
        .0
        .chunks_exact(4)
        .all(|p| close(p.try_into().unwrap(), [0, 0, 255, 255], 1)));

    // Pixel budget: 2 frames * 64 px = 128.
    assert!(matches!(
        webp::decode_frames(ANIM, 127),
        Err(webp::DecodingError::MemoryLimitExceeded)
    ));
    let (_, _, still) = webp::decode_frames(LOSSLESS, 1000).unwrap();
    assert_eq!((still.len(), still[0].1), (1, 0));
}

#[test]
fn size_guard() {
    // VP8X header announcing a 20000 x 20000 canvas (> 16384 * 16384 pixels):
    // flags=0, canvas w-1/h-1 as 24-bit LE, then an empty VP8 chunk.
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&30u32.to_le_bytes());
    f.extend_from_slice(b"WEBPVP8X");
    f.extend_from_slice(&10u32.to_le_bytes());
    f.extend_from_slice(&[0, 0, 0, 0]);
    let d = 19999u32.to_le_bytes();
    f.extend_from_slice(&d[..3]);
    f.extend_from_slice(&d[..3]);
    f.extend_from_slice(b"VP8 ");
    f.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(webp::dimensions(&f).unwrap(), (20000, 20000));
    assert!(matches!(webp::decode(&f), Err(webp::DecodingError::ImageTooLarge)));
}

#[test]
fn corrupt_inputs_error_not_panic() {
    assert!(webp::decode(&[]).is_err());
    assert!(webp::dimensions(b"RIFF\0\0\0\0WEBP").is_err());
    for src in [LOSSY, LOSSLESS, ALPHA, ANIM] {
        // Every truncation must fail or succeed cleanly.
        for n in 0..src.len() {
            let _ = webp::decode(&src[..n]);
            let _ = webp::decode_frames(&src[..n], 1 << 20);
        }
        // Single-byte corruptions.
        for i in 0..src.len() {
            let mut v = src.to_vec();
            v[i] ^= 0x5a;
            let _ = webp::decode(&v);
            let _ = webp::decode_frames(&v, 1 << 20);
        }
    }
    let e = webp::decode(b"RIFX\0\0\0\0WEBP").unwrap_err();
    assert!(!format!("{e}").is_empty());
}
