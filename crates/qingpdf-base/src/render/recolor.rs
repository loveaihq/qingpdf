//! Colours for the screen only (2026-10-20): the pixels a viewer shows of a page are changed, never the page. A viewer draws the page, then
//! calls [`recolor`] on the bitmap before keeping it, so what is printed, exported, copied or saved is as the file has it.
//!
//! * Night keeps the hue and turns the lightness over: the luma `Y` (BT.601 weights) of a pixel becomes `INK - (INK - PAPER) * Y / 255` and
//!   the pixel moves by the same amount in all three channels, so its chroma (the differences between the channels) stays. White paper is
//!   a near black (`PAPER`), black ink a light grey (`INK`), neither pure, which is easier on the eyes; a yellow highlight is a dark yellow,
//!   blue text light blue. A channel that would leave 0..=255 is cut.
//! * Sepia multiplies each channel by a warm paper colour: white paper becomes that colour, black text stays black, greys go between.

/// What a viewer does with the colours of a page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Tint {
    /// As the file draws it.
    #[default]
    Original = 0,
    Night = 1,
    Sepia = 2,
}

/// What white paper becomes in Night, and black ink.
const NIGHT_PAPER: i32 = 0x1C;
const NIGHT_INK: i32 = 0xD8;
/// `(INK - PAPER) / 255` in 256ths, rounded: the slope of the luma's way from ink to paper.
const NIGHT_SLOPE: i32 = ((NIGHT_INK - NIGHT_PAPER) * 256 + 127) / 255;
/// The paper of Sepia: #F4ECD8, the warm white that reading apps use for their sepia page (blue at 85 percent, red at 96 percent: the
/// light is kinder and the contrast with black text is still about 18 to 1).
const SEPIA: u32 = 0x00F4_ECD8;

impl Tint {
    /// What white becomes, as 0xRRGGBB (what is shown of a page before its bitmap has come).
    pub fn paper(self) -> u32 {
        match self {
            Tint::Original => 0xFF_FFFF,
            Tint::Night => (NIGHT_PAPER as u32) * 0x01_0101,
            Tint::Sepia => SEPIA,
        }
    }
}

/// The night colour of one pixel (a BGRA pixel as a little-endian number: blue is the low byte, alpha the high one, which is kept). Everything
/// fits 16 bits (the largest sum is 65 408), so that the compiler can do eight pixels' channels at a time.
#[inline(always)]
fn night(px: u32) -> u32 {
    let (b, g, r) = ((px & 255) as u16, ((px >> 8) & 255) as u16, ((px >> 16) & 255) as u16);
    let y = (77 * r + 150 * g + 29 * b + 128) >> 8;
    let shift = NIGHT_INK as i16 - ((y * NIGHT_SLOPE as u16 + 128) >> 8) as i16 - y as i16;
    let c = |v: u16| (v as i16 + shift).clamp(0, 255) as u32;
    (px & 0xFF00_0000) | c(b) | (c(g) << 8) | (c(r) << 16)
}

/// The sepia colour of one pixel.
#[inline(always)]
fn sepia(px: u32) -> u32 {
    let m = |c: u32, t: u32| (c * (t + 1)) >> 8;
    (px & 0xFF00_0000) | m(px & 255, SEPIA & 255) | (m((px >> 8) & 255, (SEPIA >> 8) & 255) << 8) | (m((px >> 16) & 255, SEPIA >> 16) << 16)
}

/// A bitmap of at least this many pixels is worked on by several threads at once.
const PARALLEL_FROM: usize = 1 << 19;
const MOST_THREADS: usize = 4;

/// Change the pixels of a BGRA bitmap (rows from the top, no padding; bytes beyond the last whole pixel are left alone) to `tint`. A big bitmap
/// (a tile of a page on a big window) is cut into pieces for up to four threads; one that cannot be started leaves its piece as it was.
pub fn recolor(bgra: &mut [u8], tint: Tint) {
    if tint == Tint::Original {
        return;
    }
    let (pixels, _) = bgra.as_chunks_mut::<4>();
    let threads = if pixels.len() >= PARALLEL_FROM { std::thread::available_parallelism().map_or(1, |n| n.get()).min(MOST_THREADS) } else { 1 };
    let piece = pixels.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let mut pieces = pixels.chunks_mut(piece);
        // The first piece is done here, the others by threads of their own.
        let mine = pieces.next();
        for p in pieces {
            let _ = std::thread::Builder::new().spawn_scoped(scope, move || work(p, tint));
        }
        if let Some(p) = mine {
            work(p, tint);
        }
    });
}

