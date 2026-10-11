//! The inverse discrete wavelet transforms of ISO/IEC 15444-1 Annex F: the reversible 5/3 filter on integers and the
//! irreversible 9/7 filter on floats, both as lifting steps.
//!
//! A resolution's samples sit in the buffer as the standard's subbands do after 2D_DEINTERLEAVE, in the layout
//! `[LL | HL]` over `[LH | HH]`; one level of synthesis turns that into the samples of the next resolution in place.
//! Rows are done first (F.3.2: HOR_SR, then VER_SR). The lifting steps work on the low and the high half apart, so
//! that each step is a pass over two slices; columns are done sixteen at a time, a "sample" then being a lane of 16.

use std::sync::Mutex;

const LANES: usize = 16;

/// `a[j] = f(a[j], left, right)` with the neighbours of the interleaved line taken from `b`: the left neighbour of
/// `a[j]` is `b[j + off - 1]`, the right one `b[j + off]`, and a neighbour off the end is the end sample (the
/// symmetric extension of F.3.7 comes to that). `off` is 0 or 1.
fn lift<E: Copy>(a: &mut [E], b: &[E], off: usize, f: impl Fn(E, E, E) -> E) {
    let (na, nb) = (a.len(), b.len());
    // The end samples of b stand for what lies beyond it.
    let (Some(&first), Some(&last)) = (b.first(), b.last()) else { return };
    if off == 0 {
        // Left neighbour b[j-1], right neighbour b[j].
        if let Some(x) = a.first_mut() {
            *x = f(*x, first, first);
        }
        let m = na.min(nb);
        if let (Some(dst), Some(left), Some(right)) = (a.get_mut(1..m), b.get(..m.saturating_sub(1)), b.get(1..m)) {
            for ((x, &l), &r) in dst.iter_mut().zip(left).zip(right) {
                *x = f(*x, l, r);
            }
        }
        if na > nb
            && let Some(x) = a.get_mut(nb)
        {
            *x = f(*x, last, last);
        }
    } else {
        // Left neighbour b[j], right neighbour b[j+1].
        let m = na.min(nb - 1);
        if let (Some(dst), Some(left), Some(right)) = (a.get_mut(..m), b.get(..m), b.get(1..m + 1)) {
            for ((x, &l), &r) in dst.iter_mut().zip(left).zip(right) {
                *x = f(*x, l, r);
            }
        }
        if na > m
            && let Some(x) = a.get_mut(m)
        {
            *x = f(*x, last, last);
        }
    }
}

/// Which half a step updates from which: `(off for low, off for high)`. If the line starts with a low-pass sample,
/// `L[j]` lies between `H[j-1]` and `H[j]`, and `H[j]` between `L[j]` and `L[j+1]`; otherwise the other way round.
fn offsets(first_low: bool) -> (usize, usize) {
    if first_low { (0, 1) } else { (1, 0) }
}

/// The 5/3 synthesis (F.3.8.1) of a line given as its low and high halves.
fn synth53<E: Copy>(low: &mut [E], high: &mut [E], first_low: bool, s1: impl Fn(E, E, E) -> E, s2: impl Fn(E, E, E) -> E) {
    let (ol, oh) = offsets(first_low);
    lift(low, high, ol, s1);
    lift(high, low, oh, s2);
}

/// The 9/7 synthesis (F.3.8.2). `scale(x, k)` multiplies, `step(a, l, r, c)` is `a - c * (l + r)`.
fn synth97<E: Copy>(low: &mut [E], high: &mut [E], first_low: bool, scale: impl Fn(E, f32) -> E, step: impl Fn(E, E, E, f32) -> E + Copy) {
    const ALPHA: f32 = -1.586_134_3;
    const BETA: f32 = -0.052_980_12;
    const GAMMA: f32 = 0.882_911_1;
    const DELTA: f32 = 0.443_506_85;
    const K: f32 = 1.230_174_1;
    for x in low.iter_mut() {
        *x = scale(*x, K);
    }
    for x in high.iter_mut() {
        *x = scale(*x, 1.0 / K);
    }
    let (ol, oh) = offsets(first_low);
    lift(low, high, ol, |a, l, r| step(a, l, r, DELTA));
    lift(high, low, oh, |a, l, r| step(a, l, r, GAMMA));
    lift(low, high, ol, |a, l, r| step(a, l, r, BETA));
    lift(high, low, oh, |a, l, r| step(a, l, r, ALPHA));
}

