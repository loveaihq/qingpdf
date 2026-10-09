//! Test encoder, continued: symbol dictionaries and text regions (see [`super::test_enc`]).

#![allow(clippy::too_many_arguments, clippy::type_complexity, clippy::useless_vec, clippy::manual_is_multiple_of)]

use super::bitmap::{Bitmap, Op};
use super::huffman::Table;
use super::test_enc::*;

/// The contexts of the integer procedures of a text region.
pub struct TextCtx {
    pub dt: Vec<u8>,
    pub fs: Vec<u8>,
    pub ds: Vec<u8>,
    pub it: Vec<u8>,
    pub ri: Vec<u8>,
    pub rdw: Vec<u8>,
    pub rdh: Vec<u8>,
    pub rdx: Vec<u8>,
    pub rdy: Vec<u8>,
    pub iaid: Vec<u8>,
    pub gr: Vec<u8>,
}

impl TextCtx {
    pub fn new(code_len: u32) -> TextCtx {
        TextCtx {
            dt: vec![0; 512],
            fs: vec![0; 512],
            ds: vec![0; 512],
            it: vec![0; 512],
            ri: vec![0; 512],
            rdw: vec![0; 512],
            rdh: vec![0; 512],
            rdx: vec![0; 512],
            rdy: vec![0; 512],
            iaid: vec![0; 1 << (code_len + 1)],
            gr: vec![0; 1 << 13],
        }
    }
}

/// How a text region is set up (7.4.3.1.1).
#[derive(Clone)]
pub struct TextOpts {
    pub log_strips: u32,
    /// 0 bottom left, 1 top left, 2 bottom right, 3 top right.
    pub corner: u8,
    pub transposed: bool,
    pub op: Op,
    pub default_black: bool,
    pub ds_offset: i64,
    pub refine: bool,
    pub rtemplate: u8,
    pub rat: [(i8, i8); 2],
}

impl TextOpts {
    pub fn plain() -> TextOpts {
        TextOpts { log_strips: 0, corner: 1, transposed: false, op: Op::Or, default_black: false, ds_offset: 0, refine: false, rtemplate: 0, rat: [(-1, -1), (-1, -1)] }
    }

    pub fn flags(&self, huff: bool) -> u16 {
        let op = match self.op {
            Op::Or => 0,
            Op::And => 1,
            Op::Xor => 2,
            _ => 3,
        };
        u16::from(huff)
            | (u16::from(self.refine) << 1)
            | ((self.log_strips as u16) << 2)
            | (u16::from(self.corner) << 4)
            | (u16::from(self.transposed) << 6)
            | (op << 7)
            | (u16::from(self.default_black) << 9)
            | (((self.ds_offset & 0x1F) as u16) << 10)
            | (u16::from(self.rtemplate) << 15)
    }
}

/// One instance of a symbol: where its top left corner is, and, if it is refined, the bitmap it becomes and the
/// offsets of the refinement (the refined bitmap may differ in size from the symbol).
#[derive(Clone)]
pub struct Inst {
    pub id: usize,
    pub x: i64,
    pub y: i64,
    pub refined: Option<(Bitmap, i64, i64)>,
}

impl Inst {
    pub fn at(id: usize, x: i64, y: i64) -> Inst {
        Inst { id, x, y, refined: None }
    }

    fn size(&self, syms: &[Bitmap]) -> (i64, i64) {
        self.refined.as_ref().map_or((syms[self.id].w as i64, syms[self.id].h as i64), |r| (r.0.w as i64, r.0.h as i64))
    }
}

/// The (S, T) of an instance whose top left corner is at (x, y) and that is `w` by `h` (6.4.5).
pub fn st_of(o: &TextOpts, x: i64, y: i64, w: i64, h: i64) -> (i64, i64) {
    let (right, bottom) = (matches!(o.corner, 2 | 3), matches!(o.corner, 0 | 2));
    if !o.transposed {
        (x + if right { w - 1 } else { 0 }, y + if bottom { h - 1 } else { 0 })
    } else {
        (y + if bottom { h - 1 } else { 0 }, x + if right { w - 1 } else { 0 })
    }
}

