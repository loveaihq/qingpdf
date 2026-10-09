//! Pattern dictionaries (T.88 6.7, 7.4.4) and halftone regions (6.6, 7.4.5, Annex C): a grid of cells, the gray
//! value of each picking a pattern, the grid laid on the region at an angle.

use std::rc::Rc;

use super::bitmap::{Bitmap, Op};
use super::generic::{self, GENERIC_CONTEXTS, GenericParams};
use super::text::ceil_log2;
use super::{Ctx, MAX_DIM, Region, bad, be32};
use crate::error::{Error, Result};
use crate::render::mq::Mq;
use crate::render::work::cost;

/// Most patterns of a dictionary, and most cells of a halftone grid.
const MAX_PATTERNS: usize = 1 << 16;
const MAX_CELLS: u64 = 1 << 24;

pub(super) struct PatternDict {
    /// All patterns side by side.
    sheet: Rc<Bitmap>,
    count: usize,
    pw: usize,
    ph: usize,
}

impl PatternDict {
    /// The memory of the image the patterns hold.
    pub fn held(&self) -> u64 {
        self.sheet.data.len() as u64 + super::BITMAP_OVERHEAD
    }
}

/// A pattern dictionary segment (7.4.4).
pub(super) fn decode_patterns(ctx: &Ctx<'_>, data: &[u8]) -> Result<PatternDict> {
    let short = || bad("a pattern dictionary ends too soon");
    let flags = *data.first().ok_or_else(short)?;
    let (pw, ph) = (usize::from(*data.get(1).ok_or_else(short)?), usize::from(*data.get(2).ok_or_else(short)?));
    let count = u64::from(be32(data, 3)?) + 1;
    if pw == 0 || ph == 0 || count > MAX_PATTERNS as u64 || count as usize * pw > MAX_DIM {
        return Err(bad("a pattern dictionary of impossible size"));
    }
    let count = count as usize;
    let body = data.get(7..).unwrap_or(&[]);
    let width = count * pw;
    let sheet = if flags & 1 != 0 {
        let (b, _, complete) = generic::decode_mmr(ctx, body, width, ph)?;
        if !complete {
            return Err(bad("a pattern dictionary is damaged"));
        }
        b
    } else {
        let template = (flags >> 1) & 3;
        if pw > 128 {
            return Err(Error::Unsupported("JBIG2: patterns wider than 128 pixels".to_string()));
        }
        let at = [(-(pw as i32) as i8, 0), (-3, -1), (2, -2), (-2, -2)];
        let mut mq = Mq::new(body);
        let mut cx = vec![0u8; GENERIC_CONTEXTS];
        ctx.charge(cx.len() as f64 * cost::JB2_CLEAR_BYTE)?;
        let params = GenericParams { template, tpgdon: false, at, skip: None };
        let (b, complete) = generic::decode_generic(ctx, &mut mq, &mut cx, width, ph, &params)?;
        if !complete {
            return Err(bad("a pattern dictionary is cut short"));
        }
        b
    };
    Ok(PatternDict { sheet: Rc::new(sheet), count, pw, ph })
}