fn lane_map3<T: Copy>(a: [T; LANES], l: [T; LANES], r: [T; LANES], f: impl Fn(T, T, T) -> T) -> [T; LANES] {
    let mut out = a;
    for (o, (l, r)) in out.iter_mut().zip(l.iter().zip(r.iter())) {
        *o = f(*o, *l, *r);
    }
    out
}

/// A sample type of the buffer: `i32` (5/3) or `f32` (9/7).
pub(super) trait Sample: Copy + Default + Send {
    /// Sixteen samples.
    type Lane: Copy + Default + Send + AsRef<[Self]> + AsMut<[Self]>;
    /// A single sample line that is not interleaved yet: `x` of a line of one sample that is high-pass (F.3.6).
    fn half(self) -> Self;
    fn synth(low: &mut [Self], high: &mut [Self], first_low: bool);
    fn synth_lanes(low: &mut [Self::Lane], high: &mut [Self::Lane], first_low: bool);
}

impl Sample for i32 {
    type Lane = [i32; LANES];
    fn half(self) -> i32 {
        self >> 1
    }
    fn synth(low: &mut [i32], high: &mut [i32], first_low: bool) {
        synth53(low, high, first_low, |a, l, r| a.wrapping_sub(l.wrapping_add(r).wrapping_add(2) >> 2), |a, l, r| a.wrapping_add(l.wrapping_add(r) >> 1));
    }
    fn synth_lanes(low: &mut [[i32; LANES]], high: &mut [[i32; LANES]], first_low: bool) {
        synth53(
            low,
            high,
            first_low,
            |a, l, r| lane_map3(a, l, r, |a, l, r| a.wrapping_sub(l.wrapping_add(r).wrapping_add(2) >> 2)),
            |a, l, r| lane_map3(a, l, r, |a, l, r| a.wrapping_add(l.wrapping_add(r) >> 1)),
        );
    }
}

impl Sample for f32 {
    type Lane = [f32; LANES];
    fn half(self) -> f32 {
        self * 0.5
    }
    fn synth(low: &mut [f32], high: &mut [f32], first_low: bool) {
        synth97(low, high, first_low, |x, k| x * k, |a, l, r, c| a - c * (l + r));
    }
    fn synth_lanes(low: &mut [[f32; LANES]], high: &mut [[f32; LANES]], first_low: bool) {
        synth97(
            low,
            high,
            first_low,
            |x, k| x.map(|v| v * k),
            |a, l, r, c| {
                let mut out = a;
                for (o, (l, r)) in out.iter_mut().zip(l.iter().zip(r.iter())) {
                    *o -= c * (l + r);
                }
                out
            },
        );
    }
}

/// Scratch space for [`synth_level`], reused from level to level.
pub(super) struct Scratch<S: Sample> {
    line: Vec<S>,
    low: Vec<S::Lane>,
    high: Vec<S::Lane>,
}

impl<S: Sample> Scratch<S> {
    pub fn new() -> Scratch<S> {
        Scratch { line: Vec::new(), low: Vec::new(), high: Vec::new() }
    }
}

/// The bytes of scratch space a level of `longest` samples (the longer side) needs.
pub(super) fn scratch_bytes<S: Sample>(longest: usize) -> u64 {
    let lane = std::mem::size_of::<S::Lane>() as u64;
    let own = std::mem::size_of::<S>() as u64;
    longest as u64 * (lane + own) + 4096
}

/// Where a line of the next resolution begins and what it is made of: `low` samples of the low-pass kind at the start
/// of the stored line, `n` in all, `first_low`: the first sample of the line (by its place on the grid) is a low-pass
/// one.
pub(super) struct Level {
    /// Width and height of the resolution being made, and of the low-pass part (the resolution below).
    pub w: usize,
    pub h: usize,
    pub lw: usize,
    pub lh: usize,
    /// The grid coordinates of the first sample are odd.
    pub x_odd: bool,
    pub y_odd: bool,
}

