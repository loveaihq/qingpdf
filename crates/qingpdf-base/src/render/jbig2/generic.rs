//! Generic regions (T.88 6.2) and generic refinement regions (6.3): bitmaps decoded pixel by pixel with an
//! arithmetic coder whose context is made of the pixels already decoded (and, for refinement, of a reference
//! bitmap), or, for generic regions, MMR (T.6 fax) coded.

use super::bitmap::Bitmap;
use super::{Ctx, bad};
use crate::error::Result;
use crate::render::ccitt;
use crate::render::mq::Mq;
use crate::render::work::cost;

/// Contexts of a generic region template: at most 16 bits.
pub(super) const GENERIC_CONTEXTS: usize = 1 << 16;
/// Contexts of a refinement template: 13 bits at most.
pub(super) const REFINE_CONTEXTS: usize = 1 << 13;

pub(super) struct GenericParams<'a> {
    pub template: u8,
    /// Typical prediction of whole rows (`TPGDON`).
    pub tpgdon: bool,
    /// The adaptive pixels: four for template 0, one (the first) for the others.
    pub at: [(i8, i8); 4],
    /// Pixels that are not coded and are 0 (`USESKIP`).
    pub skip: Option<&'a Bitmap>,
}

/// How a template reads its context from three windows on the row two above, the row above and the row being
/// decoded. A window is a run of pixels, the leftmost the most significant bit, `right` pixels reaching right of the
/// pixel being decoded. The adaptive pixels at their nominal places are inside the windows: the context bit of an
/// adaptive pixel is the same whether it is read from the window or placed (`slots`).
struct Tpl {
    mask2: u32,
    right2: usize,
    mask1: u32,
    right1: usize,
    mask0: u32,
    shift2: u32,
    shift1: u32,
    /// The context that codes `SLTP` (6.2.5.7, figures 8 to 11).
    sltp: usize,
    /// The bit of the context each adaptive pixel has, and where each is unless the file moves it.
    slots: [u32; 4],
    nominal: [(i8, i8); 4],
    adaptive: usize,
}

const fn tpl(t: usize) -> Tpl {
    match t {
        0 => Tpl {
            mask2: 0x1F,
            right2: 2,
            mask1: 0x7F,
            right1: 3,
            mask0: 0xF,
            shift2: 11,
            shift1: 4,
            sltp: 0x9B25,
            slots: [4, 10, 11, 15],
            nominal: [(3, -1), (-3, -1), (2, -2), (-2, -2)],
            adaptive: 4,
        },
        1 => Tpl {
            mask2: 0xF,
            right2: 2,
            mask1: 0x3F,
            right1: 3,
            mask0: 0x7,
            shift2: 9,
            shift1: 3,
            sltp: 0x0795,
            slots: [3, 0, 0, 0],
            nominal: [(3, -1), (0, 0), (0, 0), (0, 0)],
            adaptive: 1,
        },
        2 => Tpl {
            mask2: 0x7,
            right2: 1,
            mask1: 0x1F,
            right1: 2,
            mask0: 0x3,
            shift2: 7,
            shift1: 2,
            sltp: 0x00E5,
            slots: [2, 0, 0, 0],
            nominal: [(2, -1), (0, 0), (0, 0), (0, 0)],
            adaptive: 1,
        },
        _ => Tpl {
            mask2: 0,
            right2: 0,
            mask1: 0x3F,
            right1: 2,
            mask0: 0xF,
            shift2: 0,
            shift1: 4,
            sltp: 0x0195,
            slots: [4, 0, 0, 0],
            nominal: [(2, -1), (0, 0), (0, 0), (0, 0)],
            adaptive: 1,
        },
    }
}

/// Zero pixels either side of a row, more than any window reaches.
const PAD: usize = 8;

/// Decode a generic region of `w` by `h` pixels. The bool is false when the data ran out before the last row (the
/// rest is white).
pub(super) fn decode_generic(ctx: &Ctx<'_>, mq: &mut Mq<'_>, cx: &mut [u8], w: usize, h: usize, p: &GenericParams<'_>) -> Result<(Bitmap, bool)> {
    let template = usize::from(p.template.min(3));
    let t = tpl(template);
    for &(ax, ay) in p.at.iter().take(t.adaptive) {
        // An adaptive pixel is a pixel already decoded (6.2.5.4).
        if ay > 0 || (ay == 0 && ax >= 0) || ax < -128 {
            return Err(bad("an adaptive template pixel that is not decoded yet"));
        }
    }
    let Some(cx) = cx.first_chunk_mut::<GENERIC_CONTEXTS>() else {
        return Err(bad("generic region contexts"));
    };
    let mut out = ctx.bitmap(w, h, false)?;
    let nominal = p.at.iter().zip(&t.nominal).take(t.adaptive).all(|(a, b)| a == b);
    let complete = match template {
        0 => rows::<0>(ctx, mq, cx, &mut out, p, nominal)?,
        1 => rows::<1>(ctx, mq, cx, &mut out, p, nominal)?,
        2 => rows::<2>(ctx, mq, cx, &mut out, p, nominal)?,
        _ => rows::<3>(ctx, mq, cx, &mut out, p, nominal)?,
    };
    Ok((out, complete))
}

