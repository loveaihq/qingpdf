//! Shadings (ISO 32000-1 8.7.4.5): function-based (type 1), axial (2), radial (3), and the meshes of
//! triangles (4, 5), Coons patches (6) and tensor-product patches (7). A shading is drawn into a bitmap
//! of the part of the device that is to be painted; the caller puts that bitmap on the page through the
//! clip, the soft mask and the blend mode, like any picture.
//!
//! Everything a file can make large is held in check: the colour function of the types 2 and 3 (and of the
//! meshes that have one) is run 256 times, into a table, however many pixels use it; a type 1 function is
//! run on a grid of at most about 260 thousand points; a mesh has at most [`MAX_MESH_TRIANGLES`] triangles
//! or [`MAX_PATCHES`] patches, is read from its bits once when the shading is loaded (what it holds then is kept, at
//! most [`MAX_PARSED_MESH_BYTES`] of it), a patch is cut into at most [`MAX_PATCH_DIVISIONS`] squares a side, and
//! reading a mesh and drawing each of its triangles and patches (seen or not) are charged to the page's work meter.

use std::sync::Arc;

use tiny_skia::{FillRule, Mask, PathBuilder, Pixmap, Transform};

use crate::document::Document;
use crate::object::{Dict, Object};
use crate::text::interp::{Matrix, mul};

use super::color::ColorSpace;
use super::func::{Function, Meter};
use super::work::{Work, cost};

/// Largest data of a mesh read (decoded).
const MAX_MESH_BYTES: usize = 32 * 1024 * 1024;
/// Most triangles a mesh of triangles may have, and patches a patch mesh.
pub(crate) const MAX_MESH_TRIANGLES: u64 = 1_000_000;
pub(crate) const MAX_PATCHES: u64 = 200_000;
/// A patch is cut into at most this many squares along each side.
pub(crate) const MAX_PATCH_DIVISIONS: usize = 32;
/// Most vertices in a row of a lattice mesh.
const MAX_ROW: usize = 1 << 16;
/// Entries in the colour table of a shading with one parameter.
const LUT: usize = 256;
/// Function evaluations of a type 1 shading on one bitmap (it runs on a grid when there would be more).
const MAX_GRID: u64 = 512 * 512;
/// Most bytes a mesh may take once it is read (its vertices and triangles, or its patches).
const MAX_PARSED_MESH_BYTES: usize = 48 * 1024 * 1024;
/// What a page may spend on evaluations of type 1 functions.
pub(crate) const EVALS_PER_PAGE: u64 = 4_000_000;

/// The allowance of one page for type 1 functions, spent as the page is drawn.
pub(crate) struct Budget {
    pub evals: u64,
}

impl Budget {
    pub fn new() -> Budget {
        Budget { evals: EVALS_PER_PAGE }
    }
}

/// The colour functions of a shading: one with the colour space's components as outputs, or one for each.
struct Funcs(Vec<Arc<Function>>);

impl Funcs {
    fn eval(&self, input: &[f64], n: usize, meter: &Meter) -> Option<Vec<f64>> {
        let mut out = Vec::new();
        if let [only] = self.0.as_slice() {
            only.eval(input, &mut out, meter).then_some(out)
        } else {
            let mut all = Vec::with_capacity(n);
            for f in &self.0 {
                if !f.eval(input, &mut out, meter) {
                    return None;
                }
                all.push(out.first().copied().unwrap_or(0.0));
            }
            Some(all)
        }
    }
}

/// 256 colours along the parameter of a shading.
struct Lut(Vec<[u8; 3]>);

impl Lut {
    fn build(funcs: &Funcs, space: &ColorSpace, (t0, t1): (f64, f64), meter: &Meter) -> Option<Lut> {
        let n = space.components();
        let mut table = Vec::with_capacity(LUT);
        for i in 0..LUT {
            // (Entry i is the colour at i / 256 of the range, as in PDFium; the last entry stops a step short of the end.)
            let t = t0 + (t1 - t0) * i as f64 / LUT as f64;
            let comps = funcs.eval(&[t], n, meter)?;
            table.push(to_bytes(space.to_rgb(&comps, meter)));
        }
        Some(Lut(table))
    }

    fn at(&self, s: f64) -> [u8; 3] {
        let i = (s.clamp(0.0, 1.0) * (LUT - 1) as f64) as usize;
        self.0.get(i).copied().unwrap_or([0; 3])
    }
}

fn to_bytes(c: [f32; 3]) -> [u8; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

enum Kind {
    Function { domain: [f64; 4], matrix: Matrix, funcs: Funcs },
    Axial { coords: [f64; 4], extend: (bool, bool), lut: Lut },
    Radial { coords: [f64; 6], extend: (bool, bool), lut: Lut },
    Mesh(MeshData),
}

pub(crate) struct Shading {
    kind: Kind,
    space: Arc<ColorSpace>,
    background: Option<[f32; 3]>,
    bbox: Option<[f64; 4]>,
    /// About how much memory it holds.
    pub bytes: usize,
}

fn numbers(doc: &Document, dict: &Dict, key: &str) -> Option<Vec<f64>> {
    let obj = doc.resolve(dict.get(key)?).ok()?;
    obj.as_array()?.iter().map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64()).filter(|v| v.is_finite())).collect()
}

fn int(doc: &Document, dict: &Dict, key: &str) -> Option<i64> {
    doc.resolve(dict.get(key)?).ok()?.as_int()
}

fn flag_pair(doc: &Document, dict: &Dict) -> (bool, bool) {
    match dict.get("Extend").and_then(|o| doc.resolve(o).ok()) {
        Some(Object::Array(a)) => {
            let get = |i: usize| matches!(a.get(i).and_then(|o| doc.resolve(o).ok()), Some(Object::Bool(true)));
            (get(0), get(1))
        }
        _ => (false, false),
    }
}

