//! What the glyph readers draw into: a path in em units (1.0 is the font size), with a cap on
//! its size, so that no font file can ask for an endless outline.

use tiny_skia::{Path, PathBuilder};

/// Most segments (moves, lines, curves, closes) one glyph may have.
pub(crate) const MAX_SEGMENTS: usize = 40_000;

pub(crate) type Res<T> = Result<T, &'static str>;

/// A glyph outline under construction. Points are given in font units and are mapped to em
/// units by `matrix` (the font matrix, or 1 / units per em).
pub(crate) struct Builder {
    pb: PathBuilder,
    matrix: [f64; 6],
    segments: usize,
    open: bool,
    /// Work the glyph reader has done for this outline (charstring operators, points, segments), kept for the page's budget.
    work: usize,
}

impl Builder {
    pub fn new(matrix: [f64; 6]) -> Builder {
        Builder { pb: PathBuilder::new(), matrix, segments: 0, open: false, work: 0 }
    }

    fn map(&self, x: f64, y: f64) -> (f32, f32) {
        let m = &self.matrix;
        let (fx, fy) = (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        // A hostile font may name any number; keep the path finite and small.
        (fx.clamp(-1e4, 1e4) as f32, fy.clamp(-1e4, 1e4) as f32)
    }

    /// Add to the work done (the interpreters call this with what they spent, whether the glyph came out or not).
    pub fn charge(&mut self, work: usize) {
        self.work = self.work.saturating_add(work);
    }

    /// The work done so far: the interpreters' own, and one for each segment.
    pub fn work(&self) -> usize {
        self.work
    }

    fn count(&mut self) -> Res<()> {
        self.work = self.work.saturating_add(1);
        self.segments += 1;
        if self.segments > MAX_SEGMENTS {
            return Err("the glyph outline has too many segments");
        }
        Ok(())
    }

    /// Start a new contour; the one before it, if open, is closed.
    pub fn move_to(&mut self, x: f64, y: f64) -> Res<()> {
        self.close()?;
        self.count()?;
        let (x, y) = self.map(x, y);
        self.pb.move_to(x, y);
        self.open = true;
        Ok(())
    }

    pub fn line_to(&mut self, x: f64, y: f64) -> Res<()> {
        self.count()?;
        let (x, y) = self.map(x, y);
        self.pb.line_to(x, y);
        Ok(())
    }

    pub fn quad_to(&mut self, x1: f64, y1: f64, x: f64, y: f64) -> Res<()> {
        self.count()?;
        let ((x1, y1), (x, y)) = (self.map(x1, y1), self.map(x, y));
        self.pb.quad_to(x1, y1, x, y);
        Ok(())
    }

    pub fn cubic_to(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64) -> Res<()> {
        self.count()?;
        let ((x1, y1), (x2, y2), (x, y)) = (self.map(x1, y1), self.map(x2, y2), self.map(x, y));
        self.pb.cubic_to(x1, y1, x2, y2, x, y);
        Ok(())
    }

    pub fn close(&mut self) -> Res<()> {
        if self.open {
            self.count()?;
            self.pb.close();
            self.open = false;
        }
        Ok(())
    }

    /// The path, `None` when there is nothing to draw.
    pub fn finish(mut self) -> Option<Path> {
        let _ = self.close();
        self.pb.finish()
    }
}

/// Bytes a path takes in a cache (its points and verbs, and a little for the rest).
pub(crate) fn path_bytes(path: &Path) -> usize {
    64 + path.points().len() * 8 + path.verbs().len()
}
