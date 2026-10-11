//! From a tile's chunks to its samples: the code-blocks decoded ([`super::t1`]; in parallel when there are many),
//! dequantized and put in place (E.1), the inverse wavelet transform of the resolutions kept ([`super::dwt`]), the
//! component transform (Annex G), the DC level shift and the clamping to the sample's range (G.1), and the result
//! written on the component's plane.
//!
//! The code-blocks of a component are decoded in batches by up to four threads that take them one after the other;
//! the batch's results (the magnitudes of each block) are put in place by the caller's thread, so that no thread
//! writes where another does. The work meter and the memory cap are shared by the threads ([`super::Ctx`] is `Sync`),
//! and every coding pass is charged before it is run, whichever thread runs it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use super::dwt::{self, Level, Sample, Scratch};
use super::t1::{Job, NEGATIVE, Seg, T1};
use super::tagtree::Node;
use super::tile::{Band, Block, Chunk, PBand, TileComp, TileDec};
use super::{Ctx, Plane, PlaneData, Warnings, bad};
use crate::error::Result;
use crate::render::work::cost;

const NONE: u32 = u32::MAX;
pub(super) enum Buf {
    Int(Vec<i32>),
    Float(Vec<f32>),
}

/// A tile-component reconstructed at the resolution that is kept.
pub(super) struct Recon {
    pub buf: Buf,
    pub w: usize,
    pub h: usize,
    /// Where it is on the component's grid at that resolution.
    pub x0: u64,
    pub y0: u64,
    /// Its component.
    pub comp: usize,
}

impl Recon {
    fn bytes(&self) -> u64 {
        (self.w * self.h * 4) as u64
    }
}

/// What a worker keeps between the blocks it decodes.
struct Worker {
    t1: T1,
    bytes: Vec<u8>,
    segs: Vec<(usize, usize, usize)>,
}

impl Worker {
    fn new() -> Worker {
        Worker { t1: T1::new(), bytes: Vec::new(), segs: Vec::new() }
    }
}

/// Run `f(state, i)` for `i` in `0..n` on up to `threads` threads that take the indices one after the other, each with
/// its own `init()` state; a result that `stop` says is the last makes the others stop taking jobs. The results in
/// order of `i`; `None` where the job was not run (a stop, or a worker that was lost).
pub(super) fn par_map<S, T: Send>(n: usize, threads: usize, init: impl Fn() -> S + Sync, f: impl Fn(&mut S, usize) -> T + Sync, stop: impl Fn(&T) -> bool + Sync) -> Vec<Option<T>> {
    let next = AtomicUsize::new(0);
    let work = || -> Vec<(usize, T)> {
        let mut state = init();
        let mut out = Vec::new();
        loop {
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= n {
                break;
            }
            let r = f(&mut state, i);
            let halt = stop(&r);
            out.push((i, r));
            if halt {
                next.store(n, Ordering::Relaxed);
                break;
            }
        }
        out
    };
    let mut results: Vec<Option<T>> = (0..n).map(|_| None).collect();
    let mut parts: Vec<Vec<(usize, T)>> = Vec::new();
    if threads <= 1 {
        parts.push(work());
    } else {
        std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 1..threads {
                if let Ok(h) = std::thread::Builder::new().spawn_scoped(scope, work) {
                    handles.push(h);
                }
            }
            // This thread works too.
            parts.push(work());
            for h in handles {
                if let Ok(part) = h.join() {
                    parts.push(part);
                }
            }
        });
    }
    for (i, r) in parts.into_iter().flatten() {
        if let Some(slot) = results.get_mut(i) {
            *slot = Some(r);
        }
    }
    results
}

/// The coefficients of a block, dequantized, and whether its data was damaged.
type Decoded = Option<(Coefs, bool)>;

enum Coefs {
    Int(Vec<i32>),
    Float(Vec<f32>),
}

