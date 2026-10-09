//! Offscreen layers and what is drawn on them (ISO 32000-1 11 and 8.7): transparency groups (11.4), soft masks
//! (11.6.5), knockout (11.4.6.2), the cells of tiling patterns (8.7.3), shading patterns and `sh` (8.7.4), and
//! optional content that is hidden (8.11).
//!
//! Every layer is a bitmap the size of the part of the device it can show, and is paid for four ways: it
//! counts against the bytes of layers alive at once ([`MAX_LIVE_LAYER_BYTES`]), against the pixels of layers
//! the page may make in all ([`MAX_LAYER_PIXELS`], and the painted area), against the page's work meter
//! (`work.rs`: the pixels made, composited, shaded and masked, in proportion to their real cost), and it nests at
//! most [`MAX_LAYER_DEPTH`] deep. A layer refused is not drawn (the content goes straight on the page, or is
//! skipped), with a warning. The content of a pattern cell or a group is run on the page's own interpreter,
//! so its operators, path segments and painted area come out of the same page allowances. A page that has no
//! work left starts nothing more and keeps what it has drawn.

use super::super::work::cost;
use super::*;
use tiny_skia::{Pattern, SpreadMode};

/// Most layers nested (a group in a group, a soft mask group, a pattern cell).
pub(super) const MAX_LAYER_DEPTH: usize = 12;
/// Bytes of layers alive at once.
pub(super) const MAX_LIVE_LAYER_BYTES: usize = 64 * 1024 * 1024;
/// Pixels of layers (groups, soft masks) a page may make in all (about 45 full pages at 150 dpi).
pub(super) const MAX_LAYER_PIXELS: u64 = 100_000_000;
/// Pixels of a pattern cell, and of the cells a page may make in all.
const MAX_TILE_PIXELS: u64 = 1 << 22;
pub(super) const MAX_TILE_PIXELS_PER_PAGE: u64 = 1 << 26;
/// How many times the content of a cell is run to cover the neighbours that overlap it.
const MAX_TILE_PASSES: f64 = 16.0;
/// How many times a cell may repeat in what it fills.
const MAX_TILE_REPEATS: f64 = 16_777_216.0;
/// Bytes of pattern cells kept, and how many.
const MAX_TILE_CACHE_BYTES: usize = 16 * 1024 * 1024;
const MAX_TILES: usize = 64;
/// Soft masks kept for the next `gs` that asks for the same (how many, and their bytes), and shading bitmaps kept
/// for the next `sh` or fill (how many, and their bytes).
const MAX_SMASKS: usize = 4;
const MAX_SMASK_CACHE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SHADES: usize = 4;
/// Masks (clip times soft mask) kept for the next object that draws under the same two: how many, and their bytes.
const MAX_SM_CACHE: usize = 4;
const MAX_SM_CACHE_BYTES: usize = 16 * 1024 * 1024;
const MAX_SHADE_CACHE_BYTES: usize = 16 * 1024 * 1024;
/// Shadings written in a resource dictionary that are kept for the page.
const MAX_PAGE_SHADINGS: usize = 256;

/// Counts `bytes` in `live` for as long as it exists.
pub(super) struct LiveGuard {
    live: Rc<Cell<usize>>,
    bytes: usize,
}

impl LiveGuard {
    fn new(live: &Rc<Cell<usize>>, bytes: usize) -> LiveGuard {
        live.set(live.get().saturating_add(bytes));
        LiveGuard { live: live.clone(), bytes }
    }
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.live.set(self.live.get().saturating_sub(self.bytes));
    }
}

/// An offscreen bitmap, counted as alive while it is.
pub(super) struct Layer {
    pub pixmap: Pixmap,
    pub guard: LiveGuard,
}

/// A soft mask (11.6.5.1): values for the pixels of `rect` (left, top, right, bottom of the layer it was made in),
/// and `outside` for every other pixel.
pub(super) struct SoftMask {
    rect: [i32; 4],
    data: Vec<u8>,
    outside: u8,
    _guard: LiveGuard,
}

impl SoftMask {
    fn value(&self, x: i32, y: i32) -> u8 {
        let [x0, y0, x1, y1] = self.rect;
        if x < x0 || x >= x1 || y < y0 || y >= y1 {
            return self.outside;
        }
        self.data.get((y - y0) as usize * (x1 - x0) as usize + (x - x0) as usize).copied().unwrap_or(self.outside)
    }
}

/// What makes one soft mask the same as another, for the ones kept for the next `gs`.
#[derive(PartialEq)]
pub(super) struct SmKey {
    dict: Dict,
    ctm: [u64; 6],
    origin: [u64; 2],
    rect: [i32; 4],
    size: (u32, u32),
}

/// A shading drawn into a bitmap, kept for the next `sh` or fill that asks for the same: the shading (held, so that
/// its address means it), where it was drawn and whether the background was, and the bitmap.
pub(super) struct ShadeEntry {
    shading: Rc<Shading>,
    matrix: [u64; 6],
    area: [i32; 4],
    background: bool,
    pix: Rc<Pixmap>,
    _guard: LiveGuard,
}

/// A pattern as the page names it (8.7.3).
pub(super) enum PatternDef {
    Tiling(Rc<TilingDef>),
    Shading { shading: Rc<Shading>, matrix: Matrix },
}

pub(super) struct TilingDef {
    obj: ObjRef,
    /// 1: the cell has colours of its own; 2: it is a stencil painted in the colour `scn` gives.
    paint_type: i64,
    bbox: [f64; 4],
    xstep: f64,
    ystep: f64,
    matrix: Matrix,
    content: Vec<u8>,
    resources: Option<Rc<Resources>>,
}

/// A pattern chosen as a colour: where its space lies on the device, and the colour of an uncoloured one.
pub(super) struct PatternPaint {
    def: Rc<PatternDef>,
    /// Pattern space to device space.
    matrix: Matrix,
    colour: [f32; 3],
}

/// One cell of a tiling pattern drawn once. A texel is `xstep / w` by `ystep / h` of pattern space.
pub(super) struct Tile {
    pix: Rc<Pixmap>,
    /// Pattern space of the cell's corner.
    origin: (f64, f64),
    bytes: usize,
}

#[derive(PartialEq)]
pub(super) struct TileKey {
    obj: ObjRef,
    matrix: [i64; 4],
    colour: [u8; 3],
}

/// What a fill or stroke paints with, made for the part of the device it covers.
pub(super) enum Ink {
    Solid([f32; 3]),
    /// A bitmap placed on the device by `ts`: a shading as it falls there, or a cell repeated.
    Raster { pix: Rc<Pixmap>, ts: Transform, repeat: bool, bilinear: bool, _guard: Option<LiveGuard> },
    /// Nothing can be painted (the pattern could not be made).
    Nothing,
}

