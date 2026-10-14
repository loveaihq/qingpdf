//! The memory plan of a reader (3d-1): one total for the file, everything the engine keeps, and the bitmaps the
//! window keeps. Nothing here knows about windows or threads; it only turns "this much in all, a file of this size,
//! a window of this many pixels" into the limits each part is held to.

/// The memory goal of the reader in all: the file, the engine's caches and work memory, the page being drawn, and the
/// bitmaps the window keeps. (200 MB, counted in millions of bytes.)
pub const TOTAL_BYTES: u64 = 200_000_000;

/// A file up to this size gets the plan below as it is; a bigger one leaves less for the rest, and the parts shrink
/// in proportion.
const FULL_PLAN_FILE: u64 = 40_000_000;
/// The biggest file the reader opens: it holds the whole file in memory and must have room to draw as well.
pub const MAX_FILE_BYTES: u64 = 100_000_000;

const MIB: u64 = 1 << 20;
/// Decoded object streams kept for reuse (the document's cache; its default is 96 MiB).
const OBJECT_STREAMS: u64 = 16 * MIB;
/// Parsed forms, shadings, patterns, decoded images, font programs and glyphs kept from page to page.
const RENDER_CACHES: u64 = 40 * MIB;
/// One large decoded image kept for the pieces of the page being drawn (a scan the size of a 1080p window: 20 MB).
const IMAGE_SLOT: u64 = 24 * MIB;
/// Off-screen layers and clip masks alive at once while a page is drawn.
const LAYERS: u64 = 16 * MIB;
const MASKS: u64 = 8 * MIB;
/// What the JPEG 2000 and JBIG2 decoders hold at once while a page is drawn.
const DECODERS: u64 = 32 * MIB;
/// The text of the pages read lately, with the box of every character, kept for the search, the selection and the copy to
/// share (3d-2: a page of 3,000 characters is about 50 KB).
const TEXT_CACHE: u64 = 16 * MIB;
/// What the objects an edit puts in front of the file (annotations, their appearance streams, the pages that changed) and the
/// history that undoes them may take together, until the file is saved (4a). Counted in the plan, so that the file, the edits and
/// the rest stay within the total. A save appends them to the file in memory (the file is read with room after it for that,
/// `view::SPARE_AFTER_FILE`, 1 MiB, which an update rarely uses up).
const EDIT_BYTES: u64 = 16 * MIB;
/// The lists of form fields the window keeps for the pages round about (at most `MAX_FIELDS_BYTES` for each of 25 pages), and the
/// options of the one choice being made (at most `MAX_OPTION_BYTES`). Not cut when the file is big: a list is what it is.
const FORM_BYTES: u64 = 8 * MIB;
/// The small pictures of pages the window keeps for the thumbnails beside the pages (2026-10-14; 8 MB, counted in millions of bytes
/// like the total). They are taken off the engine's caches (each cut by the same fraction: `MemoryPlan::make_room_for_thumbnails`), not off the
/// window's bitmaps: how many pieces round the view those hold decides how deep the prefetch of a page turn goes, which is felt, and the
/// speed test of a 1920 by 1080 window turned out to sit on the edge of it (docs/decisions.md, thumbnails).
pub const THUMB_CACHE_BYTES: u64 = 8_000_000;
/// The most pixels of one piece of a page drawn at once (16 MiB of canvas).
const TILE_PIXELS: u64 = 4 * MIB;
/// The window needs room for at least a few screens of bitmaps, or there is no plan.
const MIN_BITMAP_CACHE: u64 = 8_000_000;
/// What the window's bitmaps come to, at least: a screen's worth of pieces (which stick out past the window's edge: half as
/// much again) and a screen's worth of the pages next to it, at 4 bytes a pixel.
const BYTES_PER_VIEW_PIXEL: u64 = 4 * 3;
/// And, whatever the window's size, room for what a page turn needs at rest: the piece in view, the top piece of the next page,
/// and a piece next to the first (two and a half whole pieces), plus the small pictures of the pages round about (17 of
/// 64,000 pixels: `sched::PREVIEW_PIXELS` in the window).
const PREVIEWS: u64 = 17 * 64_000 * 4;
/// The engine's parts are never cut to less than this share (per 1000), whatever the window and the file ask.
const MIN_SHARE: u64 = 250;
/// No piece is smaller than this, however big the file.
const MIN_TILE_PIXELS: u64 = MIB;

