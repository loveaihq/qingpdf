//! Glyph outlines with the font's own instructions run (3b2), for the "tricky" fonts only.
//!
//! The instructions of a glyph are run once, at a fixed large size ([`HINT_PPEM`] pixels per em, so the
//! rounding of the instructions is 1/2048 of an em at the coarsest), and the result is an ordinary outline that
//! the caller caches and scales. Nothing here runs again for another size.
//!
//! The order is the specification's: `fpgm`, then `prep`, once for the font; then for each glyph the points are
//! scaled, the phantom points added, and the glyph program run. A composite glyph first hints each component
//! on its own, moves the hinted component into place (by the offset rounded to the grid when the component asks for
//! it, or by matching two points), and then runs its own program over all the points; that program sees the points
//! already as pixels (scale 1), their "original" positions being the hinted component positions.

use std::sync::Arc;

use crate::render::outline::{Builder, Res};
use crate::render::ttvm::{self, Env, Globals, ON_CURVE, Pt, Zone, mul_fix};
use crate::text::fontprog::u16_at;

use super::{Contours, MAX_COMPONENTS, MAX_COMPOSITE_DEPTH, MAX_POINTS, TtFont, parse_component, simple_glyph};

/// The size, in pixels per em, at which the instructions are run.
pub(super) const HINT_PPEM: i32 = 2048;
/// Instructions `fpgm` and `prep` may run together.
const MAX_SETUP_INSTRUCTIONS: usize = 1_000_000;
/// Instructions the programs of one glyph (its components included) may run.
const MAX_GLYPH_INSTRUCTIONS: usize = 200_000;

/// Where the `cvt `, `fpgm` and `prep` tables of a font are.
pub(super) enum ProgSrc {
    None,
    /// Ranges (offset, length) of the font file held in memory.
    Memory { data: Arc<Vec<u8>>, cvt: (usize, usize), fpgm: (usize, usize), prep: (usize, usize) },
    /// Copies (system fonts: kept only when the font is known to be tricky).
    Owned { cvt: Vec<u8>, fpgm: Vec<u8>, prep: Vec<u8> },
}

impl ProgSrc {
    pub fn bytes(&self) -> usize {
        match self {
            ProgSrc::Owned { cvt, fpgm, prep } => cvt.len() + fpgm.len() + prep.len(),
            _ => 0,
        }
    }

    fn tables(&self) -> Option<(&[u8], &[u8], &[u8])> {
        match self {
            ProgSrc::None => None,
            ProgSrc::Owned { cvt, fpgm, prep } => Some((cvt, fpgm, prep)),
            ProgSrc::Memory { data, cvt, fpgm, prep } => {
                let part = |r: &(usize, usize)| data.get(r.0..r.0.saturating_add(r.1)).unwrap_or_default();
                Some((part(cvt), part(fpgm), part(prep)))
            }
        }
    }
}

/// A glyph with its instructions run: points in 26.6 pixels at [`HINT_PPEM`], y up.
struct HGlyph {
    cur: Vec<Pt>,
    tags: Vec<u8>,
    ends: Vec<usize>,
    /// The phantom points (origin, advance, top, bottom).
    pp: [Pt; 4],
    /// How far the instructions moved the origin along x; the outline is moved back by it.
    shift: i32,
}

/// What one glyph's loading uses up.
struct Spent {
    components: usize,
    stack: Vec<usize>,
    fuel: usize,
    work: usize,
}

fn round_pixel(v: i32) -> i32 {
    v.saturating_add(32) & !63
}

impl TtFont {
    /// The font must have its instructions run.
    pub fn is_tricky(&self) -> bool {
        self.tricky
    }

    /// Is this the name (family or PostScript, with or without a subset tag) of a font that must have them run?
    pub fn is_tricky_name(name: &[u8]) -> bool {
        super::tricky::name_is_tricky(&String::from_utf8_lossy(name))
    }

    /// The matrix from the points [`TtFont::outline_hinted`] makes to em units.
    pub fn hinted_matrix(&self) -> [f64; 6] {
        let s = 1.0 / (64.0 * f64::from(HINT_PPEM));
        [s, 0.0, 0.0, s, 0.0, 0.0]
    }

    fn maxp_value(&self, at: usize) -> usize {
        u16_at(&self.maxp, at).map_or(0, usize::from)
    }

    fn hint_env(&self) -> Env {
        let upem = self.units_per_em as i64;
        Env {
            ppem: HINT_PPEM,
            scale: ((i64::from(HINT_PPEM) * 64) << 16) / upem.max(1),
            stack: (self.maxp_value(24) + 32).clamp(64, ttvm::MAX_STACK),
            storage: self.maxp_value(18).min(ttvm::MAX_STORAGE),
            twilight: (self.maxp_value(16) + 4).min(ttvm::MAX_TWILIGHT),
            fdefs: (self.maxp_value(20) + 16).min(ttvm::MAX_FDEFS),
            idefs: (self.maxp_value(22) + 16).min(ttvm::MAX_IDEFS),
        }
    }