/// How far CURS moves before the instance is placed, and after it (6.4.5 steps vi and x).
fn curs_moves(o: &TextOpts, w: i64, h: i64) -> (i64, i64) {
    let (left, right, top, bottom) = (matches!(o.corner, 0 | 1), matches!(o.corner, 2 | 3), matches!(o.corner, 1 | 3), matches!(o.corner, 0 | 2));
    if !o.transposed {
        (if right { w - 1 } else { 0 }, if left { w - 1 } else { 0 })
    } else {
        (if bottom { h - 1 } else { 0 }, if top { h - 1 } else { 0 })
    }
}

/// The instances in the order they are coded: by strip, then by S. (strip, S, T, index).
fn coding_order(o: &TextOpts, syms: &[Bitmap], insts: &[Inst]) -> Vec<(i64, i64, i64, usize)> {
    let strips = 1i64 << o.log_strips;
    let mut order: Vec<(i64, i64, i64, usize)> = insts
        .iter()
        .enumerate()
        .map(|(i, inst)| {
            let (w, h) = inst.size(syms);
            let (s, t) = st_of(o, inst.x, inst.y, w, h);
            (t.div_euclid(strips), s, t, i)
        })
        .collect();
    order.sort();
    order
}

/// Encode the instances of a text region with the arithmetic coder (6.4.5).
pub fn encode_instances(mq: &mut MqEnc, tc: &mut TextCtx, o: &TextOpts, syms: &[Bitmap], code_len: u32, insts: &[Inst]) {
    let strips = 1i64 << o.log_strips;
    let order = coding_order(o, syms, insts);
    enc_int(mq, &mut tc.dt, Some(0));
    let (mut cur_strip, mut firsts) = (0i64, 0i64);
    let mut i = 0;
    while i < order.len() {
        let strip = order[i].0;
        enc_int(mq, &mut tc.dt, Some(strip - cur_strip));
        cur_strip = strip;
        let mut curs = 0i64;
        let mut first = true;
        while i < order.len() && order[i].0 == strip {
            let (_, s, t, k) = order[i];
            let inst = &insts[k];
            let (w, h) = inst.size(syms);
            let (before, after) = curs_moves(o, w, h);
            let want = s - before;
            if first {
                enc_int(mq, &mut tc.fs, Some(want - firsts));
                firsts = want;
                first = false;
            } else {
                enc_int(mq, &mut tc.ds, Some(want - curs - o.ds_offset));
            }
            if strips > 1 {
                enc_int(mq, &mut tc.it, Some(t - strip * strips));
            }
            enc_iaid(mq, &mut tc.iaid, inst.id, code_len);
            if o.refine {
                enc_int(mq, &mut tc.ri, Some(i64::from(inst.refined.is_some())));
            }
            if let Some((bm, rdx, rdy)) = &inst.refined {
                let sym = &syms[inst.id];
                let (rdw, rdh) = (bm.w as i64 - sym.w as i64, bm.h as i64 - sym.h as i64);
                enc_int(mq, &mut tc.rdw, Some(rdw));
                enc_int(mq, &mut tc.rdh, Some(rdh));
                enc_int(mq, &mut tc.rdx, Some(*rdx));
                enc_int(mq, &mut tc.rdy, Some(*rdy));
                let (dx, dy) = ((rdw >> 1) + rdx, (rdh >> 1) + rdy);
                encode_refinement(mq, &mut tc.gr, bm, sym, dx, dy, o.rtemplate, false, o.rat);
            }
            curs = want + before + after;
            i += 1;
        }
        enc_int(mq, &mut tc.ds, None);
    }
}