/// How many pixels from `from` on of `row` (one byte a pixel, 0 or 1) are all `v`.
fn same_ahead(row: &[u8], from: usize, v: u8) -> usize {
    let rest = row.get(from..).unwrap_or(&[]);
    let pattern = u64::from(v) * 0x0101_0101_0101_0101;
    let mut n = 0;
    let mut words = rest.chunks_exact(8);
    for word in &mut words {
        if word.try_into().is_ok_and(|b| u64::from_le_bytes(b) == pattern) {
            n += 8;
        } else {
            return n + word.iter().take_while(|&&b| b == v).count();
        }
    }
    n + words.remainder().iter().take_while(|&&b| b == v).count()
}

/// The pixels of one row when the adaptive pixels are where the standard puts them and no pixel is skipped: the
/// loop that all the time of a scanned page goes in. `cur` is the row (the pixels only), `r2` and `r1` the rows two
/// above and above, with their margins.
///
/// Where the pixels around the one being decoded are all white (or all black) and the next ones of the rows above
/// are too, the context stays the same for a long stretch, and the arithmetic decoder can take the decisions of the
/// stretch in one go ([`Mq::decode_run`]): most of a scanned page is such.
#[inline(always)]
fn row_fast<const T: usize>(mq: &mut Mq<'_>, cx: &mut [u8; GENERIC_CONTEXTS], r2: &[u8], r1: &[u8], cur: &mut [u8]) -> usize {
    let t = const { tpl(T) };
    let fold = |row: &[u8], right: usize| row.get(PAD..=PAD + right).unwrap_or(&[]).iter().fold(0u32, |a, &b| (a << 1) | u32::from(b));
    let mut w2 = if t.mask2 == 0 { 0 } else { fold(r2, t.right2) };
    let mut w1 = fold(r1, t.right1);
    let mut w0 = 0u32;
    // The pixels that come into the windows as the pixel being decoded moves right.
    let in2 = r2.get(PAD + 1 + t.right2..).unwrap_or(&[]);
    let in1 = r1.get(PAD + 1 + t.right1..).unwrap_or(&[]);
    let w = cur.len();
    let full = (t.mask2 << t.shift2) | (t.mask1 << t.shift1) | t.mask0;
    let mut x = 0;
    // The pixels decoded one by one (the rest came in runs).
    let mut singles = 0usize;
    while x < w {
        let uniform = if (w2 | w1 | w0) == 0 {
            Some(0u8)
        } else if w2 == t.mask2 && w1 == t.mask1 && w0 == t.mask0 {
            Some(1u8)
        } else {
            None
        };
        if let Some(v) = uniform {
            let mut ahead = same_ahead(in1, x, v).min(w - x);
            if t.mask2 != 0 {
                ahead = ahead.min(same_ahead(in2, x, v));
            }
            if ahead >= 8
                && let Some(state) = cx.get_mut(if v == 0 { 0 } else { full as usize & (GENERIC_CONTEXTS - 1) })
            {
                let (n, odd) = mq.decode_run(state, u32::from(v), ahead);
                if let Some(run) = cur.get_mut(x..x + n) {
                    run.fill(v);
                }
                x += n;
                if odd {
                    // The decision after the run came out the other way; the rows above are still `v` there.
                    let bit = 1 - v;
                    if let Some(d) = cur.get_mut(x) {
                        *d = bit;
                    }
                    w2 = ((w2 << 1) | u32::from(v)) & t.mask2;
                    w1 = ((w1 << 1) | u32::from(v)) & t.mask1;
                    w0 = ((w0 << 1) | u32::from(bit)) & t.mask0;
                    x += 1;
                    singles += 1;
                }
                continue;
            }
        }
        let c = (w2 << t.shift2) | (w1 << t.shift1) | w0;
        let bit = match cx.get_mut(c as usize & (GENERIC_CONTEXTS - 1)) {
            Some(state) => mq.decode(state) as u8,
            None => 0,
        };
        if let Some(d) = cur.get_mut(x) {
            *d = bit;
        }
        let a = in2.get(x).copied().unwrap_or(0);
        let b = in1.get(x).copied().unwrap_or(0);
        w2 = ((w2 << 1) | u32::from(a)) & t.mask2;
        w1 = ((w1 << 1) | u32::from(b)) & t.mask1;
        w0 = ((w0 << 1) | u32::from(bit)) & t.mask0;
        x += 1;
        singles += 1;
    }
    singles
}