/// A halftone region segment (7.4.5), `data` from the region information on.
pub(super) fn decode_region(ctx: &Ctx<'_>, data: &[u8], info: &Region, dict: &PatternDict) -> Result<Bitmap> {
    let short = || bad("a halftone region ends too soon");
    let flags = *data.get(17).ok_or_else(short)?;
    let mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    let enable_skip = flags & 8 != 0;
    let op = Op::from_code(u32::from((flags >> 4) & 7)).ok_or_else(|| bad("halftone operator"))?;
    let (gw, gh) = (u64::from(be32(data, 18)?), u64::from(be32(data, 22)?));
    let (gx, gy) = (i64::from(be32(data, 26)? as i32), i64::from(be32(data, 30)? as i32));
    let rx = i64::from(u16::from_be_bytes(data.get(34..36).and_then(|s| s.try_into().ok()).ok_or_else(short)?));
    let ry = i64::from(u16::from_be_bytes(data.get(36..38).and_then(|s| s.try_into().ok()).ok_or_else(short)?));
    if gw.saturating_mul(gh) > MAX_CELLS || gw > MAX_DIM as u64 || gh > MAX_DIM as u64 {
        return Err(bad("a halftone grid that is too large"));
    }
    let (gw, gh) = (gw as usize, gh as usize);
    let body = data.get(38..).unwrap_or(&[]);
    let mut region = ctx.bitmap(info.w, info.h, flags & 0x80 != 0)?;
    let (pw, ph) = (dict.pw as i64, dict.ph as i64);
    // Where the cell in row `m` and column `n` puts its pattern (6.6.5.2).
    let place = |m: usize, n: usize| -> (i64, i64) {
        let (m, n) = (m as i64, n as i64);
        ((gx + m * ry + n * rx) >> 8, (gy + m * rx - n * ry) >> 8)
    };
    let skip = if enable_skip {
        let mut s = ctx.bitmap(gw, gh, false)?;
        // A grid of no columns still has its rows.
        ctx.charge((gw * gh) as f64 * cost::JB2_CELL + gh as f64 * cost::JB2_ROW)?;
        for m in 0..gh {
            for n in 0..gw {
                let (x, y) = place(m, n);
                if x + pw <= 0 || x >= info.w as i64 || y + ph <= 0 || y >= info.h as i64 {
                    s.set(n, m, 1);
                }
            }
        }
        Some(s)
    } else {
        None
    };

    // The gray values: bit planes from the most significant, each the Gray code of the one before (Annex C).
    let bpp = ceil_log2(dict.count);
    ctx.take_memory((gw * gh * 4) as u64)?;
    ctx.charge((gw * gh) as f64 * (cost::JB2_CELL + f64::from(bpp) * 6.0) + gh as f64 * cost::JB2_ROW)?;
    let mut gray = vec![0u32; gw * gh];
    let mut mq = Mq::new(body);
    let mut cx = vec![0u8; if mmr { 0 } else { GENERIC_CONTEXTS }];
    ctx.charge(cx.len() as f64 * cost::JB2_CLEAR_BYTE)?;
    let at = [(if template <= 1 { 3 } else { 2 }, -1), (-3, -1), (2, -2), (-2, -2)];
    let mut offset = 0usize;
    let mut previous: Option<Bitmap> = None;
    for j in (0..bpp).rev() {
        let (mut plane, complete) = if mmr {
            let (b, used, complete) = generic::decode_mmr(ctx, body.get(offset..).unwrap_or(&[]), gw, gh)?;
            offset += used;
            (b, complete)
        } else {
            let params = GenericParams { template, tpgdon: false, at, skip: skip.as_ref() };
            generic::decode_generic(ctx, &mut mq, &mut cx, gw, gh, &params)?
        };
        if !complete {
            return Err(bad("a halftone region is cut short"));
        }
        if let Some(prev) = &previous {
            plane.combine(prev, 0, 0, Op::Xor);
        }
        for (m, row) in gray.chunks_exact_mut(gw.max(1)).enumerate() {
            for (n, g) in row.iter_mut().enumerate() {
                *g |= u32::from(plane.get(n as i64, m as i64)) << j;
            }
        }
        if let Some(old) = previous.replace(plane) {
            ctx.release(&old);
        }
    }
    if let Some(last) = previous {
        ctx.release(&last);
    }
    if let Some(s) = &skip {
        ctx.release(s);
    }

    // Draw the patterns.
    let row_cost = gw as f64 * (cost::JB2_CELL + (pw * ph) as f64 * cost::JB2_BLIT_PIXEL + ph as f64 * cost::JB2_BLIT_ROW);
    for (m, row) in gray.chunks_exact(gw.max(1)).enumerate() {
        ctx.charge(row_cost)?;
        for (n, &g) in row.iter().enumerate() {
            let (x, y) = place(m, n);
            region.combine_part(&dict.sheet, (g as usize).min(dict.count - 1) * dict.pw, dict.pw, x, y, op);
        }
    }
    ctx.give_back((gw * gh * 4) as u64);
    Ok(region)
}