/// The limits for one open file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryPlan {
    pub file_bytes: u64,
    /// What the window may keep of drawn bitmaps (what is left of the total).
    pub bitmap_cache_bytes: u64,
    /// What the window may keep of thumbnails, besides `bitmap_cache_bytes` ([`THUMB_CACHE_BYTES`]); the engine's caches gave it up.
    pub thumb_cache_bytes: u64,
    /// The most pixels in one request to draw; the window cuts a bigger page into pieces. The engine refuses a
    /// request for more.
    pub max_tile_pixels: u32,
    pub object_streams: u64,
    pub render_caches: u64,
    pub image_slot: u64,
    pub layers: u64,
    pub masks: u64,
    pub decoders: u64,
    /// What the engine keeps of the text of pages it has read.
    pub text_cache: u64,
    /// What the unsaved edits and the history that undoes them may take.
    pub edit_bytes: u64,
    /// What the lists of form fields and the options of a choice come to, in all (`view::MAX_FIELDS_BYTES`, `view::MAX_OPTION_BYTES`).
    pub form_bytes: u64,
}

impl MemoryPlan {
    /// Make room for [`THUMB_CACHE_BYTES`] of thumbnails: the engine's caches (not the page canvas, not the lists of form fields) give it up, each in
    /// proportion to what it has above the floor `MIN_SHARE` keeps for it; what they cannot give (a window so big that they are at the floor already)
    /// comes off the window's bitmaps. In the usual case the bitmaps stay as they were.
    fn make_room_for_thumbnails(&mut self) {
        let floor = |full: u64| full * MIN_SHARE / 1000;
        let mut parts = [
            (&mut self.object_streams, floor(OBJECT_STREAMS)),
            (&mut self.render_caches, floor(RENDER_CACHES)),
            (&mut self.image_slot, floor(IMAGE_SLOT)),
            (&mut self.layers, floor(LAYERS)),
            (&mut self.masks, floor(MASKS)),
            (&mut self.decoders, floor(DECODERS)),
            (&mut self.text_cache, floor(TEXT_CACHE)),
            (&mut self.edit_bytes, floor(EDIT_BYTES)),
        ];
        let spare: u64 = parts.iter().map(|(part, floor)| part.saturating_sub(*floor)).sum();
        let cut = THUMB_CACHE_BYTES.min(spare);
        let mut freed = 0u64;
        for (part, floor) in &mut parts {
            let above = part.saturating_sub(*floor);
            let take = u64::try_from(u128::from(above) * u128::from(cut) / u128::from(spare.max(1))).unwrap_or(0).min(above);
            **part -= take;
            freed += take;
        }
        self.bitmap_cache_bytes = self.bitmap_cache_bytes.saturating_sub(THUMB_CACHE_BYTES - freed);
        self.thumb_cache_bytes = THUMB_CACHE_BYTES;
    }

    /// The most the engine can have at once: its caches, the work memory of a page, and the page (4 bytes a pixel).
    pub fn engine_bytes(&self) -> u64 {
        self.object_streams
            + self.render_caches
            + self.image_slot
            + self.layers
            + self.masks
            + self.decoders
            + self.text_cache
            + self.edit_bytes
            + self.form_bytes
            + u64::from(self.max_tile_pixels) * 4
    }
}