impl TileDec<'_> {
    /// Decode the code-blocks of component `c` into a buffer (dequantized, before the wavelet transform). `None`: the
    /// component is not wanted.
    pub fn decode_blocks(&mut self, c: usize, data: &[u8], warnings: &mut Warnings) -> Result<Option<Recon>> {
        let ctx = self.ctx;
        let Some(comp) = self.comps.get(c) else { return Ok(None) };
        if !comp.need {
            return Ok(None);
        }
        let Some(top) = comp.res.get(comp.keep) else { return Ok(None) };
        let (w, h) = ((top.x1 - top.x0) as usize, (top.y1 - top.y0) as usize);
        let (x0, y0) = (u64::from(top.x0), u64::from(top.y0));
        let reversible = comp.style.reversible;
        let n = w.checked_mul(h).filter(|&n| n <= 1 << 30).unwrap_or(usize::MAX);
        let bytes = (n as u64).saturating_mul(4);
        self.take(bytes)?;
        ctx.charge(bytes as f64 * cost::JPX_CLEAR_BYTE)?;
        let mut buf = if reversible { Buf::Int(vec![0i32; n]) } else { Buf::Float(vec![0f32; n]) };
        let Some(comp) = self.comps.get(c) else { return Ok(None) };
        // The blocks that have data, in the resolutions that are kept.
        let mut refs: Vec<(&Block, &Band)> = Vec::new();
        let mut samples = 0usize;
        for r in 0..=comp.keep {
            let Some(res) = comp.res.get(r) else { continue };
            let nbands = res.bands.len();
            let precincts = res.npw as usize * res.nph as usize;
            for p in 0..precincts {
                for (bi, band) in res.bands.iter().enumerate() {
                    let Some(pb) = comp.pbands.get(res.first_pband + p * nbands + bi) else { continue };
                    let count = pb.ncw as usize * pb.nch as usize;
                    ctx.charge(count as f64 * cost::JPX_BLOCK_VISIT + cost::JPX_BLOCK_VISIT)?;
                    let first = pb.first_block as usize;
                    for blk in comp.blocks.get(first..first + count).unwrap_or(&[]) {
                        if blk.passes != 0 && blk.head != NONE {
                            refs.push((blk, band));
                            samples += (blk.x1 - blk.x0) as usize * (blk.y1 - blk.y0) as usize;
                        }
                    }
                }
            }
        }
        let threads = ctx.threads_for(samples);
        let mut damaged = 0u32;
        let this: &TileDec<'_> = self;
        if threads <= 1 {
            // One thread: each block is decoded and put in place at once.
            let mut wk = Worker::new();
            for &(blk, band) in &refs {
                match this.decode_block(comp, blk, band, data, &mut wk)? {
                    Some((coefs, dam)) => {
                        damaged += u32::from(dam);
                        place(&mut buf, w, blk, band, &coefs);
                    }
                    None => damaged += 1,
                }
            }
            ctx.charge(refs.len() as f64 * cost::JPX_BLOCK_VISIT)?;
        } else {
            // Workers decode the blocks one after the other and send them on; this thread puts them in place as they
            // come (so that no two threads write the buffer, and the placing does not wait for a batch to end).
            let next = AtomicUsize::new(0);
            let halt = AtomicBool::new(false);
            // (Blocks wait in a channel of a few places only, and what a place may hold is counted: if the decoders
            // run ahead of this thread they wait for it.)
            let places = threads * 2;
            let in_flight = (places + threads) as u64 * ((1u64 << (comp.style.xcb + comp.style.ycb)) * 4 + 64);
            ctx.take_memory(in_flight)?;
            let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, Result<Decoded>)>(places);
            let refs_ref = &refs;
            let (next_ref, halt_ref) = (&next, &halt);
            let mut failure: Option<crate::error::Error> = None;
            let mut received = 0usize;
            std::thread::scope(|scope| {
                let mut spawned = 0usize;
                for _ in 0..threads {
                    let tx = tx.clone();
                    let job = move || {
                        let mut wk = Worker::new();
                        loop {
                            if halt_ref.load(Ordering::Relaxed) {
                                break;
                            }
                            let i = next_ref.fetch_add(1, Ordering::Relaxed);
                            let Some(&(blk, band)) = refs_ref.get(i) else { break };
                            let r = this.decode_block(comp, blk, band, data, &mut wk);
                            if r.is_err() {
                                halt_ref.store(true, Ordering::Relaxed);
                            }
                            if tx.send((i, r)).is_err() {
                                break;
                            }
                        }
                    };
                    if std::thread::Builder::new().spawn_scoped(scope, job).is_ok() {
                        spawned += 1;
                    }
                }
                drop(tx);
                if spawned == 0 {
                    failure = Some(bad("no thread could be started"));
                }
                for (i, result) in rx {
                    received += 1;
                    let Some(&(blk, band)) = refs.get(i) else { continue };
                    match result {
                        Ok(Some((coefs, dam))) => {
                            damaged += u32::from(dam);
                            place(&mut buf, w, blk, band, &coefs);
                        }
                        Ok(None) => damaged += 1,
                        Err(e) => {
                            if failure.is_none() {
                                failure = Some(e);
                            }
                        }
                    }
                }
            });
            ctx.give_back(in_flight);
            if let Some(e) = failure {
                return Err(e);
            }
            // (A worker that was lost without an error would leave blocks undecoded: the count says so.)
            if received != refs.len() {
                return Err(bad("a worker was lost"));
            }
            ctx.charge(refs.len() as f64 * cost::JPX_BLOCK_VISIT)?;
        }
        if damaged > 0 {
            warnings.push(format!("{damaged} code-blocks of a tile are damaged"));
        }
        Ok(Some(Recon { buf, w, h, x0, y0, comp: c }))
    }

    /// One code-block: its bytes gathered by codeword segment and decoded. `Ok(None)`: it cannot be (it has more zero
    /// bit-planes than the subband has planes).
    fn decode_block(&self, comp: &TileComp, blk: &Block, band: &Band, data: &[u8], wk: &mut Worker) -> Result<Decoded> {
        let ctx = self.ctx;
        let (bw, bh) = ((blk.x1 - blk.x0) as usize, (blk.y1 - blk.y0) as usize);
        let top_plane = band.planes as i32 - 1 - i32::from(blk.zbp);
        if top_plane < 0 || bw == 0 || bh == 0 {
            return Ok(None);
        }
        wk.bytes.clear();
        wk.segs.clear();
        let mut id = blk.head;
        let mut cur_seg: Option<u8> = None;
        let mut guard = 0usize;
        while id != NONE && guard <= self.chunks.len() {
            guard += 1;
            let Some(ch): Option<&Chunk> = self.chunks.get(id as usize) else { break };
            ctx.charge(f64::from(ch.len) * cost::JPX_GATHER_BYTE + 10.0)?;
            if cur_seg != Some(ch.seg) {
                cur_seg = Some(ch.seg);
                wk.segs.push((wk.bytes.len(), wk.bytes.len(), 0));
            }
            let from = ch.off as usize;
            wk.bytes.extend_from_slice(data.get(from..from + ch.len as usize).unwrap_or(&[]));
            if let Some(s) = wk.segs.last_mut() {
                s.1 = wk.bytes.len();
                s.2 += usize::from(ch.passes);
            }
            id = ch.next;
        }
        let segs: Vec<Seg<'_>> = wk.segs.iter().map(|&(a, b, p)| Seg { data: wk.bytes.get(a..b).unwrap_or(&[]), passes: p }).collect();
        let job = Job { w: bw, h: bh, orient: band.orient, top_plane, style: comp.style.cbstyle, segs: &segs };
        let damaged = wk.t1.decode(ctx, &job)?;
        // Dequantize (E.1).
        ctx.charge(wk.t1.data.len() as f64 * cost::JPX_COEF)?;
        let roi = u32::from(comp.roi);
        let unshift = |mag: u32| -> u32 {
            if roi > 0 && (mag >> 1) >> roi.min(31) != 0 { mag >> roi.min(31) } else { mag }
        };
        let coefs = if comp.style.reversible {
            Coefs::Int(
                wk.t1
                    .data
                    .iter()
                    .map(|&s| {
                        let m = (unshift(s & !NEGATIVE) >> 1) as i32;
                        if s & NEGATIVE != 0 { -m } else { m }
                    })
                    .collect(),
            )
        } else {
            let half = band.step * 0.5;
            Coefs::Float(
                wk.t1
                    .data
                    .iter()
                    .map(|&s| {
                        let m = unshift(s & !NEGATIVE) as f32 * half;
                        if s & NEGATIVE != 0 { -m } else { m }
                    })
                    .collect(),
            )
        };
        Ok(Some((coefs, damaged)))
    }

    /// The inverse wavelet transform of reconstructions (each its own component's buffer), in parallel when there are
    /// several.
    pub fn transform(&self, recs: &mut [&mut Recon]) -> Result<()> {
        let samples: usize = recs.iter().map(|r| r.w * r.h).sum();
        let slots: Vec<Mutex<&mut Recon>> = recs.iter_mut().map(|r| Mutex::new(&mut **r)).collect();
        let threads = self.ctx.threads_for(samples).min(slots.len()).max(1);
        // What threads there are besides these go to each one's own rows and columns.
        let inner = (self.ctx.threads_for(samples) / threads).max(1);
        let results = par_map(
            slots.len(),
            threads,
            || (),
            |_, i| match slots.get(i) {
                Some(slot) => self.inverse_rec(&mut slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner), inner),
                None => Ok(()),
            },
            |r| r.is_err(),
        );
        for r in results {
            r.ok_or_else(|| bad("a worker was lost"))??;
        }
        Ok(())
    }

    fn inverse_rec(&self, rec: &mut Recon, threads: usize) -> Result<()> {
        let (c, w) = (rec.comp, rec.w);
        match &mut rec.buf {
            Buf::Int(v) => self.inverse::<i32>(c, v, w, threads),
            Buf::Float(v) => self.inverse::<f32>(c, v, w, threads),
        }
    }

    fn inverse<S: Sample>(&self, c: usize, buf: &mut [S], stride: usize, threads: usize) -> Result<()> {
        let ctx = self.ctx;
        let Some(comp) = self.comps.get(c) else { return Ok(()) };
        let mut levels = Vec::new();
        for r in 1..=comp.keep {
            let (Some(cur), Some(prev)) = (comp.res.get(r), comp.res.get(r - 1)) else { continue };
            levels.push(Level {
                w: (cur.x1 - cur.x0) as usize,
                h: (cur.y1 - cur.y0) as usize,
                lw: (prev.x1 - prev.x0) as usize,
                lh: (prev.y1 - prev.y0) as usize,
                x_odd: cur.x0 & 1 == 1,
                y_odd: cur.y0 & 1 == 1,
            });
        }
        if levels.is_empty() {
            return Ok(());
        }
        let longest = levels.iter().map(|l| l.w.max(l.h)).max().unwrap_or(0);
        let scratch_bytes = dwt::scratch_bytes::<S>(longest) * threads as u64;
        ctx.take_memory(scratch_bytes)?;
        let result = (|| {
            let mut scratch: Vec<Scratch<S>> = (0..threads).map(|_| Scratch::new()).collect();
            for lv in &levels {
                ctx.charge((lv.w as f64) * (lv.h as f64) * cost::JPX_DWT)?;
                // (Threads for the levels that are big enough to be worth them.)
                let t = if lv.w * lv.h >= super::MIN_PARALLEL_SAMPLES { threads } else { 1 };
                dwt::synth_level(buf, stride, lv, scratch.get_mut(..t).unwrap_or_default());
            }
            Ok(())
        })();
        ctx.give_back(scratch_bytes);
        result
    }

    /// Give back what a reconstruction held.
    pub fn release(&mut self, rec: &Recon) {
        let b = rec.bytes().min(self.held);
        self.held -= b;
        self.ctx.give_back(b);
    }
}