/// The functions of a shading dictionary: a function, or an array of 1-out functions.
fn functions(doc: &Document, dict: &Dict, n: usize) -> Option<Funcs> {
    let obj = dict.get("Function")?;
    let list = match doc.resolve(obj).ok()? {
        Object::Array(items) => Function::load_many(doc, &items)?,
        _ => vec![Function::load(doc, obj)?],
    };
    let ok = match list.as_slice() {
        [only] => only.outputs() == n || n == 1,
        many => many.len() == n && many.iter().all(|f| f.outputs() >= 1),
    };
    ok.then_some(Funcs(list))
}

impl Shading {
    /// Read a shading dictionary (a stream for the types 4 to 7). `lookup` finds a colour space by name in the
    /// resources. `Err` says why it is skipped.
    pub fn load(doc: &Document, obj: &Object, lookup: &dyn Fn(&[u8]) -> Option<Object>, meter: &Meter, work: &mut Work) -> Result<Shading, String> {
        let resolved = doc.resolve(obj).map_err(|e| e.to_string())?;
        let (dict, stream) = match &resolved {
            Object::Dict(d) => (d.clone(), None),
            Object::Stream(s) => (s.dict.clone(), Some(s)),
            _ => return Err("it is not a dictionary".to_string()),
        };
        let ty = int(doc, &dict, "ShadingType").ok_or("it has no ShadingType")?;
        let space = dict
            .get("ColorSpace")
            .and_then(|c| ColorSpace::load(doc, c, lookup, meter))
            .filter(|s| !matches!(**s, ColorSpace::Pattern(_)))
            .ok_or("its colour space cannot be read")?;
        let n = space.components();
        let background = numbers(doc, &dict, "Background").filter(|b| b.len() == n).map(|b| space.to_rgb(&b, meter));
        let bbox = crate::document::rectangle(dict.get("BBox").and_then(|o| doc.resolve(o).ok()).as_ref());
        let mut bytes = 256;
        let kind = match ty {
            1 => {
                let domain: [f64; 4] = numbers(doc, &dict, "Domain").and_then(|d| d.try_into().ok()).unwrap_or([0.0, 1.0, 0.0, 1.0]);
                let matrix: Matrix = numbers(doc, &dict, "Matrix").and_then(|d| d.try_into().ok()).unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                let funcs = functions(doc, &dict, n).ok_or("its function cannot be read")?;
                if funcs.0.iter().any(|f| f.inputs() != 2) {
                    return Err("its function does not take two inputs".to_string());
                }
                Kind::Function { domain, matrix, funcs }
            }
            2 | 3 => {
                let coords = numbers(doc, &dict, "Coords").ok_or("it has no Coords")?;
                let t = match numbers(doc, &dict, "Domain").as_deref() {
                    Some([a, b]) => (*a, *b),
                    _ => (0.0, 1.0),
                };
                let funcs = functions(doc, &dict, n).ok_or("its function cannot be read")?;
                if funcs.0.iter().any(|f| f.inputs() != 1) {
                    return Err("its function does not take one input".to_string());
                }
                let lut = Lut::build(&funcs, &space, t, meter).ok_or("its function cannot be run")?;
                bytes += LUT * 3;
                let extend = flag_pair(doc, &dict);
                if ty == 2 {
                    Kind::Axial { coords: coords.try_into().map_err(|_| "its Coords are not four numbers")?, extend, lut }
                } else {
                    Kind::Radial { coords: coords.try_into().map_err(|_| "its Coords are not six numbers")?, extend, lut }
                }
            }
            4..=7 => {
                let stream = stream.ok_or("a mesh must be a stream")?;
                let data = doc.decode_stream_limited(stream, MAX_MESH_BYTES).map_err(|e| format!("its data cannot be read: {e}"))?;
                let bits = |key: &str, ok: &[i64]| int(doc, &dict, key).filter(|v| ok.contains(v)).map(|v| v as u32);
                let bits_coord = bits("BitsPerCoordinate", &[1, 2, 4, 8, 12, 16, 24, 32]).ok_or("BitsPerCoordinate is not a valid number")?;
                let bits_comp = bits("BitsPerComponent", &[1, 2, 4, 8, 12, 16]).ok_or("BitsPerComponent is not a valid number")?;
                let bits_flag = if ty == 5 { 0 } else { bits("BitsPerFlag", &[2, 4, 8]).ok_or("BitsPerFlag is not a valid number")? };
                let per_row = if ty == 5 { int(doc, &dict, "VerticesPerRow").filter(|v| (2..=MAX_ROW as i64).contains(v)).ok_or("VerticesPerRow is not a valid number")? as usize } else { 0 };
                let has_function = dict.contains_key("Function");
                let ncomp = if has_function { 1 } else { n };
                let decode = numbers(doc, &dict, "Decode").filter(|d| d.len() >= 4 + 2 * ncomp).ok_or("its Decode array is too short")?;
                let lut = if has_function {
                    let funcs = functions(doc, &dict, n).ok_or("its function cannot be read")?;
                    let range = (decode.get(4).copied().unwrap_or(0.0), decode.get(5).copied().unwrap_or(1.0));
                    bytes += LUT * 3;
                    Some(Lut::build(&funcs, &space, range, meter).ok_or("its function cannot be run")?)
                } else {
                    None
                };
                let layout = MeshLayout { ty: ty as u8, bits_coord, bits_comp, bits_flag, per_row, decode, ncomp, has_lut: lut.is_some() };
                layout.check_size(data.len())?;
                // The bits are read here, once; the shading keeps what they hold, not the bits.
                let parsed = layout.parse(&data, &space, meter, work)?;
                bytes += parsed.bytes();
                Kind::Mesh(MeshData { ty: ty as u8, lut, parsed })
            }
            _ => return Err(format!("shading type {ty} is not one of the seven")),
        };
        Ok(Shading { kind, space, background, bbox, bytes })
    }