/// The plan for a file of `file_bytes` within `total` bytes, for a window that may show `view_pixels` pixels at most
/// (0: not known; the bitmaps get what the engine leaves). The window's bitmaps are sized from its pixels first and the
/// engine's caches give way for them when the total is short. `None`: the file is too big to open (more than
/// [`MAX_FILE_BYTES`], or more than the total leaves room for).
pub fn plan(total: u64, file_bytes: u64, view_pixels: u64) -> Option<MemoryPlan> {
    if file_bytes > MAX_FILE_BYTES || file_bytes >= total {
        return None;
    }
    // Per 1000: 1000 for a file up to FULL_PLAN_FILE, less for a bigger one.
    let file_share = if file_bytes <= FULL_PLAN_FILE {
        1000
    } else {
        ((total - file_bytes) * 1000 / total.saturating_sub(FULL_PLAN_FILE).max(1)).min(1000)
    };
    let build = |share: u64| {
        let part = |full: u64| full * share / 1000;
        let tile = (TILE_PIXELS * share / 1000).clamp(MIN_TILE_PIXELS, TILE_PIXELS);
        let mut p = MemoryPlan {
            file_bytes,
            bitmap_cache_bytes: 0,
            thumb_cache_bytes: 0,
            max_tile_pixels: u32::try_from(tile).unwrap_or(u32::MAX),
            object_streams: part(OBJECT_STREAMS),
            render_caches: part(RENDER_CACHES),
            image_slot: part(IMAGE_SLOT),
            layers: part(LAYERS),
            masks: part(MASKS),
            decoders: part(DECODERS),
            text_cache: part(TEXT_CACHE),
            edit_bytes: part(EDIT_BYTES),
            form_bytes: FORM_BYTES,
        };
        p.bitmap_cache_bytes = total.saturating_sub(file_bytes).saturating_sub(p.engine_bytes());
        p
    };
    // What the window wants for its bitmaps comes before the engine's caches: they are cut, a step at a time, until
    // the bitmaps have what the window's pixels call for (and room for a page turn), or are at the floor.
    let mut share = file_share;
    let p = loop {
        let p = build(share);
        let pieces = u64::from(p.max_tile_pixels) * 4 * 5 / 2 + PREVIEWS;
        let wanted = view_pixels.saturating_mul(BYTES_PER_VIEW_PIXEL).max(MIN_BITMAP_CACHE).max(pieces);
        if p.bitmap_cache_bytes >= wanted || share <= MIN_SHARE {
            break p;
        }
        share = share.saturating_sub(10).max(MIN_SHARE);
    };
    let mut p = p;
    p.make_room_for_thumbnails();
    (p.bitmap_cache_bytes >= MIN_BITMAP_CACHE.max(u64::from(p.max_tile_pixels) * 4)).then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window of 1920 by 1080, of 3840 by 2160, of 7680 by 4320.
    const HD: u64 = 1920 * 1080;
    const UHD: u64 = 3840 * 2160;
    const HUGE: u64 = 7680 * 4320;

    #[test]
    fn everything_fits_in_the_total() {
        for file in [0, 1_000, 5_000_000, 40_000_000, 40_000_001, 70_000_000, MAX_FILE_BYTES] {
            for view in [0, HD, UHD, HUGE] {
                let p = plan(TOTAL_BYTES, file, view).expect("a plan");
                assert!(p.file_bytes + p.engine_bytes() + p.bitmap_cache_bytes + p.thumb_cache_bytes <= TOTAL_BYTES, "{file} {view}: {p:?}");
                assert_eq!(p.thumb_cache_bytes, THUMB_CACHE_BYTES, "{file} {view}");
                // Room for a few screens of bitmaps at least.
                assert!(p.bitmap_cache_bytes >= MIN_BITMAP_CACHE, "{file} {view}: {p:?}");
                // A full-size piece of a page is held in the cache whole.
                assert!(p.bitmap_cache_bytes >= u64::from(p.max_tile_pixels) * 4, "{file} {view}: {p:?}");
            }
        }
    }

    #[test]
    fn a_small_file_gets_the_full_plan_and_a_big_one_less() {
        let small = plan(TOTAL_BYTES, 5_000_000, HD).expect("a plan");
        let big = plan(TOTAL_BYTES, 90_000_000, HD).expect("a plan");
        // Most of it (a page turn needs room for two and a half pieces of a page and the pictures, and the lists of form fields
        // have 8 MiB of their own, which the engine gives up a little for).
        assert!(small.render_caches > RENDER_CACHES * 7 / 10 && small.render_caches <= RENDER_CACHES, "{small:?}");
        assert!(u64::from(small.max_tile_pixels) > TILE_PIXELS * 7 / 10 && u64::from(small.max_tile_pixels) <= TILE_PIXELS, "{small:?}");
        assert!(big.render_caches < small.render_caches && big.decoders < small.decoders);
        assert!(big.max_tile_pixels >= MIN_TILE_PIXELS as u32);
    }

    #[test]
    fn a_big_window_takes_its_bitmaps_from_the_engine_caches() {
        // A window of full HD: the bitmaps have room for a page turn (two and a half whole pieces and the pictures), and
        // the engine gives up only a little for it.
        let hd = plan(TOTAL_BYTES, 5_000_000, HD).expect("a plan");
        assert!(hd.bitmap_cache_bytes >= HD * 12 && hd.bitmap_cache_bytes >= u64::from(hd.max_tile_pixels) * 4 * 5 / 2 + PREVIEWS, "{hd:?}");
        assert!(hd.render_caches > RENDER_CACHES * 7 / 10, "{hd:?}");
        // The thumbnails took their bytes from the engine, not from the window: the total is still used to the byte (less the rounding
        // of the cut), and the bitmaps have what the plan gave them before the thumbnails.
        assert!(TOTAL_BYTES - (hd.file_bytes + hd.engine_bytes() + hd.bitmap_cache_bytes + hd.thumb_cache_bytes) < 16, "{hd:?}");
        // A 4K window wants more than is left: the bitmaps get what they want and the engine's parts are cut.
        let uhd = plan(TOTAL_BYTES, 5_000_000, UHD).expect("a plan");
        assert!(uhd.bitmap_cache_bytes >= UHD * 12, "{uhd:?}");
        assert!(uhd.render_caches < hd.render_caches && uhd.image_slot < hd.image_slot && uhd.max_tile_pixels < hd.max_tile_pixels);
        // A window bigger than anything sensible does not starve the engine below its floor.
        let huge = plan(TOTAL_BYTES, 5_000_000, HUGE).expect("a plan");
        assert!(huge.render_caches >= RENDER_CACHES * MIN_SHARE / 1000 - 1, "{huge:?}");
    }

    #[test]
    fn a_file_that_is_too_big_has_no_plan() {
        assert!(plan(TOTAL_BYTES, MAX_FILE_BYTES + 1, HD).is_none());
        assert!(plan(50_000_000, 60_000_000, HD).is_none());
        assert!(plan(TOTAL_BYTES, u64::MAX, HD).is_none());
    }
}

