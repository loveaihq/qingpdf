//! PDF to Word (5b): a .docx made of the text, tables and pictures of the pages (ISO/IEC 29500, WordprocessingML).
//!
//! The text, with the size and style of every character, the pictures and the ruled lines come from the text layer of
//! `qingpdf-base` ([`crate::text::TextExtractor::page_layout`], one reading of each page). `flow.rs` works out reading order,
//! paragraphs, lists and tables from them; `write.rs` decides what only the whole document tells (headings, running page
//! headers and footers, the usual font) and writes the parts; `zip.rs` is the container. Pictures are written to the file as soon
//! as a page has them, so that no more than one is in memory; the text of all the pages is kept until the end.
//!
//! ```no_run
//! # use qingpdf_core::{Document, docx};
//! let doc = Document::open("a.pdf")?;
//! let report = docx::export_to_path(&doc, "", None, std::path::Path::new("a.docx"), None)?;
//! println!("{} pages, {} tables", report.pages, report.counts.tables);
//! # Ok::<(), qingpdf_core::Error>(())
//! ```

mod flow;
mod write;

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use qingpdf_base::render::export::{decode_image, png};
use qingpdf_base::render::work::{PAGE_WORK, Work};
use qingpdf_base::text::{self, ImagePlace, TextExtractor};

use crate::object::{Dict, Object, Stream};
use crate::{Document, Error, ObjRef, Result};

use flow::{Align, Pic};
pub use write::Counts;
use crate::zip::{self, Zip};

/// Most pages, characters, different pictures and bytes of picture one document is made of; and the work all its pages may take
/// together (every page has its own allowance of [`PAGE_WORK`] as well).
pub const MAX_PAGES: usize = 5_000;
pub const MAX_CHARS: u64 = 8_000_000;
pub const MAX_IMAGES: usize = 3_000;
pub const MAX_MEDIA_BYTES: u64 = 1 << 30;
/// Most runs of text (stretches of one size and font) and bytes of document text: what the memory for the pages and the text file
/// come to (some 100 bytes a run, in the pages and again in the XML).
pub const MAX_RUNS: usize = 1_000_000;
const MAX_DOCUMENT_XML: usize = 256 << 20;
const JOB_WORK: f64 = 40.0 * PAGE_WORK;
/// A picture is made at 150 dots to the inch of the size it has on the page, and never with more pixels than this.
const PICTURE_DPI: f32 = 150.0;
const MAX_PICTURE_PIXELS: f32 = 6_000_000.0;
/// Units of work for a pixel of a picture written as PNG (filtering and compressing it), and for a byte of a JPEG file that goes into
/// the package as it is (read from the file, checksummed and written).
const PNG_PIXEL_WORK: f64 = 100.0;
const JPEG_BYTE_WORK: f64 = 4.0;

/// What was written.
#[derive(Debug, Default)]
pub struct Report {
    /// Pages converted (a page that was too much to read is left out and reported in `warnings`).
    pub pages: usize,
    pub counts: Counts,
    /// Size of the file.
    pub bytes: u64,
    pub warnings: Vec<String>,
}

