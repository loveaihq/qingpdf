//! Text regions (T.88 6.4, 7.4.3): symbols from dictionaries put on a bitmap in strips, optionally refined. The
//! same procedure decodes the aggregate symbols of a symbol dictionary ([`super::symbol`]).

use std::rc::Rc;

use super::arith::{IntCtx, decode_iaid};
use super::bitmap::{Bitmap, Op};
use super::generic::{self, REFINE_CONTEXTS, RefineParams};
use super::huffman::{BitReader, Table};
use super::{Ctx, Region, at_pairs, bad, be32};
use crate::error::Result;
use crate::render::mq::Mq;
use crate::render::work::cost;

/// Most symbols a region may choose from, and most instances it may place.
pub(super) const MAX_INSTANCES: u64 = 1 << 26;
/// Longest symbol code (a dictionary adds at most `2^19` symbols to the `2^20` it was given).
const MAX_CODE_LEN: u32 = 21;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Corner {
    BottomLeft,
    TopLeft,
    BottomRight,
    TopRight,
}

pub(super) struct TextParams<'a> {
    pub w: usize,
    pub h: usize,
    pub instances: u64,
    pub log_strips: u32,
    pub syms: &'a [Rc<Bitmap>],
    /// `SBSYMCODELEN`.
    pub code_len: u32,
    pub default_black: bool,
    pub op: Op,
    pub transposed: bool,
    pub corner: Corner,
    pub ds_offset: i64,
    pub refine: bool,
    pub rtemplate: u8,
    pub rat: [(i8, i8); 2],
}

/// The contexts of the integer procedures of a text region, and of the symbol IDs.
pub(super) struct TextIa {
    pub dt: IntCtx,
    pub fs: IntCtx,
    pub ds: IntCtx,
    pub it: IntCtx,
    pub ri: IntCtx,
    pub rdw: IntCtx,
    pub rdh: IntCtx,
    pub rdx: IntCtx,
    pub rdy: IntCtx,
    pub iaid: Vec<u8>,
}

impl TextIa {
    pub fn new(code_len: u32) -> Result<TextIa> {
        if code_len > MAX_CODE_LEN {
            return Err(bad("too many symbols to number"));
        }
        Ok(TextIa {
            dt: IntCtx::new(),
            fs: IntCtx::new(),
            ds: IntCtx::new(),
            it: IntCtx::new(),
            ri: IntCtx::new(),
            rdw: IntCtx::new(),
            rdh: IntCtx::new(),
            rdx: IntCtx::new(),
            rdy: IntCtx::new(),
            iaid: vec![0; 1usize << (code_len + 1)],
        })
    }
}

/// The Huffman tables a text region reads with. `syms`: the code of the symbol numbers; without it they are
/// `code_len` bits each.
pub(super) struct HuffTables<'a> {
    pub fs: &'a Table,
    pub ds: &'a Table,
    pub dt: &'a Table,
    pub rdw: &'a Table,
    pub rdh: &'a Table,
    pub rdx: &'a Table,
    pub rdy: &'a Table,
    pub rsize: &'a Table,
    pub syms: Option<&'a Table>,
}

/// Where the integers of a region come from: an arithmetic decoder or Huffman codes.
pub(super) enum Src<'s, 'd> {
    Arith { mq: &'s mut Mq<'d>, ia: &'s mut TextIa },
    Huff { r: &'s mut BitReader<'d>, t: &'s HuffTables<'s> },
}

fn number(v: Option<i64>) -> Result<i64> {
    v.ok_or_else(|| bad("an out-of-band value where there must be a number"))
}

impl Src<'_, '_> {
    fn int(&mut self, ctx: &Ctx<'_>, which: fn(&mut TextIa) -> &mut IntCtx, table: for<'t, 'a> fn(&'t HuffTables<'a>) -> &'t Table) -> Result<Option<i64>> {
        match self {
            Src::Arith { mq, ia } => {
                ctx.charge(cost::JB2_INT)?;
                Ok(which(ia).decode(mq))
            }
            Src::Huff { r, t } => {
                ctx.charge(cost::JB2_HUFFMAN)?;
                table(t).decode(r)
            }
        }
    }

