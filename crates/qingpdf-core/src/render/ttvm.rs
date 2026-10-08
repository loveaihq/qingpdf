//! A TrueType instruction interpreter: the virtual machine of the font programs (`fpgm`, `prep`) and of the
//! glyph programs, for the few fonts that draw their strokes with it (3b2).
//!
//! Written from the Apple TrueType Reference Manual ("The TrueType instruction set", "Instructing
//! fonts") and Microsoft's OpenType specification ("TrueType instruction set", "Instructing TrueType
//! glyphs"). Coordinates are 26.6 fixed point (1/64 pixel) in `i32`; vectors are 2.14. Every number a
//! font program computes is wrapped or saturated, never trusted: a program is hostile input.
//!
//! What bounds a program: the instructions it may execute ([`Exec::fuel`], also charged for the
//! instructions skipped by `IF`/`FDEF`), the call depth, the iterations of `LOOPCALL`, the backward
//! jumps, the stack, the storage area, the twilight zone, and the number of `FDEF` and `IDEF`. An index
//! that points outside the points, the CVT or the storage area is ignored (it reads 0): fonts in the
//! wild have them, and the budgets already bound the work.
//!
//! Choices the specification leaves open (kept in `docs/decisions.md`): the distances of the original
//! outline are taken from the scaled original points (not from the unscaled ones); at the fixed large
//! size used by the caller this is exact; the twilight zone and the storage area of every glyph program
//! start as `prep` left them (a glyph does not see what another glyph wrote); `GETINFO` says version 35,
//! grayscale, not rotated, not stretched.

use std::collections::HashMap;

pub(crate) type Res<T> = Result<T, &'static str>;

/// Calls may nest this deep.
const MAX_CALL_DEPTH: usize = 64;
/// Iterations of `LOOPCALL` in one program run.
const MAX_LOOPCALLS: usize = 200_000;
/// Backward jumps (`JMPR`, `JROT`, `JROF` with a negative offset) in one program run.
const MAX_BACK_JUMPS: usize = 100_000;
/// Hard upper bounds, whatever `maxp` says.
pub(crate) const MAX_STACK: usize = 8192;
pub(crate) const MAX_STORAGE: usize = 4096;
pub(crate) const MAX_TWILIGHT: usize = 4096;
pub(crate) const MAX_FDEFS: usize = 4096;
pub(crate) const MAX_IDEFS: usize = 256;
pub(crate) const MAX_CVT: usize = 1 << 16;

const LIMIT: &str = "the instruction limit was reached";
const UNDERFLOW: &str = "the instruction stack ran empty";

// --- numbers -------------------------------------------------------------------------------------------

fn clamp32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// `a * b / c`, rounded to the nearest (halves away from zero); a zero divisor gives the largest value.
pub(crate) fn mul_div(a: i32, b: i32, c: i32) -> i32 {
    let neg = ((a < 0) != (b < 0)) != (c < 0);
    if c == 0 {
        return if neg { -i32::MAX } else { i32::MAX };
    }
    let (n, d) = (i64::from(a).abs() * i64::from(b).abs(), i64::from(c).abs());
    let q = (n + d / 2) / d;
    clamp32(if neg { -q } else { q })
}

/// `a * b / c`, truncated toward zero.
fn mul_div_trunc(a: i32, b: i32, c: i32) -> i32 {
    let neg = ((a < 0) != (b < 0)) != (c < 0);
    if c == 0 {
        return if neg { -i32::MAX } else { i32::MAX };
    }
    let q = (i64::from(a).abs() * i64::from(b).abs()) / i64::from(c).abs();
    clamp32(if neg { -q } else { q })
}

/// `a` times a 16.16 factor, rounded (the factor of the callers is below 2^31, so the product fits in 64 bits).
pub(crate) fn mul_fix(a: i32, s: i64) -> i32 {
    let neg = (a < 0) != (s < 0);
    let s = s.clamp(-(1 << 31), 1 << 31);
    let q = (i64::from(a).abs() * s.abs() + 0x8000) >> 16;
    clamp32(if neg { -q } else { q })
}

fn abs_diff(a: i32, b: i32) -> i64 {
    (i64::from(a) - i64::from(b)).abs()
}

fn pidx(v: i32) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

/// A unit vector (2.14) along (x, y); `None` for the zero vector.
fn normalize(x: i32, y: i32) -> Option<(i32, i32)> {
    if x == 0 && y == 0 {
        return None;
    }
    let (fx, fy) = (f64::from(x), f64::from(y));
    let len = fx.hypot(fy);
    Some(((fx / len * 16384.0).round() as i32, (fy / len * 16384.0).round() as i32))
}

/// Round a distance with a rounding state.
fn round_with(r: Round, d: i32) -> i32 {
    if r.off || r.period <= 0 {
        return d;
    }
    let (p, ph, th, d) = (i64::from(r.period), i64::from(r.phase), i64::from(r.threshold), i64::from(d));
    let v = if d >= 0 {
        let v = (d - ph + th) / p * p + ph;
        if v < 0 { ph } else { v }
    } else {
        let v = -((th - ph - d) / p * p) - ph;
        if v > 0 { -ph } else { v }
    };
    clamp32(v)
}

fn dot14(x: i32, y: i32, v: (i32, i32)) -> i32 {
    clamp32((i64::from(x) * i64::from(v.0) + i64::from(y) * i64::from(v.1) + 0x2000) >> 14)
}

// --- points and zones -------------------------------------------------------------------------------

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct Pt {
    pub x: i32,
    pub y: i32,
}

/// The point is on the curve (otherwise it is a control point of a quadratic curve).
pub(crate) const ON_CURVE: u8 = 1;
const TOUCH_X: u8 = 2;
const TOUCH_Y: u8 = 4;

/// A zone: the points of a glyph (zone 1) or the twilight points (zone 0).
#[derive(Clone, Default)]
pub(crate) struct Zone {
    /// Current positions, 26.6 pixels.
    pub cur: Vec<Pt>,
    /// Original positions (scaled, not fitted).
    pub org: Vec<Pt>,
    pub tags: Vec<u8>,
    /// Where each contour ends (exclusive).
    pub ends: Vec<usize>,
}

impl Zone {
    pub fn zeros(n: usize) -> Zone {
        Zone { cur: vec![Pt::default(); n], org: vec![Pt::default(); n], tags: vec![0; n], ends: Vec::new() }
    }
}

// --- the graphics state ----------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Round {
    period: i32,
    phase: i32,
    threshold: i32,
    off: bool,
}

const ROUND_GRID: Round = Round { period: 64, phase: 0, threshold: 32, off: false };

#[derive(Clone, Copy)]
pub(crate) struct Gs {
    rp: [usize; 3],
    gep: [u8; 3],
    dual: (i32, i32),
    proj: (i32, i32),
    free: (i32, i32),
    lp: u32,
    min_dist: i32,
    round: Round,
    auto_flip: bool,
    cvt_cutin: i32,
    sw_cutin: i32,
    sw_value: i32,
    delta_base: i32,
    delta_shift: i32,
    /// Bit 0: glyph programs are not run. Bit 1: glyph programs start from the default state.
    pub instruct_control: u8,
}

impl Default for Gs {
    fn default() -> Gs {
        Gs {
            rp: [0; 3],
            gep: [1; 3],
            dual: (0x4000, 0),
            proj: (0x4000, 0),
            free: (0x4000, 0),
            lp: 1,
            min_dist: 64,
            round: ROUND_GRID,
            auto_flip: true,
            cvt_cutin: 68,
            sw_cutin: 0,
            sw_value: 0,
            delta_base: 9,
            delta_shift: 3,
            instruct_control: 0,
        }
    }
}

// --- font-wide state ---------------------------------------------------------------------------------------

/// What does not change between the glyphs of a font at one size.
#[derive(Clone, Copy)]
pub(crate) struct Env {
    pub ppem: i32,
    /// Font units to 26.6 pixels, 16.16.
    pub scale: i64,
    pub stack: usize,
    pub storage: usize,
    pub twilight: usize,
    pub fdefs: usize,
    pub idefs: usize,
}