/// Run `f(job)` for each of `jobs`, each on a thread of its own but the first (which is done here); a job whose thread
/// cannot be started is done here too.
pub(super) fn run_jobs<J: Send>(jobs: Vec<J>, f: impl Fn(J) + Sync) {
    if jobs.len() <= 1 {
        jobs.into_iter().for_each(f);
        return;
    }
    let slots: Vec<Mutex<Option<J>>> = jobs.into_iter().map(|j| Mutex::new(Some(j))).collect();
    let run = |i: usize| {
        let job = slots.get(i).and_then(|s| s.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take());
        if let Some(j) = job {
            f(j);
        }
    };
    std::thread::scope(|scope| {
        for i in 1..slots.len() {
            let run = &run;
            let _ = std::thread::Builder::new().spawn_scoped(scope, move || run(i));
        }
        (0..slots.len()).for_each(&run);
    });
}

/// The rows of one level (F.3.2, HOR_SR): each is synthesised and interleaved through the scratch line.
fn rows_pass<'a, S: Sample + 'a>(rows: impl Iterator<Item = &'a mut [S]>, lv: &Level, sc: &mut Scratch<S>) {
    let w = lv.w;
    for row in rows {
        let Some(row) = row.get_mut(..w) else { return };
        if w == 1 {
            if lv.x_odd
                && let Some(x) = row.first_mut()
            {
                *x = x.half();
            }
            continue;
        }
        {
            let (low, high) = row.split_at_mut(lv.lw.min(w));
            S::synth(low, high, !lv.x_odd);
        }
        // Interleave through the scratch line.
        if sc.line.len() != w {
            sc.line.clear();
            sc.line.resize(w, S::default());
        }
        let (low, high) = row.split_at(lv.lw.min(w));
        let (first, second) = if lv.x_odd { (high, low) } else { (low, high) };
        for (pair, (&a, &b)) in sc.line.chunks_exact_mut(2).zip(first.iter().zip(second)) {
            if let [p, q] = pair {
                *p = a;
                *q = b;
            }
        }
        if w % 2 == 1
            && let (Some(o), Some(&v)) = (sc.line.last_mut(), first.get(w / 2))
        {
            *o = v;
        }
        row.copy_from_slice(&sc.line);
    }
}

/// The columns of one level (VER_SR) of a band of `cw` columns, sixteen at a time: `rows` holds the band's part of
/// every row of the level.
fn cols_pass<S: Sample>(rows: &mut [&mut [S]], cw: usize, lv: &Level, sc: &mut Scratch<S>) {
    let h = lv.h;
    let lh = lv.lh.min(h);
    let mut x0 = 0;
    while x0 < cw {
        let n = LANES.min(cw - x0);
        if h == 1 {
            if lv.y_odd
                && let Some(row) = rows.first_mut().and_then(|r| r.get_mut(x0..x0 + n))
            {
                for x in row {
                    *x = x.half();
                }
            }
            x0 += n;
            continue;
        }
        sc.low.clear();
        sc.high.clear();
        for y in 0..h {
            let mut lane = S::Lane::default();
            if let (Some(src), Some(dst)) = (rows.get(y).and_then(|r| r.get(x0..x0 + n)), lane.as_mut().get_mut(..n)) {
                dst.copy_from_slice(src);
            }
            if y < lh { sc.low.push(lane) } else { sc.high.push(lane) }
        }
        S::synth_lanes(&mut sc.low, &mut sc.high, !lv.y_odd);
        let (first, second) = if lv.y_odd { (&sc.high, &sc.low) } else { (&sc.low, &sc.high) };
        for y in 0..h {
            let src = if y % 2 == 0 { first } else { second };
            if let (Some(lane), Some(dst)) = (src.get(y / 2), rows.get_mut(y).and_then(|r| r.get_mut(x0..x0 + n)))
                && let Some(s) = lane.as_ref().get(..n)
            {
                dst.copy_from_slice(s);
            }
        }
        x0 += n;
    }
}