/// Put the coefficients of a block in their place in the buffer.
fn place(buf: &mut Buf, stride: usize, blk: &Block, band: &Band, coefs: &Coefs) {
    let (bw, bh) = ((blk.x1 - blk.x0) as usize, (blk.y1 - blk.y0) as usize);
    let (ox, oy) = (band.x_off + (blk.x0 - band.x0) as usize, band.y_off + (blk.y0 - band.y0) as usize);
    match (buf, coefs) {
        (Buf::Int(v), Coefs::Int(src)) => copy_rows(v, src, stride, (ox, oy), (bw, bh)),
        (Buf::Float(v), Coefs::Float(src)) => copy_rows(v, src, stride, (ox, oy), (bw, bh)),
        _ => {}
    }
}

fn copy_rows<T: Copy>(dst: &mut [T], src: &[T], stride: usize, (ox, oy): (usize, usize), (bw, bh): (usize, usize)) {
    for (y, row) in src.chunks_exact(bw).enumerate().take(bh) {
        let at = (oy + y) * stride + ox;
        if let Some(d) = dst.get_mut(at..at + bw) {
            d.copy_from_slice(row);
        }
    }
}

/// The component transform of the first three components (G.2, G.3), in place: RCT on integers, ICT on floats.
pub(super) fn component_transform(a: &mut Recon, b: &mut Recon, c: &mut Recon) -> bool {
    if a.w != b.w || a.w != c.w || a.h != b.h || a.h != c.h {
        return false;
    }
    match (&mut a.buf, &mut b.buf, &mut c.buf) {
        (Buf::Int(y0), Buf::Int(y1), Buf::Int(y2)) => {
            for ((p, q), r) in y0.iter_mut().zip(y1.iter_mut()).zip(y2.iter_mut()) {
                let green = p.wrapping_sub(q.wrapping_add(*r) >> 2);
                let red = r.wrapping_add(green);
                let blue = q.wrapping_add(green);
                *p = red;
                *q = green;
                *r = blue;
            }
            true
        }
        (Buf::Float(y0), Buf::Float(y1), Buf::Float(y2)) => {
            for ((p, q), r) in y0.iter_mut().zip(y1.iter_mut()).zip(y2.iter_mut()) {
                let (y, cb, cr) = (*p, *q, *r);
                *p = y + 1.402 * cr;
                *q = y - 0.344_13 * cb - 0.714_14 * cr;
                *r = y + 1.772 * cb;
            }
            true
        }
        _ => false,
    }
}

