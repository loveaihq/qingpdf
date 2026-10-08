//! The work meter of a page. The limits of the interpreter count operators, path segments, painted area and image
//! pixels; none of them tells what a shading, a soft mask, a layer or a membership dictionary really costs. This
//! meter does: everything the transparency, shading, pattern and optional content code does is charged to it in
//! units of about one nanosecond of a 2020s desktop core, in proportion to the work (pixels shaded or composited,
//! mask pixels built, mesh bits read, optional content nodes visited), whether or not it shows on the page.
//!
//! A page that has used up the meter does not start new work: the content stream stops at the next operator, the
//! page gives one warning, and what is drawn so far is kept. The weights below were measured on the machine the
//! tests run on (see `docs/decisions.md`, 3c); they need to be right to a factor of two, not exactly.

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
}

/// The page's allowance, spent as it is drawn.
pub(crate) struct Work {
    left: f64,
    used: f64,
    over: bool,
}

impl Work {
    pub fn new() -> Work {
        Work { left: PAGE_WORK, used: 0.0, over: false }
    }

    /// Spend `units` on work about to be done. `false` (and nothing spent) when the page has not that much left; from
    /// then on every call says no.
    pub fn charge(&mut self, units: f64) -> bool {
        if self.over || units.is_nan() || units > self.left {
            self.over = true;
            return false;
        }
        self.left -= units;
        self.used += units;
        true
    }

    /// Spend `units` on work that is done already, or must be finished to keep what is drawn (putting a layer on the
    /// page). The page may not start more when this takes what was left.
    pub fn spend(&mut self, units: f64) {
        if units.is_nan() || units < 0.0 {
            return;
        }
        self.used += units;
        if units >= self.left {
            self.left = 0.0;
            self.over = true;
        } else {
            self.left -= units;
        }
    }

    /// Has the page used up its work (or been refused some)?
    pub fn is_over(&self) -> bool {
        self.over
    }

    /// Units spent so far.
    #[cfg(test)]
    pub fn used(&self) -> f64 {
        self.used
    }
}