    fn build_globals(&self, fuel: &mut usize) -> Result<Globals, &'static str> {
        let (cvt, fpgm, prep) = self.prog.tables().ok_or("the font has no instruction tables")?;
        let cvt: Vec<i16> = (0..cvt.len() / 2).take(ttvm::MAX_CVT).filter_map(|i| u16_at(cvt, i * 2)).map(|v| v as i16).collect();
        Globals::setup(fpgm, prep, &cvt, self.hint_env(), fuel)
    }

    /// The state of `fpgm` and `prep`, made the first time (its instructions are charged to `out`); a font whose
    /// programs fail stays failed.
    fn globals(&self, out: &mut Builder) -> Res<&Globals> {
        let mut spent = 0usize;
        let made = self.hint.get_or_init(|| {
            let mut fuel = MAX_SETUP_INSTRUCTIONS;
            let made = self.build_globals(&mut fuel);
            spent = MAX_SETUP_INSTRUCTIONS - fuel;
            made
        });
        out.charge(spent);
        made.as_ref().map_err(|why| *why)
    }

    /// Advance and left side bearing of a glyph, in font units.
    fn hmetrics(&self, gid: usize) -> (i32, i32) {
        let n = self.num_hmetrics;
        if n == 0 {
            return (0, 0);
        }
        let advance = u16_at(&self.hmtx, gid.min(n - 1) * 4).unwrap_or(0);
        let lsb = if gid < n { u16_at(&self.hmtx, gid * 4 + 2) } else { u16_at(&self.hmtx, n * 4 + (gid - n) * 2) };
        (i32::from(advance), i32::from(lsb.unwrap_or(0) as i16))
    }

    /// The phantom points of a glyph before any instruction: origin and advance on the baseline, top and bottom.
    fn phantoms(&self, xmin: i32, gid: usize, scale: i64) -> [Pt; 4] {
        let (advance, lsb) = self.hmetrics(gid);
        let x0 = xmin.wrapping_sub(lsb);
        let sc = |v: i32| mul_fix(v, scale);
        [
            Pt { x: sc(x0), y: 0 },
            Pt { x: sc(x0.wrapping_add(advance)), y: 0 },
            Pt { x: sc(x0), y: sc(i32::from(self.ascender)) },
            Pt { x: sc(x0), y: sc(i32::from(self.descender)) },
        ]
    }

    /// Draw glyph `gid` into `out` with its instructions run (the points are those of [`TtFont::hinted_matrix`]).
    pub fn outline_hinted(&self, gid: u32, out: &mut Builder) -> Res<()> {
        let g = self.globals(out)?;
        let mut st = Spent { components: MAX_COMPONENTS, stack: Vec::new(), fuel: MAX_GLYPH_INSTRUCTIONS, work: 0 };
        let loaded = usize::try_from(gid).map_err(|_| "glyph number out of range").and_then(|gid| self.load_hinted(g, gid, 0, &mut st));
        out.charge(st.work.saturating_add(MAX_GLYPH_INSTRUCTIONS - st.fuel));
        let h = loaded?;
        let pts = h.cur.iter().zip(&h.tags).map(|(p, t)| (p.x.wrapping_sub(h.shift) as f32, p.y as f32, t & ON_CURVE != 0)).collect();
        Contours { pts, ends: h.ends, work: 0 }.emit(out)
    }

    fn load_hinted(&self, g: &Globals, gid: usize, depth: usize, st: &mut Spent) -> Res<HGlyph> {
        if depth > MAX_COMPOSITE_DEPTH {
            return Err("composite glyphs nested too deep");
        }
        if st.stack.contains(&gid) {
            return Err("a composite glyph includes itself");
        }
        let (a, b) = self.record(gid)?;
        let scale = g.env.scale;
        if a == b {
            let pp = self.phantoms(0, gid, scale);
            return Ok(HGlyph { cur: Vec::new(), tags: Vec::new(), ends: Vec::new(), pp, shift: 0 });
        }
        let rec = self.glyf.slice(a, b).ok_or("a glyph record cannot be read")?;
        let n = u16_at(&rec, 0).ok_or("truncated glyph")? as i16;
        let xmin = i32::from(u16_at(&rec, 2).ok_or("truncated glyph")? as i16);
        let pp0 = self.phantoms(xmin, gid, scale);
        let off = g.glyph_programs_off();
        if n >= 0 {
            let contours = usize::try_from(n).map_err(|_| "bad contour count")?;
            let mut c = Contours::default();
            simple_glyph(&rec, contours, &mut c)?;
            st.work = st.work.saturating_add(c.work);
            let ilen = usize::from(u16_at(&rec, 10 + 2 * contours).unwrap_or(0));
            let ins = rec.get(12 + 2 * contours..12 + 2 * contours + ilen).unwrap_or_default();
            let cur: Vec<Pt> = c.pts.iter().map(|p| Pt { x: mul_fix(p.0 as i32, scale), y: mul_fix(p.1 as i32, scale) }).collect();
            let tags: Vec<u8> = c.pts.iter().map(|p| u8::from(p.2)).collect();
            return self.run_points(g, cur, tags, c.ends, pp0, if off { &[] } else { ins }, scale, st);
        }
        // A composite glyph: the components, each hinted on its own and put in place.
        let sc = |v: i32| mul_fix(v, scale);
        let (mut cur, mut tags, mut ends) = (Vec::<Pt>::new(), Vec::<u8>::new(), Vec::<usize>::new());
        let (mut pp, mut pp1_start) = (pp0, pp0[0].x);
        let mut pos = 10usize;
        let ins: &[u8] = loop {
            if st.components == 0 {
                return Err("a glyph has too many components");
            }
            st.components -= 1;
            let comp = parse_component(&rec, &mut pos)?;
            st.stack.push(gid);
            let child = self.load_hinted(g, comp.child, depth + 1, st);
            st.stack.pop();
            let mut child = child?;
            let [ma, mb, mc, md] = comp.matrix;
            if comp.flags & (0x08 | 0x40 | 0x80) != 0 {
                for p in &mut child.cur {
                    let (x, y) = (f64::from(p.x), f64::from(p.y));
                    *p = Pt { x: (ma * x + mc * y).round() as i32, y: (mb * x + md * y).round() as i32 };
                }
            }
            let (dx, dy) = if comp.flags & 2 != 0 {
                let (mut dx, mut dy) = (f64::from(comp.arg1), f64::from(comp.arg2));
                if comp.flags & 0x800 != 0 && comp.flags & 0x1000 == 0 {
                    (dx, dy) = (ma * dx + mc * dy, mb * dx + md * dy);
                }
                let (x, y) = (sc(dx.round() as i32), sc(dy.round() as i32));
                if comp.flags & 4 != 0 { (round_pixel(x), round_pixel(y)) } else { (x, y) }
            } else {
                // Two points to match: the one of the glyph so far and the one of the component.
                match (cur.get(usize::try_from(comp.arg1).unwrap_or(usize::MAX)), child.cur.get(usize::try_from(comp.arg2).unwrap_or(usize::MAX))) {
                    (Some(p), Some(q)) => (p.x.wrapping_sub(q.x), p.y.wrapping_sub(q.y)),
                    _ => (0, 0),
                }
            };
            if cur.len() + child.cur.len() > MAX_POINTS || ends.len() + child.ends.len() > MAX_POINTS {
                return Err("a glyph has too many points");
            }
            st.work = st.work.saturating_add(child.cur.len() + 1);
            let base = cur.len();
            cur.extend(child.cur.iter().map(|p| Pt { x: p.x.saturating_add(dx), y: p.y.saturating_add(dy) }));
            tags.extend(child.tags.iter().map(|t| t & ON_CURVE));
            ends.extend(child.ends.iter().map(|e| e + base));
            if comp.flags & 0x200 != 0 {
                pp = child.pp;
                pp1_start = child.pp[0].x.wrapping_sub(child.shift);
            }
            if comp.flags & 0x20 == 0 {
                if comp.flags & 0x100 != 0 && !off {
                    let ilen = usize::from(u16_at(&rec, pos).ok_or("truncated composite glyph")?);
                    break rec.get(pos + 2..pos + 2 + ilen).ok_or("truncated composite glyph")?;
                }
                break &[];
            }
        };
        let mut glyph = self.run_points(g, cur, tags, ends, pp, ins, 1 << 16, st)?;
        glyph.shift = glyph.pp[0].x.wrapping_sub(pp1_start);
        Ok(glyph)
    }

    /// Run `ins` over the points (if there are instructions) with the phantom points after them.
    #[allow(clippy::too_many_arguments)]
    fn run_points(&self, g: &Globals, cur: Vec<Pt>, tags: Vec<u8>, ends: Vec<usize>, pp: [Pt; 4], ins: &[u8], scale: i64, st: &mut Spent) -> Res<HGlyph> {
        if ins.is_empty() {
            return Ok(HGlyph { cur, tags, ends, pp, shift: 0 });
        }
        let n = cur.len();
        let mut zone = Zone { org: cur.clone(), cur, tags, ends };
        zone.org.extend(pp);
        zone.cur.extend(pp);
        zone.tags.extend([0u8; 4]);
        let start = pp[0].x;
        if !ins.is_empty() {
            // The program sees the origin and the advance rounded to whole pixels (and the top and bottom likewise).
            for (i, p) in zone.cur.iter_mut().skip(n).enumerate() {
                if i < 2 {
                    p.x = round_pixel(p.x);
                } else {
                    p.y = round_pixel(p.y);
                }
            }
            // Each run starts from copies of the twilight zone and the storage area: that is work too.
            st.work = st.work.saturating_add(zone.cur.len() + 1 + g.env.twilight + g.env.storage);
            zone = g.run_glyph(ins, zone, scale, &mut st.fuel)?;
        }
        let tail = zone.cur.split_off(n);
        let pp: [Pt; 4] = tail.try_into().map_err(|_| "the glyph zone lost its phantom points")?;
        zone.tags.truncate(n);
        Ok(HGlyph { cur: zone.cur, tags: zone.tags, ends: zone.ends, pp, shift: pp[0].x.wrapping_sub(start) })
    }
}
