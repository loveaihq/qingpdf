//! Symbol dictionaries (T.88 6.5, 7.4.2): the bitmaps a text region draws with, in height classes, arithmetic or
//! Huffman coded, optionally refinements or aggregates of earlier symbols, and a list of which are exported.
//!
//! The bitmaps are shared (`Rc`): a dictionary that exports the symbols it was given holds the same bitmaps, not
//! copies, however many dictionaries and regions use them.

use std::rc::Rc;

use super::arith::{IntCtx, decode_iaid};
use super::bitmap::{Bitmap, Op};
use super::generic::{self, GENERIC_CONTEXTS, GenericParams, REFINE_CONTEXTS, RefineParams};
use super::huffman::{BitReader, Table};
use super::text::{self, Corner, HuffTables, Src, TextIa, TextParams, ceil_log2};
use super::{Ctx, MAX_DIM, at_pairs, bad, be32};
use crate::error::Result;
use crate::render::mq::Mq;
use crate::render::work::cost;

/// Most symbols a segment may use from the dictionaries it refers to, and most a dictionary may define.
pub(super) const MAX_SYMBOLS: usize = 1 << 20;
const MAX_NEW_SYMBOLS: u64 = 1 << 19;
/// Most pixels the new symbols of one dictionary may have together.
const MAX_SYMBOL_PIXELS: u64 = 64 << 20;
/// Most instances one aggregate symbol may be made of.
const MAX_AGGREGATE: i64 = 1 << 16;
/// What an entry of a list of symbols takes (a pointer), a `Vec` apart from its entries, and the dictionary itself
/// (its `Rc` box included).
pub(super) const PTR: u64 = 8;
const VEC_HEADER: u64 = 24;
const DICT_BYTES: u64 = 96;

pub(super) struct SymbolDict {
    pub exported: Vec<Rc<Bitmap>>,
    /// The statistics of the generic and refinement coding, kept when the dictionary says so for any later one that
    /// has this one as its last dictionary to start from (7.4.2.2). They live as long as the dictionary does.
    retained: Option<(Vec<u8>, Vec<u8>)>,
    /// What the dictionary itself holds, apart from the bitmaps (those are shared and counted when they are made): the
    /// list of exported symbols and the retained statistics. Taken from the image's memory when it is made.
    pub held: u64,
}

fn number(v: Option<i64>) -> Result<i64> {
    v.ok_or_else(|| bad("an out-of-band value where there must be a number"))
}