/// Make a .docx of the pages `pages` (counted from 0; `None`: all) of `doc` and write it to `out`. `password` is the one the file was
/// opened with (the author's permission to take text out of the file is looked at, and the owner password lifts it). `cancel` stops
/// the work when it is set ([`Error::Cancelled`]). Nothing from the file can make this panic; what is too much for a limit gives
/// [`Error::Limit`].
pub fn export<W: Write>(doc: &Document, password: &str, pages: Option<&[usize]>, out: W, cancel: Option<Arc<AtomicBool>>) -> Result<Report> {
    text::check_extraction_allowed(doc, password)?;
    let count = doc.page_count()?;
    let wanted: Vec<usize> = match pages {
        Some(list) => list.to_vec(),
        None => (0..count).collect(),
    };
    if wanted.len() > MAX_PAGES {
        return Err(Error::Limit(format!("{} pages are more than the {MAX_PAGES} one Word file is made of; choose some of them", wanted.len())));
    }
    let all = doc.pages()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut zip = Zip::new(out, now);
    zip.add_deflated("[Content_Types].xml", CONTENT_TYPES.as_bytes())?;
    zip.add_deflated("_rels/.rels", ROOT_RELS.as_bytes())?;

    let mut extractor = TextExtractor::new(doc);
    let job = Work::with_allowance(JOB_WORK);
    let mut images = Images::default();
    let mut done = Vec::with_capacity(wanted.len());
    let mut warnings = Vec::new();
    let (mut chars, mut runs) = (0u64, 0usize);
    for &index in &wanted {
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let Some(page) = all.get(index) else { return Err(Error::Invalid(format!("the file has no page {}", index + 1))) };
        let work = Work::new().with_cancel(cancel.clone()).with_decoder_memory(48 << 20);
        // (Every page has the decoding allowance of a page, as drawing has: the pictures of a long scan are far more than one
        // allowance for the whole file holds, and the pages after the allowance was used up would lose their text.)
        let _budget = doc.page_decode_budget();
        match convert_page(doc, &mut extractor, page, &work, &mut zip, &mut images) {
            Ok(p) => {
                chars += p.chars;
                runs += p.out.run_count();
                if chars > MAX_CHARS || runs > MAX_RUNS {
                    return Err(Error::Limit(format!("more than {MAX_CHARS} characters or {MAX_RUNS} differently written pieces of text; choose some of the pages")));
                }
                if p.left_out > 0 {
                    let (n, s) = (p.left_out, if p.left_out == 1 { "" } else { "s" });
                    warnings.push(format!("page {}: {n} picture{s} left out ({})", index + 1, p.why));
                }
                done.push(p.out);
            }
            Err(Error::Limit(m)) => warnings.push(format!("page {} is left out: {m}", index + 1)),
            Err(e) => return Err(e),
        }
        if !job.charge(work.used()) {
            return Err(Error::Limit("the pages ask for more work than one conversion is allowed; choose some of the pages".to_string()));
        }
    }
    warnings.extend(extractor.take_warnings());

    let parts = write::assemble(&mut done);
    if parts.document.len() > MAX_DOCUMENT_XML {
        return Err(Error::Limit("the text of the document is too big; choose some of the pages".to_string()));
    }
    let rels = document_rels(&images.files);
    let core = zip::core_properties(now);
    zip.add_deflated("word/document.xml", parts.document.as_bytes())?;
    zip.add_deflated("word/_rels/document.xml.rels", rels.as_bytes())?;
    zip.add_deflated("word/styles.xml", parts.styles.as_bytes())?;
    zip.add_deflated("word/numbering.xml", parts.numbering.as_bytes())?;
    zip.add_deflated("word/settings.xml", SETTINGS.as_bytes())?;
    zip.add_deflated("word/header1.xml", parts.header.as_bytes())?;
    zip.add_deflated("word/footer1.xml", parts.footer.as_bytes())?;
    zip.add_deflated("docProps/core.xml", core.as_bytes())?;
    let (_, bytes) = zip.finish()?;
    Ok(Report { pages: done.len(), counts: parts.counts, bytes, warnings })
}

/// [`export`] to the file `path`: written into a file of its own in the same folder, which replaces `path` only when it is whole.
pub fn export_to_path(doc: &Document, password: &str, pages: Option<&[usize]>, path: &Path, cancel: Option<Arc<AtomicBool>>) -> Result<Report> {
    crate::zip::write_file(path, |sink| export(doc, password, pages, sink, cancel))
}