    /// The part of the shading's own space it can paint, when that is bounded: its `/BBox`, and for a radial shading
    /// that is not extended, the box of its two circles (the paint stays inside what they sweep). `sh` paints the whole
    /// clip; this is what keeps it from working out every pixel of the page for a glow the size of a coin.
    pub fn extent(&self) -> Option<[f64; 4]> {
        let circles = match &self.kind {
            Kind::Radial { coords: [x0, y0, r0, x1, y1, r1], extend: (false, false), .. } => {
                let (r0, r1) = (r0.abs(), r1.abs());
                Some([(x0 - r0).min(x1 - r1), (y0 - r0).min(y1 - r1), (x0 + r0).max(x1 + r1), (y0 + r0).max(y1 + r1)])
            }
            _ => None,
        };
        match (self.bbox, circles) {
            (Some([a, b, c, d]), Some([e, f, g, h])) => Some([a.max(e), b.max(f), c.min(g), d.min(h)]),
            (Some(r), None) | (None, Some(r)) => Some(r),
            (None, None) => None,
        }
    }

    /// What each pixel of a bitmap of this shading costs, in the units of the work meter: the bitmap, and the colour of
    /// the pixel (a mesh pays for its triangles as it draws them).
    pub fn pixel_cost(&self) -> f64 {
        cost::SHADE_BITMAP
            + match &self.kind {
                Kind::Axial { .. } => cost::SHADE_AXIAL,
                Kind::Radial { .. } => cost::SHADE_RADIAL,
                Kind::Function { .. } => cost::SHADE_FUNCTION,
                Kind::Mesh(_) => 0.0,
            }
    }