/// Add the DC level shift (the sample range's middle: it also makes signed samples unsigned), clamp and write on the
/// plane. `depth` is the component's bits.
pub(super) fn store(ctx: &Ctx, rec: &Recon, depth: u8, plane: &mut Plane) -> Result<()> {
    plane.ensure(ctx)?;
    ctx.charge((rec.w as f64) * (rec.h as f64) * cost::JPX_SAMPLE)?;
    let max = (1u32 << depth) - 1;
    let shift = 1i64 << (depth - 1);
    let px = rec.x0.saturating_sub(plane.x0) as usize;
    let py = rec.y0.saturating_sub(plane.y0) as usize;
    let rows = Rows { pw: plane.w, px, py, shift, max };
    match &mut plane.data {
        PlaneData::Bytes(d) => write_rows(d, rec, &rows, |v| v as u8),
        PlaneData::Words(d) => write_rows(d, rec, &rows, |v| v as u16),
    }
    Ok(())
}

/// Where a reconstruction goes on a plane and how its values are brought into range.
struct Rows {
    pw: usize,
    px: usize,
    py: usize,
    shift: i64,
    max: u32,
}

fn write_rows<P: Copy>(dst: &mut [P], rec: &Recon, rows: &Rows, conv: impl Fn(u32) -> P) {
    let width = rec.w.min(rows.pw.saturating_sub(rows.px));
    for y in 0..rec.h {
        let at = (rows.py + y) * rows.pw + rows.px;
        let Some(out) = dst.get_mut(at..at + width) else { continue };
        let from = y * rec.w;
        match &rec.buf {
            Buf::Int(v) => {
                if let Some(src) = v.get(from..from + width) {
                    for (d, &s) in out.iter_mut().zip(src) {
                        *d = conv((i64::from(s) + rows.shift).clamp(0, i64::from(rows.max)) as u32);
                    }
                }
            }
            Buf::Float(v) => {
                if let Some(src) = v.get(from..from + width) {
                    let (shift, maxf) = (rows.shift as f32 + 0.5, rows.max as f32);
                    for (d, &s) in out.iter_mut().zip(src) {
                        *d = conv((s + shift).clamp(0.0, maxf) as u32);
                    }
                }
            }
        }
    }
}

