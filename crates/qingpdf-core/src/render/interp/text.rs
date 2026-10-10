//! Showing text (ISO 32000-1 9.4.4, 9.3.6): glyphs from the font reader of `fonts`, filled, stroked or added to the
//! clip by the text rendering mode, and a box for a character whose glyph cannot be had.
//!
//! A filled glyph of ordinary size is drawn from a coverage bitmap kept per glyph, text matrix scale and
//! quarter pixel position (one rasterization serves every later use of the letter); the bitmaps are
//! limited in bytes. Anything else (big glyphs, strokes, clipping) goes through paths.

use std::rc::Rc;

use tiny_skia::{FillRule, Mask, Path, PathBuilder, PathSegment, Transform};

use super::{FontEntry, Interp, Resources, Shown, apply, finite, is_blank, mul};
use crate::error::Result;
use crate::render::fonts::{self, CharInfo, Cause, Glyph, LoadCtx, Loaded, Lookup};
use crate::text::interp::Matrix;

/// Quarter pixel positions.
const SUBPIXEL: f64 = 4.0;
/// Bitmaps of glyphs are kept up to this size, and only for glyphs that fit this many pixels on a side.
const MAX_BITMAP_BYTES: usize = 8 << 20;
const MAX_BITMAP_SIDE: f64 = 96.0;
/// Outline segments the glyphs of one text object may add to its clip (modes 4 to 7).
const MAX_TEXT_CLIP_SEGMENTS: usize = 1_000_000;

pub(super) type BitmapKey = (u64, u64, u8);

pub(super) struct GlyphBitmap {
    left: i32,
    top: i32,
    width: usize,
    height: usize,
    alpha: Vec<u8>,
}

