//! The work meter of a page. The limits of the interpreter count operators, path segments, painted area and image
//! pixels; none of them tells what a shading, a soft mask, a layer or a membership dictionary really costs. This
//! meter does: everything the transparency, shading, pattern and optional content code does is charged to it in
//! units of about one nanosecond of a 2020s desktop core, in proportion to the work (pixels shaded or composited,
//! mask pixels built, mesh bits read, optional content nodes visited), whether or not it shows on the page.
//!
//! A page that has used up the meter does not start new work: the content stream stops at the next operator, the
//! page gives one warning, and what is drawn so far is kept. The weights below were measured on the machine the
//! tests run on (see `docs/decisions.md`, 3c); they need to be right to a factor of two, not exactly.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dict, ObjRef, Object};

/// The work one page may do, in units of about a nanosecond: a hostile page stops after about two seconds.
pub(crate) const PAGE_WORK: f64 = 2.0e9;

/// What a unit of work is in the code that spends it. Per pixel unless it says otherwise.
pub(crate) mod cost {
    /// A new layer: allocated and cleared.
    pub const LAYER: f64 = 1.0;
    /// A layer put on the page with Normal blending, and with another blend mode.
    pub const COMPOSITE: f64 = 2.5;
    pub const COMPOSITE_BLEND: f64 = 10.0;
    /// A page-sized mask made: cleared (all of it), and worked out (the part the clip leaves).
    pub const MASK_CLEAR: f64 = 0.05;
    pub const MASK: f64 = 3.0;
    /// The pixels of a soft mask group turned into mask values.
    pub const SOFT_MASK: f64 = 5.0;
    /// A pixel painted from a pattern cell.
    pub const PATTERN_FILL: f64 = 6.0;
    /// A shading bitmap made: allocated, cleared and cut to the box of the shading; and the colour of a pixel of each kind.
    pub const SHADE_BITMAP: f64 = 2.0;
    pub const SHADE_AXIAL: f64 = 1.5;
    pub const SHADE_RADIAL: f64 = 17.0;
    pub const SHADE_FUNCTION: f64 = 14.0;
    /// A point of the grid a function-based shading is worked out on.
    pub const SHADE_EVAL: f64 = 300.0;
    /// A byte of a stream decoded for a mesh or a pattern. A vertex or a patch of a mesh read from its bits.
    pub const DECODE_BYTE: f64 = 2.0;
    pub const MESH_VERTEX: f64 = 150.0;
    pub const MESH_PATCH: f64 = 700.0;
    /// A triangle or a patch looked at when the mesh is drawn (seen or not), and a pixel a triangle covers.
    pub const MESH_TRIANGLE: f64 = 40.0;
    pub const MESH_PATCH_DRAWN: f64 = 100.0;
    pub const MESH_PIXEL: f64 = 6.0;
    /// A point of the grid a patch is cut into.
    pub const MESH_GRID_POINT: f64 = 60.0;
    /// A membership dictionary (optional content) evaluated, and each group or expression node it visits.
    pub const OCMD: f64 = 400.0;
    pub const OC_NODE: f64 = 120.0;