/// What the region looks like when its instances are put on it, each at its (x, y).
pub fn expected_region(w: usize, h: usize, o: &TextOpts, syms: &[Bitmap], insts: &[Inst]) -> Bitmap {
    let mut b = Bitmap::new(w, h, o.default_black);
    // The instances go on in the order they are coded (that matters for XOR and the like).
    for (_, _, _, k) in coding_order(o, syms, insts) {
        let inst = &insts[k];
        let bm = inst.refined.as_ref().map_or(&syms[inst.id], |r| &r.0);
        b.combine(bm, inst.x, inst.y, o.op);
    }
    b
}

pub fn ceil_log2(n: usize) -> u32 {
    if n <= 1 { 0 } else { usize::BITS - (n - 1).leading_zeros() }
}

fn put_at(d: &mut Vec<u8>, at: &[(i8, i8)]) {
    for a in at {
        d.push(a.0 as u8);
        d.push(a.1 as u8);
    }
}

/// A text region segment's data, arithmetic coded.
pub fn text_region(w: usize, h: usize, x: i64, y: i64, region_op: Op, o: &TextOpts, syms: &[Bitmap], insts: &[Inst]) -> Vec<u8> {
    let mut d = region_info(w, h, x, y, region_op);
    d.extend(o.flags(false).to_be_bytes());
    if o.refine && o.rtemplate == 0 {
        put_at(&mut d, &o.rat);
    }
    d.extend((insts.len() as u32).to_be_bytes());
    let code_len = ceil_log2(syms.len());
    let mut mq = MqEnc::new();
    let mut tc = TextCtx::new(code_len);
    encode_instances(&mut mq, &mut tc, o, syms, code_len, insts);
    d.extend(mq.finish());
    d
}

/// The export flags as runs of no, yes, no, ... (6.5.10).
fn export_runs(mq: &mut MqEnc, iaex: &mut [u8], export: &[bool]) {
    let (mut i, mut flag) = (0, false);
    while i < export.len() {
        let run = export[i..].iter().take_while(|e| **e == flag).count();
        enc_int(mq, iaex, Some(run as i64));
        i += run;
        flag = !flag;
    }
}

/// A symbol dictionary segment's data (arithmetic coded, no aggregates): `new` are the new symbols, in order of
/// height; `inputs` how many symbols the dictionaries it refers to have; `export` which of inputs and new are exported.
pub fn symbol_dict(new: &[Bitmap], template: u8, at: [(i8, i8); 4], inputs: usize, export: &[bool]) -> Vec<u8> {
    assert_eq!(export.len(), inputs + new.len());
    let mut d = (u16::from(template) << 10).to_be_bytes().to_vec();
    put_at(&mut d, &at[..if template == 0 { 4 } else { 1 }]);
    d.extend((export.iter().filter(|e| **e).count() as u32).to_be_bytes());
    d.extend((new.len() as u32).to_be_bytes());
    let mut mq = MqEnc::new();
    let (mut gb, mut iadh, mut iadw, mut iaex) = (vec![0u8; 1 << 16], vec![0u8; 512], vec![0u8; 512], vec![0u8; 512]);
    let (mut i, mut cur_h) = (0, 0i64);
    while i < new.len() {
        let h = new[i].h;
        enc_int(&mut mq, &mut iadh, Some(h as i64 - cur_h));
        cur_h = h as i64;
        let mut prev_w = 0i64;
        while i < new.len() && new[i].h == h {
            enc_int(&mut mq, &mut iadw, Some(new[i].w as i64 - prev_w));
            prev_w = new[i].w as i64;
            encode_generic(&mut mq, &mut gb, &new[i], template, false, at, None);
            i += 1;
        }
        enc_int(&mut mq, &mut iadw, None);
    }
    export_runs(&mut mq, &mut iaex, export);
    d.extend(mq.finish());
    d
}

/// One new symbol of a dictionary with aggregates: made of one refined symbol or of several instances.
pub enum NewSymbol {
    /// The symbol is the bitmap, coded as a refinement of the symbol `id` (with the given RDX, RDY).
    Refined { bitmap: Bitmap, id: usize, rdx: i64, rdy: i64 },
    /// The bitmap is the instances put together (each at its x, y).
    Aggregate { bitmap: Bitmap, insts: Vec<Inst> },
}

