//! PDF to Excel (5d): a .xlsx with a sheet for each table of the pages (ISO/IEC 29500-1, SpreadsheetML: 18.2 workbook, 18.3 worksheets,
//! 18.4 shared strings, 18.8 styles and number formats).
//!
//! The tables are found as the Word export finds them ([`crate::tables`]: ruled ones from the lines of the page, others from text
//! that lines up in rows and columns), one reading of each page. Each table becomes a sheet when its page is done, so that no more
//! than one sheet is in memory (a table that goes on over several pages, a statement, is one sheet: see [`Pending`]); the strings, which the sheets share, and the number formats they use are written at the end. A file
//! with no table at all gets one sheet with the text of its pages, line by line.
//!
//! ```no_run
//! # use qingpdf_core::{Document, xlsx};
//! let doc = Document::open("a.pdf")?;
//! let report = xlsx::export_to_path(&doc, "", None, std::path::Path::new("a.xlsx"), None)?;
//! println!("{} pages, {} tables", report.pages, report.tables);
//! # Ok::<(), qingpdf_core::Error>(())
//! ```

mod cell;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use qingpdf_base::render::work::{PAGE_WORK, Work};
use qingpdf_base::text::{PageLayout, TextExtractor, TextLine};

use crate::tables::{self, Grid, is_wide, sort};
use crate::xml::{esc, esc_attr, xml_ok};
use crate::zip::{self, Zip};
use crate::{Document, Error, Result, text};

use cell::{Fmt, Value, detect};

/// Most pages, characters, sheets (tables), cells with something in them and bytes of text one workbook is made of; the characters in
/// a cell (a spreadsheet's own limit); and the work all the pages may take together (every page has its own allowance of [`PAGE_WORK`] as well).
pub const MAX_PAGES: usize = 5_000;
pub const MAX_CHARS: u64 = 8_000_000;
pub const MAX_SHEETS: usize = 1_000;
pub const MAX_CELL_CHARS: usize = 32_767;
pub const MAX_CELLS: u64 = 4_000_000;
const MAX_STRING_BYTES: usize = 128 << 20;
const MAX_SHEET_XML: u64 = 256 << 20;
/// The lines of text put in one sheet when there is no table: a spreadsheet has 1,048,576 rows.
const MAX_TEXT_LINES: usize = 1_000_000;
const JOB_WORK: f64 = 40.0 * PAGE_WORK;
/// Work units for a cell of a table (finding its text, deciding what it is, writing it).
const CELL_WORK: f64 = 10_000.0;
/// Most formats (kinds of number) the sheets use (Excel keeps about 250 custom ones; a number beyond this is shown in the general format).
const MAX_FORMATS: usize = 200;
/// What a shared string costs besides its text (the map, the list, the markup), in the bytes the strings may have together.
const STRING_OVERHEAD: usize = 64;
/// Most rows, and bytes of XML, of a table that goes on over pages (a sheet of Excel has 1,048,576 rows).
const MAX_TABLE_ROWS: usize = 100_000;
const MAX_TABLE_XML: usize = 32 << 20;

/// What was written.
#[derive(Debug, Default)]
pub struct Report {
    /// Pages converted (a page that was too much to read is left out and reported in `warnings`).
    pub pages: usize,
    /// Sheets with a table (0 when the file has none: the one sheet then has the text).
    pub tables: usize,
    /// Cells with something in them.
    pub cells: u64,
    /// Size of the file.
    pub bytes: u64,
    pub warnings: Vec<String>,
}