    // JBIG2 (3c2-1). Measured on the machine the tests run on, see `docs/decisions.md`.
    /// A pixel of an arithmetic-coded generic region that is decoded on its own (context, decision, bit packing), one
    /// that the slow loop decodes (adaptive pixels moved, or pixels skipped), a pixel that is one of a long run of the
    /// same colour decoded in one go with its neighbours, and one of a row that is a copy of the row above (typical
    /// prediction).
    pub const JB2_GENERIC_PIXEL: f64 = 45.0;
    pub const JB2_SLOW_PIXEL: f64 = 60.0;
    pub const JB2_RUN_PIXEL: f64 = 1.5;
    pub const JB2_COPY_PIXEL: f64 = 0.2;
    /// A pixel of a refinement region (13 or 10 context pixels read through bounds checks).
    pub const JB2_REFINE_PIXEL: f64 = 130.0;
    /// A pixel of an MMR (fax) coded bitmap.
    pub const JB2_MMR_PIXEL: f64 = 10.0;
    /// A pixel put on a bitmap from another, and a row of that; a byte of a new bitmap cleared.
    pub const JB2_BLIT_PIXEL: f64 = 0.3;
    pub const JB2_BLIT_ROW: f64 = 40.0;
    /// A row of a bitmap that is decoded, whatever its width (a bitmap of width 0 has as many rows as any other).
    pub const JB2_ROW: f64 = 40.0;
    /// A line of a custom Huffman table read (its prefix and range lengths, pushed), and the same line counted and
    /// put in its place when the prefix codes are given out.
    pub const JB2_TABLE_LINE: f64 = 40.0;
    pub const JB2_TABLE_BUILD: f64 = 12.0;
    pub const JB2_CLEAR_BYTE: f64 = 0.15;
    /// An integer decoded arithmetically (up to about 40 decisions), one decoded from a Huffman table.
    pub const JB2_INT: f64 = 220.0;
    pub const JB2_HUFFMAN: f64 = 60.0;
    /// The bookkeeping of one symbol (made, listed, exported) or one symbol instance of a text region, apart from
    /// the pixels and integers it costs.
    pub const JB2_SYMBOL: f64 = 200.0;
    /// A shared symbol put in a list (a clone of an `Rc`) and let go of again: the list of the symbols a segment refers
    /// to, the list a dictionary exports. About 12 ns a clone with its drop when the symbols are spread over memory
    /// (a million of them: 34 ns for the two lists of a dictionary).
    pub const JB2_RC: f64 = 20.0;
    /// A segment looked at, and a cell of a halftone grid (its gray value and the position of its pattern).
    pub const JB2_SEGMENT: f64 = 600.0;
    pub const JB2_CELL: f64 = 60.0;

    // JPEG 2000 (3c2-2). First guesses; the measured values are in `docs/decisions.md`.
    /// A byte of a marker segment, and a marker segment looked at; a tile-part found; a box of the JP2 file.
    pub const JPX_HEADER_BYTE: f64 = 2.0;
    pub const JPX_MARKER: f64 = 100.0;
    pub const JPX_TILE_PART: f64 = 300.0;
    pub const JPX_BOX: f64 = 200.0;
    /// A tile started, one of its components, one of their resolutions.
    pub const JPX_TILE: f64 = 3000.0;
    pub const JPX_TILE_COMP: f64 = 2000.0;
    pub const JPX_RES: f64 = 300.0;
    /// A precinct, a precinct's part of one subband, a code-block, a node of a tag tree (built).
    pub const JPX_PRECINCT: f64 = 120.0;
    pub const JPX_PBAND: f64 = 150.0;
    pub const JPX_BLOCK_INIT: f64 = 120.0;
    pub const JPX_TREE_NODE: f64 = 10.0;
    /// A packet looked at (also one that is skipped or empty), a bit of a packet header, a code-block named in a
    /// header, a codeword segment of it, a precinct put in the order of a position-based progression.
    pub const JPX_PACKET: f64 = 150.0;
    pub const JPX_HEADER_BIT: f64 = 8.0;
    pub const JPX_BLOCK_HDR: f64 = 200.0;
    pub const JPX_SEG: f64 = 100.0;
    pub const JPX_ORDER: f64 = 80.0;
    /// A coding pass of a code-block (fixed part, and each sample of the block it passes over) and an MQ or raw decision.
    pub const JPX_PASS: f64 = 100.0;
    pub const JPX_PASS_SAMPLE: f64 = 1.2;
    pub const JPX_DECISION: f64 = 10.0;
    /// A byte of code-block data gathered; a coefficient dequantized and put in place.
    pub const JPX_GATHER_BYTE: f64 = 0.5;
    pub const JPX_COEF: f64 = 2.5;
    /// A sample at a level of the inverse wavelet transform (rows and columns).
    pub const JPX_DWT: f64 = 3.5;
    /// A sample through the component transform and put on its plane; a pixel of the result per channel.
    pub const JPX_SAMPLE: f64 = 2.0;
    pub const JPX_PIXEL: f64 = 2.0;
    pub const JPX_CLEAR_BYTE: f64 = 0.15;
    /// A code-block looked at when the blocks to decode are listed, and when its result is put in place.
    pub const JPX_BLOCK_VISIT: f64 = 4.0;