    /// The position of a symbol instance inside its strip (`LOGSBSTRIPS` bits when Huffman coded).
    fn strip_t(&mut self, ctx: &Ctx<'_>, bits: u32) -> Result<i64> {
        match self {
            Src::Arith { mq, ia } => {
                ctx.charge(cost::JB2_INT)?;
                number(ia.it.decode(mq))
            }
            Src::Huff { r, .. } => Ok(i64::from(r.bits(bits)?)),
        }
    }

    /// Does the instance have a refinement?
    fn refined(&mut self, ctx: &Ctx<'_>) -> Result<bool> {
        match self {
            Src::Arith { mq, ia } => {
                ctx.charge(cost::JB2_INT)?;
                Ok(number(ia.ri.decode(mq))? != 0)
            }
            Src::Huff { r, .. } => Ok(r.bit()? != 0),
        }
    }

    /// The number of a symbol.
    fn symbol(&mut self, ctx: &Ctx<'_>, code_len: u32) -> Result<usize> {
        match self {
            Src::Arith { mq, ia } => {
                ctx.charge(cost::JB2_INT)?;
                Ok(decode_iaid(mq, &mut ia.iaid, code_len))
            }
            Src::Huff { r, t } => {
                ctx.charge(cost::JB2_HUFFMAN)?;
                match t.syms {
                    Some(table) => Ok(table.decode_value(r)?.max(0) as usize),
                    None => Ok(r.bits(code_len)? as usize),
                }
            }
        }
    }
}

/// Decode the instances of a text region (6.4.5).
pub(super) fn decode_text(ctx: &Ctx<'_>, p: &TextParams<'_>, src: &mut Src<'_, '_>, gr: &mut [u8]) -> Result<Bitmap> {
    let mut region = ctx.bitmap(p.w, p.h, p.default_black)?;
    let strips = 1i64 << p.log_strips;
    let mut stript = number(src.int(ctx, |i| &mut i.dt, |t| t.dt)?)?.saturating_mul(-strips);
    let mut firsts = 0i64;
    let mut n = 0u64;
    while n < p.instances {
        stript = stript.saturating_add(number(src.int(ctx, |i| &mut i.dt, |t| t.dt)?)?.saturating_mul(strips));
        firsts = firsts.saturating_add(number(src.int(ctx, |i| &mut i.fs, |t| t.fs)?)?);
        let mut curs = firsts;
        loop {
            let curt = if strips == 1 { 0 } else { src.strip_t(ctx, p.log_strips)? };
            let ti = stript.saturating_add(curt);
            let id = src.symbol(ctx, p.code_len)?;
            let refined = p.refine && src.refined(ctx)?;
            ctx.charge(cost::JB2_SYMBOL)?;
            let symbol = p.syms.get(id).ok_or_else(|| bad("a symbol number that is in no dictionary"))?;
            let made = if refined { Some(refine_symbol(ctx, p, src, gr, symbol)?) } else { None };
            let bitmap: &Bitmap = made.as_ref().unwrap_or(symbol);
            let (wi, hi) = (bitmap.w as i64, bitmap.h as i64);
            if !p.transposed && matches!(p.corner, Corner::TopRight | Corner::BottomRight) {
                curs = curs.saturating_add(wi - 1);
            } else if p.transposed && matches!(p.corner, Corner::BottomLeft | Corner::BottomRight) {
                curs = curs.saturating_add(hi - 1);
            }
            let s = curs;
            let (x, y) = if !p.transposed {
                match p.corner {
                    Corner::TopLeft => (s, ti),
                    Corner::TopRight => (s - wi + 1, ti),
                    Corner::BottomLeft => (s, ti - hi + 1),
                    Corner::BottomRight => (s - wi + 1, ti - hi + 1),
                }
            } else {
                match p.corner {
                    Corner::TopLeft => (ti, s),
                    Corner::TopRight => (ti - wi + 1, s),
                    Corner::BottomLeft => (ti, s - hi + 1),
                    Corner::BottomRight => (ti - wi + 1, s - hi + 1),
                }
            };
            // Charged whether or not it lands on the region.
            ctx.charge(region.overlap(bitmap.w, bitmap.h, x, y) as f64 * cost::JB2_BLIT_PIXEL + hi as f64 * cost::JB2_BLIT_ROW)?;
            region.combine(bitmap, x, y, p.op);
            if let Some(m) = &made {
                ctx.release(m);
            }
            if !p.transposed && matches!(p.corner, Corner::TopLeft | Corner::BottomLeft) {
                curs = curs.saturating_add(wi - 1);
            } else if p.transposed && matches!(p.corner, Corner::TopLeft | Corner::TopRight) {
                curs = curs.saturating_add(hi - 1);
            }
            n += 1;
            match src.int(ctx, |i| &mut i.ds, |t| t.ds)? {
                None => break,
                Some(ids) => {
                    if n >= p.instances {
                        break;
                    }
                    curs = curs.saturating_add(ids).saturating_add(p.ds_offset);
                }
            }
        }
    }
    Ok(region)
}

/// A symbol instance with a refinement (6.4.11).
fn refine_symbol(ctx: &Ctx<'_>, p: &TextParams<'_>, src: &mut Src<'_, '_>, gr: &mut [u8], symbol: &Bitmap) -> Result<Bitmap> {
    let rdw = number(src.int(ctx, |i| &mut i.rdw, |t| t.rdw)?)?;
    let rdh = number(src.int(ctx, |i| &mut i.rdh, |t| t.rdh)?)?;
    let rdx = number(src.int(ctx, |i| &mut i.rdx, |t| t.rdx)?)?;
    let rdy = number(src.int(ctx, |i| &mut i.rdy, |t| t.rdy)?)?;
    let (w, h) = (symbol.w as i64 + rdw, symbol.h as i64 + rdh);
    if w < 0 || h < 0 || w > super::MAX_DIM as i64 || h > super::MAX_DIM as i64 {
        return Err(bad("a refined symbol of impossible size"));
    }
    let params = RefineParams { template: p.rtemplate, tpgron: false, at: p.rat, reference: symbol, dx: (rdw >> 1) + rdx, dy: (rdh >> 1) + rdy };
    match src {
        Src::Arith { mq, .. } => Ok(generic::decode_refinement(ctx, mq, gr, w as usize, h as usize, &params)?.0),
        Src::Huff { r, t } => {
            ctx.charge(cost::JB2_HUFFMAN)?;
            let size = number(t.rsize.decode(r)?)?;
            let at = r.byte_pos();
            let end = usize::try_from(size).ok().and_then(|s| at.checked_add(s)).ok_or_else(|| bad("a refinement of impossible size"))?;
            let slice = r.data().get(at..end).ok_or_else(|| bad("a refinement cut off"))?;
            let mut mq = Mq::new(slice);
            let made = generic::decode_refinement(ctx, &mut mq, gr, w as usize, h as usize, &params)?.0;
            r.seek(end);
            Ok(made)
        }
    }
}

/// `ceil(log2(n))`, 0 for 0 and 1.
pub(super) fn ceil_log2(n: usize) -> u32 {
    if n <= 1 { 0 } else { usize::BITS - (n - 1).leading_zeros() }
}

/// The symbol ID codes of a Huffman coded region (7.4.3.1.7): 35 run codes, then the code lengths of the symbols.
fn read_symbol_codes(r: &mut BitReader<'_>, n: usize) -> Result<Table> {
    let mut run_lengths = [0u8; 35];
    for l in &mut run_lengths {
        *l = r.bits(4)? as u8;
    }
    let runs = Table::from_lengths(&run_lengths)?;
    let mut lengths: Vec<u8> = Vec::with_capacity(n);
    while lengths.len() < n {
        let code = runs.decode_value(r)?;
        let (value, count) = match code {
            0..=31 => (code as u8, 1),
            32 => (lengths.last().copied().ok_or_else(|| bad("a repeat with nothing to repeat"))?, 3 + r.bits(2)? as usize),
            33 => (0, 3 + r.bits(3)? as usize),
            _ => (0, 11 + r.bits(7)? as usize),
        };
        if lengths.len() + count > n {
            return Err(bad("symbol code lengths run past the last symbol"));
        }
        lengths.resize(lengths.len() + count, value);
    }
    r.align();
    Table::from_lengths(&lengths)
}

/// The sign-extended 5 bit offset of the strip distances.
fn ds_offset(flags: u16) -> i64 {
    let v = i64::from((flags >> 10) & 0x1F);
    if v >= 16 { v - 32 } else { v }
}

/// A text region segment (7.4.3), `data` from the region information on.
pub(super) fn decode_region(ctx: &Ctx<'_>, data: &[u8], info: &Region, syms: Vec<Rc<Bitmap>>, custom: &[Rc<Table>]) -> Result<Bitmap> {
    let flags = u16::from_be_bytes(data.get(17..19).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("a text region ends too soon"))?);
    let mut pos = 19;
    let huff = flags & 1 != 0;
    let refine = flags & 2 != 0;
    let corner = match (flags >> 4) & 3 {
        0 => Corner::BottomLeft,
        1 => Corner::TopLeft,
        2 => Corner::BottomRight,
        _ => Corner::TopRight,
    };
    let rtemplate = ((flags >> 15) & 1) as u8;
    let op = Op::from_code(u32::from((flags >> 7) & 3)).ok_or_else(|| bad("text region operator"))?;
    let hflags = if huff {
        let f = u16::from_be_bytes(data.get(pos..pos + 2).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("a text region ends too soon"))?);
        pos += 2;
        f
    } else {
        0
    };
    let mut rat = [(0i8, 0i8); 2];
    if refine && rtemplate == 0 {
        rat = at_pairs::<2>(data, pos)?;
        pos += 4;
    }
    let instances = u64::from(be32(data, pos)?);
    pos += 4;
    if instances > MAX_INSTANCES {
        return Err(bad("a text region with too many symbol instances"));
    }
    if syms.len() > super::symbol::MAX_SYMBOLS {
        return Err(bad("a text region with too many symbols"));
    }
    let body = data.get(pos..).unwrap_or(&[]);
    let params = |code_len: u32| TextParams {
        w: info.w,
        h: info.h,
        instances,
        log_strips: u32::from((flags >> 2) & 3),
        syms: &syms,
        code_len,
        default_black: flags & 0x200 != 0,
        op,
        transposed: flags & 0x40 != 0,
        corner,
        ds_offset: ds_offset(flags),
        refine,
        rtemplate,
        rat,
    };
    let mut gr = if refine { vec![0u8; REFINE_CONTEXTS] } else { Vec::new() };
    if !huff {
        let code_len = ceil_log2(syms.len());
        let mut ia = TextIa::new(code_len)?;
        ctx.charge((ia.iaid.len() + 9 * 512 + gr.len()) as f64 * cost::JB2_CLEAR_BYTE)?;
        let mut mq = Mq::new(body);
        let mut src = Src::Arith { mq: &mut mq, ia: &mut ia };
        return decode_text(ctx, &params(code_len), &mut src, &mut gr);
    }
    // Huffman: the tables the flags choose (custom ones in the order they are used), then the symbol codes.
    let mut used = custom.iter();
    let fs = Table::select(&mut used, hflags & 3, &[6, 7, -1, 0])?;
    let ds = Table::select(&mut used, (hflags >> 2) & 3, &[8, 9, 10, 0])?;
    let dt = Table::select(&mut used, (hflags >> 4) & 3, &[11, 12, 13, 0])?;
    let rdw = Table::select(&mut used, (hflags >> 6) & 3, &[14, 15, -1, 0])?;
    let rdh = Table::select(&mut used, (hflags >> 8) & 3, &[14, 15, -1, 0])?;
    let rdx = Table::select(&mut used, (hflags >> 10) & 3, &[14, 15, -1, 0])?;
    let rdy = Table::select(&mut used, (hflags >> 12) & 3, &[14, 15, -1, 0])?;
    let rsize = Table::select(&mut used, (hflags >> 14) & 1, &[1, 0])?;
    let mut r = BitReader::new(body);
    // Charged first: a code for every symbol is read and the table made (32 passes over them), and the table is held
    // while the region is decoded.
    ctx.charge(syms.len() as f64 * cost::JB2_HUFFMAN + Table::build_work(syms.len()))?;
    let held = Table::bytes_for(syms.len()) + syms.len() as u64;
    ctx.take_memory(held)?;
    let codes = read_symbol_codes(&mut r, syms.len())?;
    let tables = HuffTables { fs, ds, dt, rdw, rdh, rdx, rdy, rsize, syms: Some(&codes) };
    let mut src = Src::Huff { r: &mut r, t: &tables };
    let region = decode_text(ctx, &params(0), &mut src, &mut gr);
    ctx.give_back(held);
    region
}