impl NewSymbol {
    pub fn bitmap(&self) -> &Bitmap {
        match self {
            NewSymbol::Refined { bitmap, .. } | NewSymbol::Aggregate { bitmap, .. } => bitmap,
        }
    }
}

/// A symbol dictionary with refinement and aggregate coding (SDREFAGG = 1), arithmetic coded. `inputs`: the symbols
/// of the dictionaries it refers to. Every symbol is exported.
pub fn symbol_dict_agg(new: &[NewSymbol], rtemplate: u8, rat: [(i8, i8); 2], inputs: &[Bitmap], template: u8) -> Vec<u8> {
    let total = inputs.len() + new.len();
    let mut d = ((1u16 << 1) | (u16::from(template) << 10) | (u16::from(rtemplate) << 12)).to_be_bytes().to_vec();
    put_at(&mut d, &nominal_at(template)[..if template == 0 { 4 } else { 1 }]);
    if rtemplate == 0 {
        put_at(&mut d, &rat);
    }
    d.extend((total as u32).to_be_bytes());
    d.extend((new.len() as u32).to_be_bytes());
    let code_len = ceil_log2(total);
    let mut mq = MqEnc::new();
    let mut tc = TextCtx::new(code_len);
    let (mut iadh, mut iadw, mut iaex, mut iaai) = (vec![0u8; 512], vec![0u8; 512], vec![0u8; 512], vec![0u8; 512]);
    let mut all: Vec<Bitmap> = inputs.to_vec();
    let (mut i, mut cur_h) = (0, 0i64);
    while i < new.len() {
        let h = new[i].bitmap().h;
        enc_int(&mut mq, &mut iadh, Some(h as i64 - cur_h));
        cur_h = h as i64;
        let mut prev_w = 0i64;
        while i < new.len() && new[i].bitmap().h == h {
            let bm = new[i].bitmap();
            enc_int(&mut mq, &mut iadw, Some(bm.w as i64 - prev_w));
            prev_w = bm.w as i64;
            match &new[i] {
                NewSymbol::Refined { bitmap, id, rdx, rdy } => {
                    enc_int(&mut mq, &mut iaai, Some(1));
                    enc_iaid(&mut mq, &mut tc.iaid, *id, code_len);
                    enc_int(&mut mq, &mut tc.rdx, Some(*rdx));
                    enc_int(&mut mq, &mut tc.rdy, Some(*rdy));
                    encode_refinement(&mut mq, &mut tc.gr, bitmap, &all[*id], *rdx, *rdy, rtemplate, false, rat);
                }
                NewSymbol::Aggregate { insts, .. } => {
                    enc_int(&mut mq, &mut iaai, Some(insts.len() as i64));
                    let o = TextOpts { refine: true, rtemplate, rat, ..TextOpts::plain() };
                    encode_instances(&mut mq, &mut tc, &o, &all, code_len, insts);
                }
            }
            all.push(bm.clone());
            i += 1;
        }
        enc_int(&mut mq, &mut iadw, None);
    }
    export_runs(&mut mq, &mut iaex, &vec![true; total]);
    d.extend(mq.finish());
    d
}

// ---- Huffman coded -----------------------------------------------------------------------------------------

/// The Huffman table choices of a text region: selector values for FS, DS, DT, RDW, RDH, RDX, RDY, RSIZE, and the
/// custom tables the selectors of 3 (1 for RSIZE) mean, in that order.
#[derive(Clone)]
pub struct HuffSel {
    pub fs: u16,
    pub ds: u16,
    pub dt: u16,
    pub rdw: u16,
    pub rdh: u16,
    pub rdx: u16,
    pub rdy: u16,
    pub rsize: u16,
    pub custom: [Option<std::rc::Rc<Table>>; 8],
}

impl HuffSel {
    pub fn standard() -> HuffSel {
        HuffSel { fs: 0, ds: 0, dt: 0, rdw: 1, rdh: 1, rdx: 1, rdy: 1, rsize: 0, custom: Default::default() }
    }