/// Make a .xlsx of the tables of the pages `pages` (counted from 0; `None`: all) of `doc` and write it to `out`. `password` is the one
/// the file was opened with (the author's permission to take text out of the file is looked at). `cancel` stops the work when it is
/// set ([`Error::Cancelled`]). Nothing from the file can make this panic; what is too much for a limit gives [`Error::Limit`].
pub fn export<W: Write>(doc: &Document, password: &str, pages: Option<&[usize]>, out: W, cancel: Option<Arc<AtomicBool>>) -> Result<Report> {
    text::check_extraction_allowed(doc, password)?;
    let count = doc.page_count()?;
    let wanted: Vec<usize> = match pages {
        Some(list) => list.to_vec(),
        None => (0..count).collect(),
    };
    if wanted.len() > MAX_PAGES {
        return Err(Error::Limit(format!("{} pages are more than the {MAX_PAGES} one Excel file is made of; choose some of them", wanted.len())));
    }
    let all = doc.pages()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut zip = Zip::new(out, now);
    let mut extractor = TextExtractor::new(doc);
    let job = Work::with_allowance(JOB_WORK);
    let mut book = Book::default();
    // The text of the pages, while no page has had a table.
    let mut lines: Option<Vec<String>> = Some(Vec::new());
    let mut warnings = Vec::new();
    let (mut pages_done, mut chars, mut left_out) = (0usize, 0u64, 0usize);
    for &index in &wanted {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let Some(page) = all.get(index) else { return Err(Error::Invalid(format!("the file has no page {}", index + 1))) };
        let work = Work::new().with_cancel(cancel.clone());
        match convert_page(&mut extractor, page, index + 1, &work, &mut book, &mut zip, &mut lines) {
            Ok(n) => {
                pages_done += 1;
                chars += n;
                if chars > MAX_CHARS {
                    return Err(Error::Limit(format!("more than {MAX_CHARS} characters; choose some of the pages")));
                }
            }
            // (What the whole workbook is too much for ends the conversion; what one page is too much for leaves that page out.)
            Err(Error::Limit(m)) if book.full => return Err(Error::Limit(m)),
            Err(Error::Limit(m)) => {
                left_out += 1;
                warnings.push(format!("page {} is left out: {m}", index + 1));
            }
            Err(e) => return Err(e),
        }
        if !job.charge(work.used()) {
            return Err(Error::Limit("the pages ask for more work than one conversion is allowed; choose some of the pages".to_string()));
        }
    }
    if let Some(table) = book.pending.take() {
        book.close(&mut zip, table)?;
    }
    warnings.extend(extractor.take_warnings());
    if book.truncated > 0 {
        warnings.push(format!("{} cell{} cut to {MAX_CELL_CHARS} characters", book.truncated, if book.truncated == 1 { "" } else { "s" }));
    }
    let tables = book.sheets.len();
    if tables == 0 {
        // No table: the text of the pages, a line to a row.
        let rows = lines.unwrap_or_default();
        warnings.push(nothing_found(!rows.is_empty(), left_out, pages_done));
        let xml = book.text_sheet(&rows);
        book.add_sheet(&mut zip, "文字", xml)?;
    }
    book.finish(&mut zip, now)?;
    let (_, bytes) = zip.finish()?;
    Ok(Report { pages: pages_done, tables, cells: book.cells, bytes, warnings })
}

/// What to say when no page had a table: whether there is text, how many pages were left out for a limit, how many were read.
fn nothing_found(text: bool, left_out: usize, pages_done: usize) -> String {
    let s = if left_out == 1 { "" } else { "s" };
    match (text, left_out) {
        (false, 0) => "no table and no text was found".to_string(),
        (false, n) => format!("nothing was converted: {n} page{s} left out (see above){}", if pages_done > 0 { ", and the others have no table and no text" } else { "" }),
        (true, 0) => "no table was found: the text of the pages is in one sheet, line by line".to_string(),
        (true, n) => format!("no table was found in the pages that could be read ({n} page{s} left out, see above): their text is in one sheet, line by line"),
    }
}

/// [`export`] to the file `path`: written into a file of its own in the same folder, which replaces `path` only when it is whole.
pub fn export_to_path(doc: &Document, password: &str, pages: Option<&[usize]>, path: &Path, cancel: Option<Arc<AtomicBool>>) -> Result<Report> {
    zip::write_file(path, |sink| export(doc, password, pages, sink, cancel))
}

/// The sheets, the strings and the formats of the workbook so far.
#[derive(Default)]
struct Book {
    /// The names of the sheets, in order, and the same in lower case (a name is taken when it differs by case only).
    sheets: Vec<String>,
    taken: HashSet<String>,
    /// The shared strings: the number of each, the bytes of all, and how many cells use one.
    strings: HashMap<String, u32>,
    string_bytes: usize,
    string_uses: u64,
    formats: Vec<Fmt>,
    cells: u64,
    xml_bytes: u64,
    /// Cells with more characters than a cell may have.
    truncated: usize,
    /// A limit of the whole workbook was reached.
    full: bool,
    /// The table that ended its page at the bottom of it, and may go on with the next page.
    pending: Option<Pending>,
}

/// A table on its way to being a sheet: the XML of its rows so far (a table that goes on over pages gets the rows of each page
/// after those of the one before), and what the next page needs to know to see whether it goes on.
struct Pending {
    data: String,
    merges: String,
    merged: usize,
    widths: Vec<usize>,
    rows: usize,
    /// The column edges and the texts of the first row of the table on its first page.
    xs: Vec<f32>,
    header: Vec<String>,
    /// The first and the last page (from 1) it is on, and its number among the tables of the first.
    first: usize,
    last: usize,
    k: usize,
}