// --- the first three components of a big tile --------------------------------------------------------------------

/// Pieces a buffer is converted in (it is cut off and given back piece by piece as it goes).
const PACK_PIECES: usize = 8;
/// Rows of this many samples (a few at a time for each thread) are unpacked, transformed and written at a time.
const STORE_SAMPLES: usize = 1 << 16;
/// Floats are kept as fixed point with 2^4 to 2^12 steps to the unit (the steps are powers of two, so the samples that
/// come back are exactly what was stored): samples of 8 bits reach about 2^9 before the clamping, which leaves 2^6 or
/// more, and a step of 1/64 is a tenth of the 1/2 the final rounding to 8 bits works at.
const MIN_SCALE_BITS: u32 = 4;
const MAX_SCALE_BITS: u32 = 12;

enum Stored {
    /// Integers that fit 16 bits, as they are.
    Int(Vec<i16>),
    /// Floats times a power of two (the number is its inverse), rounded.
    Fixed(Vec<i16>, f32),
    /// Samples that do not fit: as they are.
    Full(Buf),
}

/// A reconstruction (its wavelet transform done) kept at two bytes a sample until the component transform needs it.
pub(super) struct Packed {
    w: usize,
    h: usize,
    x0: u64,
    y0: u64,
    pub comp: usize,
    store: Stored,
}