    fn flags(&self) -> u16 {
        self.fs | (self.ds << 2) | (self.dt << 4) | (self.rdw << 6) | (self.rdh << 8) | (self.rdx << 10) | (self.rdy << 12) | (self.rsize << 14)
    }

    /// The table each selector means.
    fn tables(&self) -> [&Table; 8] {
        let std = |n: usize| Table::standard(n).expect("a standard table");
        let pick = |i: usize, sel: u16, custom_sel: u16, list: &[usize]| -> &Table {
            if sel == custom_sel {
                self.custom[i].as_deref().expect("a custom table")
            } else {
                std(list[usize::from(sel)])
            }
        };
        [
            pick(0, self.fs, 3, &[6, 7]),
            pick(1, self.ds, 3, &[8, 9, 10]),
            pick(2, self.dt, 3, &[11, 12, 13]),
            pick(3, self.rdw, 3, &[14, 15]),
            pick(4, self.rdh, 3, &[14, 15]),
            pick(5, self.rdx, 3, &[14, 15]),
            pick(6, self.rdy, 3, &[14, 15]),
            pick(7, self.rsize, 1, &[1]),
        ]
    }
}

/// The run-length coded symbol code lengths of a Huffman text region (7.4.3.1.7): every symbol gets a code of
/// `len` bits. Returns the bits to put in front of the data, and the table the lengths make.
fn symbol_code_header(w: &mut BitWriter, lengths: &[u8]) -> Table {
    // Run codes: 0..=31 for lengths themselves; all given 5 bits, which is a complete code for 32 of the 35.
    // 35 codes: lengths 5 for the first 29, 6 for the rest keep the code space exactly full (29/32 + 6/64 = 1).
    let run_lengths: Vec<u8> = (0..35).map(|i| if i < 29 { 5 } else { 6 }).collect();
    for l in &run_lengths {
        w.bits(u64::from(*l), 4);
    }
    let runs = Table::from_lengths(&run_lengths).expect("a table");
    for &l in lengths {
        w.value(&runs, Some(i64::from(l)));
    }
    w.align();
    Table::from_lengths(lengths).expect("a table")
}

/// A text region segment's data, Huffman coded with the tables `sel` chooses (all standard).
pub fn text_region_huffman(w: usize, h: usize, x: i64, y: i64, region_op: Op, o: &TextOpts, sel: &HuffSel, syms: &[Bitmap], insts: &[Inst]) -> Vec<u8> {
    let mut d = region_info(w, h, x, y, region_op);
    d.extend(o.flags(true).to_be_bytes());
    d.extend(sel.flags().to_be_bytes());
    if o.refine && o.rtemplate == 0 {
        put_at(&mut d, &o.rat);
    }
    d.extend((insts.len() as u32).to_be_bytes());
    let mut bw = BitWriter::default();
    // Every symbol has a code of the same length (or two lengths when the number is not a power of two).
    let n = syms.len();
    let len = ceil_log2(n).max(1) as u8;
    let lengths: Vec<u8> = vec![len; n];
    // A complete code is not needed; a code with unused patterns is fine.
    let sym_table = symbol_code_header(&mut bw, &lengths);
    let [fs_t, ds_t, dt_t, rdw_t, rdh_t, rdx_t, rdy_t, rsize_t] = sel.tables();
    let strips = 1i64 << o.log_strips;
    let order = coding_order(o, syms, insts);
    // The refinement statistics last for the whole region: each refinement has its own coder but not its own contexts.
    let mut gr = vec![0u8; 1 << 13];
    // The Huffman tables for the strip steps start at 1: the first value is 1 (the strip before the first).
    bw.value(dt_t, Some(1));
    let (mut cur_strip, mut firsts) = (-1i64, 0i64);
    let mut i = 0;
    while i < order.len() {
        let strip = order[i].0;
        bw.value(dt_t, Some(strip - cur_strip));
        cur_strip = strip;
        let mut curs = 0i64;
        let mut first = true;
        while i < order.len() && order[i].0 == strip {
            let (_, s, t, k) = order[i];
            let inst = &insts[k];
            let (iw, ih) = inst.size(syms);
            let (before, after) = curs_moves(o, iw, ih);
            let want = s - before;
            if first {
                bw.value(fs_t, Some(want - firsts));
                firsts = want;
                first = false;
            } else {
                bw.value(ds_t, Some(want - curs - o.ds_offset));
            }
            if strips > 1 {
                bw.bits((t - strip * strips) as u64, o.log_strips);
            }
            bw.value(&sym_table, Some(inst.id as i64));
            if o.refine {
                bw.bit(u32::from(inst.refined.is_some()));
            }
            if let Some((bm, rdx, rdy)) = &inst.refined {
                let sym = &syms[inst.id];
                let (rdw, rdh) = (bm.w as i64 - sym.w as i64, bm.h as i64 - sym.h as i64);
                bw.value(rdw_t, Some(rdw));
                bw.value(rdh_t, Some(rdh));
                bw.value(rdx_t, Some(*rdx));
                bw.value(rdy_t, Some(*rdy));
                let mut mq = MqEnc::new();
                let (dx, dy) = ((rdw >> 1) + rdx, (rdh >> 1) + rdy);
                encode_refinement(&mut mq, &mut gr, bm, sym, dx, dy, o.rtemplate, false, o.rat);
                let data = mq.finish();
                bw.value(rsize_t, Some(data.len() as i64));
                bw.put_bytes(&data);
            }
            curs = want + before + after;
            i += 1;
        }
        bw.value(ds_t, None);
    }
    d.extend(bw.bytes);
    d
}