    // Annotations (3c2-3). First guesses; the measured values are in `docs/decisions.md`.
    /// An entry of `/Annots` looked at (its dictionary read, flags, `/Rect` and `/OC` checked), drawn or not, hidden or not,
    /// on the page or off it.
    pub const ANNOT_VISIT: f64 = 2000.0;
    /// An annotation drawn: its matrix worked out and its appearance form set up (the content costs what any content costs).
    pub const ANNOT_DRAW: f64 = 15000.0;
    /// An entry of the `/Annots` array, copied.
    pub const ANNOT_ENTRY: f64 = 40.0;
    /// A byte of an appearance stream that is read again for every annotation because it could not be kept.
    pub const ANNOT_LOAD_BYTE: f64 = 4.0;

    // Objects the drawing code reads by reference, through `Work::resolve` (3c2-3 review). Measured on the machine the
    // tests run on: 7 ns per byte of `approx_size` for an array of numbers or of references, 11 for a dictionary, 0.2
    // for stream data.
    /// A byte (by `approx_size`) of an object read from the file for the page's first time, apart from stream data; and
    /// a byte of stream data.
    pub const RESOLVE_BYTE: f64 = 10.0;
    pub const RESOLVE_DATA_BYTE: f64 = 0.3;
    /// An object the page read before and kept (a clone of an `Arc`), and each entry of it when it is a dictionary
    /// (which whoever asked searches).
    pub const RESOLVE_REPEAT: f64 = 60.0;
    pub const RESOLVE_ENTRY: f64 = 40.0;
}

/// The most `approx_size` bytes of objects (with `KEPT_SLOT` for each) the page keeps for [`Work::resolve`]; when
/// the next would not fit, those nobody asked for a second time are let go of (all of them if that is not enough).
const KEPT_BYTES: usize = 8 << 20;
const KEPT_SLOT: usize = 96;

/// An object read through [`Work::resolve`]: one that was written in place is the very object that was asked about (not
/// copied); one that was a reference is shared with the objects the page keeps.
pub(crate) enum Held<'a> {
    Direct(&'a Object),
    Shared(Arc<Object>),
}

impl Deref for Held<'_> {
    type Target = Object;

    fn deref(&self) -> &Object {
        match self {
            Held::Direct(o) => o,
            Held::Shared(o) => o,
        }
    }
}

/// What reading a reference came to the first time.
#[derive(Clone)]
enum Kept {
    /// The object, and its number of entries if it is a dictionary (what a lookup in it goes through).
    Object(Arc<Object>, usize),
    /// A limit error (the same one each time), and any other error (the object is not there as far as drawing goes).
    Limit(String),
    Failed,
}

/// A kept reading, what it weighs, and whether it was asked for again since the objects kept were last let go of.
struct Slot {
    kept: Kept,
    weight: usize,
    asked_again: bool,
}

#[derive(Default)]
struct KeptObjects {
    map: HashMap<ObjRef, Slot>,
    bytes: usize,
}

/// The page's allowance, spent as it is drawn. The meter can be shared by reference between threads: the image
/// decoders that decode in parallel (JPEG 2000) charge it from their workers.
pub(crate) struct Work {
    /// (units left, units used)
    state: Mutex<(f64, f64)>,
    over: AtomicBool,
    /// The objects the page has read by reference, and the references that could not be read.
    kept: Mutex<KeptObjects>,
    /// Most bytes the image decoders (JPEG 2000, JBIG2) may hold at once; they also have limits of their own.
    decoder_memory: u64,
    /// Set from another thread to stop the page: the next [`Work::charge`] says no, as if the allowance were gone.
    cancel: Option<Arc<AtomicBool>>,
    /// The page was stopped by that flag (not by running out of work).
    cancelled: AtomicBool,
}