impl Pending {
    fn new(grid: &Grid, texts: &[String], number: usize, k: usize) -> Pending {
        let cols = grid.cols();
        Pending {
            data: String::new(),
            merges: String::new(),
            merged: 0,
            widths: vec![0; cols],
            rows: 0,
            xs: grid.xs.clone(),
            header: texts.get(..cols).unwrap_or_default().to_vec(),
            first: number,
            last: number,
            k,
        }
    }

    /// Does the table `grid` of page `number` go on from this one (which ended the page before)? It has the same columns (the edges
    /// within a few points; those of the sides, which are where the text of the page ends, within more), it begins in the upper
    /// part of the page (below a running header, if the page has one), and there is room left.
    fn goes_on(&self, grid: &Grid, layout: &PageLayout, number: usize) -> bool {
        let last = self.xs.len().saturating_sub(1);
        let same = |(i, (a, b)): (usize, (&f32, &f32))| (a - b).abs() <= if i == 0 || i == last { 20.0 } else { 6.0 };
        self.last + 1 == number
            && grid.xs.len() == self.xs.len()
            && grid.xs.iter().zip(&self.xs).enumerate().all(same)
            && grid.bbox[1] <= 0.25 * layout.size.1
            && self.rows + grid.rows() <= MAX_TABLE_ROWS
            && self.data.len() < MAX_TABLE_XML
    }

    /// The XML of the sheet.
    fn xml(&self) -> String {
        let cols = self.widths.len();
        let mut xml = String::with_capacity(self.data.len() + 512);
        xml.push_str(SHEET_HEAD);
        let _ = write!(xml, "<dimension ref=\"A1:{}{}\"/><sheetViews><sheetView workbookViewId=\"0\"/></sheetViews><sheetFormatPr defaultRowHeight=\"15\"/><cols>", column_name(cols.saturating_sub(1)), self.rows.max(1));
        for (c, w) in self.widths.iter().enumerate() {
            let _ = write!(xml, "<col min=\"{0}\" max=\"{0}\" width=\"{1}\" customWidth=\"1\"/>", c + 1, (*w + 2).clamp(6, 60));
        }
        xml.push_str("</cols><sheetData>");
        xml.push_str(&self.data);
        xml.push_str("</sheetData>");
        if self.merged > 0 {
            let _ = write!(xml, "<mergeCells count=\"{}\">{}</mergeCells>", self.merged, self.merges);
        }
        xml.push_str(SHEET_TAIL);
        xml
    }
}

/// A piece of the text of a cell: the pieces `from..to` of line `line` of the page, where on the page.
#[derive(Clone, Copy)]
struct Part {
    line: usize,
    from: usize,
    to: usize,
    across: f32,
    along: f32,
    size: f32,
}

/// `[left, top, right, bottom]` of the pieces of a line.
fn bounds(line: &TextLine) -> [f32; 4] {
    let mut b = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for p in &line.pieces {
        b = [b[0].min(p.bbox[0]), b[1].min(p.bbox[1]), b[2].max(p.bbox[2]), b[3].max(p.bbox[3])];
    }
    b
}

/// Does a table end where its page ends: low on the page, with no text under it but a footer (the lowest tenth of the page)? Only
/// such a table may go on with the next page.
fn ends_page(layout: &PageLayout, grid: &Grid) -> bool {
    let h = layout.size.1;
    grid.bbox[3] >= 0.5 * h
        && !layout.lines.iter().any(|l| {
            let b = bounds(l);
            let y = 0.5 * (b[1] + b[3]);
            !l.vertical && !l.pieces.is_empty() && y > grid.bbox[3] + 1.5 && y < 0.9 * h
        })
}

/// The text of the cells of every grid (row by row), from one pass over the lines of the page: a piece of text is in the cell it is
/// in, and what is in a cell is read top to bottom, left to right.
fn texts(layout: &PageLayout, grids: &[Grid]) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<Vec<Part>>> = grids.iter().map(|g| vec![Vec::new(); g.rows() * g.cols()]).collect();
    for (li, line) in layout.lines.iter().enumerate() {
        if line.vertical || line.pieces.is_empty() {
            continue;
        }
        let b = bounds(line);
        let (x, y) = (0.5 * (b[0] + b[2]), 0.5 * (b[1] + b[3]));
        let Some(g) = grids.iter().position(|g| x >= g.bbox[0] - 1.5 && x <= g.bbox[2] + 1.5 && y >= g.bbox[1] - 1.5 && y <= g.bbox[3] + 1.5) else { continue };
        let Some(grid) = grids.get(g) else { continue };
        let cell_of = |p: &qingpdf_base::text::Piece| grid.cell_at(0.5 * (p.bbox[0] + p.bbox[2]), 0.5 * (p.bbox[1] + p.bbox[3]));
        let mut at = 0usize;
        while let Some(first) = line.pieces.get(at) {
            let cell = cell_of(first);
            let mut end = at + 1;
            while line.pieces.get(end).is_some_and(|p| cell_of(p) == cell) {
                end += 1;
            }
            let part = Part { line: li, from: at, to: end, across: 0.5 * (first.bbox[1] + first.bbox[3]), along: first.bbox[0], size: first.size };
            if let Some(slot) = parts.get_mut(g).and_then(|cells| cells.get_mut(cell.0 * grid.cols() + cell.1)) {
                slot.push(part);
            }
            at = end;
        }
    }
    parts.into_iter().map(|cells| cells.into_iter().map(|p| join(layout, p)).collect()).collect()
}

