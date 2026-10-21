//! A page read for editing its text (5a): the characters of the page as `TextExtractor::page_chars` finds them, and for each one
//! where it is in the content: the stream (the page's, or a form XObject's), the operator that shows it, the string of that
//! operator and the bytes of the character in the string. Nothing is changed here; `qingpdf-core` works out what to write.

use std::rc::Rc;

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::idmap::IdSet;
use crate::object::{Dict, ObjRef, Object};
use crate::render::work::{Work, cost};

use super::font::Font;
use super::interp::{FxMap, Glyph, Interp, Scope, Shared, spent};
use super::layout::{self, Mark};

/// Most content streams (the page's own and forms) one page may be read with, and fonts it may switch between.
const MAX_SOURCES: usize = 4_096;
const MAX_FONTS: usize = 20_000;
/// Most pieces one page may be read into: text-showing operators that show a character, their strings and the numbers of their
/// arrays, and marked-content sequences with an `/ActualText` (each takes some 100 bytes). Past it the page is refused with
/// `Error::Limit`; operators that show nothing, and empty strings, are not kept and do not count.
pub(super) const MAX_PIECES: usize = 400_000;
/// The most bytes of content a page may have to be read for editing (the text of the page is kept next to it).
const MAX_CONTENT: usize = 64 << 20;

/// The operators that show text (9.4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowOp {
    /// `(s) Tj`
    Tj,
    /// `[...] TJ`
    TJ,
    /// `(s) '`
    Quote,
    /// `aw ac (s) "`
    DoubleQuote,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShowItem {
    /// A string of the operator, by its index in [`Scan::parts`].
    Str(u32),
    /// A number of a `TJ` array.
    Num(f64),
}

/// One use of a text-showing operator.
#[derive(Clone, Debug)]
pub struct Show {
    /// The content stream it is in ([`Scan::sources`]) and the bytes it takes there, operands included.
    pub src: u32,
    pub start: u32,
    pub end: u32,
    pub op: ShowOp,
    /// The font at that point ([`Scan::fonts`]; `u32::MAX`: none), its size, and the character and word spacing and horizontal
    /// scaling in force (for `"`, after its own operands).
    pub font: u32,
    pub size: f64,
    pub tc: f64,
    pub tw: f64,
    pub th: f64,
    /// The text rendering mode (9.3.6): 3 draws nothing.
    pub render: u8,
    /// The operands `aw` and `ac` of a `"`.
    pub spacing: Option<(f64, f64)>,
    pub items: Vec<ShowItem>,
    /// How far a unit of text space runs along the line in default user space.
    pub scale: f64,
}

/// A string operand, decoded.
#[derive(Clone, Debug)]
pub struct Part {
    pub show: u32,
    pub bytes: Vec<u8>,
}

/// Where a character is: its string, and its bytes in it. One for each glyph of [`Scan::glyphs`], in the same order.
#[derive(Clone, Copy, Debug)]
pub struct CharRec {
    pub part: u32,
    pub off: u32,
    pub len: u8,
}

/// A font as a content stream names it.
#[derive(Clone)]
pub struct FontRec {
    pub src: u32,
    /// The name in the `/Font` resources, and what they have under it (a reference, or the dictionary itself).
    pub name: Vec<u8>,
    pub entry: Object,
    pub font: Rc<Font>,
}

/// Where one of the page's content streams is in its joined content (start, end) and which object it is (`None`: not a reference).
pub type StreamSpan = (usize, usize, Option<ObjRef>);

/// A content stream the text is in.
#[derive(Clone)]
pub struct Source {
    /// The form XObject it is (`None`: the page's own content, all its streams one after the other, a line feed between).
    pub form: Option<ObjRef>,
    /// The stream that draws this one with `Do`, and the name it used for it in its resources.
    pub parent: u32,
    pub name: Vec<u8>,
    /// The form's stream dictionary (empty for the page).
    pub dict: Dict,
    pub content: Rc<Vec<u8>>,
    /// How many times the page draws it.
    pub uses: u32,
    /// Other places that draw it by another name or from another stream: (the stream, the name). Empty when every draw is the one
    /// `parent` and `name` say.
    pub also: Vec<(u32, Vec<u8>)>,
    /// For the page: where each of its streams is in `content` (start, end) and which object it is (`None`: not a reference).
    pub streams: Vec<StreamSpan>,
    /// For the page: for each of `streams`, whether its object is in the page's list more than once.
    pub repeated: Vec<bool>,
}