impl Work {
    pub fn new() -> Work {
        Work {
            state: Mutex::new((PAGE_WORK, 0.0)),
            over: AtomicBool::new(false),
            kept: Mutex::default(),
            decoder_memory: u64::MAX,
            cancel: None,
            cancelled: AtomicBool::new(false),
        }
    }

    /// The same, stopped when `flag` is set. Every decoder and every shading, layer or pattern charges this meter as
    /// it goes, so one long piece of work (a JPEG 2000 or JBIG2 image, a big shading) stops within a row or a block
    /// instead of running to its end.
    pub fn with_cancel(mut self, flag: Option<Arc<AtomicBool>>) -> Work {
        self.cancel = flag;
        self
    }

    /// Did the flag of [`Work::with_cancel`] stop this page?
    pub fn was_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// The same, with the image decoders held to `bytes` of memory (when that is less than their own limits).
    pub fn with_decoder_memory(mut self, bytes: u64) -> Work {
        self.decoder_memory = bytes;
        self
    }

    pub fn decoder_memory(&self) -> u64 {
        self.decoder_memory
    }

    /// A meter with `units` to spend (tests of the decoders).
    #[cfg(test)]
    pub fn with_allowance(units: f64) -> Work {
        Work { state: Mutex::new((units, 0.0)), ..Work::new() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, (f64, f64)> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Spend `units` on work about to be done. `false` (and nothing spent) when the page has not that much left; from
    /// then on every call says no.
    pub fn charge(&self, units: f64) -> bool {
        if self.over.load(Ordering::Relaxed) {
            return false;
        }
        if self.cancel.as_ref().is_some_and(|f| f.load(Ordering::Relaxed)) {
            self.cancelled.store(true, Ordering::Relaxed);
            self.over.store(true, Ordering::Relaxed);
            return false;
        }
        let mut st = self.lock();
        if units.is_nan() || units > st.0 {
            self.over.store(true, Ordering::Relaxed);
            return false;
        }
        st.0 -= units;
        st.1 += units;
        true
    }

    /// Spend `units` on work that is done already, or must be finished to keep what is drawn (putting a layer on the
    /// page). The page may not start more when this takes what was left.
    pub fn spend(&self, units: f64) {
        if units.is_nan() || units < 0.0 {
            return;
        }
        let mut st = self.lock();
        st.1 += units;
        if units >= st.0 {
            st.0 = 0.0;
            self.over.store(true, Ordering::Relaxed);
        } else {
            st.0 -= units;
        }
    }

    /// The object `o` is or refers to, for the drawing code: the way to read anything the file points to. An object
    /// written in place is handed back as it is (it was paid for with the object it is in). A reference is read from the
    /// file once for the page and kept, and so are the failures, so that a bad reference does not cost a parse for each
    /// annotation or operator that names it. The first read is charged by the size of what it brought, a repeat a little
    /// and by the entries a dictionary has to be searched through. Once the page has no work left nothing more is read.
    /// `Err` is a limit that ends the drawing; `None` is an object that cannot be read, or no work left.
    pub fn resolve<'a>(&self, doc: &Document, o: &'a Object) -> Result<Option<Held<'a>>> {
        let Object::Ref(r) = o else { return Ok(Some(Held::Direct(o))) };
        if self.is_over() {
            return Ok(None);
        }
        let hit = self.lock_kept().map.get_mut(r).map(|slot| {
            slot.asked_again = true;
            slot.kept.clone()
        });
        match hit {
            Some(Kept::Object(obj, entries)) => {
                return Ok(self.charge(cost::RESOLVE_REPEAT + entries as f64 * cost::RESOLVE_ENTRY).then_some(Held::Shared(obj)));
            }
            Some(Kept::Limit(m)) => return Err(Error::Limit(m)),
            Some(Kept::Failed) => return Ok(None),
            None => {}
        }
        let (slot, size, result) = match doc.resolve(o) {
            Ok(obj) => {
                let size = obj.approx_size();
                let data = if let Object::Stream(s) = &obj { s.data.len() } else { 0 };
                if !self.charge(size.saturating_sub(data) as f64 * cost::RESOLVE_BYTE + data as f64 * cost::RESOLVE_DATA_BYTE) {
                    return Ok(None);
                }
                let entries = obj.as_dict().map_or(0, Dict::len);
                let obj = Arc::new(obj);
                (Kept::Object(obj.clone(), entries), size, Ok(Some(Held::Shared(obj))))
            }
            Err(Error::Limit(m)) => (Kept::Limit(m.clone()), 0, Err(Error::Limit(m))),
            Err(_) => (Kept::Failed, 0, Ok(None)),
        };
        let mut store = self.lock_kept();
        let weight = size.saturating_add(KEPT_SLOT);
        if weight <= KEPT_BYTES {
            if store.bytes.saturating_add(weight) > KEPT_BYTES {
                // Full: what nobody asked for again (the annotations themselves, say) goes, what was asked for stays.
                store.map.retain(|_, slot| slot.asked_again);
                store.bytes = store.map.values().map(|slot| slot.weight).sum();
                for slot in store.map.values_mut() {
                    slot.asked_again = false;
                }
                if store.bytes.saturating_add(weight) > KEPT_BYTES {
                    store.map.clear();
                    store.bytes = 0;
                }
            }
            store.bytes += weight;
            store.map.insert(*r, Slot { kept: slot, weight, asked_again: false });
        }
        result
    }