/// The text of the parts of one cell: the lines of text in it from top to bottom, the parts of a line left to right, no space where
/// a wide character is next to the join.
fn join(layout: &PageLayout, mut parts: Vec<Part>) -> String {
    sort(&mut parts, &mut |a, b| a.across < b.across);
    let mut out = String::new();
    let mut i = 0;
    while i < parts.len() {
        let mut j = i + 1;
        while parts.get(j).zip(parts.get(i)).is_some_and(|(p, first)| p.across - first.across <= 0.5 * first.size.max(1.0)) {
            j += 1;
        }
        if let Some(line) = parts.get_mut(i..j) {
            sort(line, &mut |a, b| a.along < b.along);
            for p in line.iter() {
                let Some(l) = layout.lines.get(p.line) else { continue };
                let (Some(first), Some(last)) = (l.pieces.get(p.from), l.pieces.get(p.to.saturating_sub(1))) else { continue };
                let piece = l.text.get(first.start as usize..last.end as usize).unwrap_or_default().trim();
                if let (Some(a), Some(b)) = (out.chars().next_back(), piece.chars().next())
                    && !(is_wide(a) || is_wide(b))
                {
                    out.push(' ');
                }
                out.push_str(piece);
            }
        }
        i = j;
    }
    // (What XML has no place for is dropped, the other kinds of space are spaces.)
    out.chars().filter_map(|c| if c.is_whitespace() { Some(' ') } else { xml_ok(c).then_some(c) }).collect::<String>().trim().to_string()
}

fn convert_page<W: Write>(
    extractor: &mut TextExtractor<'_>,
    page: &crate::Page,
    number: usize,
    work: &Work,
    book: &mut Book,
    zip: &mut Zip<W>,
    lines: &mut Option<Vec<String>>,
) -> Result<u64> {
    let layout = extractor.page_layout(page, work)?;
    if !work.charge(tables::cost(&layout)) {
        return Err(Error::Limit("the page asks for more work than one page of a conversion is allowed".to_string()));
    }
    let chars = layout.lines.iter().map(|l| l.text.chars().count()).sum::<usize>() as u64;
    let (grids, found) = tables::find(&layout, tables::vertical(&layout), false);
    let cells: usize = grids.iter().map(|g| g.rows() * g.cols()).sum();
    if !work.charge(found + cells as f64 * CELL_WORK) {
        return Err(Error::Limit("the tables of the page are more than one page of a conversion is allowed".to_string()));
    }
    if grids.is_empty() {
        if let Some(table) = book.pending.take() {
            book.close(zip, table)?;
        }
        // (Kept for the sheet of text, until a page has a table.)
        if let Some(rows) = lines.as_mut() {
            if !rows.is_empty() {
                rows.push(String::new());
            }
            for l in &layout.lines {
                if rows.len() < MAX_TEXT_LINES {
                    rows.push(l.text.clone());
                }
            }
        }
        return Ok(chars);
    }
    *lines = None;
    let all = texts(&layout, &grids);
    let kept: Vec<usize> = (0..grids.len()).filter(|&i| all.get(i).is_some_and(|t| t.iter().any(|s| !s.is_empty()))).collect();
    // The table the page before ended with: the first table of this page goes on from it, or it is a sheet as it is.
    let mut before = book.pending.take();
    for (n, &i) in kept.iter().enumerate() {
        let (Some(grid), Some(cell_texts)) = (grids.get(i), all.get(i)) else { continue };
        let mut table = match before.take() {
            Some(t) if t.goes_on(grid, &layout, number) => t,
            other => {
                if let Some(t) = other {
                    book.close(zip, t)?;
                }
                Pending::new(grid, cell_texts, number, n + 1)
            }
        };
        // (The heading, written again at the top of the page, is not a row of the table twice.)
        let skip = table.rows > 0 && grid.rows() > 1 && table.header.iter().any(|h| !h.is_empty()) && cell_texts.get(..grid.cols()) == Some(table.header.as_slice());
        book.append(&mut table, grid, cell_texts, skip)?;
        if book.cells > MAX_CELLS {
            return Err(book.full(format!("more than {MAX_CELLS} cells; choose some of the pages")));
        }
        table.last = number;
        if n + 1 == kept.len() && ends_page(&layout, grid) {
            book.pending = Some(table);
        } else {
            book.close(zip, table)?;
        }
    }
    if let Some(table) = before {
        book.close(zip, table)?;
    }
    Ok(chars)
}