impl Source {
    /// The stream of the page (an index of `streams`) that holds all of the bytes `a..b` of its content, if there is one.
    pub fn stream_at(&self, a: usize, b: usize) -> Option<usize> {
        let i = self.streams.partition_point(|s| s.0 <= a).checked_sub(1)?;
        self.streams.get(i).filter(|s| b <= s.1).map(|_| i)
    }
}

/// A marked-content sequence (`BDC` ... `EMC`) whose property list has an `/ActualText` (14.9.4): extraction takes the sequence's
/// text from there, not from the characters drawn in it.
#[derive(Clone, Copy, Debug)]
pub struct ActualSpan {
    /// The `BDC` statement (a name and a property list written in the operator): its stream and its bytes there.
    pub src: u32,
    pub start: u32,
    pub end: u32,
    /// The characters drawn in it are `glyphs[first..last]`.
    pub first: u32,
    pub last: u32,
    /// The sequence of this kind that it is inside (`u32::MAX`: none).
    pub parent: u32,
}

/// What the page draws other than text, for taking it out (redaction): where the statement is, and the matrix the drawing is under.
#[derive(Clone, Debug)]
pub struct Draw {
    /// The content stream it is in ([`Scan::sources`]) and the bytes of the statement there, operands included.
    pub src: u32,
    pub start: u32,
    pub end: u32,
    /// The current transformation matrix of default user space when it is drawn.
    pub ctm: [f64; 6],
    pub kind: DrawKind,
}

#[derive(Clone, Debug)]
pub enum DrawKind {
    /// `Do` of an image XObject.
    Image(ObjRef),
    /// `BI ... ID ... EI`: the statement holds the image.
    Inline,
    /// A path painted by `S`, `f`, `B` or one of their kin (a path that is a clip is not one): its bounds in default user space, the
    /// width of the stroke included.
    Path([f64; 4]),
}

/// What reading the page for editing finds.
pub struct Scan {
    pub sources: Vec<Source>,
    pub shows: Vec<Show>,
    pub parts: Vec<Part>,
    /// The characters shown, in the order they are drawn: `glyphs[i]` is where and how big, `chars[i]` where in the content.
    pub glyphs: Vec<Glyph>,
    pub chars: Vec<CharRec>,
    pub fonts: Vec<FontRec>,
    /// The page's text the way text extraction gives it, and for each character's text where it comes from.
    pub text: String,
    pub marks: Vec<Mark>,
    /// The marked-content sequences with an `/ActualText`, by `BDC` order; and from which glyph on which of them is the innermost
    /// one the glyphs are in (`u32::MAX`: none).
    pub spans: Vec<ActualSpan>,
    /// The images and paths drawn (only when the page is read for redaction), in the order they are drawn.
    pub draws: Vec<Draw>,
    /// Text that was shown with no font (not read); the patterns chosen as a colour (the stream, the name); and what is painted with them: (the pattern as an
    /// index of those, the path drawn as an index of `draws`; `u32::MAX`: something else, text or a picture).
    pub unreadable: usize,
    pub pat_names: Vec<(u32, Vec<u8>)>,
    pub pat_uses: Vec<(u32, u32)>,
    runs: Vec<(u32, u32)>,
}

impl Scan {
    /// The innermost sequence with an `/ActualText` that the glyph is drawn in.
    pub fn span_at(&self, glyph: usize) -> Option<usize> {
        let g = u32::try_from(glyph).ok()?;
        let i = self.runs.partition_point(|r| r.0 <= g).checked_sub(1)?;
        self.runs.get(i).map(|r| r.1 as usize).filter(|&s| s < self.spans.len())
    }
}