/// One level of synthesis in place: the resolution below (`lw` by `lh`) and the three subbands around it make the
/// `w` by `h` samples at the start of `buf`, whose rows are `stride` apart. One thread for each scratch space in `scs`
/// at most: rows are shared out among them, and then bands of columns.
pub(super) fn synth_level<S: Sample>(buf: &mut [S], stride: usize, lv: &Level, scs: &mut [Scratch<S>]) {
    let (w, h) = (lv.w, lv.h);
    if w == 0 || h == 0 || stride < w {
        return;
    }
    let threads = scs.len().min(h).min(w.div_ceil(LANES)).max(1);
    let rows_each = h.div_ceil(threads);
    let jobs: Vec<_> = buf.chunks_mut(rows_each * stride).take(threads).zip(scs.iter_mut()).enumerate().map(|(i, (chunk, sc))| (chunk, rows_each.min(h - i * rows_each), sc)).collect();
    run_jobs(jobs, |(chunk, n, sc)| rows_pass(chunk.chunks_mut(stride).take(n), lv, sc));
    // Bands of columns, a multiple of sixteen wide: every row is cut into them.
    let strips = w.div_ceil(LANES);
    let band = strips.div_ceil(threads) * LANES;
    let nbands = w.div_ceil(band);
    let mut parts: Vec<Vec<&mut [S]>> = (0..nbands).map(|_| Vec::with_capacity(h)).collect();
    for row in buf.chunks_mut(stride).take(h) {
        let Some(mut rest) = row.get_mut(..w) else { return };
        for part in &mut parts {
            let at = band.min(rest.len());
            let (head, tail) = std::mem::take(&mut rest).split_at_mut(at);
            part.push(head);
            rest = tail;
        }
    }
    let jobs: Vec<_> = parts.into_iter().zip(scs.iter_mut()).enumerate().map(|(i, (rows, sc))| (rows, band.min(w - i * band), sc)).collect();
    run_jobs(jobs, |(mut rows, cw, sc)| cols_pass(&mut rows, cw, lv, sc));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 5/3 analysis of one line (F.4.8.1), for the round trip.
    fn analyse53(x: &[i32], first_low: bool) -> (Vec<i32>, Vec<i32>) {
        let n = x.len() as i64;
        let at = |i: i64| -> i32 {
            let m = 2 * (n - 1).max(1);
            let mut j = i.rem_euclid(m);
            if j >= n {
                j = m - j;
            }
            x[j as usize]
        };
        let p0 = i64::from(!first_low);
        // Absolute parity of index i is (p0 + i) & 1; odd absolute = high.
        let mut y = vec![0i32; n as usize];
        for i in 0..n {
            if (p0 + i) % 2 == 1 {
                y[i as usize] = at(i) - ((at(i - 1) + at(i + 1)) >> 1);
            }
        }
        let yat = |i: i64| -> i32 {
            let m = 2 * (n - 1).max(1);
            let mut j = i.rem_euclid(m);
            if j >= n {
                j = m - j;
            }
            // Odd samples are the updated ones; even ones are still the originals.
            if (p0 + j) % 2 == 1 { y[j as usize] } else { x[j as usize] }
        };
        let mut low = Vec::new();
        let mut high = Vec::new();
        for i in 0..n {
            if (p0 + i) % 2 == 0 {
                low.push(x[i as usize] + ((yat(i - 1) + yat(i + 1) + 2) >> 2));
            } else {
                high.push(y[i as usize]);
            }
        }
        (low, high)
    }

    #[test]
    fn the_53_synthesis_undoes_the_analysis_for_every_length_and_parity() {
        let mut seed = 7u32;
        for n in 2..40usize {
            for first_low in [true, false] {
                let line: Vec<i32> = (0..n)
                    .map(|_| {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        ((seed >> 20) as i32) - 2048
                    })
                    .collect();
                let (mut low, mut high) = analyse53(&line, first_low);
                i32::synth(&mut low, &mut high, first_low);
                let mut out = vec![0i32; n];
                let (a, b) = if first_low { (&low, &high) } else { (&high, &low) };
                for (o, &v) in out.iter_mut().step_by(2).zip(a) {
                    *o = v;
                }
                for (o, &v) in out.iter_mut().skip(1).step_by(2).zip(b) {
                    *o = v;
                }
                assert_eq!(out, line, "n {n} first_low {first_low}");
            }
        }
    }

    #[test]
    fn a_level_of_rows_and_columns_round_trips_on_a_grid() {
        // A 2D grid of odd size and odd origin: analyse columns then rows with the 1D analysis, lay out in the
        // [LL | HL] / [LH | HH] order, and synthesise.
        let (w, h) = (13usize, 9usize);
        let grid: Vec<i32> = (0..w * h).map(|i| ((i * 37 + (i / w) * 11) % 255) as i32 - 100).collect();
        for (x_odd, y_odd) in [(false, false), (true, false), (false, true), (true, true)] {
            // Vertical analysis of each column.
            let mut cols = vec![0i32; w * h];
            let lh = (h + usize::from(!y_odd)) / 2;
            for x in 0..w {
                let col: Vec<i32> = (0..h).map(|y| grid[y * w + x]).collect();
                let (l, hgh) = analyse53(&col, !y_odd);
                for (y, v) in l.iter().chain(hgh.iter()).enumerate() {
                    cols[y * w + x] = *v;
                }
            }
            // Horizontal analysis of each row.
            let lw = (w + usize::from(!x_odd)) / 2;
            let mut out = vec![0i32; w * h];
            for y in 0..h {
                let row: Vec<i32> = (0..w).map(|x| cols[y * w + x]).collect();
                let (l, hgh) = analyse53(&row, !x_odd);
                for (x, v) in l.iter().chain(hgh.iter()).enumerate() {
                    out[y * w + x] = *v;
                }
            }
            let mut sc = [Scratch::<i32>::new(), Scratch::<i32>::new(), Scratch::<i32>::new()];
            synth_level(&mut out, w, &Level { w, h, lw, lh, x_odd, y_odd }, &mut sc);
            assert_eq!(out, grid, "x_odd {x_odd} y_odd {y_odd}");
        }
    }

    /// The 9/7 analysis of one line (F.4.8.2): the lifting steps the other way round.
    fn analyse97(x: &[f32], first_low: bool) -> (Vec<f32>, Vec<f32>) {
        let p0 = usize::from(!first_low);
        let mut low: Vec<f32> = x.iter().enumerate().filter(|(i, _)| (p0 + i) % 2 == 0).map(|(_, v)| *v).collect();
        let mut high: Vec<f32> = x.iter().enumerate().filter(|(i, _)| (p0 + i) % 2 == 1).map(|(_, v)| *v).collect();
        let (ol, oh) = offsets(first_low);
        let add = |a: f32, l: f32, r: f32, c: f32| a + c * (l + r);
        lift(&mut high, &low, oh, |a, l, r| add(a, l, r, -1.586_134_3));
        lift(&mut low, &high, ol, |a, l, r| add(a, l, r, -0.052_980_12));
        lift(&mut high, &low, oh, |a, l, r| add(a, l, r, 0.882_911_1));
        lift(&mut low, &high, ol, |a, l, r| add(a, l, r, 0.443_506_85));
        for v in &mut low {
            *v /= 1.230_174_1;
        }
        for v in &mut high {
            *v *= 1.230_174_1;
        }
        (low, high)
    }

    #[test]
    fn the_97_synthesis_undoes_the_analysis_for_every_length_and_parity() {
        for n in 2..40usize {
            for first_low in [true, false] {
                let line: Vec<f32> = (0..n).map(|i| ((i * 29 + 5) % 97) as f32 - 40.0).collect();
                let (mut low, mut high) = analyse97(&line, first_low);
                f32::synth(&mut low, &mut high, first_low);
                let mut out = vec![0f32; n];
                let (a, b) = if first_low { (&low, &high) } else { (&high, &low) };
                for (o, &v) in out.iter_mut().step_by(2).zip(a) {
                    *o = v;
                }
                for (o, &v) in out.iter_mut().skip(1).step_by(2).zip(b) {
                    *o = v;
                }
                for (a, b) in out.iter().zip(&line) {
                    assert!((a - b).abs() < 1e-3, "n {n} first_low {first_low}: {a} {b}");
                }
            }
        }
    }
}