/// The name of column `c` (from 0): A to Z, AA, AB...
fn column_name(c: usize) -> String {
    let mut s = String::new();
    let mut n = c + 1;
    while n > 0 {
        s.insert(0, char::from(b'A' + ((n - 1) % 26) as u8));
        n = (n - 1) / 26;
    }
    s
}

/// How wide a text looks, in characters of the digit's width (a wide character counts as two).
fn display_width(text: &str) -> usize {
    text.chars().map(|c| if is_wide(c) { 2 } else { 1 }).sum()
}

const SHEET_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">";
const SHEET_TAIL: &str = "<pageMargins left=\"0.7\" right=\"0.7\" top=\"0.75\" bottom=\"0.75\" header=\"0.3\" footer=\"0.3\"/></worksheet>";

impl Book {
    /// A limit of the whole workbook: the error to give, and no page is left out for it, the conversion ends.
    fn full(&mut self, message: String) -> Error {
        self.full = true;
        Error::Limit(message)
    }

    /// The number of a string among the shared ones.
    fn string(&mut self, text: &str) -> Result<u32> {
        self.string_uses += 1;
        if let Some(&i) = self.strings.get(text) {
            return Ok(i);
        }
        let cost = text.len() + STRING_OVERHEAD;
        if self.string_bytes + cost > MAX_STRING_BYTES {
            return Err(self.full("the text of the cells is too much for one Excel file; choose some of the pages".to_string()));
        }
        let i = u32::try_from(self.strings.len()).map_err(|_| self.full("too many different texts".to_string()))?;
        self.string_bytes += cost;
        self.strings.insert(text.to_string(), i);
        Ok(i)
    }

    /// The style (the position among the cell formats of the styles part) for numbers shown as `f`; 0 is the general format.
    fn style(&mut self, f: Fmt) -> usize {
        if f == Fmt::default() {
            return 0;
        }
        match self.formats.iter().position(|g| *g == f) {
            Some(i) => i + 1,
            None if self.formats.len() < MAX_FORMATS => {
                self.formats.push(f);
                self.formats.len()
            }
            None => 0,
        }
    }

    /// The cell at row `r` and column `c`, with text `text` that is `value`: a number, a date, or a shared string.
    fn cell(&mut self, out: &mut String, r: usize, c: usize, text: &str, value: Value) -> Result<()> {
        let at = format!("{}{}", column_name(c), r + 1);
        match value {
            Value::Number(v, f) | Value::Weak(v, f) => {
                let style = self.style(f);
                let _ = if style == 0 { write!(out, "<c r=\"{at}\"><v>{v}</v></c>") } else { write!(out, "<c r=\"{at}\" s=\"{style}\"><v>{v}</v></c>") };
            }
            Value::Text => {
                let i = self.string(text)?;
                let _ = write!(out, "<c r=\"{at}\" t=\"s\"><v>{i}</v></c>");
            }
        }
        self.cells += 1;
        Ok(())
    }

