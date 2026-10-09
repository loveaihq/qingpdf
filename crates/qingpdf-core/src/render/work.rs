//! The work meter of a page. The limits of the interpreter count operators, path segments, painted area and image
//! pixels; none of them tells what a shading, a soft mask, a layer or a membership dictionary really costs. This
//! meter does: everything the transparency, shading, pattern and optional content code does is charged to it in
//! units of about one nanosecond of a 2020s desktop core, in proportion to the work (pixels shaded or composited,
//! mask pixels built, mesh bits read, optional content nodes visited), whether or not it shows on the page.
//!
//! A page that has used up the meter does not start new work: the content stream stops at the next operator, the
//! page gives one warning, and what is drawn so far is kept. The weights below were measured on the machine the
//! tests run on (see `docs/decisions.md`, 3c); they need to be right to a factor of two, not exactly.

use std::cell::Cell;

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
}

/// The page's allowance, spent as it is drawn. The counters are cells so that the image decoders, which hold the
/// meter by shared reference while they work, can charge it too.
pub(crate) struct Work {
    left: Cell<f64>,
    used: Cell<f64>,
    over: Cell<bool>,
}

impl Work {
    pub fn new() -> Work {
        Work { left: Cell::new(PAGE_WORK), used: Cell::new(0.0), over: Cell::new(false) }
    }

    /// A meter with `units` to spend (tests of the decoders).
    #[cfg(test)]
    pub fn with_allowance(units: f64) -> Work {
        Work { left: Cell::new(units), used: Cell::new(0.0), over: Cell::new(false) }
    }

    /// Spend `units` on work about to be done. `false` (and nothing spent) when the page has not that much left; from
    /// then on every call says no.
    pub fn charge(&self, units: f64) -> bool {
        if self.over.get() || units.is_nan() || units > self.left.get() {
            self.over.set(true);
            return false;
        }
        self.left.set(self.left.get() - units);
        self.used.set(self.used.get() + units);
        true
    }

    /// Spend `units` on work that is done already, or must be finished to keep what is drawn (putting a layer on the
    /// page). The page may not start more when this takes what was left.
    pub fn spend(&self, units: f64) {
        if units.is_nan() || units < 0.0 {
            return;
        }
        self.used.set(self.used.get() + units);
        if units >= self.left.get() {
            self.left.set(0.0);
            self.over.set(true);
        } else {
            self.left.set(self.left.get() - units);
        }
    }

    /// Has the page used up its work (or been refused some)?
    pub fn is_over(&self) -> bool {
        self.over.get()
    }

    /// Units spent so far.
    #[cfg(test)]
    pub fn used(&self) -> f64 {
        self.used.get()
    }
}