    fn lock_kept(&self) -> std::sync::MutexGuard<'_, KeptObjects> {
        self.kept.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// [`Work::resolve`] for the one who does not need to tell "not there" from "a limit".
    pub fn read<'a>(&self, doc: &Document, o: &'a Object) -> Option<Held<'a>> {
        self.resolve(doc, o).ok().flatten()
    }

    /// A rectangle (7.9.5): an array of four numbers, the array and each number written in place or by reference,
    /// all read through [`Work::resolve`].
    pub fn rectangle(&self, doc: &Document, o: Option<&Object>) -> Option<[f64; 4]> {
        let held = self.read(doc, o?)?;
        let [a, b, c, d] = held.as_array()? else { return None };
        let num = |o: &Object| self.read(doc, o).and_then(|v| v.as_f64());
        crate::document::normalized_rect([num(a)?, num(b)?, num(c)?, num(d)?])
    }

    /// Has the page used up its work (or been refused some)?
    pub fn is_over(&self) -> bool {
        self.over.load(Ordering::Relaxed)
    }

    /// Units spent so far.
    #[cfg(test)]
    pub fn used(&self) -> f64 {
        self.lock().1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_stopped_in_the_middle_of_one_piece_of_work() {
        let flag = Arc::new(AtomicBool::new(false));
        let work = Work::new().with_cancel(Some(flag.clone()));
        // Work in many small steps, as a decoder charges it: the flag stops it at the next step, not at the end.
        let mut steps = 0u64;
        let stopper = {
            let flag = flag.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(20));
                flag.store(true, Ordering::Relaxed);
            })
        };
        let started = std::time::Instant::now();
        while work.charge(1.0) {
            steps += 1;
            if started.elapsed() > std::time::Duration::from_secs(10) {
                break;
            }
            std::hint::spin_loop();
        }
        stopper.join().expect("the stopper");
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "{steps} steps");
        assert!(work.was_cancelled() && work.is_over());
        // The meter has plenty left: it was the flag.
        assert!(work.used() < 1.0e9);
        // Without the flag, or with it down, nothing changes.
        let calm = Work::new().with_cancel(Some(Arc::new(AtomicBool::new(false))));
        assert!(calm.charge(1.0) && !calm.was_cancelled() && !calm.is_over());
        assert!(Work::new().charge(1.0));
    }

    #[test]
    fn a_flag_that_is_up_before_the_work_starts_refuses_all_of_it() {
        let work = Work::new().with_cancel(Some(Arc::new(AtomicBool::new(true))));
        assert!(!work.charge(0.0));
        assert!(work.was_cancelled());
    }
}