impl Packed {
    fn bytes(&self) -> u64 {
        match &self.store {
            Stored::Int(v) | Stored::Fixed(v, _) => v.len() as u64 * 2,
            Stored::Full(Buf::Int(v)) => v.len() as u64 * 4,
            Stored::Full(Buf::Float(v)) => v.len() as u64 * 4,
        }
    }

    /// Rows `y..y + n` as a reconstruction of their own.
    fn rows(&self, y: usize, n: usize) -> Recon {
        let range = y * self.w..(y + n) * self.w;
        let buf = match &self.store {
            Stored::Int(v) => Buf::Int(v.get(range).unwrap_or(&[]).iter().map(|&s| i32::from(s)).collect()),
            Stored::Fixed(v, step) => Buf::Float(v.get(range).unwrap_or(&[]).iter().map(|&s| f32::from(s) * step).collect()),
            Stored::Full(Buf::Int(v)) => Buf::Int(v.get(range).unwrap_or(&[]).to_vec()),
            Stored::Full(Buf::Float(v)) => Buf::Float(v.get(range).unwrap_or(&[]).to_vec()),
        };
        Recon { buf, w: self.w, h: n, x0: self.x0, y0: self.y0 + y as u64, comp: self.comp }
    }
}

/// `f` over `src` cut into one piece for each thread.
fn scan<T: Sync>(src: &[T], threads: usize, f: impl Fn(&[T]) + Sync) {
    let part = src.len().div_ceil(threads.max(1)).max(1);
    dwt::run_jobs(src.chunks(part).collect(), f);
}

/// `src` as 16-bit numbers, converted from the back and cut off as it goes, so that the two are never whole together
/// (the 16-bit numbers are made in memory that is not touched before it is written).
fn halve<T: Copy + Sync>(mut src: Vec<T>, threads: usize, conv: impl Fn(T) -> i16 + Sync) -> Vec<i16> {
    let n = src.len();
    let mut out = vec![0i16; n];
    let piece = n.div_ceil(PACK_PIECES).max(1);
    let mut end = n;
    while end > 0 {
        let from = end.saturating_sub(piece);
        if let (Some(s), Some(o)) = (src.get(from..end), out.get_mut(from..end)) {
            let part = s.len().div_ceil(threads.max(1)).max(1);
            dwt::run_jobs(s.chunks(part).zip(o.chunks_mut(part)).collect(), |(s, o)| {
                for (d, &x) in o.iter_mut().zip(s) {
                    *d = conv(x);
                }
            });
        }
        src.truncate(from);
        src.shrink_to_fit();
        end = from;
    }
    out
}