fn rows<const T: usize>(ctx: &Ctx<'_>, mq: &mut Mq<'_>, cx: &mut [u8; GENERIC_CONTEXTS], out: &mut Bitmap, p: &GenericParams<'_>, nominal: bool) -> Result<bool> {
    let t = const { tpl(T) };
    let (w, h) = (out.w, out.h);
    let mut r2 = vec![0u8; w + 2 * PAD];
    let mut r1 = r2.clone();
    let mut cur = r2.clone();
    let mut ltp = false;
    for y in 0..h {
        if mq.exhausted() {
            return Ok(false);
        }
        if p.tpgdon {
            let bit = cx.get_mut(t.sltp).map_or(0, |c| mq.decode(c));
            ltp ^= bit == 1;
            if ltp {
                ctx.charge(w as f64 * cost::JB2_COPY_PIXEL + cost::JB2_ROW)?;
                cur.clone_from(&r1);
                out.pack_row(y, cur.get(PAD..).unwrap_or(&[]));
                std::mem::swap(&mut r2, &mut r1);
                std::mem::swap(&mut r1, &mut cur);
                continue;
            }
        }
        if nominal && p.skip.is_none() {
            // What a row costs depends on how much of it is runs: the runs are charged first, the pixels decoded on their
            // own when they are known (a page that has none of the one kind is at most one row over).
            ctx.charge(w as f64 * cost::JB2_RUN_PIXEL + cost::JB2_ROW)?;
            if let Some(pixels) = cur.get_mut(PAD..PAD + w) {
                ctx.spend(row_fast::<T>(mq, cx, &r2, &r1, pixels) as f64 * cost::JB2_GENERIC_PIXEL);
            }
            out.pack_row(y, cur.get(PAD..).unwrap_or(&[]));
            std::mem::swap(&mut r2, &mut r1);
            std::mem::swap(&mut r1, &mut cur);
            continue;
        }
        ctx.charge(w as f64 * cost::JB2_SLOW_PIXEL + cost::JB2_ROW)?;
        // The windows at the first pixel: the pixels from 0 to `right` (those before 0 are zeros).
        let fold = |row: &[u8], right: usize| row.get(PAD..=PAD + right).unwrap_or(&[]).iter().fold(0u32, |a, &b| (a << 1) | u32::from(b));
        let mut w2 = if t.mask2 == 0 { 0 } else { fold(&r2, t.right2) };
        let mut w1 = fold(&r1, t.right1);
        let mut w0 = 0u32;
        let skip = p.skip;
        for x in 0..w {
            let mut c = (w2 << t.shift2) | (w1 << t.shift1) | w0;
            if !nominal {
                for (i, &(ax, ay)) in p.at.iter().take(t.adaptive).enumerate() {
                    let (px, py) = (x as i64 + i64::from(ax), y as i64 + i64::from(ay));
                    let v = if ay == 0 {
                        // Left of the pixel in the row being decoded.
                        usize::try_from(px).ok().and_then(|px| cur.get(PAD + px)).map_or(0, |&v| u32::from(v))
                    } else {
                        u32::from(out.get(px, py))
                    };
                    let slot = t.slots.get(i).copied().unwrap_or(0);
                    c = (c & !(1 << slot)) | (v << slot);
                }
            }
            let bit = if skip.is_some_and(|s| s.get(x as i64, y as i64) != 0) {
                0
            } else {
                match cx.get_mut(c as usize) {
                    Some(state) => mq.decode(state) as u8,
                    None => 0,
                }
            };
            if let Some(d) = cur.get_mut(PAD + x) {
                *d = bit;
            }
            w2 = ((w2 << 1) | u32::from(r2.get(PAD + x + 1 + t.right2).copied().unwrap_or(0))) & t.mask2;
            w1 = ((w1 << 1) | u32::from(r1.get(PAD + x + 1 + t.right1).copied().unwrap_or(0))) & t.mask1;
            w0 = ((w0 << 1) | u32::from(bit)) & t.mask0;
        }
        out.pack_row(y, cur.get(PAD..).unwrap_or(&[]));
        std::mem::swap(&mut r2, &mut r1);
        std::mem::swap(&mut r1, &mut cur);
    }
    Ok(true)
}