/// What the interpreter notes down while it runs a page.
pub struct Recorder {
    pub sources: Vec<Source>,
    pub shows: Vec<Show>,
    pub parts: Vec<Part>,
    pub chars: Vec<CharRec>,
    pub fonts: Vec<FontRec>,
    font_index: FxMap<(u32, Vec<u8>), u32>,
    /// The forms by object, for finding the source of one again (sorted).
    form_index: Vec<(ObjRef, u32)>,
    /// How many pieces are kept (see [`MAX_PIECES`]) and, for the operator being run, how many strings and characters there were
    /// before it.
    pieces: usize,
    pub(super) mark: (usize, usize),
    pub(super) spans: Vec<ActualSpan>,
    /// The sequences open now (`u32::MAX`: one without an `/ActualText`), for each level of marked content, and the innermost
    /// sequence with one.
    pub(super) open: Vec<u32>,
    pub(super) runs: Vec<(u32, u32)>,
    /// The images and paths drawn, kept only when `graphics` is set.
    pub draws: Vec<Draw>,
    pub(super) graphics: bool,
    pub(super) pat_names: Vec<(u32, Vec<u8>)>,
    pub(super) pat_uses: Vec<(u32, u32)>,
}

impl Recorder {
    fn new(page: Source) -> Recorder {
        Recorder {
            sources: vec![page],
            shows: Vec::new(),
            parts: Vec::new(),
            chars: Vec::new(),
            fonts: Vec::new(),
            font_index: FxMap::default(),
            form_index: Vec::new(),
            pieces: 0,
            mark: (0, 0),
            spans: Vec::new(),
            open: Vec::new(),
            runs: Vec::new(),
            draws: Vec::new(),
            graphics: false,
            pat_names: Vec::new(),
            pat_uses: Vec::new(),
        }
    }

    /// A drawing is kept (when the page is read for redaction); an error when the page has too many.
    pub(super) fn add_draw(&mut self, draw: Draw) -> Result<()> {
        self.add_pieces(1)?;
        self.draws.push(draw);
        Ok(())
    }

    /// `n` more pieces are kept; an error when the page has too many.
    pub(super) fn add_pieces(&mut self, n: usize) -> Result<()> {
        self.pieces = self.pieces.saturating_add(n);
        if self.pieces > MAX_PIECES {
            return Err(Error::Limit("the page has too much text in too many pieces to be changed".to_string()));
        }
        Ok(())
    }

    pub(super) fn drop_pieces(&mut self, n: usize) {
        self.pieces = self.pieces.saturating_sub(n);
    }

    /// A form is drawn: the index of its source (made when the page first draws it).
    pub(super) fn enter_form(&mut self, r: ObjRef, name: &[u8], parent: u32, content: &Rc<Vec<u8>>, dict: Option<&Dict>) -> u32 {
        let at = self.form_index.partition_point(|e| e.0 < r);
        if let Some(&(_, i)) = self.form_index.get(at).filter(|e| e.0 == r) {
            if let Some(s) = self.sources.get_mut(i as usize) {
                s.uses = s.uses.saturating_add(1);
                if (s.parent != parent || s.name != name) && s.also.len() < 64 && !s.also.iter().any(|(p, n)| *p == parent && n == name) {
                    s.also.push((parent, name.to_vec()));
                }
            }
            return i;
        }
        if self.sources.len() >= MAX_SOURCES {
            // (Too many forms to follow: the text in this one is taken as the parent's, and cannot be edited.)
            return u32::MAX;
        }
        self.sources.push(Source {
            form: Some(r),
            parent,
            name: name.to_vec(),
            dict: dict.cloned().unwrap_or_default(),
            content: Rc::clone(content),
            uses: 1,
            also: Vec::new(),
            streams: Vec::new(),
            repeated: Vec::new(),
        });
        let i = u32::try_from(self.sources.len() - 1).unwrap_or(0);
        self.form_index.insert(at, (r, i));
        i
    }