    /// Put the rows of a table after those `table` has: the cells have the texts `texts` (a row after the other, `cols` to a row); the
    /// first row is left out if `skip`. A first pass finds what each cell is, so that the second knows the column it is in: a whole
    /// number in brackets or with the minus after it is a negative number if the column is made of amounts (more than half of its
    /// other numbers have decimals, separators or a currency sign), else a list number or a range, and text.
    fn append(&mut self, table: &mut Pending, grid: &Grid, texts: &[String], skip: bool) -> Result<()> {
        let (rows, cols) = (grid.rows(), grid.cols());
        let (first, base) = (usize::from(skip), table.rows);
        let mut placed: Vec<(usize, usize, Cow<'_, str>, Value)> = Vec::new();
        // For each column: its numbers (dates not counted), and how many of them are written as amounts.
        let mut count = vec![(0usize, 0usize); cols];
        for r in first..rows {
            for c in 0..cols {
                if grid.owner(r, c) != (r, c) {
                    continue;
                }
                let (mut across, mut down) = (1usize, 1usize);
                while c + across < cols && grid.owner(r, c + across) == (r, c) {
                    across += 1;
                }
                while r + down < rows && grid.owner(r + down, c) == (r, c) {
                    down += 1;
                }
                // (Lines that are missing here and there can leave a merged area that is no rectangle; a spreadsheet needs rectangles
                // that do not overlap, so then only the first row of the area is merged.)
                if down > 1 && !(0..down).all(|i| (0..across).all(|j| grid.owner(r + i, c + j) == (r, c))) {
                    down = 1;
                }
                let row = base + r - first;
                if across > 1 || down > 1 {
                    table.merged += 1;
                    let _ = write!(table.merges, "<mergeCell ref=\"{}{}:{}{}\"/>", column_name(c), row + 1, column_name(c + across - 1), row + down);
                }
                let Some(text) = texts.get(r * cols + c).filter(|t| !t.is_empty()) else { continue };
                // (A cell can hold this many characters; the rest is cut.)
                let text = match text.char_indices().nth(MAX_CELL_CHARS) {
                    Some((i, _)) => {
                        self.truncated += 1;
                        Cow::Owned(text.get(..i).unwrap_or_default().to_string())
                    }
                    None => Cow::Borrowed(text.as_str()),
                };
                let value = detect(&text);
                if let Value::Number(_, f) | Value::Weak(_, f) = &value
                    && !f.is_date()
                    && let Some(n) = count.get_mut(c)
                {
                    *n = (n.0 + 1, n.1 + usize::from(f.rich()));
                }
                if across == 1 && let Some(w) = table.widths.get_mut(c) {
                    *w = (*w).max(display_width(&text));
                }
                placed.push((r, c, text, value));
            }
        }
        let mut open: Option<usize> = None;
        for (r, c, text, value) in placed {
            let (numbers, amounts) = count.get(c).copied().unwrap_or_default();
            let value = match value {
                Value::Weak(v, f) if amounts * 2 > numbers.saturating_sub(1) => Value::Number(v, f),
                Value::Weak(..) => Value::Text,
                v => v,
            };
            let row = base + r - first;
            if open != Some(row) {
                if open.is_some() {
                    table.data.push_str("</row>");
                }
                let _ = write!(table.data, "<row r=\"{}\">", row + 1);
                open = Some(row);
            }
            self.cell(&mut table.data, row, c, &text, value)?;
        }
        if open.is_some() {
            table.data.push_str("</row>");
        }
        table.rows += rows - first;
        Ok(())
    }

    /// Make the sheet of a table that is done.
    fn close<W: Write>(&mut self, zip: &mut Zip<W>, table: Pending) -> Result<()> {
        if self.sheets.len() >= MAX_SHEETS {
            return Err(self.full(format!("more than {MAX_SHEETS} tables; choose some of the pages")));
        }
        let name = if table.first == table.last { format!("第{}页-表{}", table.first, table.k) } else { format!("第{}-{}页-表{}", table.first, table.last, table.k) };
        self.add_sheet(zip, &name, table.xml())
    }

    /// The sheet of the text of the pages when there is no table: column A, a line to a row, an empty row between the pages.
    fn text_sheet(&mut self, rows: &[String]) -> String {
        let mut xml = String::from(SHEET_HEAD);
        let _ = write!(xml, "<dimension ref=\"A1:A{}\"/><sheetViews><sheetView workbookViewId=\"0\"/></sheetViews><sheetFormatPr defaultRowHeight=\"15\"/><cols><col min=\"1\" max=\"1\" width=\"100\" customWidth=\"1\"/></cols><sheetData>", rows.len().max(1));
        for (r, line) in rows.iter().enumerate() {
            let text: String = line.chars().take(MAX_CELL_CHARS).collect();
            if text.trim().is_empty() {
                continue;
            }
            // (Text, whatever it looks like: these lines are not cells of a table.)
            if let Ok(i) = self.string(text.trim()) {
                let _ = write!(xml, "<row r=\"{0}\"><c r=\"A{0}\" t=\"s\"><v>{i}</v></c></row>", r + 1);
                self.cells += 1;
            }
        }
        xml.push_str("</sheetData>");
        xml.push_str(SHEET_TAIL);
        xml
    }

    /// Put a sheet in the package under a name of its own (at most 31 characters, none of `[]:*?/\`, not one already used).
    fn add_sheet<W: Write>(&mut self, zip: &mut Zip<W>, name: &str, xml: String) -> Result<()> {
        self.xml_bytes += xml.len() as u64;
        if self.xml_bytes > MAX_SHEET_XML {
            return Err(self.full("the sheets are too big; choose some of the pages".to_string()));
        }
        let name = self.unique_name(name);
        zip.add_deflated(&format!("xl/worksheets/sheet{}.xml", self.sheets.len() + 1), xml.as_bytes())?;
        self.sheets.push(name);
        Ok(())
    }

    fn unique_name(&mut self, wanted: &str) -> String {
        let mut base: String = wanted.chars().map(|c| if "[]:*?/\\".contains(c) || c.is_control() { '_' } else { c }).collect();
        base = base.trim_matches('\'').to_string();
        if base.is_empty() {
            base = "Sheet".to_string();
        }
        let mut n = 1usize;
        loop {
            let tail = if n == 1 { String::new() } else { format!("({n})") };
            // (Excel counts the length in UTF-16 units.)
            let room = 31usize.saturating_sub(tail.encode_utf16().count());
            let (mut name, mut used) = (String::new(), 0usize);
            for c in base.chars() {
                used += c.len_utf16();
                if used > room {
                    break;
                }
                name.push(c);
            }
            name.push_str(&tail);
            if self.taken.insert(name.to_lowercase()) {
                return name;
            }
            n += 1;
        }
    }

    /// The parts that need the whole workbook: the workbook, its relationships, the styles, the shared strings, the content types.
    fn finish<W: Write>(&mut self, zip: &mut Zip<W>, now: u64) -> Result<()> {
        let rel = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        let n = self.sheets.len();
        let mut workbook = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"{rel}\"><bookViews><workbookView/></bookViews><sheets>");
        let mut rels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
        let mut types = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/><Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
        );
        for (i, name) in self.sheets.iter().enumerate() {
            let id = i + 1;
            workbook.push_str("<sheet name=\"");
            esc_attr(&mut workbook, name);
            let _ = write!(workbook, "\" sheetId=\"{id}\" r:id=\"rId{id}\"/>");
            let _ = write!(rels, "<Relationship Id=\"rId{id}\" Type=\"{rel}/worksheet\" Target=\"worksheets/sheet{id}.xml\"/>");
            let _ = write!(types, "<Override PartName=\"/xl/worksheets/sheet{id}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>");
        }
        workbook.push_str("</sheets></workbook>");
        let _ = write!(rels, "<Relationship Id=\"rId{}\" Type=\"{rel}/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId{}\" Type=\"{rel}/sharedStrings\" Target=\"sharedStrings.xml\"/></Relationships>", n + 1, n + 2);
        types.push_str("</Types>");