#[derive(Clone, Copy)]
struct Def {
    range: usize,
    start: usize,
}

#[derive(Clone, Default)]
struct Defs {
    /// By function number.
    funcs: Vec<Option<Def>>,
    /// How many of them are defined.
    count: usize,
    instrs: HashMap<u8, Def>,
}

/// The state `fpgm` and `prep` leave: shared by the glyph programs of a font, which each work on a copy.
pub(crate) struct Globals {
    pub env: Env,
    fpgm: Vec<u8>,
    prep: Vec<u8>,
    defs: Defs,
    cvt: Vec<i32>,
    storage: Vec<i32>,
    twilight: Zone,
    gs: Gs,
}

impl Globals {
    /// Run `fpgm` and then `prep`. `cvt` holds the font's control values in font units. `fuel` is the
    /// number of instructions both may run; what is left is returned in it.
    pub fn setup(fpgm: &[u8], prep: &[u8], cvt: &[i16], env: Env, fuel: &mut usize) -> Res<Globals> {
        let scaled: Vec<i32> = cvt.iter().take(MAX_CVT).map(|&v| mul_fix(i32::from(v), env.scale)).collect();
        let mut ex = Exec::blank(env);
        ex.storage = vec![0; env.storage];
        ex.zones[0] = Zone::zeros(env.twilight);
        ex.cvt = Some(scaled);
        ex.fuel = *fuel;
        ex.allow_defs = true;
        let (f, p): (&[u8], &[u8]) = (fpgm, prep);
        ex.code = [f, p, &[]];
        let done = ex.run(0).and_then(|()| {
            // The state `prep` starts from is the default one; what it leaves becomes the glyphs' default (less the vectors,
            // reference points and zones, which every program starts afresh).
            ex.gs = Gs::default();
            ex.in_prep = true;
            ex.run(1)
        });
        *fuel = ex.fuel;
        done?;
        let mut gs = ex.gs;
        gs.dual = (0x4000, 0);
        gs.proj = (0x4000, 0);
        gs.free = (0x4000, 0);
        gs.rp = [0; 3];
        gs.gep = [1; 3];
        gs.lp = 1;
        let twilight = std::mem::take(&mut ex.zones[0]);
        let (defs, cvt, storage) = (std::mem::take(&mut ex.defs), ex.cvt.take().unwrap_or_default(), std::mem::take(&mut ex.storage));
        Ok(Globals { env, fpgm: fpgm.to_vec(), prep: prep.to_vec(), defs, cvt, storage, twilight, gs })
    }

    /// Glyph programs are switched off by the font (`INSTCTRL`).
    pub fn glyph_programs_off(&self) -> bool {
        self.gs.instruct_control & 1 != 0
    }

    /// Run the program of a glyph over `zone` (zone 1; the last four points are the phantom points) and
    /// give the zone back. `scale` is the factor from font units to 26.6 pixels the program sees (that of
    /// the font, or 1.0 for a composite glyph, whose points are already pixels). `fuel` is what the
    /// program may run; what is left comes back in it.
    pub fn run_glyph(&self, code: &[u8], zone: Zone, scale: i64, fuel: &mut usize) -> Res<Zone> {
        let mut ex = Exec::blank(Env { scale, ..self.env });
        ex.defs_shared = Some(&self.defs);
        ex.base_cvt = &self.cvt;
        ex.cvt = None;
        ex.storage = self.storage.clone();
        ex.zones = [self.twilight.clone(), zone];
        ex.gs = if self.gs.instruct_control & 2 != 0 { Gs { instruct_control: self.gs.instruct_control, ..Gs::default() } } else { self.gs };
        ex.update();
        ex.code = [&self.fpgm, &self.prep, code];
        ex.fuel = *fuel;
        let done = ex.run(2);
        *fuel = ex.fuel;
        done?;
        Ok(std::mem::take(&mut ex.zones[1]))
    }
}

// --- the machine ---------------------------------------------------------------------------------------------

struct Frame {
    ret_range: usize,
    ret_ip: usize,
    def: Def,
    count: usize,
}

struct Exec<'a> {
    env: Env,
    /// The three code ranges: `fpgm`, `prep`, the glyph program.
    code: [&'a [u8]; 3],
    defs: Defs,
    defs_shared: Option<&'a Defs>,
    base_cvt: &'a [i32],
    /// The control values when this run changes them (copied from `base_cvt` at the first write).
    cvt: Option<Vec<i32>>,
    storage: Vec<i32>,
    zones: [Zone; 2],
    gs: Gs,
    f_dot_p: i32,
    stack: Vec<i32>,
    calls: Vec<Frame>,
    range: usize,
    ip: usize,
    fuel: usize,
    back_jumps: usize,
    loopcalls: usize,
    allow_defs: bool,
    in_prep: bool,
}

/// The length in bytes of the instruction at `at` (pushes carry their data).
fn ins_len(code: &[u8], at: usize) -> usize {
    let n = |extra: usize, unit: usize| code.get(at + 1).map_or(1, |&c| 2usize.saturating_add(usize::from(c).saturating_mul(unit)).saturating_add(extra));
    match code.get(at) {
        Some(0x40) => n(0, 1),
        Some(0x41) => n(0, 2),
        Some(&op @ 0xB0..=0xB7) => 2 + usize::from(op - 0xB0),
        Some(&op @ 0xB8..=0xBF) => 1 + 2 * (usize::from(op - 0xB8) + 1),
        _ => 1,
    }
}