    /// Draw the shading into a new bitmap of `area` (left, top, right, bottom in device pixels), `m` taking
    /// the shading's space to the device. Where it paints nothing the bitmap is transparent, except
    /// that `background` fills the shading's bounding box (a pattern fill: 8.7.4.3). The caller has paid for the
    /// pixels ([`Shading::pixel_cost`]); the triangles and patches of a mesh are paid for here.
    pub fn render(&self, m: &Matrix, area: [i32; 4], background: bool, budget: &mut Budget, meter: &Meter, work: &mut Work) -> Result<Pixmap, String> {
        let (w, h) = (area[2].saturating_sub(area[0]).max(0) as u32, area[3].saturating_sub(area[1]).max(0) as u32);
        let mut pix = Pixmap::new(w, h).ok_or("the area is empty or too large")?;
        let det = m[0] * m[3] - m[1] * m[2];
        if !det.is_finite() || det.abs() < 1e-14 {
            return Ok(pix);
        }
        // Device (pixel centre) to shading space.
        let inv = [m[3] / det, -m[1] / det, -m[2] / det, m[0] / det, (m[2] * m[5] - m[3] * m[4]) / det, (m[1] * m[4] - m[0] * m[5]) / det];
        let origin = (f64::from(area[0]) + 0.5, f64::from(area[1]) + 0.5);
        let at = |x: usize, y: usize| apply(&inv, origin.0 + x as f64, origin.1 + y as f64);
        let (wu, hu) = (w as usize, h as usize);
        match &self.kind {
            Kind::Axial { coords, extend, lut } => {
                let [x0, y0, x1, y1] = *coords;
                let (dx, dy) = (x1 - x0, y1 - y0);
                let len2 = dx * dx + dy * dy;
                if len2 > 0.0 {
                    let (sx0, sy0) = at(0, 0);
                    let s0 = ((sx0 - x0) * dx + (sy0 - y0) * dy) / len2;
                    let ds_x = (inv[0] * dx + inv[1] * dy) / len2;
                    let ds_y = (inv[2] * dx + inv[3] * dy) / len2;
                    for (y, row) in pix.data_mut().chunks_exact_mut(wu * 4).enumerate() {
                        let mut s = s0 + ds_y * y as f64;
                        for px in row.chunks_exact_mut(4) {
                            let c = if s < 0.0 {
                                extend.0.then(|| lut.at(0.0))
                            } else if s > 1.0 {
                                extend.1.then(|| lut.at(1.0))
                            } else {
                                Some(lut.at(s))
                            };
                            if let Some([r, g, b]) = c {
                                px.copy_from_slice(&[r, g, b, 255]);
                            }
                            s += ds_x;
                        }
                    }
                }
            }
            Kind::Radial { coords, extend, lut } => {
                let [x0, y0, r0, x1, y1, r1] = *coords;
                let (cdx, cdy, dr) = (x1 - x0, y1 - y0, r1 - r0);
                let a = cdx * cdx + cdy * cdy - dr * dr;
                for (y, row) in pix.data_mut().chunks_exact_mut(wu * 4).enumerate() {
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        let (sx, sy) = at(x, y);
                        let (pdx, pdy) = (sx - x0, sy - y0);
                        let b = pdx * cdx + pdy * cdy + r0 * dr;
                        let c = pdx * pdx + pdy * pdy - r0 * r0;
                        // The largest s with |p - c(s)| = r(s) and r(s) >= 0, inside the range or an extended end.
                        let mut roots = [f64::NAN; 2];
                        if a.abs() < 1e-12 {
                            if b.abs() > 1e-12 {
                                roots[0] = c / (2.0 * b);
                            }
                        } else {
                            let disc = b * b - a * c;
                            if disc >= 0.0 {
                                let sq = disc.sqrt();
                                let (s1, s2) = ((b + sq) / a, (b - sq) / a);
                                roots = [s1.max(s2), s1.min(s2)];
                            }
                        }
                        for s in roots {
                            if !s.is_finite() || r0 + s * dr < 0.0 {
                                continue;
                            }
                            let ok = (0.0..=1.0).contains(&s) || (s < 0.0 && extend.0) || (s > 1.0 && extend.1);
                            if ok {
                                let [r, g, bl] = lut.at(s);
                                px.copy_from_slice(&[r, g, bl, 255]);
                                break;
                            }
                        }
                    }
                }
            }
            Kind::Function { domain, matrix, funcs } => {
                let to_domain = invert(&mul(matrix, m)).ok_or("its matrix cannot be turned over")?;
                let step = (((u64::from(w) * u64::from(h)) as f64 / MAX_GRID as f64).sqrt().ceil() as usize).max(1);
                let (nx, ny) = ((wu - 1) / step + 2, (hu - 1) / step + 2);
                let nodes = (nx * ny) as u64;
                budget.evals = budget.evals.checked_sub(nodes).ok_or("the page runs shading functions more often than is allowed")?;
                if !work.charge(nodes as f64 * cost::SHADE_EVAL) {
                    return Err("the page's shadings need more work than is allowed".to_string());
                }
                let n = self.space.components();
                let mut grid: Vec<[f32; 3]> = Vec::with_capacity(nx * ny);
                for j in 0..ny {
                    for i in 0..nx {
                        let (u, v) = apply(&to_domain, origin.0 + (i * step) as f64, origin.1 + (j * step) as f64);
                        let (u, v) = (u.clamp(domain[0].min(domain[1]), domain[0].max(domain[1])), v.clamp(domain[2].min(domain[3]), domain[2].max(domain[3])));
                        let comps = funcs.eval(&[u, v], n, meter).ok_or("its function cannot be run")?;
                        grid.push(self.space.to_rgb(&comps, meter));
                    }
                }
                let cell = |i: usize, j: usize| grid.get(j * nx + i).copied().unwrap_or([0.0; 3]);
                for (y, row) in pix.data_mut().chunks_exact_mut(wu * 4).enumerate() {
                    let (j, fy) = (y / step, (y % step) as f32 / step as f32);
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        let (u, v) = apply(&to_domain, origin.0 + x as f64, origin.1 + y as f64);
                        if u < domain[0].min(domain[1]) || u > domain[0].max(domain[1]) || v < domain[2].min(domain[3]) || v > domain[2].max(domain[3]) {
                            continue;
                        }
                        let (i, fx) = (x / step, (x % step) as f32 / step as f32);
                        let (c00, c10, c01, c11) = (cell(i, j), cell(i + 1, j), cell(i, j + 1), cell(i + 1, j + 1));
                        let mut rgb = [0.0f32; 3];
                        for (k, slot) in rgb.iter_mut().enumerate() {
                            let g = |c: [f32; 3]| c.get(k).copied().unwrap_or(0.0);
                            let top = g(c00) + (g(c10) - g(c00)) * fx;
                            let bottom = g(c01) + (g(c11) - g(c01)) * fx;
                            *slot = top + (bottom - top) * fy;
                        }
                        let [r, g, b] = to_bytes(rgb);
                        px.copy_from_slice(&[r, g, b, 255]);
                    }
                }
            }
            Kind::Mesh(mesh) => {
                let to_device = |x: f64, y: f64| {
                    let (dx, dy) = apply(m, x, y);
                    ((dx - f64::from(area[0])).clamp(-1e7, 1e7) as f32, (dy - f64::from(area[1])).clamp(-1e7, 1e7) as f32)
                };
                let mut canvas = Canvas { data: pix.data_mut(), w: wu, h: hu, work, lut: mesh.lut.as_ref() };
                mesh.draw(&to_device, &mut canvas)?;
            }
        }
        if background && let Some(bg) = self.background {
            let [r, g, b] = to_bytes(bg);
            for px in pix.data_mut().chunks_exact_mut(4) {
                if px.get(3) == Some(&0) {
                    px.copy_from_slice(&[r, g, b, 255]);
                }
            }
        }
        self.clip_to_bbox(&mut pix, m, area);
        Ok(pix)
    }

    /// The shading paints only inside its `/BBox` (8.7.4.3).
    fn clip_to_bbox(&self, pix: &mut Pixmap, m: &Matrix, area: [i32; 4]) {
        let Some([bx0, by0, bx1, by1]) = self.bbox else { return };
        let corners = [apply(m, bx0, by0), apply(m, bx1, by0), apply(m, bx1, by1), apply(m, bx0, by1)];
        let mut pb = PathBuilder::new();
        for (i, (x, y)) in corners.iter().enumerate() {
            let (x, y) = ((*x - f64::from(area[0])).clamp(-1e7, 1e7) as f32, (*y - f64::from(area[1])).clamp(-1e7, 1e7) as f32);
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        pb.close();
        let Some(mask) = pb.finish().and_then(|path| {
            let mut mask = Mask::new(pix.width(), pix.height())?;
            mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
            Some(mask)
        }) else {
            pix.data_mut().fill(0);
            return;
        };
        for (px, &k) in pix.data_mut().chunks_exact_mut(4).zip(mask.data()) {
            if k == 255 {
                continue;
            }
            for v in px.iter_mut() {
                *v = ((u32::from(*v) * u32::from(k) + 127) / 255) as u8;
            }
        }
    }
}

fn apply(m: &Matrix, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if !det.is_finite() || det.abs() < 1e-14 {
        return None;
    }
    Some([m[3] / det, -m[1] / det, -m[2] / det, m[0] / det, (m[2] * m[5] - m[3] * m[4]) / det, (m[1] * m[4] - m[0] * m[5]) / det])
}

// --- meshes --------------------------------------------------------------------------------------------