impl Interp<'_> {
    /// The font reader of the current font, made now if needed (other fonts give their memory back when
    /// the programs held are many).
    fn glyph_reader(&mut self, entry: &Rc<FontEntry>) -> std::result::Result<Rc<Loaded>, Cause> {
        let Some(source) = &entry.glyphs else { return Err(Cause::NoFont) };
        if !source.is_loaded() && self.shared.budget.programs.get() > self.shared.caps.programs / 2 {
            for other in self.shared.fonts.values().flatten() {
                if !Rc::ptr_eq(other, entry)
                    && let Some(s) = &other.glyphs
                {
                    s.unload();
                }
            }
        }
        let mut ctx = LoadCtx { doc: self.doc, warnings: &mut self.shared.warnings, budget: &self.shared.budget };
        source.loaded(&mut ctx)
    }

    fn trim_glyph_caches(&mut self) {
        if self.shared.budget.glyphs.get() > self.shared.caps.glyphs {
            for e in self.shared.fonts.values().flatten() {
                if let Some(s) = &e.glyphs {
                    s.trim_glyphs();
                }
            }
        }
    }

    /// Show strings (9.4.4): a Type 3 glyph is drawn from its procedure, any other character from the outline of
    /// its glyph, or as an outline box when the font has none.
    pub(super) fn show(&mut self, strings: &[&[u8]], res: &Rc<Resources>, depth: usize) -> Result<()> {
        let Some(entry) = self.gs.font.clone() else { return Ok(()) };
        let (size, tc, tw, th, rise) = (self.gs.size, self.gs.tc, self.gs.tw, self.gs.th, self.gs.rise);
        let mode = self.gs.mode;
        // Mode 3 shows nothing, whatever the font; no other mode has an effect on a Type 3 glyph (9.3.6).
        // (Optional content that is hidden shows nothing either.)
        let visible = mode != 3 && self.hidden == 0;
        let type3_visible = mode != 3 && self.hidden == 0;
        let full = self.full();
        let no_widths = entry.glyphs.as_ref().is_some_and(|g| g.no_widths);
        let gbk_pairs = entry.glyphs.as_ref().is_some_and(|g| g.gbk_pairs);
        let mut reader: Option<std::result::Result<Rc<Loaded>, Cause>> = None;
        let mut boxes = PathBuilder::new();
        let mut any_box = false;
        for s in strings {
            let mut chars = std::mem::take(&mut self.chars);
            chars.clear();
            entry.font.show(s, |ch| {
                chars.push(Shown {
                    code: ch.code,
                    cid: ch.cid,
                    glyph_uni: ch.glyph_uni,
                    w0: ch.w0,
                    vertical: ch.vertical,
                    is_space: ch.is_space_code,
                    blank: is_blank(&ch.uni),
                    skip: false,
                });
            });
            if gbk_pairs {
                // The first byte of a pair carries the character, the second only its width.
                let mut i = 0;
                while i + 1 < chars.len() {
                    let (lead, trail) = (chars.get(i).map_or(0, |c| c.code), chars.get(i + 1).map_or(0, |c| c.code));
                    if (0x81..=0xFE).contains(&lead) && (0x40..=0xFE).contains(&trail) {
                        let code = lead << 8 | trail;
                        if let Some(c) = chars.get_mut(i) {
                            c.cid = code;
                            c.glyph_uni = fonts::gbk_to_unicode(code);
                            c.blank = false;
                        }
                        if let Some(c) = chars.get_mut(i + 1) {
                            c.skip = true;
                        }
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            for ch in &chars {
                // A glyph is work even if it draws nothing (a Type 3 glyph may show strings of its own).
                self.charge_op()?;
                let mut w0 = ch.w0;
                let mut cause = None;
                let mut found = None;
                if entry.type3.is_none() && visible && !ch.skip && (!ch.blank || no_widths) && size != 0.0 && self.segs_left > 0 {
                    if reader.is_none() {
                        reader = Some(self.glyph_reader(&entry));
                    }
                    match &reader {
                        Some(Ok(loaded)) => {
                            let info = CharInfo { code: ch.code, cid: ch.cid, glyph_uni: ch.glyph_uni };
                            match loaded.lookup(&info) {
                                Lookup::Draw(glyph) => {
                                    // A font with no /Widths (the 14 standard ones may come so) is spaced by the glyphs of
                                    // the font that stands in for it; any other is squeezed where its stand-in is wider.
                                    if loaded.substituted && no_widths && glyph.advance > 0.0 {
                                        w0 = f64::from(glyph.advance);
                                    }
                                    found = Some((glyph, loaded.substituted));
                                }
                                Lookup::Blank(advance) => {
                                    if loaded.substituted && no_widths && advance > 0.0 {
                                        w0 = f64::from(advance);
                                    }
                                }
                                Lookup::Absent => self.absent += 1,
                                Lookup::Skipped => self.warn("the glyph outlines of the page took more work than a page may spend; the rest of the text is not drawn"),
                                Lookup::Box(c) => {
                                    if !ch.blank {
                                        cause = Some(c);
                                    }
                                }
                            }
                        }
                        Some(Err(c)) => cause = Some(*c),
                        None => {}
                    }
                }
                let tx_ty = match ch.vertical {
                    None => {
                        let tx = (w0 * size + tc + if ch.is_space { tw } else { 0.0 }) * th;
                        (tx * self.tm[0], tx * self.tm[1])
                    }
                    Some((w1, _, _)) => {
                        let ty = w1 * size + tc + if ch.is_space { tw } else { 0.0 };
                        (ty * self.tm[2], ty * self.tm[3])
                    }
                };
                if let Some(t3) = &entry.type3 {
                    if type3_visible {
                        self.type3_glyph(t3, ch.code, res, depth)?;
                    }
                } else if let Some((glyph, substituted)) = found {
                    let hs = if substituted && glyph.advance > 0.0 && w0 > 0.0 && f64::from(glyph.advance) > w0 * 1.02 { w0 / f64::from(glyph.advance) } else { 1.0 };
                    self.draw_glyph(&glyph, ch, hs, full)?;
                } else if let Some(cause) = cause
                    && mode != 7
                    && ch.w0 > 0.0
                {
                    self.queue_box(ch, cause, size, th, rise, full, &mut boxes, &mut any_box);
                }
                self.tm[4] += tx_ty.0;
                self.tm[5] += tx_ty.1;
            }
            self.chars = chars;
            if let Some(Ok(loaded)) = &reader
                && let Some(message) = loaded.take_error()
            {
                self.warn(message);
            }
        }
        if !finite(&self.tm) {
            self.tm = super::IDENTITY;
        }
        if any_box && let Some(path) = boxes.finish() {
            self.stroke_boxes(&path)?;
        }
        self.trim_glyph_caches();
        Ok(())
    }

    /// The outline box of a character with no glyph: its width, from a bit below the baseline to a bit
    /// above the x-height of an average font.
    #[allow(clippy::too_many_arguments)]
    fn queue_box(&mut self, ch: &Shown, cause: Cause, size: f64, th: f64, rise: f64, full: Matrix, boxes: &mut PathBuilder, any_box: &mut bool) {
        let (x0, x1, y0, y1) = match ch.vertical {
            None => (0.0, ch.w0 * size * th, rise - 0.2 * size, rise + 0.8 * size),
            Some((_, vx, vy)) => (-vx * size, (-vx + ch.w0) * size, rise - vy * size - 0.2 * size, rise - vy * size + 0.8 * size),
        };
        let tm_ctm = mul(&self.tm, &full);
        let pts = [apply(&tm_ctm, x0, y0), apply(&tm_ctm, x1, y0), apply(&tm_ctm, x1, y1), apply(&tm_ctm, x0, y1)];
        self.boxed += 1;
        let slot = match cause {
            Cause::NoFont => 0,
            Cause::NoCharacter => 1,
            Cause::NotInFont => 2,
        };
        if let Some(n) = self.boxed_by.get_mut(slot) {
            *n += 1;
        }
        // Text that runs straight across the page: the box is drawn right here, pixel by pixel.
        let upright = tm_ctm[1].abs() <= 1e-6 * tm_ctm[0].abs().max(1e-9) && tm_ctm[2].abs() <= 1e-6 * tm_ctm[3].abs().max(1e-9);
        if !(upright && self.draw_box_fast(&pts)) {
            for (i, (x, y)) in pts.iter().enumerate() {
                let (x, y) = (x.clamp(-1e7, 1e7) as f32, y.clamp(-1e7, 1e7) as f32);
                if i == 0 {
                    boxes.move_to(x, y);
                } else {
                    boxes.line_to(x, y);
                }
            }
            boxes.close();
            *any_box = true;
        }
    }

    /// Draw one glyph by the text rendering mode (9.3.6).
    fn draw_glyph(&mut self, glyph: &Rc<Glyph>, ch: &Shown, hs: f64, full: Matrix) -> Result<()> {
        // A glyph costs its segments, like a path does: a font whose glyphs are huge cannot make a page last forever.
        let cost = glyph.path.verbs().len();
        if self.segs_left < cost {
            self.warn("the glyphs of the page have more outline segments than a page may draw; the rest are not drawn");
            self.segs_left = 0;
            return Ok(());
        }
        self.segs_left -= cost;
        let (size, th, rise) = (self.gs.size, self.gs.th, self.gs.rise);
        let (tx, ty) = match ch.vertical {
            None => (0.0, rise),
            Some((_, vx, vy)) => (-vx * size * th, rise - vy * size),
        };
        // Em units to text space, then the text matrix, the CTM and the page.
        let local: Matrix = [size * th * hs, 0.0, 0.0, size, tx, ty];
        let user = mul(&local, &self.tm);
        let m = mul(&user, &full);
        if !finite(&m) {
            return Ok(());
        }
        let mode = self.gs.mode;
        if matches!(mode, 0 | 2 | 4 | 6) && !self.gs.fill.none {
            self.fill_glyph(glyph, &m)?;
        }
        if matches!(mode, 1 | 2 | 5 | 6) && !self.gs.stroke.none {
            self.stroke_glyph(glyph, &user)?;
        }
        if mode >= 4 {
            self.add_to_text_clip(glyph, &m);
        }
        Ok(())
    }

    fn fill_glyph(&mut self, glyph: &Rc<Glyph>, m: &Matrix) -> Result<()> {
        if self.gs.clip.is_empty() {
            return Ok(());
        }
        let color = self.gs.fill.rgb;
        let alpha = self.gs.fill_alpha;
        // A pattern, a blend mode, a soft mask or a knockout group: the glyph goes through the general path machinery.
        let general = self.gs.fill.pattern.is_some() || !self.plain();
        // Size on the device: the box of the outline under the 2 by 2 part of the matrix.
        let b = glyph.path.bounds();
        let corners = [(b.left(), b.top()), (b.right(), b.top()), (b.right(), b.bottom()), (b.left(), b.bottom())];
        let (mut lx, mut ly, mut hx, mut hy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners {
            let (dx, dy) = (m[0] * f64::from(x) + m[2] * f64::from(y), m[1] * f64::from(x) + m[3] * f64::from(y));
            lx = lx.min(dx);
            hx = hx.max(dx);
            ly = ly.min(dy);
            hy = hy.max(dy);
        }
        // A glyph that falls outside the page or the clip draws nothing: it is not rasterized (nor kept).
        let c = self.gs.clip.rect;
        let (page_w, page_h) = (f64::from(self.pixmap.width()), f64::from(self.pixmap.height()));
        let (cx0, cy0) = (f64::from(c[0]).max(0.0) - 2.0, f64::from(c[1]).max(0.0) - 2.0);
        let (cx1, cy1) = (f64::from(c[2]).min(page_w) + 2.0, f64::from(c[3]).min(page_h) + 2.0);
        if m[4] + hx < cx0 || m[4] + lx > cx1 || m[5] + hy < cy0 || m[5] + ly > cy1 {
            return Ok(());
        }
        let small = hx - lx <= MAX_BITMAP_SIDE && hy - ly <= MAX_BITMAP_SIDE;
        if general || !small || m[4].abs() > 1e6 || m[5].abs() > 1e6 {
            let ts = Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32);
            if let Some(path) = glyph.path.clone().transform(ts) {
                self.fill_current(&path, FillRule::Winding)?;
            }
            return Ok(());
        }
        // Where the origin falls. The baseline goes to a whole pixel (what PDFium does, and the nearest to its
        // pictures: measured against it, any other choice differs more); across the line the position is the
        // quarter pixel below it, which is the same thing the measurements prefer.
        let ex = (m[4] * SUBPIXEL).floor() as i64;
        let (ix, sx) = (ex.div_euclid(4) as i32, ex.rem_euclid(4) as u8);
        let iy = m[5].round() as i32;
        // The matrix to 1/16 of a pixel per em.
        let q = |v: f64| (v * 16.0).round().clamp(-32000.0, 32000.0) as i16 as u16;
        let packed = u64::from(q(m[0])) | u64::from(q(m[1])) << 16 | u64::from(q(m[2])) << 32 | u64::from(q(m[3])) << 48;
        let key: BitmapKey = (glyph.id, packed, sx);
        let bitmap = match self.shared.bitmaps.get(&key) {
            Some(hit) => hit.clone(),
            None => {
                let made = rasterize(&glyph.path, [f64::from(q(m[0]) as i16) / 16.0, f64::from(q(m[1]) as i16) / 16.0, f64::from(q(m[2]) as i16) / 16.0, f64::from(q(m[3]) as i16) / 16.0], f64::from(sx) / SUBPIXEL);
                let made = made.map(Rc::new);
                // The cache is held to its size as it grows, not afterwards.
                let size = made.as_ref().map_or(48, |b| b.alpha.len() + 64);
                if self.shared.bitmap_bytes + size > MAX_BITMAP_BYTES.min(self.shared.caps.glyph_bitmaps) {
                    self.shared.bitmaps.clear();
                    self.shared.bitmap_bytes = 0;
                }
                self.shared.bitmap_bytes += size;
                #[cfg(test)]
                {
                    self.shared.bitmap_peak = self.shared.bitmap_peak.max(self.shared.bitmap_bytes);
                }
                self.shared.bitmaps.insert(key, made.clone());
                made
            }
        };
        let Some(bitmap) = bitmap else { return Ok(()) };
        self.blit_glyph(&bitmap, ix, iy, color, alpha)
    }

    /// Composite a coverage bitmap onto the page in the fill colour, inside the clip.
    fn blit_glyph(&mut self, bm: &GlyphBitmap, ox: i32, oy: i32, rgb: [f32; 3], alpha: f32) -> Result<()> {
        let clip = self.gs.clip.clone();
        let (x, y) = (ox.saturating_add(bm.left), oy.saturating_add(bm.top));
        let c = clip.rect;
        let (x0, y0) = (x.max(c[0]).max(0), y.max(c[1]).max(0));
        let page_w = self.pixmap.width() as i32;
        let page_h = self.pixmap.height() as i32;
        let (x1, y1) = (x.saturating_add(bm.width as i32).min(c[2]).min(page_w), y.saturating_add(bm.height as i32).min(c[3]).min(page_h));
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        self.charge_area(f64::from(x1 - x0) * f64::from(y1 - y0))?;
        let width = page_w as usize;
        let src = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32);
        let constant = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        let mask = clip.mask.as_ref().map(|m| m.mask.data());
        let (cols, bx) = ((x1 - x0) as usize, (x0 - x) as usize);
        let data = self.pixmap.data_mut();
        for (r, drow) in data.chunks_exact_mut(width * 4).enumerate().skip(y0 as usize).take((y1 - y0) as usize) {
            let by = r - y0 as usize + (y0 - y) as usize;
            let Some(cov_row) = bm.alpha.get(by * bm.width + bx..by * bm.width + bx + cols) else { continue };
            let Some(dst_row) = drow.get_mut(x0 as usize * 4..(x0 as usize + cols) * 4) else { continue };
            let mask_row = mask.and_then(|m| m.get(r * width + x0 as usize..r * width + x0 as usize + cols));
            for (i, (&cov, px)) in cov_row.iter().zip(dst_row.chunks_exact_mut(4)).enumerate() {
                if cov == 0 {
                    continue;
                }
                let mut a = u32::from(cov);
                if constant != 255 {
                    a = (a * constant + 127) / 255;
                }
                if let Some(mr) = mask_row {
                    a = (a * u32::from(mr.get(i).copied().unwrap_or(0)) + 127) / 255;
                }
                if a == 0 {
                    continue;
                }
                for (slot, s) in px.iter_mut().zip(src) {
                    *slot = ((u32::from(*slot) * (255 - a) + s * a + 127) / 255) as u8;
                }
            }
        }
        Ok(())
    }

    /// Stroke a glyph with the line width of the graphics state: it goes through the path machinery in
    /// user space (`user` takes em units to user space).
    fn stroke_glyph(&mut self, glyph: &Rc<Glyph>, user: &Matrix) -> Result<()> {
        let saved_path = std::mem::take(&mut self.path);
        let saved_clip = self.pending_clip.take();
        let (mut last, mut start) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
        let push = |s: &mut Interp<'_>, verb: u8, pts: &[(f32, f32)]| {
            let flat: Vec<f64> = pts
                .iter()
                .flat_map(|&(x, y)| {
                    let (ux, uy) = apply(user, f64::from(x), f64::from(y));
                    [ux, uy]
                })
                .collect();
            s.add_seg(verb, &flat);
        };
        for seg in glyph.path.segments() {
            match seg {
                PathSegment::MoveTo(p) => {
                    push(self, 0, &[(p.x, p.y)]);
                    last = (p.x, p.y);
                    start = last;
                }
                PathSegment::LineTo(p) => {
                    push(self, 1, &[(p.x, p.y)]);
                    last = (p.x, p.y);
                }
                PathSegment::QuadTo(c, p) => {
                    let c1 = (last.0 + 2.0 / 3.0 * (c.x - last.0), last.1 + 2.0 / 3.0 * (c.y - last.1));
                    let c2 = (p.x + 2.0 / 3.0 * (c.x - p.x), p.y + 2.0 / 3.0 * (c.y - p.y));
                    push(self, 2, &[c1, c2, (p.x, p.y)]);
                    last = (p.x, p.y);
                }
                PathSegment::CubicTo(c1, c2, p) => {
                    push(self, 2, &[(c1.x, c1.y), (c2.x, c2.y), (p.x, p.y)]);
                    last = (p.x, p.y);
                }
                PathSegment::Close => {
                    push(self, 3, &[]);
                    last = start;
                }
            }
        }
        let result = self.paint(false, false, true, FillRule::Winding);
        self.path = saved_path;
        self.pending_clip = saved_clip;
        result
    }

    /// Add the glyph's outline (device space) to the clip the text object makes at `ET` (9.3.6).
    fn add_to_text_clip(&mut self, glyph: &Rc<Glyph>, m: &Matrix) {
        // A text object with endless glyphs in a clipping mode holds on to all their outlines until `ET`: past the cap the
        // later glyphs do not add to the clip.
        let cost = glyph.path.verbs().len();
        if self.text_clip_segs.saturating_add(cost) > MAX_TEXT_CLIP_SEGMENTS {
            self.warn("the glyphs of a text object that clips have more outline segments than allowed; the rest do not add to the clip");
            return;
        }
        let ts = Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, m[4] as f32, m[5] as f32);
        let Some(path) = glyph.path.clone().transform(ts) else { return };
        self.text_clip_segs += cost;
        self.text_clip.get_or_insert_with(PathBuilder::new).push_path(&path);
    }

    /// `ET`: the glyphs shown in a clipping mode are now the clip.
    pub(super) fn end_text(&mut self) -> Result<()> {
        self.text_clip_segs = 0;
        if let Some(pb) = self.text_clip.take()
            && let Some(path) = pb.finish()
        {
            self.clip_device(&path, FillRule::Winding)?;
        }
        Ok(())
    }
}

/// A glyph outline as coverage: the matrix is em units to pixels (to 1/16), the origin is `sx` of a pixel to the
/// right of a whole pixel position. `None` when nothing is covered.
fn rasterize(path: &Path, m: [f64; 4], sx: f64) -> Option<GlyphBitmap> {
    let ts = Transform::from_row(m[0] as f32, m[1] as f32, m[2] as f32, m[3] as f32, sx as f32, 0.0);
    let placed = path.clone().transform(ts)?;
    let b = placed.bounds();
    let (left, top) = (b.left().floor() as i32, b.top().floor() as i32);
    let (right, bottom) = (b.right().ceil() as i32, b.bottom().ceil() as i32);
    let (w, h) = ((right - left).max(1) as u32, (bottom - top).max(1) as u32);
    if w > 400 || h > 400 {
        return None;
    }
    // Coverage straight into an 8-bit mask: the same scan conversion as a black fill on a bitmap, without the colour pipeline.
    let mut mask = Mask::new(w, h)?;
    mask.fill_path(&placed, FillRule::Winding, true, Transform::from_translate(-(left as f32), -(top as f32)));
    let alpha = mask.take();
    Some(GlyphBitmap { left, top, width: w as usize, height: h as usize, alpha })
}