/// The pictures written so far.
#[derive(Default)]
struct Images {
    /// By the image's object: the number of its file in the package and its extension (`None`: not usable, not tried again).
    seen: HashMap<ObjRef, Option<(usize, &'static str)>>,
    /// The numbers and extensions of the files, for the relationships.
    files: Vec<&'static str>,
}

/// One page, converted.
struct Converted {
    out: flow::PageOut,
    chars: u64,
    /// How many pictures could not be made (a limit was reached), and the reason of the first.
    left_out: usize,
    why: String,
}

/// Where a picture of a page went.
enum Placed {
    Pic(Pic),
    /// The picture cannot be used (a damaged stream, a filter we do not have); nothing to say about it.
    Unusable,
    /// The picture was too much for a limit: it is left out, the page is not.
    TooMuch(String),
}

fn convert_page<W: Write>(
    doc: &Document,
    extractor: &mut TextExtractor<'_>,
    page: &crate::Page,
    work: &Work,
    zip: &mut Zip<W>,
    images: &mut Images,
) -> Result<Converted> {
    let layout = extractor.page_layout(page, work)?;
    if !work.charge(flow::cost(&layout)) {
        return Err(Error::Limit("the page asks for more work than one page of a conversion is allowed".to_string()));
    }
    let chars: usize = layout.lines.iter().map(|l| l.text.chars().count()).sum();
    let (pw, ph) = layout.size;
    let mut pics = Vec::with_capacity(layout.images.len());
    let (mut left_out, mut why) = (0usize, String::new());
    for place in &layout.images {
        // A picture over the whole page under text is a background (a scan with its recognised text on top, a letterhead).
        let covers = (place.bbox[2] - place.bbox[0]) * (place.bbox[3] - place.bbox[1]) >= 0.85 * pw * ph;
        if covers && chars >= 20 {
            pics.push(None);
            continue;
        }
        // (What goes wrong with a picture does not take the text of the page with it.)
        match images.place(doc, place, (pw, ph), work, zip)? {
            Placed::Pic(p) => pics.push(Some(p)),
            Placed::Unusable => pics.push(None),
            Placed::TooMuch(m) => {
                if left_out == 0 {
                    why = m;
                }
                left_out += 1;
                pics.push(None);
            }
        }
    }
    let out = flow::analyze(&layout, &pics);
    let tables: f64 = out.blocks.iter().map(|b| if let flow::Block::Table(t) = b { flow::table_cost(t) } else { 0.0 }).sum();
    if !work.charge(tables + out.table_work) {
        return Err(Error::Limit("the tables of the page are more than one page of a conversion is allowed".to_string()));
    }
    Ok(Converted { out, chars: chars as u64, left_out, why })
}

impl Images {
    /// Write the picture of `place` to the package (once for each image object) and say where it goes on the page. Only what
    /// ends the whole conversion (it was cancelled, the file cannot be written) is an error.
    fn place<W: Write>(&mut self, doc: &Document, place: &ImagePlace, page: (f32, f32), work: &Work, zip: &mut Zip<W>) -> Result<Placed> {
        let (w, h) = (place.bbox[2] - place.bbox[0], place.bbox[3] - place.bbox[1]);
        let known = match self.seen.get(&place.obj) {
            Some(k) => *k,
            None => {
                let made = match self.make(doc, place, work, zip.len()) {
                    Ok(Some((ext, data))) => {
                        let number = self.files.len() + 1;
                        zip.add_stored(&format!("word/media/image{number}.{ext}"), &data)?;
                        self.files.push(ext);
                        Some((number, ext))
                    }
                    Ok(None) => None,
                    Err(e @ Error::Cancelled) => return Err(e),
                    Err(e) => {
                        // (Not tried again where the picture comes again.)
                        self.seen.insert(place.obj, None);
                        return Ok(Placed::TooMuch(e.to_string()));
                    }
                };
                self.seen.insert(place.obj, made);
                made
            }
        };
        let Some((index, ext)) = known else { return Ok(Placed::Unusable) };
        let mid = 0.5 * (place.bbox[0] + place.bbox[2]);
        let align = if (mid - 0.5 * page.0).abs() < 0.05 * page.0 {
            Align::Center
        } else if mid > 0.7 * page.0 {
            Align::Right
        } else {
            Align::Left
        };
        Ok(Placed::Pic(Pic { index, ext, w, h, flip_h: place.flip_h, flip_v: place.flip_v, align, rect: place.bbox }))
    }