/// `x` rounded to the nearest integer (halves away from zero, as `f32::round` does; that function is a call without
/// SSE4.1), as 16 bits.
fn round_half_away(x: f32) -> i16 {
    (x + if x < 0.0 { -0.5 } else { 0.5 }) as i16
}

impl Recon {
    fn pack(self, threads: usize) -> Packed {
        let Recon { buf, w, h, x0, y0, comp } = self;
        let store = match buf {
            Buf::Int(v) => {
                let fits = AtomicBool::new(true);
                scan(&v, threads, |part| {
                    if !part.iter().all(|&s| i16::try_from(s).is_ok()) {
                        fits.store(false, Ordering::Relaxed);
                    }
                });
                if fits.load(Ordering::Relaxed) { Stored::Int(halve(v, threads, |s| s as i16)) } else { Stored::Full(Buf::Int(v)) }
            }
            Buf::Float(v) => {
                // (Non-negative floats are in the order of their bits.)
                let most = AtomicU32::new(0);
                scan(&v, threads, |part| {
                    let m = part.iter().fold(0f32, |m, &s| m.max(s.abs()));
                    most.fetch_max(m.to_bits(), Ordering::Relaxed);
                });
                let max = f32::from_bits(most.load(Ordering::Relaxed));
                match (MIN_SCALE_BITS..=MAX_SCALE_BITS).rev().find(|&b| max * (1u32 << b) as f32 + 1.0 < 32767.0) {
                    Some(bits) if max.is_finite() => {
                        let scale = (1u32 << bits) as f32;
                        Stored::Fixed(halve(v, threads, |s| round_half_away(s * scale)), 1.0 / scale)
                    }
                    _ => Stored::Full(Buf::Float(v)),
                }
            }
        };
        Packed { w, h, x0, y0, comp, store }
    }
}

impl TileDec<'_> {
    /// Keep a reconstruction at two bytes a sample (what is given back of its memory is given back).
    pub fn pack(&mut self, rec: Recon) -> Result<Packed> {
        self.ctx.charge((rec.w as f64) * (rec.h as f64) * cost::JPX_SAMPLE)?;
        let before = rec.bytes();
        let rec_samples = rec.w * rec.h;
        let packed = rec.pack(self.ctx.threads_for(rec_samples));
        let freed = before.saturating_sub(packed.bytes()).min(self.held);
        self.held -= freed;
        self.ctx.give_back(freed);
        Ok(packed)
    }

    /// Give back what a packed reconstruction held.
    pub fn release_packed(&mut self, p: &Packed) {
        let b = p.bytes().min(self.held);
        self.held -= b;
        self.ctx.give_back(b);
    }

    /// Let go of the structures of component `c`'s code-blocks once they are decoded.
    pub fn free_coding(&mut self, c: usize) {
        let Some(comp) = self.comps.get_mut(c) else { return };
        let bytes = comp.blocks.len() * std::mem::size_of::<Block>() + comp.nodes.len() * std::mem::size_of::<Node>() + comp.pbands.len() * std::mem::size_of::<PBand>();
        comp.blocks = Vec::new();
        comp.nodes = Vec::new();
        comp.pbands = Vec::new();
        let b = (bytes as u64).min(self.held);
        self.held -= b;
        self.ctx.give_back(b);
    }

    /// Let go of the chunks of the tile once every component is decoded.
    pub fn free_chunks(&mut self) {
        let b = ((self.chunks.len() * std::mem::size_of::<Chunk>()) as u64).min(self.held);
        self.chunks = Vec::new();
        self.held -= b;
        self.ctx.give_back(b);
    }
}

/// A band of rows of a plane, and where the samples go in it.
enum RowBand<'p> {
    Bytes(&'p mut [u8]),
    Words(&'p mut [u16]),
}

struct Target<'p> {
    band: RowBand<'p>,
    pw: usize,
    px: usize,
    shift: i64,
    max: u32,
}