/// An MMR coded bitmap (T.88 6.2.6): fax Group 4 with 1 for black and no end-of-line codes. Returns the bitmap, the
/// number of bytes of `data` it used (the end-of-block code included) and whether all rows were there.
pub(super) fn decode_mmr(ctx: &Ctx<'_>, data: &[u8], w: usize, h: usize) -> Result<(Bitmap, usize, bool)> {
    let mut out = ctx.bitmap(w, h, false)?;
    // A row costs something whatever its width: a bitmap of width 0 has as many rows as any other.
    ctx.charge((w as f64 * cost::JB2_MMR_PIXEL + cost::JB2_ROW) * h as f64)?;
    if w == 0 || h == 0 {
        return Ok((out, 0, true));
    }
    let params = ccitt::Params { k: -1, columns: w, rows: h, byte_align: false, black_is_1: true };
    let decoded = ccitt::decode(data, &params, out.data.len());
    let complete = !decoded.damaged && decoded.rows >= h;
    let n = decoded.data.len().min(out.data.len());
    if let (Some(dst), Some(src)) = (out.data.get_mut(..n), decoded.data.get(..n)) {
        dst.copy_from_slice(src);
    }
    Ok((out, decoded.used, complete))
}

pub(super) struct RefineParams<'a> {
    pub template: u8,
    /// Typical prediction (`TPGRON`).
    pub tpgron: bool,
    /// The adaptive pixels: the first in the bitmap being decoded, the second in the reference (template 0 only).
    pub at: [(i8, i8); 2],
    pub reference: &'a Bitmap,
    /// The reference pixel of (x, y) is that of (x - dx, y - dy).
    pub dx: i64,
    pub dy: i64,
}

/// Decode a refinement of `p.reference` as a bitmap of `w` by `h` pixels (T.88 6.3). The bool is false when the data
/// ran out.
pub(super) fn decode_refinement(ctx: &Ctx<'_>, mq: &mut Mq<'_>, cx: &mut [u8], w: usize, h: usize, p: &RefineParams<'_>) -> Result<(Bitmap, bool)> {
    if cx.len() < REFINE_CONTEXTS {
        return Err(bad("refinement contexts"));
    }
    let (ax1, ay1) = (i64::from(p.at[0].0), i64::from(p.at[0].1));
    if p.template == 0 && (ay1 > 0 || (ay1 == 0 && ax1 >= 0)) {
        return Err(bad("an adaptive refinement pixel that is not decoded yet"));
    }
    let mut out = ctx.bitmap(w, h, false)?;
    let template0 = p.template == 0;
    let sltp = if template0 { 0x20 } else { 0x08 };
    let r = p.reference;
    let (ax2, ay2) = (i64::from(p.at[1].0), i64::from(p.at[1].1));
    let mut ltp = false;
    for y in 0..h as i64 {
        if mq.exhausted() {
            return Ok((out, false));
        }
        ctx.charge(w as f64 * cost::JB2_REFINE_PIXEL + cost::JB2_ROW)?;
        if p.tpgron {
            let bit = cx.get_mut(sltp).map_or(0, |c| mq.decode(c));
            ltp ^= bit == 1;
        }
        for x in 0..w as i64 {
            let (rx, ry) = (x - p.dx, y - p.dy);
            if ltp {
                // Typical prediction: a pixel whose 3x3 reference neighbourhood is of one colour has that colour.
                let v = r.get(rx, ry);
                let same = (-1..=1).all(|oy| (-1..=1).all(|ox| r.get(rx + ox, ry + oy) == v));
                if same {
                    if v != 0 {
                        out.set(x as usize, y as usize, 1);
                    }
                    continue;
                }
            }
            let g = |ox: i64, oy: i64| usize::from(r.get(rx + ox, ry + oy));
            let o = |ox: i64, oy: i64| usize::from(out.get(x + ox, y + oy));
            let c = if template0 {
                (o(0, -1) << 12)
                    | (o(1, -1) << 11)
                    | (o(-1, 0) << 10)
                    | (o(ax1, ay1) << 9)
                    | (g(0, -1) << 8)
                    | (g(1, -1) << 7)
                    | (g(-1, 0) << 6)
                    | (g(0, 0) << 5)
                    | (g(1, 0) << 4)
                    | (g(-1, 1) << 3)
                    | (g(0, 1) << 2)
                    | (g(1, 1) << 1)
                    | g(ax2, ay2)
            } else {
                (o(-1, -1) << 9) | (o(0, -1) << 8) | (o(1, -1) << 7) | (o(-1, 0) << 6) | (g(0, -1) << 5) | (g(-1, 0) << 4) | (g(0, 0) << 3) | (g(1, 0) << 2) | (g(0, 1) << 1) | g(1, 1)
            };
            if let Some(state) = cx.get_mut(c)
                && mq.decode(state) == 1
            {
                out.set(x as usize, y as usize, 1);
            }
        }
    }
    Ok((out, true))
}