/// A Huffman coded symbol dictionary (SDHUFF = 1, no aggregates) with the standard tables B.4 (heights), B.2 (widths)
/// and B.1 (bitmap sizes and export runs). The bitmaps of a height class are coded as one collective bitmap, not
/// compressed (BMSIZE 0) when `raw`, otherwise MMR coded by the closure.
pub fn symbol_dict_huffman(new: &[Bitmap], export: &[bool], mmr: Option<&dyn Fn(&Bitmap) -> Vec<u8>>) -> Vec<u8> {
    let inputs = export.len() - new.len();
    let mut d = 1u16.to_be_bytes().to_vec();
    d.extend((export.iter().filter(|e| **e).count() as u32).to_be_bytes());
    d.extend((new.len() as u32).to_be_bytes());
    let (b4, b2, b1) = (Table::standard(4).expect("t"), Table::standard(2).expect("t"), Table::standard(1).expect("t"));
    let mut bw = BitWriter::default();
    let (mut i, mut cur_h) = (0, 0i64);
    while i < new.len() {
        let h = new[i].h;
        bw.value(b4, Some(h as i64 - cur_h));
        cur_h = h as i64;
        let mut prev_w = 0i64;
        let start = i;
        while i < new.len() && new[i].h == h {
            bw.value(b2, Some(new[i].w as i64 - prev_w));
            prev_w = new[i].w as i64;
            i += 1;
        }
        bw.value(b2, None);
        // The collective bitmap: the symbols side by side.
        let total_w: usize = new[start..i].iter().map(|b| b.w).sum();
        let mut sheet = Bitmap::new(total_w, h, false);
        let mut x = 0;
        for b in &new[start..i] {
            sheet.combine(b, x, 0, Op::Replace);
            x += b.w as i64;
        }
        match mmr {
            Some(f) => {
                let data = f(&sheet);
                bw.value(b1, Some(data.len() as i64));
                bw.put_bytes(&data);
            }
            None => {
                bw.value(b1, Some(0));
                bw.align();
                for y in 0..h {
                    bw.put_bytes(sheet.row(y));
                }
            }
        }
    }
    // Export runs with table B.1.
    let (mut j, mut flag) = (0, false);
    while j < export.len() {
        let run = export[j..].iter().take_while(|e| **e == flag).count();
        bw.value(b1, Some(run as i64));
        j += run;
        flag = !flag;
    }
    let _ = inputs;
    d.extend(bw.bytes);
    d
}