    /// The file of the picture: its extension and bytes. `Ok(None)`: the picture cannot be used. `used` is what the package holds
    /// already.
    fn make(&self, doc: &Document, place: &ImagePlace, work: &Work, used: u64) -> Result<Option<(&'static str, Vec<u8>)>> {
        if self.files.len() >= MAX_IMAGES {
            return Err(Error::Limit(format!("more than {MAX_IMAGES} different pictures")));
        }
        let stream = match doc.get(place.obj) {
            Ok(Object::Stream(s)) => s,
            Err(e @ (Error::Limit(_) | Error::Cancelled)) => return Err(e),
            _ => return Ok(None),
        };
        let (w, h) = (place.bbox[2] - place.bbox[0], place.bbox[3] - place.bbox[1]);
        let (ext, data): (&'static str, Vec<u8>) = if jpeg_as_it_is(&stream) {
            if !work.charge(stream.data.len() as f64 * JPEG_BYTE_WORK) {
                return Err(Error::Limit("the pictures of the page are more than one page of a conversion is allowed".to_string()));
            }
            ("jpeg", stream.data)
        } else {
            // About 150 dots to the inch of the place the picture has, at most `MAX_PICTURE_PIXELS` of them.
            let (mut tw, mut th) = ((w * PICTURE_DPI / 72.0).max(1.0), (h * PICTURE_DPI / 72.0).max(1.0));
            if tw * th > MAX_PICTURE_PIXELS {
                let k = (MAX_PICTURE_PIXELS / (tw * th)).sqrt();
                (tw, th) = ((tw * k).max(1.0), (th * k).max(1.0));
            }
            let Some(px) = decode_image(doc, &stream, work, (tw as usize, th as usize))? else { return Ok(None) };
            if !work.charge(f64::from(px.width) * f64::from(px.height) * PNG_PIXEL_WORK) {
                return Err(Error::Limit("the pictures of the page are more than one page of a conversion is allowed".to_string()));
            }
            ("png", png(px.width, px.height, px.channels, &px.data))
        };
        // (Checked for each picture: the pictures of one page can be more than the package may hold.)
        if used.saturating_add(data.len() as u64) > MAX_MEDIA_BYTES {
            return Err(Error::Limit("the pictures make the file too big".to_string()));
        }
        Ok(Some((ext, data)))
    }
}

/// Can the stream's bytes go into the .docx as they are: one JPEG file of 1 or 3 components that no other part of the image
/// (a mask, a decode array, a colour space that is not RGB or grey) changes?
fn jpeg_as_it_is(s: &Stream) -> bool {
    let d: &Dict = &s.dict;
    let single = match d.get("Filter") {
        Some(Object::Name(n)) => n.as_bytes() == b"DCTDecode",
        Some(Object::Array(a)) => matches!(a.as_slice(), [Object::Name(n)] if n.as_bytes() == b"DCTDecode"),
        _ => false,
    };
    let plain_space = match d.get("ColorSpace") {
        Some(Object::Name(n)) => matches!(n.as_bytes(), b"DeviceRGB" | b"DeviceGray"),
        Some(Object::Array(a)) => matches!(a.first(), Some(Object::Name(n)) if matches!(n.as_bytes(), b"ICCBased" | b"CalRGB" | b"CalGray")),
        _ => false,
    };
    if !(single && plain_space) || ["SMask", "Mask", "Decode", "DecodeParms"].iter().any(|k| d.contains_key(k)) || matches!(d.get("ImageMask"), Some(Object::Bool(true))) {
        return false;
    }
    // The file itself: baseline or progressive, 8 bits, 1 or 3 components, and no Adobe marker (which may say the colours are
    // something else).
    let data = &s.data;
    if data.get(..2) != Some(&[0xFF, 0xD8]) {
        return false;
    }
    let mut at = 2usize;
    while let (Some(&0xFF), Some(&m)) = (data.get(at), data.get(at + 1)) {
        if m == 0xFF {
            at += 1;
            continue;
        }
        if (0xD0..=0xD9).contains(&m) || m == 0x01 {
            at += 2;
            continue;
        }
        let len = match (data.get(at + 2), data.get(at + 3)) {
            (Some(&a), Some(&b)) => usize::from(a) << 8 | usize::from(b),
            _ => return false,
        };
        match m {
            0xEE => return false,
            0xC0..=0xC2 => {
                let (bits, comps) = (data.get(at + 4).copied(), data.get(at + 9).copied());
                return bits == Some(8) && matches!(comps, Some(1 | 3)) && len >= 8;
            }
            0xC3 | 0xC5..=0xCF => return false,
            _ => {}
        }
        at += 2 + len;
    }
    false
}

const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/><Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/><Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/><Override PartName=\"/word/settings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml\"/><Override PartName=\"/word/header1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml\"/><Override PartName=\"/word/footer1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.footer+xml\"/><Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/></Types>";

const ROOT_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/></Relationships>";

const SETTINGS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:settings xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:defaultTabStop w:val=\"420\"/><w:compat><w:compatSetting w:name=\"compatibilityMode\" w:uri=\"http://schemas.microsoft.com/office/word\" w:val=\"15\"/></w:compat></w:settings>";

fn document_rels(files: &[&str]) -> String {
    use std::fmt::Write as _;
    let base = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let mut s = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{base}/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId2\" Type=\"{base}/numbering\" Target=\"numbering.xml\"/><Relationship Id=\"rId3\" Type=\"{base}/settings\" Target=\"settings.xml\"/><Relationship Id=\"rId4\" Type=\"{base}/header\" Target=\"header1.xml\"/><Relationship Id=\"rId5\" Type=\"{base}/footer\" Target=\"footer1.xml\"/>"
    );
    for (i, ext) in files.iter().enumerate() {
        let n = i + 1;
        let _ = write!(s, "<Relationship Id=\"rId{}\" Type=\"{base}/image\" Target=\"media/image{n}.{ext}\"/>", 100 + n);
    }
    s.push_str("</Relationships>");
    s
}