        // The number formats: a code for each, and a cell format (xf) for each that uses it.
        let mut styles = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
        if !self.formats.is_empty() {
            let _ = write!(styles, "<numFmts count=\"{}\">", self.formats.len());
            for (i, f) in self.formats.iter().enumerate() {
                let _ = write!(styles, "<numFmt numFmtId=\"{}\" formatCode=\"", 164 + i);
                esc_attr(&mut styles, &f.code());
                styles.push_str("\"/>");
            }
            styles.push_str("</numFmts>");
        }
        styles.push_str("<fonts count=\"1\"><font><sz val=\"11\"/><name val=\"Calibri\"/><family val=\"2\"/></font></fonts><fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills><borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>");
        let _ = write!(styles, "<cellXfs count=\"{}\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>", self.formats.len() + 1);
        for i in 0..self.formats.len() {
            let _ = write!(styles, "<xf numFmtId=\"{}\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyNumberFormat=\"1\"/>", 164 + i);
        }
        styles.push_str("</cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>");

        // The shared strings, in the order of their numbers.
        let mut list: Vec<&str> = vec![""; self.strings.len()];
        for (s, &i) in &self.strings {
            if let Some(slot) = list.get_mut(i as usize) {
                *slot = s;
            }
        }
        let mut sst = String::with_capacity(self.string_bytes + 200);
        let _ = write!(sst, "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{}\" uniqueCount=\"{}\">", self.string_uses, list.len());
        for s in list {
            sst.push_str(if s.starts_with(char::is_whitespace) || s.ends_with(char::is_whitespace) || s.contains("  ") { "<si><t xml:space=\"preserve\">" } else { "<si><t>" });
            esc(&mut sst, s);
            sst.push_str("</t></si>");
        }
        sst.push_str("</sst>");

        zip.add_deflated("xl/workbook.xml", workbook.as_bytes())?;
        zip.add_deflated("xl/_rels/workbook.xml.rels", rels.as_bytes())?;
        zip.add_deflated("xl/styles.xml", styles.as_bytes())?;
        zip.add_deflated("xl/sharedStrings.xml", sst.as_bytes())?;
        zip.add_deflated("docProps/core.xml", zip::core_properties(now).as_bytes())?;
        zip.add_deflated("_rels/.rels", ROOT_RELS.as_bytes())?;
        // (The list of the parts comes last, as the writers of the best known libraries do: it names every sheet.)
        zip.add_deflated("[Content_Types].xml", types.as_bytes())?;
        Ok(())
    }
}