    pub(super) fn font(&mut self, src: u32, name: &[u8], entry: Object, font: Rc<Font>) -> u32 {
        if let Some(&i) = self.font_index.get(&(src, name.to_vec()))
            && self.fonts.get(i as usize).is_some_and(|f| Rc::ptr_eq(&f.font, &font))
        {
            return i;
        }
        if self.fonts.len() >= MAX_FONTS {
            return u32::MAX;
        }
        let i = u32::try_from(self.fonts.len()).unwrap_or(u32::MAX);
        self.fonts.push(FontRec { src, name: name.to_vec(), entry, font });
        self.font_index.insert((src, name.to_vec()), i);
        i
    }
}

/// Reads pages for editing. Fonts and forms are kept between pages, so one scanner should do all the pages of a run.
pub struct EditScanner<'a> {
    doc: &'a Document,
    shared: Shared,
    /// Read for redaction: all the text (that of watermarks and page numbers too), and the images and paths drawn.
    redaction: bool,
}

impl<'a> EditScanner<'a> {
    pub fn new(doc: &'a Document) -> EditScanner<'a> {
        EditScanner { doc, shared: Shared::default(), redaction: false }
    }

    /// A scanner for taking things off the pages: it also reads the text of watermarks and page numbers, and notes down the images
    /// and paths that are drawn.
    pub fn for_redaction(doc: &'a Document) -> EditScanner<'a> {
        EditScanner { redaction: true, ..EditScanner::new(doc) }
    }

    /// Read `page`; everything it costs is charged to `work` (`Error::Limit` or `Error::Cancelled` when that is spent).
    pub fn scan(&mut self, page: &Page, work: &Work) -> Result<Scan> {
        if self.doc.is_locked() {
            return Err(Error::PasswordRequired);
        }
        let (content, streams) = super::page_content_parts(self.doc, page, &mut self.shared.warnings)?;
        if content.len() > MAX_CONTENT {
            return Err(Error::Limit("the page's content is too big to be changed".to_string()));
        }
        if !work.charge(content.len() as f64 * cost::TEXT_BYTE) {
            return Err(spent(work));
        }
        let content = Rc::new(content);
        let (mut seen, mut twice): (IdSet<ObjRef>, IdSet<ObjRef>) = (IdSet::default(), IdSet::default());
        for r in streams.iter().filter_map(|s| s.2) {
            if !seen.insert(r) {
                twice.insert(r);
            }
        }
        let repeated = streams.iter().map(|s| s.2.is_some_and(|r| twice.contains(&r))).collect();
        let source = Source { form: None, parent: 0, name: Vec::new(), dict: Dict::new(), content: Rc::clone(&content), uses: 1, also: Vec::new(), streams, repeated };
        let scope = Rc::new(Scope::new(self.doc, page.resources()));
        let mut interp = Interp::new(self.doc, &mut self.shared).with_work(work);
        let mut recorder = Recorder::new(source);
        recorder.graphics = self.redaction;
        interp.keep_artifacts = self.redaction;
        interp.rec = Some(Box::new(recorder));
        interp.run(&content, &scope, 0)?;
        let glyphs = std::mem::take(&mut interp.glyphs);
        let rec = interp.rec.take();
        let unreadable = interp.unreadable;
        drop(interp);
        if !work.charge(glyphs.len() as f64 * cost::TEXT_LAYOUT) {
            return Err(spent(work));
        }
        let (text, marks) = layout::render_marked(&glyphs);
        let Some(rec) = rec else { return Err(Error::Invalid("the page was not read for editing".to_string())) };
        let mut rec = *rec;
        // A sequence that was never closed runs to the end of the page.
        let end = u32::try_from(glyphs.len()).unwrap_or(u32::MAX);
        for s in &mut rec.spans {
            s.last = s.last.min(end);
        }
        Ok(Scan { sources: rec.sources, shows: rec.shows, parts: rec.parts, glyphs, chars: rec.chars, fonts: rec.fonts, text, marks, spans: rec.spans, draws: rec.draws, unreadable, pat_names: rec.pat_names, pat_uses: rec.pat_uses, runs: rec.runs })
    }

    /// What went wrong without stopping the reading, and forget it.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.shared.warnings.list)
    }
}