/// A vertex in the bitmap's pixels; `c` is the colour, or the parameter in `c[0]` (0 to 1 of its range).
#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    c: [f32; 3],
}

/// A vertex as the mesh has it, in the shading's own space.
#[derive(Clone, Copy)]
struct MeshVertex {
    x: f64,
    y: f64,
    c: [f32; 3],
}

/// A patch as the mesh has it, in the shading's own space: the points in the order of the data (the 12 of the
/// boundary first, then the four inside points of a tensor patch) and the colours of its corners.
struct MeshPatch {
    pts: [(f64, f64); 16],
    cols: [[f32; 3]; 4],
}

/// What a mesh holds, read from its bits once (however many times it is drawn).
enum Parsed {
    /// Types 4 and 5: the vertices, and the triangles between them (three indices into the vertices each).
    Triangles { verts: Vec<MeshVertex>, tris: Vec<[u32; 3]> },
    /// Types 6 and 7.
    Patches(Vec<MeshPatch>),
}

impl Parsed {
    fn bytes(&self) -> usize {
        match self {
            Parsed::Triangles { verts, tris } => verts.len() * size_of::<MeshVertex>() + tris.len() * size_of::<[u32; 3]>(),
            Parsed::Patches(p) => p.len() * size_of::<MeshPatch>(),
        }
    }
}

/// A mesh as it is drawn.
struct MeshData {
    /// 4 to 7.
    ty: u8,
    /// The colour table when there is a function; the decode range of the parameter is its domain.
    lut: Option<Lut>,
    parsed: Parsed,
}

struct Canvas<'a> {
    data: &'a mut [u8],
    w: usize,
    h: usize,
    work: &'a mut Work,
    lut: Option<&'a Lut>,
}

const OUT_OF_WORK: &str = "the page's meshes cover more than is allowed";