/// The state of the interpreter when it goes to draw on another layer.
pub(super) struct Frame {
    pixmap: Pixmap,
    base: Matrix,
    pattern_base: Matrix,
    gs: GState,
    stack_len: usize,
    stack_floor: usize,
    tm: Matrix,
    tlm: Matrix,
    path: PathData,
    pending_clip: Option<FillRule>,
    uncolored: bool,
    knockout: bool,
    hidden: usize,
    marked: (usize, usize),
    text_clip: Option<PathBuilder>,
    text_clip_segs: usize,
}

impl Frame {
    /// The CTM of the drawing it put away.
    fn ctm(&self) -> Matrix {
        self.gs.ctm
    }
}

fn bits(m: &Matrix) -> [u64; 6] {
    m.map(f64::to_bits)
}

/// Brightness of an RGB colour, the way the other viewers take it (30, 59, 11 per cent).
fn luminosity(r: u32, g: u32, b: u32) -> u32 {
    (30 * r + 59 * g + 11 * b + 50) / 100
}

impl Interp<'_> {
    // --- layers ----------------------------------------------------------------------------------------

    /// A transparent bitmap of `w` by `h`, if the page may still have one. `Ok(None)`, with a warning, when it may not.
    pub(super) fn alloc_layer(&mut self, w: u32, h: u32) -> Result<Option<Layer>> {
        if w == 0 || h == 0 {
            return Ok(None);
        }
        let px = u64::from(w) * u64::from(h);
        let bytes = usize::try_from(px.saturating_mul(4)).unwrap_or(usize::MAX);
        if self.layer_depth >= MAX_LAYER_DEPTH {
            self.warn(format!("transparency groups, soft masks and patterns are nested more than {MAX_LAYER_DEPTH} deep; the deeper ones are not drawn as layers"));
            return Ok(None);
        }
        if self.live_layers.get().saturating_add(bytes) > MAX_LIVE_LAYER_BYTES {
            self.warn("transparency layers use more memory than is allowed; some groups, soft masks and patterns are not drawn as layers");
            return Ok(None);
        }
        if self.layer_pixels_left < px {
            self.warn("the page makes more transparency layers than is allowed; the rest are not drawn as layers");
            return Ok(None);
        }
        if !self.work.charge(px as f64 * cost::LAYER) {
            return Ok(None);
        }
        self.charge_area(2.0 * px as f64)?;
        self.layer_pixels_left -= px;
        let Some(pixmap) = Pixmap::new(w, h) else {
            self.warn("a transparency layer is too large to make");
            return Ok(None);
        };
        Ok(Some(Layer { pixmap, guard: LiveGuard::new(&self.live_layers, bytes) }))
    }

    /// Start drawing on `pixmap`, whose top left corner is the device pixel `origin` of the current layer. Everything
    /// that is the current drawing's own is put away; `leave_frame` brings it back. The new layer starts with the
    /// graphics state it is given by the caller (this keeps the current one, with a clip of the whole layer).
    pub(super) fn enter_frame(&mut self, pixmap: Pixmap, origin: (i32, i32)) -> Frame {
        let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
        let frame = Frame {
            pixmap: std::mem::replace(&mut self.pixmap, pixmap),
            base: self.base,
            pattern_base: self.pattern_base,
            gs: self.gs.clone(),
            stack_len: self.stack.len(),
            stack_floor: self.stack_floor,
            tm: self.tm,
            tlm: self.tlm,
            path: std::mem::take(&mut self.path),
            pending_clip: self.pending_clip.take(),
            uncolored: self.uncolored,
            knockout: self.knockout,
            hidden: self.hidden,
            marked: self.enter_marked_content(),
            text_clip: self.text_clip.take(),
            text_clip_segs: self.text_clip_segs,
        };
        for m in [&mut self.base, &mut self.pattern_base] {
            m[4] -= f64::from(origin.0);
            m[5] -= f64::from(origin.1);
        }
        self.gs.clip = Rc::new(Clip::new([0, 0, w, h], None));
        self.stack_floor = self.stack.len();
        self.knockout = false;
        self.hidden = 0;
        self.text_clip_segs = 0;
        self.sm_cache.clear();
        self.layer_depth += 1;
        frame
    }

    /// Back to the drawing `enter_frame` left; the layer that was drawn on is given back.
    pub(super) fn leave_frame(&mut self, f: Frame) -> Pixmap {
        let layer = std::mem::replace(&mut self.pixmap, f.pixmap);
        self.base = f.base;
        self.pattern_base = f.pattern_base;
        self.stack.truncate(f.stack_len);
        self.stack_floor = f.stack_floor;
        self.gs = f.gs;
        self.tm = f.tm;
        self.tlm = f.tlm;
        self.path = f.path;
        self.pending_clip = f.pending_clip;
        self.uncolored = f.uncolored;
        self.knockout = f.knockout;
        self.leave_marked_content(f.marked);
        self.hidden = f.hidden;
        self.text_clip = f.text_clip;
        self.text_clip_segs = f.text_clip_segs;
        self.sm_cache.clear();
        self.layer_depth = self.layer_depth.saturating_sub(1);
        layer
    }

    // --- transparency groups ---------------------------------------------------------------------------

    /// Does this group have to be drawn on a layer of its own (11.4)? One that is not isolated and not knockout and is
    /// drawn with Normal blend mode, no soft mask and full opacity gives the same picture as its objects drawn
    /// one by one. An isolated one differs only if something in it blends (11.4.5); a form that does not say
    /// so is taken to blend only if the graphics state dictionaries of its resources ask for a blend mode.
    /// Inside a knockout group a group is one object that replaces what is under it (11.4.6.2), so it needs a layer
    /// whatever else it is.
    pub(super) fn group_needs_layer(&self, g: &Group, resources: &Rc<Resources>) -> bool {
        self.gs.fill_alpha < 0.999 || self.gs.blend != BlendMode::SourceOver || self.gs.smask.is_some() || g.knockout || self.knockout || (g.isolated && resources.has_blend(self.doc))
    }

    /// Run a form that is a group on a layer of its own and put the result on the page with the opacity, blend mode
    /// and soft mask in force when it was drawn; inside it they start again (11.4.8, 11.6.6).
    pub(super) fn run_group(&mut self, form: &Rc<Form>, inner: &Rc<Resources>, depth: usize, group: Group) -> Result<()> {
        let rect = self.gs.clip.rect;
        let (w, h) = ((rect[2] - rect[0]).max(0) as u32, (rect[3] - rect[1]).max(0) as u32);
        let Some(Layer { pixmap, guard }) = self.alloc_layer(w, h)? else {
            // Drawn straight on the page, as if it were no group.
            return self.run(&form.content, inner, depth);
        };
        let (alpha, blend) = (self.gs.fill_alpha, self.gs.blend);
        let parent_knockout = self.knockout;
        let frame = self.enter_frame(pixmap, (rect[0], rect[1]));
        self.gs.fill_alpha = 1.0;
        self.gs.stroke_alpha = 1.0;
        self.gs.blend = BlendMode::SourceOver;
        self.gs.smask = None;
        self.knockout = group.knockout;
        let result = self.run(&form.content, inner, depth);
        let layer = self.leave_frame(frame);
        result?;
        // Through the clip (the form's box is part of it) and the soft mask that were in force.
        let mask = self.mask_for(true);
        let done = mask.and_then(|mask| self.composite(&layer, (rect[0], rect[1]), alpha, blend, as_mask(&mask), parent_knockout));
        drop(guard);
        done
    }

    /// Put a layer on the page at (`ox`, `oy`): with `alpha`, the blend mode and the mask. A knockout parent
    /// takes the layer in place of what was there (11.4.6.2).
    pub(super) fn composite(&mut self, src: &Pixmap, at: (i32, i32), alpha: f32, blend: BlendMode, mask: Option<&Mask>, knock: bool) -> Result<()> {
        let (ox, oy) = at;
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let (sw, sh) = (src.width() as i32, src.height() as i32);
        let (x0, y0) = (ox.max(0), oy.max(0));
        let (x1, y1) = (ox.saturating_add(sw).min(pw), oy.saturating_add(sh).min(ph));
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        let pixels = f64::from(x1 - x0) * f64::from(y1 - y0);
        self.charge_area(pixels)?;
        // (Done even when the page has no work left: the layer was paid for and what is in it is kept.)
        self.work.spend(pixels * if blend == BlendMode::SourceOver { cost::COMPOSITE } else { cost::COMPOSITE_BLEND });
        if blend != BlendMode::SourceOver {
            if knock {
                self.knock_out_layer(src, ox, oy);
            }
            let paint = PixmapPaint { opacity: alpha, blend_mode: blend, quality: FilterQuality::Nearest };
            self.pixmap.draw_pixmap(ox, oy, src.as_ref(), &paint, Transform::identity(), mask);
            return Ok(());
        }
        let k = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let (pwu, swu, n) = (pw as usize, sw as usize, (x1 - x0) as usize);
        let src_data = src.data();
        let mask_data = mask.map(Mask::data);
        let dst = self.pixmap.data_mut();
        for y in y0..y1 {
            let s_at = ((y - oy) as usize * swu + (x0 - ox) as usize) * 4;
            let d_at = (y as usize * pwu + x0 as usize) * 4;
            let (Some(srow), Some(drow)) = (src_data.get(s_at..s_at + n * 4), dst.get_mut(d_at..d_at + n * 4)) else { continue };
            let mrow = mask_data.and_then(|m| m.get(y as usize * pwu + x0 as usize..y as usize * pwu + x0 as usize + n));
            for (i, (sp, dp)) in srow.chunks_exact(4).zip(drow.chunks_exact_mut(4)).enumerate() {
                let &[sr, sg, sb, sa] = sp else { continue };
                if sa == 0 {
                    continue;
                }
                let mut f = k;
                if let Some(m) = mrow {
                    f = (f * u32::from(m.get(i).copied().unwrap_or(0)) + 127) / 255;
                }
                if f == 0 {
                    continue;
                }
                let scale = |v: u8| if f == 255 { u32::from(v) } else { (u32::from(v) * f + 127) / 255 };
                let (r, g, b, a) = (scale(sr), scale(sg), scale(sb), scale(sa));
                if a == 255 || knock {
                    dp.copy_from_slice(&[r as u8, g as u8, b as u8, a as u8]);
                } else {
                    for (slot, v) in dp.iter_mut().zip([r, g, b, a]) {
                        *slot = (v + (u32::from(*slot) * (255 - a) + 127) / 255).min(255) as u8;
                    }
                }
            }
        }
        Ok(())
    }

    /// Clear the pixels a layer covers (where its alpha is not zero).
    fn knock_out_layer(&mut self, src: &Pixmap, ox: i32, oy: i32) {
        let (pw, ph) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let sw = src.width() as usize;
        let dst = self.pixmap.data_mut();
        for (sy, srow) in src.data().chunks_exact(sw * 4).enumerate() {
            let y = oy.saturating_add(sy as i32);
            if y < 0 || y >= ph {
                continue;
            }
            for (sx, sp) in srow.chunks_exact(4).enumerate() {
                let x = ox.saturating_add(sx as i32);
                if x < 0 || x >= pw || sp.get(3) == Some(&0) {
                    continue;
                }
                let at = (y as usize * pw as usize + x as usize) * 4;
                if let Some(d) = dst.get_mut(at..at + 4) {
                    d.fill(0);
                }
            }
        }
    }

    /// In a knockout group an object replaces what is under it (11.4.6.2): take away the part of the layer its
    /// shape covers before it is drawn. With `/AIS` the alpha constant is the shape: it takes away that much.
    pub(super) fn knock_out_fill(&mut self, path: &tiny_skia::Path, rule: FillRule, mask: Option<&Mask>, alpha: f32) {
        let mut clear = self.paint_of([0.0; 3], if self.gs.ais { alpha } else { 1.0 });
        clear.blend_mode = BlendMode::DestinationOut;
        self.pixmap.fill_path(path, &clear, rule, Transform::identity(), mask);
    }

    // --- masks -----------------------------------------------------------------------------------------

    /// The mask to draw with: the clip's, times the soft mask's; none when the clip is a rectangle that contains
    /// everything drawn and there is no soft mask. With a soft mask a mask of the whole layer is made (and paid for in
    /// full, to the page's painted area and to its work); a page with no work left gets a mask that draws nothing.
    pub(super) fn mask_for(&mut self, inside: bool) -> Result<Option<Rc<MaskBuf>>> {
        let clip = self.gs.clip.clone();
        let (w, h) = (self.pixmap.width(), self.pixmap.height());
        let Some(sm) = self.gs.smask.clone() else {
            if clip.mask.is_none() && inside {
                return Ok(None);
            }
            return Ok(clip.mask_for(w, h, &self.live_masks));
        };
        if let Some((c, s, m)) = self.sm_cache.iter().find(|(c, s, _)| Rc::ptr_eq(c, &clip) && Rc::ptr_eq(s, &sm)).map(|(c, s, m)| (c.clone(), s.clone(), m.clone())) {
            // (The newest is found first next time.)
            self.sm_cache.retain(|(oc, os, _)| !(Rc::ptr_eq(oc, &c) && Rc::ptr_eq(os, &s)));
            self.sm_cache.push((c, s, m.clone()));
            return Ok(Some(m));
        }
        // Only the part of the page the clip leaves is looked at; a mask that is zero outside its box leaves the rest zero.
        let [cx0, cy0, cx1, cy1] = clip.rect;
        let (cx0, cy0) = (cx0.max(0), cy0.max(0));
        let (cx1, cy1) = (cx1.min(w as i32), cy1.min(h as i32));
        let (rows, cols) = ((cy1 - cy0).max(0) as usize, (cx1 - cx0).max(0) as usize);
        let region = rows as f64 * cols as f64;
        self.charge_area(region)?;
        if !self.work.charge(f64::from(w) * f64::from(h) * cost::MASK_CLEAR + region * cost::MASK) {
            return Ok(Mask::new(w, h).map(|m| Rc::new(MaskBuf::new(m, None))));
        }
        let Some(mut out) = Mask::new(w, h) else { return Ok(None) };
        // A clip that is a rectangle lets everything inside it through (the part looked at is the rectangle).
        let base = clip.mask.clone();
        let width = w as usize;
        let src = base.as_ref().map(|b| b.mask.data());
        let wanted = if sm.outside == 0 { [sm.rect[0].max(cx0), sm.rect[1].max(cy0), sm.rect[2].min(cx1), sm.rect[3].min(cy1)] } else { [cx0, cy0, cx1, cy1] };
        for y in wanted[1]..wanted[3] {
            let at = y as usize * width;
            let Some(drow) = out.data_mut().get_mut(at..at + width) else { continue };
            let srow = match src {
                Some(s) => match s.get(at..at + width) {
                    Some(row) => Some(row),
                    None => continue,
                },
                None => None,
            };
            for x in wanted[0]..wanted[2] {
                let xi = x as usize;
                let Some(d) = drow.get_mut(xi) else { continue };
                let b = match srow {
                    Some(row) => row.get(xi).copied().unwrap_or(0),
                    None => 255,
                };
                if b != 0 {
                    *d = ((u32::from(b) * u32::from(sm.value(x, y)) + 127) / 255) as u8;
                }
            }
        }
        let bytes = width * h as usize;
        if self.live_masks.get().saturating_add(bytes) > MAX_LIVE_MASK_BYTES {
            return Ok(Some(Rc::new(MaskBuf::new(out, None))));
        }
        let rc = Rc::new(MaskBuf::new(out, Some(&self.live_masks)));
        self.sm_cache.push((clip, sm, rc.clone()));
        let mut kept = self.sm_cache.iter().map(|(_, _, m)| m.mask.data().len()).sum::<usize>();
        while self.sm_cache.len() > 1 && (self.sm_cache.len() > MAX_SM_CACHE || kept > MAX_SM_CACHE_BYTES) {
            let (_, _, old) = self.sm_cache.remove(0);
            kept = kept.saturating_sub(old.mask.data().len());
        }
        Ok(Some(rc))
    }

    /// `/SMask` of a graphics state dictionary (11.6.5.1): draw the group `/G` on a layer, with the CTM now in force,
    /// and keep what it makes of alpha or brightness, through `/TR`, as a mask.
    pub(super) fn make_soft_mask(&mut self, sd: &Dict, res: &Rc<Resources>, depth: usize) -> Result<Option<Rc<SoftMask>>> {
        let doc = self.doc;
        let Some(gref) = sd.get("G").and_then(Object::as_obj_ref) else { return Ok(None) };
        let luminosity = sd.get_name("S").is_some_and(|s| s.as_bytes() == b"Luminosity");
        let group_ref = Object::Ref(gref);
        let Ok(Some(group)) = self.work.resolve(doc, &group_ref) else { return Ok(None) };
        let Object::Stream(stream) = &*group else { return Ok(None) };
        let Some(form) = self.load_form(stream, Some(gref))? else { return Ok(None) };
        // Where the group can show: its box under the matrices, inside the clip.
        let m = mul(&mul(&form.matrix, &self.gs.ctm), &self.base);
        let clip = self.gs.clip.rect;
        let rect = match form.bbox {
            Some([x0, y0, x1, y1]) if finite(&m) => {
                let corners = [apply(&m, x0, y0), apply(&m, x1, y0), apply(&m, x1, y1), apply(&m, x0, y1)];
                let lo = corners.iter().fold((f64::MAX, f64::MAX), |a, c| (a.0.min(c.0), a.1.min(c.1)));
                let hi = corners.iter().fold((f64::MIN, f64::MIN), |a, c| (a.0.max(c.0), a.1.max(c.1)));
                let r = |v: f64| v.clamp(-1e7, 1e7);
                [(r(lo.0).floor() as i32).max(clip[0]), (r(lo.1).floor() as i32).max(clip[1]), (r(hi.0).ceil() as i32).min(clip[2]), (r(hi.1).ceil() as i32).min(clip[3])]
            }
            _ => clip,
        };
        let key = SmKey {
            dict: sd.clone(),
            ctm: bits(&self.gs.ctm),
            origin: [self.base[4].to_bits(), self.base[5].to_bits()],
            rect,
            size: (self.pixmap.width(), self.pixmap.height()),
        };
        if let Some(at) = self.smasks.iter().position(|(k, _)| *k == key) {
            // Newest last: the one found is the newest now.
            let hit = self.smasks.remove(at);
            let mask = hit.1.clone();
            self.smasks.push(hit);
            return Ok(Some(mask));
        }
        // Backdrop colour (default black) in the group's colour space, and the transfer function.
        let backdrop = self.mask_backdrop(sd, &stream.dict, res);
        let transfer = self.mask_transfer(sd);
        let outside_value = |v: u8| transfer.as_ref().map_or(v, |t| t.get(usize::from(v)).copied().unwrap_or(v));
        let outside = if luminosity { outside_value(luminosity_of(backdrop)) } else { outside_value(0) };
        let (w, h) = ((rect[2] - rect[0]).max(0) as u32, (rect[3] - rect[1]).max(0) as u32);
        let mut data = Vec::new();
        let mut guard = None;
        if w > 0 && h > 0 {
            // The group's pixels are turned into mask values (the layer itself is paid for by `alloc_layer`).
            if !self.work.charge(f64::from(w) * f64::from(h) * cost::SOFT_MASK) {
                return Ok(None);
            }
            let Some(Layer { pixmap, guard: layer_guard }) = self.alloc_layer(w, h)? else {
                self.warn("a soft mask could not be made; it is left out");
                return Ok(None);
            };
            let frame = self.enter_frame(pixmap, (rect[0], rect[1]));
            // The group starts with a state of its own, the CTM of the `gs` excepted (11.6.5.1).
            let ctm = frame.ctm();
            self.gs = initial_gstate(w as i32, h as i32, &self.gray);
            self.gs.ctm = ctm;
            let result = self.run_form(gref, &form, res, depth);
            let layer = self.leave_frame(frame);
            result?;
            let bytes = (w as usize) * (h as usize);
            guard = Some(LiveGuard::new(&self.live_layers, bytes));
            data.reserve_exact(bytes);
            for px in layer.data().chunks_exact(4) {
                let &[r, g, b, a] = px else { continue };
                let v = if luminosity {
                    // The group over its backdrop colour, then the brightness of that (premultiplied: the backdrop shows through 1 - alpha).
                    let behind = |c: u8, bc: f32| u32::from(c) + ((bc.clamp(0.0, 1.0) * 255.0 + 0.5) as u32 * (255 - u32::from(a)) + 127) / 255;
                    luminosity_of_u32(behind(r, backdrop[0]), behind(g, backdrop[1]), behind(b, backdrop[2]))
                } else {
                    a
                };
                data.push(outside_value(v));
            }
            drop(layer_guard);
        }
        let mask = Rc::new(SoftMask { rect: if data.is_empty() { [0, 0, 0, 0] } else { rect }, data, outside, _guard: guard.unwrap_or_else(|| LiveGuard::new(&self.live_layers, 0)) });
        // Keep it, with the few made before it, for a `gs` that asks for the same (their bytes are kept in check).
        self.smasks.push((key, mask.clone()));
        let mut kept: usize = self.smasks.iter().map(|(_, m)| m.data.len()).sum();
        while self.smasks.len() > 1 && (self.smasks.len() > MAX_SMASKS || kept > MAX_SMASK_CACHE_BYTES) {
            let (_, old) = self.smasks.remove(0);
            kept = kept.saturating_sub(old.data.len());
        }
        Ok(Some(mask))
    }

    /// `/BC` of a soft mask in the colour space of its group, as RGB (black when there is none).
    fn mask_backdrop(&mut self, sd: &Dict, group_form: &Dict, res: &Rc<Resources>) -> [f32; 3] {
        let doc = self.doc;
        let backdrop = self.entry(sd, "BC");
        let Some(Object::Array(bc)) = backdrop.as_deref() else { return [0.0; 3] };
        let comps: Vec<f64> = bc.iter().filter_map(|o| self.work.read(doc, o).and_then(|o| o.as_f64())).collect();
        let lookup = |n: &[u8]| res.get(doc, Cat::ColorSpace, n);
        let space = group_form
            .get("Group")
            .and_then(|g| self.work.read(doc, g))
            .and_then(|g| g.as_dict().and_then(|g| g.get("CS").cloned()))
            .and_then(|cs| ColorSpace::load(doc, &cs, &lookup, &self.meter))
            .filter(|s| s.components() == comps.len());
        let space = space.unwrap_or_else(|| match comps.len() {
            1 => self.gray.clone(),
            4 => self.cmyk.clone(),
            _ => self.rgb.clone(),
        });
        space.to_rgb(&comps, &self.meter)
    }

    /// `/TR` of a soft mask as a table of 256 (`None` for `/Identity` or when there is none).
    fn mask_transfer(&mut self, sd: &Dict) -> Option<Vec<u8>> {
        let doc = self.doc;
        let obj = sd.get("TR")?;
        if matches!(&*self.work.read(doc, obj)?, Object::Name(_)) {
            return None;
        }
        let f = super::super::func::Function::load(doc, obj)?;
        let mut out = Vec::new();
        let mut table = Vec::with_capacity(256);
        for i in 0..256 {
            if !f.eval(&[f64::from(i) / 255.0], &mut out, &self.meter) {
                return None;
            }
            table.push((out.first().copied().unwrap_or(0.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
        Some(table)
    }

    // --- shadings and patterns -------------------------------------------------------------------------

    /// A shading by object, read once however often the page names it. One written in a resource dictionary (`name` is
    /// its name there) is read once for the page. Reading is charged to the page's work (a mesh is read from its bits).
    fn load_shading(&mut self, obj: &Object, res: &Rc<Resources>, name: Option<&[u8]>) -> Option<Rc<Shading>> {
        let key = obj.as_obj_ref();
        if let Some(r) = key
            && let Some(hit) = self.shared.shadings.get(&r)
        {
            return hit.clone();
        }
        let direct = match (key, name) {
            (None, Some(n)) => Some((res.id, n.to_vec())),
            _ => None,
        };
        if let Some(d) = &direct
            && let Some(hit) = self.page_shadings.get(d)
        {
            return hit.clone();
        }
        let doc = self.doc;
        let refused = self.meter.was_refused();
        let loaded = {
            let lookup = |n: &[u8]| res.get(doc, Cat::ColorSpace, n);
            Shading::load(doc, obj, &lookup, &self.meter, &mut self.work)
        };
        let loaded = match loaded {
            Ok(s) => Some(Rc::new(s)),
            Err(message) => {
                self.warn(format!("a shading is skipped: {message}"));
                None
            }
        };
        // A shading made while the page was out of work, or of colour conversion steps, may be less than it is: not kept.
        if self.work.is_over() || (!refused && self.meter.was_refused()) {
            return loaded;
        }
        if let Some(r) = key {
            let bytes = loaded.as_ref().map_or(0, |s| s.bytes);
            if self.shared.cache_bytes.saturating_add(bytes) <= MAX_CACHE_BYTES {
                self.shared.cache_bytes += bytes;
                self.shared.shadings.insert(r, loaded.clone());
            }
        } else if let Some(d) = direct {
            if self.page_shadings.len() >= MAX_PAGE_SHADINGS {
                self.page_shadings.clear();
            }
            self.page_shadings.insert(d, loaded.clone());
        }
        loaded
    }

    /// The bitmap of a shading over `area` (left, top, right, bottom of the layer being drawn), `m` taking its space to the
    /// device: drawn, or found again from the last few. It is paid for, in the page's memory for layers and in its work
    /// (the pixels, at what its kind costs). With it comes the guard that counts its bytes while it is in use, if the
    /// bitmap is not kept. `None`, with a warning if there is something to say, when it cannot be had.
    fn shading_bitmap(&mut self, shading: &Rc<Shading>, m: &Matrix, area: [i32; 4], background: bool) -> Result<Option<(Rc<Pixmap>, Option<LiveGuard>)>> {
        let matrix = bits(m);
        if let Some(hit) = self.shade_cache.iter().find(|e| Rc::ptr_eq(&e.shading, shading) && e.matrix == matrix && e.area == area && e.background == background) {
            return Ok(Some((hit.pix.clone(), None)));
        }
        let pixels = f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]);
        let bytes = (area[2] - area[0]).max(0) as usize * (area[3] - area[1]).max(0) as usize * 4;
        if self.live_layers.get().saturating_add(bytes) > MAX_LIVE_LAYER_BYTES {
            self.warn("transparency layers use more memory than is allowed; a shading is not drawn");
            return Ok(None);
        }
        if !self.work.charge(pixels * shading.pixel_cost()) {
            return Ok(None);
        }
        self.charge_area(pixels)?;
        let guard = LiveGuard::new(&self.live_layers, bytes);
        let pix = match shading.render(m, area, background, &mut self.shading_budget, &self.meter, &mut self.work) {
            Ok(p) => Rc::new(p),
            Err(message) => {
                self.warn(format!("a shading is not drawn: {message}"));
                return Ok(None);
            }
        };
        if bytes > MAX_SHADE_CACHE_BYTES {
            return Ok(Some((pix, Some(guard))));
        }
        self.shade_cache.push(ShadeEntry { shading: shading.clone(), matrix, area, background, pix: pix.clone(), _guard: guard });
        let mut kept: usize = self.shade_cache.iter().map(|e| e.pix.data().len()).sum();
        while self.shade_cache.len() > 1 && (self.shade_cache.len() > MAX_SHADES || kept > MAX_SHADE_CACHE_BYTES) {
            let old = self.shade_cache.remove(0);
            kept = kept.saturating_sub(old.pix.data().len());
        }
        Ok(Some((pix, None)))
    }

    /// `sh` (8.7.4.2): paint the shading over the whole clip.
    pub(super) fn paint_shading(&mut self, name: &[u8], res: &Rc<Resources>) -> Result<()> {
        if self.gs.clip.is_empty() || self.hidden > 0 {
            return Ok(());
        }
        let Some(entry) = res.get(self.doc, Cat::Shading, name) else {
            self.warn(format!("shading /{} is not in the resources", String::from_utf8_lossy(name)));
            return Ok(());
        };
        let Some(shading) = self.load_shading(&entry, res, Some(name)) else { return Ok(()) };
        let m = self.full();
        if !finite(&m) {
            return Ok(());
        }
        // Inside the clip, and inside the part of the page the shading can reach.
        let mut area = self.gs.clip.rect;
        if let Some([x0, y0, x1, y1]) = shading.extent() {
            let corners = [apply(&m, x0, y0), apply(&m, x1, y0), apply(&m, x1, y1), apply(&m, x0, y1)];
            let lo = corners.iter().fold((f64::MAX, f64::MAX), |a, c| (a.0.min(c.0), a.1.min(c.1)));
            let hi = corners.iter().fold((f64::MIN, f64::MIN), |a, c| (a.0.max(c.0), a.1.max(c.1)));
            let r = |v: f64| v.clamp(-1e7, 1e7);
            area = [area[0].max(r(lo.0).floor() as i32), area[1].max(r(lo.1).floor() as i32), area[2].min(r(hi.0).ceil() as i32), area[3].min(r(hi.1).ceil() as i32)];
        }
        if area[0] >= area[2] || area[1] >= area[3] {
            return Ok(());
        }
        let Some((pix, _guard)) = self.shading_bitmap(&shading, &m, area, false)? else { return Ok(()) };
        let mask = self.mask_for(true)?;
        let (alpha, blend, knock) = (self.gs.fill_alpha, self.gs.blend, self.knockout);
        self.composite(&pix, (area[0], area[1]), alpha, blend, as_mask(&mask), knock)
    }

    /// The pattern the page names with `scn` (8.7.3.2), as a colour. `rgb` is the colour that goes with an uncoloured one.
    pub(super) fn pattern_paint(&mut self, name: &[u8], rgb: [f32; 3], res: &Rc<Resources>) -> Option<Rc<PatternPaint>> {
        let Some(entry) = res.get(self.doc, Cat::Pattern, name) else {
            self.warn(format!("pattern /{} is not in the resources", String::from_utf8_lossy(name)));
            return None;
        };
        let def = self.load_pattern(&entry, res)?;
        let own = match &*def {
            PatternDef::Tiling(t) => t.matrix,
            PatternDef::Shading { matrix, .. } => *matrix,
        };
        let matrix = mul(&own, &self.pattern_base);
        if !finite(&matrix) {
            return None;
        }
        Some(Rc::new(PatternPaint { def, matrix, colour: rgb }))
    }

    fn load_pattern(&mut self, entry: &Object, res: &Rc<Resources>) -> Option<Rc<PatternDef>> {
        let key = entry.as_obj_ref();
        if let Some(r) = key
            && let Some(hit) = self.shared.patterns.get(&r)
        {
            return hit.clone();
        }
        let loaded = self.read_pattern(entry, key, res);
        // Kept, like a form, while the bytes of what is kept stay under the cap (a tiling pattern keeps its decoded content).
        if let Some(r) = key
            && !self.work.is_over()
        {
            let bytes = match loaded.as_deref() {
                Some(PatternDef::Tiling(t)) => t.content.len() + 256,
                _ => 256,
            };
            if self.shared.cache_bytes.saturating_add(bytes) <= MAX_CACHE_BYTES {
                self.shared.cache_bytes += bytes;
                self.shared.patterns.insert(r, loaded.clone());
            }
        }
        loaded
    }

    fn read_pattern(&mut self, entry: &Object, key: Option<ObjRef>, res: &Rc<Resources>) -> Option<Rc<PatternDef>> {
        let doc = self.doc;
        let resolved = match self.work.resolve(doc, entry) {
            Ok(Some(o)) => o,
            Ok(None) => {
                if !self.work.is_over() {
                    self.warn("a pattern could not be read");
                }
                return None;
            }
            Err(e) => {
                self.warn(format!("a pattern could not be read: {e}"));
                return None;
            }
        };
        let dict = resolved.as_dict()?;
        let matrix = self.matrix_of(dict.get("Matrix"), IDENTITY);
        let number = |k: &str| self.number(dict, k).filter(|v| v.is_finite());
        match self.entry(dict, "PatternType").and_then(|o| o.as_int()) {
            Some(2) => {
                let shading = self.load_shading(dict.get("Shading")?, res, None)?;
                Some(Rc::new(PatternDef::Shading { shading, matrix }))
            }
            Some(1) => {
                let Object::Stream(stream) = &*resolved else { return None };
                let bbox = self.work.rectangle(doc, dict.get("BBox"))?;
                let bbox = [bbox[0].min(bbox[2]), bbox[1].min(bbox[3]), bbox[0].max(bbox[2]), bbox[1].max(bbox[3])];
                let (xstep, ystep) = (number("XStep").unwrap_or(bbox[2] - bbox[0]), number("YStep").unwrap_or(bbox[3] - bbox[1]));
                let paint_type = self.entry(dict, "PaintType").and_then(|o| o.as_int()).unwrap_or(1);
                let content = match doc.decode_stream(stream) {
                    Ok(c) => c,
                    Err(e) => {
                        self.warn(format!("a tiling pattern is skipped: {e}"));
                        return None;
                    }
                };
                // (Read again each time it is chosen if it is not kept: that is paid for.)
                if !self.work.charge(content.len() as f64 * cost::DECODE_BYTE) {
                    return None;
                }
                let resources = dict.get("Resources").map(|r| self.resources_of(r));
                Some(Rc::new(PatternDef::Tiling(Rc::new(TilingDef {
                    obj: key.unwrap_or(ObjRef::new(0, 0)),
                    paint_type,
                    bbox,
                    xstep,
                    ystep,
                    matrix,
                    content,
                    resources,
                }))))
            }
            _ => {
                self.warn("a pattern is neither a tiling pattern nor a shading pattern");
                None
            }
        }
    }

    /// What a pattern colour paints with over `area`.
    pub(super) fn pattern_ink(&mut self, p: &Rc<PatternPaint>, area: [i32; 4]) -> Result<Ink> {
        match &*p.def {
            PatternDef::Shading { shading, .. } => {
                // The bitmap is made (or found) for exactly this area; painting it through the shape is a pixel's work too.
                if !self.work.charge(f64::from(area[2] - area[0]) * f64::from(area[3] - area[1]) * cost::PATTERN_FILL) {
                    return Ok(Ink::Nothing);
                }
                match self.shading_bitmap(shading, &p.matrix, area, true)? {
                    Some((pix, guard)) => Ok(Ink::Raster { pix, ts: Transform::from_translate(area[0] as f32, area[1] as f32), repeat: false, bilinear: false, _guard: guard }),
                    None => Ok(Ink::Nothing),
                }
            }
            PatternDef::Tiling(t) => {
                let t = t.clone();
                self.tiling_ink(p, &t, area)
            }
        }
    }

    /// The paint for an ink.
    pub(super) fn paint_from<'p>(&self, ink: &'p Ink, alpha: f32) -> Option<Paint<'p>> {
        let mut paint = Paint::default();
        match ink {
            Ink::Solid(c) => {
                paint.set_color(Color::from_rgba(c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0), alpha.clamp(0.0, 1.0)).unwrap_or(Color::BLACK));
            }
            Ink::Raster { pix, ts, repeat, bilinear, .. } => {
                let spread = if *repeat { SpreadMode::Repeat } else { SpreadMode::Pad };
                let quality = if *bilinear { FilterQuality::Bilinear } else { FilterQuality::Nearest };
                paint.shader = Pattern::new(Pixmap::as_ref(pix), spread, quality, alpha, *ts);
            }
            Ink::Nothing => return None,
        }
        paint.anti_alias = true;
        paint.blend_mode = self.gs.blend;
        Some(paint)
    }

    /// A tiling pattern over `area`: its cell drawn once (or found again), repeated by the rasterizer (8.7.3.1).
    fn tiling_ink(&mut self, p: &PatternPaint, t: &Rc<TilingDef>, area: [i32; 4]) -> Result<Ink> {
        let pm = p.matrix;
        let (xs, ys) = (t.xstep.abs(), t.ystep.abs());
        if !(xs.is_finite() && ys.is_finite() && xs > 0.0 && ys > 0.0) {
            self.warn("a tiling pattern has a step of zero; it is not drawn");
            return Ok(Ink::Nothing);
        }
        let (sx, sy) = (pm[0].hypot(pm[1]), pm[2].hypot(pm[3]));
        if !(sx.is_finite() && sy.is_finite() && sx > 1e-12 && sy > 1e-12) {
            return Ok(Ink::Nothing);
        }
        // A cell smaller than a pixel stands for one pixel of its average colour.
        let (dx, dy) = ((xs * sx).max(1.0), (ys * sy).max(1.0));
        let (aw, ah) = (f64::from(area[2] - area[0]).max(1.0), f64::from(area[3] - area[1]).max(1.0));
        // Painting the cell over the area: a pixel's work.
        if !self.work.charge(aw * ah * cost::PATTERN_FILL) {
            return Ok(Ink::Nothing);
        }
        if (aw / dx).ceil() * (ah / dy).ceil() > MAX_TILE_REPEATS {
            self.warn("a tiling pattern repeats more often than is allowed; it is not drawn");
            return Ok(Ink::Nothing);
        }
        // A cell as many pixels as it has on the device, scaled down to fit the cap on cell pixels.
        let cap = MAX_TILE_PIXELS as f64;
        let (mut w, mut h) = (dx.round().clamp(1.0, cap), dy.round().clamp(1.0, cap));
        if w * h > cap {
            let f = (cap / (w * h)).sqrt();
            h = (h * f).floor().max(1.0);
            w = (w * f).floor().clamp(1.0, (cap / h).floor().max(1.0));
        }
        let colour = if t.paint_type == 2 { p.colour.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8) } else { [0; 3] };
        let key = TileKey { obj: t.obj, matrix: [pm[0], pm[1], pm[2], pm[3]].map(|v| (v * 1024.0).round().clamp(-9e15, 9e15) as i64), colour };
        let tile = match self.tiles.iter().find(|(k, _)| *k == key) {
            Some((_, tile)) => Some(tile.clone()),
            None => self.build_tile(t, &key, (w as u32, h as u32), p.colour)?,
        };
        let Some(tile) = tile else { return Ok(Ink::Nothing) };
        // Texel to pattern space to device space; the period is the cell's, kept to a pixel at least.
        let (vxs, vys) = (dx / sx, dy / sy);
        let (tw, th) = (f64::from(tile.pix.width()), f64::from(tile.pix.height()));
        let s: Matrix = [vxs / tw, 0.0, 0.0, vys / th, tile.origin.0, tile.origin.1];
        let m = mul(&s, &pm);
        if !finite(&m) {
            return Ok(Ink::Nothing);
        }
        let upright = pm[1].abs() <= 1e-9 * pm[0].abs().max(1e-12) && pm[2].abs() <= 1e-9 * pm[3].abs().max(1e-12);
        let near_one = (m[0].abs() - 1.0).abs() < 0.02 && (m[3].abs() - 1.0).abs() < 0.02;
        let ts = Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32);
        Ok(Ink::Raster { pix: tile.pix.clone(), ts, repeat: true, bilinear: !(upright && near_one), _guard: None })
    }

    /// Draw one cell of a tiling pattern on a bitmap of `size`: the content of the pattern, once for each of the
    /// neighbouring cells that reach into this one (the cell is a step wide; the box may be wider).
    fn build_tile(&mut self, t: &Rc<TilingDef>, key: &TileKey, size: (u32, u32), colour: [f32; 3]) -> Result<Option<Rc<Tile>>> {
        if self.patterns_in_progress.contains(&t.obj) {
            self.warn("a tiling pattern uses itself; the repeat is not drawn");
            return Ok(None);
        }
        // The cell's content nests inside the stream that uses the pattern: forms and glyphs count all the way down.
        let depth = self.form_depth + 1;
        if depth > MAX_FORM_DEPTH {
            self.warn(format!("forms, glyphs and patterns are nested more than {MAX_FORM_DEPTH} deep; the deeper ones are skipped"));
            return Ok(None);
        }
        let (xs, ys) = (t.xstep.abs(), t.ystep.abs());
        let [bx0, by0, bx1, by1] = t.bbox;
        let (nx, ny) = (((bx1 - bx0) / xs - 1e-6).ceil().max(1.0), ((by1 - by0) / ys - 1e-6).ceil().max(1.0));
        if nx * ny > MAX_TILE_PASSES {
            self.warn("the cells of a tiling pattern overlap each other too much; it is not drawn");
            return Ok(None);
        }
        let (w, h) = size;
        let passes = (nx * ny) as u64;
        let px = u64::from(w) * u64::from(h);
        if self.tile_pixels_left < px.saturating_mul(passes) {
            self.warn("the page makes more pattern cells than is allowed; the rest are not drawn");
            return Ok(None);
        }
        self.tile_pixels_left -= px.saturating_mul(passes);
        let Some(Layer { pixmap, guard }) = self.alloc_layer(w, h)? else { return Ok(None) };
        let doc = self.doc;
        let frame = self.enter_frame(pixmap, (0, 0));
        // The bitmap is the device here; the cell's content has its own CTM.
        self.base = IDENTITY;
        self.patterns_in_progress.push(t.obj);
        let (rx, ry) = (f64::from(w) / xs, f64::from(h) / ys);
        let resources = t.resources.clone().unwrap_or_else(|| Resources::new(doc, None));
        let mut result = Ok(());
        'passes: for kx in 0..nx as usize {
            for ky in 0..ny as usize {
                self.stack.truncate(self.stack_floor);
                let mut gs = initial_gstate(w as i32, h as i32, &self.gray);
                gs.ctm = [rx, 0.0, 0.0, ry, (-(kx as f64) * xs - bx0) * rx, (-(ky as f64) * ys - by0) * ry];
                if t.paint_type == 2 {
                    // An uncoloured pattern is painted in the colour `scn` gave, whatever its content says (8.7.3.3).
                    let c = Colour { space: self.rgb.clone(), rgb: colour, none: false, pattern: None };
                    gs.fill = c.clone();
                    gs.stroke = c;
                }
                // The default space of the cell's content is pattern space, which the cell's matrix puts on the bitmap.
                self.pattern_base = gs.ctm;
                self.gs = gs;
                self.uncolored = t.paint_type == 2;
                if let Err(e) = self.clip_rect_user(t.bbox) {
                    result = Err(e);
                    break 'passes;
                }
                if !self.gs.clip.is_empty()
                    && let Err(e) = self.run(&t.content, &resources, depth)
                {
                    result = Err(e);
                    break 'passes;
                }
            }
        }
        self.patterns_in_progress.pop();
        let pixmap = self.leave_frame(frame);
        result?;
        drop(guard);
        let bytes = pixmap.data().len();
        let tile = Rc::new(Tile { pix: Rc::new(pixmap), origin: (bx0, by0), bytes });
        // Keep it for the next object that uses the pattern the same way.
        while !self.tiles.is_empty() && (self.tile_bytes.saturating_add(bytes) > MAX_TILE_CACHE_BYTES || self.tiles.len() >= MAX_TILES) {
            let (_, old) = self.tiles.remove(0);
            self.tile_bytes = self.tile_bytes.saturating_sub(old.bytes);
        }
        if bytes <= MAX_TILE_CACHE_BYTES {
            self.tile_bytes += bytes;
            self.tiles.push((TileKey { obj: key.obj, matrix: key.matrix, colour: key.colour }, tile.clone()));
        }
        Ok(Some(tile))
    }

    // --- optional content ------------------------------------------------------------------------------

    /// Is the marked content that `BDC` opens hidden (8.11.3.2)? Only `/OC` property lists name optional content. The
    /// answer is kept for the membership dictionary (by object, or by name and resources, or by bytes), and what it
    /// costs is charged to the page's work.
    pub(super) fn marked_content_hidden(&mut self, ops: &[Operand], sc: &Scanner<'_>, res: &Rc<Resources>) -> bool {
        let [.., Operand::Name(tag), prop] = ops else { return false };
        if sc.bytes(*tag) != b"OC" {
            return false;
        }
        let Some(oc) = self.shared.oc.clone() else { return false };
        if !oc.is_present() {
            return false;
        }
        match prop {
            Operand::Name(n) => {
                let name = sc.bytes(*n);
                let Some(obj) = res.get(self.doc, Cat::Properties, name) else { return false };
                let key = obj.as_obj_ref().is_none().then(|| super::super::oc::direct_key(res.id, name));
                !oc.visible(self.doc, &obj, key.as_deref(), &mut self.work)
            }
            Operand::Dict(d) => !oc.visible_inline(self.doc, sc.bytes(*d), &mut self.work),
            _ => false,
        }
    }

    /// Is the XObject shown, by its `/OC` entry (8.11.3.3)? Answers are kept by object.
    pub(super) fn xobject_shown(&mut self, r: ObjRef) -> bool {
        let Some(oc) = self.shared.oc.clone() else { return true };
        if !oc.is_present() {
            return true;
        }
        if let Some(v) = self.shared.xobject_shown.get(&r) {
            return *v;
        }
        let target = Object::Ref(r);
        let held = self.work.read(self.doc, &target);
        let shown = match held.as_deref() {
            Some(Object::Stream(s)) => s.dict.get("OC").is_none_or(|o| oc.visible(self.doc, o, None, &mut self.work)),
            _ => true,
        };
        if self.work.is_over() {
            // (A page out of work may have been given a guess.)
            return shown;
        }
        if self.shared.xobject_shown.len() >= 100_000 {
            self.shared.xobject_shown.clear();
        }
        self.shared.xobject_shown.insert(r, shown);
        shown
    }

    pub(super) fn open_marked_content(&mut self, hide: bool) {
        // (No cap on the depth: a level is an operator, and a page has at most `MAX_OPERATORS`. A level that hides
        // always counts, so that nothing inside it shows.)
        self.mc_stack.push(hide);
        if hide {
            self.hidden = self.hidden.saturating_add(1);
        }
    }

    pub(super) fn close_marked_content(&mut self) {
        if self.mc_stack.len() > self.mc_floor
            && let Some(hide) = self.mc_stack.pop()
            && hide
        {
            self.hidden = self.hidden.saturating_sub(1);
        }
    }

    /// A stream (a form, a layer) starts: what it closes is only its own marked content. Returns what to restore.
    pub(super) fn enter_marked_content(&mut self) -> (usize, usize) {
        let saved = (self.mc_stack.len(), self.mc_floor);
        self.mc_floor = self.mc_stack.len();
        saved
    }

    /// The stream is over: marked content it left open is closed.
    pub(super) fn leave_marked_content(&mut self, saved: (usize, usize)) {
        while self.mc_stack.len() > saved.0 {
            if self.mc_stack.pop() == Some(true) {
                self.hidden = self.hidden.saturating_sub(1);
            }
        }
        self.mc_floor = saved.1;
    }
}

fn luminosity_of(rgb: [f32; 3]) -> u8 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    luminosity_of_u32(c(rgb[0]), c(rgb[1]), c(rgb[2]))
}

fn luminosity_of_u32(r: u32, g: u32, b: u32) -> u8 {
    luminosity(r.min(255), g.min(255), b.min(255)).min(255) as u8
}