fn work(pixels: &mut [[u8; 4]], tint: Tint) {
    match tint {
        Tint::Original => {}
        Tint::Night => each(pixels, night),
        Tint::Sepia => each(pixels, sepia),
    }
}

/// `f` on each pixel. A page is mostly paper, so sixteen pixels of white at a time are given the answer for white without working it out; the rest
/// are worked out one by one (a loop with nothing carried from one pixel to the next, which the compiler turns into SIMD).
#[inline(always)]
fn each(pixels: &mut [[u8; 4]], f: impl Fn(u32) -> u32) {
    let white = f(u32::MAX).to_le_bytes();
    for block in pixels.chunks_mut(16) {
        if block.iter().all(|p| p == &[255; 4]) {
            block.fill(white);
        } else {
            for p in block {
                *p = f(u32::from_le_bytes(*p)).to_le_bytes();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pixel as BGRA bytes from r, g, b.
    fn px(r: u8, g: u8, b: u8) -> [u8; 4] {
        [b, g, r, 255]
    }

    fn one(tint: Tint, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        let mut v = px(r, g, b).to_vec();
        recolor(&mut v, tint);
        assert_eq!(v[3], 255, "alpha is kept");
        (v[2], v[1], v[0])
    }

    #[test]
    fn paper_and_ink_go_where_they_are_said_to() {
        assert_eq!(one(Tint::Night, 255, 255, 255), (0x1C, 0x1C, 0x1C));
        assert_eq!(one(Tint::Night, 0, 0, 0), (0xD8, 0xD8, 0xD8));
        assert_eq!(one(Tint::Sepia, 255, 255, 255), (0xF4, 0xEC, 0xD8));
        assert_eq!(one(Tint::Sepia, 0, 0, 0), (0, 0, 0));
        assert_eq!(one(Tint::Original, 12, 34, 56), (12, 34, 56));
        assert_eq!(Tint::Night.paper(), 0x1C1C1C);
        assert_eq!(Tint::Sepia.paper(), 0xF4ECD8);
        assert_eq!(Tint::Original.paper(), 0xFFFFFF);
        // What paper() says is what the white of a page becomes.
        for t in [Tint::Night, Tint::Sepia] {
            let (r, g, b) = one(t, 255, 255, 255);
            assert_eq!((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b), t.paper());
        }
    }

    #[test]
    fn night_turns_the_lightness_over_and_keeps_the_hue() {
        // Greys go from light to dark as they go from dark to light, without a break.
        let mut last = 256;
        for g in 0..=255u8 {
            let (r, gg, b) = one(Tint::Night, g, g, g);
            assert!(r == gg && gg == b, "a grey stays grey: {g}");
            assert!(i32::from(r) <= last, "{g}");
            last = i32::from(r);
        }
        // A red stays red, a yellow highlight stays yellow (red and green equal, blue least), blue text stays blue; each is the lightness
        // turned over, so a light colour is dark and a dark one light.
        let (r, g, b) = one(Tint::Night, 255, 0, 0);
        assert!(r > g && g == b && r == 255, "{r} {g} {b}");
        let (r, g, b) = one(Tint::Night, 255, 255, 0);
        assert!(r == g && g > b && r < 120, "{r} {g} {b}");
        let (r, g, b) = one(Tint::Night, 0, 0, 128);
        assert!(b > r && r == g && r > 150, "{r} {g} {b}");
        // The difference between the channels is what it was (where nothing is cut): a mid green.
        let (r, g, b) = one(Tint::Night, 100, 150, 120);
        assert_eq!((i32::from(g) - i32::from(r), i32::from(b) - i32::from(r)), (50, 20));
        // Black text on white, the usual page: light grey on near black, with a lot of contrast.
        let (ink, paper) = (one(Tint::Night, 0, 0, 0).0, one(Tint::Night, 255, 255, 255).0);
        assert!(ink > 200 && paper < 40);
    }

    #[test]
    fn sepia_multiplies_by_the_paper_colour() {
        assert_eq!(one(Tint::Sepia, 128, 128, 128), (122, 118, 108));
        // Dark text stays dark: it is darker than it was, never lighter.
        for v in [0u8, 10, 60, 120, 200, 255] {
            let (r, g, b) = one(Tint::Sepia, v, v, v);
            assert!(r <= v && g <= v && b <= v);
        }
        // Red stays red.
        let (r, g, b) = one(Tint::Sepia, 200, 0, 0);
        assert!(r > 150 && g == 0 && b == 0);
    }

    /// A pattern of runs and changes, with the translucent and the opaque, and `extra` bytes that are not a pixel.
    fn pattern(count: usize, extra: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut seed = 12345u32;
        for i in 0..count {
            if i % 7 == 0 {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            }
            // (Now and then sixteen pixels of white, the paper.)
            let white = (i / 16) % 5 == 0;
            bytes.extend_from_slice(&if white { [255, 255, 255, 255] } else { [(seed >> 8) as u8, (seed >> 16) as u8, (seed >> 24) as u8, if i % 11 == 0 { 7 } else { 255 }] });
        }
        bytes.extend((0..extra).map(|i| i as u8 + 1));
        bytes
    }

    #[test]
    fn working_in_runs_and_in_pieces_gives_the_same_answer_as_working_each_pixel_out_and_odd_bytes_are_left_alone() {
        // 5000 pixels are done by one thread, 700 000 (more than PARALLEL_FROM) by several.
        for (count, extra) in [(5000, 3), (PARALLEL_FROM + 12_345, 0), (PARALLEL_FROM + 12_345, 2)] {
            let bytes = pattern(count, extra);
            for tint in [Tint::Night, Tint::Sepia] {
                let f: fn(u32) -> u32 = if tint == Tint::Night { night } else { sepia };
                let mut run = bytes.clone();
                recolor(&mut run, tint);
                let mut each = bytes.clone();
                for p in each.chunks_exact_mut(4) {
                    p.copy_from_slice(&f(u32::from_le_bytes([p[0], p[1], p[2], p[3]])).to_le_bytes());
                }
                assert!(run == each, "{tint:?} {count} pixels");
                assert_eq!(run.get(count * 4..), bytes.get(count * 4..));
            }
        }
        let mut empty: Vec<u8> = Vec::new();
        recolor(&mut empty, Tint::Night);
        let mut short = vec![9u8, 9];
        recolor(&mut short, Tint::Sepia);
        assert_eq!(short, [9, 9]);
    }
}

#[cfg(test)]
mod speed {
    use super::*;

    /// How long recolouring a tile of 1889 by 2048 pixels takes (a page at fit width on a 1920 wide window): run by hand with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_time_to_recolor_a_tile() {
        let n = 1889 * 2048;
        // A page of text: runs of white with now and then a dark or grey pixel; and a photograph: every pixel different.
        let (mut text, mut photo) = (Vec::with_capacity(n * 4), Vec::with_capacity(n * 4));
        let mut seed = 1u32;
        for i in 0..n {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let v = (seed >> 16) as u8;
            text.extend_from_slice(&if i % 11 == 0 && v.is_multiple_of(3) { [v, v, v, 255] } else { [255, 255, 255, 255] });
            photo.extend_from_slice(&[v, (seed >> 8) as u8, (seed >> 24) as u8, 255]);
        }
        for (name, data) in [("text", &text), ("photo", &photo)] {
            for tint in [Tint::Night, Tint::Sepia] {
                let mut best = f64::MAX;
                for _ in 0..5 {
                    let mut copy = data.clone();
                    let t = std::time::Instant::now();
                    recolor(&mut copy, tint);
                    best = best.min(t.elapsed().as_secs_f64() * 1000.0);
                    std::hint::black_box(&copy);
                }
                println!("{name} {tint:?}: {best:.2} ms for {n} pixels");
            }
        }
    }
}