impl Canvas<'_> {
    /// Fill a triangle with Gouraud shading, sampling at pixel centres. `Err` when the page is out of work.
    fn triangle(&mut self, a: &Vertex, b: &Vertex, c: &Vertex) -> Result<(), String> {
        let (minx, maxx) = (a.x.min(b.x).min(c.x), a.x.max(b.x).max(c.x));
        let (miny, maxy) = (a.y.min(b.y).min(c.y), a.y.max(b.y).max(c.y));
        let x0 = ((minx - 0.5).ceil().max(0.0)) as usize;
        let y0 = ((miny - 0.5).ceil().max(0.0)) as usize;
        let x1 = (((maxx - 0.5).floor()).min(self.w as f32 - 1.0)).max(-1.0);
        let y1 = (((maxy - 0.5).floor()).min(self.h as f32 - 1.0)).max(-1.0);
        let pixels = if x1 >= x0 as f32 && y1 >= y0 as f32 { (x1 as u64 + 1 - x0 as u64) as f64 * (y1 as u64 + 1 - y0 as u64) as f64 } else { 0.0 };
        if !self.work.charge(cost::MESH_TRIANGLE + pixels * cost::MESH_PIXEL) {
            return Err(OUT_OF_WORK.to_string());
        }
        if x1 < x0 as f32 || y1 < y0 as f32 {
            return Ok(());
        }
        let (x1, y1) = (x1 as usize, y1 as usize);
        let area2 = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        if area2.abs() < 1e-9 || !area2.is_finite() {
            return Ok(());
        }
        let inv = 1.0 / area2;
        // Barycentric weights are linear in x and y.
        let (w0x, w0y) = ((b.y - c.y) * inv, (c.x - b.x) * inv);
        let (w1x, w1y) = ((c.y - a.y) * inv, (a.x - c.x) * inv);
        let eps = -1e-4f32;
        let stride = self.w * 4;
        for (y, row) in self.data.chunks_exact_mut(stride).enumerate().skip(y0).take(y1 + 1 - y0) {
            let py = y as f32 + 0.5;
            let px0 = x0 as f32 + 0.5;
            let mut w0 = (px0 - b.x) * w0x + (py - b.y) * w0y;
            let mut w1 = (px0 - c.x) * w1x + (py - c.y) * w1y;
            let Some(span) = row.get_mut(x0 * 4..(x1 + 1) * 4) else { continue };
            for px in span.chunks_exact_mut(4) {
                let w2 = 1.0 - w0 - w1;
                if w0 >= eps && w1 >= eps && w2 >= eps {
                    let mix = |k: usize| {
                        let g = |v: &Vertex| v.c.get(k).copied().unwrap_or(0.0);
                        w0 * g(a) + w1 * g(b) + w2 * g(c)
                    };
                    let rgb = match self.lut {
                        Some(lut) => lut.at(f64::from(mix(0))),
                        None => to_bytes([mix(0), mix(1), mix(2)]),
                    };
                    px.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                }
                w0 += w0x;
                w1 += w1x;
            }
        }
        Ok(())
    }
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    fn read(&mut self, n: u32) -> Option<u64> {
        if self.left() < n as usize {
            return None;
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data.get(self.pos / 8).copied().unwrap_or(0);
            v = (v << 1) | u64::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// How the bits of a mesh are laid out.
struct MeshLayout {
    /// 4 to 7.
    ty: u8,
    bits_coord: u32,
    bits_comp: u32,
    bits_flag: u32,
    per_row: usize,
    /// `[xmin xmax ymin ymax c1min c1max ...]`.
    decode: Vec<f64>,
    /// Numbers of colour in a vertex: 1 (a parameter) with a function, else the components of the space.
    ncomp: usize,
    /// There is a function: the colour of a vertex is a parameter.
    has_lut: bool,
}

impl MeshLayout {
    fn vertex_bits(&self) -> usize {
        self.bits_flag as usize + 2 * self.bits_coord as usize + self.ncomp * self.bits_comp as usize
    }

    /// Refuse a mesh that is too big before reading any of it (`len` bytes of data).
    fn check_size(&self, len: usize) -> Result<(), String> {
        let too_many = || format!("it has more than {MAX_MESH_TRIANGLES} triangles or {MAX_PATCHES} patches");
        match self.ty {
            4 => {
                let vertices = len / self.vertex_bits().div_ceil(8).max(1);
                if vertices as u64 > MAX_MESH_TRIANGLES + 2 {
                    return Err(too_many());
                }
            }
            5 => {
                let vertices = len / self.vertex_bits().div_ceil(8).max(1);
                if (vertices as u64 / self.per_row.max(1) as u64).saturating_mul(self.per_row as u64) > MAX_MESH_TRIANGLES {
                    return Err(too_many());
                }
            }
            _ => {
                // The least a patch takes is the shared edge's worth less: 8 or 12 points and two colours.
                let points = if self.ty == 6 { 8 } else { 12 };
                let bits = self.bits_flag as usize + points * 2 * self.bits_coord as usize + 2 * self.ncomp * self.bits_comp as usize;
                if (len / bits.div_ceil(8).max(1)) as u64 > MAX_PATCHES {
                    return Err(too_many());
                }
            }
        }
        Ok(())
    }

    fn coord(&self, raw: u64, axis: usize) -> f64 {
        let max = ((1u64 << self.bits_coord) - 1) as f64;
        let (lo, hi) = (self.decode.get(axis * 2).copied().unwrap_or(0.0), self.decode.get(axis * 2 + 1).copied().unwrap_or(1.0));
        lo + raw as f64 / max * (hi - lo)
    }

    /// Read the colour of a vertex: the parameter as a fraction of its range, or the colour converted to RGB.
    fn read_colour(&self, bits: &mut Bits<'_>, space: &ColorSpace, meter: &Meter) -> Option<[f32; 3]> {
        let max = ((1u64 << self.bits_comp) - 1) as f64;
        let mut comps = Vec::with_capacity(self.ncomp);
        for k in 0..self.ncomp {
            let raw = bits.read(self.bits_comp)?;
            let (lo, hi) = (self.decode.get(4 + k * 2).copied().unwrap_or(0.0), self.decode.get(5 + k * 2).copied().unwrap_or(1.0));
            comps.push(lo + raw as f64 / max * (hi - lo));
        }
        if self.has_lut {
            // Position of the parameter within its range.
            let (lo, hi) = (self.decode.get(4).copied().unwrap_or(0.0), self.decode.get(5).copied().unwrap_or(1.0));
            let t = comps.first().copied().unwrap_or(0.0);
            let frac = if hi != lo { (t - lo) / (hi - lo) } else { 0.0 };
            Some([frac.clamp(0.0, 1.0) as f32, 0.0, 0.0])
        } else {
            Some(space.to_rgb(&comps, meter))
        }
    }

    /// One vertex of a triangle mesh: its edge flag (0 when the type has none), position and colour.
    fn read_vertex(&self, bits: &mut Bits<'_>, space: &ColorSpace, meter: &Meter) -> Option<(u64, MeshVertex)> {
        let flag = if self.bits_flag > 0 { bits.read(self.bits_flag)? & 3 } else { 0 };
        let x = self.coord(bits.read(self.bits_coord)?, 0);
        let y = self.coord(bits.read(self.bits_coord)?, 1);
        let c = self.read_colour(bits, space, meter)?;
        // Each vertex takes a whole number of bytes.
        bits.align();
        Some((flag, MeshVertex { x, y, c }))
    }

    /// Read the whole mesh, paying for it: by the bytes, and by each vertex or patch. `Err` says why it is not used.
    fn parse(&self, data: &[u8], space: &ColorSpace, meter: &Meter, work: &mut Work) -> Result<Parsed, String> {
        if !work.charge(data.len() as f64 * cost::DECODE_BYTE) {
            return Err(OUT_OF_WORK.to_string());
        }
        let too_big = || "its decoded vertices take more memory than is allowed".to_string();
        let mut bits = Bits { data, pos: 0 };
        match self.ty {
            4 => {
                let vertex_bits = self.vertex_bits();
                let mut verts: Vec<MeshVertex> = Vec::new();
                let mut tris: Vec<[u32; 3]> = Vec::new();
                let mut tri: Option<[u32; 3]> = None;
                while bits.left() >= vertex_bits {
                    if !work.charge(cost::MESH_VERTEX) {
                        return Err(OUT_OF_WORK.to_string());
                    }
                    if verts.len() * size_of::<MeshVertex>() + tris.len() * size_of::<[u32; 3]>() > MAX_PARSED_MESH_BYTES {
                        return Err(too_big());
                    }
                    let Some((flag, v)) = self.read_vertex(&mut bits, space, meter) else { break };
                    let iv = verts.len() as u32;
                    verts.push(v);
                    match (flag, tri) {
                        // 8.7.4.5.5: the new vertex continues on the side vbc (1) or vac (2) of the last triangle.
                        (1, Some([_, b, c])) => tri = Some([b, c, iv]),
                        (2, Some([a, _, c])) => tri = Some([a, c, iv]),
                        _ => {
                            // A new triangle: this vertex and the next two (their flags are ignored).
                            let (Some((_, vb)), Some((_, vc))) = (self.read_vertex(&mut bits, space, meter), self.read_vertex(&mut bits, space, meter)) else {
                                break;
                            };
                            verts.push(vb);
                            verts.push(vc);
                            tri = Some([iv, iv + 1, iv + 2]);
                        }
                    }
                    if let Some(t) = tri {
                        tris.push(t);
                    }
                }
                Ok(Parsed::Triangles { verts, tris })
            }
            5 => {
                let vertex_bits = self.vertex_bits();
                let per_row = self.per_row;
                let mut verts: Vec<MeshVertex> = Vec::new();
                let mut tris: Vec<[u32; 3]> = Vec::new();
                while bits.left() >= vertex_bits {
                    if !work.charge(cost::MESH_VERTEX) {
                        return Err(OUT_OF_WORK.to_string());
                    }
                    if verts.len() * size_of::<MeshVertex>() + tris.len() * size_of::<[u32; 3]>() > MAX_PARSED_MESH_BYTES {
                        return Err(too_big());
                    }
                    let Some((_, v)) = self.read_vertex(&mut bits, space, meter) else { break };
                    verts.push(v);
                    if per_row > 0 && verts.len().is_multiple_of(per_row) && verts.len() >= 2 * per_row {
                        // A row is complete and there is one before it: two triangles for each cell between them.
                        let this = verts.len() - per_row;
                        let before = this - per_row;
                        for i in 0..per_row - 1 {
                            let (a, b, c, d) = ((before + i) as u32, (before + i + 1) as u32, (this + i) as u32, (this + i + 1) as u32);
                            // (Vi,j, Vi,j+1, Vi+1,j) and (Vi,j+1, Vi+1,j, Vi+1,j+1)
                            tris.push([a, b, c]);
                            tris.push([b, c, d]);
                        }
                    }
                }
                Ok(Parsed::Triangles { verts, tris })
            }
            _ => self.parse_patches(&mut bits, space, meter, work),
        }
    }

    /// Types 6 and 7 (8.7.4.5.7, 8.7.4.5.8).
    fn parse_patches(&self, bits: &mut Bits<'_>, space: &ColorSpace, meter: &Meter, work: &mut Work) -> Result<Parsed, String> {
        let total = if self.ty == 6 { 12 } else { 16 };
        let mut patches: Vec<MeshPatch> = Vec::new();
        while let Some(flag) = bits.read(self.bits_flag).map(|f| f & 3) {
            if !work.charge(cost::MESH_PATCH) {
                return Err(OUT_OF_WORK.to_string());
            }
            let mut pts = [(0.0f64, 0.0f64); 16];
            let mut cols = [[0.0f32; 3]; 4];
            let (first_pt, first_col) = match (flag, patches.last()) {
                (0, _) | (_, None) => (0, 0),
                (f, Some(prev)) => {
                    let from: [usize; 4] = match f {
                        1 => [3, 4, 5, 6],
                        2 => [6, 7, 8, 9],
                        _ => [9, 10, 11, 0],
                    };
                    for (slot, &i) in pts.iter_mut().zip(&from) {
                        *slot = prev.pts.get(i).copied().unwrap_or((0.0, 0.0));
                    }
                    let cfrom: [usize; 2] = match f {
                        1 => [1, 2],
                        2 => [2, 3],
                        _ => [3, 0],
                    };
                    for (slot, &i) in cols.iter_mut().zip(&cfrom) {
                        *slot = prev.cols.get(i).copied().unwrap_or([0.0; 3]);
                    }
                    (4, 2)
                }
            };
            let mut ok = true;
            for slot in pts.iter_mut().take(total).skip(first_pt) {
                match (bits.read(self.bits_coord), bits.read(self.bits_coord)) {
                    (Some(x), Some(y)) => *slot = (self.coord(x, 0), self.coord(y, 1)),
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                break;
            }
            for slot in cols.iter_mut().skip(first_col) {
                match self.read_colour(bits, space, meter) {
                    Some(c) => *slot = c,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                break;
            }
            bits.align();
            if patches.len() as u64 >= MAX_PATCHES {
                return Err(format!("it has more than {MAX_PATCHES} patches"));
            }
            if (patches.len() + 1) * size_of::<MeshPatch>() > MAX_PARSED_MESH_BYTES {
                return Err("its decoded patches take more memory than is allowed".to_string());
            }
            patches.push(MeshPatch { pts, cols });
        }
        Ok(Parsed::Patches(patches))
    }
}

impl MeshData {
    fn draw(&self, to_device: &dyn Fn(f64, f64) -> (f32, f32), canvas: &mut Canvas<'_>) -> Result<(), String> {
        match &self.parsed {
            Parsed::Triangles { verts, tris } => {
                let device = |i: u32| {
                    verts.get(i as usize).map(|v| {
                        let (x, y) = to_device(v.x, v.y);
                        Vertex { x, y, c: v.c }
                    })
                };
                for &[a, b, c] in tris {
                    let (Some(a), Some(b), Some(c)) = (device(a), device(b), device(c)) else { continue };
                    canvas.triangle(&a, &b, &c)?;
                }
            }
            Parsed::Patches(patches) => {
                for patch in patches {
                    self.draw_patch(patch, to_device, canvas)?;
                }
            }
        }
        Ok(())
    }

    fn draw_patch(&self, patch: &MeshPatch, to_device: &dyn Fn(f64, f64) -> (f32, f32), canvas: &mut Canvas<'_>) -> Result<(), String> {
        // Where each point of the data goes in the 4 by 4 array (column i, row j).
        const SLOT: [(usize, usize); 16] = [(0, 0), (0, 1), (0, 2), (0, 3), (1, 3), (2, 3), (3, 3), (3, 2), (3, 1), (3, 0), (2, 0), (1, 0), (1, 1), (1, 2), (2, 2), (2, 1)];
        if !canvas.work.charge(cost::MESH_PATCH_DRAWN) {
            return Err(OUT_OF_WORK.to_string());
        }
        let total = if self.ty == 6 { 12 } else { 16 };
        // The 4 by 4 array of control points, in f64.
        let mut p = [[(0.0f64, 0.0f64); 4]; 4];
        for (k, &(i, j)) in SLOT.iter().enumerate().take(total) {
            if let Some(cell) = p.get_mut(i).and_then(|col| col.get_mut(j)) {
                let (x, y) = patch.pts.get(k).copied().unwrap_or((0.0, 0.0));
                let (x, y) = to_device(x, y);
                *cell = (f64::from(x), f64::from(y));
            }
        }
        if self.ty == 6 {
            coons_inside(&mut p);
        }
        self.draw_surface(&p, &patch.cols, canvas)
    }

    fn draw_surface(&self, p: &[[(f64, f64); 4]; 4], cols: &[[f32; 3]; 4], canvas: &mut Canvas<'_>) -> Result<(), String> {
        // Cut into squares of about 4 pixels, at most MAX_PATCH_DIVISIONS a side.
        let (mut lx, mut ly, mut hx, mut hy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for &(x, y) in p.iter().flatten() {
            lx = lx.min(x);
            hx = hx.max(x);
            ly = ly.min(y);
            hy = hy.max(y);
        }
        if !(hx - lx).is_finite() || !(hy - ly).is_finite() {
            return Ok(());
        }
        if hx < 0.0 || hy < 0.0 || lx > canvas.w as f64 || ly > canvas.h as f64 {
            // Entirely outside the bitmap (the surface stays inside the hull of its control points).
            return Ok(());
        }
        let n = (((hx - lx).max(hy - ly) / 4.0).ceil() as usize).clamp(1, MAX_PATCH_DIVISIONS);
        if !canvas.work.charge(((n + 1) * (n + 1)) as f64 * cost::MESH_GRID_POINT) {
            return Err(OUT_OF_WORK.to_string());
        }
        let bern = |t: f64| {
            let s = 1.0 - t;
            [s * s * s, 3.0 * t * s * s, 3.0 * t * t * s, t * t * t]
        };
        let weights: Vec<[f64; 4]> = (0..=n).map(|i| bern(i as f64 / n as f64)).collect();
        let mut grid: Vec<Vertex> = Vec::with_capacity((n + 1) * (n + 1));
        // The corners: c1 at (u, v) = (0, 0), c2 at (0, 1), c3 at (1, 1), c4 at (1, 0).
        let [c1, c2, c3, c4] = *cols;
        for (iu, bu) in weights.iter().enumerate() {
            for (iv, bv) in weights.iter().enumerate() {
                let (mut x, mut y) = (0.0, 0.0);
                for (i, col) in p.iter().enumerate() {
                    for (j, &(px, py)) in col.iter().enumerate() {
                        let wgt = bu.get(i).copied().unwrap_or(0.0) * bv.get(j).copied().unwrap_or(0.0);
                        x += px * wgt;
                        y += py * wgt;
                    }
                }
                let (u, v) = (iu as f32 / n as f32, iv as f32 / n as f32);
                let mut c = [0.0f32; 3];
                for (k, slot) in c.iter_mut().enumerate() {
                    let g = |col: [f32; 3]| col.get(k).copied().unwrap_or(0.0);
                    *slot = (1.0 - u) * (1.0 - v) * g(c1) + (1.0 - u) * v * g(c2) + u * v * g(c3) + u * (1.0 - v) * g(c4);
                }
                grid.push(Vertex { x: x as f32, y: y as f32, c });
            }
        }
        let at = |iu: usize, iv: usize| grid.get(iu * (n + 1) + iv).copied();
        for iu in 0..n {
            for iv in 0..n {
                let (Some(a), Some(b), Some(c), Some(d)) = (at(iu, iv), at(iu + 1, iv), at(iu, iv + 1), at(iu + 1, iv + 1)) else { continue };
                canvas.triangle(&a, &b, &c)?;
                canvas.triangle(&b, &c, &d)?;
            }
        }
        Ok(())
    }
}

/// The four inside control points of a Coons patch from its boundary (8.7.4.5.8).
fn coons_inside(p: &mut [[(f64, f64); 4]; 4]) {
    let g = |p: &[[(f64, f64); 4]; 4], i: usize, j: usize| p.get(i).and_then(|c| c.get(j)).copied().unwrap_or((0.0, 0.0));
    let (p00, p01, p02, p03) = (g(p, 0, 0), g(p, 0, 1), g(p, 0, 2), g(p, 0, 3));
    let (p10, p13, p20, p23) = (g(p, 1, 0), g(p, 1, 3), g(p, 2, 0), g(p, 2, 3));
    let (p30, p31, p32, p33) = (g(p, 3, 0), g(p, 3, 1), g(p, 3, 2), g(p, 3, 3));
    let f = |a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64), e: (f64, f64), g: (f64, f64), h: (f64, f64), k: (f64, f64)| {
        // [-4 a + 6 (b + c) - 2 (d + e) + 3 (g + h) - k] / 9
        (
            (-4.0 * a.0 + 6.0 * (b.0 + c.0) - 2.0 * (d.0 + e.0) + 3.0 * (g.0 + h.0) - k.0) / 9.0,
            (-4.0 * a.1 + 6.0 * (b.1 + c.1) - 2.0 * (d.1 + e.1) + 3.0 * (g.1 + h.1) - k.1) / 9.0,
        )
    };
    let p11 = f(p00, p01, p10, p03, p30, p31, p13, p33);
    let p12 = f(p03, p02, p13, p00, p33, p32, p10, p30);
    let p21 = f(p30, p31, p20, p33, p00, p01, p23, p03);
    let p22 = f(p33, p32, p23, p30, p03, p02, p20, p00);
    for ((i, j), v) in [((1, 1), p11), ((1, 2), p12), ((2, 1), p21), ((2, 2), p22)] {
        if let Some(cell) = p.get_mut(i).and_then(|c| c.get_mut(j)) {
            *cell = v;
        }
    }
}