/// Decode a symbol dictionary segment. `inputs`: the symbols of the dictionaries it refers to; `custom`: the tables
/// it refers to; `last`: the last dictionary it refers to.
pub(super) fn decode_dictionary(ctx: &Ctx<'_>, data: &[u8], inputs: Vec<Rc<Bitmap>>, custom: &[Rc<Table>], last: Option<&SymbolDict>) -> Result<SymbolDict> {
    let flags = u16::from_be_bytes(data.get(0..2).and_then(|s| s.try_into().ok()).ok_or_else(|| bad("a symbol dictionary ends too soon"))?);
    let huff = flags & 1 != 0;
    let refagg = flags & 2 != 0;
    let template = ((flags >> 10) & 3) as u8;
    let rtemplate = ((flags >> 12) & 1) as u8;
    let mut pos = 2;
    let mut at = [(0i8, 0i8); 4];
    if !huff {
        if template == 0 {
            at = at_pairs::<4>(data, pos)?;
            pos += 8;
        } else {
            at[0] = at_pairs::<1>(data, pos)?[0];
            pos += 2;
        }
    }
    let mut rat = [(0i8, 0i8); 2];
    if refagg && rtemplate == 0 {
        rat = at_pairs::<2>(data, pos)?;
        pos += 4;
    }
    let num_ex = u64::from(be32(data, pos)?);
    let num_new = u64::from(be32(data, pos + 4)?);
    pos += 8;
    if num_new > MAX_NEW_SYMBOLS || inputs.len() as u64 + num_new > MAX_SYMBOLS as u64 + MAX_NEW_SYMBOLS {
        return Err(bad("a symbol dictionary with too many symbols"));
    }
    let num_new = num_new as usize;
    let total = inputs.len() + num_new;
    if num_ex > total as u64 {
        return Err(bad("a symbol dictionary that exports more symbols than it has"));
    }
    ctx.charge(total as f64 * (cost::JB2_SYMBOL * 0.1 + cost::JB2_RC))?;
    // The list of all the symbols (it may grow twice over as it fills) and the export flags, until the dictionary is made.
    let scratch = num_new as u64 * 2 * PTR + total as u64;
    ctx.take_memory(scratch)?;
    let body = data.get(pos..).unwrap_or(&[]);

    // Tables (Huffman): the standard ones the flags choose and the custom ones in the order they are used.
    let mut used = custom.iter();
    let (dh_t, dw_t, bm_t, agg_t) = if huff {
        (
            Table::select(&mut used, (flags >> 2) & 3, &[4, 5, -1, 0])?,
            Table::select(&mut used, (flags >> 4) & 3, &[2, 3, -1, 0])?,
            Table::select(&mut used, (flags >> 6) & 1, &[1, 0])?,
            Table::select(&mut used, (flags >> 7) & 1, &[1, 0])?,
        )
    } else {
        let b1 = Table::standard(1)?;
        (b1, b1, b1, b1)
    };
    let b1 = Table::standard(1)?;
    let b15 = Table::standard(15)?;

    // Coders and contexts.
    let empty: &[u8] = &[];
    let mut mq = Mq::new(if huff && !refagg { empty } else { body });
    let mut r = BitReader::new(body);
    let mut gb = vec![0u8; GENERIC_CONTEXTS];
    let mut gr = vec![0u8; REFINE_CONTEXTS];
    if flags & 0x100 != 0
        && let Some((pgb, pgr)) = last.and_then(|d| d.retained.as_ref())
    {
        gb.clone_from(pgb);
        gr.clone_from(pgr);
    }
    let code_len = if huff { ceil_log2(total).max(1) } else { ceil_log2(total) };
    let mut ia = TextIa::new(if refagg { code_len } else { 0 })?;
    let (mut iadh, mut iadw, mut iaex, mut iaai) = (IntCtx::new(), IntCtx::new(), IntCtx::new(), IntCtx::new());
    ctx.charge((gb.len() + gr.len() + ia.iaid.len() + 13 * 512) as f64 * cost::JB2_CLEAR_BYTE)?;

    let mut all: Vec<Rc<Bitmap>> = inputs;
    let mut height: i64 = 0;
    let mut decoded = 0usize;
    let mut pixels = 0u64;
    while decoded < num_new {
        ctx.charge(cost::JB2_INT)?;
        let dh = if huff { dh_t.decode_value(&mut r)? } else { number(iadh.decode(&mut mq))? };
        height = height.saturating_add(dh);
        if !(0..=MAX_DIM as i64).contains(&height) {
            return Err(bad("a symbol height that is impossible"));
        }
        let mut sym_w: i64 = 0;
        let mut total_w: i64 = 0;
        let mut class_widths: Vec<usize> = Vec::new();
        loop {
            if !huff && mq.exhausted() {
                return Err(bad("a symbol dictionary is cut short"));
            }
            ctx.charge(cost::JB2_INT)?;
            let dw = if huff { dw_t.decode(&mut r)? } else { iadw.decode(&mut mq) };
            let Some(dw) = dw else { break };
            if decoded >= num_new {
                return Err(bad("a height class with more symbols than the dictionary has"));
            }
            sym_w = sym_w.saturating_add(dw);
            if !(0..=MAX_DIM as i64).contains(&sym_w) {
                return Err(bad("a symbol width that is impossible"));
            }
            total_w += sym_w;
            pixels += sym_w as u64 * height as u64;
            if pixels > MAX_SYMBOL_PIXELS {
                return Err(bad("a symbol dictionary with too many pixels"));
            }
            ctx.charge(cost::JB2_SYMBOL)?;
            let (w, h) = (sym_w as usize, height as usize);
            if huff && !refagg {
                class_widths.push(w);
            } else if !refagg {
                let params = GenericParams { template, tpgdon: false, at, skip: None };
                let (bitmap, complete) = generic::decode_generic(ctx, &mut mq, &mut gb, w, h, &params)?;
                if !complete {
                    return Err(bad("a symbol dictionary is cut short"));
                }
                all.push(Rc::new(bitmap));
            } else {
                let count = if huff { agg_t.decode_value(&mut r)? } else { number(iaai.decode(&mut mq))? };
                ctx.charge(cost::JB2_INT)?;
                if !(1..=MAX_AGGREGATE).contains(&count) {
                    return Err(bad("an aggregate symbol of impossible size"));
                }
                let bitmap = if count == 1 {
                    // One symbol, refined (6.5.8.2.2).
                    let (id, rdx, rdy);
                    if huff {
                        id = r.bits(code_len)? as usize;
                        rdx = b15.decode_value(&mut r)?;
                        rdy = b15.decode_value(&mut r)?;
                    } else {
                        id = decode_iaid(&mut mq, &mut ia.iaid, code_len);
                        rdx = number(ia.rdx.decode(&mut mq))?;
                        rdy = number(ia.rdy.decode(&mut mq))?;
                    }
                    let reference = all.get(id).ok_or_else(|| bad("an aggregate refers to a symbol that does not exist"))?.clone();
                    let params = RefineParams { template: rtemplate, tpgron: false, at: rat, reference: &reference, dx: rdx, dy: rdy };
                    let (bitmap, complete) = if huff {
                        let size = usize::try_from(b1.decode_value(&mut r)?).map_err(|_| bad("a refinement of impossible size"))?;
                        let at_byte = r.byte_pos();
                        let end = at_byte.checked_add(size).ok_or_else(|| bad("a refinement of impossible size"))?;
                        let slice = body.get(at_byte..end).ok_or_else(|| bad("a refinement cut off"))?;
                        let mut own = Mq::new(slice);
                        let made = generic::decode_refinement(ctx, &mut own, &mut gr, w, h, &params)?;
                        r.seek(end);
                        made
                    } else {
                        generic::decode_refinement(ctx, &mut mq, &mut gr, w, h, &params)?
                    };
                    if !complete {
                        return Err(bad("a symbol dictionary is cut short"));
                    }
                    bitmap
                } else {
                    // Several symbols put together by a text region (6.5.8.2).
                    let p = TextParams {
                        w,
                        h,
                        instances: count as u64,
                        log_strips: 0,
                        syms: &all,
                        code_len,
                        default_black: false,
                        op: Op::Or,
                        transposed: false,
                        corner: Corner::TopLeft,
                        ds_offset: 0,
                        refine: true,
                        rtemplate,
                        rat,
                    };
                    if huff {
                        let t = HuffTables {
                            fs: Table::standard(6)?,
                            ds: Table::standard(8)?,
                            dt: Table::standard(11)?,
                            rdw: b15,
                            rdh: b15,
                            rdx: b15,
                            rdy: b15,
                            rsize: b1,
                            syms: None,
                        };
                        text::decode_text(ctx, &p, &mut Src::Huff { r: &mut r, t: &t }, &mut gr)?
                    } else {
                        text::decode_text(ctx, &p, &mut Src::Arith { mq: &mut mq, ia: &mut ia }, &mut gr)?
                    }
                };
                all.push(Rc::new(bitmap));
            }
            decoded += 1;
        }
        if huff && !refagg {
            // The symbols of the class are one bitmap side by side (6.5.9).
            let size = number(Some(bm_t.decode_value(&mut r)?))?;
            ctx.charge(cost::JB2_HUFFMAN)?;
            let at_byte = r.byte_pos();
            let (tw, h) = (total_w as usize, height as usize);
            let (collective, end) = if size == 0 {
                let stride = tw.div_ceil(8);
                let end = at_byte.checked_add(stride.saturating_mul(h)).ok_or_else(|| bad("a bitmap of impossible size"))?;
                let raw = body.get(at_byte..end).ok_or_else(|| bad("an uncompressed bitmap cut off"))?;
                let mut b = ctx.bitmap(tw, h, false)?;
                for (dst, src) in b.data.chunks_exact_mut(stride.max(1)).zip(raw.chunks_exact(stride.max(1))) {
                    dst.copy_from_slice(src);
                }
                (b, end)
            } else {
                let end = at_byte.checked_add(usize::try_from(size).map_err(|_| bad("a bitmap of impossible size"))?).ok_or_else(|| bad("a bitmap of impossible size"))?;
                let slice = body.get(at_byte..end).ok_or_else(|| bad("a bitmap cut off"))?;
                let (b, _, complete) = generic::decode_mmr(ctx, slice, tw, h)?;
                if !complete {
                    return Err(bad("a symbol bitmap is damaged"));
                }
                (b, end)
            };
            r.seek(end);
            let mut x = 0i64;
            for w in class_widths {
                let mut symbol = ctx.bitmap(w, h, false)?;
                ctx.charge(h as f64 * cost::JB2_BLIT_ROW + (w * h) as f64 * cost::JB2_BLIT_PIXEL)?;
                symbol.combine(&collective, -x, 0, Op::Replace);
                x += w as i64;
                all.push(Rc::new(symbol));
            }
            ctx.release(&collective);
        }
    }
    if all.len() != total {
        return Err(bad("a symbol dictionary with fewer symbols than it says"));
    }

    // Which symbols are exported: runs of "no" and "yes" (6.5.10).
    let mut keep = vec![false; total];
    let (mut index, mut flag, mut steps) = (0usize, false, 0usize);
    while index < total {
        steps += 1;
        if steps > 2 * total + 16 {
            return Err(bad("the export flags of a symbol dictionary never end"));
        }
        ctx.charge(cost::JB2_INT)?;
        let run = if huff { b1.decode_value(&mut r)? } else { number(iaex.decode(&mut mq))? };
        if run < 0 || run as u64 > (total - index) as u64 {
            return Err(bad("an export run that is impossible"));
        }
        let run = run as usize;
        if flag && let Some(s) = keep.get_mut(index..index + run) {
            s.fill(true);
        }
        index += run;
        flag = !flag;
    }
    let exported: Vec<Rc<Bitmap>> = all.iter().zip(&keep).filter(|(_, k)| **k).map(|(b, _)| b.clone()).collect();
    ctx.give_back(scratch);
    let retained = (flags & 0x200 != 0).then_some((gb, gr));
    // What stays for the rest of the image: the list of exported symbols and the retained statistics.
    let held = exported.len() as u64 * PTR + DICT_BYTES + retained.as_ref().map_or(0, |(a, b)| (a.len() + b.len()) as u64 + 2 * VEC_HEADER);
    ctx.take_memory(held)?;
    Ok(SymbolDict { exported, retained, held })
}