impl<'a> Exec<'a> {
    fn blank(env: Env) -> Exec<'a> {
        Exec {
            env,
            code: [&[], &[], &[]],
            defs: Defs::default(),
            defs_shared: None,
            base_cvt: &[],
            cvt: None,
            storage: Vec::new(),
            zones: [Zone::default(), Zone::default()],
            gs: Gs::default(),
            f_dot_p: 0x4000,
            stack: Vec::with_capacity(64),
            calls: Vec::with_capacity(8),
            range: 0,
            ip: 0,
            fuel: 0,
            back_jumps: 0,
            loopcalls: 0,
            allow_defs: false,
            in_prep: false,
        }
    }

    /// Execute code range `range` (0 `fpgm`, 1 `prep`, 2 the glyph program) to its end.
    fn run(&mut self, range: usize) -> Res<()> {
        self.range = range;
        self.ip = 0;
        self.calls.clear();
        self.stack.clear();
        self.back_jumps = 0;
        self.loopcalls = 0;
        self.update();
        loop {
            let code = self.code.get(self.range).copied().unwrap_or(&[]);
            if self.ip >= code.len() {
                return if self.calls.is_empty() { Ok(()) } else { Err("a function ran past the end of its code") };
            }
            self.step(code)?;
        }
    }

    // --- the stack ---

    fn pop(&mut self) -> Res<i32> {
        self.stack.pop().ok_or(UNDERFLOW)
    }

    /// The two top values, deeper first.
    fn pop2(&mut self) -> Res<(i32, i32)> {
        let b = self.pop()?;
        let a = self.pop()?;
        Ok((a, b))
    }

    fn push(&mut self, v: i32) -> Res<()> {
        if self.stack.len() >= self.env.stack {
            return Err("the instruction stack overflowed");
        }
        self.stack.push(v);
        Ok(())
    }

    /// Replace the two top values (a deeper, b on top) by `f(a, b)`.
    fn bin(&mut self, f: impl FnOnce(i32, i32) -> Res<i32>) -> Res<()> {
        let b = self.stack.pop().ok_or(UNDERFLOW)?;
        let a = self.stack.last_mut().ok_or(UNDERFLOW)?;
        *a = f(*a, b)?;
        Ok(())
    }

    /// Replace the top value by `f(value)`.
    fn un(&mut self, f: impl FnOnce(i32) -> i32) -> Res<()> {
        let a = self.stack.last_mut().ok_or(UNDERFLOW)?;
        *a = f(*a);
        Ok(())
    }

    fn spend(&mut self, n: usize) -> Res<()> {
        self.fuel = self.fuel.checked_sub(n).ok_or(LIMIT)?;
        Ok(())
    }

    // --- vectors, rounding ---

    fn update(&mut self) {
        let (p, f) = (self.gs.proj, self.gs.free);
        let mut d = if f.0 == 0x4000 {
            p.0
        } else if f.1 == 0x4000 {
            p.1
        } else {
            ((i64::from(p.0) * i64::from(f.0) + i64::from(p.1) * i64::from(f.1)) >> 14) as i32
        };
        if d.unsigned_abs() < 0x400 {
            d = 0x4000;
        }
        self.f_dot_p = d;
    }

    fn project(&self, x: i32, y: i32) -> i32 {
        dot14(x, y, self.gs.proj)
    }

    fn dual_project(&self, x: i32, y: i32) -> i32 {
        dot14(x, y, self.gs.dual)
    }

    fn round_val(&self, d: i32) -> i32 {
        round_with(self.gs.round, d)
    }

    fn set_round(&mut self, period: i32, phase: i32, threshold: i32) {
        self.gs.round = Round { period, phase, threshold, off: false };
    }

    /// `SROUND` and `S45ROUND`: the period, phase and threshold from a selector byte.
    fn super_round(&mut self, selector: i32, grid: i32) {
        let period = match (selector >> 6) & 3 {
            0 => grid / 2,
            2 => grid * 2,
            _ => grid,
        };
        let phase = match (selector >> 4) & 3 {
            0 => 0,
            1 => period / 4,
            2 => period / 2,
            _ => period * 3 / 4,
        };
        let low = selector & 15;
        let threshold = if low == 0 { period - 1 } else { (low - 4) * period / 8 };
        self.set_round(period, phase, threshold);
    }

    // --- points ---

    fn zone(&self, id: u8) -> &Zone {
        if id == 0 { &self.zones[0] } else { &self.zones[1] }
    }

    fn zone_mut(&mut self, id: u8) -> &mut Zone {
        if id == 0 { &mut self.zones[0] } else { &mut self.zones[1] }
    }

    fn cur_pt(&self, zone: u8, p: usize) -> Option<Pt> {
        self.zone(zone).cur.get(p).copied()
    }

    fn org_pt(&self, zone: u8, p: usize) -> Option<Pt> {
        self.zone(zone).org.get(p).copied()
    }

    /// Move a point along the freedom vector so that its projection changes by `dist`; it is touched.
    fn move_pt(&mut self, zone: u8, p: usize, dist: i32) {
        let (free, fd) = (self.gs.free, self.f_dot_p);
        let z = self.zone_mut(zone);
        if let (Some(c), Some(t)) = (z.cur.get_mut(p), z.tags.get_mut(p)) {
            if free.0 != 0 {
                c.x = c.x.saturating_add(mul_div(dist, free.0, fd));
                *t |= TOUCH_X;
            }
            if free.1 != 0 {
                c.y = c.y.saturating_add(mul_div(dist, free.1, fd));
                *t |= TOUCH_Y;
            }
        }
    }

    /// The same for the original position (no touch).
    fn move_org(&mut self, zone: u8, p: usize, dist: i32) {
        let (free, fd) = (self.gs.free, self.f_dot_p);
        if let Some(o) = self.zone_mut(zone).org.get_mut(p) {
            if free.0 != 0 {
                o.x = o.x.saturating_add(mul_div(dist, free.0, fd));
            }
            if free.1 != 0 {
                o.y = o.y.saturating_add(mul_div(dist, free.1, fd));
            }
        }
    }

    /// Shift a point by (dx, dy) in the directions of the freedom vector.
    fn shift_pt(&mut self, zone: u8, p: usize, dx: i32, dy: i32, touch: bool) {
        let free = self.gs.free;
        let z = self.zone_mut(zone);
        if let (Some(c), Some(t)) = (z.cur.get_mut(p), z.tags.get_mut(p)) {
            if free.0 != 0 {
                c.x = c.x.saturating_add(dx);
                if touch {
                    *t |= TOUCH_X;
                }
            }
            if free.1 != 0 {
                c.y = c.y.saturating_add(dy);
                if touch {
                    *t |= TOUCH_Y;
                }
            }
        }
    }

    // --- the control values ---

    fn cvt_get(&self, i: i32) -> Option<i32> {
        let i = usize::try_from(i).ok()?;
        match &self.cvt {
            Some(v) => v.get(i).copied(),
            None => self.base_cvt.get(i).copied(),
        }
    }

    /// Write a control value. The first write of a glyph program copies the whole table, which is charged as work.
    fn cvt_set(&mut self, i: i32, value: i32) -> Res<()> {
        let Ok(i) = usize::try_from(i) else { return Ok(()) };
        if self.cvt.is_none() {
            self.spend(self.base_cvt.len() / 8)?;
            self.cvt = Some(self.base_cvt.to_vec());
        }
        if let Some(slot) = self.cvt.as_mut().and_then(|v| v.get_mut(i)) {
            *slot = value;
        }
        Ok(())
    }

    // --- code navigation ---

    fn jump(&mut self, at: usize, off: i32, len: usize) -> Res<()> {
        if off == 0 {
            return Err("a jump to itself");
        }
        let target = i64::try_from(at).unwrap_or(0).saturating_add(i64::from(off));
        if target < 0 || target > i64::try_from(len).unwrap_or(0) {
            return Err("a jump out of the code");
        }
        if off < 0 {
            self.back_jumps += 1;
            if self.back_jumps > MAX_BACK_JUMPS {
                return Err("too many backward jumps");
            }
        }
        self.ip = usize::try_from(target).map_err(|_| "a jump out of the code")?;
        Ok(())
    }

    /// After a false `IF` (stop at the matching `ELSE` or `EIF`) or a true branch's `ELSE` (stop at `EIF`).
    fn skip_branch(&mut self, code: &[u8], stop_at_else: bool) -> Res<()> {
        let mut nest = 1u32;
        let mut i = self.ip;
        loop {
            let op = *code.get(i).ok_or("an IF has no EIF")?;
            self.spend(1)?;
            i = i.saturating_add(ins_len(code, i));
            match op {
                0x58 => nest += 1,
                0x1B if nest == 1 && stop_at_else => break,
                0x59 => {
                    nest -= 1;
                    if nest == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        self.ip = i;
        Ok(())
    }

    /// The `ENDF` that closes the definition starting at the current position.
    fn find_endf(&mut self, code: &[u8]) -> Res<usize> {
        let mut i = self.ip;
        loop {
            let op = *code.get(i).ok_or("a definition has no ENDF")?;
            self.spend(1)?;
            match op {
                0x2D => return Ok(i),
                0x2C | 0x89 => return Err("a definition inside a definition"),
                _ => {}
            }
            i = i.saturating_add(ins_len(code, i));
        }
    }

    fn call(&mut self, def: Def, count: usize) -> Res<()> {
        if self.calls.len() >= MAX_CALL_DEPTH {
            return Err("calls nested too deep");
        }
        self.calls.push(Frame { ret_range: self.range, ret_ip: self.ip, def, count });
        self.range = def.range;
        self.ip = def.start;
        Ok(())
    }

    fn func(&self, n: i32) -> Option<Def> {
        let n = usize::try_from(n).ok()?;
        let own = self.defs.funcs.get(n).copied().flatten();
        own.or_else(|| self.defs_shared.and_then(|d| d.funcs.get(n).copied().flatten()))
    }

    fn instr_def(&self, op: u8) -> Option<Def> {
        self.defs.instrs.get(&op).or_else(|| self.defs_shared.and_then(|d| d.instrs.get(&op))).copied()
    }

    // --- one instruction ---

    fn step(&mut self, code: &'a [u8]) -> Res<()> {
        let at = self.ip;
        let op = *code.get(at).ok_or("the code ended")?;
        self.spend(1)?;
        self.ip = at + 1;
        match op {
            // SVTCA, SPVTCA, SFVTCA: the vectors along an axis (odd: x, even: y)
            0x00..=0x05 => {
                let v = if op & 1 != 0 { (0x4000, 0) } else { (0, 0x4000) };
                if op < 4 {
                    self.gs.proj = v;
                    self.gs.dual = v;
                }
                if !(2..4).contains(&op) {
                    self.gs.free = v;
                }
                self.update();
            }
            // SPVTL, SFVTL: along (or at a right angle to) the line through two points
            0x06..=0x09 => {
                let (deep, top) = self.pop2()?;
                if let Some(v) = self.line_vector(top, deep, op & 1 != 0, false) {
                    if op < 8 {
                        self.gs.proj = v;
                        self.gs.dual = v;
                    } else {
                        self.gs.free = v;
                    }
                    self.update();
                }
            }
            // SPVFS, SFVFS
            0x0A | 0x0B => {
                let (x, y) = self.pop2()?;
                if let Some(v) = normalize(i32::from(x as i16), i32::from(y as i16)) {
                    if op == 0x0A {
                        self.gs.proj = v;
                        self.gs.dual = v;
                    } else {
                        self.gs.free = v;
                    }
                    self.update();
                }
            }
            // GPV, GFV
            0x0C | 0x0D => {
                let v = if op == 0x0C { self.gs.proj } else { self.gs.free };
                self.push(v.0)?;
                self.push(v.1)?;
            }
            // SFVTPV
            0x0E => {
                self.gs.free = self.gs.proj;
                self.update();
            }
            0x0F => self.isect()?,
            // SRP0, SRP1, SRP2
            0x10..=0x12 => {
                let v = pidx(self.pop()?);
                match op {
                    0x10 => self.gs.rp[0] = v,
                    0x11 => self.gs.rp[1] = v,
                    _ => self.gs.rp[2] = v,
                }
            }
            // SZP0, SZP1, SZP2, SZPS
            0x13..=0x16 => {
                let z = match self.pop()? {
                    0 => 0,
                    1 => 1,
                    _ => return Err("a zone that is not 0 or 1"),
                };
                match op {
                    0x13 => self.gs.gep[0] = z,
                    0x14 => self.gs.gep[1] = z,
                    0x15 => self.gs.gep[2] = z,
                    _ => self.gs.gep = [z; 3],
                }
            }
            // SLOOP
            0x17 => {
                let v = self.pop()?;
                if v < 0 {
                    return Err("a negative loop count");
                }
                self.gs.lp = v.min(0xFFFF) as u32;
            }
            0x18 => self.gs.round = ROUND_GRID,                       // RTG
            0x19 => self.set_round(64, 32, 32),                      // RTHG
            0x1A => self.gs.min_dist = self.pop()?,                  // SMD
            0x1B => self.skip_branch(code, false)?,                  // ELSE (the true branch ended)
            0x1C => {
                let off = self.pop()?;
                self.jump(at, off, code.len())?;
            }
            0x1D => self.gs.cvt_cutin = self.pop()?,                 // SCVTCI
            0x1E => self.gs.sw_cutin = self.pop()?,                  // SSWCI
            0x1F => {
                let v = self.pop()?;
                self.gs.sw_value = mul_fix(v, self.env.scale);       // SSW (font units)
            }
            // DUP, POP, CLEAR, SWAP, DEPTH, CINDEX, MINDEX
            0x20 => {
                let v = *self.stack.last().ok_or(UNDERFLOW)?;
                self.push(v)?;
            }
            0x21 => {
                self.pop()?;
            }
            0x22 => self.stack.clear(),
            0x23 => {
                let n = self.stack.len();
                if n < 2 {
                    return Err(UNDERFLOW);
                }
                self.stack.swap(n - 1, n - 2);
            }
            0x24 => {
                let n = i32::try_from(self.stack.len()).unwrap_or(i32::MAX);
                self.push(n)?;
            }
            0x25 | 0x26 => {
                let k = usize::try_from(self.pop()?).map_err(|_| "a bad stack index")?;
                let i = self.stack.len().checked_sub(k).filter(|_| k > 0).ok_or("a bad stack index")?;
                if op == 0x25 {
                    let v = *self.stack.get(i).ok_or("a bad stack index")?;
                    self.push(v)?;
                } else if i < self.stack.len() {
                    let v = self.stack.remove(i);
                    self.stack.push(v);
                }
            }
            0x27 => self.alignpts()?,
            0x29 => {
                // UTP: the point can be moved again along the axes of the freedom vector
                let p = pidx(self.pop()?);
                let (free, z) = (self.gs.free, self.gs.gep[0]);
                if let Some(t) = self.zone_mut(z).tags.get_mut(p) {
                    if free.0 != 0 {
                        *t &= !TOUCH_X;
                    }
                    if free.1 != 0 {
                        *t &= !TOUCH_Y;
                    }
                }
            }
            // LOOPCALL, CALL
            0x2A | 0x2B => {
                let (count, f) = if op == 0x2A { self.pop2()? } else { (1, self.pop()?) };
                let def = self.func(f).ok_or("a call of an undefined function")?;
                if count > 0 {
                    let count = usize::try_from(count).unwrap_or(usize::MAX);
                    self.loopcalls = self.loopcalls.saturating_add(count);
                    if self.loopcalls > MAX_LOOPCALLS {
                        return Err("too many LOOPCALL iterations");
                    }
                    self.call(def, count)?;
                }
            }
            // FDEF, IDEF
            0x2C | 0x89 => {
                if !self.allow_defs {
                    return Err("a definition in a glyph program");
                }
                let n = self.pop()?;
                let end = self.find_endf(code)?;
                let def = Def { range: self.range, start: self.ip };
                if op == 0x2C {
                    let key = usize::try_from(n).ok().filter(|&k| k < MAX_FDEFS).ok_or("a bad function number")?;
                    if self.defs.funcs.len() <= key {
                        self.defs.funcs.resize(key + 1, None);
                    }
                    let slot = self.defs.funcs.get_mut(key).ok_or("a bad function number")?;
                    if slot.is_none() {
                        if self.defs.count >= self.env.fdefs {
                            return Err("too many function definitions");
                        }
                        self.defs.count += 1;
                    }
                    *slot = Some(def);
                } else {
                    let key = u8::try_from(n).map_err(|_| "a bad instruction number")?;
                    if !self.defs.instrs.contains_key(&key) && self.defs.instrs.len() >= self.env.idefs {
                        return Err("too many instruction definitions");
                    }
                    self.defs.instrs.insert(key, def);
                }
                self.ip = end + 1;
            }
            // ENDF
            0x2D => {
                let frame = self.calls.last_mut().ok_or("ENDF outside a function")?;
                if frame.count > 1 {
                    frame.count -= 1;
                    self.ip = frame.def.start;
                    self.range = frame.def.range;
                } else if let Some(f) = self.calls.pop() {
                    self.range = f.ret_range;
                    self.ip = f.ret_ip;
                }
            }
            // MDAP
            0x2E | 0x2F => {
                let p = pidx(self.pop()?);
                let z0 = self.gs.gep[0];
                if let Some(c) = self.cur_pt(z0, p) {
                    let d = if op & 1 != 0 {
                        let cd = self.project(c.x, c.y);
                        self.round_val(cd).wrapping_sub(cd)
                    } else {
                        0
                    };
                    self.move_pt(z0, p, d);
                }
                self.gs.rp[0] = p;
                self.gs.rp[1] = p;
            }
            0x30 | 0x31 => self.iup(op & 1 != 0)?,
            // SHP, SHC, SHZ, SHPIX
            0x32 | 0x33 => self.shp(op)?,
            0x34..=0x37 => self.shc_shz(op)?,
            0x38 => self.shpix()?,
            0x39 => self.ip_instr()?,
            // MSIRP
            0x3A | 0x3B => self.msirp(op)?,
            // ALIGNRP
            0x3C => {
                let n = self.loop_count()?;
                let (z0, z1, rp0) = (self.gs.gep[0], self.gs.gep[1], self.gs.rp[0]);
                for _ in 0..n {
                    let p = pidx(self.pop()?);
                    if let (Some(a), Some(b)) = (self.cur_pt(z1, p), self.cur_pt(z0, rp0)) {
                        let d = self.project(a.x.wrapping_sub(b.x), a.y.wrapping_sub(b.y));
                        self.move_pt(z1, p, d.wrapping_neg());
                    }
                }
            }
            0x3D => self.set_round(32, 0, 16),                       // RTDG
            0x3E | 0x3F => self.miap(op)?,
            // NPUSHB, NPUSHW, PUSHB[n], PUSHW[n]
            0x40 | 0x41 | 0xB0..=0xBF => {
                let words = op == 0x41 || op >= 0xB8;
                let n = match op {
                    0x40 | 0x41 => usize::from(*code.get(self.ip).ok_or("a push is cut off")?),
                    0xB0..=0xB7 => usize::from(op - 0xB0) + 1,
                    _ => usize::from(op - 0xB8) + 1,
                };
                if matches!(op, 0x40 | 0x41) {
                    self.ip += 1;
                }
                let bytes = if words { n * 2 } else { n };
                let data = code.get(self.ip..self.ip.saturating_add(bytes)).ok_or("a push is cut off")?;
                if self.stack.len() + n > self.env.stack {
                    return Err("the instruction stack overflowed");
                }
                if words {
                    self.stack.extend(data.chunks_exact(2).map(|w| match w {
                        [a, b] => i32::from(i16::from_be_bytes([*a, *b])),
                        _ => 0,
                    }));
                } else {
                    self.stack.extend(data.iter().map(|&b| i32::from(b)));
                }
                self.ip += bytes;
            }
            // WS, RS
            0x42 => {
                let (i, v) = self.pop2()?;
                if let Some(slot) = usize::try_from(i).ok().and_then(|i| self.storage.get_mut(i)) {
                    *slot = v;
                }
            }
            0x43 => {
                let storage = &self.storage;
                let top = self.stack.last_mut().ok_or(UNDERFLOW)?;
                *top = usize::try_from(*top).ok().and_then(|i| storage.get(i).copied()).unwrap_or(0);
            }
            // WCVTP, RCVT
            0x44 => {
                let (i, v) = self.pop2()?;
                self.cvt_set(i, v)?;
            }
            0x45 => {
                let i = *self.stack.last().ok_or(UNDERFLOW)?;
                let v = self.cvt_get(i).unwrap_or(0);
                self.un(|_| v)?;
            }
            // GC
            0x46 | 0x47 => {
                let p = pidx(self.pop()?);
                let z2 = self.gs.gep[2];
                let v = if op & 1 != 0 {
                    self.org_pt(z2, p).map_or(0, |o| self.dual_project(o.x, o.y))
                } else {
                    self.cur_pt(z2, p).map_or(0, |c| self.project(c.x, c.y))
                };
                self.push(v)?;
            }
            // SCFS
            0x48 => {
                let (p, v) = self.pop2()?;
                let (p, z2) = (pidx(p), self.gs.gep[2]);
                if let Some(c) = self.cur_pt(z2, p) {
                    let k = self.project(c.x, c.y);
                    self.move_pt(z2, p, v.wrapping_sub(k));
                    if z2 == 0
                        && let Some(c) = self.cur_pt(0, p)
                        && let Some(o) = self.zones[0].org.get_mut(p)
                    {
                        *o = c;
                    }
                }
            }
            // MD
            0x49 | 0x4A => {
                let (l, k) = self.pop2()?;
                let (l, k, z0, z1) = (pidx(l), pidx(k), self.gs.gep[0], self.gs.gep[1]);
                let d = if op & 1 != 0 {
                    match (self.cur_pt(z0, l), self.cur_pt(z1, k)) {
                        (Some(a), Some(b)) => self.project(a.x.wrapping_sub(b.x), a.y.wrapping_sub(b.y)),
                        _ => 0,
                    }
                } else {
                    match (self.org_pt(z0, l), self.org_pt(z1, k)) {
                        (Some(a), Some(b)) => self.dual_project(a.x.wrapping_sub(b.x), a.y.wrapping_sub(b.y)),
                        _ => 0,
                    }
                };
                self.push(d)?;
            }
            // MPPEM, MPS (the point size is the size in pixels)
            0x4B | 0x4C => self.push(self.env.ppem)?,
            0x4D => self.gs.auto_flip = true,                        // FLIPON
            0x4E => self.gs.auto_flip = false,                       // FLIPOFF
            0x4F => {
                self.pop()?;                                         // DEBUG
            }
            // LT, LTEQ, GT, GTEQ, EQ, NEQ
            0x50..=0x55 => self.bin(|a, b| {
                Ok(i32::from(match op {
                    0x50 => a < b,
                    0x51 => a <= b,
                    0x52 => a > b,
                    0x53 => a >= b,
                    0x54 => a == b,
                    _ => a != b,
                }))
            })?,
            // ODD, EVEN
            0x56 | 0x57 => {
                let v = self.pop()?;
                let r = self.round_val(v) & 127;
                self.push(i32::from(r == if op == 0x56 { 64 } else { 0 }))?;
            }
            0x58 => {
                if self.pop()? == 0 {
                    self.skip_branch(code, true)?;
                }
            }
            0x59 => {}                                               // EIF
            0x5A => self.bin(|a, b| Ok(i32::from(a != 0 && b != 0)))?,
            0x5B => self.bin(|a, b| Ok(i32::from(a != 0 || b != 0)))?,
            0x5C => self.un(|a| i32::from(a == 0))?,
            0x5D | 0x71 | 0x72 => self.deltap(op)?,
            0x5E => self.gs.delta_base = self.pop()?,                // SDB
            0x5F => {
                let v = self.pop()?;                                 // SDS
                if !(0..=6).contains(&v) {
                    return Err("a bad delta shift");
                }
                self.gs.delta_shift = v;
            }
            // arithmetic
            0x60 | 0x61 | 0x62 | 0x63 | 0x8B | 0x8C => self.bin(|a, b| match op {
                0x60 => Ok(a.wrapping_add(b)),
                0x61 => Ok(a.wrapping_sub(b)),
                0x62 => {
                    if b == 0 {
                        return Err("a division by zero");
                    }
                    Ok(mul_div_trunc(a, 64, b))
                }
                0x63 => Ok(mul_div(a, b, 64)),
                0x8B => Ok(a.max(b)),
                _ => Ok(a.min(b)),
            })?,
            0x64..=0x67 => self.un(|a| match op {
                0x64 => a.saturating_abs(),
                0x65 => a.saturating_neg(),
                0x66 => a & !63,
                _ => a.saturating_add(63) & !63,
            })?,
            // ROUND, NROUND
            0x68..=0x6B => {
                let round = self.gs.round;
                self.un(|a| round_with(round, a))?;
            }
            0x6C..=0x6F => {
                self.stack.last().ok_or(UNDERFLOW)?; // NROUND: the value stays as it is
            }
            // WCVTF (value in font units)
            0x70 => {
                let (i, v) = self.pop2()?;
                let scaled = mul_fix(v, self.env.scale);
                self.cvt_set(i, scaled)?;
            }
            0x73..=0x75 => self.deltac(op)?,
            0x76 => {
                let v = self.pop()?;                                 // SROUND
                self.super_round(v, 64);
            }
            0x77 => {
                let v = self.pop()?;                                 // S45ROUND
                self.super_round(v, 45);
            }
            0x78 | 0x79 => {
                let (off, e) = self.pop2()?;
                if (e != 0) == (op == 0x78) {
                    self.jump(at, off, code.len())?;
                }
            }
            0x7A => self.gs.round.off = true,                        // ROFF
            0x7C => self.set_round(64, 0, 63),                       // RUTG
            0x7D => self.set_round(64, 0, 0),                        // RDTG
            0x7E | 0x7F => {
                self.pop()?;                                         // SANGW, AA
            }
            // FLIPPT, FLIPRGON, FLIPRGOFF (the glyph zone)
            0x80 => {
                let n = self.loop_count()?;
                for _ in 0..n {
                    let p = pidx(self.pop()?);
                    if let Some(t) = self.zones[1].tags.get_mut(p) {
                        *t ^= ON_CURVE;
                    }
                }
            }
            0x81 | 0x82 => {
                let (a, b) = self.pop2()?;
                let (a, b) = (pidx(a), pidx(b));
                let n = self.zones[1].tags.len();
                if a <= b && b < n {
                    self.spend(b - a + 1)?;
                    for t in self.zones[1].tags.get_mut(a..=b).unwrap_or_default() {
                        if op == 0x81 { *t |= ON_CURVE } else { *t &= !ON_CURVE }
                    }
                }
            }
            0x85 | 0x8D => {
                self.pop()?;                                         // SCANCTRL, SCANTYPE
            }
            0x86 | 0x87 => {
                // SDPVTL: the dual vector from the original points, the projection vector from the current ones
                let (deep, top) = self.pop2()?;
                let dual = self.line_vector(top, deep, op & 1 != 0, true);
                let proj = self.line_vector(top, deep, op & 1 != 0, false);
                if let (Some(d), Some(p)) = (dual, proj) {
                    self.gs.dual = d;
                    self.gs.proj = p;
                    self.update();
                }
            }
            // GETINFO
            0x88 => {
                let s = self.pop()?;
                let mut r = 0;
                if s & 1 != 0 {
                    r = 35;
                }
                if s & 32 != 0 {
                    r |= 1 << 12;
                }
                self.push(r)?;
            }
            // ROLL
            0x8A => {
                let n = self.stack.len();
                if let Some([c, b, a]) = n.checked_sub(3).and_then(|k| self.stack.get_mut(k..)) {
                    (*c, *b, *a) = (*b, *a, *c);
                } else {
                    return Err(UNDERFLOW);
                }
            }
            // INSTCTRL: only the control value program may use it
            0x8E => {
                let (value, selector) = self.pop2()?;
                if self.in_prep && (1..=3).contains(&selector) {
                    let bit = 1i32 << (selector - 1);
                    if value == 0 || value == bit {
                        self.gs.instruct_control = ((i32::from(self.gs.instruct_control) & !bit) | value) as u8;
                    }
                }
            }
            // MDRP, MIRP
            0xC0..=0xDF => self.mdrp(op)?,
            0xE0..=0xFF => self.mirp(op)?,
            _ => {
                // An instruction the font defines (IDEF), else an error.
                let def = self.instr_def(op).ok_or("an unknown instruction")?;
                self.call(def, 1)?;
            }
        }
        Ok(())
    }

    // --- the longer instructions ---

    /// The unit vector along (perpendicular to) the line from the point `top` in zone `zp2` to the point `deep` in zone `zp1`.
    fn line_vector(&self, top: i32, deep: i32, perp: bool, original: bool) -> Option<(i32, i32)> {
        let (z1, z2) = (self.gs.gep[1], self.gs.gep[2]);
        let get = |z: u8, i: i32| if original { self.org_pt(z, pidx(i)) } else { self.cur_pt(z, pidx(i)) };
        let (p1, p2) = (get(z1, deep)?, get(z2, top)?);
        let (mut a, mut b) = (p1.x.wrapping_sub(p2.x), p1.y.wrapping_sub(p2.y));
        let mut perp = perp;
        if a == 0 && b == 0 {
            a = 0x4000;
            perp = false;
        }
        if perp {
            (a, b) = (b.saturating_neg(), a);
        }
        normalize(a, b)
    }

    /// `ISECT`: a point at the crossing of two lines.
    #[inline(never)]
    fn isect(&mut self) -> Res<()> {
        let b1 = pidx(self.pop()?);
        let b0 = pidx(self.pop()?);
        let a1 = pidx(self.pop()?);
        let a0 = pidx(self.pop()?);
        let p = pidx(self.pop()?);
        let (z0, z1, z2) = (self.gs.gep[0], self.gs.gep[1], self.gs.gep[2]);
        let (Some(pa0), Some(pa1), Some(pb0), Some(pb1)) = (self.cur_pt(z1, a0), self.cur_pt(z1, a1), self.cur_pt(z0, b0), self.cur_pt(z0, b1)) else { return Ok(()) };
        if p >= self.zone(z2).cur.len() {
            return Ok(());
        }
        let d = |a: i32, b: i32| i128::from(a) - i128::from(b);
        let (dbx, dby) = (d(pb1.x, pb0.x), d(pb1.y, pb0.y));
        let (dax, day) = (d(pa1.x, pa0.x), d(pa1.y, pa0.y));
        let (dx, dy) = (d(pb0.x, pa0.x), d(pb0.y, pa0.y));
        // The cross product of the lines and their dot product stand for the sine and cosine of the angle between them;
        // lines closer than about 3 degrees to parallel meet nowhere useful.
        let cross = dax * dby - day * dbx;
        let dot = dax * dbx + day * dby;
        let at = if cross != 0 && 19 * cross.abs() > dot.abs() {
            let num = dx * dby - dy * dbx;
            let off = |v: i128| {
                let (n, q) = (v * num, cross);
                let r = (n.abs() * 2 + q.abs()) / (2 * q.abs());
                clamp32(i64::try_from(if (n < 0) != (q < 0) { -r } else { r }).unwrap_or(0))
            };
            Pt { x: pa0.x.saturating_add(off(dax)), y: pa0.y.saturating_add(off(day)) }
        } else {
            let mid = |a: i32, b: i32, c: i32, e: i32| clamp32((i64::from(a) + i64::from(b) + i64::from(c) + i64::from(e)) / 4);
            Pt { x: mid(pa0.x, pa1.x, pb0.x, pb1.x), y: mid(pa0.y, pa1.y, pb0.y, pb1.y) }
        };
        let z = self.zone_mut(z2);
        if let (Some(c), Some(t)) = (z.cur.get_mut(p), z.tags.get_mut(p)) {
            *c = at;
            *t |= TOUCH_X | TOUCH_Y;
        }
        Ok(())
    }

    #[inline(never)]
    fn alignpts(&mut self) -> Res<()> {
        let (p1, p2) = self.pop2()?;
        let (p1, p2, z0, z1) = (pidx(p1), pidx(p2), self.gs.gep[0], self.gs.gep[1]);
        if let (Some(a), Some(b)) = (self.cur_pt(z1, p1), self.cur_pt(z0, p2)) {
            let d = self.project(b.x.wrapping_sub(a.x), b.y.wrapping_sub(a.y)) / 2;
            self.move_pt(z1, p1, d);
            self.move_pt(z0, p2, d.wrapping_neg());
        }
        Ok(())
    }

    /// The number of points a looping instruction works on; the loop count goes back to 1.
    fn loop_count(&mut self) -> Res<usize> {
        let n = self.gs.lp as usize;
        self.gs.lp = 1;
        if self.stack.len() < n {
            return Err(UNDERFLOW);
        }
        self.spend(n)?;
        Ok(n)
    }

    /// How far the reference point of `SHP`, `SHC` and `SHZ` has moved, along the freedom vector: (zone, point, dx, dy).
    fn displacement(&self, op: u8) -> Option<(u8, usize, i32, i32)> {
        let (z, p) = if op & 1 != 0 { (self.gs.gep[0], self.gs.rp[1]) } else { (self.gs.gep[1], self.gs.rp[2]) };
        let (c, o) = (self.cur_pt(z, p)?, self.org_pt(z, p)?);
        let d = self.project(c.x.wrapping_sub(o.x), c.y.wrapping_sub(o.y));
        Some((z, p, mul_div(d, self.gs.free.0, self.f_dot_p), mul_div(d, self.gs.free.1, self.f_dot_p)))
    }

    #[inline(never)]
    fn shp(&mut self, op: u8) -> Res<()> {
        let n = self.loop_count()?;
        let disp = self.displacement(op);
        let z2 = self.gs.gep[2];
        for _ in 0..n {
            let p = pidx(self.pop()?);
            if let Some((_, _, dx, dy)) = disp {
                self.shift_pt(z2, p, dx, dy, true);
            }
        }
        Ok(())
    }

    /// `SHC` (a contour) and `SHZ` (a zone).
    #[inline(never)]
    fn shc_shz(&mut self, op: u8) -> Res<()> {
        let arg = self.pop()?;
        let Some((rz, rp, dx, dy)) = self.displacement(op) else { return Ok(()) };
        let z2 = self.gs.gep[2];
        let (range, touch) = if op < 0x36 {
            let c = pidx(arg);
            let z = self.zone(z2);
            let range = if z2 == 0 {
                (c == 0).then_some((0, z.cur.len()))
            } else {
                let start = if c == 0 { Some(0) } else { c.checked_sub(1).and_then(|k| z.ends.get(k).copied()) };
                start.zip(z.ends.get(c).copied())
            };
            (range, true)
        } else {
            // A zone; the phantom points of the glyph zone stay where they are.
            let z = match arg {
                0 => 0u8,
                1 => 1,
                _ => return Err("a zone that is not 0 or 1"),
            };
            let zz = self.zone(z);
            (Some((0, if z == 0 { zz.cur.len() } else { zz.ends.last().copied().unwrap_or(0) })), false)
        };
        let shift_zone = if op < 0x36 { z2 } else { u8::from(arg != 0) };
        if let Some((a, b)) = range {
            self.spend(b.saturating_sub(a))?;
            for i in a..b {
                if !(shift_zone == rz && i == rp) {
                    self.shift_pt(shift_zone, i, dx, dy, touch);
                }
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn shpix(&mut self) -> Res<()> {
        let amount = self.pop()?;
        let n = self.loop_count()?;
        let (dx, dy) = (mul_div(amount, self.gs.free.0, 0x4000), mul_div(amount, self.gs.free.1, 0x4000));
        let z2 = self.gs.gep[2];
        for _ in 0..n {
            let p = pidx(self.pop()?);
            self.shift_pt(z2, p, dx, dy, true);
        }
        Ok(())
    }

    /// `IP`: points between two reference points keep their place between them.
    #[inline(never)]
    fn ip_instr(&mut self) -> Res<()> {
        let n = self.loop_count()?;
        let (z0, z1, z2) = (self.gs.gep[0], self.gs.gep[1], self.gs.gep[2]);
        let (rp1, rp2) = (self.gs.rp[1], self.gs.rp[2]);
        let refs = (self.org_pt(z0, rp1), self.org_pt(z1, rp2), self.cur_pt(z0, rp1), self.cur_pt(z1, rp2));
        let (Some(o1), Some(o2), Some(c1), Some(c2)) = refs else {
            for _ in 0..n {
                self.pop()?;
            }
            return Ok(());
        };
        let old_range = self.dual_project(o2.x.wrapping_sub(o1.x), o2.y.wrapping_sub(o1.y));
        let cur_range = self.project(c2.x.wrapping_sub(c1.x), c2.y.wrapping_sub(c1.y));
        for _ in 0..n {
            let p = pidx(self.pop()?);
            if let (Some(po), Some(pc)) = (self.org_pt(z2, p), self.cur_pt(z2, p)) {
                let org_dist = self.dual_project(po.x.wrapping_sub(o1.x), po.y.wrapping_sub(o1.y));
                let cur_dist = self.project(pc.x.wrapping_sub(c1.x), pc.y.wrapping_sub(c1.y));
                let new_dist = if org_dist == 0 {
                    0
                } else if old_range != 0 {
                    mul_div(org_dist, cur_range, old_range)
                } else {
                    org_dist
                };
                self.move_pt(z2, p, new_dist.wrapping_sub(cur_dist));
            }
        }
        Ok(())
    }

    #[inline(never)]
    fn msirp(&mut self, op: u8) -> Res<()> {
        let (p, dist) = self.pop2()?;
        let p = pidx(p);
        let (z0, z1, rp0) = (self.gs.gep[0], self.gs.gep[1], self.gs.rp[0]);
        if p < self.zone(z1).cur.len()
            && let Some(rc) = self.cur_pt(z0, rp0)
        {
            if z1 == 0 {
                // A twilight point starts at the reference point's original position and is then placed `dist` from it.
                if let Some(ro) = self.org_pt(z0, rp0)
                    && let Some(o) = self.zones[0].org.get_mut(p)
                {
                    *o = ro;
                }
                self.move_org(0, p, dist);
                if let Some(o) = self.org_pt(0, p)
                    && let Some(c) = self.zones[0].cur.get_mut(p)
                {
                    *c = o;
                }
            }
            let c = self.cur_pt(z1, p).unwrap_or_default();
            let d = self.project(c.x.wrapping_sub(rc.x), c.y.wrapping_sub(rc.y));
            self.move_pt(z1, p, dist.wrapping_sub(d));
        }
        self.gs.rp[1] = rp0;
        self.gs.rp[2] = p;
        if op & 1 != 0 {
            self.gs.rp[0] = p;
        }
        Ok(())
    }

    #[inline(never)]
    fn miap(&mut self, op: u8) -> Res<()> {
        let (p, n) = self.pop2()?;
        let (p, z0) = (pidx(p), self.gs.gep[0]);
        if let Some(mut dist) = self.cvt_get(n).filter(|_| p < self.zone(z0).cur.len()) {
            if z0 == 0 {
                let np = Pt { x: mul_div(dist, self.gs.free.0, 0x4000), y: mul_div(dist, self.gs.free.1, 0x4000) };
                let z = self.zone_mut(0);
                if let (Some(o), Some(c)) = (z.org.get_mut(p), z.cur.get_mut(p)) {
                    *o = np;
                    *c = np;
                }
            }
            let c = self.cur_pt(z0, p).unwrap_or_default();
            let org_dist = self.project(c.x, c.y);
            if op & 1 != 0 {
                if abs_diff(dist, org_dist) > i64::from(self.gs.cvt_cutin) {
                    dist = org_dist;
                }
                dist = self.round_val(dist);
            }
            self.move_pt(z0, p, dist.wrapping_sub(org_dist));
        }
        self.gs.rp[0] = p;
        self.gs.rp[1] = p;
        Ok(())
    }

    #[inline(never)]
    fn mdrp(&mut self, op: u8) -> Res<()> {
        let p = pidx(self.pop()?);
        let (z0, z1, rp0) = (self.gs.gep[0], self.gs.gep[1], self.gs.rp[0]);
        if let (Some(po), Some(ro), Some(pc), Some(rc)) = (self.org_pt(z1, p), self.org_pt(z0, rp0), self.cur_pt(z1, p), self.cur_pt(z0, rp0)) {
            let mut org_dist = self.dual_project(po.x.wrapping_sub(ro.x), po.y.wrapping_sub(ro.y));
            let (sv, sc) = (self.gs.sw_value, self.gs.sw_cutin);
            if sc > 0 && org_dist < sv.saturating_add(sc) && org_dist > sv.saturating_sub(sc) {
                org_dist = if org_dist >= 0 { sv } else { sv.saturating_neg() };
            }
            let mut dist = if op & 4 != 0 { self.round_val(org_dist) } else { org_dist };
            if op & 8 != 0 {
                let min = self.gs.min_dist;
                if org_dist >= 0 {
                    dist = dist.max(min);
                } else if dist > min.saturating_neg() {
                    dist = min.saturating_neg();
                }
            }
            let cur_dist = self.project(pc.x.wrapping_sub(rc.x), pc.y.wrapping_sub(rc.y));
            self.move_pt(z1, p, dist.wrapping_sub(cur_dist));
        }
        self.gs.rp[1] = rp0;
        self.gs.rp[2] = p;
        if op & 16 != 0 {
            self.gs.rp[0] = p;
        }
        Ok(())
    }

    #[inline(never)]
    fn mirp(&mut self, op: u8) -> Res<()> {
        let (p, n) = self.pop2()?;
        let p = pidx(p);
        let (z0, z1, rp0) = (self.gs.gep[0], self.gs.gep[1], self.gs.rp[0]);
        let cvt = if n == -1 { Some(0) } else { self.cvt_get(n) };
        if let (Some(mut cvt_dist), true, true) = (cvt, p < self.zone(z1).cur.len(), self.cur_pt(z0, rp0).is_some()) {
            let (sv, sc) = (self.gs.sw_value, self.gs.sw_cutin);
            if abs_diff(cvt_dist, sv) < i64::from(sc) {
                cvt_dist = if cvt_dist >= 0 { sv } else { sv.saturating_neg() };
            }
            let ro = self.org_pt(z0, rp0).unwrap_or_default();
            if z1 == 0 {
                let np = Pt { x: ro.x.saturating_add(mul_div(cvt_dist, self.gs.free.0, 0x4000)), y: ro.y.saturating_add(mul_div(cvt_dist, self.gs.free.1, 0x4000)) };
                let z = self.zone_mut(0);
                if let (Some(o), Some(c)) = (z.org.get_mut(p), z.cur.get_mut(p)) {
                    *o = np;
                    *c = np;
                }
            }
            let po = self.org_pt(z1, p).unwrap_or_default();
            let (pc, rc) = (self.cur_pt(z1, p).unwrap_or_default(), self.cur_pt(z0, rp0).unwrap_or_default());
            let org_dist = self.dual_project(po.x.wrapping_sub(ro.x), po.y.wrapping_sub(ro.y));
            let cur_dist = self.project(pc.x.wrapping_sub(rc.x), pc.y.wrapping_sub(rc.y));
            if self.gs.auto_flip && (org_dist ^ cvt_dist) < 0 {
                cvt_dist = cvt_dist.saturating_neg();
            }
            let mut dist = if op & 4 != 0 {
                if z0 == z1 && abs_diff(cvt_dist, org_dist) > i64::from(self.gs.cvt_cutin) {
                    cvt_dist = org_dist;
                }
                self.round_val(cvt_dist)
            } else {
                cvt_dist
            };
            if op & 8 != 0 {
                let min = self.gs.min_dist;
                if org_dist >= 0 {
                    dist = dist.max(min);
                } else if dist > min.saturating_neg() {
                    dist = min.saturating_neg();
                }
            }
            self.move_pt(z1, p, dist.wrapping_sub(cur_dist));
        }
        self.gs.rp[1] = rp0;
        if op & 16 != 0 {
            self.gs.rp[0] = p;
        }
        self.gs.rp[2] = p;
        Ok(())
    }

    /// `DELTAP1..3`: moves of points at particular sizes.
    #[inline(never)]
    fn deltap(&mut self, op: u8) -> Res<()> {
        let n = self.pop()?;
        let base = self.gs.delta_base.saturating_add(match op {
            0x5D => 0,
            0x71 => 16,
            _ => 32,
        });
        let z0 = self.gs.gep[0];
        for _ in 0..n.max(0) {
            self.spend(1)?;
            let p = pidx(self.pop()?);
            let arg = self.pop()?;
            if ((arg & 0xF0) >> 4).saturating_add(base) == self.env.ppem {
                let mut b = (arg & 0xF) - 8;
                if b >= 0 {
                    b += 1;
                }
                self.move_pt(z0, p, b * (1 << (6 - self.gs.delta_shift)));
            }
        }
        Ok(())
    }

    /// `DELTAC1..3`: changes of control values at particular sizes.
    #[inline(never)]
    fn deltac(&mut self, op: u8) -> Res<()> {
        let n = self.pop()?;
        let base = self.gs.delta_base.saturating_add(match op {
            0x73 => 0,
            0x74 => 16,
            _ => 32,
        });
        for _ in 0..n.max(0) {
            self.spend(1)?;
            let i = self.pop()?;
            let arg = self.pop()?;
            if ((arg & 0xF0) >> 4).saturating_add(base) == self.env.ppem {
                let mut b = (arg & 0xF) - 8;
                if b >= 0 {
                    b += 1;
                }
                if let Some(v) = self.cvt_get(i) {
                    self.cvt_set(i, v.saturating_add(b * (1 << (6 - self.gs.delta_shift))))?;
                }
            }
        }
        Ok(())
    }

    /// `IUP`: the points no instruction touched follow the ones it did, contour by contour.
    #[inline(never)]
    fn iup(&mut self, x_axis: bool) -> Res<()> {
        // Every point and contour is visited, so the work is charged per call, not as one instruction.
        let work = self.zones[1].cur.len().saturating_add(self.zones[1].ends.len());
        self.spend(work)?;
        let touch = if x_axis { TOUCH_X } else { TOUCH_Y };
        let z = &mut self.zones[1];
        let ends = std::mem::take(&mut z.ends);
        let mut first = 0usize;
        for &end in &ends {
            let (a, b) = (first, end.min(z.cur.len()));
            first = end;
            if b <= a {
                continue;
            }
            let is_touched = |z: &Zone, i: usize| z.tags.get(i).is_some_and(|t| t & touch != 0);
            let Some(first_touched) = (a..b).find(|&i| is_touched(z, i)) else { continue };
            let mut prev = first_touched;
            for i in first_touched + 1..b {
                if is_touched(z, i) {
                    iup_span(z, x_axis, prev, i, a, b);
                    prev = i;
                }
            }
            if prev == first_touched {
                // One touched point: the whole contour moves with it.
                let (c, o) = (z.cur.get(prev).copied().unwrap_or_default(), z.org.get(prev).copied().unwrap_or_default());
                let delta = if x_axis { c.x.wrapping_sub(o.x) } else { c.y.wrapping_sub(o.y) };
                if delta != 0 {
                    for i in (a..b).filter(|&i| i != prev) {
                        if let Some(p) = z.cur.get_mut(i) {
                            if x_axis { p.x = p.x.saturating_add(delta) } else { p.y = p.y.saturating_add(delta) }
                        }
                    }
                }
            } else {
                iup_span(z, x_axis, prev, first_touched, a, b);
            }
        }
        z.ends = ends;
        Ok(())
    }
}

/// Place the untouched points after `from` and before `to` (wrapping round the contour `a..b`) between the
/// touched points `from` and `to`, by where their original positions lie between those of the two.
fn iup_span(z: &mut Zone, x_axis: bool, from: usize, to: usize, a: usize, b: usize) {
    let coord = |p: Pt| if x_axis { p.x } else { p.y };
    let get = |v: &[Pt], i: usize| v.get(i).copied().map_or(0, coord);
    let (mut r1, mut r2) = (from, to);
    if get(&z.org, r1) > get(&z.org, r2) {
        std::mem::swap(&mut r1, &mut r2);
    }
    let (org1, org2) = (get(&z.org, r1), get(&z.org, r2));
    let (cur1, cur2) = (get(&z.cur, r1), get(&z.cur, r2));
    let (delta1, delta2) = (cur1.wrapping_sub(org1), cur2.wrapping_sub(org2));
    let mut i = from;
    for _ in 0..b - a {
        i += 1;
        if i >= b {
            i = a;
        }
        if i == to {
            break;
        }
        let o = get(&z.org, i);
        let n = if o <= org1 {
            o.saturating_add(delta1)
        } else if o >= org2 {
            o.saturating_add(delta2)
        } else {
            cur1.saturating_add(mul_div(o.wrapping_sub(org1), cur2.wrapping_sub(cur1), org2.wrapping_sub(org1)))
        };
        if let Some(p) = z.cur.get_mut(i) {
            if x_axis { p.x = n } else { p.y = n }
        }
    }
}

#[cfg(test)]
impl Env {
    pub(crate) fn for_test(ppem: i32) -> Env {
        Env { ppem, scale: i64::from(ppem) * 64 * 65536 / 2048, stack: 256, storage: 64, twilight: 16, fdefs: 64, idefs: 8 }
    }
}

#[cfg(test)]
#[path = "ttvm_tests.rs"]
mod tests;