/// Write packed reconstructions on their planes a few rows at a time, with the component transform when `transform`
/// (they are the first three components) and they have the same size. `Ok(false)`: they have not, or are not both
/// integers or both floats, and are written as they are.
pub(super) fn store_packed(ctx: &Ctx, packed: &[Packed], depths: &[u8], planes: &mut [Option<Plane>], transform: bool) -> Result<bool> {
    let Some(first) = packed.first() else { return Ok(true) };
    let together = transform && packed.iter().all(|p| p.w == first.w && p.h == first.h);
    if !together {
        for (p, &depth) in packed.iter().zip(depths) {
            let rows = (STORE_SAMPLES / p.w.max(1)).max(1);
            let mut y = 0usize;
            while y < p.h {
                let n = rows.min(p.h - y);
                if let Some(Some(plane)) = planes.get_mut(p.comp) {
                    store(ctx, &p.rows(y, n), depth, plane)?;
                }
                y += n;
            }
        }
        return Ok(!transform);
    }
    // Bands of rows: each is unpacked, transformed and written by whichever thread takes it, on the rows of the
    // planes that are its own.
    let rows = (STORE_SAMPLES / first.w.max(1)).max(1);
    let nbands = first.h.div_ceil(rows);
    let mut jobs: Vec<Mutex<Vec<(usize, Target<'_>)>>> = (0..nbands).map(|_| Mutex::new(Vec::new())).collect();
    for (ci, slot) in planes.iter_mut().enumerate() {
        let Some(plane) = slot.as_mut() else { continue };
        let Some((k, (p, &depth))) = packed.iter().zip(depths).enumerate().find(|(_, (p, _))| p.comp == ci) else { continue };
        plane.ensure(ctx)?;
        let (px, py) = (p.x0.saturating_sub(plane.x0) as usize, p.y0.saturating_sub(plane.y0) as usize);
        let (pw, shift, max) = (plane.w, 1i64 << (depth - 1), (1u32 << depth) - 1);
        let span = py * pw..(py + p.h) * pw;
        let bands: Vec<RowBand<'_>> = match &mut plane.data {
            PlaneData::Bytes(d) => d.get_mut(span).map(|s| s.chunks_mut(rows * pw.max(1)).map(RowBand::Bytes).collect()).unwrap_or_default(),
            PlaneData::Words(d) => d.get_mut(span).map(|s| s.chunks_mut(rows * pw.max(1)).map(RowBand::Words).collect()).unwrap_or_default(),
        };
        for (job, band) in jobs.iter_mut().zip(bands) {
            job.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).push((k, Target { band, pw, px, shift, max }));
        }
    }
    let fit = AtomicBool::new(true);
    let results = par_map(
        nbands,
        ctx.threads_for(first.w * first.h * 3),
        || (),
        |_, i| -> Result<()> {
            let Some(job) = jobs.get(i) else { return Ok(()) };
            let mut job = job.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let y = i * rows;
            let n = rows.min(first.h - y);
            ctx.charge((first.w as f64) * (n as f64) * 3.0 * cost::JPX_SAMPLE)?;
            let mut recs: Vec<Recon> = packed.iter().map(|p| p.rows(y, n)).collect();
            if let [a, b, c] = recs.as_mut_slice()
                && !component_transform(a, b, c)
            {
                fit.store(false, Ordering::Relaxed);
            }
            for (k, t) in job.iter_mut() {
                let Some(rec) = recs.get(*k) else { continue };
                let rows = Rows { pw: t.pw, px: t.px, py: 0, shift: t.shift, max: t.max };
                match &mut t.band {
                    RowBand::Bytes(d) => write_rows(d, rec, &rows, |v| v as u8),
                    RowBand::Words(d) => write_rows(d, rec, &rows, |v| v as u16),
                }
            }
            Ok(())
        },
        |r| r.is_err(),
    );
    for r in results {
        r.ok_or_else(|| bad("a worker was lost"))??;
    }
    Ok(fit.load(Ordering::Relaxed))
}