const ROOT_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_names_run_on_after_z() {
        assert_eq!([0, 1, 25, 26, 27, 51, 52, 701, 702].map(column_name), ["A", "B", "Z", "AA", "AB", "AZ", "BA", "ZZ", "AAA"]);
    }

    #[test]
    fn sheet_names_are_short_clean_and_all_different() {
        let mut b = Book::default();
        assert_eq!(b.unique_name("第3页-表1"), "第3页-表1");
        assert_eq!(b.unique_name("第3页-表1"), "第3页-表1(2)");
        assert_eq!(b.unique_name("第3页-表1"), "第3页-表1(3)");
        // A name differing by case only is the same name to Excel.
        assert_eq!(b.unique_name("Data"), "Data");
        assert_eq!(b.unique_name("DATA"), "DATA(2)");
        assert_eq!(b.unique_name("a[b]:c*d?e/f\\g"), "a_b__c_d_e_f_g");
        assert_eq!(b.unique_name("'quoted'"), "quoted");
        assert_eq!(b.unique_name("''"), "Sheet");
        let long = b.unique_name(&"x".repeat(60));
        assert_eq!(long.len(), 31);
        assert_eq!(b.unique_name(&"x".repeat(60)), format!("{}(2)", "x".repeat(28)));
        // 31 characters of two UTF-16 units each are too long; the limit counts the units.
        let emoji = b.unique_name(&"😀".repeat(20));
        assert!(emoji.encode_utf16().count() <= 31 && !emoji.is_empty());
        assert_eq!(b.unique_name("a\u{1}b"), "a_b");
        for name in &b.taken {
            assert!(name.encode_utf16().count() <= 31);
        }
    }

    #[test]
    fn at_most_two_hundred_kinds_of_number_get_a_style_and_the_rest_the_general_format() {
        let mut b = Book::default();
        let mut styles = Vec::new();
        for cur in ["", "¥", "$", "€", "£"] {
            for dec in 0..10usize {
                for (thousands, paren, percent) in (0..8).map(|i| (i & 1 != 0, i & 2 != 0, i & 4 != 0)) {
                    let mut t = format!("{cur}{}{}{}", if thousands { "1,234" } else { "1234" }, if dec > 0 { format!(".{}", "5".repeat(dec)) } else { String::new() }, if percent { "%" } else { "" });
                    if paren {
                        t = format!("({t})");
                    }
                    if let Value::Number(_, f) | Value::Weak(_, f) = detect(&t) {
                        styles.push(b.style(f));
                    }
                }
            }
        }
        assert_eq!(styles.len(), 400);
        assert_eq!(b.formats.len(), MAX_FORMATS);
        assert!(styles.iter().all(|&s| s <= MAX_FORMATS) && styles.last() == Some(&0));
    }

    #[test]
    fn the_warning_for_a_file_with_no_table_says_when_pages_were_left_out() {
        assert_eq!(nothing_found(false, 0, 3), "no table and no text was found");
        assert!(nothing_found(false, 1, 0).contains("nothing was converted: 1 page left out") && !nothing_found(false, 1, 0).contains("others"));
        assert!(nothing_found(false, 2, 5).contains("2 pages left out") && nothing_found(false, 2, 5).contains("the others have no table"));
        assert!(nothing_found(true, 0, 1).starts_with("no table was found:"));
        assert!(nothing_found(true, 1, 1).contains("1 page left out"));
    }

    #[test]
    fn strings_are_shared_and_counted_and_numbers_get_a_style() {
        let mut b = Book::default();
        let (a, c, a2) = (b.string("alpha").unwrap(), b.string("beta").unwrap(), b.string("alpha").unwrap());
        assert_eq!((a, c, a2, b.strings.len(), b.string_uses), (0, 1, 0, 2, 3));
        // A string costs its text and the map and markup round it.
        assert_eq!(b.string_bytes, 5 + 4 + 2 * STRING_OVERHEAD);
        let mut xml = String::new();
        for (r, c, t) in [(0, 0, "x1"), (0, 1, "1,234.50"), (1, 27, "12"), (1, 28, "007"), (2, 0, "2024-05-01")] {
            b.cell(&mut xml, r, c, t, detect(t)).unwrap();
        }
        assert_eq!(
            xml,
            "<c r=\"A1\" t=\"s\"><v>2</v></c><c r=\"B1\" s=\"1\"><v>1234.5</v></c><c r=\"AB2\"><v>12</v></c><c r=\"AC2\" t=\"s\"><v>3</v></c><c r=\"A3\" s=\"2\"><v>45413</v></c>"
        );
        assert_eq!(b.formats.len(), 2);
    }
}
